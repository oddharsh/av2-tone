//! One transform block's coefficient tokens: a port of av2_read_coeffs_txb
//! and av2_read_coeffs_txb_skip (av2/decoder/decodetxb.c) and the context
//! functions they use (av2/common/txb_common.h, hr_coding.c, quant_common.c).
//!
//! Coefficients come in two passes. The first walks the scan backwards from
//! the end of block, reading each coefficient's base level (with a short
//! low-range extension) from a context of the levels already read below and
//! to its right. The second walks it again for signs and the Golomb-Rice
//! high range. The forward-skip (FSC) path for identity transforms reads in
//! scan order instead, from levels above and to the left. Only the levels are
//! kept; dequantisation comes with reconstruction.
//!
//! AVM pads its level buffer (TX_PAD_*) so neighbour reads never cross an
//! edge. `Levels` keeps the same layout and checks every index anyway.

use crate::avm_defs::*;
use crate::blk::*;
use crate::block::{Xd, adjusted_tx_size};
use crate::cdf_tables::*;
use crate::error::{Error, Result};
use crate::parse::{FrameParse, TileState};
use crate::residual::{TxbCtx, max_eob, primary, txsize_entropy_ctx};

/// AVM's levels_buf (or signs_buf), addressed from `levels`, its row 0.
struct Levels<T> {
    buf: Vec<T>,
    base: usize,
}

impl<T: Copy + Default> Levels<T> {
    /// set_levels / set_signs.
    fn new(width: usize) -> Levels<T> {
        Levels { buf: vec![T::default(); TX_PAD_2D], base: TX_PAD_TOP * (width + TX_PAD_HOR) }
    }
    fn get(&self, i: isize) -> Result<T> {
        let j = self.base as isize + i;
        if j < 0 {
            return Err(Error::Invalid("level index"));
        }
        self.buf.get(j as usize).copied().ok_or(Error::Invalid("level index"))
    }
    fn set(&mut self, i: isize, v: T) -> Result<()> {
        let j = usize::try_from(self.base as isize + i).map_err(|_| Error::Invalid("level index"))?;
        *self.buf.get_mut(j).ok_or(Error::Invalid("level index"))? = v;
        Ok(())
    }
}

/// get_padded_idx.
fn padded(idx: usize, bwl: usize) -> isize {
    (idx + ((idx >> bwl) << TX_PAD_HOR_LOG2)) as isize
}

/// get_padded_idx_left.
fn padded_left(idx: usize, bwl: usize) -> isize {
    TX_PAD_LEFT as isize + padded(idx, bwl)
}

/// A level read at `p + off`, clipped to `max`.
fn lv(l: &Levels<u8>, p: isize, off: isize, max: usize) -> Result<usize> {
    Ok((l.get(p + off)? as usize).min(max))
}

/// The padded row stride, (1 << bwl) + TX_PAD_HOR.
fn stride(bwl: usize) -> isize {
    ((1 << bwl) + TX_PAD_HOR) as isize
}

/// get_lf_limits: whether a position is in the low-frequency region.
fn lf_limits(row: usize, col: usize, class: usize, plane: usize) -> bool {
    match class {
        TX_CLASS_2D => row + col < if plane == 0 { LF_2D_LIM } else { LF_2D_LIM_UV },
        TX_CLASS_HORIZ => col < if plane == 0 { LF_RC_LIM } else { LF_RC_LIM_UV },
        _ => row < if plane == 0 { LF_RC_LIM } else { LF_RC_LIM_UV },
    }
}

/// get_nz_mag (max 3) and get_nz_mag_lf (max 5 for the 2-D taps): the
/// template sum below and right of `p`.
fn nz_mag(l: &Levels<u8>, p: isize, bwl: usize, class: usize, lf: bool) -> Result<usize> {
    let s = stride(bwl);
    let near = if lf { 5 } else { 3 };
    let mut m = lv(l, p, 1, near)? + lv(l, p, s, near)?;
    m += match class {
        TX_CLASS_2D => lv(l, p, s + 1, near)? + lv(l, p, 2, near)? + lv(l, p, 2 * s, near)?,
        TX_CLASS_VERT => lv(l, p, 2 * s, 3)? + lv(l, p, 3 * s, 3)? + lv(l, p, 4 * s, 3)?,
        _ => lv(l, p, 2, 3)? + lv(l, p, 3, 3)? + lv(l, p, 4, 3)?,
    };
    Ok(m)
}

/// get_nz_mag_chroma (max 3) and get_nz_mag_lf_chroma (max 5).
fn nz_mag_chroma(l: &Levels<u8>, p: isize, bwl: usize, class: usize, max: usize) -> Result<usize> {
    let s = stride(bwl);
    let mut m = lv(l, p, 1, max)? + lv(l, p, s, max)?;
    if class == TX_CLASS_2D {
        m += lv(l, p, s + 1, max)?;
    }
    Ok(m)
}

fn row_col(idx: usize, bwl: usize) -> (usize, usize) {
    let row = idx >> bwl;
    (row, idx - (row << bwl))
}

/// get_lower_levels_lf_ctx: base-level context in the low-frequency region.
fn lower_lf_ctx(l: &Levels<u8>, idx: usize, bwl: usize, class: usize) -> Result<usize> {
    let stats = nz_mag(l, padded(idx, bwl), bwl, class, true)?;
    let ctx = (stats + 1) >> 1;
    let (row, col) = row_col(idx, bwl);
    Ok(match class {
        TX_CLASS_2D if idx == 0 => ctx.min(8),
        TX_CLASS_2D if row + col < 2 => ctx.min(6) + 9,
        TX_CLASS_2D => ctx.min(4) + 16,
        TX_CLASS_HORIZ if col == 0 => ctx.min(6) + LF_SIG_COEF_CONTEXTS_2D,
        TX_CLASS_VERT if row == 0 => ctx.min(6) + LF_SIG_COEF_CONTEXTS_2D,
        _ => ctx.min(4) + LF_SIG_COEF_CONTEXTS_2D + 7,
    })
}

/// get_lower_levels_ctx: base-level context elsewhere.
fn lower_ctx(l: &Levels<u8>, idx: usize, bwl: usize, class: usize, plane: usize) -> Result<usize> {
    if class == TX_CLASS_2D && idx == 0 {
        return Ok(0);
    }
    let stats = nz_mag(l, padded(idx, bwl), bwl, class, false)?;
    let ctx = ((stats + 1) >> 1).min(4);
    if class != TX_CLASS_2D {
        return Ok(ctx + 15);
    }
    let (row, col) = row_col(idx, bwl);
    Ok(if plane > 0 || row + col < 6 {
        ctx
    } else if row + col < 8 {
        ctx + 5
    } else {
        ctx + 10
    })
}

/// get_lower_levels_lf_ctx_chroma.
fn lower_lf_ctx_chroma(l: &Levels<u8>, idx: usize, bwl: usize, class: usize, plane: usize) -> Result<usize> {
    let ctx = ((nz_mag_chroma(l, padded(idx, bwl), bwl, class, 5)? + 1) >> 1).min(3);
    Ok(if class == TX_CLASS_2D { if plane == 1 { ctx } else { ctx + 4 } } else { ctx + LF_SIG_COEF_CONTEXTS_2D_UV })
}

/// get_lower_levels_ctx_chroma.
fn lower_ctx_chroma(l: &Levels<u8>, idx: usize, bwl: usize, class: usize, plane: usize) -> Result<usize> {
    if class == TX_CLASS_2D && idx == 0 {
        return Ok(0);
    }
    let ctx = ((nz_mag_chroma(l, padded(idx, bwl), bwl, class, 3)? + 1) >> 1).min(3);
    Ok(if class == TX_CLASS_2D { if plane == 1 { ctx } else { ctx + 4 } } else { ctx + LF_SIG_COEF_CONTEXTS_2D_UV })
}

/// get_br_ctx (and _2d, the same with a 2-D class): low-range context.
fn br_ctx(l: &Levels<u8>, idx: usize, bwl: usize, class: usize) -> Result<usize> {
    let p = padded(idx, bwl);
    let s = stride(bwl);
    let m = MAX_VAL_BR_CTX;
    let third = match class {
        TX_CLASS_2D => s + 1,
        TX_CLASS_VERT => 2 * s,
        _ => 2,
    };
    let mag = lv(l, p, 1, m)? + lv(l, p, s, m)? + lv(l, p, third, m)?;
    Ok(((mag + 1) >> 1).min(6))
}

/// get_br_lf_ctx (and get_br_lf_ctx_2d, which never sees DC).
fn br_lf_ctx(l: &Levels<u8>, idx: usize, bwl: usize, class: usize) -> Result<usize> {
    let mag = br_ctx(l, idx, bwl, class)?;
    Ok(if class == TX_CLASS_2D && idx == 0 { mag } else { mag + 7 })
}

/// get_br_ctx_chroma (and _2d_chroma): unclipped taps, at most 3.
fn br_ctx_chroma(l: &Levels<u8>, idx: usize, bwl: usize, class: usize, clip: usize) -> Result<usize> {
    let p = padded(idx, bwl);
    let s = stride(bwl);
    let mut mag = lv(l, p, 1, clip)? + lv(l, p, s, clip)?;
    if class == TX_CLASS_2D {
        mag += lv(l, p, s + 1, clip)?;
    }
    Ok(((mag + 1) >> 1).min(3))
}

/// get_lower_levels_ctx_eob.
fn eob_ctx(bwl: usize, height: usize, si: usize) -> usize {
    let n = height << bwl;
    if si == 0 {
        0
    } else if si <= n / 8 {
        1
    } else if si <= n / 4 {
        2
    } else {
        3
    }
}

/// get_adaptive_param: the Rice parameter for a running high-range average.
fn adaptive_param(ctx: i32) -> u32 {
    1 + [4, 8, 16, 32, 64].iter().filter(|&&t| ctx >= t).count() as u32
}

/// tcq_next_state: an 8-state trellis stepped by each level's parity.
fn tcq_next(state: i32, level: usize) -> i32 {
    let mode = state >> 8;
    if mode != 1 {
        return mode << 8;
    }
    const NEXT: [[i32; 2]; 8] = [[0, 4], [4, 0], [1, 5], [5, 1], [6, 2], [2, 6], [7, 3], [3, 7]];
    let s = (state & 0xff) as usize;
    (mode << 8) | NEXT.get(s).and_then(|n| n.get(level & 1)).copied().unwrap_or(0)
}

impl FrameParse<'_> {
    /// read_low_range: up to COEFF_BASE_RANGE more, three at a time.
    fn low_range<const R: usize>(&self, ts: &mut TileState, f: &crate::cdf::Cdf<R>, idx: [usize; R]) -> Result<usize> {
        let mut br = 0;
        for _ in (0..COEFF_BASE_RANGE).step_by(BR_CDF_SIZE - 1) {
            let k = ts.fc.read(&mut ts.r, f, idx)?;
            br += k;
            if k < BR_CDF_SIZE - 1 {
                break;
            }
        }
        Ok(br)
    }

    /// read_adaptive_hr: a truncated-Rice value with an Exp-Golomb tail.
    fn adaptive_hr(&self, ts: &mut TileState, ctx: i32) -> Result<i32> {
        let m = adaptive_param(ctx);
        let (k, cmax) = (m + 1, (m + 4).min(6));
        let q = ts.r.read_unary(cmax)?;
        let rem = if q == cmax {
            // read_exp_golomb
            let len = ts.r.read_unary(21)?;
            if len > 20 {
                return Err(Error::Invalid("Exp-Golomb length"));
            }
            let len = len + k;
            ((1i64 << len) + i64::from(ts.r.read_literal(len)?) - (1i64 << k)) as i32
        } else {
            ts.r.read_literal(m)? as i32
        };
        Ok(rem + ((q as i32) << m))
    }

    /// read_high_range.
    fn high_range(
        &self,
        ts: &mut TileState,
        tcq: bool,
        level: i32,
        lf: bool,
        avg: &mut i32,
        plane: usize,
    ) -> Result<i32> {
        let max_br =
            if lf { if plane == 0 { LF_MAX_BASE_BR_RANGE } else { LF_NUM_BASE_LEVELS + 1 } } else { MAX_BASE_BR_RANGE }
                as i32;
        if (tcq && level >= max_br - 1) || level >= max_br {
            let hr = self.adaptive_hr(ts, *avg)?;
            *avg = (*avg + hr) >> 1;
            return Ok(level + (hr << u32::from(tcq)));
        }
        Ok(level)
    }

    /// The scan for a transform size and type: av2_scan_orders[tx][primary].
    fn scan(tx: usize, tx_type: usize) -> Result<&'static [u16]> {
        let start = t2(&SCAN_START, tx, primary(tx_type))?;
        SCAN.get(start..start + max_eob(tx)?).ok_or(Error::Invalid("scan order"))
    }

    /// av2_read_coeffs_txb. Returns the cul_level for the entropy contexts.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn read_coeffs_txb(
        &mut self,
        ts: &mut TileState,
        xd: &Xd,
        row: usize,
        col: usize,
        plane: usize,
        ctx: TxbCtx,
        tx: usize,
        eob: usize,
    ) -> Result<u8> {
        let txs_ctx = txsize_entropy_ctx(tx)?;
        let tx_type = self.tx_type(xd, plane, row, col, tx)?;
        let class = t(&TX_TYPE_TO_CLASS, primary(tx_type))?;
        let adj = adjusted_tx_size(tx);
        let bwl = t(&TX_SIZE_WIDE_LOG2, adj)?;
        let height = t(&TX_SIZE_HIGH, adj)?;
        let scan = Self::scan(tx, tx_type)?;
        let pos_at = |c: usize| -> Result<usize> { Ok(*scan.at(c)? as usize) };

        let pt = usize::from(self.tree_type == CHROMA_PART);
        let (inter, fsc) = {
            let m = self.grid.mi(xd.cur)?;
            (m.is_inter(self.tree_type), *m.fsc_mode.at(pt)? != 0)
        };
        let s = &self.h.seq;
        let ist = if inter { eob > 3 && s.enable_inter_ist } else { eob != 1 && s.enable_ist && !fsc };
        if plane == 0 && ist {
            self.read_sec_tx_type(ts, xd, row, col, tx, eob)?;
        }

        let mut levels = Levels::<u8>::new(1 << bwl);
        // tcq_enable / tcq_init_state
        let tcq = self.h.frame.tcq_mode != 0 && plane == 0 && class == TX_CLASS_2D;
        let mut state = i32::from(tcq) << 8;
        let q = |state: i32| usize::from(state & 2 != 0); // tcq_quant

        // The last coefficient, known to be nonzero.
        {
            let c = eob - 1;
            let pos = pos_at(c)?;
            let cctx = eob_ctx(bwl, height, c);
            let (r, cl) = row_col(pos, bwl);
            let limits = lf_limits(r, cl, class, plane);
            let level = match (plane > 0, limits) {
                (true, true) => ts.fc.read(&mut ts.r, &COEFF_BASE_LF_EOB_UV_CDF, [cctx])? + 1,
                (true, false) => {
                    let l = ts.fc.read(&mut ts.r, &COEFF_BASE_EOB_UV_CDF, [cctx])? + 1;
                    if l > NUM_BASE_LEVELS { l + self.low_range(ts, &COEFF_BR_UV_CDF, [0])? } else { l }
                }
                (false, true) => {
                    let l = ts.fc.read(&mut ts.r, &COEFF_BASE_LF_EOB_CDF, [txs_ctx, cctx])? + 1;
                    if l > LF_NUM_BASE_LEVELS {
                        // get_br_ctx_lf_eob
                        let br = if class == TX_CLASS_2D && pos == 0 { 0 } else { 7 };
                        l + self.low_range(ts, &COEFF_BR_LF_CDF, [br])?
                    } else {
                        l
                    }
                }
                (false, false) => {
                    let l = ts.fc.read(&mut ts.r, &COEFF_BASE_EOB_CDF, [txs_ctx, cctx])? + 1;
                    if l > NUM_BASE_LEVELS { l + self.low_range(ts, &COEFF_BR_CDF, [0])? } else { l }
                }
            };
            levels.set(padded(pos, bwl), level as u8)?;
            state = tcq_next(state, level);
        }

        // read_coeffs_reverse and read_coeffs_reverse_2d: the same reads,
        // with the 2-D variants' contexts for a 2-D class away from DC.
        let reverse = |me: &Self,
                       ts: &mut TileState,
                       levels: &mut Levels<u8>,
                       from: usize,
                       to: usize,
                       state: &mut i32|
         -> Result<()> {
            for c in (from..=to).rev() {
                let pos = pos_at(c)?;
                let (r, cl) = row_col(pos, bwl);
                let limits = lf_limits(r, cl, class, plane);
                let qi = q(*state);
                let level = if plane > 0 {
                    if limits {
                        let cctx = lower_lf_ctx_chroma(levels, pos, bwl, class, plane)?;
                        ts.fc.read(&mut ts.r, &COEFF_BASE_LF_UV_CDF, [cctx])?
                    } else {
                        let cctx = lower_ctx_chroma(levels, pos, bwl, class, plane)?;
                        let l = ts.fc.read(&mut ts.r, &COEFF_BASE_UV_CDF, [cctx])?;
                        if l > NUM_BASE_LEVELS {
                            // get_br_ctx_2d_chroma clips its taps; get_br_ctx_chroma does not.
                            let clip = if class == TX_CLASS_2D && c > 0 { MAX_VAL_BR_CTX } else { usize::MAX };
                            let br = br_ctx_chroma(levels, pos, bwl, class, clip)?;
                            l + me.low_range(ts, &COEFF_BR_UV_CDF, [br])?
                        } else {
                            l
                        }
                    }
                } else if limits {
                    let cctx = lower_lf_ctx(levels, pos, bwl, class)?;
                    let l = ts.fc.read(&mut ts.r, &COEFF_BASE_LF_CDF, [txs_ctx, cctx, qi])?;
                    if l > LF_NUM_BASE_LEVELS {
                        let br = br_lf_ctx(levels, pos, bwl, class)?;
                        l + me.low_range(ts, &COEFF_BR_LF_CDF, [br])?
                    } else {
                        l
                    }
                } else {
                    let cctx = lower_ctx(levels, pos, bwl, class, plane)?;
                    let l = ts.fc.read(&mut ts.r, &COEFF_BASE_CDF, [txs_ctx, cctx, qi])?;
                    if l > NUM_BASE_LEVELS {
                        let br = br_ctx(levels, pos, bwl, class)?;
                        l + me.low_range(ts, &COEFF_BR_CDF, [br])?
                    } else {
                        l
                    }
                };
                levels.set(padded(pos, bwl), level as u8)?;
                *state = tcq_next(*state, level);
            }
            Ok(())
        };

        let ph = self.h.frame.allow_parity_hiding
            && plane == 0
            && *PH_ALLOWED_TX_TYPES.at(primary(tx_type))? != 0
            && eob > PHTHRESH;
        let mut hidden = false;
        if eob > 1 {
            reverse(self, ts, &mut levels, 1, eob - 2, &mut state)?;
            let (mut nz, mut sum) = (0, 0);
            if ph {
                for si in (1..eob).rev() {
                    let l = (levels.get(padded(pos_at(si)?, bwl))? as usize).min(MAX_BASE_BR_RANGE);
                    if l > 0 {
                        nz += 1;
                        sum += l;
                    }
                }
                hidden = nz >= PHTHRESH;
            }
            if hidden {
                // read_coeff_hidden: DC's level is coded halved, its parity
                // implied by the sum of the others.
                let pos = pos_at(0)?;
                let stats = nz_mag(&levels, padded(pos, bwl), bwl, class, false)?;
                let cctx = ((stats + 1) >> 1).min(COEFF_BASE_PH_CONTEXTS - 1);
                let qi = ts.fc.read(&mut ts.r, &COEFF_BASE_PH_CDF, [cctx])?;
                levels.set(padded(pos, bwl), ((qi << 1) + (sum & 1)) as u8)?;
            } else {
                reverse(self, ts, &mut levels, 0, 0, &mut state)?;
            }
        }

        // Signs and high ranges, from the end of block back to DC.
        let mut avg = 0;
        let mut cul: i64 = 0;
        let mut dc_val = 0;
        for c in (0..eob).rev() {
            let pos = pos_at(c)?;
            let mut level = i32::from(levels.get(padded(pos, bwl))?);
            if level == 0 {
                continue;
            }
            let (r, cl) = row_col(pos, bwl);
            let dc_2d = c == 0;
            let dc_1d = (cl == 0 && class == TX_CLASS_HORIZ) || (r == 0 && class == TX_CLASS_VERT);
            let sign = if dc_2d || dc_1d {
                let dctx = if dc_2d { ctx.dc_sign } else { 0 };
                if plane == 0 {
                    ts.fc.read(&mut ts.r, &DC_SIGN_CDF, [0, usize::from(hidden), dctx])? == 1
                } else {
                    ts.r.read_literal(1)? == 1
                }
            } else {
                ts.r.read_bit()?
            };
            if hidden && c == 0 {
                if level >= ((NUM_BASE_LEVELS as i32 + 1) << 1) {
                    let hr = self.adaptive_hr(ts, avg >> 1)? << 1;
                    level += hr;
                    avg = (avg + hr) >> 1;
                }
            } else {
                level = self.high_range(ts, tcq, level, lf_limits(r, cl, class, plane), &mut avg, plane)?;
            }
            if c == 0 {
                dc_val = if sign { -level } else { level };
            }
            if level >> 20 != 0 {
                return Err(Error::Invalid("coefficient level beyond 20 bits"));
            }
            cul += i64::from(level);
        }
        Ok(cul_level(cul, dc_val))
    }

    /// av2_read_coeffs_txb_skip: forward-skip coding of an identity
    /// transform. The decoded eob marks where coding begins ("bob"); every
    /// position from there to the end is coded.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn read_coeffs_txb_skip(
        &mut self,
        ts: &mut TileState,
        xd: &Xd,
        row: usize,
        col: usize,
        plane: usize,
        tx: usize,
        bob_eob: usize,
    ) -> Result<u8> {
        let adj = adjusted_tx_size(tx);
        let bwl = t(&TX_SIZE_WIDE_LOG2, adj)?;
        let height = t(&TX_SIZE_HIGH, adj)?;
        let eob = max_eob(tx)?;
        let tx_type = self.tx_type(xd, plane, row, col, tx)?;
        let scan = Self::scan(tx, tx_type)?;
        let pos_at = |c: usize| -> Result<usize> { Ok(*scan.at(c)? as usize) };
        let size_ctx = txsize_entropy_ctx(tx)?.min(TX_16X16);
        let mut levels = Levels::<u8>::new(1 << bwl);
        let mut signs = Levels::<i8>::new(1 << bwl);
        let bob = eob - bob_eob;
        let up = -((1isize << bwl) + TX_PAD_LEFT as isize);
        // get_br_ctx_skip and get_upper_levels_ctx_2d: left plus above.
        let left_above = |l: &Levels<u8>, p: isize, max: usize| -> Result<usize> {
            Ok((lv(l, p, -1, max)? + lv(l, p, up, max)?).min(6))
        };
        if eob > 1 {
            let pos = pos_at(bob)?;
            let n = height << bwl;
            let cctx = if bob <= n / 8 {
                0
            } else if bob <= n / 4 {
                1
            } else {
                2
            };
            let mut level = ts.fc.read(&mut ts.r, &COEFF_BASE_BOB_CDF, [size_ctx, cctx])? + 1;
            if level > NUM_BASE_LEVELS {
                let br = left_above(&levels, padded_left(pos, bwl), MAX_VAL_BR_CTX)?;
                level += self.low_range(ts, &COEFF_BR_CDF_IDTX, [size_ctx, br])?;
            }
            levels.set(padded_left(pos, bwl), level as u8)?;
            // read_coeffs_forward_2d
            for c in bob + 1..eob {
                let pos = pos_at(c)?;
                let p = padded_left(pos, bwl);
                let cctx = left_above(&levels, p, 3)?;
                let mut level = ts.fc.read(&mut ts.r, &COEFF_BASE_CDF_IDTX, [size_ctx, cctx])?;
                if level > NUM_BASE_LEVELS {
                    let br = left_above(&levels, p, MAX_VAL_BR_CTX)?;
                    level += self.low_range(ts, &COEFF_BR_CDF_IDTX, [size_ctx, br])?;
                }
                levels.set(p, level as u8)?;
            }
        }
        let mut avg = 0;
        let mut cul: i64 = 0;
        let mut dc_val = 0;
        for c in bob..eob {
            let pos = pos_at(c)?;
            let p = padded_left(pos, bwl);
            let mut level = i32::from(levels.get(p)?);
            if level == 0 {
                continue;
            }
            // get_sign_ctx_skip: the signs left, above and above-left.
            let mut sc = 0i32;
            for off in [-1, up, up - 1] {
                if levels.get(p + off)? != 0 {
                    sc += i32::from(signs.get(p + off)?);
                }
            }
            let mut sctx = match sc {
                s if s > 2 => 5,
                s if s < -2 => 6,
                s if s > 0 => 1,
                s if s < 0 => 2,
                _ => 0,
            };
            if level as usize > COEFF_BASE_RANGE && sctx != 0 {
                sctx += 2;
            }
            let sign = ts.fc.read(&mut ts.r, &IDTX_SIGN_CDF, [size_ctx, sctx])? == 1;
            signs.set(p, if sign { -1 } else { 1 })?;
            level = self.high_range(ts, false, level, false, &mut avg, plane)?;
            if c == 0 {
                dc_val = if sign { -level } else { level };
            }
            if level >> 20 != 0 {
                return Err(Error::Invalid("FSC coefficient level beyond 20 bits"));
            }
            cul += i64::from(level);
        }
        Ok(cul_level(cul, dc_val))
    }
}

/// The entropy context a transform block leaves: its level sum clipped to
/// COEFF_CONTEXT_MASK, and set_dc_sign's DC sign above it.
fn cul_level(sum: i64, dc: i32) -> u8 {
    let mut v = sum.min(COEFF_CONTEXT_MASK as i64) as u8;
    if dc < 0 {
        v |= 1 << COEFF_CONTEXT_BITS;
    } else if dc > 0 {
        v += 2 << COEFF_CONTEXT_BITS;
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rice_parameter_follows_the_thresholds() {
        assert_eq!([0, 3, 4, 7, 8, 16, 31, 32, 64, 1000].map(adaptive_param), [1, 1, 2, 2, 3, 4, 4, 5, 6, 6]);
    }

    #[test]
    fn tcq_steps_only_in_8_state_mode() {
        assert_eq!(tcq_next(0, 1), 0);
        assert_eq!(tcq_next(256, 1), 256 | 4);
        assert_eq!(tcq_next(256 | 4, 0), 256 | 6);
    }

    #[test]
    fn cul_level_packs_the_dc_sign() {
        assert_eq!(cul_level(100, 0), 7);
        assert_eq!(cul_level(5, -3), 5 | 8);
        assert_eq!(cul_level(5, 3), 5 + 16);
    }

    #[test]
    fn levels_refuse_out_of_range_reads() {
        let l = Levels::<u8>::new(32);
        assert!(l.get(-1).is_ok());
        assert!(l.get(-((TX_PAD_TOP * 36) as isize) - 1).is_err());
        assert!(l.get(TX_PAD_2D as isize).is_err());
    }
}
