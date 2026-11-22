//! Golomb-Rice coding of prediction residuals.
//!
//! Residuals are mapped to unsigned values with the standard zig-zag transform
//! and then coded with a per-partition Rice parameter. Parameters 0..=63 are
//! supported, which covers the full 64-bit residual range used by float
//! streams.

use crate::bitio::{BitReader, BitWriter};
use crate::error::{Error, Result};

#[inline]
pub fn zigzag_encode(value: i64) -> u64 {
    ((value as u64) << 1) ^ ((value >> 63) as u64)
}

#[inline]
pub fn zigzag_decode(value: u64) -> i64 {
    ((value >> 1) as i64) ^ -((value & 1) as i64)
}

/// Number of bits needed for `values` with parameter `k` (excludes param byte).
pub fn rice_bit_cost(values: &[u64], k: u8) -> u64 {
    let k = k as u32;
    let mut cost: u128 = values.len() as u128 * (k as u128 + 1);
    for &value in values {
        cost += (value >> k) as u128;
        if cost > u64::MAX as u128 {
            return u64::MAX;
        }
    }
    cost as u64
}

/// Pick the Rice parameter with the smallest exact bit cost for one partition.
pub fn choose_parameter(values: &[u64]) -> (u8, u64) {
    if values.is_empty() {
        return (0, 0);
    }
    // Count how many values have each bit set. From that histogram the sum of
    // `value >> k` for every k follows by a suffix recurrence, so the true
    // optimum is found in O(n + 64) time. This matters for blocks that contain
    // rare extreme residuals: a parameter chosen only from the mean would make
    // their unary quotient enormous.
    let mut counts = [0u64; 64];
    for &value in values {
        let mut bits = value;
        while bits != 0 {
            let index = bits.trailing_zeros() as usize;
            counts[index] += 1;
            bits &= bits - 1;
        }
    }
    let n = values.len() as u128;
    let mut suffix = 0u128;
    let mut best_k = 0u8;
    let mut best_cost = u64::MAX;
    for k in (0..64u32).rev() {
        suffix = suffix * 2 + counts[k as usize] as u128;
        let cost = n * (k as u128 + 1) + suffix;
        if cost < best_cost as u128 {
            best_cost = cost.min(u64::MAX as u128) as u64;
            best_k = k as u8;
        }
    }
    (best_k, best_cost)
}

/// Cheap one-pass estimate used to rank predictor candidates.
///
/// The absolute value is not bit-exact, but the ordering is reliable enough to
/// select a small set of finalists whose cost is then computed exactly.
pub fn estimate_residual_bits(residuals: &[i64]) -> u64 {
    if residuals.is_empty() {
        return 0;
    }
    let n = residuals.len() as u128;
    let mut sum: u128 = 0;
    for &value in residuals {
        sum += zigzag_encode(value) as u128;
    }
    let mean = (sum / n) as u64;
    let k = if mean == 0 {
        0
    } else {
        63 - mean.leading_zeros()
    };
    let cost = n * (k as u128 + 1) + (sum >> k);
    cost.min(u64::MAX as u128) as u64
}

/// Write zig-zag-mapped residuals using Rice parameter `k`.
pub fn write_values(writer: &mut BitWriter, values: &[u64], k: u8) {
    let k = k as u32;
    let mask = if k == 0 { 0 } else { (1u64 << k) - 1 };
    for &value in values {
        let quotient = value >> k;
        writer.write_unary_zeros(quotient);
        if k > 0 {
            writer.write_bits(value & mask, k);
        }
    }
}

/// Read exactly `count` Rice-coded residuals.
pub fn read_values(
    reader: &mut BitReader<'_>,
    count: usize,
    k: u8,
    output: &mut Vec<i64>,
) -> Result<()> {
    output.clear();
    output.reserve(count);
    let k32 = k as u32;
    let remainder_mask = if k == 0 { 0 } else { (1u64 << k) - 1 };
    // A quotient cannot require more zero bits than remain in the payload.
    let max_unary = reader.remaining_bits() as u64;
    for _ in 0..count {
        let quotient = reader.read_unary_zeros(max_unary)?;
        let remainder = if k > 0 { reader.read_bits(k32)? } else { 0 };
        let value = quotient
            .checked_mul(1u64 << k32)
            .and_then(|base| base.checked_add(remainder & remainder_mask))
            .ok_or_else(|| Error::invalid_ocaf("Rice value overflows 64 bits"))?;
        output.push(zigzag_decode(value));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zigzag_roundtrip() {
        for &v in &[0i64, 1, -1, 2, -2, 127, -128, i64::MAX, i64::MIN] {
            assert_eq!(zigzag_decode(zigzag_encode(v)), v);
        }
        assert_eq!(zigzag_encode(-1), 1);
        assert_eq!(zigzag_encode(1), 2);
    }

    #[test]
    fn residuals_roundtrip() {
        let values: Vec<i64> = (0..10_000)
            .map(|i| ((i * 7919) % 2001) as i64 - 1000)
            .collect();
        for k in 0..=16u8 {
            let mapped: Vec<u64> = values.iter().map(|&v| zigzag_encode(v)).collect();
            let mut writer = BitWriter::new();
            write_values(&mut writer, &mapped, k);
            let bytes = writer.finish();
            let mut reader = BitReader::new(&bytes);
            let mut decoded = Vec::new();
            read_values(&mut reader, values.len(), k, &mut decoded).unwrap();
            assert_eq!(decoded, values, "k={k}");
        }
    }

    #[test]
    fn extreme_values_roundtrip_with_large_parameter() {
        let values = [0i64, 1, -1, i64::MAX, i64::MIN, 1 << 62, -(1 << 62), 42];
        let mapped: Vec<u64> = values.iter().map(|&v| zigzag_encode(v)).collect();
        for k in [47u8, 55, 60, 62, 63] {
            let mut writer = BitWriter::new();
            write_values(&mut writer, &mapped, k);
            let bytes = writer.finish();
            let mut reader = BitReader::new(&bytes);
            let mut decoded = Vec::new();
            read_values(&mut reader, values.len(), k, &mut decoded).unwrap();
            assert_eq!(decoded, values, "k={k}");
        }
    }

    #[test]
    fn parameter_choice() {
        let values = vec![0u64, 1, 1, 2, 3, 5, 8, 13, 21, 34];
        let (k, cost) = choose_parameter(&values);
        assert!(k < 10);
        assert_eq!(cost, rice_bit_cost(&values, k));
    }

    #[test]
    fn empty_partition() {
        assert_eq!(choose_parameter(&[]), (0, 0));
    }
}
