//! Dequantization: AVM's per-segment quantizer steps (setup_segmentation_dequant,
//! av2_dc_quant_QTX, av2_ac_quant_QTX), its quantization matrices
//! (av2_qm_init, av2_get_iqmatrix) and the coefficient arithmetic in
//! av2_read_coeffs_txb, trellis-coded quantization's second pass included.
//! The cross-chroma transform (av2_inv_cross_chroma_tx_block) is here too,
//! since AVM applies it to the dequantized coefficients before the inverse
//! transform sees them.

use crate::Headers;
use crate::avm_defs::*;
use crate::blk::{At, t};
use crate::block::adjusted_tx_size;
use crate::error::{Error, Result};

/// AVM's max qindex for a bit depth (MAXQ_8_BITS, MAXQ_10_BITS, MAXQ).
fn max_q(bit_depth: u32) -> i32 {
    (match bit_depth {
        8 => MAXQ_8_BITS,
        10 => MAXQ_10_BITS,
        _ => MAXQ,
    }) as i32
}

/// get_q: qlookup of the clamped index, with AVM's qindex-0 exception.
fn get_q(qindex: i32, delta: i32, bit_depth: u32) -> Result<i32> {
    if qindex == 0 && delta <= 0 {
        return Ok(*Q_LOOKUP.at(0)? as i32);
    }
    let q = (qindex + delta).clamp(1, max_q(bit_depth));
    Ok(*Q_LOOKUP.at(q as usize)? as i32)
}

/// Where each transform size's matrix starts in a level's
/// predefined_iwt_matrix_ref row, as av2_qm_init lays them out: one per
/// adjusted size, in TX_SIZE order; larger sizes reuse their adjusted one.
fn qm_offsets() -> Result<[usize; TX_SIZES_ALL]> {
    let mut off = [0; TX_SIZES_ALL];
    let mut current = 0;
    for tx in 0..TX_SIZES_ALL {
        let adj = adjusted_tx_size(tx);
        if adj == tx {
            *off.at_mut(tx)? = current;
            current += t(&TX_SIZE_2D, tx)?;
        } else {
            *off.at_mut(tx)? = *off.at(adj)?;
        }
    }
    if current > QM_TOTAL_SIZE {
        return Err(Error::Invalid("quantization matrix layout"));
    }
    Ok(off)
}

/// The frame's dequantization state: what setup_segmentation_dequant leaves
/// in quant_params, per segment and plane.
pub(crate) struct Dequant {
    /// [segment][plane][DC, AC].
    dq: [[[i32; 2]; 3]; MAX_SEGMENTS],
    /// [segment][plane][transforms up to 8x8, larger ones]: the QM level, or
    /// NUM_QM_LEVELS - 1 for none.
    level: [[[usize; 2]; 3]; MAX_SEGMENTS],
    qm_off: [usize; TX_SIZES_ALL],
    bit_depth: u32,
}

impl Dequant {
    pub fn new(h: &Headers) -> Result<Dequant> {
        let s = &h.seq;
        let q = &h.frame.quant;
        let bd = s.bit_depth;
        let mut d = Dequant {
            dq: [[[0; 2]; 3]; MAX_SEGMENTS],
            level: [[[NUM_QM_LEVELS - 1; 2]; 3]; MAX_SEGMENTS],
            qm_off: qm_offsets()?,
            bit_depth: bd,
        };
        let seg = &h.frame.seg;
        let segments = if seg.enabled { MAX_SEGMENTS } else { 1 };
        for i in 0..segments {
            let qi = *q.qindex.at(i)? as i32;
            let dq = d.dq.at_mut(i)?;
            dq[0] = [get_q(qi, q.y_dc_delta_q + s.base_y_dc_delta_q, bd)?, get_q(qi, 0, bd)?];
            dq[1] = [
                get_q(qi, q.u_dc_delta_q + s.base_uv_dc_delta_q, bd)?,
                get_q(qi, q.u_ac_delta_q + s.base_uv_ac_delta_q, bd)?,
            ];
            dq[2] = [
                get_q(qi, q.v_dc_delta_q + s.base_uv_dc_delta_q, bd)?,
                get_q(qi, q.v_ac_delta_q + s.base_uv_ac_delta_q, bd)?,
            ];
            // av2_use_qmatrix; a segment's own QM set reaches transforms up
            // to 8x8 only, larger ones take set 0's level.
            if q.using_qmatrix && !*q.lossless.at(i)? {
                let qm = *q.qm_index.at(i)? as usize;
                for (p, levels) in [&q.qm_y, &q.qm_u, &q.qm_v].into_iter().enumerate() {
                    let small = *levels.at(qm)? as usize;
                    let large = *levels.at(0)? as usize;
                    if small >= NUM_QM_LEVELS || large >= NUM_QM_LEVELS {
                        return Err(Error::Invalid("quantization matrix level"));
                    }
                    *d.level.at_mut(i)?.at_mut(p)? = [small, large];
                }
            }
        }
        Ok(d)
    }

    /// get_dqv for a coefficient at `pos` of a block in `segment` and
    /// `plane`, through av2_get_iqmatrix's matrix for its size and type.
    pub fn dqv(&self, segment: usize, plane: usize, tx: usize, tx_type: usize, pos: usize) -> Result<i32> {
        let dqv = *self.dq.at(segment)?.at(plane)?.at(usize::from(pos != 0))?;
        // 1-D and identity transforms use the flat matrix (is_2d_transform).
        if crate::residual::primary(tx_type) >= IDTX {
            return Ok(dqv);
        }
        // seg_iqmatrix is indexed by the adjusted size, and so is the test.
        let adj = adjusted_tx_size(tx);
        let large = adj > TX_8X8 && adj != TX_4X8 && adj != TX_8X4;
        let level = *self.level.at(segment)?.at(plane)?.at(usize::from(large))?;
        if level == NUM_QM_LEVELS - 1 {
            return Ok(dqv);
        }
        let off = *self.qm_off.at(adj)?;
        let w = i64::from(*PREDEFINED_IWT_MATRIX_REF.at(level)?.at(usize::from(plane > 0))?.at(off + pos)?);
        Ok(((w * i64::from(dqv) + (1 << (AVM_QM_BITS - 1))) >> AVM_QM_BITS) as i32)
    }

    fn clamp(&self, v: i64) -> i32 {
        let max = (1i64 << (7 + self.bit_depth)) - 1;
        v.clamp(-max - 1, max) as i32
    }

    /// Turns a block's signed levels (at their positions) into dequantized
    /// coefficients, in place: av2_read_coeffs_txb's per-coefficient path, or
    /// with TCQ its second pass, which walks the scan from the end of block.
    #[allow(clippy::too_many_arguments)]
    pub fn apply(
        &self,
        coeffs: &mut [i32],
        scan: &[u16],
        eob: usize,
        segment: usize,
        plane: usize,
        tx: usize,
        tx_type: usize,
        tcq: bool,
    ) -> Result<()> {
        // av2_get_tx_scale
        let pels = t(&TX_SIZE_2D, tx)?;
        let shift = u32::from(pels > 256) + u32::from(pels > 1024);
        let mut state = i32::from(tcq) << 8;
        for c in (0..eob).rev() {
            let pos = *scan.at(c)? as usize;
            let v = *coeffs.at(pos)?;
            let level = i64::from(v.unsigned_abs());
            if level != 0 {
                let dqv = i64::from(self.dqv(segment, plane, tx, tx_type, pos)?);
                let (idx, s) = if tcq {
                    // qIdx = max(0, 2 * level - Qx), dequantized one bit finer.
                    let qx = i64::from(state & 2 != 0);
                    ((level << 1) - qx, shift + 1)
                } else {
                    (level, shift)
                };
                if tcq && idx >> 20 != 0 {
                    return Err(Error::Invalid("TCQ coefficient level beyond 20 bits"));
                }
                let hp = (idx.max(0) * dqv) & 0xff_ffff;
                let dq = ((hp + (1 << (QUANT_TABLE_BITS - 1))) >> QUANT_TABLE_BITS) >> s;
                *coeffs.at_mut(pos)? = self.clamp(if v < 0 { -dq } else { dq });
            }
            if tcq {
                state = crate::txb::tcq_next(state, level as usize);
            }
        }
        Ok(())
    }

    /// av2_inv_cross_chroma_tx_block: rotates each U/V coefficient pair by
    /// the block's CCTX angle.
    pub fn cross_chroma(&self, u: &mut [i32], v: &mut [i32], tx: usize, cctx: usize) -> Result<()> {
        if cctx == CCTX_NONE {
            return Ok(());
        }
        let m = CCTX_MTX.at(cctx.checked_sub(CCTX_START).ok_or(Error::Invalid("CCTX type"))?)?;
        let (a, b) = (i64::from(m[0]), i64::from(m[1]));
        let round = |x: i64| -> i64 {
            // ROUND_POWER_OF_TWO_SIGNED_64
            let half = 1i64 << (CCTX_PREC_BITS - 1);
            if x < 0 { -((-x + half) >> CCTX_PREC_BITS) } else { (x + half) >> CCTX_PREC_BITS }
        };
        let lim = 1i64 << (7 + self.bit_depth); // clamp_value(x, 8 + bd)
        for i in 0..crate::residual::max_eob(tx)? {
            let (c1, c2) = (i64::from(*u.at(i)?), i64::from(*v.at(i)?));
            *u.at_mut(i)? = round(a * c1 - b * c2).clamp(-lim, lim - 1) as i32;
            *v.at_mut(i)? = round(b * c1 + a * c2).clamp(-lim, lim - 1) as i32;
        }
        Ok(())
    }
}
