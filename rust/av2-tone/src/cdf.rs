//! The frame context: every adaptive CDF a tile decodes with, laid out exactly
//! as AVM's FRAME_CONTEXT so the two can be compared byte for byte. The
//! fields and default values are generated from AVM by ../../gen/cdf.ts into
//! cdf_tables.rs; this file only indexes into them.

use crate::cdf_tables::{
    AVM_FRAME_CONTEXT_SIZE, AVM_INITIALIZED_OFFSET, DEFAULT, FC_LEN, QCTX_FIELDS, QCTX_THRESHOLDS,
    REDUCED_TX_PART_SET_FIELDS,
};
use crate::entropy::{CDF_EXTRA, SymbolReader};
use crate::error::{Error, Result};

/// One FRAME_CONTEXT field: an R-dimensional array of CDFs over `nsyms`
/// symbols, each CDF_SIZE(nsyms) = nsyms + 4 values long.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cdf<const R: usize> {
    pub name: &'static str,
    /// In u16 values from the start of the frame context.
    pub offset: usize,
    pub dims: [usize; R],
    /// The declared alphabet. A few fields pack smaller alphabets into it
    /// (cdf_tables::PACKED_ALPHABETS); their callers pass the real size.
    pub nsyms: usize,
}

impl<const R: usize> Cdf<R> {
    pub const fn new(name: &'static str, offset: usize, dims: [usize; R], nsyms: usize) -> Self {
        Cdf { name, offset, dims, nsyms }
    }

    /// Where the CDF at `idx` starts, or an error naming the field if any
    /// index is out of range.
    fn locate(&self, idx: [usize; R]) -> Result<usize> {
        let mut flat = 0;
        for (i, d) in idx.iter().zip(self.dims) {
            if *i >= d {
                return Err(Error::Invalid("CDF context index out of range"));
            }
            flat = flat * d + i;
        }
        Ok(self.offset + flat * (self.nsyms + CDF_EXTRA))
    }
}

/// get_q_ctx: the coefficient-CDF bucket for a frame's base_qindex.
pub fn q_ctx(base_qindex: u32) -> usize {
    QCTX_THRESHOLDS.iter().position(|&t| base_qindex <= t).unwrap_or(QCTX_THRESHOLDS.len())
}

/// A frame context. One per tile while decoding; tiles adapt their own copy.
#[derive(Clone, PartialEq, Eq)]
pub struct FrameContext {
    cdfs: Vec<u16>,
}

impl std::fmt::Debug for FrameContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "FrameContext({} CDF values)", self.cdfs.len())
    }
}

impl FrameContext {
    /// av2_setup_past_independence: the defaults a key frame (or any frame
    /// with no primary reference) starts every tile from. They depend on the
    /// base_qindex bucket (coefficient CDFs) and reduced_tx_part_set (one
    /// transform-partition CDF), nothing else.
    pub fn default_for(base_qindex: u32, reduced_tx_part_set: bool) -> Result<FrameContext> {
        let mut cdfs = DEFAULT.to_vec();
        let q = q_ctx(base_qindex);
        for (offset, by_q) in &QCTX_FIELDS {
            let src = by_q.get(q).ok_or(Error::Invalid("q bucket"))?;
            cdfs.get_mut(*offset..offset + src.len()).ok_or(Error::Invalid("CDF table"))?.copy_from_slice(src);
        }
        if reduced_tx_part_set {
            for (offset, src) in &REDUCED_TX_PART_SET_FIELDS {
                cdfs.get_mut(*offset..offset + src.len()).ok_or(Error::Invalid("CDF table"))?.copy_from_slice(src);
            }
        }
        Ok(FrameContext { cdfs })
    }

    /// The CDF at `idx` in `field`, CDF_SIZE long, to read and adapt.
    pub fn cdf_mut<const R: usize>(&mut self, field: &Cdf<R>, idx: [usize; R]) -> Result<&mut [u16]> {
        let at = field.locate(idx)?;
        self.cdfs.get_mut(at..at + field.nsyms + CDF_EXTRA).ok_or(Error::Invalid("CDF table"))
    }

    /// Reads one symbol of `field[idx]`, adapting it if the tile allows.
    pub fn read<const R: usize>(&mut self, r: &mut SymbolReader, field: &Cdf<R>, idx: [usize; R]) -> Result<usize> {
        let nsyms = field.nsyms;
        r.read_symbol(self.cdf_mut(field, idx)?, nsyms)
    }

    /// Reads one symbol of a packed field (cdf_tables::PACKED_ALPHABETS) over
    /// its real alphabet, `nsyms`, no larger than the declared one.
    pub fn read_n<const R: usize>(
        &mut self,
        r: &mut SymbolReader,
        field: &Cdf<R>,
        idx: [usize; R],
        nsyms: usize,
    ) -> Result<usize> {
        if nsyms > field.nsyms {
            return Err(Error::Invalid("alphabet larger than its CDF"));
        }
        r.read_symbol(self.cdf_mut(field, idx)?, nsyms)
    }

    /// Every CDF value, in AVM's memory order.
    pub fn values(&self) -> &[u16] {
        &self.cdfs
    }

    /// The bytes of the equivalent AVM FRAME_CONTEXT on a little-endian
    /// machine: the CDFs, zeroed alignment padding, then `initialized` = 1.
    pub fn to_avm_bytes(&self) -> Vec<u8> {
        let mut out: Vec<u8> = self.cdfs.iter().flat_map(|v| v.to_le_bytes()).collect();
        out.resize(AVM_INITIALIZED_OFFSET, 0);
        out.extend_from_slice(&1i32.to_le_bytes());
        debug_assert_eq!(out.len(), AVM_FRAME_CONTEXT_SIZE);
        out
    }

    /// The padding bytes in `to_avm_bytes`, which AVM never writes on purpose.
    pub fn avm_padding() -> std::ops::Range<usize> {
        FC_LEN * 2..AVM_INITIALIZED_OFFSET
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cdf_tables::{FIELDS, TXB_SKIP_CDF, Y_MODE_IDX_CDF};

    #[test]
    fn buckets_follow_get_q_ctx() {
        assert_eq!([0, 90, 91, 140, 141, 190, 191, 255].map(q_ctx), [0, 0, 1, 1, 2, 2, 3, 3]);
    }

    #[test]
    fn fields_tile_the_context() {
        let mut at = 0;
        for (name, offset, len) in FIELDS {
            assert_eq!(offset, at, "{name}");
            at += len;
        }
        assert_eq!(at, FC_LEN);
    }

    #[test]
    fn indexing_is_checked() {
        let mut fc = FrameContext::default_for(100, false).unwrap();
        assert_eq!(fc.cdf_mut(&TXB_SKIP_CDF, [0, 0, 0]).unwrap().len(), 6);
        assert!(fc.cdf_mut(&TXB_SKIP_CDF, [2, 0, 0]).is_err());
        let ctx = Y_MODE_IDX_CDF.dims[0];
        assert!(fc.cdf_mut(&Y_MODE_IDX_CDF, [ctx]).is_err());
    }

    #[test]
    fn the_avm_image_has_avm_size() {
        let fc = FrameContext::default_for(0, true).unwrap();
        assert_eq!(fc.to_avm_bytes().len(), crate::cdf_tables::AVM_FRAME_CONTEXT_SIZE);
    }
}
