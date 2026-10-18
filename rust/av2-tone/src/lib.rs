//! av2-tone: an AV2 still-image decoder in safe Rust, checked bit for bit
//! against AVM 1.0.0, the AV2 reference decoder.
//!
//! The rules: no `unsafe` (forbidden below, not just avoided); every read of
//! the bitstream is bounds-checked and returns a [`Result`]; a panic on any
//! input, however malformed, is a bug.
//!
//! Scope, for now: one intra key frame in a single-picture sequence, the
//! stream this repo's encoder writes for a still. Anything else is refused
//! with [`Error::Unsupported`] naming what it was. Built so far: the AVIF
//! container, OBU framing, the sequence header and the key frame's header.
//! Reconstruction comes next, in pipeline order.

#![forbid(unsafe_code)]

pub mod avif;
mod bits;
pub mod dump;
pub mod error;
pub mod frame;
pub mod obu;
pub mod seq;

pub use error::{Error, Result};
pub use frame::FrameHeader;
pub use seq::SequenceHeader;

/// Everything parsed before the first tile's data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Headers {
    pub seq: SequenceHeader,
    pub frame: FrameHeader,
}

impl Headers {
    /// The decoded picture's size after the conformance window, as AVM crops
    /// it on output (av2_iface_common.h).
    pub fn output_size(&self) -> (u32, u32) {
        let s = &self.seq;
        let f = &self.frame;
        if !s.conf_win_enabled {
            return (f.width, f.height);
        }
        let scale = |off: u32, size: u32, max: u32| (u64::from(off) * u64::from(size) / u64::from(max)) as u32;
        let left = scale(s.conf_win_left, f.width, s.max_frame_width);
        let right = scale(s.conf_win_right, f.width, s.max_frame_width);
        let top = scale(s.conf_win_top, f.height, s.max_frame_height);
        let bottom = scale(s.conf_win_bottom, f.height, s.max_frame_height);
        (f.width.saturating_sub(left).saturating_sub(right), f.height.saturating_sub(top).saturating_sub(bottom))
    }
}

/// Parses an OBU stream (a temporal unit holding one still) up to the first
/// tile's data. Returns the headers and the tile-group payload they precede.
pub fn parse_obus(stream: &[u8]) -> Result<(Headers, &[u8])> {
    let mut seq: Option<SequenceHeader> = None;
    for obu in obu::obus(stream) {
        let obu = obu?;
        match obu.header.obu_type {
            obu::OBU_TEMPORAL_DELIMITER | obu::OBU_PADDING => {}
            obu::OBU_SEQUENCE_HEADER => {
                if seq.is_some() {
                    return Err(Error::Unsupported("more than one sequence header"));
                }
                seq = Some(SequenceHeader::parse(obu.payload)?);
            }
            obu::OBU_CLOSED_LOOP_KEY => {
                let seq = seq.ok_or(Error::Invalid("key frame before any sequence header"))?;
                let frame = FrameHeader::parse(&seq, obu.header.obu_type, obu.payload)?;
                let tiles = obu.payload.get(frame.tile_data_offset..).ok_or(Error::Truncated("tile group"))?;
                return Ok((Headers { seq, frame }, tiles));
            }
            obu::OBU_QUANTIZATION_MATRIX => return Err(Error::Unsupported("quantization matrix OBUs")),
            obu::OBU_FILM_GRAIN_MODEL => return Err(Error::Unsupported("film grain OBUs")),
            obu::OBU_CONTENT_INTERPRETATION => return Err(Error::Unsupported("content interpretation OBUs")),
            obu::OBU_METADATA_SHORT | obu::OBU_METADATA_GROUP => return Err(Error::Unsupported("metadata OBUs")),
            obu::OBU_MULTI_FRAME_HEADER => return Err(Error::Unsupported("multi-frame header OBUs")),
            _ => return Err(Error::Unsupported("an OBU type outside a single intra still")),
        }
    }
    Err(Error::Invalid("no key frame in the stream"))
}

/// A decoded picture: 16-bit samples, as AVM holds them at every bit depth.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Picture {
    pub width: u32,
    pub height: u32,
    pub bit_depth: u32,
    pub subsampling_x: u32,
    pub subsampling_y: u32,
    /// Y, then U and V unless monochrome; each `plane_size(p)` row-major.
    pub planes: Vec<Vec<u16>>,
}

impl Picture {
    pub fn plane_size(&self, plane: usize) -> (u32, u32) {
        if plane == 0 {
            (self.width, self.height)
        } else {
            (
                (self.width + self.subsampling_x) >> self.subsampling_x,
                (self.height + self.subsampling_y) >> self.subsampling_y,
            )
        }
    }
}

/// Decodes an OBU stream to a picture.
///
/// Reconstruction isn't built yet: the headers are parsed and checked, and
/// every sample comes back as mid-grey, 1 << (bit_depth - 1), the value AVM
/// fills a plane with before anything is decoded into it. The oracle harness
/// reports where that first differs from AVM.
pub fn decode_obus(stream: &[u8]) -> Result<(Headers, Picture)> {
    let (headers, _tiles) = parse_obus(stream)?;
    let (width, height) = headers.output_size();
    let s = &headers.seq;
    let mut pic = Picture {
        width,
        height,
        bit_depth: s.bit_depth,
        subsampling_x: s.subsampling_x,
        subsampling_y: s.subsampling_y,
        planes: Vec::new(),
    };
    for p in 0..s.num_planes() {
        let (w, h) = pic.plane_size(p);
        pic.planes.push(vec![1u16 << (s.bit_depth - 1); w as usize * h as usize]);
    }
    Ok((headers, pic))
}

/// Decodes the AV2 still inside an AVIF file.
pub fn decode_avif(file: &[u8]) -> Result<(Headers, Picture)> {
    decode_obus(&avif::primary_item(file)?)
}
