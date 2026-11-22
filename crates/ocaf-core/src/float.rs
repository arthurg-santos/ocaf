//! Exact floating-point compression helpers.
//!
//! IEEE floats are first mapped to order-preserving integer keys, which keeps
//! every bit (including NaN payloads and signed zero). For blocks whose values
//! are all exactly representable as a fixed-point integer with a shared scale,
//! a much better predictor is available: the encoder stores that integer and
//! the decoder reconstructs the original float bits exactly. Blocks that do
//! not qualify fall back to the raw-key path.
//!
//! A single shared scale is a poor fit for a waveform that crosses zero: one
//! tiny sample forces a very large scale for the whole block. When the raw-key
//! path wins for a large block, the encoder therefore retries the block as a
//! sequence of smaller independent chunks and keeps the result only when it is
//! smaller. This is still exact and is especially effective for float tones,
//! where most chunks have a normal amplitude and only the zero-crossing chunks
//! need the raw representation.

use crate::bitio::{BitReader, BitWriter};
use crate::error::{Error, Result};
use crate::pcm::{
    float32_bits_to_key, float64_bits_to_key, key_to_float32_bits, key_to_float64_bits,
};
use crate::subframe::{self, SubframeConfig};

/// Extended subframe type used only for floating-point streams.
pub const SUBFRAME_FLOAT_FIXED: u8 = 4;
/// Extended subframe type grouping several independent float subframes.
pub const SUBFRAME_FLOAT_GROUP: u8 = 5;
/// Extended subframe type using a fixed-point base plus exact exceptions.
pub const SUBFRAME_FLOAT_EXCEPTIONS: u8 = 6;

const MAX_SCALE: u8 = 62;
/// Power-of-two chunk size used by the grouped float representation.
const GROUP_CHUNK_SHIFT: u8 = 9;
const GROUP_CHUNK_SIZE: usize = 1 << GROUP_CHUNK_SHIFT;
/// Do not pay the group header unless at least two chunks are present.
const MIN_GROUP_SAMPLES: usize = GROUP_CHUNK_SIZE * 2;

struct FixedPlan {
    integers: Vec<i64>,
    scale: u8,
    integer_bps: u8,
}

struct ExceptionPlan {
    integers: Vec<i64>,
    scale: u8,
    integer_bps: u8,
    positions: Vec<u16>,
    keys: Vec<i64>,
}

struct ExceptionCandidate {
    total_bits: u64,
    plan: ExceptionPlan,
    base_bits: u64,
    base: Vec<u8>,
}

/// Encode one floating-point channel block (given as ordered keys).
pub fn encode(
    writer: &mut BitWriter,
    keys: &[i64],
    bits: u32,
    config: &SubframeConfig,
) -> Result<()> {
    if !matches!(bits, 32 | 64) {
        return Err(Error::argument("float subframes require 32 or 64 bits"));
    }
    if keys.is_empty() {
        return Err(Error::argument("cannot encode an empty float subframe"));
    }

    let (single_bits, single_bytes, single_is_raw) = encode_single(keys, bits, config)?;
    let mut chosen_bits = single_bits;
    let mut chosen_bytes = single_bytes;

    // The raw-key path is deliberately LPC-free. When it wins for a whole
    // block, splitting into chunks gives the fixed-point path a second chance
    // on the chunks that do not contain a near-zero sample.
    if single_is_raw && keys.len() >= MIN_GROUP_SAMPLES {
        let (group_bits, group_bytes) = encode_group(keys, bits, config)?;
        if group_bits < chosen_bits {
            chosen_bits = group_bits;
            chosen_bytes = group_bytes;
        }
    }

    write_exact_bits(writer, &chosen_bytes, chosen_bits);
    Ok(())
}

/// Encode one self-contained float subframe. Returns the exact bit length, the
/// byte-padded payload, and whether the raw-key representation was chosen.
fn encode_single(keys: &[i64], bits: u32, config: &SubframeConfig) -> Result<(u64, Vec<u8>, bool)> {
    // The raw-key path never uses LPC: the ordered key is monotonic but not
    // linear in the audio waveform.
    let raw_config = SubframeConfig {
        allow_lpc: false,
        ..config.clone()
    };
    let mut raw_writer = BitWriter::with_capacity(keys.len() * 2);
    subframe::encode(&mut raw_writer, keys, 64, &raw_config)?;
    let mut best_bits = raw_writer.bit_len();
    let mut best_bytes = raw_writer.finish();

    if let Some(plan) = try_fixed_plan(keys, bits) {
        let fixed_config = SubframeConfig {
            allow_lpc: config.allow_lpc,
            ..config.clone()
        };
        let mut body_writer = BitWriter::with_capacity(keys.len() * 2);
        subframe::encode(
            &mut body_writer,
            &plan.integers,
            plan.integer_bps as u32,
            &fixed_config,
        )?;
        let body_bits = body_writer.bit_len();
        let body = body_writer.finish();
        // type + float width + scale + integer bps + exact body bit length
        let total_bits = 64 + body_bits;
        if total_bits < best_bits {
            let mut writer = BitWriter::with_capacity(body.len() + 16);
            writer.write_bits(SUBFRAME_FLOAT_FIXED as u64, 8);
            writer.write_bits(bits as u64, 8);
            writer.write_bits(plan.scale as u64, 8);
            writer.write_bits(plan.integer_bps as u64, 8);
            writer.write_bits(body_bits, 32);
            write_exact_bits(&mut writer, &body, body_bits);
            best_bits = writer.bit_len();
            best_bytes = writer.finish();
        }
    }

    if let Some((exception_bits, exception_bytes)) = encode_exception_candidate(keys, bits, config)?
    {
        if exception_bits < best_bits {
            best_bits = exception_bits;
            best_bytes = exception_bytes;
        }
    }

    let is_raw = match best_bytes.first() {
        Some(&kind) => kind != SUBFRAME_FLOAT_FIXED && kind != SUBFRAME_FLOAT_EXCEPTIONS,
        None => true,
    };
    Ok((best_bits, best_bytes, is_raw))
}

fn encode_group(keys: &[i64], bits: u32, config: &SubframeConfig) -> Result<(u64, Vec<u8>)> {
    let mut writer = BitWriter::with_capacity(keys.len() * 2);
    writer.write_bits(SUBFRAME_FLOAT_GROUP as u64, 8);
    writer.write_bits(bits as u64, 8);
    writer.write_bits(GROUP_CHUNK_SHIFT as u64, 8);
    for chunk in keys.chunks(GROUP_CHUNK_SIZE) {
        let (chunk_bits, chunk_bytes, _) = encode_single(chunk, bits, config)?;
        let chunk_bits = u32::try_from(chunk_bits)
            .map_err(|_| Error::limit("float chunk payload exceeds 4 GiB"))?;
        writer.write_bits(chunk_bits as u64, 32);
        write_exact_bits(&mut writer, &chunk_bytes, chunk_bits as u64);
    }
    Ok((writer.bit_len(), writer.finish()))
}

/// Try a fixed-point base plus a sparse list of exact exceptions.
///
/// This is the representation that handles zero crossings efficiently. Tiny
/// values are simply listed verbatim instead of forcing a huge shared scale;
/// special values (NaN, infinity, subnormals and negative zero) are also stored
/// as exceptions, so this path never changes a single bit.
fn encode_exception_candidate(
    keys: &[i64],
    bits: u32,
    config: &SubframeConfig,
) -> Result<Option<(u64, Vec<u8>)>> {
    let Some(layout) = FloatLayout::new(bits) else {
        return Ok(None);
    };
    if keys.len() > u16::MAX as usize {
        return Ok(None);
    }
    let mut max_exponent: Option<i32> = None;
    for &key in keys {
        let raw = layout.raw_from_key(key);
        if let Some(Some(exponent)) = layout.decode(raw) {
            max_exponent = Some(max_exponent.map_or(exponent, |current| current.max(exponent)));
        }
    }
    let Some(max_exponent) = max_exponent else {
        return Ok(None);
    };

    // The smallest useful scale is the one that represents the loudest sample
    // in the block without an exception. Values below it (mostly quiet samples
    // around zero crossings) are listed separately. Larger scales widen the
    // base integers and amplify the rounding noise of the source floats, so
    // only a few nearby candidates are worth measuring.
    let base_scale = (layout.fraction_bits as i32 - max_exponent).max(0);
    let mut scales = Vec::with_capacity(3);
    for offset in [0, 3, 6] {
        let scale = (base_scale + offset).clamp(0, MAX_SCALE as i32) as u8;
        if !scales.contains(&scale) {
            scales.push(scale);
        }
    }

    let mut best: Option<ExceptionCandidate> = None;
    for scale in scales {
        let Some(plan) = build_exception_plan(keys, scale, &layout) else {
            continue;
        };
        if plan.positions.is_empty() {
            continue;
        }
        let fixed_config = SubframeConfig {
            allow_lpc: config.allow_lpc,
            ..config.clone()
        };
        let mut base_writer = BitWriter::with_capacity(keys.len() * 2);
        subframe::encode(
            &mut base_writer,
            &plan.integers,
            plan.integer_bps as u32,
            &fixed_config,
        )?;
        let base_bits = base_writer.bit_len();
        let base = base_writer.finish();
        let total_bits =
            32 + 32 + base_bits + 32 + plan.positions.len() as u64 * (16 + bits as u64);
        if best
            .as_ref()
            .map_or(true, |current| total_bits < current.total_bits)
        {
            best = Some(ExceptionCandidate {
                total_bits,
                plan,
                base_bits,
                base,
            });
        }
    }

    let Some(candidate) = best else {
        return Ok(None);
    };
    let plan = candidate.plan;
    let mut writer = BitWriter::with_capacity(candidate.base.len() + plan.positions.len() * 4);
    writer.write_bits(SUBFRAME_FLOAT_EXCEPTIONS as u64, 8);
    writer.write_bits(bits as u64, 8);
    writer.write_bits(plan.scale as u64, 8);
    writer.write_bits(plan.integer_bps as u64, 8);
    writer.write_bits(candidate.base_bits, 32);
    write_exact_bits(&mut writer, &candidate.base, candidate.base_bits);
    writer.write_bits(plan.positions.len() as u64, 32);
    for (&position, &key) in plan.positions.iter().zip(&plan.keys) {
        writer.write_bits(position as u64, 16);
        writer.write_bits(key as u64, bits);
    }
    Ok(Some((writer.bit_len(), writer.finish())))
}

fn build_exception_plan(keys: &[i64], scale: u8, layout: &FloatLayout) -> Option<ExceptionPlan> {
    let mut integers = Vec::with_capacity(keys.len());
    let mut positions = Vec::new();
    let mut exception_keys = Vec::new();
    for (index, &key) in keys.iter().enumerate() {
        let raw = layout.raw_from_key(key);
        let integer = match layout.decode(raw) {
            Some(None) => Some(0),
            Some(Some(exponent)) => {
                let shift = exponent + scale as i32;
                if !(0..=63).contains(&shift) {
                    None
                } else {
                    let significand = layout.reduced_significand(raw)?;
                    let magnitude = (significand as u128) << shift as u32;
                    if layout.sign(raw) {
                        if magnitude > (1u128 << 63) {
                            None
                        } else if magnitude == 1u128 << 63 {
                            Some(i64::MIN)
                        } else {
                            Some(-(magnitude as i64))
                        }
                    } else if magnitude > i64::MAX as u128 {
                        None
                    } else {
                        Some(magnitude as i64)
                    }
                }
            }
            None => None,
        };
        match integer {
            Some(value) => integers.push(value),
            None => {
                integers.push(0);
                positions.push(index as u16);
                exception_keys.push(key);
            }
        }
    }
    let integer_bps = integers
        .iter()
        .map(|&value| signed_bits(value))
        .max()
        .unwrap_or(1);
    Some(ExceptionPlan {
        integers,
        scale,
        integer_bps,
        positions,
        keys: exception_keys,
    })
}

/// Append `bits` bits from a byte-padded buffer without carrying the padding
/// into the caller's bitstream.
fn write_exact_bits(writer: &mut BitWriter, bytes: &[u8], bits: u64) {
    let full = (bits / 8) as usize;
    writer.write_bytes(&bytes[..full]);
    let remainder = (bits % 8) as u32;
    if remainder > 0 {
        writer.write_bits((bytes[full] >> (8 - remainder)) as u64, remainder);
    }
}

/// Decode one floating-point channel block into ordered keys.
pub fn decode(
    reader: &mut BitReader<'_>,
    output: &mut Vec<i64>,
    block_size: usize,
    bits: u32,
) -> Result<()> {
    if !matches!(bits, 32 | 64) {
        return Err(Error::invalid_ocaf("invalid float sample width"));
    }
    let kind = reader.read_bits(8)? as u8;
    if kind == SUBFRAME_FLOAT_GROUP {
        decode_group(reader, output, block_size, bits)
    } else {
        decode_single(reader, output, block_size, bits, kind)
    }
}

fn decode_group(
    reader: &mut BitReader<'_>,
    output: &mut Vec<i64>,
    block_size: usize,
    bits: u32,
) -> Result<()> {
    let stored_bits = reader.read_bits(8)? as u32;
    if stored_bits != bits {
        return Err(Error::invalid_ocaf(
            "float group width does not match the stream",
        ));
    }
    let shift = reader.read_bits(8)? as u32;
    if !(1..=16).contains(&shift) {
        return Err(Error::invalid_ocaf("invalid float chunk size"));
    }
    let chunk_size = 1usize << shift;
    if chunk_size > block_size {
        return Err(Error::invalid_ocaf(
            "float chunk is larger than the channel block",
        ));
    }
    output.clear();
    output.reserve(block_size);
    let mut produced = 0usize;
    let mut chunk_output = Vec::new();
    while produced < block_size {
        let chunk_len = chunk_size.min(block_size - produced);
        let chunk_bits = reader.read_bits(32)? as usize;
        let start = reader.consumed_bits();
        let kind = reader.read_bits(8)? as u8;
        if kind == SUBFRAME_FLOAT_GROUP {
            return Err(Error::invalid_ocaf("nested float groups are not allowed"));
        }
        decode_single(reader, &mut chunk_output, chunk_len, bits, kind)?;
        let used = reader.consumed_bits() - start;
        if used != chunk_bits {
            return Err(Error::invalid_ocaf("float chunk payload length mismatch"));
        }
        if chunk_output.len() != chunk_len {
            return Err(Error::invalid_ocaf("decoded float chunk length mismatch"));
        }
        output.extend_from_slice(&chunk_output);
        produced += chunk_len;
    }
    Ok(())
}

fn decode_single(
    reader: &mut BitReader<'_>,
    output: &mut Vec<i64>,
    block_size: usize,
    bits: u32,
    kind: u8,
) -> Result<()> {
    if kind == SUBFRAME_FLOAT_EXCEPTIONS {
        return decode_exceptions(reader, output, block_size, bits);
    }
    if kind != SUBFRAME_FLOAT_FIXED {
        // Raw ordered-key subframe, encoded with a full 64-bit container.
        subframe::decode_kind(reader, output, block_size, 64, kind)?;
        return Ok(());
    }
    let stored_bits = reader.read_bits(8)? as u32;
    if stored_bits != bits {
        return Err(Error::invalid_ocaf(
            "float subframe width does not match the stream",
        ));
    }
    let scale = reader.read_bits(8)? as u8;
    if scale > MAX_SCALE {
        return Err(Error::invalid_ocaf("invalid float fixed-point scale"));
    }
    let integer_bps = reader.read_bits(8)? as u32;
    if !(1..=64).contains(&integer_bps) {
        return Err(Error::invalid_ocaf("invalid float integer width"));
    }
    let body_bits = reader.read_bits(32)? as usize;
    let body_start = reader.consumed_bits();
    let mut integers = Vec::new();
    subframe::decode(reader, &mut integers, block_size, integer_bps)?;
    let used = reader.consumed_bits() - body_start;
    if used != body_bits {
        return Err(Error::invalid_ocaf(
            "float fixed-point payload length mismatch",
        ));
    }
    output.clear();
    output.reserve(block_size);
    for &value in &integers {
        output.push(fixed_to_key(value, scale, bits)?);
    }
    Ok(())
}

fn decode_exceptions(
    reader: &mut BitReader<'_>,
    output: &mut Vec<i64>,
    block_size: usize,
    bits: u32,
) -> Result<()> {
    let stored_bits = reader.read_bits(8)? as u32;
    if stored_bits != bits {
        return Err(Error::invalid_ocaf(
            "float exception width does not match the stream",
        ));
    }
    let scale = reader.read_bits(8)? as u8;
    if scale > MAX_SCALE {
        return Err(Error::invalid_ocaf("invalid float fixed-point scale"));
    }
    let integer_bps = reader.read_bits(8)? as u32;
    if !(1..=64).contains(&integer_bps) {
        return Err(Error::invalid_ocaf("invalid float integer width"));
    }
    let base_bits = reader.read_bits(32)? as usize;
    let base_start = reader.consumed_bits();
    let mut integers = Vec::new();
    subframe::decode(reader, &mut integers, block_size, integer_bps)?;
    let used = reader.consumed_bits() - base_start;
    if used != base_bits {
        return Err(Error::invalid_ocaf(
            "float exception base payload length mismatch",
        ));
    }
    if integers.len() != block_size {
        return Err(Error::invalid_ocaf(
            "decoded float exception base length mismatch",
        ));
    }
    output.clear();
    output.reserve(block_size);
    for &value in &integers {
        output.push(fixed_to_key(value, scale, bits)?);
    }
    let count = reader.read_bits(32)? as usize;
    if count > block_size {
        return Err(Error::invalid_ocaf(
            "float exception count exceeds the block size",
        ));
    }
    let mut previous: Option<usize> = None;
    for _ in 0..count {
        let position = reader.read_bits(16)? as usize;
        if position >= block_size || previous.is_some_and(|last| position <= last) {
            return Err(Error::invalid_ocaf(
                "float exception positions are out of order",
            ));
        }
        let raw = reader.read_bits(bits)?;
        let key = if bits == 32 {
            raw as u32 as i64
        } else {
            raw as i64
        };
        output[position] = key;
        previous = Some(position);
    }
    Ok(())
}

fn try_fixed_plan(keys: &[i64], bits: u32) -> Option<FixedPlan> {
    let layout = FloatLayout::new(bits)?;
    let mut scale = 0u32;
    for &key in keys {
        let raw = layout.raw_from_key(key);
        let value = layout.decode(raw)?;
        if let Some(exponent) = value {
            let need = (-exponent).max(0) as u32;
            scale = scale.max(need);
            if scale > MAX_SCALE as u32 {
                return None;
            }
        }
    }

    let mut integers = Vec::with_capacity(keys.len());
    for &key in keys {
        let raw = layout.raw_from_key(key);
        let value = layout.decode(raw)?;
        let integer = match value {
            None => 0,
            Some(exponent) => {
                let shift = exponent + scale as i32;
                if !(0..=63).contains(&shift) {
                    return None;
                }
                let magnitude = (layout.reduced_significand(raw)? as u128) << shift as u32;
                if layout.sign(raw) {
                    if magnitude > (1u128 << 63) {
                        return None;
                    }
                    if magnitude == 1u128 << 63 {
                        i64::MIN
                    } else {
                        -(magnitude as i64)
                    }
                } else {
                    if magnitude > i64::MAX as u128 {
                        return None;
                    }
                    magnitude as i64
                }
            }
        };
        integers.push(integer);
    }

    let integer_bps = integers
        .iter()
        .map(|&value| signed_bits(value))
        .max()
        .unwrap_or(1);
    Some(FixedPlan {
        integers,
        scale: scale as u8,
        integer_bps,
    })
}

fn fixed_to_key(value: i64, scale: u8, bits: u32) -> Result<i64> {
    let layout =
        FloatLayout::new(bits).ok_or_else(|| Error::invalid_ocaf("invalid float sample width"))?;
    if value == 0 {
        return Ok(layout.key_from_raw(0));
    }
    let negative = value < 0;
    let magnitude = value.unsigned_abs();
    let top = 63 - magnitude.leading_zeros();
    let exponent = top as i32 - scale as i32;
    if exponent < layout.min_exponent() || exponent > layout.max_exponent() {
        return Err(Error::invalid_ocaf(
            "fixed-point value is outside the normal float range",
        ));
    }
    let shift = layout.fraction_bits as i32 - top as i32;
    let significand = if shift >= 0 {
        magnitude << shift as u32
    } else {
        let drop = (-shift) as u32;
        if drop >= 64 || magnitude & ((1u64 << drop) - 1) != 0 {
            return Err(Error::invalid_ocaf(
                "fixed-point value is not exactly representable",
            ));
        }
        magnitude >> drop
    };
    debug_assert!(significand >> layout.fraction_bits == 1);
    let exponent_field = (exponent + layout.bias) as u64;
    let fraction_mask = (1u64 << layout.fraction_bits) - 1;
    let fraction = significand & fraction_mask;
    let raw =
        ((negative as u64) << (bits - 1)) | (exponent_field << layout.fraction_bits) | fraction;
    Ok(layout.key_from_raw(raw))
}

fn signed_bits(value: i64) -> u8 {
    let bits = if value >= 0 {
        64 - value.leading_zeros()
    } else {
        64 - (!value).leading_zeros()
    };
    (bits + 1).min(64) as u8
}

struct FloatLayout {
    bits: u32,
    fraction_bits: u32,
    exponent_bits: u32,
    bias: i32,
}

impl FloatLayout {
    fn new(bits: u32) -> Option<Self> {
        match bits {
            32 => Some(Self {
                bits,
                fraction_bits: 23,
                exponent_bits: 8,
                bias: 127,
            }),
            64 => Some(Self {
                bits,
                fraction_bits: 52,
                exponent_bits: 11,
                bias: 1023,
            }),
            _ => None,
        }
    }

    fn sign(&self, raw: u64) -> bool {
        raw >> (self.bits - 1) != 0
    }

    fn exponent_field(&self, raw: u64) -> u64 {
        (raw >> self.fraction_bits) & ((1u64 << self.exponent_bits) - 1)
    }

    fn fraction(&self, raw: u64) -> u64 {
        raw & ((1u64 << self.fraction_bits) - 1)
    }

    fn min_exponent(&self) -> i32 {
        1 - self.bias
    }

    fn max_exponent(&self) -> i32 {
        self.bias
    }

    fn raw_from_key(&self, key: i64) -> u64 {
        if self.bits == 32 {
            key_to_float32_bits(key) as u64
        } else {
            key_to_float64_bits(key)
        }
    }

    fn key_from_raw(&self, raw: u64) -> i64 {
        if self.bits == 32 {
            float32_bits_to_key(raw as u32)
        } else {
            float64_bits_to_key(raw)
        }
    }

    /// Return `Some(effective_exponent)` for a finite normal value, `None` for
    /// positive zero, and reject subnormals, infinities, NaNs and negative
    /// zero (which the integer representation cannot preserve).
    fn decode(&self, raw: u64) -> Option<Option<i32>> {
        let exponent_field = self.exponent_field(raw);
        let fraction = self.fraction(raw);
        if exponent_field == 0 {
            if fraction == 0 && !self.sign(raw) {
                return Some(None);
            }
            return None;
        }
        if exponent_field == (1u64 << self.exponent_bits) - 1 {
            return None;
        }
        let significand = (1u64 << self.fraction_bits) | fraction;
        let trailing = significand.trailing_zeros() as i32;
        let exponent = exponent_field as i32 - self.bias - self.fraction_bits as i32 + trailing;
        Some(Some(exponent))
    }

    fn reduced_significand(&self, raw: u64) -> Option<u64> {
        let significand = (1u64 << self.fraction_bits) | self.fraction(raw);
        Some(significand >> significand.trailing_zeros())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(keys: &[i64], bits: u32) {
        let config = SubframeConfig::default();
        let mut writer = BitWriter::new();
        encode(&mut writer, keys, bits, &config).unwrap();
        let bytes = writer.finish();
        let mut reader = BitReader::new(&bytes);
        let mut decoded = Vec::new();
        decode(&mut reader, &mut decoded, keys.len(), bits).unwrap();
        assert_eq!(decoded, keys);
        assert!(reader.remaining_bits() < 8);
    }

    #[test]
    fn fixed_point_float32_roundtrip() {
        let keys: Vec<i64> = (0..4096)
            .map(|i| {
                let value = 0.1 * (i as f64 * 0.01).sin();
                float32_bits_to_key((value as f32).to_bits())
            })
            .collect();
        roundtrip(&keys, 32);
    }

    #[test]
    fn fixed_point_float64_roundtrip() {
        let keys: Vec<i64> = (0..4096)
            .map(|i| {
                let value = 0.1 * (i as f64 * 0.01).sin();
                float64_bits_to_key(value.to_bits())
            })
            .collect();
        roundtrip(&keys, 64);
    }

    #[test]
    fn zero_crossing_tone_uses_groups_and_roundtrips() {
        let keys: Vec<i64> = (0..4096)
            .map(|i| {
                let value = 0.7 * (std::f64::consts::TAU * 440.0 * i as f64 / 48_000.0).sin();
                float32_bits_to_key((value as f32).to_bits())
            })
            .collect();
        let mut writer = BitWriter::new();
        encode(&mut writer, &keys, 32, &SubframeConfig::default()).unwrap();
        let bytes = writer.finish();
        assert!(
            bytes[0] == SUBFRAME_FLOAT_GROUP || bytes[0] == SUBFRAME_FLOAT_EXCEPTIONS,
            "unexpected zero-crossing representation {}",
            bytes[0]
        );
        let mut reader = BitReader::new(&bytes);
        let mut decoded = Vec::new();
        decode(&mut reader, &mut decoded, keys.len(), 32).unwrap();
        assert_eq!(decoded, keys);
        assert!(reader.remaining_bits() < 8);
    }

    #[test]
    fn exception_representation_is_exact() {
        // A normal-amplitude tone with a few extremely quiet samples: the
        // shared scale would need more than 62 fractional bits, so those
        // samples become exceptions.
        let mut keys: Vec<i64> = (0..4096)
            .map(|i| {
                let value = 0.7 * (std::f64::consts::TAU * 440.0 * i as f64 / 48_000.0).sin();
                float32_bits_to_key((value as f32).to_bits())
            })
            .collect();
        keys[100] = float32_bits_to_key(1.0e-30f32.to_bits());
        keys[2000] = float32_bits_to_key((-0.0f32).to_bits());
        keys[3000] = float32_bits_to_key(f32::NAN.to_bits());

        let candidate = encode_exception_candidate(&keys, 32, &SubframeConfig::default())
            .unwrap()
            .expect("exception candidate should be available");
        assert_eq!(candidate.1[0], SUBFRAME_FLOAT_EXCEPTIONS);
        let mut reader = BitReader::new(&candidate.1);
        let mut decoded = Vec::new();
        decode(&mut reader, &mut decoded, keys.len(), 32).unwrap();
        assert_eq!(decoded, keys);
        assert!(reader.remaining_bits() < 8);
    }

    #[test]
    fn special_float_bits_roundtrip() {
        let values = [
            0.0f32,
            -0.0,
            f32::INFINITY,
            f32::NEG_INFINITY,
            f32::NAN,
            1.0,
            -1.0,
            1.0e-30,
        ];
        let keys: Vec<i64> = values
            .iter()
            .map(|&v| float32_bits_to_key(v.to_bits()))
            .collect();
        roundtrip(&keys, 32);
    }

    #[test]
    fn signed_zero_falls_back_without_changing_bits() {
        let keys = [
            float32_bits_to_key(0.0f32.to_bits()),
            float32_bits_to_key((-0.0f32).to_bits()),
            float32_bits_to_key(1.0f32.to_bits()),
        ];
        roundtrip(&keys, 32);
    }
}
