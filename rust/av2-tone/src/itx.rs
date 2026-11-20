//! The inverse transforms: AVM's secondary transform (av2_inv_stxfm, IST)
//! and its primary one (inv_txfm_c), from a transform block's dequantized
//! coefficients to its residual.
//!
//! AVM writes each 1-D DCT as a partial butterfly over a matrix and every
//! other kernel as a plain matrix product. Both sum exact integer products,
//! and the butterfly only uses the DCT matrix's even/odd symmetry, so one
//! matrix product per kernel gives AVM's numbers; the `dct_symmetry` test
//! holds the tables to that symmetry. The sums wrap at 32 bits as AVM's
//! `int` arithmetic does on every target it runs on, which keeps the two
//! forms equal even if a sum overflowed.
//!
//! Lossless blocks never reach here (the parse refuses them), and neither do
//! data-driven transforms (DDTX, FDDT): AVM swaps them in only for inter
//! blocks, which a key frame has none of.

use crate::avm_defs::*;
use crate::blk::{At, t};
use crate::error::{Error, Result};
use crate::parse::FrameParse;
use crate::recon::TxBlock;
use crate::residual::primary;

/// clamp_value: the signed range of `bits` bits.
fn clamp_bits(v: i64, bits: u32) -> i32 {
    let max = (1i64 << (bits - 1)) - 1;
    v.clamp(-max - 1, max) as i32
}

/// ROUND_POWER_OF_TWO_SIGNED on a 32-bit int.
fn round_signed(v: i32, n: u32) -> i32 {
    let half = 1i32 << (n - 1);
    if v < 0 { -((half.wrapping_sub(v)) >> n) } else { v.wrapping_add(half) >> n }
}

/// The block's IST type (1..=3, or 0 for none) and kernel set, as
/// init_txfm_param leaves them.
fn secondary(b: &TxBlock) -> Result<(usize, usize)> {
    let ty = usize::from(b.tx_type);
    let tx = usize::from(b.tx_size);
    let mode_dependent = if b.inter {
        primary(ty) == DCT_DCT && t(&TX_SIZE_WIDE, tx)? >= 16 && t(&TX_SIZE_HIGH, tx)? >= 16
    } else {
        usize::from(b.mode) < PAETH_PRED
    };
    if !mode_dependent {
        return Ok((0, 0));
    }
    Ok(((ty >> PRIMARY_TX_BITS) & ((1 << SECONDARY_TX_BITS) - 1), ty >> (PRIMARY_TX_BITS + SECONDARY_TX_BITS)))
}

/// av2_inv_stxfm, in place on the first 32x32 of `coeff`.
fn inverse_secondary(coeff: &mut [i32], b: &TxBlock, stx: usize, set: usize, bd: u32) -> Result<()> {
    let tx = usize::from(b.tx_size);
    let p = primary(usize::from(b.tx_type));
    let width = t(&TX_SIZE_WIDE, tx)?.min(32);
    let height = t(&TX_SIZE_HIGH, tx)?.min(32);
    if width < 4 || height < 4 {
        return Ok(());
    }
    let big = width >= 8 && height >= 8;
    let scan = FrameParse::scan(tx, p)?;
    let reduced_width = if big { IST_8x8_WIDTH } else { IST_4x4_WIDTH };
    let mut buf0 = [0i32; 64];
    for (r, v) in buf0.iter_mut().take(reduced_width).enumerate() {
        *v = *coeff.at(*scan.at(r)? as usize)?;
    }
    let mode = if b.inter { DC_PRED } else { usize::from(b.mode) }.min(SMOOTH_H_PRED);
    let transpose = matches!(mode, H_PRED | D157_PRED | D67_PRED | SMOOTH_H_PRED);
    let log2w = t(&TX_SIZE_WIDE_LOG2, tx)?;
    let class = if width == 8 && height == 8 && p == DCT_DCT {
        1
    } else if big {
        if p == DCT_DCT { 2 } else { 3 }
    } else {
        0
    };
    let reduced_height = match class {
        0 => IST_4x4_HEIGHT,
        1 => IST_8x8_HEIGHT_RED,
        3 => IST_ADST_NZ_CNT,
        _ => IST_8x8_HEIGHT,
    };
    let k = stx.checked_sub(1).ok_or(Error::Invalid("secondary transform type"))?;
    // inv_stxfm: each output j is column j of the kernel against the inputs.
    let mut buf1 = [0i32; 64];
    for (j, out) in buf1.iter_mut().take(reduced_width).enumerate() {
        let mut resi = 0i32;
        for (i, &s) in buf0.iter().take(reduced_height).enumerate() {
            let w = i32::from(if big {
                *IST_8X8_KERNEL.at(set)?.at(k)?.at(i)?.at(j)?
            } else {
                *IST_4X4_KERNEL.at(set)?.at(k)?.at(i)?.at(j)?
            });
            resi = resi.wrapping_add(s.wrapping_mul(w));
        }
        *out = clamp_bits(i64::from(round_signed(resi, 7)), 8 + bd);
    }
    coeff.get_mut(..width * height).ok_or(Error::Invalid("coefficient buffer"))?.fill(0);
    let i = log2w.checked_sub(2).ok_or(Error::Invalid("transform width"))?;
    for (r, &v) in buf1.iter().take(reduced_width).enumerate() {
        let pos = if big {
            let m = *COEFF8X8_MAPPING.at(set * 3 + k)?.at(r)? as usize;
            if transpose { *STX_SCAN_ORDERS_TRANSPOSE_8X8.at(i)?.at(m)? } else { *STX_SCAN_ORDERS_8X8.at(i)?.at(m)? }
        } else if transpose {
            *STX_SCAN_ORDERS_TRANSPOSE_4X4.at(i)?.at(r)?
        } else {
            *STX_SCAN_ORDERS_4X4.at(i)?.at(r)?
        };
        *coeff.at_mut(pos as usize)? = v;
    }
    Ok(())
}

/// A kernel's coefficient for input frequency `r` at output `c`.
fn kernel_at(kind: usize, n: usize, r: usize, c: usize) -> Result<i32> {
    let inv = INV_TXFM;
    let v = match (kind, n) {
        (DCT2, 4) => *TX_KERNEL_DCT2_SIZE4.at(inv)?.at(r)?.at(c)?,
        (DCT2, 8) => *TX_KERNEL_DCT2_SIZE8.at(inv)?.at(r)?.at(c)?,
        (DCT2, 16) => *TX_KERNEL_DCT2_SIZE16.at(inv)?.at(r)?.at(c)?,
        (DCT2, 32) => *TX_KERNEL_DCT2_SIZE32.at(inv)?.at(r)?.at(c)?,
        (DST7, 4) => *TX_KERNEL_ADST_SIZE4.at(inv)?.at(r)?.at(c)?,
        (DST7, 8) => *TX_KERNEL_ADST_SIZE8.at(inv)?.at(r)?.at(c)?,
        (DST7, 16) => *TX_KERNEL_ADST_SIZE16.at(inv)?.at(r)?.at(c)?,
        (DCT8, 4) => *TX_KERNEL_FDST_SIZE4.at(inv)?.at(r)?.at(c)?,
        (DCT8, 8) => *TX_KERNEL_FDST_SIZE8.at(inv)?.at(r)?.at(c)?,
        (DCT8, 16) => *TX_KERNEL_FDST_SIZE16.at(inv)?.at(r)?.at(c)?,
        // inv_transform_1d_c has no 32-point ADST or flipped ADST, and DDTX
        // and FDDT are inter-only.
        _ => return Err(Error::Invalid("1-D transform type for its size")),
    };
    Ok(i32::from(v))
}

/// inv_transform_1d_c over `line` vectors of `n` samples: src's vector i is
/// src[i * n..], and output j of it lands at dst[j * line + i] (transposed).
/// Each output is clamped to `range`.
fn inverse_1d(
    src: &[i32],
    dst: &mut [i32],
    kind: usize,
    n: usize,
    line: usize,
    shift: u32,
    (lo, hi): (i32, i32),
) -> Result<()> {
    let add = 1i32 << (shift - 1);
    if kind == IDT {
        let scale = match n {
            4 => 128,
            8 => 181,
            16 => 256,
            _ => 362,
        };
        for i in 0..line {
            for j in 0..n {
                let v = src.at(i * n + j)?.wrapping_mul(scale).wrapping_add(add) >> shift;
                *dst.at_mut(j * line + i)? = v.clamp(lo, hi);
            }
        }
        return Ok(());
    }
    // The kernel, read once per call rather than per sample.
    let mut mat = [0i32; 32 * 32];
    for r in 0..n {
        for c in 0..n {
            *mat.at_mut(r * n + c)? = kernel_at(kind, n, r, c)?;
        }
    }
    for i in 0..line {
        let v = src.get(i * n..i * n + n).ok_or(Error::Invalid("transform buffer"))?;
        for j in 0..n {
            let mut sum = 0i32;
            for (k, &s) in v.iter().enumerate() {
                sum = sum.wrapping_add(s.wrapping_mul(*mat.at(k * n + j)?));
            }
            *dst.at_mut(j * line + i)? = (sum.wrapping_add(add) >> shift).clamp(lo, hi);
        }
    }
    Ok(())
}

/// A transform block's residual, `tx_size_wide x tx_size_high` row-major:
/// what inv_txfm_c adds to the prediction (before the clip to the bit depth).
pub fn residual(b: &TxBlock, bd: u32) -> Result<Vec<i32>> {
    let tx = usize::from(b.tx_size);
    let (w, h) = (t(&TX_SIZE_WIDE, tx)?, t(&TX_SIZE_HIGH, tx)?);
    let mut coeff = vec![0i32; MAX_TX_SQUARE];
    let n = b.coeffs.len().min(t(&TX_SIZE_2D, tx)?);
    coeff
        .get_mut(..n)
        .ok_or(Error::Invalid("coefficient count"))?
        .copy_from_slice(b.coeffs.get(..n).ok_or(Error::Invalid("coefficient count"))?);

    let (stx, set) = secondary(b)?;
    if stx != 0 {
        inverse_secondary(&mut coeff, b, stx, set, bd)?;
    }

    let p = primary(usize::from(b.tx_type));
    let (width, height) = (w.min(32), h.min(32));
    let (log2w, log2h) = (t(&TX_SIZE_WIDE_LOG2, tx)?, t(&TX_SIZE_HIGH_LOG2, tx)?);
    let count = (width * height).min(1024);
    // A 2:1 block's coefficients are scaled by 1/sqrt(2) first.
    let mut block = vec![0i32; MAX_TX_SQUARE];
    for (i, out) in block.iter_mut().take(count).enumerate() {
        let v = i64::from(*coeff.at(i)?);
        let v = if (log2w + log2h) & 1 == 1 {
            ((v * NewInvSqrt2 as i64 + (1 << (NewSqrt2Bits - 1))) >> NewSqrt2Bits) as i32
        } else {
            v as i32
        };
        *out = clamp_bits(i64::from(v), bd + 8);
    }

    let shifts = INV_TX_SHIFT.at(tx)?;
    let (row_kind, col_kind) = (t(&G_HOR_TX_TYPE, p)?, t(&G_VER_TX_TYPE, p)?);
    let mid = 1i32 << (bd + 7);
    let out = 1i32 << bd;
    let mut tmp = vec![0i32; MAX_TX_SQUARE];
    inverse_1d(&block, &mut tmp, row_kind, width, height, u32::from(shifts[0]), (-mid, mid - 1))?;
    inverse_1d(&tmp, &mut block, col_kind, height, width, u32::from(shifts[1]), (-out, out - 1))?;

    // 64-point sides run at 32 and are widened by repeating each sample.
    let mut res = vec![0i32; w * h];
    let (sx, sy) = (w / width, h / height);
    for y in 0..h {
        for x in 0..w {
            *res.at_mut(y * w + x)? = *block.at((y / sy) * width + x / sx)?;
        }
    }
    Ok(res)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// AVM's partial butterfly folds each output pair (c, n-1-c) into a sum
    /// and a difference, then does the same to the even rows' first half,
    /// down to 4 points. It equals the full product exactly when, at every
    /// level, row r is even about the middle for even r and odd for odd r.
    fn butterfly_holds(rows: &[Vec<i32>], what: &str) {
        // The last two columns are read straight from the table (AVM's c/d,
        // e/f, g/h pairs), so folding stops at 4 points.
        let n = rows[0].len();
        if n < 4 {
            return;
        }
        for (r, row) in rows.iter().enumerate() {
            for c in 0..n / 2 {
                let want = if r % 2 == 0 { row[c] } else { -row[c] };
                assert_eq!(row[n - 1 - c], want, "{what}: {n}-point level, row {r}, column {c}");
            }
        }
        let even: Vec<Vec<i32>> = rows.iter().step_by(2).map(|row| row[..n / 2].to_vec()).collect();
        if even.len() >= 2 {
            butterfly_holds(&even, what);
        }
    }

    #[test]
    fn dct_symmetry() {
        for n in [4, 8, 16, 32] {
            let rows: Vec<Vec<i32>> =
                (0..n).map(|r| (0..n).map(|c| kernel_at(DCT2, n, r, c).unwrap()).collect()).collect();
            butterfly_holds(&rows, &format!("DCT2 size {n}"));
        }
    }
}
