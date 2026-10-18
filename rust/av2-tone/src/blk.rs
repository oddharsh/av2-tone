//! Checked lookups into AVM's tables (avm_defs.rs) and the small block-size
//! helpers from av2/common/blockd.h and common_data.h that everything in the
//! block parse leans on. A value that came from the bitstream never indexes a
//! table directly: `at` returns an error instead of panicking.

use crate::avm_defs::*;
use crate::error::{Error, Result};

/// Bounds-checked indexing that returns `Error::Invalid` instead of panicking.
pub(crate) trait At<T> {
    fn at(&self, i: usize) -> Result<&T>;
    fn at_mut(&mut self, i: usize) -> Result<&mut T>;
}

impl<T> At<T> for [T] {
    fn at(&self, i: usize) -> Result<&T> {
        self.get(i).ok_or(Error::Invalid("index out of range"))
    }
    fn at_mut(&mut self, i: usize) -> Result<&mut T> {
        self.get_mut(i).ok_or(Error::Invalid("index out of range"))
    }
}

/// A table entry as usize.
pub(crate) fn t<T: Copy + Into<u32>>(table: &[T], i: usize) -> Result<usize> {
    Ok((*table.at(i)?).into() as usize)
}

/// A 2-D table entry as usize.
pub(crate) fn t2<T: Copy + Into<u32>, const N: usize>(table: &[[T; N]], i: usize, j: usize) -> Result<usize> {
    Ok((*table.at(i)?.at(j)?).into() as usize)
}

// ---- block sizes ------------------------------------------------------------

pub(crate) fn mi_w(bsize: usize) -> Result<usize> {
    t(&MI_SIZE_WIDE, bsize)
}
pub(crate) fn mi_h(bsize: usize) -> Result<usize> {
    t(&MI_SIZE_HIGH, bsize)
}
pub(crate) fn mi_w_log2(bsize: usize) -> Result<usize> {
    t(&MI_SIZE_WIDE_LOG2, bsize)
}
pub(crate) fn mi_h_log2(bsize: usize) -> Result<usize> {
    t(&MI_SIZE_HIGH_LOG2, bsize)
}
pub(crate) fn bw(bsize: usize) -> Result<usize> {
    t(&BLOCK_SIZE_WIDE, bsize)
}
pub(crate) fn bh(bsize: usize) -> Result<usize> {
    t(&BLOCK_SIZE_HIGH, bsize)
}

/// is_partition_point.
pub(crate) fn is_partition_point(bsize: usize) -> bool {
    bsize != BLOCK_4X4 && bsize < BLOCK_SIZES
}

/// get_partition_subsize: BLOCK_INVALID when the partition doesn't fit.
pub(crate) fn partition_subsize(bsize: usize, partition: usize) -> Result<usize> {
    if partition == PARTITION_INVALID {
        Ok(BLOCK_INVALID)
    } else if is_partition_point(bsize) {
        t2(&SUBSIZE_LOOKUP, partition, bsize)
    } else {
        Ok(if partition == PARTITION_NONE { bsize } else { BLOCK_INVALID })
    }
}

/// get_plane_block_size.
pub(crate) fn plane_bsize(bsize: usize, ss_x: usize, ss_y: usize) -> Result<usize> {
    Ok(*SS_SIZE_LOOKUP.at(bsize)?.at(ss_x)?.at(ss_y)? as usize)
}

pub(crate) fn is_square(bsize: usize) -> Result<bool> {
    Ok(bw(bsize)? == bh(bsize)?)
}
pub(crate) fn is_tall(bsize: usize) -> Result<bool> {
    Ok(bh(bsize)? > bw(bsize)?)
}
