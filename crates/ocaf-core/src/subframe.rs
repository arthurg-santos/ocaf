//! Adaptive subframe encoder and decoder.
//!
//! For every channel block the encoder evaluates constant, verbatim, fixed
//! polynomial and quantized LPC predictors, then chooses the candidate with the
//! smallest exact bit cost. Residuals are partitioned and each partition gets
//! its own Golomb-Rice parameter. All choices are stored explicitly in the
//! bitstream, so decoding never has to guess.

use crate::bitio::{BitReader, BitWriter};
use crate::error::{Error, Result};
use crate::lpc;
use crate::rice;

pub const SUBFRAME_CONSTANT: u8 = 0;
pub const SUBFRAME_FIXED: u8 = 1;
pub const SUBFRAME_LPC: u8 = 2;
pub const SUBFRAME_VERBATIM: u8 = 3;

const MAX_LPC_ORDER: usize = 32;

/// Coefficient precision grows with the sample bit depth. With 15 fractional
/// bits a 32-bit block loses most of the predictor's accuracy, because the
/// coefficient error is multiplied by a sample magnitude near 2^31.
fn lpc_precision(bps: u32) -> u8 {
    if bps <= 20 {
        15
    } else if bps <= 25 {
        21
    } else {
        28
    }
}

#[derive(Clone, Debug)]
pub struct SubframeConfig {
    pub max_lpc_order: u8,
    pub max_partition_order: u8,
    /// Disable LPC for data whose bit patterns do not behave like PCM.
    pub allow_lpc: bool,
}

impl Default for SubframeConfig {
    fn default() -> Self {
        Self {
            max_lpc_order: 16,
            max_partition_order: 6,
            allow_lpc: true,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct SubframeStats {
    pub kind: u8,
    pub order: u8,
    pub partition_order: u8,
    pub bits: u64,
}

#[derive(Clone, Debug)]
enum Plan {
    Verbatim,
    Predict {
        lpc: bool,
        order: u8,
        precision: u8,
        coefficients: Vec<i32>,
        residual: Vec<i64>,
        partition_order: u8,
        parameters: Vec<u8>,
    },
}

#[derive(Clone, Debug)]
struct PartitionPlan {
    order: u8,
    parameters: Vec<u8>,
    bits: u64,
}

#[derive(Clone, Debug)]
struct Candidate {
    lpc: bool,
    order: u8,
    precision: u8,
    coefficients: Vec<i32>,
    residual: Vec<i64>,
    quick_cost: u64,
}

/// Number of predictor candidates that get the expensive partitioned cost
/// analysis. The rest are ranked with a single-partition estimate first.
const FINALIST_COUNT: usize = 3;

/// Encode one channel block into `writer` and return statistics.
pub fn encode(
    writer: &mut BitWriter,
    samples: &[i64],
    bps: u32,
    config: &SubframeConfig,
) -> Result<SubframeStats> {
    if samples.is_empty() {
        return Err(Error::argument("cannot encode an empty subframe"));
    }
    if !(1..=64).contains(&bps) {
        return Err(Error::argument("subframe bit depth must be 1..=64"));
    }
    let n = samples.len();

    // Remove trailing zero bits that every sample shares. This is common for
    // integer files stored with a higher nominal depth than their real
    // precision, and it shrinks both residuals and verbatim samples without
    // losing information.
    let wasted = wasted_bits(samples, bps);
    let shifted: Vec<i64>;
    let samples: &[i64] = if wasted > 0 {
        shifted = samples.iter().map(|&s| s >> wasted).collect();
        &shifted
    } else {
        samples
    };
    let bps = bps - wasted as u32;

    // Constant blocks are always cheaper than any predicted representation.
    if samples.iter().all(|&s| s == samples[0]) {
        writer.write_bits(SUBFRAME_CONSTANT as u64, 8);
        writer.write_bits(wasted as u64, 8);
        writer.write_signed(samples[0], bps);
        return Ok(SubframeStats {
            kind: SUBFRAME_CONSTANT,
            bits: 16 + bps as u64,
            ..Default::default()
        });
    }

    let mut best_cost = 16u64 + n as u64 * bps as u64;
    let mut best_plan = Plan::Verbatim;

    let max_po = config
        .max_partition_order
        .min(n.trailing_zeros().min(8) as u8);
    let mut finalists: Vec<Candidate> = Vec::with_capacity(FINALIST_COUNT);

    // Fixed polynomial predictors, including order zero (Rice-only coding).
    for order in 0..=4u8 {
        let coeffs = lpc::fixed_coefficients(order).unwrap();
        if coeffs.len() >= n {
            continue;
        }
        let mut residual = Vec::new();
        lpc::residuals(samples, coeffs, 0, &mut residual);
        let quick_cost = quick_candidate_cost(&residual, coeffs.len(), bps, false);
        consider(
            &mut finalists,
            Candidate {
                lpc: false,
                order,
                precision: 0,
                coefficients: Vec::new(),
                residual,
                quick_cost,
            },
        );
    }

    // LPC candidates.
    if config.allow_lpc && config.max_lpc_order > 0 && n >= 8 {
        let max_order = (config.max_lpc_order as usize)
            .min(MAX_LPC_ORDER)
            .min(n.saturating_sub(1));
        if max_order > 0 {
            if let Some(autocorr) = lpc::autocorrelation(samples, max_order) {
                let precision = lpc_precision(bps);
                for order in 1..=max_order {
                    let Some(coefficients) = lpc::levinson_durbin(&autocorr, order) else {
                        continue;
                    };
                    let quantized = lpc::quantize_coefficients(&coefficients, precision);
                    let wide = lpc::widen(&quantized);
                    let mut residual = Vec::new();
                    lpc::residuals(samples, &wide, precision, &mut residual);
                    let quick_cost = quick_candidate_cost(&residual, order, bps, true);
                    consider(
                        &mut finalists,
                        Candidate {
                            lpc: true,
                            order: order as u8,
                            precision,
                            coefficients: quantized,
                            residual,
                            quick_cost,
                        },
                    );
                }
            }
        }
    }

    for candidate in finalists {
        let partition = choose_partition(&candidate.residual, n, candidate.order as usize, max_po);
        let cost =
            predictor_header_cost(candidate.order as usize, bps, candidate.lpc) + partition.bits;
        if cost < best_cost {
            best_cost = cost;
            best_plan = Plan::Predict {
                lpc: candidate.lpc,
                order: candidate.order,
                precision: candidate.precision,
                coefficients: candidate.coefficients,
                residual: candidate.residual,
                partition_order: partition.order,
                parameters: partition.parameters,
            };
        }
    }

    let stats = SubframeStats {
        kind: match &best_plan {
            Plan::Verbatim => SUBFRAME_VERBATIM,
            Plan::Predict { lpc: false, .. } => SUBFRAME_FIXED,
            Plan::Predict { lpc: true, .. } => SUBFRAME_LPC,
        },
        order: match &best_plan {
            Plan::Predict { order, .. } => *order,
            _ => 0,
        },
        partition_order: match &best_plan {
            Plan::Predict {
                partition_order, ..
            } => *partition_order,
            _ => 0,
        },
        bits: best_cost,
    };
    write_plan(writer, samples, bps, wasted, &best_plan)?;
    Ok(stats)
}

/// Number of low zero bits shared by all samples, clamped so at least one
/// significant bit remains.
fn wasted_bits(samples: &[i64], bps: u32) -> u8 {
    let mut wasted = 63u32;
    for &sample in samples {
        if sample != 0 {
            wasted = wasted.min(sample.trailing_zeros());
        }
    }
    wasted.min(bps - 1) as u8
}

fn predictor_header_cost(order: usize, bps: u32, lpc: bool) -> u64 {
    let mut cost = 8 + 8 + 8 + order as u64 * bps as u64;
    if lpc {
        cost += 8 + order as u64 * 32;
    }
    cost
}

fn quick_candidate_cost(residual: &[i64], order: usize, bps: u32, lpc: bool) -> u64 {
    let rice_bits = rice::estimate_residual_bits(residual);
    predictor_header_cost(order, bps, lpc) + 8 + 8 + rice_bits
}

fn consider(finalists: &mut Vec<Candidate>, candidate: Candidate) {
    let position = finalists
        .iter()
        .position(|existing| candidate.quick_cost < existing.quick_cost)
        .unwrap_or(finalists.len());
    if position < FINALIST_COUNT {
        finalists.insert(position, candidate);
        if finalists.len() > FINALIST_COUNT {
            finalists.pop();
        }
    } else if finalists.len() < FINALIST_COUNT {
        finalists.push(candidate);
    }
}

fn choose_partition(
    residual: &[i64],
    block_size: usize,
    predictor_order: usize,
    max_order: u8,
) -> PartitionPlan {
    debug_assert_eq!(residual.len(), block_size - predictor_order);
    let mut best = PartitionPlan {
        order: 0,
        parameters: Vec::new(),
        bits: u64::MAX,
    };
    let mut mapped = Vec::new();
    for po in 0..=max_order {
        let parts = 1usize << po;
        if parts > block_size || block_size % parts != 0 {
            continue;
        }
        let part_size = block_size / parts;
        let mut parameters = Vec::with_capacity(parts);
        let mut cost = 8u64; // partition order byte
        for part in 0..parts {
            let start_sample = part * part_size;
            let end_sample = start_sample + part_size;
            let start = start_sample
                .saturating_sub(predictor_order)
                .min(residual.len());
            let end = end_sample
                .saturating_sub(predictor_order)
                .min(residual.len());
            mapped.clear();
            mapped.extend(residual[start..end].iter().map(|&v| rice::zigzag_encode(v)));
            let (k, bits) = rice::choose_parameter(&mapped);
            parameters.push(k);
            cost += bits + 8;
        }
        if cost < best.bits {
            best = PartitionPlan {
                order: po,
                parameters,
                bits: cost,
            };
        }
    }
    if best.bits == u64::MAX {
        // Always reachable: po=0, block_size % 1 == 0.
        best.bits = 0;
    }
    best
}

fn write_plan(
    writer: &mut BitWriter,
    samples: &[i64],
    bps: u32,
    wasted: u8,
    plan: &Plan,
) -> Result<()> {
    match plan {
        Plan::Verbatim => {
            writer.write_bits(SUBFRAME_VERBATIM as u64, 8);
            writer.write_bits(wasted as u64, 8);
            for &sample in samples {
                writer.write_signed(sample, bps);
            }
        }
        Plan::Predict {
            lpc: is_lpc,
            order,
            precision,
            coefficients,
            residual,
            partition_order,
            parameters,
        } => {
            writer.write_bits(if *is_lpc { 2 } else { 1 }, 8);
            writer.write_bits(wasted as u64, 8);
            writer.write_bits(*order as u64, 8);
            if *is_lpc {
                writer.write_bits(*precision as u64, 8);
                for &coefficient in coefficients {
                    writer.write_signed(coefficient as i64, 32);
                }
            }
            for &sample in &samples[..*order as usize] {
                writer.write_signed(sample, bps);
            }
            writer.write_bits(*partition_order as u64, 8);
            for &k in parameters {
                writer.write_bits(k as u64, 8);
            }
            let parts = 1usize << *partition_order;
            let part_size = samples.len() / parts;
            let mut mapped = Vec::new();
            for (part, &k) in parameters.iter().enumerate().take(parts) {
                let start_sample = part * part_size;
                let end_sample = start_sample + part_size;
                let start = start_sample
                    .saturating_sub(*order as usize)
                    .min(residual.len());
                let end = end_sample
                    .saturating_sub(*order as usize)
                    .min(residual.len());
                mapped.clear();
                mapped.extend(residual[start..end].iter().map(|&v| rice::zigzag_encode(v)));
                rice::write_values(writer, &mapped, k);
            }
        }
    }
    Ok(())
}

/// Decode one channel block.
pub fn decode(
    reader: &mut BitReader<'_>,
    output: &mut Vec<i64>,
    block_size: usize,
    bps: u32,
) -> Result<SubframeStats> {
    let kind = reader.read_bits(8)? as u8;
    decode_kind(reader, output, block_size, bps, kind)
}

/// Decode a subframe whose type byte has already been consumed. The float
/// codec uses this to sniff its own extended subframe types.
pub(crate) fn decode_kind(
    reader: &mut BitReader<'_>,
    output: &mut Vec<i64>,
    block_size: usize,
    bps: u32,
    kind: u8,
) -> Result<SubframeStats> {
    if block_size == 0 {
        return Err(Error::invalid_ocaf("zero-sized audio block"));
    }
    if !(1..=64).contains(&bps) {
        return Err(Error::invalid_ocaf("invalid encoded sample width"));
    }
    let wasted = reader.read_bits(8)? as u8;
    if wasted as u32 >= bps {
        return Err(Error::invalid_ocaf("invalid wasted-bits count"));
    }
    let stored_bps = bps - wasted as u32;
    match kind {
        SUBFRAME_CONSTANT => {
            let mut value = reader.read_signed(stored_bps)?;
            value = value.wrapping_shl(wasted as u32);
            validate_value(value, bps)?;
            output.clear();
            output.resize(block_size, value);
            Ok(SubframeStats {
                kind,
                ..Default::default()
            })
        }
        SUBFRAME_VERBATIM => {
            output.clear();
            output.reserve(block_size);
            for _ in 0..block_size {
                let value = reader.read_signed(stored_bps)?;
                let value = value.wrapping_shl(wasted as u32);
                validate_value(value, bps)?;
                output.push(value);
            }
            Ok(SubframeStats {
                kind,
                ..Default::default()
            })
        }
        SUBFRAME_FIXED | SUBFRAME_LPC => {
            let is_lpc = kind == SUBFRAME_LPC;
            let order = reader.read_bits(8)? as usize;
            let (precision, coeffs_i32) = if is_lpc {
                if !(1..=MAX_LPC_ORDER).contains(&order) {
                    return Err(Error::invalid_ocaf("invalid LPC order"));
                }
                let precision = reader.read_bits(8)? as u8;
                if !(1..=30).contains(&precision) {
                    return Err(Error::invalid_ocaf("invalid LPC coefficient precision"));
                }
                let coefficients = (0..order)
                    .map(|_| reader.read_signed(32).map(|v| v as i32))
                    .collect::<Result<Vec<_>>>()?;
                (precision, coefficients)
            } else {
                if order > 4 {
                    return Err(Error::invalid_ocaf("invalid fixed predictor order"));
                }
                (0, Vec::new())
            };
            if order >= block_size {
                return Err(Error::invalid_ocaf(
                    "predictor order is not smaller than block size",
                ));
            }
            let coeffs: Vec<i64> = if is_lpc {
                lpc::widen(&coeffs_i32)
            } else {
                lpc::fixed_coefficients(order as u8)
                    .ok_or_else(|| Error::invalid_ocaf("invalid fixed predictor"))?
                    .to_vec()
            };
            let mut warmup = Vec::with_capacity(order);
            for _ in 0..order {
                warmup.push(reader.read_signed(stored_bps)?);
            }
            let partition_order = reader.read_bits(8)? as u8;
            if partition_order > 15 {
                return Err(Error::invalid_ocaf("invalid partition order"));
            }
            let parts = 1usize << partition_order;
            if parts > block_size || block_size % parts != 0 {
                return Err(Error::invalid_ocaf(
                    "partition count does not divide the block size",
                ));
            }
            let parameters: Vec<u8> = (0..parts)
                .map(|_| {
                    let k = reader.read_bits(8)? as u8;
                    if k > 63 {
                        return Err(Error::invalid_ocaf("invalid Rice parameter"));
                    }
                    Ok(k)
                })
                .collect::<Result<Vec<_>>>()?;
            let residual_count = block_size - order;
            let mut residual = Vec::with_capacity(residual_count);
            let mut scratch = Vec::new();
            let part_size = block_size / parts;
            for (part, &k) in parameters.iter().enumerate() {
                let start_sample = part * part_size;
                let end_sample = start_sample + part_size;
                let start = start_sample.saturating_sub(order).min(residual_count);
                let end = end_sample.saturating_sub(order).min(residual_count);
                let count = end - start;
                rice::read_values(reader, count, k, &mut scratch)?;
                residual.extend_from_slice(&scratch);
            }
            if residual.len() != residual_count {
                return Err(Error::invalid_ocaf("subframe residual count mismatch"));
            }
            lpc::restore(&warmup, &residual, &coeffs, precision, block_size, output)?;
            if wasted > 0 {
                for value in output.iter_mut() {
                    *value = value.wrapping_shl(wasted as u32);
                }
            }
            for &value in output.iter() {
                validate_value(value, bps)?;
            }
            Ok(SubframeStats {
                kind,
                order: order as u8,
                partition_order,
                bits: 0,
            })
        }
        _ => Err(Error::invalid_ocaf(format!("unknown subframe type {kind}"))),
    }
}

fn validate_value(value: i64, bps: u32) -> Result<()> {
    if bps >= 64 {
        return Ok(());
    }
    let limit = 1i64 << (bps - 1);
    if value < -limit || value >= limit {
        return Err(Error::invalid_ocaf(
            "decoded sample does not fit its encoded bit depth",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(samples: &[i64], bps: u32, config: &SubframeConfig) -> SubframeStats {
        let mut writer = BitWriter::new();
        let stats = encode(&mut writer, samples, bps, config).unwrap();
        let bytes = writer.finish();
        let mut reader = BitReader::new(&bytes);
        let mut decoded = Vec::new();
        decode(&mut reader, &mut decoded, samples.len(), bps).unwrap();
        assert_eq!(decoded, samples);
        assert!(reader.remaining_bits() < 8);
        stats
    }

    #[test]
    fn constant_silence_is_constant() {
        let samples = vec![0i64; 4096];
        let stats = roundtrip(&samples, 16, &SubframeConfig::default());
        assert_eq!(stats.kind, SUBFRAME_CONSTANT);
    }

    #[test]
    fn random_noise_uses_verbatim_or_rice() {
        let mut state = 123456789u64;
        let samples: Vec<i64> = (0..4096)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                (state as i16) as i64
            })
            .collect();
        let stats = roundtrip(&samples, 16, &SubframeConfig::default());
        assert!(stats.kind == SUBFRAME_VERBATIM || stats.bits > 0);
    }

    #[test]
    fn tone_uses_predictor() {
        let samples: Vec<i64> = (0..4096)
            .map(|i| {
                let t = i as f64 * 0.03;
                (12000.0 * t.sin() + 3000.0 * (t * 2.7).sin()) as i64
            })
            .collect();
        let stats = roundtrip(&samples, 16, &SubframeConfig::default());
        assert!(stats.kind == SUBFRAME_LPC || stats.kind == SUBFRAME_FIXED);
    }

    #[test]
    fn short_blocks_roundtrip() {
        for n in 1..=40 {
            let samples: Vec<i64> = (0..n as i64).map(|i| (i * 37) % 101 - 50).collect();
            roundtrip(&samples, 16, &SubframeConfig::default());
        }
    }

    #[test]
    fn extreme_16_bit_values_roundtrip() {
        let samples = vec![i16::MIN as i64, i16::MAX as i64, -1, 0, 1];
        roundtrip(&samples, 16, &SubframeConfig::default());
        roundtrip(&samples, 17, &SubframeConfig::default());
    }

    #[test]
    fn float_keys_roundtrip() {
        let mut state = 0x1234_5678_9abc_def0u64;
        let samples: Vec<i64> = (0..1024)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                state as i64
            })
            .collect();
        let config = SubframeConfig {
            allow_lpc: false,
            ..Default::default()
        };
        roundtrip(&samples, 64, &config);
    }
}
