//! A tile group's tiles, each with the entropy state it starts from. AVM:
//! get_tile_buffers and the per-tile setup in decode_tiles
//! (av2/decoder/decodeframe.c).
//!
//! Every tile but the last carries its size first, `tile_size_bytes` bytes
//! little-endian, minus one; the last runs to the end of the OBU. Every tile
//! starts from the same frame context (AVM copies cm->fc into each tile's
//! tctx) and its own arithmetic decoder over its bytes.

use crate::Headers;
use crate::cdf::FrameContext;
use crate::entropy::SymbolReader;
use crate::error::{Error, Result};

/// One tile's bytes, located in the tile group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TileBuffer<'a> {
    pub row: u32,
    pub col: u32,
    pub data: &'a [u8],
    /// Offset of `data` from the start of the tile-group data.
    pub offset: usize,
}

/// get_tile_buffers: splits the tile data that follows the tile-group header.
pub fn tile_buffers<'a>(h: &Headers, data: &'a [u8]) -> Result<Vec<TileBuffer<'a>>> {
    let t = &h.frame.tiles;
    let size_bytes = t.tile_size_bytes as usize;
    let last = h.frame.tile_end;
    let mut out = Vec::new();
    let mut at = 0usize;
    let mut index = 0;
    for row in 0..t.rows {
        for col in 0..t.cols {
            if index < h.frame.tile_start || index > last {
                index += 1;
                continue;
            }
            if at >= data.len() {
                return Err(Error::Truncated("tile group ended before all tiles were read"));
            }
            let size = if index == last {
                data.len() - at
            } else {
                let field = data.get(at..at + size_bytes).ok_or(Error::Truncated("tile size"))?;
                at += size_bytes;
                let size = field.iter().rev().fold(0usize, |v, &b| (v << 8) | usize::from(b)) + 1;
                if size > data.len() - at {
                    return Err(Error::Truncated("tile size past the end of the tile group"));
                }
                size
            };
            let bytes = data.get(at..at + size).ok_or(Error::Truncated("tile"))?;
            out.push(TileBuffer { row, col, data: bytes, offset: at });
            at += size;
            index += 1;
        }
    }
    Ok(out)
}

/// A tile ready to decode: its bytes and the entropy state it starts from.
#[derive(Debug, Clone)]
pub struct Tile<'a> {
    pub buffer: TileBuffer<'a>,
    pub reader: SymbolReader<'a>,
    pub ctx: FrameContext,
}

/// Sets up every tile of the key frame's tile group, as AVM does before
/// decoding the first superblock.
pub fn init_tiles<'a>(h: &Headers, data: &'a [u8]) -> Result<Vec<Tile<'a>>> {
    let ctx = FrameContext::default_for(h.frame.quant.base_qindex, h.seq.reduced_tx_part_set)?;
    let allow_update = !h.frame.disable_cdf_update;
    tile_buffers(h, data)?
        .into_iter()
        .map(|b| Ok(Tile { reader: SymbolReader::new(b.data, allow_update), ctx: ctx.clone(), buffer: b }))
        .collect()
}
