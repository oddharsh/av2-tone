//! OBU framing. AV2 puts the size first and counts the header in it, where AV1
//! put a has_size flag inside the header: obu_size (LEB128), then one header
//! byte, then an extension byte if the header says so. AVM: obu_util.c.

use crate::error::{Error, Result};

pub const OBU_SEQUENCE_HEADER: u8 = 1;
pub const OBU_TEMPORAL_DELIMITER: u8 = 2;
pub const OBU_MULTI_FRAME_HEADER: u8 = 3;
pub const OBU_CLOSED_LOOP_KEY: u8 = 4;
pub const OBU_OPEN_LOOP_KEY: u8 = 5;
pub const OBU_METADATA_SHORT: u8 = 8;
pub const OBU_METADATA_GROUP: u8 = 9;
pub const OBU_MULTI_STREAM_DECODER_OPERATION: u8 = 20;
pub const OBU_QUANTIZATION_MATRIX: u8 = 22;
pub const OBU_FILM_GRAIN_MODEL: u8 = 23;
pub const OBU_CONTENT_INTERPRETATION: u8 = 24;
pub const OBU_PADDING: u8 = 25;

/// The xlayer a temporal delimiter or MSDO OBU sits in when it has no
/// extension byte.
pub const GLOBAL_XLAYER_ID: u8 = 31;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObuHeader {
    pub obu_type: u8,
    pub extension: bool,
    pub tlayer_id: u8,
    pub mlayer_id: u8,
    pub xlayer_id: u8,
}

#[derive(Debug, Clone, Copy)]
pub struct Obu<'a> {
    pub header: ObuHeader,
    /// The bytes after the header, up to obu_size.
    pub payload: &'a [u8],
    /// Where the payload starts in the stream, in bytes.
    pub offset: usize,
}

/// leb128() as avm_uleb_decode reads it: at most 8 bytes, at most 2^32 - 1.
/// Returns (value, bytes read).
pub fn uleb128(data: &[u8]) -> Result<(u32, usize)> {
    let mut value: u64 = 0;
    for (i, &b) in data.iter().take(8).enumerate() {
        value |= u64::from(b & 0x7f) << (7 * i);
        if b & 0x80 == 0 {
            let v = u32::try_from(value).map_err(|_| Error::Invalid("leb128 above 2^32 - 1"))?;
            return Ok((v, i + 1));
        }
    }
    Err(if data.len() < 8 { Error::Truncated("leb128") } else { Error::Invalid("leb128 longer than 8 bytes") })
}

fn header(data: &[u8]) -> Result<(ObuHeader, usize)> {
    let &b = data.first().ok_or(Error::Truncated("OBU header"))?;
    let extension = b & 0x80 != 0;
    let obu_type = (b >> 2) & 31;
    let tlayer_id = b & 3;
    let (mlayer_id, xlayer_id, len) = if extension {
        let &e = data.get(1).ok_or(Error::Truncated("OBU extension"))?;
        (e >> 5, e & 31, 2)
    } else if obu_type == OBU_MULTI_STREAM_DECODER_OPERATION || obu_type == OBU_TEMPORAL_DELIMITER {
        (0, GLOBAL_XLAYER_ID, 1)
    } else {
        (0, 0, 1)
    };
    if obu_type == OBU_TEMPORAL_DELIMITER && xlayer_id != GLOBAL_XLAYER_ID {
        return Err(Error::Invalid("temporal delimiter outside the global xlayer"));
    }
    Ok((ObuHeader { obu_type, extension, tlayer_id, mlayer_id, xlayer_id }, len))
}

/// Splits a stream into OBUs. Stops at the first framing error.
pub struct Obus<'a> {
    rest: &'a [u8],
    consumed: usize,
}

pub fn obus(data: &[u8]) -> Obus<'_> {
    Obus { rest: data, consumed: 0 }
}

impl<'a> Iterator for Obus<'a> {
    type Item = Result<Obu<'a>>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.rest.is_empty() {
            return None;
        }
        let r = (|| {
            let (size, n) = uleb128(self.rest)?;
            let size = size as usize;
            let body = self.rest.get(n..n + size).ok_or(Error::Truncated("OBU"))?;
            let (h, hlen) = header(body)?;
            let payload = body.get(hlen..).ok_or(Error::Invalid("obu_size smaller than its header"))?;
            let offset = self.consumed + n + hlen;
            self.rest = self.rest.get(n + size..).unwrap_or_default();
            self.consumed += n + size;
            Ok(Obu { header: h, payload, offset })
        })();
        if r.is_err() {
            self.rest = &[];
        }
        Some(r)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leb() {
        assert_eq!(uleb128(&[0x05]), Ok((5, 1)));
        assert_eq!(uleb128(&[0x80, 0x01]), Ok((128, 2)));
        assert!(uleb128(&[0x80]).is_err());
        assert!(uleb128(&[0xff, 0xff, 0xff, 0xff, 0x7f]).is_err()); // 2^35 - 1
    }

    #[test]
    fn splits_a_still() {
        // TD, then a 2-byte sequence-header OBU, then a key OBU with extension.
        let s = [1, 0x08, 3, 0x04, 0xaa, 0xbb, 3, 0x90, 0x22, 0xcc];
        let v: Vec<_> = obus(&s).collect::<Result<_>>().unwrap_or_default();
        assert_eq!(v.len(), 3);
        assert_eq!(v[0].header.obu_type, OBU_TEMPORAL_DELIMITER);
        assert_eq!(v[0].header.xlayer_id, GLOBAL_XLAYER_ID);
        assert_eq!(v[1].header.obu_type, OBU_SEQUENCE_HEADER);
        assert_eq!(v[1].payload, &[0xaa, 0xbb]);
        assert_eq!(v[2].header.obu_type, OBU_CLOSED_LOOP_KEY);
        assert_eq!((v[2].header.mlayer_id, v[2].header.xlayer_id), (1, 2));
        assert_eq!(v[2].payload, &[0xcc]);
        assert_eq!((v[1].offset, v[2].offset), (4, 9));
    }

    #[test]
    fn truncated() {
        let s = [5, 0x04, 0xaa];
        let mut it = obus(&s);
        assert!(matches!(it.next(), Some(Err(Error::Truncated(_)))));
        assert!(it.next().is_none());
    }
}
