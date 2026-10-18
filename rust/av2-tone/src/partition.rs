//! The partition rules: which partitions a block may signal, which are forced,
//! the chroma-reference bookkeeping, and the partition tree. Ports of
//! av2/common/av2_common_int.h (init_allowed_partitions_for_signaling and its
//! helpers), blockd.h (set_chroma_ref_info and friends) and common_data.h
//! (sdp_chroma_part_from_luma), function for function.

use crate::avm_defs::*;
use crate::blk::*;
use crate::error::{Error, Result};

/// CHROMA_REF_INFO.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ChromaRef {
    pub is_chroma_ref: bool,
    pub offset_started: bool,
    pub mi_row_base: usize,
    pub mi_col_base: usize,
    pub bsize: usize,
    pub bsize_base: usize,
}

impl ChromaRef {
    /// initialize_chroma_ref_info.
    fn new(mi_row: usize, mi_col: usize, bsize: usize) -> ChromaRef {
        ChromaRef {
            is_chroma_ref: true,
            offset_started: false,
            mi_row_base: mi_row,
            mi_col_base: mi_col,
            bsize,
            bsize_base: bsize,
        }
    }
}

/// have_nz_chroma_ref_offset.
pub(crate) fn have_nz_chroma_ref_offset(bsize: usize, partition: usize, ss_x: usize, ss_y: usize) -> Result<bool> {
    let w = bw(bsize)? >> ss_x;
    let h = bh(bsize)? >> ss_y;
    Ok(match partition {
        PARTITION_NONE => w < 4 || h < 4,
        PARTITION_HORZ => w < 4 || h < 8,
        PARTITION_VERT => w < 8 || h < 4,
        PARTITION_SPLIT => w < 8 || h < 8,
        PARTITION_HORZ_4A | PARTITION_HORZ_4B => w < 4 || h < 32,
        PARTITION_VERT_4A | PARTITION_VERT_4B => w < 32 || h < 4,
        PARTITION_HORZ_3 => w < 8 || h < 16,
        PARTITION_VERT_3 => w < 16 || h < 8,
        _ => return Err(Error::Invalid("partition type")),
    })
}

/// is_sub_partition_chroma_ref.
fn is_sub_partition_chroma_ref(
    partition: usize,
    index: usize,
    bsize: usize,
    parent_bsize: usize,
    ss_x: usize,
    ss_y: usize,
    offset_started: bool,
) -> Result<bool> {
    let w_lt4 = (bw(bsize)? >> ss_x) < 4;
    let h_lt4 = (bh(bsize)? >> ss_y) < 4;
    Ok(match partition {
        PARTITION_NONE => true,
        PARTITION_HORZ | PARTITION_VERT => index == 1,
        PARTITION_SPLIT => {
            if offset_started || (w_lt4 && h_lt4) {
                index == 3
            } else if w_lt4 {
                index == 1 || index == 3
            } else if h_lt4 {
                index == 2 || index == 3
            } else {
                true
            }
        }
        PARTITION_HORZ_4A | PARTITION_HORZ_4B | PARTITION_VERT_4A | PARTITION_VERT_4B => index == 3,
        PARTITION_HORZ_3 => {
            if parent_bsize == BLOCK_8X32 {
                index != 1
            } else {
                index == 3
            }
        }
        PARTITION_VERT_3 => {
            if parent_bsize == BLOCK_32X8 {
                index != 1
            } else {
                index == 3
            }
        }
        _ => return Err(Error::Invalid("partition type")),
    })
}

/// set_chroma_ref_offset_size.
#[allow(clippy::too_many_arguments)]
fn set_chroma_ref_offset_size(
    mi_row: usize,
    mi_col: usize,
    partition: usize,
    bsize: usize,
    parent_bsize: usize,
    ss_x: usize,
    ss_y: usize,
    info: &mut ChromaRef,
    parent: &ChromaRef,
) -> Result<()> {
    let w_lt4 = (bw(bsize)? >> ss_x) < 4;
    let h_lt4 = (bh(bsize)? >> ss_y) < 4;
    let inherit = |info: &mut ChromaRef| {
        info.mi_row_base = parent.mi_row_base;
        info.mi_col_base = parent.mi_col_base;
        info.bsize_base = parent_bsize;
    };
    match partition {
        PARTITION_NONE | PARTITION_HORZ | PARTITION_VERT | PARTITION_HORZ_4A | PARTITION_HORZ_4B
        | PARTITION_VERT_4A | PARTITION_VERT_4B => inherit(info),
        PARTITION_HORZ_3 => {
            if parent_bsize == BLOCK_8X32 {
                if bsize == BLOCK_4X16 {
                    info.mi_row_base = parent.mi_row_base + mi_h(BLOCK_8X8)?;
                    info.mi_col_base = parent.mi_col_base;
                    info.bsize_base = BLOCK_8X16;
                } else {
                    info.offset_started = false;
                }
            } else {
                inherit(info);
            }
        }
        PARTITION_VERT_3 => {
            if parent_bsize == BLOCK_32X8 {
                if bsize == BLOCK_16X4 {
                    info.mi_row_base = parent.mi_row_base;
                    info.mi_col_base = parent.mi_col_base + mi_w(BLOCK_8X8)?;
                    info.bsize_base = BLOCK_16X8;
                } else {
                    info.offset_started = false;
                }
            } else {
                inherit(info);
            }
        }
        PARTITION_SPLIT => {
            if w_lt4 && h_lt4 {
                inherit(info);
            } else if w_lt4 {
                info.bsize_base = partition_subsize(parent_bsize, PARTITION_HORZ)?;
                info.mi_col_base = parent.mi_col_base;
                info.mi_row_base =
                    if mi_row == parent.mi_row_base { parent.mi_row_base } else { parent.mi_row_base + mi_h(bsize)? };
            } else {
                info.bsize_base = partition_subsize(parent_bsize, PARTITION_VERT)?;
                info.mi_row_base = parent.mi_row_base;
                info.mi_col_base =
                    if mi_col == parent.mi_col_base { parent.mi_col_base } else { parent.mi_col_base + mi_w(bsize)? };
            }
        }
        _ => return Err(Error::Invalid("partition type")),
    }
    Ok(())
}

/// set_chroma_ref_info.
#[allow(clippy::too_many_arguments)]
pub(crate) fn chroma_ref_info(
    tree_type: usize,
    mi_row: usize,
    mi_col: usize,
    index: usize,
    bsize: usize,
    parent: Option<&ChromaRef>,
    parent_bsize: usize,
    parent_partition: usize,
    ss_x: usize,
    ss_y: usize,
) -> Result<ChromaRef> {
    let mut info = ChromaRef::new(mi_row, mi_col, bsize);
    if tree_type == LUMA_PART {
        info.is_chroma_ref = false;
        return Ok(info);
    }
    if tree_type == CHROMA_PART {
        return Ok(info);
    }
    let Some(p) = parent else { return Ok(info) };
    if p.is_chroma_ref {
        if p.offset_started {
            info.is_chroma_ref =
                is_sub_partition_chroma_ref(parent_partition, index, bsize, parent_bsize, ss_x, ss_y, true)?;
            info.offset_started = true;
            info.mi_row_base = p.mi_row_base;
            info.mi_col_base = p.mi_col_base;
            info.bsize_base = p.bsize_base;
        } else if have_nz_chroma_ref_offset(parent_bsize, parent_partition, ss_x, ss_y)? {
            info.offset_started = true;
            info.is_chroma_ref =
                is_sub_partition_chroma_ref(parent_partition, index, bsize, parent_bsize, ss_x, ss_y, false)?;
            set_chroma_ref_offset_size(
                mi_row,
                mi_col,
                parent_partition,
                bsize,
                parent_bsize,
                ss_x,
                ss_y,
                &mut info,
                p,
            )?;
        }
    } else {
        info.is_chroma_ref = false;
        info.offset_started = true;
        info.mi_row_base = p.mi_row_base;
        info.mi_col_base = p.mi_col_base;
        info.bsize_base = p.bsize_base;
    }
    Ok(info)
}

// ---- the partition tree -----------------------------------------------------

/// PARTITION_TREE, in an arena.
#[derive(Debug, Clone, Default)]
pub(crate) struct PNode {
    pub parent: Option<usize>,
    pub sub: [Option<usize>; 4],
    pub partition: usize,
    pub region_type: usize,
    pub extended_sdp_allowed: bool,
    pub bsize: usize,
    pub is_settled: bool,
    pub mi_row: usize,
    pub mi_col: usize,
    pub index: usize,
    pub chroma_ref: ChromaRef,
    pub is_cfl_allowed: usize,
    pub sb_root_partition_info: usize,
}

/// One superblock's partition trees: the shared/luma tree and, with SDP, the
/// chroma tree (sbi->ptree_root[0] and [1]).
#[derive(Debug, Clone, Default)]
pub(crate) struct PTrees {
    pub nodes: Vec<PNode>,
}

impl PTrees {
    /// av2_alloc_ptree_node: calloc'd, then parent, index and an invalid
    /// partition.
    pub fn alloc(&mut self, parent: Option<usize>, index: usize) -> usize {
        self.nodes.push(PNode { parent, index, partition: PARTITION_INVALID, ..PNode::default() });
        self.nodes.len() - 1
    }

    pub fn get(&self, i: usize) -> Result<&PNode> {
        self.nodes.get(i).ok_or(Error::Invalid("partition tree node"))
    }

    pub fn get_mut(&mut self, i: usize) -> Result<&mut PNode> {
        self.nodes.get_mut(i).ok_or(Error::Invalid("partition tree node"))
    }

    /// get_partition_subtree_const.
    pub fn child(&self, node: Option<usize>, idx: usize) -> Result<Option<usize>> {
        match node {
            None => Ok(None),
            Some(n) => Ok(*self.get(n)?.sub.get(idx).ok_or(Error::Invalid("subtree index"))?),
        }
    }
}

// ---- which partitions a block may take -------------------------------------

/// is_bsize_above_decoupled_thresh.
fn above_decoupled_thresh(bsize: usize) -> bool {
    (BLOCK_64X64..=BLOCK_LARGEST).contains(&bsize)
}

/// is_sdp_share_partition.
fn sdp_share_partition(luma: usize, child: usize) -> bool {
    if child == PARTITION_NONE {
        return false;
    }
    let horz = [PARTITION_HORZ, PARTITION_HORZ_3, PARTITION_HORZ_4A, PARTITION_HORZ_4B];
    let vert = [PARTITION_VERT, PARTITION_VERT_3, PARTITION_VERT_4A, PARTITION_VERT_4B];
    match luma {
        PARTITION_HORZ | PARTITION_HORZ_3 => !horz.contains(&child),
        PARTITION_VERT | PARTITION_VERT_3 => !vert.contains(&child),
        _ => false,
    }
}

/// is_luma_chroma_share_same_partition.
pub(crate) fn luma_chroma_share_partition(
    trees: &PTrees,
    tree_type: usize,
    luma: Option<usize>,
    bsize: usize,
) -> Result<bool> {
    let Some(l) = luma else { return Ok(false) };
    let ln = trees.get(l)?;
    if tree_type != CHROMA_PART || !above_decoupled_thresh(bsize) || ln.partition == PARTITION_INVALID {
        return Ok(false);
    }
    if bsize > BLOCK_64X64 || ln.partition == PARTITION_NONE {
        return Ok(true);
    }
    for s in ln.sub.iter().flatten() {
        let sp = trees.get(*s)?.partition;
        if sp != PARTITION_INVALID && !sdp_share_partition(ln.partition, sp) {
            return Ok(false);
        }
    }
    Ok(true)
}

/// is_cfl_allowed_for_this_luma_partition.
fn cfl_allowed_for_luma_partition(enabled: bool, luma: usize, current: usize) -> usize {
    if !enabled {
        return CFL_DISALLOWED_FOR_CHROMA;
    }
    if luma == current {
        return CFL_ALLOWED_FOR_CHROMA;
    }
    let horz = [PARTITION_HORZ, PARTITION_HORZ_3, PARTITION_HORZ_4A, PARTITION_HORZ_4B];
    let vert = [PARTITION_VERT, PARTITION_VERT_3, PARTITION_VERT_4A, PARTITION_VERT_4B];
    if (horz.contains(&luma) && horz.contains(&current)) || (vert.contains(&luma) && vert.contains(&current)) {
        CFL_ALLOWED_FOR_CHROMA
    } else {
        CFL_DISALLOWED_FOR_CHROMA
    }
}

/// is_cfl_allowed_for_sdp, for an intra frame.
pub(crate) fn cfl_allowed_for_sdp(
    trees: &PTrees,
    enable_cfl: bool,
    enable_mhccp: bool,
    tree_type: usize,
    luma: Option<usize>,
    current: usize,
    bsize_luma: usize,
) -> Result<usize> {
    if !enable_cfl && !enable_mhccp {
        return Ok(CFL_DISALLOWED_FOR_CHROMA);
    }
    if tree_type != CHROMA_PART {
        return Ok(CFL_ALLOWED_FOR_CHROMA);
    }
    if bsize_luma > BLOCK_64X64 && bsize_luma <= BLOCK_LARGEST && current == PARTITION_NONE {
        return Ok(CFL_ALLOWED_FOR_CHROMA);
    }
    if bsize_luma != BLOCK_64X64 {
        return Ok(CFL_DISALLOWED_FOR_CHROMA);
    }
    if current == PARTITION_NONE {
        return Ok(CFL_ALLOWED_FOR_CHROMA);
    }
    if luma.is_some() && luma_chroma_share_partition(trees, tree_type, luma, bsize_luma)? {
        return Ok(CFL_ALLOWED_FOR_CHROMA);
    }
    match luma {
        Some(l) => Ok(cfl_allowed_for_luma_partition(enable_cfl || enable_mhccp, trees.get(l)?.partition, current)),
        None => Ok(CFL_DISALLOWED_FOR_CHROMA),
    }
}

/// sdp_chroma_part_from_luma.
fn sdp_chroma_part_from_luma(bsize: usize, luma: usize, ss_x: usize, ss_y: usize) -> Result<usize> {
    let h = bh(bsize)? >> ss_y;
    let w = bw(bsize)? >> ss_x;
    let by = |n: usize, four: usize, two: usize| {
        if n >= 32 {
            four
        } else if n >= 8 {
            two
        } else {
            PARTITION_NONE
        }
    };
    Ok(match luma {
        PARTITION_NONE => PARTITION_NONE,
        PARTITION_HORZ => {
            if h < 8 {
                PARTITION_NONE
            } else {
                PARTITION_HORZ
            }
        }
        PARTITION_VERT => {
            if w < 8 {
                PARTITION_NONE
            } else {
                PARTITION_VERT
            }
        }
        PARTITION_HORZ_4A => by(h, PARTITION_HORZ_4A, PARTITION_HORZ),
        PARTITION_HORZ_4B => by(h, PARTITION_HORZ_4B, PARTITION_HORZ),
        PARTITION_VERT_4A => by(w, PARTITION_VERT_4A, PARTITION_VERT),
        PARTITION_VERT_4B => by(w, PARTITION_VERT_4B, PARTITION_VERT),
        PARTITION_HORZ_3 => {
            if h >= 16 {
                PARTITION_HORZ_3
            } else if h < 8 {
                PARTITION_NONE
            } else {
                PARTITION_HORZ
            }
        }
        PARTITION_VERT_3 => {
            if w >= 16 {
                PARTITION_VERT_3
            } else if w < 8 {
                PARTITION_NONE
            } else {
                PARTITION_VERT
            }
        }
        PARTITION_SPLIT => {
            if h < 8 || w < 8 {
                PARTITION_NONE
            } else {
                PARTITION_SPLIT
            }
        }
        PARTITION_INVALID => PARTITION_INVALID,
        _ => return Err(Error::Invalid("luma partition")),
    })
}

/// is_partition_implied_at_boundary.
fn implied_at_boundary(
    mi_rows: usize,
    mi_cols: usize,
    mi_row: usize,
    mi_col: usize,
    bsize: usize,
) -> Result<Option<usize>> {
    if bsize >= BLOCK_SIZES_ALL {
        return Ok(None);
    }
    let has_rows = mi_row + mi_h(bsize)? / 2 < mi_rows;
    let has_cols = mi_col + mi_w(bsize)? / 2 < mi_cols;
    if has_rows && has_cols {
        return Ok(None);
    }
    if is_square(bsize)? {
        return Ok(Some(if has_rows && !has_cols { PARTITION_VERT } else { PARTITION_HORZ }));
    }
    if is_tall(bsize)? {
        if !has_rows {
            return Ok(Some(PARTITION_HORZ));
        }
        let sub_has_cols = mi_col + mi_w(bsize)? / 4 < mi_cols;
        return Ok((mi_w(bsize)? >= 4 && !sub_has_cols).then_some(PARTITION_HORZ));
    }
    if !has_cols {
        return Ok(Some(PARTITION_VERT));
    }
    let sub_has_rows = mi_row + mi_h(bsize)? / 4 < mi_rows;
    Ok((mi_h(bsize)? >= 4 && !sub_has_rows).then_some(PARTITION_VERT))
}

/// Frame-level inputs the partition rules read.
#[derive(Debug, Clone, Copy)]
pub(crate) struct PartFrame {
    pub mi_rows: usize,
    pub mi_cols: usize,
    pub ss_x: usize,
    pub ss_y: usize,
    pub monochrome: bool,
    pub sdp: bool,
    pub enable_ext_partitions: bool,
    pub enable_uneven_4way: bool,
    pub max_pb_aspect_ratio_log2_m1: usize,
}

/// av2_get_normative_forced_partition_type.
pub(crate) fn forced_partition(
    f: &PartFrame,
    trees: &PTrees,
    tree_type: usize,
    mi_row: usize,
    mi_col: usize,
    bsize: usize,
    luma: Option<usize>,
) -> Result<usize> {
    if !is_partition_point(bsize) {
        return Ok(PARTITION_NONE);
    }
    if tree_type == CHROMA_PART && bsize == BLOCK_8X8 {
        return Ok(PARTITION_NONE);
    }
    if luma_chroma_share_partition(trees, tree_type, luma, bsize)? {
        let lp = trees.get(luma.ok_or(Error::Invalid("luma tree"))?)?.partition;
        return sdp_chroma_part_from_luma(bsize, lp, f.ss_x, f.ss_y);
    }
    Ok(implied_at_boundary(f.mi_rows, f.mi_cols, mi_row, mi_col, bsize)?.unwrap_or(PARTITION_INVALID))
}

/// is_ext_partition_allowed_at_bsize.
fn ext_allowed_at_bsize(bsize: usize, tree_type: usize) -> bool {
    if bsize >= BLOCK_SIZES || (bsize > BLOCK_64X64 && bsize <= BLOCK_LARGEST) || bsize <= BLOCK_8X8 {
        return false;
    }
    if tree_type == CHROMA_PART && bsize <= BLOCK_16X16 {
        return false;
    }
    if bsize == BLOCK_4X16 || bsize == BLOCK_16X4 {
        return false;
    }
    !(tree_type == CHROMA_PART && (bsize == BLOCK_8X32 || bsize == BLOCK_32X8))
}

/// is_ext_partition_allowed.
fn ext_allowed(bsize: usize, rect: usize, tree_type: usize) -> bool {
    if !ext_allowed_at_bsize(bsize, tree_type) {
        return false;
    }
    let bad = [
        (BLOCK_16X8, HORZ),
        (BLOCK_8X16, VERT),
        (BLOCK_32X8, HORZ),
        (BLOCK_8X32, VERT),
        (BLOCK_64X16, HORZ),
        (BLOCK_16X64, VERT),
    ];
    if bad.contains(&(bsize, rect)) {
        return false;
    }
    !(tree_type == CHROMA_PART && ((bsize == BLOCK_32X16 && rect == HORZ) || (bsize == BLOCK_16X32 && rect == VERT)))
}

/// is_uneven_4way_partition_allowed_at_bsize.
fn uneven_allowed_at_bsize(bsize: usize, tree_type: usize) -> bool {
    if !ext_allowed_at_bsize(bsize, tree_type) {
        return false;
    }
    if bsize > BLOCK_LARGEST {
        return bsize >= BLOCK_16X64 || (tree_type != CHROMA_PART && bsize >= BLOCK_8X32);
    }
    bsize >= BLOCK_32X64 || (tree_type != CHROMA_PART && bsize >= BLOCK_16X32)
}

/// is_uneven_4way_partition_allowed.
fn uneven_allowed(bsize: usize, rect: usize, tree_type: usize) -> Result<bool> {
    if !ext_allowed(bsize, rect, tree_type) || !uneven_allowed_at_bsize(bsize, tree_type) {
        return Ok(false);
    }
    let dim = if rect == HORZ { bh(bsize)? } else { bw(bsize)? };
    Ok(dim == 64 || (dim >= 32 && tree_type != CHROMA_PART))
}

/// rect_type_implied_by_bsize.
pub(crate) fn rect_type_implied(bsize: usize, tree_type: usize) -> usize {
    match bsize {
        BLOCK_128X256 | BLOCK_4X8 | BLOCK_64X128 | BLOCK_4X16 => HORZ,
        BLOCK_256X128 | BLOCK_8X4 | BLOCK_128X64 | BLOCK_16X4 => VERT,
        BLOCK_8X16 | BLOCK_8X32 if tree_type == CHROMA_PART => HORZ,
        BLOCK_16X8 | BLOCK_32X8 if tree_type == CHROMA_PART => VERT,
        _ => RECT_INVALID,
    }
}

/// is_partition_valid.
fn partition_valid(bsize: usize, p: usize) -> Result<bool> {
    if is_partition_point(bsize) { Ok(partition_subsize(bsize, p)? < BLOCK_SIZES_ALL) } else { Ok(p == PARTITION_NONE) }
}

/// get_chroma_ref_offsets.
fn chroma_ref_offsets(bsize: usize, partition: usize) -> Result<(usize, usize)> {
    Ok(match partition {
        PARTITION_NONE => (0, 0),
        PARTITION_HORZ => (mi_h(bsize)? / 2, 0),
        PARTITION_VERT => (0, mi_w(bsize)? / 2),
        PARTITION_HORZ_3 => {
            if bsize == BLOCK_8X32 {
                (0, mi_w(bsize)? / 2)
            } else {
                (3 * mi_h(bsize)? / 4, 0)
            }
        }
        PARTITION_VERT_3 => {
            if bsize == BLOCK_32X8 {
                (mi_h(bsize)? / 2, 0)
            } else {
                (0, 3 * mi_w(bsize)? / 4)
            }
        }
        PARTITION_HORZ_4A | PARTITION_HORZ_4B => (7 * mi_h(bsize)? / 8, 0),
        PARTITION_VERT_4A | PARTITION_VERT_4B => (0, 7 * mi_w(bsize)? / 8),
        _ => return Err(Error::Invalid("chroma ref offsets")),
    })
}

/// is_chroma_ref_within_boundary.
#[allow(clippy::too_many_arguments)]
pub(crate) fn chroma_ref_within_boundary(
    f: &PartFrame,
    tree_type: usize,
    is_chroma_ref: bool,
    mi_row: usize,
    mi_col: usize,
    bsize: usize,
    partition: usize,
) -> Result<bool> {
    if tree_type != SHARED_PART
        || f.monochrome
        || !is_chroma_ref
        || !have_nz_chroma_ref_offset(bsize, partition, f.ss_x, f.ss_y)?
    {
        return Ok(true);
    }
    let (r, c) = chroma_ref_offsets(bsize, partition)?;
    Ok(mi_row + r < f.mi_rows && mi_col + c < f.mi_cols)
}

/// check_is_chroma_size_valid.
#[allow(clippy::too_many_arguments)]
fn chroma_size_valid(
    f: &PartFrame,
    tree_type: usize,
    partition: usize,
    bsize: usize,
    mi_row: usize,
    mi_col: usize,
    parent: Option<&ChromaRef>,
) -> Result<bool> {
    let sdp_tree = tree_type == LUMA_PART || (f.sdp && tree_type == SHARED_PART);
    if sdp_tree {
        if above_decoupled_thresh(bsize) {
            let sub = partition_subsize(bsize, partition)?;
            if sub == BLOCK_INVALID {
                return Ok(false);
            }
            return Ok(plane_bsize(sub, f.ss_x, f.ss_y)? != BLOCK_INVALID);
        }
        return Ok(true);
    }
    let sub = partition_subsize(bsize, partition)?;
    if sub >= BLOCK_SIZES_ALL {
        return Ok(false);
    }
    let info = chroma_ref_info(tree_type, mi_row, mi_col, 0, sub, parent, bsize, partition, f.ss_x, f.ss_y)?;
    Ok(plane_bsize(info.bsize_base, f.ss_x, f.ss_y)? != BLOCK_INVALID)
}

/// check_partition_aspect_ratio.
fn aspect_ratio_ok(bsize: usize, partition: usize, max: usize) -> Result<bool> {
    let sub = partition_subsize(bsize, partition)?;
    if sub == BLOCK_INVALID {
        return Ok(false);
    }
    let (w, h) = (bw(sub)?, bh(sub)?);
    if (w > h * max || h > w * max) && (partition == PARTITION_NONE || w >= h * 8 || h >= w * 8) {
        return Ok(false);
    }
    Ok(true)
}

/// init_allowed_partitions_for_signaling, for an intra frame (every region
/// is INTRA_REGION, so is_valid_partition_in_mixed_region always holds).
#[allow(clippy::too_many_arguments)]
pub(crate) fn allowed_partitions(
    f: &PartFrame,
    sb_size: usize,
    tree_type: usize,
    mi_row: usize,
    mi_col: usize,
    bsize: usize,
    chroma_ref: Option<&ChromaRef>,
) -> Result<[bool; ALL_PARTITION_TYPES]> {
    let has_rows = mi_row + mi_h(bsize)? / 2 < f.mi_rows;
    let has_cols = mi_col + mi_w(bsize)? / 2 < f.mi_cols;
    let is_chroma_ref = chroma_ref.is_none_or(|c| c.is_chroma_ref);
    let implied = rect_type_implied(bsize, tree_type);
    let within = |p| chroma_ref_within_boundary(f, tree_type, is_chroma_ref, mi_row, mi_col, bsize, p);
    let size_ok = |p| chroma_size_valid(f, tree_type, p, bsize, mi_row, mi_col, chroma_ref);
    let splittable = is_partition_point(bsize);
    let mut a = [false; ALL_PARTITION_TYPES];

    a[PARTITION_NONE] = (tree_type == CHROMA_PART && bsize == BLOCK_8X8) || (has_rows && has_cols);
    let horz_ok = partition_valid(bsize, PARTITION_HORZ)? && implied != VERT && size_ok(PARTITION_HORZ)?;
    let vert_ok = partition_valid(bsize, PARTITION_VERT)? && implied != HORZ && size_ok(PARTITION_VERT)?;
    a[PARTITION_HORZ] = splittable && horz_ok && within(PARTITION_HORZ)?;
    a[PARTITION_VERT] = splittable && vert_ok && within(PARTITION_VERT)?;

    let ext = splittable && f.enable_ext_partitions;
    for (p, rect, not) in [(PARTITION_HORZ_3, HORZ, VERT), (PARTITION_VERT_3, VERT, HORZ)] {
        *a.at_mut(p)? = ext
            && implied != not
            && ext_allowed(bsize, rect, tree_type)
            && partition_subsize(bsize, p)? != BLOCK_INVALID
            && size_ok(p)?
            && within(p)?;
    }
    let uneven = ext && f.enable_uneven_4way;
    for (p, rect, not) in [
        (PARTITION_HORZ_4A, HORZ, VERT),
        (PARTITION_HORZ_4B, HORZ, VERT),
        (PARTITION_VERT_4A, VERT, HORZ),
        (PARTITION_VERT_4B, VERT, HORZ),
    ] {
        *a.at_mut(p)? = uneven
            && implied != not
            && uneven_allowed(bsize, rect, tree_type)?
            && partition_subsize(bsize, p)? != BLOCK_INVALID
            && size_ok(p)?
            && within(p)?;
    }
    let _ = sb_size;
    a[PARTITION_SPLIT] = bsize == BLOCK_128X128 || bsize == BLOCK_256X256;

    let max = 1usize << (f.max_pb_aspect_ratio_log2_m1 + 1);
    let mut n = 0;
    for (p, allowed) in a.iter_mut().enumerate() {
        *allowed = *allowed && aspect_ratio_ok(bsize, p, max)?;
        n += usize::from(*allowed);
    }
    if n == 0 {
        a[PARTITION_NONE] = true;
    }
    Ok(a)
}

/// only_allowed_partition.
pub(crate) fn only_allowed(a: &[bool; ALL_PARTITION_TYPES]) -> usize {
    let mut found = PARTITION_INVALID;
    for (p, &ok) in a.iter().enumerate() {
        if ok {
            if found != PARTITION_INVALID {
                return PARTITION_INVALID;
            }
            found = p;
        }
    }
    found
}

/// only_allowed_rect_type.
pub(crate) fn only_allowed_rect(a: &[bool; ALL_PARTITION_TYPES]) -> usize {
    let horz = a[PARTITION_HORZ] || a[PARTITION_HORZ_3] || a[PARTITION_HORZ_4A] || a[PARTITION_HORZ_4B];
    let vert = a[PARTITION_VERT] || a[PARTITION_VERT_3] || a[PARTITION_VERT_4A] || a[PARTITION_VERT_4B];
    match (horz, vert) {
        (true, true) => RECT_INVALID,
        (true, false) => HORZ,
        _ => VERT,
    }
}

/// is_do_ext_partition_implied: Some(implied) or None if signalled.
pub(crate) fn ext_implied(a: &[bool; ALL_PARTITION_TYPES], rect: usize) -> Option<bool> {
    let (non_ext, ext) = if rect == HORZ {
        (a[PARTITION_HORZ], a[PARTITION_HORZ_3] || a[PARTITION_HORZ_4A] || a[PARTITION_HORZ_4B])
    } else {
        (a[PARTITION_VERT], a[PARTITION_VERT_3] || a[PARTITION_VERT_4A] || a[PARTITION_VERT_4B])
    };
    match (non_ext, ext) {
        (true, true) => None,
        (true, false) => Some(false),
        _ => Some(true),
    }
}

/// is_do_uneven_4way_partition_implied.
pub(crate) fn uneven_implied(a: &[bool; ALL_PARTITION_TYPES], rect: usize) -> Option<bool> {
    let (three, four) = if rect == HORZ {
        (a[PARTITION_HORZ_3], a[PARTITION_HORZ_4A] || a[PARTITION_HORZ_4B])
    } else {
        (a[PARTITION_VERT_3], a[PARTITION_VERT_4A] || a[PARTITION_VERT_4B])
    };
    match (three, four) {
        (true, true) => None,
        (true, false) => Some(false),
        _ => Some(true),
    }
}

/// partition_plane_context_helper's two maps (av2_common_int.h).
const BSIZE_RECT_MAP: [usize; BLOCK_SIZES] =
    [0, 0, 0, 0, 1, 2, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 13, 14, 13, 14];
const BSIZE_MAP: [usize; BLOCK_SIZES] =
    [0, 0, 0, 0, 1, 1, 1, 2, 2, 2, 3, 3, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15];

/// partition_plane_context: `above` and `left` are the partition contexts at
/// the block's first column and row (and `above_mid`/`left_mid` at its middle,
/// for the ext/4-way modes).
pub(crate) fn plane_context(
    above: u8,
    left: u8,
    above_mid: u8,
    left_mid: u8,
    bsize: usize,
    rect: usize,
    mode: usize,
) -> Result<usize> {
    let bsl_w = mi_w_log2(bsize)?;
    let bsl_h = mi_h_log2(bsize)?;
    let raw = if mode == EXT_PART_CTX_MODE || mode == FOUR_WAY_CTX_MODE {
        let (c1, c2) = if rect == HORZ {
            ((left >> bsl_h.saturating_sub(2)) & 1, (left_mid >> bsl_h.saturating_sub(2)) & 1)
        } else {
            ((above >> bsl_w.saturating_sub(2)) & 1, (above_mid >> bsl_w.saturating_sub(2)) & 1)
        };
        usize::from(c1 * 2 + c2)
    } else {
        let a = (above >> bsl_w.saturating_sub(1)) & 1;
        let l = (left >> bsl_h.saturating_sub(1)) & 1;
        usize::from(l * 2 + a)
    };
    let ctx = if mode == SQUARE_SPLIT_CTX_MODE {
        raw + usize::from(bsize == BLOCK_256X256) * PARTITION_PLOFFSET
    } else if mode == RECT_TYPE_CTX_MODE {
        raw + BSIZE_RECT_MAP.at(bsize)? * PARTITION_PLOFFSET
    } else {
        raw + BSIZE_MAP.at(bsize)? * PARTITION_PLOFFSET
    };
    if ctx >= PARTITION_CONTEXTS {
        return Err(Error::Invalid("partition context"));
    }
    Ok(ctx)
}

/// rect_part_table: (do_ext, do_4way, 4way type, rect) -> partition.
pub(crate) fn rect_partition(do_ext: bool, do_4way: bool, b_type: bool, rect: usize) -> usize {
    match (do_ext, do_4way, b_type, rect == HORZ) {
        (false, _, _, true) => PARTITION_HORZ,
        (false, _, _, false) => PARTITION_VERT,
        (true, false, _, true) => PARTITION_HORZ_3,
        (true, false, _, false) => PARTITION_VERT_3,
        (true, true, false, true) => PARTITION_HORZ_4A,
        (true, true, false, false) => PARTITION_VERT_4A,
        (true, true, true, true) => PARTITION_HORZ_4B,
        (true, true, true, false) => PARTITION_VERT_4B,
    }
}

/// get_h_partition_subsize's middle sizes (blockd.h).
const MID_SUB_BLOCK_HPART: [usize; BLOCK_SIZES] = [
    BLOCK_INVALID,
    BLOCK_INVALID,
    BLOCK_INVALID,
    BLOCK_INVALID,
    BLOCK_4X8,
    BLOCK_8X4,
    BLOCK_8X8,
    BLOCK_8X16,
    BLOCK_16X8,
    BLOCK_16X16,
    BLOCK_16X32,
    BLOCK_32X16,
    BLOCK_32X32,
    BLOCK_INVALID,
    BLOCK_INVALID,
    BLOCK_INVALID,
    BLOCK_INVALID,
    BLOCK_INVALID,
    BLOCK_INVALID,
    BLOCK_INVALID,
    BLOCK_INVALID,
    BLOCK_4X16,
    BLOCK_16X4,
    BLOCK_8X32,
    BLOCK_32X8,
];

/// get_h_partition_subsize and the offsets of sub-block `i`.
pub(crate) fn h_partition(bsize: usize, i: usize, partition: usize) -> Result<(usize, usize, usize)> {
    if !is_partition_point(bsize) || partition_subsize(bsize, partition)? == BLOCK_INVALID {
        return Err(Error::Invalid("3-way partition"));
    }
    let sub = if i == 0 || i == 3 { partition_subsize(bsize, partition)? } else { *MID_SUB_BLOCK_HPART.at(bsize)? };
    let (hbh, hbw) = (mi_h(bsize)? >> 1, mi_w(bsize)? >> 1);
    let row = if partition == PARTITION_VERT_3 {
        if i == 2 { hbh } else { 0 }
    } else {
        [0, hbh >> 1, hbh >> 1, 3 * (hbh >> 1)].get(i).copied().unwrap_or(0)
    };
    let col = if partition == PARTITION_HORZ_3 {
        if i == 2 { hbw } else { 0 }
    } else {
        [0, hbw >> 1, hbw >> 1, 3 * (hbw >> 1)].get(i).copied().unwrap_or(0)
    };
    Ok((sub, row, col))
}
