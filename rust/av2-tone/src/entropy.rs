//! The multi-symbol arithmetic decoder and CDF adaptation. A port of AVM's
//! avm_dsp/entdec.c, entdec.h, entcode.c, the reader half of bitreader.h,
//! and update_cdf and od_ec_prob_scale from prob.h, operation for operation.
//!
//! Reading past the end of the tile's bytes is defined, as in AVM: the window
//! fills with zeros and `tell` keeps counting. `has_overflowed` reports it,
//! the way AVM's decoder checks for a corrupt tile.
//!
//! A CDF here is AVM's `CDF_SIZE(n)` layout: n inverse-CDF values (the last
//! is 0), a symbol counter, then three adaptation rates, one per counter
//! interval. AV2 carries the rates in the table; AV1 derived them from n.

use crate::error::{Error, Result};

/// Window bits; od_ec_window is 64 bits in AVM.
const WINDOW: i32 = 64;
const EC_MIN_BITS: i32 = 8;
const LOTS_OF_BITS: i32 = 0x4000;
const EC_PROB_SHIFT: u32 = 7;
/// Extra CDF entries after the n probabilities: counter and three rates.
pub const CDF_EXTRA: usize = 4;

/// av2_prob_inc_tbl, entcode.c: the minimum width each symbol keeps after
/// scaling, by alphabet size. AVM stores -1 for unused entries in a uint16_t.
const PROB_INC: [[u16; 16]; 15] = {
    const X: u16 = 0xffff;
    [
        [8, 0, X, X, X, X, X, X, X, X, X, X, X, X, X, X],
        [10, 5, 0, X, X, X, X, X, X, X, X, X, X, X, X, X],
        [12, 8, 4, 0, X, X, X, X, X, X, X, X, X, X, X, X],
        [12, 9, 6, 3, 0, X, X, X, X, X, X, X, X, X, X, X],
        [13, 10, 8, 5, 2, 0, X, X, X, X, X, X, X, X, X, X],
        [13, 11, 9, 6, 4, 2, 0, X, X, X, X, X, X, X, X, X],
        [14, 12, 10, 8, 6, 4, 2, 0, X, X, X, X, X, X, X, X],
        [14, 12, 10, 8, 7, 5, 3, 1, 0, X, X, X, X, X, X, X],
        [14, 12, 11, 9, 8, 6, 4, 3, 1, 0, X, X, X, X, X, X],
        [14, 13, 11, 10, 8, 7, 5, 4, 2, 1, 0, X, X, X, X, X],
        [14, 13, 12, 10, 9, 8, 6, 5, 4, 2, 1, 0, X, X, X, X],
        [14, 13, 12, 11, 9, 8, 7, 6, 4, 3, 2, 1, 0, X, X, X],
        [14, 13, 12, 11, 10, 9, 8, 6, 5, 4, 3, 2, 1, 0, X, X],
        [14, 13, 12, 11, 10, 9, 8, 7, 6, 5, 4, 3, 2, 1, 0, X],
        [15, 14, 13, 12, 11, 10, 9, 8, 7, 6, 5, 4, 3, 2, 1, 0],
    ]
};

/// od_ec_prob_scale: where symbol n's boundary falls in the current range.
fn prob_scale(p: u16, r: u32, n: usize, nsyms: usize) -> Result<u32> {
    let inc = PROB_INC.get(nsyms.wrapping_sub(2)).and_then(|row| row.get(n)).ok_or(Error::Invalid("alphabet size"))?;
    let rr = r >> 8;
    let pp = (u32::from(p >> EC_PROB_SHIFT) << 4) + u32::from(*inc);
    Ok(((rr * pp) >> 7) << 3)
}

/// update_cdf, prob.h: move the CDF toward the symbol just coded at the rate
/// the table gives for the current counter interval, then count the symbol.
pub fn update_cdf(cdf: &mut [u16], val: usize, nsyms: usize) -> Result<()> {
    if !(2..=16).contains(&nsyms) || cdf.len() < nsyms + CDF_EXTRA {
        return Err(Error::Invalid("CDF shorter than its alphabet"));
    }
    let (probs, tail) = cdf.split_at_mut(nsyms);
    let [count, r0, r1, r2, ..] = tail else { return Err(Error::Invalid("CDF tail")) };
    let interval_rate = if *count > 31 {
        *r2
    } else if *count > 15 {
        *r1
    } else {
        *r0
    };
    let rate = 2 + u32::from(interval_rate);
    let mut tmp: u32 = 32768; // AVM_ICDF(0)
    for (i, p) in probs.iter_mut().take(nsyms - 1).enumerate() {
        if i == val {
            tmp = 0;
        }
        let c = u32::from(*p);
        let next = if tmp < c {
            c - ((c - tmp).checked_shr(rate).unwrap_or(0))
        } else {
            c + ((tmp - c).checked_shr(rate).unwrap_or(0))
        };
        *p = next as u16;
    }
    if *count < 32 {
        *count += 1;
    }
    Ok(())
}

/// od_ec_tell_frac, entcode.c: bits used, in 1/65536ths, rounded up.
fn tell_frac(nbits_total: u32, mut rng: u32) -> u64 {
    let nbits = u64::from(nbits_total) << 16;
    let mut l: u64 = 0;
    for _ in 0..16 {
        rng = (rng * rng) >> 15;
        let b = rng >> 16;
        l = (l << 1) | u64::from(b);
        rng >>= b;
    }
    nbits.wrapping_sub(l)
}

/// AVM's SYMBOL_CODING_MODE (av2/decoder/accounting.h).
pub const SYMBOL_BIT: u8 = 0;
pub const SYMBOL_BIT_BYPASS: u8 = 1;
pub const SYMBOL_LITERAL_BYPASS: u8 = 2;
pub const SYMBOL_UNARY: u8 = 3;
pub const SYMBOL_CDF: u8 = 4;

/// One read, as AVM's accounting build records it: where (the block's mi
/// position and tree), how it was coded, the value, and its cost in 1/65536
/// bits since the previous record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TraceSym {
    pub x: i32,
    pub y: i32,
    pub tree: u8,
    pub mode: u8,
    pub value: i32,
    pub bits: u64,
}

/// A symbol-by-symbol record of a tile's parse, to diff against AVM's.
#[derive(Debug, Clone, Default)]
pub struct Trace {
    pub syms: Vec<TraceSym>,
    ctx: (i32, i32, u8),
    last: u64,
}

/// AVM's avm_reader over od_ec_dec, for one tile.
#[derive(Debug, Clone)]
pub struct SymbolReader<'a> {
    buf: &'a [u8],
    /// Next byte to load (od_ec_dec.bptr - buf).
    pos: usize,
    tell_offs: i32,
    dif: u64,
    rng: u32,
    cnt: i32,
    /// Adapt CDFs after each symbol (!disable_cdf_update).
    pub allow_update_cdf: bool,
    trace: Option<Box<Trace>>,
}

impl<'a> SymbolReader<'a> {
    /// avm_reader_init / avm_od_ec_dec_init.
    pub fn new(buf: &'a [u8], allow_update_cdf: bool) -> Self {
        let mut d = SymbolReader {
            buf,
            pos: 0,
            tell_offs: 0,
            dif: (1u64 << (WINDOW - 1)) - 1,
            rng: 0x8000,
            cnt: -15,
            allow_update_cdf,
            trace: None,
        };
        d.tell_offs = d.cnt + 1;
        d.refill();
        d
    }

    /// Starts recording every read, as AVM's accounting does from the
    /// moment a tile's reader is set up.
    pub fn enable_trace(&mut self) {
        let last = self.tell_frac();
        self.trace = Some(Box::new(Trace { last, ..Trace::default() }));
    }

    /// avm_accounting_set_context: the block the next reads belong to.
    pub fn set_trace_context(&mut self, mi_col: i32, mi_row: i32, tree: u8) {
        if let Some(t) = self.trace.as_mut() {
            t.ctx = (mi_col, mi_row, tree);
        }
    }

    /// The block the next reads will be recorded against.
    pub fn trace_context(&self) -> Option<(i32, i32, u8)> {
        self.trace.as_ref().map(|t| t.ctx)
    }

    /// The reads recorded so far.
    pub fn take_trace(&mut self) -> Vec<TraceSym> {
        self.trace.as_mut().map(|t| std::mem::take(&mut t.syms)).unwrap_or_default()
    }

    /// avm_process_accounting.
    fn record(&mut self, mode: u8, value: u32) {
        let now = match &self.trace {
            Some(_) => self.tell_frac(),
            None => return,
        };
        if let Some(t) = self.trace.as_mut() {
            let (x, y, tree) = t.ctx;
            t.syms.push(TraceSym { x, y, tree, mode, value: value as i32, bits: now.wrapping_sub(t.last) });
            t.last = now;
        }
    }

    /// od_ec_dec_refill: load whole bytes into the window; past the end, act
    /// as if lots of zero bytes were loaded.
    fn refill(&mut self) {
        let mut s = WINDOW - 9 - (self.cnt + 15);
        while s >= 0 {
            let Some(&b) = self.buf.get(self.pos) else { break };
            self.dif ^= u64::from(b) << s;
            self.cnt += 8;
            self.pos += 1;
            s -= 8;
        }
        if self.pos >= self.buf.len() {
            self.tell_offs += LOTS_OF_BITS - self.cnt;
            self.cnt = LOTS_OF_BITS;
        }
    }

    /// od_ec_dec_normalize: bring rng back to [32768, 65536).
    fn normalize(&mut self, dif: u64, rng: u32, ret: usize) -> Result<usize> {
        if rng == 0 || rng > 0xffff {
            return Err(Error::Invalid("arithmetic decoder range"));
        }
        let d = rng.leading_zeros() as i32 - 16; // 16 - OD_ILOG_NZ(rng)
        self.cnt -= d;
        self.dif = (dif.wrapping_add(1) << d).wrapping_sub(1);
        self.rng = rng << d;
        if self.cnt < EC_MIN_BITS {
            self.refill();
        }
        Ok(ret)
    }

    /// od_ec_dec_bypass_normalize: n bypass bits consumed, range unchanged.
    fn bypass_normalize(&mut self, dif: u64, n: u32, ret: u32) -> u32 {
        self.cnt -= n as i32;
        self.dif = (dif.wrapping_add(1) << n).wrapping_sub(1);
        if self.cnt < EC_MIN_BITS {
            self.refill();
        }
        ret
    }

    /// avm_od_ec_decode_cdf_q15: one symbol from an inverse CDF, no update.
    pub fn read_cdf(&mut self, icdf: &[u16], nsyms: usize) -> Result<usize> {
        if !(2..=16).contains(&nsyms) || icdf.len() < nsyms || icdf.get(nsyms - 1) != Some(&0) {
            return Err(Error::Invalid("CDF does not end at 0"));
        }
        let r = self.rng;
        let c = (self.dif >> (WINDOW - 16)) as u32;
        let (mut u, mut v) = (r, r);
        let mut ret = None;
        for (n, &p) in icdf.iter().take(nsyms).enumerate() {
            u = v;
            v = prob_scale(p, r, n, nsyms)?;
            if c >= v {
                ret = Some(n);
                break;
            }
        }
        let ret = ret.ok_or(Error::Invalid("symbol past the CDF"))?;
        if v >= u {
            return Err(Error::Invalid("CDF not decreasing"));
        }
        let dif = self.dif.wrapping_sub(u64::from(v) << (WINDOW - 16));
        let s = self.normalize(dif, u - v, ret)?;
        self.record(SYMBOL_CDF, s as u32);
        Ok(s)
    }

    /// avm_read_symbol: a symbol, then adapt the CDF if the frame allows it.
    pub fn read_symbol(&mut self, cdf: &mut [u16], nsyms: usize) -> Result<usize> {
        let s = self.read_cdf(cdf, nsyms)?;
        if self.allow_update_cdf {
            update_cdf(cdf, s, nsyms)?;
        }
        Ok(s)
    }

    /// avm_od_ec_decode_bool_q15: a bit that is 1 with probability f / 32768.
    pub fn read_bool_q15(&mut self, f: u32) -> Result<bool> {
        if f == 0 || f >= 32768 {
            return Err(Error::Invalid("bool probability"));
        }
        let r = self.rng;
        let v = prob_scale(f as u16, r, 0, 2)?;
        let vw = u64::from(v) << (WINDOW - 16);
        let (dif, r_new, bit) = if self.dif >= vw { (self.dif - vw, r - v, false) } else { (self.dif, v, true) };
        Ok(self.normalize(dif, r_new, usize::from(bit))? == 1)
    }

    /// avm_read: a bit with an 8-bit probability, as AV1's bool coder took it.
    pub fn read_bool(&mut self, prob: u32) -> Result<bool> {
        if prob > 255 {
            return Err(Error::Invalid("8-bit probability"));
        }
        let p = (0x7f_ffff - (prob << 15) + prob) >> 8;
        let b = self.read_bool_q15(p)?;
        self.record(SYMBOL_BIT, u32::from(b));
        Ok(b)
    }

    /// od_ec_decode_literal_bypass: 1..=8 equiprobable bits, MSB first.
    fn bypass(&mut self, n: u32) -> Result<u32> {
        if !(1..=8).contains(&n) {
            return Err(Error::Invalid("bypass width"));
        }
        let mut dif = self.dif;
        let mut vw = u64::from(self.rng) << (WINDOW - 16);
        let mut ret = 0;
        for _ in 0..n {
            vw >>= 1;
            ret <<= 1;
            if dif >= vw {
                dif -= vw;
            } else {
                ret |= 1;
            }
        }
        Ok(self.bypass_normalize(dif, n, ret))
    }

    /// avm_read_bypass: one bypass bit.
    pub fn read_bypass(&mut self) -> Result<bool> {
        let b = self.bypass(1)?;
        self.record(SYMBOL_BIT_BYPASS, b);
        Ok(b == 1)
    }

    /// avm_read_bit. AVM's accounting records it twice: once inside
    /// avm_read_bypass and again here, at no cost.
    pub fn read_bit(&mut self) -> Result<bool> {
        let b = self.read_bypass()?;
        self.record(SYMBOL_BIT_BYPASS, u32::from(b));
        Ok(b)
    }

    /// avm_read_literal: up to 32 bits, MSB first, in 8-bit bypass chunks.
    pub fn read_literal(&mut self, bits: u32) -> Result<u32> {
        if bits > 32 {
            return Err(Error::Invalid("literal width"));
        }
        let mut v: u64 = 0;
        let mut left = bits;
        while left > 0 {
            let n = left.min(8);
            v = (v << n) | u64::from(self.bypass(n)?);
            left -= n;
        }
        self.record(SYMBOL_LITERAL_BYPASS, v as u32);
        Ok(v as u32)
    }

    /// avm_read_unary / od_ec_decode_unary_bypass: count bypass zeros up to a
    /// one, stopping at max_bits.
    pub fn read_unary(&mut self, max_bits: u32) -> Result<u32> {
        if !(1..=32).contains(&max_bits) {
            return Err(Error::Invalid("unary width"));
        }
        if self.cnt < max_bits as i32 - 1 {
            self.refill();
        }
        let mut dif = self.dif;
        let mut vw = u64::from(self.rng) << (WINDOW - 16);
        let mut ret = 0;
        let mut consumed = max_bits;
        for bit in 0..max_bits {
            vw >>= 1;
            if dif >= vw {
                dif -= vw;
                ret += 1;
            } else {
                consumed = bit + 1;
                break;
            }
        }
        let v = self.bypass_normalize(dif, consumed, ret);
        self.record(SYMBOL_UNARY, v);
        Ok(v)
    }

    /// avm_od_ec_dec_tell: whole bits consumed, rounded up.
    pub fn tell(&self) -> u32 {
        ((self.pos as i64) * 8 - i64::from(self.cnt) + i64::from(self.tell_offs)) as u32
    }

    /// avm_od_ec_dec_tell_frac: bits consumed in 1/65536ths.
    pub fn tell_frac(&self) -> u64 {
        tell_frac(self.tell(), self.rng)
    }

    /// avm_reader_has_overflowed: decoding has needed more bytes than the
    /// tile holds.
    pub fn has_overflowed(&self) -> bool {
        (self.tell() as usize).div_ceil(8) > self.buf.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_cdf_moves_toward_the_symbol_and_counts() {
        // A 2-symbol CDF at 50%, rates (0, 1, 2): first interval rate 2 + 0 = 2.
        let mut cdf = [16384u16, 0, 0, 0, 1, 2];
        update_cdf(&mut cdf, 0, 2).unwrap();
        assert_eq!(cdf, [16384 - (16384 >> 2), 0, 1, 0, 1, 2]);
        update_cdf(&mut cdf, 1, 2).unwrap();
        assert_eq!(cdf[0], 12288 + ((32768 - 12288) >> 2));
        // The counter saturates at 32.
        cdf[2] = 32;
        update_cdf(&mut cdf, 1, 2).unwrap();
        assert_eq!(cdf[2], 32);
    }

    #[test]
    fn a_fresh_decoder_has_used_one_bit() {
        let d = SymbolReader::new(&[0x55; 8], true);
        assert_eq!(d.tell(), 1);
        assert_eq!(d.tell_frac(), 1 << 16);
    }

    #[test]
    fn bad_cdfs_are_errors() {
        let mut d = SymbolReader::new(&[0x12, 0x34], true);
        assert!(d.read_cdf(&[100, 5], 2).is_err()); // doesn't end at 0
        assert!(d.read_cdf(&[0; 20], 17).is_err());
        let mut short = [100u16, 0, 0];
        assert!(d.read_symbol(&mut short, 2).is_err());
    }

    #[test]
    fn reading_past_the_end_is_defined_and_reported() {
        let mut d = SymbolReader::new(&[0xa5], true);
        for _ in 0..40 {
            d.read_literal(8).unwrap();
        }
        assert!(d.has_overflowed());
    }
}
