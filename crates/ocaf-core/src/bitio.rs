//! MSB-first bit stream primitives used by the entropy coder.
//!
//! Bits are written most-significant-bit first within each byte. Unused bits
//! at the end of a frame payload are zero padding and are ignored by the
//! decoder; the frame length and frame CRC make the boundary unambiguous.

use crate::error::{Error, Result};

/// A growable, MSB-first bit writer.
#[derive(Debug, Default)]
pub struct BitWriter {
    bytes: Vec<u8>,
    acc: u128,
    nbits: u32,
}

impl BitWriter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_capacity(bytes: usize) -> Self {
        Self {
            bytes: Vec::with_capacity(bytes),
            acc: 0,
            nbits: 0,
        }
    }

    /// Append the low `n` bits of `value`. `n` must be between 0 and 64.
    #[inline]
    pub fn write_bits(&mut self, value: u64, n: u32) {
        debug_assert!(n <= 64);
        if n == 0 {
            return;
        }
        let value = if n == 64 {
            value as u128
        } else {
            (value & ((1u64 << n) - 1)) as u128
        };
        self.acc = (self.acc << n) | value;
        self.nbits += n;
        while self.nbits >= 8 {
            let shift = self.nbits - 8;
            self.bytes.push(((self.acc >> shift) & 0xff) as u8);
            self.nbits -= 8;
            self.acc &= if self.nbits == 0 {
                0
            } else {
                (1u128 << self.nbits) - 1
            };
        }
    }

    /// Append `count` zero bits followed by a one bit (Rice unary prefix).
    #[inline]
    pub fn write_unary_zeros(&mut self, mut count: u64) {
        while count >= 32 {
            self.write_bits(0, 32);
            count -= 32;
        }
        if count > 0 {
            self.write_bits(0, count as u32);
        }
        self.write_bits(1, 1);
    }

    /// Append a two's-complement signed value using exactly `bits` bits.
    #[inline]
    pub fn write_signed(&mut self, value: i64, bits: u32) {
        debug_assert!((1..=64).contains(&bits));
        self.write_bits(value as u64, bits);
    }

    /// Append raw bytes in order. The writer stays bit-aligned, so this is
    /// equivalent to writing each byte with [`BitWriter::write_bits`].
    pub fn write_bytes(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.write_bits(byte as u64, 8);
        }
    }

    /// Pad the final partial byte with zeros and return the buffer.
    pub fn finish(mut self) -> Vec<u8> {
        if self.nbits > 0 {
            self.bytes
                .push(((self.acc << (8 - self.nbits)) & 0xff) as u8);
            self.acc = 0;
            self.nbits = 0;
        }
        self.bytes
    }

    pub fn byte_len(&self) -> usize {
        self.bytes.len() + (self.nbits as usize).div_ceil(8)
    }

    /// Exact number of bits written so far, excluding final padding.
    pub fn bit_len(&self) -> u64 {
        self.bytes.len() as u64 * 8 + self.nbits as u64
    }
}

/// An MSB-first bit reader over a byte slice.
#[derive(Debug, Clone)]
pub struct BitReader<'a> {
    data: &'a [u8],
    /// Absolute bit position from the start of `data`.
    pos: usize,
}

impl<'a> BitReader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    #[inline]
    pub fn remaining_bits(&self) -> usize {
        self.data.len() * 8 - self.pos
    }

    #[inline]
    pub fn consumed_bits(&self) -> usize {
        self.pos
    }

    #[inline]
    pub fn read_bits(&mut self, n: u32) -> Result<u64> {
        if n == 0 {
            return Ok(0);
        }
        if n > 64 {
            return Err(Error::invalid_ocaf("bit read wider than 64 bits"));
        }
        if self.remaining_bits() < n as usize {
            return Err(Error::invalid_ocaf("truncated bitstream"));
        }
        let mut result = 0u64;
        let mut left = n;
        while left > 0 {
            let byte_index = self.pos / 8;
            let bit_offset = self.pos % 8;
            let available = 8 - bit_offset;
            let take = left.min(available as u32);
            let shift = available as u32 - take;
            let mask = (1u16 << take) - 1;
            let chunk = ((self.data[byte_index] >> shift) as u16) & mask;
            result = (result << take) | chunk as u64;
            self.pos += take as usize;
            left -= take;
        }
        Ok(result)
    }

    /// Read a Rice unary prefix: the number of zero bits before the next one.
    #[inline]
    pub fn read_unary_zeros(&mut self, max: u64) -> Result<u64> {
        let mut count = 0u64;
        loop {
            if self.pos >= self.data.len() * 8 {
                return Err(Error::invalid_ocaf("truncated unary code"));
            }
            let byte = self.data[self.pos / 8];
            let offset = self.pos % 8;
            let available = 8 - offset;
            let low_mask = (1u16 << available) - 1;
            let remaining = (byte as u16) & low_mask;
            if remaining == 0 {
                count += available as u64;
                self.pos += available;
                if count > max {
                    return Err(Error::limit("Rice unary code exceeds payload"));
                }
                continue;
            }
            let zeros = remaining.leading_zeros() as usize - (16 - available);
            if zeros > 0 {
                count += zeros as u64;
                self.pos += zeros;
                if count > max {
                    return Err(Error::limit("Rice unary code exceeds payload"));
                }
            }
            // The next bit must be the terminating one.
            self.pos += 1;
            return Ok(count);
        }
    }

    /// Read a two's-complement signed value of exactly `bits` bits.
    #[inline]
    pub fn read_signed(&mut self, bits: u32) -> Result<i64> {
        debug_assert!((1..=64).contains(&bits));
        let raw = self.read_bits(bits)?;
        if bits == 64 {
            return Ok(raw as i64);
        }
        let sign = 1u64 << (bits - 1);
        if raw & sign != 0 {
            Ok((raw | !((1u64 << bits) - 1)) as i64)
        } else {
            Ok(raw as i64)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bits_roundtrip() {
        for &(value, width) in &[
            (0u64, 1u32),
            (1, 1),
            (2, 2),
            (0xdead_beef, 32),
            (u64::MAX, 64),
            (0x1234_5678_9abc_def0, 64),
            (5, 3),
        ] {
            let mut w = BitWriter::new();
            w.write_bits(value, width);
            let bytes = w.finish();
            let mut r = BitReader::new(&bytes);
            assert_eq!(r.read_bits(width).unwrap(), value);
        }
    }

    #[test]
    fn mixed_roundtrip() {
        let mut w = BitWriter::new();
        let mut expected = Vec::new();
        for i in 0..200u64 {
            let width = (i % 64) + 1;
            let value = i.wrapping_mul(0x9E37_79B9_7F4A_7C15);
            let masked = if width == 64 {
                value
            } else {
                value & ((1u64 << width) - 1)
            };
            w.write_bits(value, width as u32);
            expected.push((masked, width as u32));
        }
        w.write_unary_zeros(0);
        w.write_unary_zeros(3);
        w.write_unary_zeros(40);
        w.write_signed(-1234567, 33);
        let bytes = w.finish();
        let mut r = BitReader::new(&bytes);
        for (value, width) in expected {
            assert_eq!(r.read_bits(width).unwrap(), value);
        }
        assert_eq!(r.read_unary_zeros(100).unwrap(), 0);
        assert_eq!(r.read_unary_zeros(100).unwrap(), 3);
        assert_eq!(r.read_unary_zeros(100).unwrap(), 40);
        assert_eq!(r.read_signed(33).unwrap(), -1234567);
    }

    #[test]
    fn signed_roundtrip() {
        let values = [0i64, 1, -1, 127, -128, 32767, -32768, i64::MIN, i64::MAX];
        for bits in 1..=64 {
            let mut w = BitWriter::new();
            for &v in &values {
                let truncated = if bits == 64 {
                    v
                } else {
                    let mask = (1u64 << bits) - 1;
                    let raw = (v as u64) & mask;
                    ((raw << (64 - bits)) as i64) >> (64 - bits)
                };
                w.write_signed(truncated, bits);
            }
            let bytes = w.finish();
            let mut r = BitReader::new(&bytes);
            for &v in &values {
                let expected = if bits == 64 {
                    v
                } else {
                    let mask = (1u64 << bits) - 1;
                    let raw = (v as u64) & mask;
                    ((raw << (64 - bits)) as i64) >> (64 - bits)
                };
                assert_eq!(r.read_signed(bits).unwrap(), expected, "bits={bits}");
            }
        }
    }

    #[test]
    fn truncated_input_is_rejected() {
        let mut r = BitReader::new(&[0xff]);
        assert_eq!(r.read_bits(8).unwrap(), 0xff);
        assert!(r.read_bits(1).is_err());
    }
}
