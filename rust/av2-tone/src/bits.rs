//! The uncompressed-header bit reader. Mirrors AVM's avm_dsp/bitreader_buffer.c
//! function for function, except that running off the end is an error where
//! AVM longjmps through its error handler.

use crate::error::{Error, Result};

pub struct BitReader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> BitReader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        BitReader { data, pos: 0 }
    }

    /// Bits consumed so far.
    pub fn bit_pos(&self) -> usize {
        self.pos
    }

    /// Bytes touched so far, rounding a partial byte up (avm_rb_bytes_read).
    pub fn bytes_read(&self) -> usize {
        self.pos.div_ceil(8)
    }

    pub fn bit(&mut self) -> Result<bool> {
        let byte = self.data.get(self.pos >> 3).ok_or(Error::Truncated("header bits"))?;
        let bit = (byte >> (7 - (self.pos & 7))) & 1;
        self.pos += 1;
        Ok(bit == 1)
    }

    /// An n-bit unsigned literal, most significant bit first. n <= 32.
    pub fn f(&mut self, n: u32) -> Result<u32> {
        debug_assert!(n <= 32);
        let mut v: u32 = 0;
        for _ in 0..n {
            v = (v << 1) | u32::from(self.bit()?);
        }
        Ok(v)
    }

    /// avm_rb_read_uvlc: leading zeros, a one, then that many bits.
    pub fn uvlc(&mut self) -> Result<u32> {
        let mut zeros = 0;
        while !self.bit()? {
            zeros += 1;
            if zeros == 32 {
                return Err(Error::Invalid("uvlc longer than 32 bits"));
            }
        }
        // zeros <= 31, so the sum stays below 2^32 - 1.
        Ok(((1u32 << zeros) - 1) + self.f(zeros)?)
    }

    /// avm_rb_read_inv_signed_literal: bits + 1 bits, two's complement.
    pub fn inv_signed(&mut self, bits: u32) -> Result<i32> {
        debug_assert!(bits < 31);
        let shift = 32 - (bits + 1);
        let v = self.f(bits + 1)?;
        Ok(((v << shift) as i32) >> shift)
    }

    /// avm_rb_read_primitive_quniform, also AVM's rb_read_uniform: a value in
    /// 0..n with the short codes first.
    pub fn quniform(&mut self, n: u32) -> Result<u32> {
        if n <= 1 {
            return Ok(0);
        }
        let l = 32 - n.leading_zeros();
        let m = (1u32 << l) - n;
        let v = self.f(l - 1)?;
        Ok(if v < m { v } else { (v << 1) - m + u32::from(self.bit()?) })
    }

    /// avm_rb_read_primitive_ref_quniform: one bit saying "not r", then which.
    pub fn ref_quniform(&mut self, n: u32, r: u32) -> Result<u32> {
        if self.bit()? {
            let v = self.quniform(n - 1)?;
            Ok(v + u32::from(v >= r))
        } else {
            Ok(r)
        }
    }

    /// av2_check_trailing_bits: a one, then zeros to the byte boundary. Takes
    /// a whole byte when already aligned.
    pub fn trailing_bits(&mut self) -> Result<()> {
        let n = 8 - (self.pos & 7) as u32;
        if self.f(n)? != 1 << (n - 1) {
            return Err(Error::Invalid("trailing bits"));
        }
        Ok(())
    }

    /// av2_check_byte_alignment: zeros to the byte boundary.
    pub fn byte_align(&mut self) -> Result<()> {
        while self.pos & 7 != 0 {
            if self.bit()? {
                return Err(Error::Invalid("nonzero alignment bit"));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literals_and_uvlc() {
        // 1 | 010 | 00111 | 0... : uvlc 0, uvlc 1, uvlc 6
        let mut r = BitReader::new(&[0b1010_0011, 0b1000_0000]);
        assert_eq!(r.uvlc(), Ok(0));
        assert_eq!(r.uvlc(), Ok(1));
        assert_eq!(r.uvlc(), Ok(6));
        assert_eq!(r.bit_pos(), 9);
    }

    #[test]
    fn signed() {
        let mut r = BitReader::new(&[0b1111_1110, 0b0000_0000]);
        assert_eq!(r.inv_signed(6), Ok(-1)); // 1111111
        let mut r = BitReader::new(&[0b0000_0110]);
        assert_eq!(r.inv_signed(6), Ok(3)); // 0000011
    }

    #[test]
    fn quniform_matches_avm() {
        // n = 5: l = 3, m = 3. Values 0..2 take 2 bits, 3 and 4 take 3.
        let mut r = BitReader::new(&[0b1011_1000]);
        assert_eq!(r.quniform(5), Ok(2)); // 10
        assert_eq!(r.quniform(5), Ok(4)); // 11 1
        assert_eq!(r.bit_pos(), 5);
    }

    #[test]
    fn running_off_the_end_is_an_error() {
        let mut r = BitReader::new(&[0xff]);
        assert_eq!(r.f(8), Ok(255));
        assert!(matches!(r.bit(), Err(Error::Truncated(_))));
        let mut r = BitReader::new(&[0, 0, 0, 0, 0]);
        assert!(r.uvlc().is_err());
    }

    #[test]
    fn trailing() {
        let mut r = BitReader::new(&[0b1011_0000]);
        assert_eq!(r.f(3), Ok(0b101));
        assert_eq!(r.trailing_bits(), Ok(()));
        let mut r = BitReader::new(&[0b1010_0000]);
        assert_eq!(r.f(3), Ok(0b101));
        assert!(r.trailing_bits().is_err());
    }
}
