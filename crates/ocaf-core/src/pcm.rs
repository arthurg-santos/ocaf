//! Conversion between canonical WAV PCM bytes and OCAF's planar sample
//! representation.
//!
//! Integer samples are kept as sign-extended `i64` values. Floating-point
//! samples are transformed to "ordered integer keys" that preserve every bit,
//! including inf, NaN and signed zero, and are inverted exactly by the decoder.

use crate::error::{Error, Result};
use crate::format::{SampleFormat, StreamInfo};

/// Map raw float32 bits to a monotonic signed integer key.
#[inline]
pub fn float32_bits_to_key(bits: u32) -> i64 {
    let key = if bits & 0x8000_0000 != 0 {
        !bits
    } else {
        bits ^ 0x8000_0000
    };
    key as i64
}

#[inline]
pub fn key_to_float32_bits(key: i64) -> u32 {
    let key = key as u32;
    if key & 0x8000_0000 != 0 {
        key ^ 0x8000_0000
    } else {
        !key
    }
}

#[inline]
pub fn float64_bits_to_key(bits: u64) -> i64 {
    let key = if bits & 0x8000_0000_0000_0000 != 0 {
        !bits
    } else {
        bits ^ 0x8000_0000_0000_0000
    };
    key as i64
}

#[inline]
pub fn key_to_float64_bits(key: i64) -> u64 {
    let key = key as u64;
    if key & 0x8000_0000_0000_0000 != 0 {
        key ^ 0x8000_0000_0000_0000
    } else {
        !key
    }
}

/// Expand a little-endian integer PCM buffer into planar i64 channels.
pub fn planar_from_interleaved(
    info: &StreamInfo,
    bytes: &[u8],
    frames: usize,
    planar: &mut [Vec<i64>],
) -> Result<()> {
    let channels = info.channels as usize;
    if planar.len() != channels {
        return Err(Error::argument("planar buffer channel mismatch"));
    }
    let width = info.bytes_per_sample();
    let expected = frames
        .checked_mul(info.block_align())
        .ok_or_else(|| Error::limit("PCM block size overflow"))?;
    if bytes.len() != expected {
        return Err(Error::argument(format!(
            "PCM buffer has {} bytes, expected {expected}",
            bytes.len()
        )));
    }
    for (channel, output) in planar.iter_mut().enumerate() {
        output.clear();
        output.reserve(frames);
        for frame in 0..frames {
            let offset = frame * info.block_align() + channel * width;
            let slice = &bytes[offset..offset + width];
            let value = match (info.sample_format, info.bits_per_sample) {
                (SampleFormat::Int, 16) => i16::from_le_bytes([slice[0], slice[1]]) as i64,
                (SampleFormat::Int, 24) => {
                    let raw =
                        (slice[0] as u32) | ((slice[1] as u32) << 8) | ((slice[2] as u32) << 16);
                    ((raw << 8) as i32 >> 8) as i64
                }
                (SampleFormat::Int, 32) => {
                    i32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]]) as i64
                }
                (SampleFormat::Float, 32) => float32_bits_to_key(u32::from_le_bytes([
                    slice[0], slice[1], slice[2], slice[3],
                ])),
                (SampleFormat::Float, 64) => float64_bits_to_key(u64::from_le_bytes([
                    slice[0], slice[1], slice[2], slice[3], slice[4], slice[5], slice[6], slice[7],
                ])),
                _ => return Err(Error::unsupported("unsupported PCM layout")),
            };
            output.push(value);
        }
    }
    Ok(())
}

/// Serialize planar i64 channels to canonical interleaved little-endian PCM.
///
/// The output is exactly what a WAV `data` chunk contains for the same format.
pub fn interleaved_from_planar(
    info: &StreamInfo,
    planar: &[Vec<i64>],
    frames: usize,
    out: &mut Vec<u8>,
) -> Result<()> {
    let channels = info.channels as usize;
    if planar.len() != channels {
        return Err(Error::argument("planar buffer channel mismatch"));
    }
    for (ch, samples) in planar.iter().enumerate() {
        if samples.len() < frames {
            return Err(Error::argument(format!(
                "channel {ch} has {} samples, expected at least {frames}",
                samples.len()
            )));
        }
    }
    let width = info.bytes_per_sample();
    out.clear();
    out.reserve(frames * info.block_align());
    let (min, max) = if info.sample_format == SampleFormat::Int {
        let limit = 1i64 << (info.bits_per_sample - 1);
        (-limit, limit - 1)
    } else {
        (0, 0)
    };
    for frame in 0..frames {
        for (channel, samples) in planar.iter().enumerate() {
            let value = samples[frame];
            match (info.sample_format, width) {
                (SampleFormat::Int, 2) => {
                    if value < min || value > max {
                        return Err(Error::invalid_ocaf(format!(
                            "decoded sample {value} does not fit 16-bit PCM (channel {channel})"
                        )));
                    }
                    out.extend_from_slice(&(value as i16).to_le_bytes());
                }
                (SampleFormat::Int, 3) => {
                    if value < min || value > max {
                        return Err(Error::invalid_ocaf(format!(
                            "decoded sample {value} does not fit 24-bit PCM (channel {channel})"
                        )));
                    }
                    let raw = value as i32;
                    out.extend_from_slice(&raw.to_le_bytes()[..3]);
                }
                (SampleFormat::Int, 4) => {
                    if value < min || value > max {
                        return Err(Error::invalid_ocaf(format!(
                            "decoded sample {value} does not fit 32-bit PCM (channel {channel})"
                        )));
                    }
                    out.extend_from_slice(&(value as i32).to_le_bytes());
                }
                (SampleFormat::Float, 4) => {
                    out.extend_from_slice(&key_to_float32_bits(value).to_le_bytes());
                }
                (SampleFormat::Float, 8) => {
                    out.extend_from_slice(&key_to_float64_bits(value).to_le_bytes());
                }
                _ => return Err(Error::unsupported("unsupported PCM layout")),
            }
        }
    }
    Ok(())
}

/// Allocate empty planar channels for `frames` samples.
pub fn planar_channels(channels: usize, frames: usize) -> Vec<Vec<i64>> {
    (0..channels).map(|_| Vec::with_capacity(frames)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(format: SampleFormat, bits: u16, channels: u16) -> StreamInfo {
        StreamInfo {
            sample_rate: 48_000,
            channels,
            bits_per_sample: bits,
            sample_format: format,
            channel_mask: 0,
            total_samples: 0,
        }
    }

    #[test]
    fn integer_roundtrip_16() {
        let i = info(SampleFormat::Int, 16, 2);
        let bytes: Vec<u8> = (0..40u8).collect();
        let mut planar = planar_channels(2, 10);
        planar_from_interleaved(&i, &bytes, 10, &mut planar).unwrap();
        let mut out = Vec::new();
        interleaved_from_planar(&i, &planar, 10, &mut out).unwrap();
        assert_eq!(out, bytes);
    }

    #[test]
    fn integer_roundtrip_24() {
        let i = info(SampleFormat::Int, 24, 1);
        let mut planar = vec![vec![-8_388_608, -1, 0, 1, 8_388_607]];
        for _ in 0..1000 {
            let len = planar[0].len() as i64;
            planar[0].push((len * 7919) % 8_388_608);
        }
        let frames = planar[0].len();
        let mut bytes = Vec::new();
        interleaved_from_planar(&i, &planar, frames, &mut bytes).unwrap();
        let mut decoded = planar_channels(1, frames);
        planar_from_interleaved(&i, &bytes, frames, &mut decoded).unwrap();
        assert_eq!(decoded, planar);
    }

    #[test]
    fn integer_roundtrip_32_extremes() {
        let i = info(SampleFormat::Int, 32, 1);
        let planar = vec![vec![i32::MIN as i64, -1, 0, 1, i32::MAX as i64]];
        let mut bytes = Vec::new();
        interleaved_from_planar(&i, &planar, 5, &mut bytes).unwrap();
        let mut decoded = planar_channels(1, 5);
        planar_from_interleaved(&i, &bytes, 5, &mut decoded).unwrap();
        assert_eq!(decoded, planar);
    }

    #[test]
    fn float_bits_are_preserved_exactly() {
        let f32_bits = [
            0x0000_0000u32,
            0x8000_0000,
            0x7f80_0000,
            0xff80_0000,
            0x7fc0_0001,
            0x0000_0001,
            0x8000_0001,
            0x3f80_0000,
        ];
        for bits in f32_bits {
            assert_eq!(key_to_float32_bits(float32_bits_to_key(bits)), bits);
        }
        let f64_bits = [
            0x0000_0000_0000_0000u64,
            0x8000_0000_0000_0000,
            0x7ff0_0000_0000_0000,
            0xfff0_0000_0000_0000,
            0x7ff8_0000_0000_0001,
            0x0000_0000_0000_0001,
            0x8000_0000_0000_0001,
            0x3ff0_0000_0000_0000,
        ];
        for bits in f64_bits {
            assert_eq!(key_to_float64_bits(float64_bits_to_key(bits)), bits);
        }
    }

    #[test]
    fn float_roundtrip() {
        for &(format, bits) in &[(SampleFormat::Float, 32u16), (SampleFormat::Float, 64)] {
            let i = info(format, bits, 2);
            let frames = 64;
            let mut planar = planar_channels(2, frames);
            for frame in 0..frames {
                for (ch, channel) in planar.iter_mut().enumerate().take(2) {
                    let value = if bits == 32 {
                        float32_bits_to_key(0x3f00_0000 + (frame * 37 + ch) as u32)
                    } else {
                        float64_bits_to_key(0x3ff0_0000_0000_0000 + (frame * 37 + ch) as u64)
                    };
                    channel.push(value);
                }
            }
            let mut bytes = Vec::new();
            interleaved_from_planar(&i, &planar, frames, &mut bytes).unwrap();
            let mut decoded = planar_channels(2, frames);
            planar_from_interleaved(&i, &bytes, frames, &mut decoded).unwrap();
            assert_eq!(decoded, planar);
        }
    }
}
