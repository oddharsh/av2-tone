//! One coding block's mode info, as an intra frame codes it: a port of
//! decode_mbmi_block and set_offsets (av2/decoder/decodeframe.c),
//! read_intra_frame_mode_info and its readers (decodemv.c), the neighbour
//! and context helpers they use (pred_common, reconintra, cfl, blockd), and
//! the palette colour-map tokens (detokenize.c).
//!
//! AVM keeps one MB_MODE_INFO per 4x4 and a grid of pointers into them; a
//! shared-tree block points all its 4x4s at its first record, and later
//! per-unit writes (CDEF strength, CCSO flags) go through those pointers.
//! `Grid` keeps exactly that shape, with indices for pointers.

use crate::avm_defs::*;
use crate::blk::*;
use crate::cdf_tables::*;
use crate::error::{Error, Result};
use crate::parse::{FrameParse, TileState};
use crate::partition::ChromaRef;

/// The MB_MODE_INFO fields the parse reads or writes.
#[derive(Debug, Clone, Default)]
pub(crate) struct Mi {
    pub sb_type: [usize; 2],
    pub partition: usize,
    pub tree_type: usize,
    pub region_type: usize,
    pub sb_root_partition_info: usize,
    pub mi_row_start: usize,
    pub mi_col_start: usize,
    pub chroma_ref: ChromaRef,
    pub segment_id: usize,
    pub seg_id_predicted: bool,
    pub skip_txfm: [bool; 2],
    pub skip_mode: bool,
    pub use_intrabc: [bool; 2],
    pub morph_pred: bool,
    pub intrabc_mode: usize,
    pub intrabc_drl_idx: usize,
    pub pb_mv_precision: usize,
    /// The coded DV difference; the predicted DV comes with reconstruction.
    pub mv_diff: (i32, i32),
    pub mode: usize,
    pub uv_mode: usize,
    pub angle_delta: [i32; 2],
    pub joint_y_mode_delta_angle: usize,
    pub y_mode_idx: usize,
    pub mrl_index: usize,
    pub multi_line_mrl: bool,
    pub fsc_mode: [usize; 2],
    pub cfl_idx: usize,
    pub cfl_alpha_idx: usize,
    pub cfl_alpha_signs: usize,
    pub mh_dir: usize,
    pub palette_size: [usize; 2],
    pub palette_colors: [u16; 3 * PALETTE_MAX_SIZE],
    pub use_intra_dip: bool,
    pub cdef_strength: i32,
    pub ccso_blk: [u8; 3],
    pub tx_size: usize,
    /// is_wide_angle and mapped_intra_mode for the current transform block,
    /// luma [0] and chroma [1]: the wide-angle mode, if the mode was mapped.
    pub wide_angle: [Option<usize>; 2],
}

impl Mi {
    /// is_intrabc_block.
    pub fn is_intrabc(&self, tree_type: usize) -> bool {
        *self.use_intrabc.get(usize::from(tree_type == CHROMA_PART)).unwrap_or(&false)
    }
    /// is_inter_block, in an intra frame: only intra block copy counts.
    pub fn is_inter(&self, tree_type: usize) -> bool {
        self.is_intrabc(tree_type)
    }
    /// av2_get_joint_mode.
    fn joint_mode(mi: Option<&Mi>) -> usize {
        match mi {
            Some(m) if !m.is_inter(SHARED_PART) && !m.is_intrabc(SHARED_PART) => m.joint_y_mode_delta_angle,
            _ => DC_PRED,
        }
    }
}

/// mi_grid_base and mi_alloc: one record per 4x4, and per-4x4 pointers.
#[derive(Debug, Clone, Default)]
pub(crate) struct Grid {
    pub stride: usize,
    pub rows: usize,
    pub ptr: Vec<Option<usize>>,
    pub rec: Vec<Mi>,
}

impl Grid {
    pub fn new(mi_rows: usize, mi_cols: usize) -> Grid {
        // calc_mi_size: aligned to the largest superblock.
        let stride = mi_cols.div_ceil(MAX_MIB_SIZE) * MAX_MIB_SIZE;
        let rows = mi_rows.div_ceil(MAX_MIB_SIZE) * MAX_MIB_SIZE;
        Grid { stride, rows, ptr: vec![None; stride * rows], rec: vec![Mi::default(); stride * rows] }
    }
    pub fn idx(&self, r: usize, c: usize) -> Result<usize> {
        if r < self.rows && c < self.stride { Ok(r * self.stride + c) } else { Err(Error::Invalid("mi position")) }
    }
    /// The record the grid points at, if any.
    pub fn at(&self, r: isize, c: isize) -> Option<usize> {
        if r < 0 || c < 0 || r as usize >= self.rows || c as usize >= self.stride {
            return None;
        }
        self.ptr.get(r as usize * self.stride + c as usize).copied().flatten()
    }
    pub fn mi(&self, i: usize) -> Result<&Mi> {
        self.rec.get(i).ok_or(Error::Invalid("mode info record"))
    }
    pub fn mi_mut(&mut self, i: usize) -> Result<&mut Mi> {
        self.rec.get_mut(i).ok_or(Error::Invalid("mode info record"))
    }
    fn opt(&self, i: Option<usize>) -> Option<&Mi> {
        i.and_then(|i| self.rec.get(i))
    }
}

/// MACROBLOCKD's per-block view: where the block is and who its neighbours are.
#[derive(Debug, Clone, Default)]
pub(crate) struct Xd {
    pub mi_row: usize,
    pub mi_col: usize,
    /// xd->mi[0].
    pub cur: usize,
    pub up_available: bool,
    pub left_available: bool,
    pub above: Option<usize>,
    pub left: Option<usize>,
    pub above_right: Option<usize>,
    pub bottom_left: Option<usize>,
    pub neighbors: [Option<usize>; MAX_NUM_NEIGHBORS],
    pub neighbors_lb: [Option<usize>; MAX_NUM_NEIGHBORS],
    pub chroma_above: Option<usize>,
    pub chroma_left: Option<usize>,
    pub is_chroma_ref: bool,
    /// In 1/8 pel, as GET_MV_SUBPEL gives them.
    pub mb_to_right_edge: i64,
    pub mb_to_bottom_edge: i64,
}

/// av2_neg_deinterleave.
fn neg_deinterleave(diff: i64, r: i64, max: i64) -> i64 {
    if r == 0 {
        return diff;
    }
    if r >= max - 1 {
        return max - diff - 1;
    }
    if 2 * r < max {
        if diff <= 2 * r {
            return if diff & 1 == 1 { r + ((diff + 1) >> 1) } else { r - (diff >> 1) };
        }
        diff
    } else {
        if diff <= 2 * (max - r - 1) {
            return if diff & 1 == 1 { r + ((diff + 1) >> 1) } else { r - (diff >> 1) };
        }
        max - (diff + 1)
    }
}

impl FrameParse<'_> {
    /// decode_mbmi_block's set_offsets: point the grid at this block's record
    /// and find its neighbours (set_mi_row_col).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn set_offsets(
        &mut self,
        mi_row: usize,
        mi_col: usize,
        bsize: usize,
        index: usize,
        parent: usize,
    ) -> Result<Xd> {
        let (bw, bh) = (mi_w(bsize)?, mi_h(bsize)?);
        let x_inside = bw.min(self.f.mi_cols - mi_col);
        let y_inside = bh.min(self.f.mi_rows - mi_row);
        let g = &mut self.grid;
        let top = g.idx(mi_row, mi_col)?;
        *g.ptr.at_mut(top)? = Some(top);
        let tree = self.tree_type;
        {
            let m = g.mi_mut(top)?;
            *m.sb_type.at_mut(usize::from(tree == CHROMA_PART))? = bsize;
            if tree != CHROMA_PART {
                m.mi_row_start = mi_row;
                m.mi_col_start = mi_col;
            }
        }
        if tree == SHARED_PART {
            for y in 0..y_inside {
                for x in 0..x_inside {
                    let i = g.idx(mi_row + y, mi_col + x)?;
                    *g.ptr.at_mut(i)? = Some(top);
                }
            }
        }
        let pn = self.trees.get(parent)?;
        let info = crate::partition::chroma_ref_info(
            tree,
            mi_row,
            mi_col,
            index,
            bsize,
            Some(&pn.chroma_ref),
            pn.bsize,
            pn.partition,
            self.f.ss_x,
            self.f.ss_y,
        )?;
        self.grid.mi_mut(top)?.chroma_ref = info;

        let t = self.tile;
        let (r, c) = (mi_row as isize, mi_col as isize);
        let s = &self.grid;
        let mut xd = Xd {
            mi_row,
            mi_col,
            cur: top,
            up_available: mi_row > t.mi_row_start,
            left_available: mi_col > t.mi_col_start,
            mb_to_right_edge: (self.f.mi_cols as i64 - bw as i64 - mi_col as i64) * 4 * 8,
            mb_to_bottom_edge: (self.f.mi_rows as i64 - bh as i64 - mi_row as i64) * 4 * 8,
            ..Xd::default()
        };
        if xd.up_available {
            xd.above = s.at(r - 1, c);
            xd.above_right = s.at(r - 1, c + bw as isize - 1);
        }
        if xd.left_available {
            xd.left = s.at(r, c - 1);
            xd.bottom_left = s.at(r + bh as isize - 1, c - 1);
        }
        // fetch_spatial_neighbors and its line-buffer variant.
        let not_sb_top = mi_row % self.mib_size != 0;
        let mut n = Vec::new();
        let mut nl = Vec::new();
        for (cand, needs_not_top) in
            [(xd.bottom_left, false), (xd.above_right, true), (xd.left, false), (xd.above, true)]
        {
            if let Some(m) = cand {
                if !needs_not_top || not_sb_top {
                    n.push(m);
                }
                nl.push(m);
            }
        }
        for (slot, v) in xd.neighbors.iter_mut().zip(n) {
            *slot = Some(v);
        }
        for (slot, v) in xd.neighbors_lb.iter_mut().zip(nl) {
            *slot = Some(v);
        }

        // The chroma neighbours.
        xd.is_chroma_ref = info.is_chroma_ref;
        let chroma_left_avail = info.mi_col_base > t.mi_col_start;
        let chroma_up_avail = info.mi_row_base > t.mi_row_start;
        if xd.is_chroma_ref {
            let base_r = info.mi_row_base as isize;
            let base_c = info.mi_col_base as isize;
            if chroma_up_avail {
                let a = s.at(base_r - 1, base_c);
                xd.chroma_above = match s.opt(a) {
                    Some(am) if am.tree_type != SHARED_PART || am.chroma_ref.is_chroma_ref => a,
                    Some(am) => {
                        let first = am.chroma_ref.mi_col_base as isize;
                        let last =
                            (first + mi_w(am.chroma_ref.bsize_base)? as isize - 1).min(self.f.mi_cols as isize - 1);
                        s.at(base_r - 1, base_c + (last - base_c))
                    }
                    None => None,
                };
            }
            if chroma_left_avail {
                let l = s.at(base_r, base_c - 1);
                xd.chroma_left = match s.opt(l) {
                    Some(lm) if lm.tree_type != SHARED_PART || lm.chroma_ref.is_chroma_ref => l,
                    Some(lm) => {
                        let first = lm.chroma_ref.mi_row_base as isize;
                        let last =
                            (first + mi_h(lm.chroma_ref.bsize_base)? as isize - 1).min(self.f.mi_rows as isize - 1);
                        s.at(base_r + (last - base_r), base_c - 1)
                    }
                    None => None,
                };
            }
        }
        Ok(xd)
    }

    /// av2_read_mode_info for an intra frame.
    pub(crate) fn read_mode_info(&mut self, t: &mut TileState, xd: &Xd) -> Result<()> {
        let tree = self.tree_type;
        {
            let m = self.grid.mi_mut(xd.cur)?;
            *m.use_intrabc.at_mut(usize::from(tree == CHROMA_PART))? = false;
            if tree == SHARED_PART {
                m.sb_type[1] = m.sb_type[0];
            }
        }
        self.read_intra_frame_mode_info(t, xd)
    }

    fn mi(&self, xd: &Xd) -> Result<&Mi> {
        self.grid.mi(xd.cur)
    }
    fn mi_mut(&mut self, xd: &Xd) -> Result<&mut Mi> {
        self.grid.mi_mut(xd.cur)
    }

    /// read_intra_frame_mode_info.
    fn read_intra_frame_mode_info(&mut self, t: &mut TileState, xd: &Xd) -> Result<()> {
        let tree = self.tree_type;
        let pt = usize::from(tree == CHROMA_PART);
        let bsize = *self.mi(xd)?.sb_type.at(pt)?;
        let h = self.h;
        if self.h.frame.seg.segid_preskip && tree != CHROMA_PART {
            let id = self.read_intra_segment_id(t, xd, bsize, false)?;
            self.mi_mut(xd)?.segment_id = id;
        }
        {
            let m = self.mi_mut(xd)?;
            m.skip_mode = false;
            if tree != CHROMA_PART {
                m.morph_pred = false;
            }
        }
        let allow_ibc = self.allow_intrabc(bsize)?;
        if allow_ibc && tree != CHROMA_PART {
            let ctx =
                xd.neighbors.iter().flatten().filter(|&&n| self.grid.mi(n).is_ok_and(|m| m.is_intrabc(tree))).count();
            let v = t.fc.read(&mut t.r, &INTRABC_CDF, [ctx])? == 1;
            let m = self.mi_mut(xd)?;
            m.use_intrabc = [false, false];
            *m.use_intrabc.at_mut(pt)? = v;
        }
        if self.mi(xd)?.is_intrabc(tree) {
            let seg = self.mi(xd)?.segment_id;
            let skip = self.read_skip_txfm(t, xd, seg)?;
            *self.mi_mut(xd)?.skip_txfm.at_mut(pt)? = skip;
        } else {
            *self.mi_mut(xd)?.skip_txfm.at_mut(pt)? = false;
        }
        if !self.h.frame.seg.segid_preskip && tree != CHROMA_PART {
            let skip = *self.mi(xd)?.skip_txfm.at(pt)?;
            let id = self.read_intra_segment_id(t, xd, bsize, skip)?;
            self.mi_mut(xd)?.segment_id = id;
        }
        self.mi_mut(xd)?.seg_id_predicted = false;
        if tree != CHROMA_PART {
            self.read_gdf(t, xd)?;
            self.read_cdef(t, xd)?;
            if h.seq.enable_ccso {
                self.read_ccso(t, xd)?;
            }
            // read_delta_q_params: delta q is refused before the parse starts.
        }
        {
            let m = self.mi_mut(xd)?;
            if tree != CHROMA_PART {
                m.palette_size[0] = 0;
                m.use_intra_dip = false;
            }
            m.palette_size[1] = 0;
        }
        if allow_ibc && tree != CHROMA_PART {
            self.read_intrabc_info(t, xd, bsize)?;
            if self.mi(xd)?.is_intrabc(tree) {
                return Ok(());
            }
        }
        if tree != CHROMA_PART {
            let seg = self.mi(xd)?.segment_id;
            if *self.lossless.at(seg)? {
                return Err(Error::Unsupported("lossless segments"));
            }
            self.read_intra_luma_mode(t, xd)?;
            let fsc = if self.allow_fsc_intra(bsize, xd)? {
                let ctx =
                    xd.neighbors.iter().flatten().map(|&n| self.grid.mi(n).map_or(0, |m| m.fsc_mode[0])).sum::<usize>();
                let grp = t2_1(&FSC_BSIZE_GROUPS, bsize)?;
                t.fc.read(&mut t.r, &FSC_MODE_CDF, [ctx, grp])?
            } else {
                0
            };
            *self.mi_mut(xd)?.fsc_mode.at_mut(pt)? = fsc;
            let mode = self.mi(xd)?.mode;
            let mrl = if h.seq.enable_mrls && (V_PRED..=D67_PRED).contains(&mode) {
                let ctx = self.mrl_ctx(xd, |m| m.mrl_index != 0);
                t.fc.read(&mut t.r, &MRL_INDEX_CDF, [ctx])?
            } else {
                0
            };
            let multi = if mrl != 0 {
                let ctx = self.mrl_ctx(xd, |m| m.multi_line_mrl);
                t.fc.read(&mut t.r, &MULTI_LINE_MRL_CDF, [ctx])? == 1
            } else {
                false
            };
            let m = self.mi_mut(xd)?;
            m.mrl_index = mrl;
            m.multi_line_mrl = multi;
        }

        if tree != LUMA_PART {
            if !h.seq.monochrome && xd.is_chroma_ref {
                let cfl_ok = self.cfl_allowed(xd)?;
                let mhccp_ok = self.mhccp_allowed(xd)?;
                self.read_intra_uv_mode(t, xd, cfl_ok || mhccp_ok)?;
                if self.mi(xd)?.uv_mode == UV_CFL_PRED {
                    if mhccp_ok {
                        let switch = if h.seq.enable_cfl_intra { t.fc.read(&mut t.r, &CFL_MHCCP_CDF, [])? } else { 1 };
                        let idx = if switch != 0 { CFL_MULTI_PARAM } else { t.fc.read(&mut t.r, &CFL_INDEX_CDF, [])? };
                        self.mi_mut(xd)?.cfl_idx = idx;
                    } else if h.seq.enable_cfl_intra {
                        let idx = t.fc.read(&mut t.r, &CFL_INDEX_CDF, [])?;
                        self.mi_mut(xd)?.cfl_idx = idx;
                    }
                    if self.mi(xd)?.cfl_idx == CFL_MULTI_PARAM {
                        let grp = t2_1(&SIZE_GROUP_LOOKUP, bsize)?;
                        let dir = t.fc.read(&mut t.r, &FILTER_DIR_CDF, [grp])?;
                        self.mi_mut(xd)?.mh_dir = dir;
                    }
                    if self.mi(xd)?.cfl_idx == 0 {
                        let (idx, signs) = self.read_cfl_alphas(t)?;
                        let m = self.mi_mut(xd)?;
                        m.cfl_alpha_idx = idx;
                        m.cfl_alpha_signs = signs;
                    }
                }
            } else {
                self.mi_mut(xd)?.uv_mode = UV_DC_PRED;
            }
        } else {
            self.mi_mut(xd)?.uv_mode = UV_DC_PRED;
        }

        let allow_sct = h.frame.allow_screen_content_tools;
        if allow_sct && bw(bsize)? <= 64 && bh(bsize)? <= 64 && bsize >= BLOCK_8X8 {
            self.read_palette_mode_info(t, xd)?;
        }
        if tree != CHROMA_PART {
            self.mi_mut(xd)?.use_intra_dip = false;
            if h.seq.enable_intra_dip {
                let m = self.mi(xd)?;
                // av2_intra_dip_allowed
                let sb = m.sb_type[0];
                let allowed = m.mode == DC_PRED
                    && m.mrl_index == 0
                    && m.palette_size[0] == 0
                    && sb != BLOCK_INVALID
                    && bw(sb)? * bh(sb)? >= 128
                    && bw(sb)? > 4
                    && bh(sb)? > 4;
                if allowed {
                    return Err(Error::Unsupported("intra DIP"));
                }
            }
        }
        Ok(())
    }

    /// The sum of a flag over the two neighbours, for MRL contexts.
    fn mrl_ctx(&self, xd: &Xd, f: impl Fn(&Mi) -> bool) -> usize {
        xd.neighbors
            .iter()
            .flatten()
            .filter_map(|&n| self.grid.mi(n).ok())
            .filter(|m| !m.is_inter(SHARED_PART) && !m.is_intrabc(SHARED_PART) && f(m))
            .count()
    }

    /// av2_allow_intrabc.
    fn allow_intrabc(&self, bsize: usize) -> Result<bool> {
        let (w, h) = (bw(bsize)?, bh(bsize)?);
        if (w == 64 && h == 64) || w > 64 || h > 64 {
            return Ok(false);
        }
        Ok(self.tree_type != CHROMA_PART && self.h.frame.allow_intrabc)
    }

    /// allow_fsc_intra.
    fn allow_fsc_intra(&self, bsize: usize, xd: &Xd) -> Result<bool> {
        let (w, h) = (bw(bsize)?, bh(bsize)?);
        Ok(self.h.seq.enable_idtx_intra
            && !self.mi(xd)?.is_inter(0)
            && w <= FSC_MAXWIDTH
            && h <= FSC_MAXHEIGHT
            && w >= FSC_MINWIDTH
            && h >= FSC_MINHEIGHT)
    }

    /// read_skip_txfm.
    fn read_skip_txfm(&mut self, t: &mut TileState, xd: &Xd, segment_id: usize) -> Result<bool> {
        let mask = *self.h.frame.seg.feature_mask.at(segment_id)?;
        if self.h.frame.seg.enabled && mask & (1 << SEG_LVL_SKIP) != 0 {
            return Ok(true);
        }
        let pt = usize::from(self.tree_type == CHROMA_PART);
        let mut ctx = 0;
        for n in xd.neighbors_lb.iter().flatten() {
            ctx += usize::from(*self.grid.mi(*n)?.skip_txfm.at(pt)?);
        }
        if self.mi(xd)?.skip_mode {
            ctx += SKIP_CONTEXTS >> 1;
        }
        Ok(t.fc.read(&mut t.r, &SKIP_TXFM_CDFS, [ctx])? == 1)
    }

    /// read_intra_segment_id and read_segment_id.
    fn read_intra_segment_id(&mut self, t: &mut TileState, xd: &Xd, bsize: usize, skip: bool) -> Result<usize> {
        let seg = &self.h.frame.seg;
        if !seg.enabled {
            return Ok(0);
        }
        let (r, c) = (xd.mi_row, xd.mi_col);
        let cols = self.f.mi_cols;
        let at = |r: usize, c: usize| -> Result<i64> { Ok(i64::from(*self.seg_map.at(r * cols + c)?)) };
        // av2_get_spatial_seg_pred
        let (mut ul, mut l, mut u) = (-1, -1, -1);
        if xd.up_available && xd.left_available {
            ul = at(r - 1, c - 1)?;
        }
        if xd.up_available {
            u = at(r - 1, c)?;
        }
        if xd.left_available {
            l = at(r, c - 1)?;
        }
        let cdf_num = if ul < 0 {
            0
        } else if ul == u && ul == l {
            2
        } else if ul == u || ul == l || u == l {
            1
        } else {
            0
        };
        let pred = if u == -1 {
            if l == -1 { 0 } else { l }
        } else if l == -1 || ul == u {
            u
        } else {
            l
        };
        let id = if skip && !self.has_lossless_segment {
            pred
        } else {
            let ext = self.h.seq.enable_ext_seg && t.fc.read(&mut t.r, &SEG_SEG_ID_EXT_FLAG_CDF, [cdf_num])? == 1;
            let field = if ext { &SEG_SPATIAL_PRED_SEG_CDF1 } else { &SEG_SPATIAL_PRED_SEG_CDF };
            let coded = t.fc.read(&mut t.r, field, [cdf_num])? as i64 + if ext { MAX_SEGMENTS_8 as i64 } else { 0 };
            let id = neg_deinterleave(coded, pred, i64::from(seg.last_active_segid) + 1);
            if id < 0 || id > i64::from(seg.last_active_segid) {
                return Err(Error::Invalid("segment id"));
            }
            id
        };
        let (w, hh) = (mi_w(bsize)?.min(cols - c), mi_h(bsize)?.min(self.f.mi_rows - r));
        for y in 0..hh {
            for x in 0..w {
                *self.seg_map.at_mut((r + y) * cols + c + x)? = id as u8;
            }
        }
        Ok(id as usize)
    }

    /// read_gdf.
    fn read_gdf(&mut self, t: &mut TileState, xd: &Xd) -> Result<()> {
        let g = &self.h.frame.gdf;
        if !self.h.seq.enable_gdf || g.mode < 2 || g.block_num <= 1 {
            return Ok(());
        }
        if xd.mi_row % self.mib_size != 0 || xd.mi_col % self.mib_size != 0 {
            return Ok(());
        }
        let bs = g.block_size as usize;
        for r in xd.mi_row..xd.mi_row + self.mib_size {
            for c in xd.mi_col..xd.mi_col + self.mib_size {
                // gdf_get_block_idx
                let (y, x) = (r * 4, c * 4);
                if y % bs == 0 && x % bs == 0 {
                    let idx = (y / bs) * g.block_num_w as usize + x / bs;
                    if idx < g.block_num as usize {
                        t.fc.read(&mut t.r, &GDF_CDF, [])?;
                    }
                }
            }
        }
        Ok(())
    }

    /// read_cdef.
    fn read_cdef(&mut self, t: &mut TileState, xd: &Xd) -> Result<()> {
        let c = &self.h.frame.cdef;
        if !self.h.seq.enable_cdef || !c.enable {
            return Ok(());
        }
        let skip = self.mi(xd)?.skip_txfm[0];
        let mask = self.mib_size - 1;
        if xd.mi_row & mask == 0 && xd.mi_col & mask == 0 {
            self.cdef_transmitted = [false; 16];
        }
        // av2_get_cdef_transmitted_index
        let index_mask = ((u32::MAX << (32 - CDEF_SB_SHIFT)) >> (32 - CDEF_SB_SHIFT - MI_IN_CDEF_LINEAR_LOG2)) as usize;
        let index = CDEF_IN_SB_STRIDE * ((xd.mi_row & index_mask) >> MI_IN_CDEF_LINEAR_LOG2)
            + ((xd.mi_col & index_mask) >> MI_IN_CDEF_LINEAR_LOG2);
        let cdef_size = 1usize << MI_IN_CDEF_LINEAR_LOG2;
        let bmask = !(cdef_size - 1);
        let unit = self.grid.idx(xd.mi_row & bmask, xd.mi_col & bmask)?;
        let unit_rec =
            || self.grid.ptr.at(unit).ok().copied().flatten().ok_or(Error::Invalid("CDEF unit's first block"));
        let transmitted = *self.cdef_transmitted.at(index)?;
        if !transmitted && (c.on_skip_txfm || !skip) {
            let n = c.strengths.len();
            let strength = if n == 1 {
                0
            } else {
                let ctx = self.cdef_context(xd)?;
                if t.fc.read(&mut t.r, &CDEF_STRENGTH_INDEX0_CDF, [ctx])? == 1 {
                    0
                } else if n == 2 {
                    1
                } else {
                    let cdf = CDEF_CDF;
                    let slot = t.fc.cdf_mut(&cdf, [n - 3])?;
                    t.r.read_symbol(slot, n - 1)? as i32 + 1
                }
            };
            let rec = unit_rec()?;
            self.grid.mi_mut(rec)?.cdef_strength = strength;
            *self.cdef_transmitted.at_mut(index)? = true;
        } else if !transmitted && !c.on_skip_txfm && skip {
            let rec = unit_rec()?;
            self.grid.mi_mut(rec)?.cdef_strength = -1;
        }
        Ok(())
    }

    /// av2_get_cdef_context.
    fn cdef_context(&self, xd: &Xd) -> Result<usize> {
        let cdef_size = 1isize << MI_IN_CDEF_LINEAR_LOG2;
        let bmask = !(cdef_size - 1);
        let (r, c) = (xd.mi_row as isize, xd.mi_col as isize);
        let n0 = if c - cdef_size >= 0 { self.grid.at(r & bmask, (c - cdef_size) & bmask) } else { None };
        let ar = r - cdef_size;
        let mib_log2 = self.mib_size.trailing_zeros();
        let n1 =
            if ar >= 0 && (r >> mib_log2) == (ar >> mib_log2) { self.grid.at(ar & bmask, c & bmask) } else { None };
        let t = &self.tile;
        let inside = |m: &Mi| {
            let (rr, cc) = ((m.mi_row_start as isize & bmask) as usize, (m.mi_col_start as isize & bmask) as usize);
            rr >= t.mi_row_start && cc >= t.mi_col_start && rr < t.mi_row_end && cc < t.mi_col_end
        };
        let a0 = self.grid.opt(n0).filter(|m| inside(m));
        let a1 = self.grid.opt(n1).filter(|m| inside(m));
        Ok(match (a0, a1) {
            (Some(m0), Some(m1)) => {
                let (z0, z1) = (m0.cdef_strength == 0, m1.cdef_strength == 0);
                if (m0.mi_row_start as isize & bmask) != (m1.mi_row_start as isize & bmask)
                    || (m0.mi_col_start as isize & bmask) != (m1.mi_col_start as isize & bmask)
                {
                    if z0 && z1 { 3 } else { usize::from(z0 || z1) }
                } else if z0 {
                    2
                } else {
                    0
                }
            }
            (Some(m), None) | (None, Some(m)) => {
                if m.cdef_strength == 0 {
                    2
                } else {
                    0
                }
            }
            (None, None) => 0,
        })
    }

    /// read_ccso.
    fn read_ccso(&mut self, t: &mut TileState, xd: &Xd) -> Result<()> {
        let blk_log2 = self.h.frame.ccso.blk_size_log2 as usize;
        let m = (1usize << (blk_log2 - 2)) - 1;
        let (r, c) = (xd.mi_row, xd.mi_col);
        let unit = self.grid.idx(r & !m, c & !m)?;
        let here = self.grid.idx(r, c)?;
        let planes: Vec<bool> = self.h.frame.ccso.planes.iter().map(|p| p.enable).collect();
        for (pli, &enabled) in planes.iter().enumerate() {
            if !enabled {
                continue;
            }
            if r & m == 0 && c & m == 0 {
                let ctx = self.ccso_context(xd, pli, m)?;
                let idc = t.fc.read(&mut t.r, &CCSO_CDF, [pli, ctx])? as u8;
                let rec = self.grid.ptr.at(unit)?.ok_or(Error::Invalid("CCSO unit's first block"))?;
                *self.grid.mi_mut(rec)?.ccso_blk.at_mut(pli)? = idc;
            } else if self.ccso_inside_tile(self.mi(xd)?, m) {
                let src = self.grid.ptr.at(unit)?.ok_or(Error::Invalid("CCSO unit's first block"))?;
                let v = *self.grid.mi(src)?.ccso_blk.at(pli)?;
                let dst = self.grid.ptr.at(here)?.ok_or(Error::Invalid("block record"))?;
                *self.grid.mi_mut(dst)?.ccso_blk.at_mut(pli)? = v;
            }
        }
        Ok(())
    }

    /// av2_check_ccso_mbmi_inside_tile.
    fn ccso_inside_tile(&self, m: &Mi, mask: usize) -> bool {
        let t = &self.tile;
        let (rr, cc) = (m.mi_row_start & !mask, m.mi_col_start & !mask);
        rr >= t.mi_row_start && cc >= t.mi_col_start && rr < t.mi_row_end && cc < t.mi_col_end
    }

    /// av2_get_ccso_context.
    fn ccso_context(&self, xd: &Xd, plane: usize, mask: usize) -> Result<usize> {
        let n0 = self.grid.opt(xd.neighbors[0]).filter(|m| self.ccso_inside_tile(m, mask));
        let n1 = self.grid.opt(xd.neighbors[1]).filter(|m| self.ccso_inside_tile(m, mask));
        let flag = |m: &Mi| m.ccso_blk.get(plane).copied().unwrap_or(0) != 0;
        Ok(match (n0, n1) {
            (Some(a), Some(b)) => {
                if (a.mi_row_start & !mask) != (b.mi_row_start & !mask)
                    || (a.mi_col_start & !mask) != (b.mi_col_start & !mask)
                {
                    if flag(a) && flag(b) { 3 } else { usize::from(flag(a) || flag(b)) }
                } else if flag(a) {
                    2
                } else {
                    0
                }
            }
            (Some(a), None) | (None, Some(a)) => {
                if flag(a) {
                    2
                } else {
                    0
                }
            }
            (None, None) => 0,
        })
    }

    /// read_intrabc_info, without the DV prediction (it only feeds
    /// reconstruction; the parse needs the coded difference alone).
    fn read_intrabc_info(&mut self, t: &mut TileState, xd: &Xd, bsize: usize) -> Result<()> {
        let tree = self.tree_type;
        if !self.mi(xd)?.is_intrabc(tree) {
            return Ok(());
        }
        let _ = bsize;
        let force_int = self.h.frame.cur_frame_force_integer_mv;
        {
            let m = self.mi_mut(xd)?;
            m.mode = DC_PRED;
            m.fsc_mode = [0, 0];
            m.uv_mode = UV_DC_PRED;
            // set_default_intraBC_bv_precision: {ONE_PEL, QTR_PEL}, the last.
            m.pb_mv_precision = if force_int { MV_PRECISION_ONE_PEL } else { MV_PRECISION_QTR_PEL };
        }
        let mode = t.fc.read(&mut t.r, &INTRABC_MODE_CDF, [])?;
        // read_intrabc_drl_idx
        let max_cnt = self.h.frame.max_bvp_drl_bits as usize + 1;
        let mut drl = 0;
        for idx in 0..max_cnt.saturating_sub(1) {
            let b = usize::from(t.r.read_bit()?);
            drl = idx + b;
            if b == 0 {
                break;
            }
        }
        if !force_int && mode == 0 {
            let i = t.fc.read(&mut t.r, &INTRABC_BV_PRECISION_CDF, [0])?;
            self.mi_mut(xd)?.pb_mv_precision = [MV_PRECISION_ONE_PEL, MV_PRECISION_QTR_PEL].at(i).copied()?;
        }
        let prec = self.mi(xd)?.pb_mv_precision;
        let mut diff = (0, 0);
        if mode != 1 {
            diff = self.read_mv(t, prec)?;
            if diff.0 != 0 && t.r.read_literal(1)? == 1 {
                diff.0 = -diff.0;
            }
            if diff.1 != 0 && t.r.read_literal(1)? == 1 {
                diff.1 = -diff.1;
            }
        }
        {
            let m = self.mi_mut(xd)?;
            m.intrabc_mode = mode;
            m.intrabc_drl_idx = drl;
            m.mv_diff = diff;
        }
        // av2_allow_intrabc_morph_pred: enable_intra_bawp (= seq enable_bawp)
        // and screen content tools, in an intra frame.
        if self.h.seq.enable_bawp && self.h.frame.allow_screen_content_tools {
            let ctx = xd
                .neighbors
                .iter()
                .flatten()
                .filter_map(|&n| self.grid.mi(n).ok())
                .filter(|m| m.is_intrabc(tree) && m.morph_pred)
                .count();
            let v = t.fc.read(&mut t.r, &MORPH_PRED_CDF, [ctx])? == 1;
            self.mi_mut(xd)?.morph_pred = v;
        }
        Ok(())
    }

    /// read_mv for a DV (ndvc), not adaptive: (row, col) magnitudes.
    fn read_mv(&mut self, t: &mut TileState, precision: usize) -> Result<(i32, i32)> {
        let num_class = MAX_NUM_SHELL_CLASS - (MV_PRECISION_ONE_EIGHTH_PEL - precision);
        let (n0, n1) = (num_class >> 1, num_class - (num_class >> 1));
        let set = t.fc.read(&mut t.r, &NDVC_JOINT_SHELL_SET_CDF, [])?;
        let class = if set == 1 {
            if precision == MV_PRECISION_ONE_EIGHTH_PEL {
                let slot = t.fc.cdf_mut(&NDVC_JOINT_SHELL_CLASS_CDF_1, [precision])?;
                let mut c = n0 + t.r.read_symbol(slot, n1 - 1)?;
                if c >= MAX_NUM_SHELL_CLASS - 2 {
                    c += t.fc.read(&mut t.r, &NDVC_JOINT_SHELL_LAST_TWO_CLASSES_CDF, [])?;
                }
                c
            } else {
                let slot = t.fc.cdf_mut(&NDVC_JOINT_SHELL_CLASS_CDF_1, [precision])?;
                n0 + t.r.read_symbol(slot, n1)?
            }
        } else {
            let slot = t.fc.cdf_mut(&NDVC_JOINT_SHELL_CLASS_CDF_0, [precision])?;
            t.r.read_symbol(slot, n0)?
        };
        if class >= num_class {
            return Err(Error::Invalid("MV shell class"));
        }
        let offset = if class < 2 {
            t.fc.read(&mut t.r, &NDVC_SHELL_OFFSET_LOW_CLASS_CDF, [class])?
        } else if class == 2 {
            // read_truncated_unary_mvd(3)
            let mut v = 0;
            for bit_idx in 0..3 {
                let b = if bit_idx == 0 {
                    t.fc.read(&mut t.r, &NDVC_SHELL_OFFSET_CLASS2_CDF, [])?
                } else {
                    t.r.read_literal(1)? as usize
                };
                v = bit_idx + b;
                if b == 0 {
                    break;
                }
            }
            v
        } else {
            let mut v = 0;
            for i in 0..class {
                v |= t.fc.read(&mut t.r, &NDVC_SHELL_OFFSET_OTHER_CLASS_CDF, [0, i])? << i;
            }
            v
        };
        let shell = offset + if class == 0 { 0 } else { 1 << class };
        let (row, col) = if shell > 0 {
            let max_pair = shell >> 1;
            let mut pair = 0;
            if max_pair > 0 {
                // read_tu_quasi_uniform
                let max_bits = max_pair.min(MAX_COL_TRUNCATED_UNARY_VAL);
                for bit_idx in 0..max_bits {
                    let ctx = bit_idx.min(NUM_CTX_COL_MV_GTX - 1);
                    let b = t.fc.read(&mut t.r, &NDVC_COL_MV_GREATER_FLAGS_CDF, [ctx])?;
                    pair = bit_idx + b;
                    if b == 0 {
                        break;
                    }
                }
                if max_pair > MAX_COL_TRUNCATED_UNARY_VAL && pair == MAX_COL_TRUNCATED_UNARY_VAL {
                    let rem = self.read_quniform(t, (max_pair - MAX_COL_TRUNCATED_UNARY_VAL + 1) as u32)?;
                    pair = rem as usize + MAX_COL_TRUNCATED_UNARY_VAL;
                }
            }
            let col = if pair == max_pair && shell % 2 == 0 {
                max_pair
            } else {
                let ctx = class.min(NUM_CTX_COL_MV_INDEX - 1);
                if t.fc.read(&mut t.r, &NDVC_COL_MV_INDEX_CDF, [ctx])? == 0 { pair } else { shell - pair }
            };
            (shell - col, col)
        } else {
            (0, 0)
        };
        let shift = MV_PRECISION_ONE_EIGHTH_PEL - precision;
        Ok(((row << shift) as i32, (col << shift) as i32))
    }

    /// avm_read_primitive_quniform (binary_codes_reader.c): its second bit
    /// is an avm_read_bit, which AVM's trace records twice.
    pub(crate) fn read_quniform(&mut self, t: &mut TileState, n: u32) -> Result<u32> {
        if n <= 1 {
            return Ok(0);
        }
        let l = 32 - n.leading_zeros();
        let m = (1u32 << l) - n;
        let v = t.r.read_literal(l - 1)?;
        Ok(if v < m { v } else { (v << 1) - m + u32::from(t.r.read_bit()?) })
    }

    /// read_intra_luma_mode.
    fn read_intra_luma_mode(&mut self, t: &mut TileState, xd: &Xd) -> Result<()> {
        let ar = Mi::joint_mode(self.grid.opt(xd.above_right));
        let bl = Mi::joint_mode(self.grid.opt(xd.bottom_left));
        let ctx = usize::from(ar >= NON_DIRECTIONAL_MODES_COUNT) + usize::from(bl >= NON_DIRECTIONAL_MODES_COUNT);
        let set = t.fc.read(&mut t.r, &Y_MODE_SET_CDF, [])?;
        let mode_idx = if set == 0 {
            let mut i = t.fc.read(&mut t.r, &Y_MODE_IDX_CDF, [ctx])?;
            if i == LUMA_INTRA_MODE_INDEX_COUNT - 1 {
                i += t.fc.read(&mut t.r, &Y_MODE_IDX_OFFSET_CDF, [ctx])?;
            }
            i
        } else {
            FIRST_MODE_COUNT + (set - 1) * SECOND_MODE_COUNT + t.r.read_literal(4)? as usize
        };
        if mode_idx >= LUMA_MODE_COUNT {
            return Err(Error::Invalid("luma mode index"));
        }
        let list = self.y_intra_mode_list(xd, bl, ar)?;
        let joint = *list.at(mode_idx)?;
        let (mode, delta) = if joint < NON_DIRECTIONAL_MODES_COUNT {
            (joint, 0)
        } else {
            let j = joint - NON_DIRECTIONAL_MODES_COUNT;
            (
                j / TOTAL_ANGLE_DELTA_COUNT + NON_DIRECTIONAL_MODES_COUNT,
                (j % TOTAL_ANGLE_DELTA_COUNT) as i32 - MAX_ANGLE_DELTA as i32,
            )
        };
        let m = self.mi_mut(xd)?;
        m.joint_y_mode_delta_angle = joint;
        m.mode = t2_1(&REORDERED_Y_MODE, mode)?;
        m.angle_delta[0] = delta;
        m.y_mode_idx = mode_idx;
        Ok(())
    }

    /// get_y_intra_mode_set: the luma mode list, neighbours' modes first.
    fn y_intra_mode_list(&self, xd: &Xd, left_joint: usize, above_joint: usize) -> Result<Vec<usize>> {
        let sb = self.mi(xd)?.sb_type[0];
        let mut nb = [left_joint, above_joint];
        let left_dir = nb[0] >= NON_DIRECTIONAL_MODES_COUNT;
        let above_dir = nb[1] >= NON_DIRECTIONAL_MODES_COUNT;
        let large = bw(sb)? * bh(sb)? > 64;
        let small = sb < BLOCK_8X8;
        let mut selected = [false; LUMA_MODE_COUNT];
        let mut list = Vec::with_capacity(LUMA_MODE_COUNT);
        let add = |m: usize, list: &mut Vec<usize>, sel: &mut [bool; LUMA_MODE_COUNT]| -> Result<()> {
            if !*sel.at(m)? {
                *sel.at_mut(m)? = true;
                list.push(m);
            }
            Ok(())
        };
        for i in 0..NON_DIRECTIONAL_MODES_COUNT {
            add(i, &mut list, &mut selected)?;
        }
        if !small {
            let mut cnt = usize::from(above_dir) + usize::from(left_dir);
            if cnt == 2 && nb[0] == nb[1] {
                cnt = 1;
            }
            if cnt == 1 && !left_dir {
                nb[0] = nb[1];
            }
            for &m in nb.iter().take(cnt) {
                list.push(m);
                *selected.at_mut(m)? = true;
            }
            if large {
                for i in 0..4 {
                    for &m in nb.iter().take(cnt) {
                        let left = (m + 56 - i + (56 - NON_DIRECTIONAL_MODES_COUNT - 1) - 56) % 56
                            + NON_DIRECTIONAL_MODES_COUNT;
                        let right = (m + i - (NON_DIRECTIONAL_MODES_COUNT - 1)) % 56 + NON_DIRECTIONAL_MODES_COUNT;
                        add(left, &mut list, &mut selected)?;
                        add(right, &mut list, &mut selected)?;
                    }
                }
            }
        }
        for &d in DEFAULT_MODE_LIST_Y.iter() {
            if list.len() >= LUMA_MODE_COUNT {
                break;
            }
            add(d as usize + NON_DIRECTIONAL_MODES_COUNT, &mut list, &mut selected)?;
        }
        Ok(list)
    }

    /// read_intra_uv_mode.
    fn read_intra_uv_mode(&mut self, t: &mut TileState, xd: &Xd, cfl_allowed: bool) -> Result<()> {
        if cfl_allowed {
            let cfl = |i: Option<usize>| self.grid.opt(i).is_some_and(|m| m.uv_mode == UV_CFL_PRED);
            let ctx = usize::from(cfl(xd.chroma_above)) + usize::from(cfl(xd.chroma_left));
            if t.fc.read(&mut t.r, &CFL_CDF, [ctx])? == 1 {
                let m = self.mi_mut(xd)?;
                m.uv_mode = UV_CFL_PRED;
                m.angle_delta[1] = 0;
                return Ok(());
            }
        }
        let mode = self.mi(xd)?.mode;
        let ctx = usize::from((V_PRED..=D67_PRED).contains(&mode));
        let mut idx = t.fc.read(&mut t.r, &UV_MODE_CDF, [ctx])?;
        if idx == CHROMA_INTRA_MODE_INDEX_COUNT - 1 {
            idx += t.r.read_literal(3)? as usize;
        }
        if idx >= UV_INTRA_MODES - 1 {
            return Err(Error::Invalid("chroma intra mode index"));
        }
        // get_uv_intra_mode_set
        let mut list = Vec::with_capacity(UV_INTRA_MODES - 1);
        if (V_PRED..=D67_PRED).contains(&mode) {
            list.push(mode);
        }
        list.extend([UV_DC_PRED, UV_SMOOTH_PRED, UV_SMOOTH_V_PRED, UV_SMOOTH_H_PRED, UV_PAETH_PRED]);
        for &d in DEFAULT_MODE_LIST_UV.iter() {
            if !list.contains(&(d as usize)) {
                list.push(d as usize);
            }
        }
        let uv = *list.at(idx)?;
        let m = self.mi_mut(xd)?;
        m.uv_mode = uv;
        m.angle_delta[1] = if uv == m.mode { m.angle_delta[0] } else { 0 };
        Ok(())
    }

    /// get_bsize_base for plane U.
    fn chroma_bsize_base(&self, xd: &Xd) -> Result<usize> {
        let m = self.mi(xd)?;
        Ok(if self.tree_type == SHARED_PART {
            m.chroma_ref.bsize_base
        } else {
            *m.sb_type.at(usize::from(self.tree_type == CHROMA_PART))?
        })
    }

    /// is_cfl_allowed.
    fn cfl_allowed(&self, xd: &Xd) -> Result<bool> {
        if self.tree_type == LUMA_PART || !self.h.seq.enable_cfl_intra {
            return Ok(false);
        }
        if self.cfl_allowed_in_sdp == CFL_DISALLOWED_FOR_CHROMA {
            return Ok(false);
        }
        let bsize = self.chroma_bsize_base(xd)?;
        let pb = plane_bsize(bsize, self.f.ss_x, self.f.ss_y)?;
        let max_uv = t2_1(&MAX_TXSIZE_RECT_LOOKUP, pb)?;
        if bw(pb)? > t2_1(&TX_SIZE_WIDE, max_uv)? || bh(pb)? > t2_1(&TX_SIZE_HIGH, max_uv)? {
            return Ok(false);
        }
        Ok(bw(bsize)? <= CFL_BUF_LINE && bh(bsize)? <= CFL_BUF_LINE)
    }

    /// is_mhccp_allowed.
    fn mhccp_allowed(&self, xd: &Xd) -> Result<bool> {
        if self.tree_type == LUMA_PART || !self.h.seq.enable_mhccp {
            return Ok(false);
        }
        if self.cfl_allowed_in_sdp == CFL_DISALLOWED_FOR_CHROMA {
            return Ok(false);
        }
        let bsize = self.chroma_bsize_base(xd)?;
        let pb = plane_bsize(bsize, self.f.ss_x, self.f.ss_y)?;
        if bh(pb)? == 4 && bw(pb)? == 4 {
            return Ok(false);
        }
        let max_uv = adjusted_tx_size(t2_1(&MAX_TXSIZE_RECT_LOOKUP, pb)?);
        if bw(pb)? > t2_1(&TX_SIZE_WIDE, max_uv)? || bh(pb)? > t2_1(&TX_SIZE_HIGH, max_uv)? {
            return Ok(false);
        }
        Ok(bw(bsize)? <= CFL_BUF_LINE / 2 && bh(bsize)? <= CFL_BUF_LINE / 2)
    }

    /// read_cfl_alphas: (alpha index, joint sign).
    fn read_cfl_alphas(&mut self, t: &mut TileState) -> Result<(usize, usize)> {
        let js = t.fc.read(&mut t.r, &CFL_SIGN_CDF, [])?;
        let sign_u = ((js + 1) * 11) >> 5;
        let sign_v = (js + 1) - CFL_SIGNS * sign_u;
        let mut idx = 0;
        if sign_u != CFL_SIGN_ZERO {
            let ctx = js + 1 - CFL_SIGNS;
            idx = t.fc.read(&mut t.r, &CFL_ALPHA_CDF, [ctx])? << CFL_ALPHABET_SIZE_LOG2;
        }
        if sign_v != CFL_SIGN_ZERO {
            let ctx = sign_v * CFL_SIGNS + sign_u - CFL_SIGNS;
            idx += t.fc.read(&mut t.r, &CFL_ALPHA_CDF, [ctx])?;
        }
        Ok((idx, js))
    }

    /// read_palette_mode_info and read_palette_colors_y.
    fn read_palette_mode_info(&mut self, t: &mut TileState, xd: &Xd) -> Result<()> {
        if self.mi(xd)?.mode != DC_PRED || self.tree_type == CHROMA_PART {
            return Ok(());
        }
        if t.fc.read(&mut t.r, &PALETTE_Y_MODE_CDF, [])? == 0 {
            return Ok(());
        }
        let n = t.fc.read(&mut t.r, &PALETTE_Y_SIZE_CDF, [])? + 2;
        let bd = self.h.seq.bit_depth;
        // av2_get_palette_cache
        let m = self.mi(xd)?;
        let row = m.mi_row_start << 2;
        let above = if row % (1 << MIN_SB_SIZE_LOG2) != 0 { self.grid.opt(xd.above) } else { None };
        let left = self.grid.opt(xd.left);
        let (an, ln) = (above.map_or(0, |m| m.palette_size[0]), left.map_or(0, |m| m.palette_size[0]));
        let mut cache = Vec::new();
        let (mut ai, mut li) = (0, 0);
        while ai < an && li < ln {
            cache.push(*above.map(|m| &m.palette_colors).ok_or(Error::Invalid("palette"))?.at(ai)?);
            ai += 1;
            cache.push(*left.map(|m| &m.palette_colors).ok_or(Error::Invalid("palette"))?.at(li)?);
            li += 1;
        }
        while ai < an {
            cache.push(*above.map(|m| &m.palette_colors).ok_or(Error::Invalid("palette"))?.at(ai)?);
            ai += 1;
        }
        while li < ln {
            cache.push(*left.map(|m| &m.palette_colors).ok_or(Error::Invalid("palette"))?.at(li)?);
            li += 1;
        }
        let mut colors = [0u16; PALETTE_MAX_SIZE];
        let mut idx = 0;
        for &c in &cache {
            if idx >= n {
                break;
            }
            if t.r.read_bit()? {
                *colors.at_mut(idx)? = c;
                idx += 1;
            }
        }
        if idx < n {
            *colors.at_mut(idx)? = t.r.read_literal(bd)? as u16;
            idx += 1;
            if idx < n {
                let mut bits = bd - 3 + t.r.read_literal(2)?;
                let maxv = (1i64 << bd) - 1;
                let mut range = (1i64 << bd) - i64::from(*colors.at(idx - 1)?) - 1;
                while idx < n {
                    let delta = i64::from(t.r.read_literal(bits)?) + 1;
                    let prev = i64::from(*colors.at(idx - 1)?);
                    let v = (prev + delta).clamp(0, maxv);
                    *colors.at_mut(idx)? = v as u16;
                    range -= v - prev;
                    // avm_ceil_log2(range)
                    let cl = if range < 2 { 0 } else { 64 - ((range - 1) as u64).leading_zeros() };
                    bits = bits.min(cl);
                    idx += 1;
                }
            }
        }
        colors.get_mut(..n).ok_or(Error::Invalid("palette size"))?.sort_unstable();
        let m = self.mi_mut(xd)?;
        m.palette_size[0] = n;
        m.palette_colors.get_mut(..PALETTE_MAX_SIZE).ok_or(Error::Invalid("palette"))?.copy_from_slice(&colors);
        Ok(())
    }

    /// av2_visit_palette / av2_decode_palette_tokens: the luma colour map.
    pub(crate) fn read_palette_tokens(&mut self, t: &mut TileState, xd: &Xd) -> Result<()> {
        let m = self.mi(xd)?;
        if m.is_inter(self.tree_type) || self.tree_type == CHROMA_PART || m.palette_size[0] == 0 {
            return Ok(());
        }
        let n = m.palette_size[0];
        let bsize = m.sb_type[0];
        // av2_get_block_dimensions, plane 0
        let (w, h) = (bw(bsize)?, bh(bsize)?);
        let rows = if xd.mb_to_bottom_edge >= 0 { h } else { (h as i64 + (xd.mb_to_bottom_edge >> 3)) as usize };
        let cols = if xd.mb_to_right_edge >= 0 { w } else { (w as i64 + (xd.mb_to_right_edge >> 3)) as usize };
        let mut map = vec![0u8; w * h];
        let direction = w < 64 && h < 64 && t.r.read_bit()?;
        let (lim1, lim2) = if direction { (rows, cols) } else { (cols, rows) };
        let mut prev_flag = 0;
        for ax2 in 0..lim2 {
            let ctx = if ax2 == 0 { 3 } else { prev_flag };
            let flag = t.fc.read(&mut t.r, &IDENTITY_ROW_CDF_Y, [ctx])?;
            if flag == 2 && ax2 == 0 {
                return Err(Error::Invalid("palette row copy on the first line"));
            }
            for ax1 in 0..lim1 {
                let (y, x) = if direction { (ax1, ax2) } else { (ax2, ax1) };
                let v = if flag == 2 {
                    if direction { *map.at(y * w + x - 1)? } else { *map.at((y - 1) * w + x)? }
                } else if flag == 1 && ax1 > 0 {
                    if direction { *map.at((y - 1) * w + x)? } else { *map.at(y * w + x - 1)? }
                } else if ax2 == 0 && ax1 == 0 {
                    self.read_uniform(t, n as u32)? as u8
                } else {
                    let (ctx, order) = color_index_ctx(&map, w, y, x)?;
                    let slot = t.fc.cdf_mut(&PALETTE_Y_COLOR_INDEX_CDF, [n - PALETTE_MIN_SIZE, ctx])?;
                    let i = t.r.read_symbol(slot, n)?;
                    *order.at(i)?
                };
                *map.at_mut(y * w + x)? = v;
            }
            prev_flag = flag;
        }
        Ok(())
    }

    /// av2_read_uniform (decoder.h).
    fn read_uniform(&mut self, t: &mut TileState, n: u32) -> Result<u32> {
        let l = 32 - n.leading_zeros();
        let m = (1u32 << l) - n;
        let v = t.r.read_literal(l - 1)?;
        Ok(if v < m { v } else { (v << 1) - m + t.r.read_literal(1)? })
    }
}

/// derive_color_index_ctx: the context and the colour order.
fn color_index_ctx(map: &[u8], stride: usize, r: usize, c: usize) -> Result<(usize, [u8; PALETTE_MAX_SIZE])> {
    let mut order = [0u8; PALETTE_MAX_SIZE];
    let mut status = [false; PALETTE_MAX_SIZE];
    let mut cnt = 0;
    let mut put =
        |order: &mut [u8; PALETTE_MAX_SIZE], status: &mut [bool; PALETTE_MAX_SIZE], idx: usize, v: u8| -> Result<()> {
            *order.at_mut(idx)? = v;
            *status.at_mut(v as usize)? = true;
            cnt += 1;
            Ok(())
        };
    let mut ctx = 0;
    if r > 0 && c > 0 {
        let n = [*map.at(r * stride + c - 1)?, *map.at((r - 1) * stride + c - 1)?, *map.at((r - 1) * stride + c)?];
        if n[0] == n[1] && n[0] == n[2] {
            ctx = 4;
            put(&mut order, &mut status, 0, n[0])?;
        } else if n[0] == n[2] {
            ctx = 3;
            put(&mut order, &mut status, 0, n[0])?;
            put(&mut order, &mut status, 1, n[1])?;
        } else if n[0] == n[1] {
            ctx = 2;
            put(&mut order, &mut status, 0, n[0])?;
            put(&mut order, &mut status, 1, n[2])?;
        } else if n[1] == n[2] {
            ctx = 2;
            put(&mut order, &mut status, 0, n[2])?;
            put(&mut order, &mut status, 1, n[0])?;
        } else {
            ctx = 1;
            put(&mut order, &mut status, 0, n[0])?;
            put(&mut order, &mut status, 1, n[2])?;
            put(&mut order, &mut status, 2, n[1])?;
        }
    } else if c == 0 && r > 0 {
        put(&mut order, &mut status, 0, *map.at((r - 1) * stride + c)?)?;
    } else if c > 0 && r == 0 {
        put(&mut order, &mut status, 0, *map.at(r * stride + c - 1)?)?;
    }
    let mut w = cnt;
    for (i, _) in status.iter().enumerate().filter(|(_, s)| !**s) {
        *order.at_mut(w)? = i as u8;
        w += 1;
    }
    Ok((ctx, order))
}

/// av2_get_adjusted_tx_size.
pub(crate) fn adjusted_tx_size(tx: usize) -> usize {
    match tx {
        TX_64X64 | TX_64X32 | TX_32X64 => TX_32X32,
        TX_64X16 => TX_32X16,
        TX_16X64 => TX_16X32,
        TX_64X8 => TX_32X8,
        TX_8X64 => TX_8X32,
        TX_64X4 => TX_32X4,
        TX_4X64 => TX_4X32,
        _ => tx,
    }
}

/// A 1-D table entry as usize.
pub(crate) fn t2_1<T: Copy + TryInto<i64>>(table: &[T], i: usize) -> Result<usize> {
    let v: i64 = (*table.at(i)?).try_into().map_err(|_| Error::Invalid("table value"))?;
    usize::try_from(v).map_err(|_| Error::Invalid("table value"))
}
