//! A coding block's residual syntax: its transform size, which transform
//! blocks it codes and in what order, and each block's transform type. A port
//! of the parse halves of parse_decode_block, decode_token_recon_block and
//! decode_reconstruct_tx (av2/decoder/decodeframe.c), av2_read_tx_type,
//! av2_read_sec_tx_type and av2_read_cctx_type (decodemv.c), and the
//! entropy-context and tx-type helpers they use (blockd.h, txb_common.h,
//! entropy.h, reconintra.h). The coefficients themselves are in txb.rs.
//!
//! Only TX_MODE_LARGEST is ported: every block uses one transform of its
//! largest size, so no transform partition is ever read. Lossless segments
//! never reach here (FrameParse refuses them).

use crate::avm_defs::*;
use crate::blk::*;
use crate::block::{Mi, Xd, adjusted_tx_size, t2_1};
use crate::cdf_tables::*;
use crate::error::{Error, Result};
use crate::parse::{FrameParse, TileState};

/// ENTROPY_CONTEXT, per plane: one byte per 4x4 column of the frame
/// (above) and per 4x4 row of the superblock (left). Each holds the
/// cul_level of the last transform block coded there: the low 3 bits its
/// level sum clipped at 7, the next 2 the sign of its DC.
pub(crate) struct EntropyContexts {
    pub above: [Vec<u8>; 3],
    pub left: [[u8; MAX_MIB_SIZE]; 3],
}

impl EntropyContexts {
    pub fn new(cols: usize) -> EntropyContexts {
        EntropyContexts { above: [vec![0; cols], vec![0; cols], vec![0; cols]], left: [[0; MAX_MIB_SIZE]; 3] }
    }
}

/// TXB_CTX.
#[derive(Debug, Clone, Copy)]
pub(crate) struct TxbCtx {
    pub skip: usize,
    pub dc_sign: usize,
}

/// get_uv_mode. A function-local table in blockd.h, so not generated:
/// every UV mode is its luma mode, except CFL, which is DC.
fn uv_to_y_mode(mode: usize) -> Result<usize> {
    match mode {
        m if m < UV_CFL_PRED => Ok(m),
        UV_CFL_PRED => Ok(DC_PRED),
        _ => Err(Error::Invalid("chroma intra mode")),
    }
}

/// intra_mode_to_tx_type's table (function-local in blockd.h).
const INTRA_MODE_TO_TX_TYPE: [usize; 13] = [
    DCT_DCT,   // DC_PRED
    ADST_DCT,  // V_PRED
    DCT_ADST,  // H_PRED
    DCT_DCT,   // D45_PRED
    ADST_ADST, // D135_PRED
    ADST_DCT,  // D113_PRED
    DCT_ADST,  // D157_PRED
    DCT_ADST,  // D203_PRED
    ADST_DCT,  // D67_PRED
    ADST_ADST, // SMOOTH_PRED
    ADST_DCT,  // SMOOTH_V_PRED
    DCT_ADST,  // SMOOTH_H_PRED
    ADST_ADST, // PAETH_PRED
];

/// get_txb_ctx's skip_contexts[top][left] (function-local in txb_common.h).
const SKIP_CONTEXTS: [[usize; 5]; 5] =
    [[1, 2, 2, 3, 3], [2, 2, 3, 3, 4], [2, 3, 3, 4, 4], [3, 3, 4, 4, 5], [3, 4, 4, 5, 5]];

/// get_intra_mode: the block's mode, or the wide-angle mode it was mapped to
/// for its current transform block.
pub(crate) fn intra_mode(m: &Mi, plane: usize) -> Result<usize> {
    if plane == 0 { Ok(m.wide_angle[0].unwrap_or(m.mode)) } else { uv_to_y_mode(m.wide_angle[1].unwrap_or(m.uv_mode)) }
}

/// get_primary_tx_type.
pub(crate) fn primary(tx_type: usize) -> usize {
    tx_type & 0xf
}

/// av2_get_max_eob.
pub(crate) fn max_eob(tx: usize) -> Result<usize> {
    t(&TX_SIZE_2D, adjusted_tx_size(tx))
}

/// get_txsize_entropy_ctx.
pub(crate) fn txsize_entropy_ctx(tx: usize) -> Result<usize> {
    Ok((t(&TXSIZE_SQR_MAP, tx)? + t(&TXSIZE_SQR_UP_MAP, tx)? + 1) >> 1)
}

/// av2_get_ext_tx_set_type.
fn ext_tx_set_type(tx: usize, inter: bool, reduced: usize) -> Result<usize> {
    let up = t(&TXSIZE_SQR_UP_MAP, tx)?;
    let sqr = t(&TXSIZE_SQR_MAP, tx)?;
    if up > TX_32X32 {
        return Ok(if sqr >= TX_32X32 { EXT_TX_SET_DCTONLY } else { EXT_TX_SET_LONG_SIDE_64 });
    }
    if up == TX_32X32 {
        return Ok(if sqr == TX_32X32 { EXT_TX_SET_DCT_IDTX } else { EXT_TX_SET_LONG_SIDE_32 });
    }
    if reduced > 0 {
        const MAP: [[usize; 3]; 2] = [
            [EXT_NEW_TX_SET, EXT_TX_SET_DCT_IDTX, EXT_NEW_TX_SET],
            [EXT_TX_SET_DCT_IDTX, EXT_TX_SET_DCT_IDTX, EXT_TX_SET_DCT_IDTX_IDDCT],
        ];
        return Ok(*MAP.at(usize::from(inter))?.at(reduced - 1)?);
    }
    Ok(if !inter {
        EXT_NEW_TX_SET
    } else if sqr == TX_16X16 {
        EXT_TX_SET_DTT9_IDTX_1DDCT
    } else {
        EXT_TX_SET_ALL16
    })
}

/// get_ext_tx_types.
fn ext_tx_types(tx: usize, inter: bool, reduced: usize) -> Result<usize> {
    let set = ext_tx_set_type(tx, inter, reduced)?;
    if inter { t2_1(&AV2_NUM_EXT_TX_SET, set) } else { t2_1(&AV2_NUM_EXT_TX_SET_INTRA, set) }
}

/// get_txtype_from_idx_for_large_txfm.
fn large_txfm_type(tx: usize, set: usize, short_side: usize, long_side_dct: bool) -> Result<usize> {
    let horz = t(&TX_SIZE_WIDE, tx)? > t(&TX_SIZE_HIGH, tx)?;
    Ok(*EXT_INDEX_TO_TX_TYPE_LARGE_TXFM
        .at(usize::from(set == EXT_TX_SET_LONG_SIDE_64))?
        .at(usize::from(!long_side_dct))?
        .at(usize::from(!horz))?
        .at(short_side)? as usize)
}

/// get_lp2tx_ctx: where the last coefficient sits, near DC, near the far
/// corner or between.
fn lp2tx_ctx(tx: usize, eob: usize) -> Result<usize> {
    let adj = adjusted_tx_size(tx);
    let bwl = t(&TX_SIZE_WIDE_LOG2, adj)?;
    let y = (eob - 1) >> bwl;
    let x = (eob - 1) - (y << bwl);
    let diag = x + y;
    let max_diag = t(&TX_SIZE_WIDE, adj)? + t(&TX_SIZE_HIGH, adj)? - 2;
    Ok(if diag < 2 {
        1
    } else if diag + 2 > max_diag {
        2
    } else {
        0
    })
}

/// skip_parsing_recon: in subsampled chroma, the second 64-wide unit of a
/// 128-wide block has no chroma of its own.
fn skip_parsing(row: usize, col: usize, ss_x: usize, ss_y: usize) -> bool {
    (ss_x == 1 && col & 16 != 0) || (ss_y == 1 && row & 16 != 0)
}

impl FrameParse<'_> {
    fn ss(&self, plane: usize) -> (usize, usize) {
        if plane == 0 { (0, 0) } else { (self.f.ss_x, self.f.ss_y) }
    }

    fn num_planes(&self) -> usize {
        if self.f.monochrome { 1 } else { 3 }
    }

    /// get_bsize_base.
    fn bsize_base(&self, xd: &Xd, plane: usize) -> Result<usize> {
        let m = self.grid.mi(xd.cur)?;
        Ok(if self.tree_type == SHARED_PART {
            if plane > 0 { m.chroma_ref.bsize_base } else { m.sb_type[0] }
        } else {
            *m.sb_type.at(usize::from(self.tree_type == CHROMA_PART))?
        })
    }

    /// get_mb_plane_block_size.
    pub(crate) fn mb_plane_bsize(&self, xd: &Xd, plane: usize) -> Result<usize> {
        let (sx, sy) = self.ss(plane);
        plane_bsize(self.bsize_base(xd, plane)?, sx, sy)
    }

    /// max_block_wide, in 4x4 units of the plane.
    fn max_block_wide(&self, xd: &Xd, bsize: usize, plane: usize) -> Result<usize> {
        let mut w = bw(bsize)? as i64;
        if xd.mb_to_right_edge < 0 {
            w += xd.mb_to_right_edge >> (3 + self.ss(plane).0);
        }
        Ok((w >> 2).max(0) as usize)
    }

    /// max_block_high.
    fn max_block_high(&self, xd: &Xd, bsize: usize, plane: usize) -> Result<usize> {
        let mut h = bh(bsize)? as i64;
        if xd.mb_to_bottom_edge < 0 {
            h += xd.mb_to_bottom_edge >> (3 + self.ss(plane).1);
        }
        Ok((h >> 2).max(0) as usize)
    }

    /// get_plane_tx_unit_width / _height: the end of the 64x64 unit at
    /// (row, col), clipped to the frame.
    fn unit_end(&self, xd: &Xd, bsize: usize, plane: usize, row: usize, col: usize) -> Result<(usize, usize)> {
        let (sx, sy) = self.ss(plane);
        let max_h = self.max_block_high(xd, bsize, plane)?;
        let max_w = self.max_block_wide(xd, bsize, plane)?;
        let mu_h = mi_h(BLOCK_64X64)?.min(max_h);
        let mu_w = mi_w(BLOCK_64X64)?.min(max_w);
        Ok(((mu_h + (row >> sy)).min(max_h), (mu_w + (col >> sx)).min(max_w)))
    }

    /// is_cctx_allowed.
    pub(crate) fn cctx_allowed(&self, xd: &Xd) -> Result<bool> {
        if !self.h.seq.enable_cctx || self.tree_type == LUMA_PART {
            return Ok(false);
        }
        let (sx, sy) = (self.f.ss_x, self.f.ss_y);
        let pb = self.mb_plane_bsize(xd, 1)?;
        if sx == 0 || sy == 0 {
            return Ok(bw(pb)? < 32 || bh(pb)? < 32);
        }
        Ok(true)
    }

    /// segfeature_active(SEG_LVL_SKIP).
    pub(crate) fn seg_skip(&self, segment_id: usize) -> Result<bool> {
        Ok(self.h.frame.seg.enabled && *self.h.frame.seg.feature_mask.at(segment_id)? & (1 << SEG_LVL_SKIP) != 0)
    }

    /// Where a plane's entropy contexts for this block start (set_entropy_context).
    fn ec_origin(&self, xd: &Xd, plane: usize) -> Result<(usize, usize)> {
        let (sx, sy) = self.ss(plane);
        let (r, c) = if plane > 0 {
            let cr = &self.grid.mi(xd.cur)?.chroma_ref;
            (cr.mi_row_base, cr.mi_col_base)
        } else {
            (xd.mi_row, xd.mi_col)
        };
        Ok((c >> sx, (r & MAX_MIB_MASK) >> sy))
    }

    /// The tx_type_map index of a luma 4x4 at (row, col) in this block.
    fn tx_map_idx(&self, xd: &Xd, row: usize, col: usize) -> Result<usize> {
        self.grid.idx(xd.mi_row + row, xd.mi_col + col)
    }

    /// parse_decode_block, from the transform size on: read_tx_size, the
    /// skip reset, decode_token_recon_block's parse, and the SDP copies.
    pub(crate) fn read_residual(&mut self, ts: &mut TileState, xd: &Xd) -> Result<()> {
        let tree = self.tree_type;
        let pt = usize::from(tree == CHROMA_PART);
        let (bsize, skip, inter) = {
            let m = self.grid.mi(xd.cur)?;
            (*m.sb_type.at(pt)?, *m.skip_txfm.at(pt)?, m.is_inter(tree))
        };
        if tree != CHROMA_PART {
            // read_tx_size under TX_MODE_LARGEST: the largest transform that fits.
            let tx_mode = self.h.frame.tx_mode as usize;
            if tx_mode != TX_MODE_LARGEST {
                return Err(Error::Unsupported("transform modes other than TX_MODE_LARGEST"));
            }
            let max_rect = t(&MAX_TXSIZE_RECT_LOOKUP, bsize)?;
            let tx = if bsize == BLOCK_4X4 {
                max_rect
            } else {
                // tx_size_from_tx_mode
                let largest = t(&TX_MODE_TO_BIGGEST_TX_SIZE, tx_mode)?;
                if t(&TXSIZE_SQR_MAP, max_rect)? <= largest { max_rect } else { largest }
            };
            self.grid.mi_mut(xd.cur)?.tx_size = tx;
        }
        if skip {
            self.reset_entropy_context(xd)?;
        }
        self.read_tokens(ts, xd, bsize, inter, skip)?;
        self.copy_sdp_records(xd, bsize)
    }

    /// av2_reset_entropy_context: a skipped block leaves zeros behind.
    fn reset_entropy_context(&mut self, xd: &Xd) -> Result<()> {
        let start = usize::from(self.tree_type == CHROMA_PART);
        let end = match self.tree_type {
            LUMA_PART => 1,
            CHROMA_PART => self.num_planes(),
            _ => 1 + (self.num_planes() - 1) * usize::from(xd.is_chroma_ref),
        };
        for plane in start..end {
            let pb = self.mb_plane_bsize(xd, plane)?;
            let (w, h) = (mi_w(pb)?, mi_h(pb)?);
            let (ac, lr) = self.ec_origin(xd, plane)?;
            self.ec.above.at_mut(plane)?.get_mut(ac..ac + w).ok_or(Error::Invalid("entropy context"))?.fill(0);
            self.ec.left.at_mut(plane)?.get_mut(lr..lr + h).ok_or(Error::Invalid("entropy context"))?.fill(0);
        }
        Ok(())
    }

    /// decode_token_recon_block's parse: every transform block, 64x64 unit
    /// by 64x64 unit (in 128x128 groups), luma then chroma within each unit.
    fn read_tokens(&mut self, ts: &mut TileState, xd: &Xd, bsize: usize, inter: bool, skip: bool) -> Result<()> {
        if inter && skip {
            return Ok(());
        }
        let start = usize::from(self.tree_type == CHROMA_PART);
        let end = if self.tree_type == LUMA_PART { 1 } else { self.num_planes() };
        let max_w = self.max_block_wide(xd, bsize, 0)?;
        let max_h = self.max_block_high(xd, bsize, 0)?;
        let mu_w = mi_w(BLOCK_64X64)?.min(max_w);
        let mu_h = mi_h(BLOCK_64X64)?.min(max_h);
        let (g_w, g_h) = (mi_w(BLOCK_128X128)?, mi_h(BLOCK_128X128)?);
        for row128 in (0..max_h).step_by(g_h) {
            for col128 in (0..max_w).step_by(g_w) {
                for row in (row128..(row128 + g_h).min(max_h)).step_by(mu_h) {
                    for col in (col128..(col128 + g_w).min(max_w)).step_by(mu_w) {
                        for plane in start..end {
                            if plane > 0 && !xd.is_chroma_ref {
                                break;
                            }
                            if inter {
                                self.inter_unit(ts, xd, plane, row, col)?;
                            } else {
                                self.intra_unit(ts, xd, plane, row, col, skip)?;
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// One plane of one 64x64 unit of an intra block.
    fn intra_unit(
        &mut self,
        ts: &mut TileState,
        xd: &Xd,
        plane: usize,
        row: usize,
        col: usize,
        skip: bool,
    ) -> Result<()> {
        let pb = self.mb_plane_bsize(xd, plane)?;
        let (unit_h, unit_w) = self.unit_end(xd, pb, plane, row, col)?;
        // Luma: one transform per unit (get_tx_partition_sizes with
        // TX_PARTITION_NONE). Chroma: av2_get_tx_size, the largest that fits.
        let tx = t(&MAX_TXSIZE_RECT_LOOKUP, pb)?;
        if plane == 0 {
            if row < unit_h && col < unit_w && !skip {
                self.read_coeffs(ts, xd, 0, row, col, tx)?;
            }
            return Ok(());
        }
        let cctx = self.cctx_allowed(xd)?;
        if plane == 1 && cctx {
            return Ok(()); // read together with V
        }
        let (sx, sy) = self.ss(plane);
        if skip || skip_parsing(row, col, sx, sy) {
            return Ok(());
        }
        let (step_r, step_c) = (t(&TX_SIZE_HIGH_UNIT, tx)?, t(&TX_SIZE_WIDE_UNIT, tx)?);
        for blk_row in ((row >> sy)..unit_h).step_by(step_r) {
            for blk_col in ((col >> sx)..unit_w).step_by(step_c) {
                if plane == 2 && cctx {
                    self.read_coeffs(ts, xd, 1, blk_row, blk_col, tx)?;
                }
                self.read_coeffs(ts, xd, plane, blk_row, blk_col, tx)?;
            }
        }
        Ok(())
    }

    /// One plane of one 64x64 unit of an intra-block-copy block, which codes
    /// its residual as an inter block does (decode_reconstruct_tx).
    fn inter_unit(&mut self, ts: &mut TileState, xd: &Xd, plane: usize, row: usize, col: usize) -> Result<()> {
        let pb = self.mb_plane_bsize(xd, plane)?;
        // get_vartx_max_txsize
        let tx = t(&MAX_TXSIZE_RECT_LOOKUP, pb)?;
        let (unit_h, unit_w) = self.unit_end(xd, pb, plane, row, col)?;
        let (sx, sy) = self.ss(plane);
        if skip_parsing(row, col, sx, sy) {
            return Ok(());
        }
        let cctx = self.cctx_allowed(xd)?;
        let max_h = self.max_block_high(xd, pb, plane)?;
        let max_w = self.max_block_wide(xd, pb, plane)?;
        let (step_r, step_c) = (t(&TX_SIZE_HIGH_UNIT, tx)?, t(&TX_SIZE_WIDE_UNIT, tx)?);
        for blk_row in ((row >> sy)..unit_h).step_by(step_r) {
            for blk_col in ((col >> sx)..unit_w).step_by(step_c) {
                if plane == 1 && cctx {
                    continue;
                }
                if blk_row >= max_h || blk_col >= max_w {
                    continue;
                }
                if plane == 2 && cctx {
                    self.read_coeffs(ts, xd, 1, blk_row, blk_col, tx)?;
                }
                self.read_coeffs(ts, xd, plane, blk_row, blk_col, tx)?;
            }
        }
        Ok(())
    }

    /// av2_read_coeffs_txb_facade: one transform block.
    fn read_coeffs(
        &mut self,
        ts: &mut TileState,
        xd: &Xd,
        plane: usize,
        row: usize,
        col: usize,
        tx: usize,
    ) -> Result<()> {
        let pt = usize::from(self.tree_type == CHROMA_PART);
        let (inter, fsc) = {
            let m = self.grid.mi(xd.cur)?;
            (m.is_inter(self.tree_type), *m.fsc_mode.at(pt)? != 0)
        };
        let enable_fsc = self.h.seq.enable_fsc;
        let pb = self.mb_plane_bsize(xd, plane)?;
        let ctx = self.txb_ctx(xd, pb, tx, plane, row, col, fsc && enable_fsc)?;
        let eob = self.read_sig_txtype(ts, xd, row, col, plane, ctx, tx)?;
        let tx_type = self.tx_type(xd, plane, row, col, tx)?;
        let mut cul_level = 0;
        if let Some(eob) = eob {
            let idtx_fsc = enable_fsc && fsc && primary(tx_type) == IDTX && plane == 0;
            // use_inter_fsc compares the whole type, secondary bits included.
            let inter_fsc = enable_fsc && plane == 0 && inter && tx_type == IDTX;
            cul_level = if idtx_fsc || inter_fsc {
                self.read_coeffs_txb_skip(ts, xd, row, col, plane, tx, eob)?
            } else {
                self.read_coeffs_txb(ts, xd, row, col, plane, ctx, tx, eob)?
            };
        }
        self.set_entropy_contexts(xd, plane, pb, tx, cul_level, col, row)?;
        if inter && plane == 0 {
            // update_txk_array: chroma of an inter block reads luma's type.
            let tx_type = self.tx_type(xd, 0, row, col, tx)?;
            let (h, w) = (t(&TX_SIZE_HIGH_UNIT, tx)?, t(&TX_SIZE_WIDE_UNIT, tx)?);
            for dy in (0..h).step_by(4) {
                for dx in (0..w).step_by(4) {
                    let i = self.tx_map_idx(xd, row + dy, col + dx)?;
                    *self.tx_type_map.at_mut(i)? = tx_type as u16;
                }
            }
        }
        Ok(())
    }

    /// get_txb_ctx.
    #[allow(clippy::too_many_arguments)]
    fn txb_ctx(
        &self,
        xd: &Xd,
        pb: usize,
        tx: usize,
        plane: usize,
        row: usize,
        col: usize,
        fsc: bool,
    ) -> Result<TxbCtx> {
        if fsc && plane == 0 {
            return Ok(TxbCtx { skip: TXB_SKIP_CONTEXTS - 1, dc_sign: 0 }); // get_txb_ctx_skip
        }
        let (ac, lr) = self.ec_origin(xd, plane)?;
        let wu = t(&TX_SIZE_WIDE_UNIT, tx)?;
        let hu = t(&TX_SIZE_HIGH_UNIT, tx)?;
        let a = self.ec.above.at(plane)?.get(ac + col..ac + col + wu).ok_or(Error::Invalid("entropy context"))?;
        let l = self.ec.left.at(plane)?.get(lr + row..lr + row + hu).ok_or(Error::Invalid("entropy context"))?;
        let mut dc_sign = 0i32;
        for &v in a.iter().chain(l) {
            dc_sign += *[0, -1, 1].at(usize::from(v >> COEFF_CONTEXT_BITS))?;
        }
        let dc_sign = match dc_sign {
            0 => 0,
            d if d < 0 => 1,
            _ => 2,
        };
        let skip = if plane == 0 {
            if bw(pb)? == t(&TX_SIZE_WIDE, tx)? && bh(pb)? == t(&TX_SIZE_HIGH, tx)? {
                0
            } else {
                let or = |s: &[u8]| (s.iter().fold(0usize, |acc, &v| acc | v as usize) & COEFF_CONTEXT_MASK).min(4);
                *SKIP_CONTEXTS.at(or(a))?.at(or(l))?
            }
        } else {
            // get_entropy_context
            let base = usize::from(a.iter().any(|&v| v != 0)) + usize::from(l.iter().any(|&v| v != 0));
            let offset = if plane == 1 {
                6
            } else if t(&NUM_PELS_LOG2_LOOKUP, pb)? > t(&TX_SIZE_WIDE_LOG2, tx)? + t(&TX_SIZE_HIGH_LOG2, tx)? {
                V_TXB_SKIP_CONTEXT_OFFSET >> 1
            } else {
                0
            };
            base + offset
        };
        Ok(TxbCtx { skip, dc_sign })
    }

    /// av2_set_entropy_contexts: the transform block's cul_level, cut off
    /// where the frame ends.
    #[allow(clippy::too_many_arguments)]
    fn set_entropy_contexts(
        &mut self,
        xd: &Xd,
        plane: usize,
        pb: usize,
        tx: usize,
        v: u8,
        col: usize,
        row: usize,
    ) -> Result<()> {
        let (ac, lr) = self.ec_origin(xd, plane)?;
        let wu = t(&TX_SIZE_WIDE_UNIT, tx)?;
        let hu = t(&TX_SIZE_HIGH_UNIT, tx)?;
        let n_a = if v != 0 && xd.mb_to_right_edge < 0 {
            wu.min(self.max_block_wide(xd, pb, plane)?.saturating_sub(col))
        } else {
            wu
        };
        let n_l = if v != 0 && xd.mb_to_bottom_edge < 0 {
            hu.min(self.max_block_high(xd, pb, plane)?.saturating_sub(row))
        } else {
            hu
        };
        let a =
            self.ec.above.at_mut(plane)?.get_mut(ac + col..ac + col + wu).ok_or(Error::Invalid("entropy context"))?;
        let (on, off) = a.split_at_mut(n_a.min(wu));
        on.fill(v);
        off.fill(0);
        let l =
            self.ec.left.at_mut(plane)?.get_mut(lr + row..lr + row + hu).ok_or(Error::Invalid("entropy context"))?;
        let (on, off) = l.split_at_mut(n_l.min(hu));
        on.fill(v);
        off.fill(0);
        Ok(())
    }

    /// av2_read_sig_txtype: whether the block has coefficients, and if so its
    /// end of block, transform type and cross-chroma type. Returns the eob.
    #[allow(clippy::too_many_arguments)]
    fn read_sig_txtype(
        &mut self,
        ts: &mut TileState,
        xd: &Xd,
        row: usize,
        col: usize,
        plane: usize,
        ctx: TxbCtx,
        tx: usize,
    ) -> Result<Option<usize>> {
        let pt = usize::from(self.tree_type == CHROMA_PART);
        let (inter, fsc) = {
            let m = self.grid.mi(xd.cur)?;
            (m.is_inter(self.tree_type), *m.fsc_mode.at(pt)? != 0)
        };
        let all_zero = if plane < 2 {
            let txs_ctx = txsize_entropy_ctx(tx)?;
            ts.fc.read(&mut ts.r, &TXB_SKIP_CDF, [usize::from(inter || fsc), txs_ctx, ctx.skip])? == 1
        } else {
            let c = ctx.skip + if self.eob_u_flag { V_TXB_SKIP_CONTEXT_OFFSET } else { 0 };
            ts.fc.read(&mut ts.r, &V_TXB_SKIP_CDF, [c])? == 1
        };
        if plane == 1 {
            self.eob_u_flag = !all_zero;
        }
        {
            let m = self.grid.mi_mut(xd.cur)?;
            let wide = if inter { None } else { wide_angle(m, tx, plane)? };
            *m.wide_angle.at_mut(usize::from(plane > 0))? = wide;
        }
        if all_zero {
            if plane == 0 {
                let i = self.tx_map_idx(xd, row, col)?;
                *self.tx_type_map.at_mut(i)? = DCT_DCT as u16;
            }
            return Ok(None);
        }
        let eob = self.read_eob(ts, plane, tx, inter)?;
        self.read_tx_type(ts, xd, row, col, tx, plane, eob, if inter { 0 } else { eob })?;
        if plane == 1 && self.cctx_allowed(xd)? {
            let cctx = if !inter && eob == 1 { CCTX_NONE } else { self.read_cctx_type(ts, xd)? };
            self.set_cctx(xd, row, col, tx, cctx)?;
        }
        Ok(Some(eob))
    }

    /// decode_eob.
    fn read_eob(&mut self, ts: &mut TileState, plane: usize, tx: usize, inter: bool) -> Result<usize> {
        let pl = if plane > 0 { 2 } else { usize::from(inter) };
        let (fc, r) = (&mut ts.fc, &mut ts.r);
        // Above 128 coefficients the eob token's alphabet runs out, and its
        // last value escapes into 1 or 2 literal bits.
        let escape =
            |fc: &mut crate::cdf::FrameContext, r: &mut crate::entropy::SymbolReader, f, bits| -> Result<usize> {
                let mut p = fc.read(r, f, [pl])?;
                if p == EOB_PT_INDEX_COUNT - 1 {
                    p += r.read_literal(bits)? as usize;
                }
                Ok(p)
            };
        let eob_pt = match t(&TXSIZE_LOG2_MINUS4, tx)? {
            0 => fc.read(r, &EOB_FLAG_CDF16, [pl])? + 1,
            1 => fc.read(r, &EOB_FLAG_CDF32, [pl])? + 1,
            2 => fc.read(r, &EOB_FLAG_CDF64, [pl])? + 1,
            3 => fc.read(r, &EOB_FLAG_CDF128, [pl])? + 1,
            4 => escape(fc, r, &EOB_FLAG_CDF256, 1)? + 1,
            5 => {
                let p = escape(fc, r, &EOB_FLAG_CDF512, 2)?;
                if p >= EOB_MAX_SYMS - 1 {
                    return Err(Error::Invalid("eob position token"));
                }
                p + 1
            }
            _ => escape(fc, r, &EOB_FLAG_CDF1024, 2)? + 1,
        };
        let bits = t(&AV2_EOB_OFFSET_BITS, eob_pt)? as u32;
        let mut extra = 0;
        if bits > 0 {
            if fc.read(r, &EOB_EXTRA_CDF, [])? == 1 {
                extra += 1 << (bits - 1);
            }
            extra += r.read_literal(bits - 1)? as usize;
        }
        let mut eob = t(&AV2_EOB_GROUP_START, eob_pt)?;
        if eob > 2 {
            eob += extra;
        }
        if eob > max_eob(tx)? {
            return Err(Error::Invalid("eob past the end of the transform"));
        }
        Ok(eob)
    }

    /// av2_read_tx_type: luma's primary transform type.
    #[allow(clippy::too_many_arguments)]
    fn read_tx_type(
        &mut self,
        ts: &mut TileState,
        xd: &Xd,
        row: usize,
        col: usize,
        tx: usize,
        plane: usize,
        eob: usize,
        dc_skip: usize,
    ) -> Result<()> {
        if plane != 0 {
            return Ok(());
        }
        let i = self.tx_map_idx(xd, row, col)?;
        *self.tx_type_map.at_mut(i)? = DCT_DCT as u16;
        if dc_skip == 1 {
            return Ok(());
        }
        let pt = usize::from(self.tree_type == CHROMA_PART);
        let m = self.grid.mi(xd.cur)?.clone();
        if *m.skip_txfm.at(pt)? || self.seg_skip(m.segment_id)? {
            return Ok(());
        }
        let inter = m.is_inter(self.tree_type);
        if *m.fsc_mode.at(pt)? != 0 {
            *self.tx_type_map.at_mut(i)? = IDTX as u16;
            return Ok(());
        }
        let reduced = self.h.frame.reduced_tx_set_used as usize;
        if ext_tx_types(tx, inter, reduced)? <= 1 {
            return Ok(());
        }
        let set = ext_tx_set_type(tx, inter, reduced)?;
        let eset = t2_1(EXT_TX_SET_INDEX.at(usize::from(inter))?, set)?;
        let square = t(&TXSIZE_SQR_MAP, tx)?;
        let up = t(&TXSIZE_SQR_UP_MAP, tx)?;
        let long_side = set == EXT_TX_SET_LONG_SIDE_64 || set == EXT_TX_SET_LONG_SIDE_32;
        let (fc, r) = (&mut ts.fc, &mut ts.r);
        let tx_type = if inter {
            let ctx = lp2tx_ctx(tx, eob)?;
            if !long_side {
                let idx = if eset == 1 || eset == 2 {
                    if fc.read(r, &INTER_TX_TYPE_SET, [eset - 1, ctx, square])? == 0 {
                        fc.read(r, &INTER_TX_TYPE_IDX, [eset - 1, ctx])?
                    } else if eset == 1 {
                        INTER_TX_TYPE_INDEX_COUNT + fc.read(r, &INTER_TX_TYPE_OFFSET_1, [ctx])?
                    } else {
                        INTER_TX_TYPE_INDEX_COUNT + fc.read(r, &INTER_TX_TYPE_OFFSET_2, [ctx])?
                    }
                } else {
                    let n = t2_1(&AV2_NUM_EXT_TX_SET, set)?;
                    fc.read_n(r, &INTER_EXT_TX_CDF, [eset, ctx, square], n)?
                };
                t2_1(AV2_EXT_TX_INV.at(set)?, idx)?
            } else {
                let long_dct = up != TX_32X32 || fc.read(r, &TX_EXT_32_CDF, [1])? == 1;
                let short = fc.read(r, &INTER_EXT_TX_SHORT_SIDE_CDF, [ctx, square])?;
                large_txfm_type(tx, set, short, long_dct)?
            }
        } else {
            if reduced == 2 {
                return Ok(());
            }
            if !long_side {
                let mode = intra_mode(&m, 0)?;
                let n = if reduced > 0 {
                    t2_1(&AV2_NUM_REDUCED_TX_SET, reduced - 1)?
                } else {
                    t2_1(&AV2_NUM_EXT_TX_SET_INTRA, set)?
                };
                let idx = fc.read_n(r, &INTRA_EXT_TX_CDF, [eset + usize::from(reduced > 0), square], n)?;
                // av2_tx_idx_to_type
                if set == EXT_NEW_TX_SET {
                    let size = t2_1(&AV2_SIZE_CLASS, tx)?;
                    let class = t2_1(&AV2_MD_CLASS, mode)?;
                    t2_1(AV2_MD_IDX2TYPE.at(size)?.at(class)?, idx)?
                } else {
                    t2_1(AV2_EXT_TX_INV.at(set)?, idx)?
                }
            } else {
                let long_dct = up != TX_32X32 || fc.read(r, &TX_EXT_32_CDF, [0])? == 1;
                let short = fc.read(r, &INTRA_EXT_TX_SHORT_SIDE_CDF, [square])?;
                large_txfm_type(tx, set, short, long_dct)?
            }
        };
        *self.tx_type_map.at_mut(i)? = tx_type as u16;
        Ok(())
    }

    /// av2_read_cctx_type.
    fn read_cctx_type(&mut self, ts: &mut TileState, xd: &Xd) -> Result<usize> {
        let pt = usize::from(self.tree_type == CHROMA_PART);
        let m = self.grid.mi(xd.cur)?;
        if *m.skip_txfm.at(pt)? || self.seg_skip(m.segment_id)? {
            return Ok(CCTX_NONE);
        }
        ts.fc.read(&mut ts.r, &CCTX_TYPE_CDF, [])
    }

    /// update_cctx_array: the type covers the transform block's luma area,
    /// from the chroma reference's top-left.
    fn set_cctx(&mut self, xd: &Xd, row: usize, col: usize, tx: usize, cctx: usize) -> Result<()> {
        let (sx, sy) = (self.f.ss_x, self.f.ss_y);
        let cr = self.grid.mi(xd.cur)?.chroma_ref;
        let (r0, c0) = (cr.mi_row_base + (row << sy), cr.mi_col_base + (col << sx));
        let (h, w) = (t(&TX_SIZE_HIGH_UNIT, tx)? << sy, t(&TX_SIZE_WIDE_UNIT, tx)? << sx);
        for r in r0..r0 + h {
            for c in c0..c0 + w {
                if let Ok(i) = self.grid.idx(r, c) {
                    *self.cctx_type_map.at_mut(i)? = cctx as u8;
                }
            }
        }
        Ok(())
    }

    /// av2_get_tx_type: the transform a block's coefficients are scanned and
    /// inverted with. Luma's was read; chroma's follows from the mode.
    pub(crate) fn tx_type(&self, xd: &Xd, plane: usize, row: usize, col: usize, tx: usize) -> Result<usize> {
        let pt = usize::from(self.tree_type == CHROMA_PART);
        let m = self.grid.mi(xd.cur)?;
        let inter = m.is_inter(self.tree_type);
        if *m.fsc_mode.at(pt)? != 0 && !inter && plane == 0 {
            return Ok(IDTX);
        }
        let reduced = if plane == 0 {
            self.h.frame.reduced_tx_set_used as usize
        } else {
            usize::from(self.h.seq.enable_chroma_dctonly)
        };
        let mut tx_type = if plane == 0 {
            *self.tx_type_map.at(self.tx_map_idx(xd, row, col)?)? as usize
        } else if reduced > 0 {
            DCT_DCT
        } else {
            let mut ty = if inter {
                let (sx, sy) = self.ss(plane);
                primary(*self.tx_type_map.at(self.tx_map_idx(xd, row << sy, col << sx)?)? as usize)
            } else {
                *INTRA_MODE_TO_TX_TYPE.at(intra_mode(m, 1)?)?
            };
            let set = ext_tx_set_type(tx, inter, reduced)?;
            if *AV2_EXT_TX_USED.at(set)?.at(primary(ty))? == 0 {
                ty = DCT_DCT;
            }
            if set == EXT_TX_SET_LONG_SIDE_64 || set == EXT_TX_SET_LONG_SIDE_32 {
                // adjust_ext_tx_used_flag
                let mut used = *AV2_EXT_TX_USED_FLAG.at(set)? as u32;
                let horz = t(&TX_SIZE_WIDE, tx)? > t(&TX_SIZE_HIGH, tx)?;
                let off: &[usize] = match (set == EXT_TX_SET_LONG_SIDE_64, horz) {
                    (true, true) => &[DCT_ADST, DCT_FLIPADST, V_DCT],
                    (true, false) => &[ADST_DCT, FLIPADST_DCT, H_DCT],
                    (false, true) => &[DCT_ADST, DCT_FLIPADST, H_ADST, H_FLIPADST],
                    (false, false) => &[ADST_DCT, FLIPADST_DCT, V_ADST, V_FLIPADST],
                };
                for &o in off {
                    used &= !(1 << o);
                }
                if used & (1 << primary(ty)) == 0 {
                    ty = DCT_DCT;
                }
            }
            ty
        };
        if t(&TXSIZE_SQR_UP_MAP, tx)? > TX_32X32 && t(&TXSIZE_SQR_MAP, tx)? >= TX_32X32 {
            tx_type &= 0xfff0; // disable_primary_tx_type
        }
        Ok(tx_type)
    }

    /// av2_read_sec_tx_type: the intra secondary transform (IST) and its
    /// kernel set, stored above the primary type's 4 bits.
    pub(crate) fn read_sec_tx_type(
        &mut self,
        ts: &mut TileState,
        xd: &Xd,
        row: usize,
        col: usize,
        tx: usize,
        eob: usize,
    ) -> Result<()> {
        let pt = usize::from(self.tree_type == CHROMA_PART);
        let m = self.grid.mi(xd.cur)?.clone();
        if *m.skip_txfm.at(pt)? || self.seg_skip(m.segment_id)? {
            return Ok(());
        }
        let inter = m.is_inter(self.tree_type);
        let i = self.tx_map_idx(xd, row, col)?;
        let mut tx_type = *self.tx_type_map.at(i)? as usize;
        if !signals_sec_tx_type(&m, inter, tx, tx_type, eob)? {
            return Ok(());
        }
        let square = t(&TXSIZE_SQR_MAP, tx)?;
        let flag = ts.fc.read(&mut ts.r, &STX_CDF, [usize::from(inter), square])?;
        tx_type |= flag << PRIMARY_TX_BITS;
        if flag > 0 {
            // read_secondary_tx_set
            let p = primary(tx_type);
            let mut set = DC_PRED;
            if !inter {
                let mode = intra_mode(&m, 0)?;
                if p == ADST_ADST && t(&TX_SIZE_WIDE, tx)? >= 8 && t(&TX_SIZE_HIGH, tx)? >= 8 {
                    let k = ts.fc.read(&mut ts.r, &MOST_PROBABLE_STX_SET_CDF_ADST_ADST, [])?;
                    set = *INV_MOST_PROBABLE_STX_MAPPING_ADST_ADST.at(mode)?.at(k)? as usize;
                } else {
                    let k = ts.fc.read(&mut ts.r, &MOST_PROBABLE_STX_SET_CDF, [])?;
                    set = *INV_MOST_PROBABLE_STX_MAPPING.at(mode)?.at(k)? as usize;
                }
            }
            if p == ADST_ADST {
                set += IST_SET_SIZE;
            }
            tx_type |= set << (PRIMARY_TX_BITS + SECONDARY_TX_BITS);
        }
        *self.tx_type_map.at_mut(i)? = tx_type as u16;
        Ok(())
    }

    /// parse_decode_block's last step. A shared-tree block's 4x4s all point
    /// at one record; a luma or chroma tree's block gets its own copies, the
    /// chroma tree copying only chroma's fields.
    fn copy_sdp_records(&mut self, xd: &Xd, bsize: usize) -> Result<()> {
        let tree = self.tree_type;
        if tree == SHARED_PART {
            return Ok(());
        }
        let x_in = mi_w(bsize)?.min(self.f.mi_cols - xd.mi_col);
        let y_in = mi_h(bsize)?.min(self.f.mi_rows - xd.mi_row);
        let src = self.grid.mi(xd.cur)?.clone();
        for y in 0..y_in {
            for x in 0..x_in {
                if x == 0 && y == 0 {
                    continue;
                }
                let i = self.grid.idx(xd.mi_row + y, xd.mi_col + x)?;
                *self.grid.ptr.at_mut(i)? = Some(i);
                let d = self.grid.mi_mut(i)?;
                if tree == LUMA_PART {
                    *d = src.clone();
                } else {
                    d.sb_type[1] = src.sb_type[1];
                    d.uv_mode = src.uv_mode;
                    d.angle_delta[1] = src.angle_delta[1];
                    d.cfl_alpha_signs = src.cfl_alpha_signs;
                    d.cfl_alpha_idx = src.cfl_alpha_idx;
                    d.partition = src.partition;
                    // AVM also copies the chroma palette when
                    // av2_allow_palette(PLANE_TYPE_UV, ...), which is never.
                }
            }
        }
        Ok(())
    }
}

/// wide_angle_mapping: a directional mode on a thin transform swaps to the
/// wide angle across it (D203 or D45).
fn wide_angle(m: &Mi, tx: usize, plane: usize) -> Result<Option<usize>> {
    let mode = if plane == 0 { m.mode } else { m.uv_mode };
    if !(V_PRED..=D67_PRED).contains(&mode) {
        return Ok(None);
    }
    let (w, h) = (t(&TX_SIZE_WIDE, tx)? as i32, t(&TX_SIZE_HIGH, tx)? as i32);
    let delta = *m.angle_delta.at(usize::from(plane > 0))? * ANGLE_STEP as i32;
    let mrl = if plane == 0 { m.mrl_index } else { 0 };
    let p = *MODE_TO_ANGLE_MAP.at(mode)? as i32 + delta + *[0, 1, -1, 0].at(mrl)?;
    let th = [
        (2, WAIP_WH_RATIO_2_THRES as i32),
        (4, WAIP_WH_RATIO_4_THRES as i32),
        (8, WAIP_WH_RATIO_8_THRES as i32),
        (16, WAIP_WH_RATIO_16_THRES as i32),
    ];
    if th.iter().any(|&(k, th)| h == k * w && p < th) {
        Ok(Some(D203_PRED))
    } else if th.iter().any(|&(k, th)| w == k * h && p > 270 - th) {
        Ok(Some(D45_PRED))
    } else {
        Ok(None)
    }
}

/// block_signals_sec_tx_type.
fn signals_sec_tx_type(m: &Mi, inter: bool, tx: usize, tx_type: usize, eob: usize) -> Result<bool> {
    if if inter { eob <= 3 } else { eob <= 1 } {
        return Ok(false);
    }
    let dir = intra_mode(m, 0)?;
    let p = primary(tx_type);
    let (w, h) = (t(&TX_SIZE_WIDE, tx)?, t(&TX_SIZE_HIGH, tx)?);
    let class = if w == 8 && h == 8 && p == DCT_DCT {
        1
    } else if w >= 8 && h >= 8 {
        if p == DCT_DCT { 2 } else { 3 }
    } else {
        0
    };
    let limit = *[IST_4x4_HEIGHT, IST_8x8_HEIGHT_RED, IST_8x8_HEIGHT, IST_ADST_NZ_CNT].at(class)?;
    let mode_ok = if inter { p == DCT_DCT && w >= 16 && h >= 16 } else { dir < PAETH_PRED };
    Ok((p == DCT_DCT || p == ADST_ADST) && mode_ok && eob <= limit)
}
