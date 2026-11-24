//! OCAF stream encoder.
//!
//! The encoder reads uncompressed WAV, splits it into independent frames and
//! chooses per-frame stereo decorrelation, per-channel predictors and Rice
//! partition parameters. It never quantizes audio samples: only predictor
//! coefficients are quantized, and their exact values are stored in the frame.

use crate::bitio::BitWriter;
use crate::crc32::{crc32, Crc32};
use crate::error::{Error, Result};
use crate::format::{
    ChannelAssignment, FrameHeader, SampleFormat, StreamHeader, StreamInfo, FRAME_HEADER_SIZE,
};
use crate::lpc;
use crate::metadata::{Metadata, ENCODER};
use crate::pcm;
use crate::rice;
use crate::subframe::{self, SubframeConfig};
use crate::wav::WavReader;
use std::io::{Read, Seek, SeekFrom, Write};

/// Stereo decorrelation policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StereoMode {
    /// Try all assignments with a fast cost estimate.
    Auto,
    Independent,
    LeftSide,
    RightSide,
    MidSide,
}

impl StereoMode {
    pub fn parse(value: &str) -> Result<Self> {
        match value.to_ascii_lowercase().as_str() {
            "auto" => Ok(StereoMode::Auto),
            "independent" | "none" => Ok(StereoMode::Independent),
            "left-side" | "left_side" | "left" => Ok(StereoMode::LeftSide),
            "right-side" | "right_side" | "right" => Ok(StereoMode::RightSide),
            "mid-side" | "mid_side" | "mid" => Ok(StereoMode::MidSide),
            other => Err(Error::argument(format!(
                "unknown stereo mode {other:?} (use auto, independent, left-side, right-side or mid-side)"
            ))),
        }
    }
}

/// Encoder tuning parameters.
#[derive(Clone, Debug)]
pub struct EncodeOptions {
    /// Samples per frame. 16384 gives the predictor longer runs to adapt
    /// while keeping encoder latency below a third of a second at 48 kHz.
    pub block_size: u16,
    /// Maximum LPC order (0 disables LPC analysis).
    pub max_lpc_order: u8,
    /// Maximum Rice partition order (0..=8).
    pub max_partition_order: u8,
    pub stereo_mode: StereoMode,
    /// Metadata to add or override in the output.
    pub metadata: Metadata,
}

impl Default for EncodeOptions {
    fn default() -> Self {
        Self {
            block_size: 16384,
            max_lpc_order: 12,
            max_partition_order: 6,
            stereo_mode: StereoMode::Auto,
            metadata: Metadata::new(),
        }
    }
}

impl EncodeOptions {
    pub fn validate(&self) -> Result<()> {
        if self.block_size == 0 {
            return Err(Error::argument("block size must be at least 1"));
        }
        if self.max_lpc_order > 32 {
            return Err(Error::argument("maximum LPC order is 32"));
        }
        if self.max_partition_order > 8 {
            return Err(Error::argument("maximum partition order is 8"));
        }
        Ok(())
    }
}

/// Result of an encode operation.
#[derive(Clone, Debug)]
pub struct EncodeSummary {
    pub info: StreamInfo,
    pub pcm_bytes: u64,
    pub ocaf_bytes: u64,
    pub frame_count: u64,
    pub min_block_size: u16,
    pub max_block_size: u16,
}

impl EncodeSummary {
    pub fn compression_ratio(&self) -> f64 {
        if self.ocaf_bytes == 0 {
            0.0
        } else {
            self.pcm_bytes as f64 / self.ocaf_bytes as f64
        }
    }
}

/// Encode a WAV stream into an OCAF stream.
pub fn encode_wav<R: Read + Seek, W: Write + Seek>(
    input: R,
    output: W,
    options: &EncodeOptions,
) -> Result<EncodeSummary> {
    let mut wav = WavReader::open(input)?;
    let wav_info = wav.info().clone();
    encode_stream(
        &mut wav,
        wav_info.spec,
        Some(wav_info.spec.total_samples),
        &wav_info.metadata,
        output,
        options,
    )
}

/// Encode raw canonical interleaved PCM into an OCAF stream.
///
/// `info` describes the sample rate, channel layout, bit depth and sample
/// format. Its `total_samples` field is ignored: the input ends at EOF and
/// the final sample count is written into the OCAF header. The input must be
/// little-endian interleaved PCM in WAV byte order and must contain complete
/// sample frames.
pub fn encode_pcm<R: Read, W: Write + Seek>(
    input: R,
    info: StreamInfo,
    output: W,
    options: &EncodeOptions,
) -> Result<EncodeSummary> {
    info.validate()?;
    let block_align = info.block_align();
    if block_align == 0 {
        return Err(Error::argument("PCM layout has a zero block alignment"));
    }
    let mut source = RawPcmSource {
        reader: input,
        block_align,
    };
    let spec = StreamInfo {
        total_samples: 0,
        ..info
    };
    encode_stream(&mut source, spec, None, &Metadata::new(), output, options)
}

fn encode_stream<S: FrameSource, W: Write + Seek>(
    source: &mut S,
    mut info: StreamInfo,
    expected_samples: Option<u64>,
    base_metadata: &Metadata,
    mut output: W,
    options: &EncodeOptions,
) -> Result<EncodeSummary> {
    options.validate()?;

    let mut metadata = base_metadata.clone();
    for entry in options.metadata.entries() {
        metadata.set(entry.key.clone(), entry.value.clone())?;
    }
    metadata.set(ENCODER, format!("ocaf {}", env!("CARGO_PKG_VERSION")))?;
    let metadata_bytes = metadata.to_bytes()?;
    if metadata_bytes.len() as u64 > crate::format::MAX_METADATA_BYTES {
        return Err(Error::argument("metadata section is too large"));
    }

    let block_size = options.block_size;
    // The sample count is only known after the source reaches EOF. The
    // header is patched with the final counts and checksums at the end.
    info.total_samples = 0;
    let mut header = StreamHeader::new(info, metadata_bytes.len() as u64);
    output.seek(SeekFrom::Start(0))?;
    output.write_all(&header.to_bytes())?;
    output.write_all(&metadata_bytes)?;

    let channels = info.channels as usize;
    let mut pcm_bytes = Vec::new();
    let mut planar = pcm::planar_channels(channels, block_size as usize);
    let mut encoded_channels = pcm::planar_channels(channels, block_size as usize);
    let mut total_crc = Crc32::new();
    let mut next_sample = 0u64;
    let mut frame_index = 0u64;
    let mut min_block = u16::MAX;
    let mut max_block = 0u16;
    let mut pcm_total = 0u64;

    loop {
        let read = source.read_frames(block_size as usize, &mut pcm_bytes)?;
        if read == 0 {
            break;
        }
        if read > block_size as usize {
            return Err(Error::argument(
                "PCM source returned more frames than requested",
            ));
        }
        pcm::planar_from_interleaved(&info, &pcm_bytes, read, &mut planar)?;
        let block_crc = crc32(&pcm_bytes);
        total_crc.update(&pcm_bytes);
        pcm_total += pcm_bytes.len() as u64;

        let assignment = choose_assignment(&info, &planar, options.stereo_mode);
        apply_assignment(&planar, assignment, &mut encoded_channels);
        let subframe_config = SubframeConfig {
            max_lpc_order: options.max_lpc_order,
            max_partition_order: options.max_partition_order,
            // Float keys are not linear, so the raw-key path disables LPC by
            // itself. Float blocks that use the exact fixed-point transform
            // are linear and benefit from LPC like integer PCM does.
            allow_lpc: true,
        };
        let mut bits = BitWriter::with_capacity(read * channels * 2);
        for (channel, samples) in encoded_channels.iter().enumerate().take(channels) {
            let bps = info.encoded_bps(assignment, channel);
            let keys_are_floats = info.sample_format == SampleFormat::Float
                && match assignment {
                    ChannelAssignment::Independent => true,
                    ChannelAssignment::LeftSide => channel == 0,
                    ChannelAssignment::RightSide => channel == 1,
                    ChannelAssignment::MidSide => false,
                };
            if keys_are_floats {
                crate::float::encode(
                    &mut bits,
                    samples,
                    info.bits_per_sample as u32,
                    &subframe_config,
                )?;
            } else if info.sample_format == SampleFormat::Float {
                // Decorrelated float channels are arbitrary 64-bit integers,
                // not ordered float keys, so they use the integer subframe
                // path and are inverted by the decoder.
                subframe::encode(&mut bits, samples, 64, &subframe_config)?;
            } else {
                subframe::encode(&mut bits, samples, bps, &subframe_config)?;
            }
        }
        let payload = bits.finish();
        let frame_len = FRAME_HEADER_SIZE as usize + payload.len() + 4;
        if frame_len > u32::MAX as usize {
            return Err(Error::limit("compressed frame exceeds 4 GiB"));
        }
        if frame_index > u32::MAX as u64 {
            return Err(Error::limit("too many frames for the format"));
        }
        let frame_header = FrameHeader {
            index: frame_index as u32,
            first_sample: next_sample,
            block_size: read as u16,
            channel_assignment: assignment,
            frame_len: frame_len as u32,
            pcm_crc32: block_crc,
        };
        let mut frame = Vec::with_capacity(frame_len);
        frame.extend_from_slice(&frame_header.to_bytes());
        frame.extend_from_slice(&payload);
        let frame_crc = crc32(&frame);
        frame.extend_from_slice(&frame_crc.to_le_bytes());
        output.write_all(&frame)?;

        next_sample += read as u64;
        frame_index += 1;
        min_block = min_block.min(read as u16);
        max_block = max_block.max(read as u16);
    }

    source.finish()?;
    if let Some(expected) = expected_samples {
        if next_sample != expected {
            return Err(Error::invalid_pcm(format!(
                "expected {expected} sample frames but read {next_sample}"
            )));
        }
    }

    info.total_samples = next_sample;
    header.info.total_samples = next_sample;
    header.frame_count = frame_index;
    header.min_block_size = if frame_index > 0 { min_block } else { 0 };
    header.max_block_size = if frame_index > 0 { max_block } else { 0 };
    header.pcm_crc32 = total_crc.finalize();
    header.audio_offset = 128 + metadata_bytes.len() as u64;
    header.validate()?;
    output.seek(SeekFrom::Start(0))?;
    output.write_all(&header.to_bytes())?;
    output.flush()?;
    let end = output.seek(SeekFrom::End(0))?;
    output.flush()?;

    Ok(EncodeSummary {
        info,
        pcm_bytes: pcm_total,
        ocaf_bytes: end,
        frame_count: frame_index,
        min_block_size: header.min_block_size,
        max_block_size: header.max_block_size,
    })
}

/// A sequential source of canonical interleaved PCM frames.
trait FrameSource {
    /// Read up to `frames` frames. Returns zero at EOF and must never return
    /// a partial sample frame.
    fn read_frames(&mut self, frames: usize, out: &mut Vec<u8>) -> Result<usize>;

    /// Validate that the source was consumed exactly to its end.
    fn finish(&mut self) -> Result<()>;
}

impl<R: Read + Seek> FrameSource for WavReader<R> {
    fn read_frames(&mut self, frames: usize, out: &mut Vec<u8>) -> Result<usize> {
        WavReader::read_frames(self, frames, out)
    }

    fn finish(&mut self) -> Result<()> {
        if self.remaining_bytes() != 0 {
            return Err(Error::invalid_wav("WAV has unread data bytes"));
        }
        Ok(())
    }
}

struct RawPcmSource<R> {
    reader: R,
    block_align: usize,
}

impl<R: Read> FrameSource for RawPcmSource<R> {
    fn read_frames(&mut self, frames: usize, out: &mut Vec<u8>) -> Result<usize> {
        let bytes = frames
            .checked_mul(self.block_align)
            .ok_or_else(|| Error::limit("PCM block size overflow"))?;
        out.clear();
        out.resize(bytes, 0);
        let mut filled = 0;
        while filled < bytes {
            match self.reader.read(&mut out[filled..]) {
                Ok(0) => break,
                Ok(read) => filled += read,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(Error::Io(error)),
            }
        }
        out.truncate(filled);
        let extra = filled % self.block_align;
        if extra != 0 {
            return Err(Error::invalid_pcm(format!(
                "input ends with {extra} extra byte(s), which is not a whole sample frame"
            )));
        }
        Ok(filled / self.block_align)
    }

    fn finish(&mut self) -> Result<()> {
        Ok(())
    }
}

fn choose_assignment(
    info: &StreamInfo,
    channels: &[Vec<i64>],
    mode: StereoMode,
) -> ChannelAssignment {
    if info.channels != 2 || channels.len() != 2 {
        return ChannelAssignment::Independent;
    }
    if mode != StereoMode::Auto {
        return match mode {
            StereoMode::Independent => ChannelAssignment::Independent,
            StereoMode::LeftSide => ChannelAssignment::LeftSide,
            StereoMode::RightSide => ChannelAssignment::RightSide,
            // A mid/side sum needs one extra bit. 64-bit float keys have no
            // spare bit, so fall back to an invertible left/side transform.
            StereoMode::MidSide
                if info.sample_format == SampleFormat::Float && info.bits_per_sample == 64 =>
            {
                ChannelAssignment::LeftSide
            }
            StereoMode::MidSide => ChannelAssignment::MidSide,
            StereoMode::Auto => ChannelAssignment::Independent,
        };
    }

    let all = [
        ChannelAssignment::Independent,
        ChannelAssignment::LeftSide,
        ChannelAssignment::RightSide,
        ChannelAssignment::MidSide,
    ];
    let no_mid = [
        ChannelAssignment::Independent,
        ChannelAssignment::LeftSide,
        ChannelAssignment::RightSide,
    ];
    let candidates: &[ChannelAssignment] =
        if info.sample_format == SampleFormat::Float && info.bits_per_sample == 64 {
            &no_mid
        } else {
            &all
        };
    let mut best = ChannelAssignment::Independent;
    let mut best_bits = u64::MAX;
    let mut transformed = vec![Vec::new(), Vec::new()];
    for &candidate in candidates {
        transform_stereo(&channels[0], &channels[1], candidate, &mut transformed);
        let bits = estimate_channel(&transformed[0], info.encoded_bps(candidate, 0))
            + estimate_channel(&transformed[1], info.encoded_bps(candidate, 1));
        if bits < best_bits {
            best_bits = bits;
            best = candidate;
        }
    }
    best
}

fn apply_assignment(source: &[Vec<i64>], assignment: ChannelAssignment, out: &mut [Vec<i64>]) {
    if assignment == ChannelAssignment::Independent || source.len() != 2 {
        for (dest, src) in out.iter_mut().zip(source.iter()) {
            dest.clear();
            dest.extend_from_slice(src);
        }
        return;
    }
    transform_stereo(&source[0], &source[1], assignment, &mut out[..2]);
}

fn transform_stereo(
    left: &[i64],
    right: &[i64],
    assignment: ChannelAssignment,
    out: &mut [Vec<i64>],
) {
    let n = left.len().min(right.len());
    for buffer in out.iter_mut() {
        buffer.clear();
        buffer.reserve(n);
    }
    match assignment {
        ChannelAssignment::Independent => {
            out[0].extend_from_slice(left);
            out[1].extend_from_slice(right);
        }
        ChannelAssignment::LeftSide => {
            for i in 0..n {
                out[0].push(left[i]);
                out[1].push(left[i].wrapping_sub(right[i]));
            }
        }
        ChannelAssignment::RightSide => {
            for i in 0..n {
                out[0].push(left[i].wrapping_sub(right[i]));
                out[1].push(right[i]);
            }
        }
        ChannelAssignment::MidSide => {
            for i in 0..n {
                out[0].push((left[i].wrapping_add(right[i])) >> 1);
                out[1].push(left[i].wrapping_sub(right[i]));
            }
        }
    }
}

fn estimate_channel(samples: &[i64], bps: u32) -> u64 {
    let n = samples.len() as u64;
    let verbatim = 8 + n * bps as u64;
    if samples.is_empty() {
        return 0;
    }
    if samples.iter().all(|&s| s == samples[0]) {
        return (8 + bps as u64).min(verbatim);
    }
    let mut best = verbatim;
    for order in 1..=2u8 {
        if samples.len() <= order as usize {
            continue;
        }
        let coeffs = lpc::fixed_coefficients(order).unwrap();
        let mut residual = Vec::with_capacity(samples.len() - order as usize);
        for i in order as usize..samples.len() {
            let mut prediction = 0i64;
            for (j, &coeff) in coeffs.iter().enumerate() {
                prediction = prediction.wrapping_add(coeff.wrapping_mul(samples[i - 1 - j]));
            }
            residual.push(samples[i].wrapping_sub(prediction));
        }
        let mapped: Vec<u64> = residual.iter().map(|&v| rice::zigzag_encode(v)).collect();
        let (_, rice_bits) = rice::choose_parameter(&mapped);
        let cost = 8 + 8 + order as u64 * bps as u64 + 8 + 8 + rice_bits;
        best = best.min(cost);
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stereo_transforms_are_invertible() {
        let left = vec![10i64, -20, 300, -4000, 0];
        let right = vec![-10i64, 21, 299, 4000, 1];
        for assignment in [
            ChannelAssignment::LeftSide,
            ChannelAssignment::RightSide,
            ChannelAssignment::MidSide,
        ] {
            let mut encoded = vec![Vec::new(), Vec::new()];
            transform_stereo(&left, &right, assignment, &mut encoded);
            let mut decoded_left = Vec::new();
            let mut decoded_right = Vec::new();
            match assignment {
                ChannelAssignment::LeftSide => {
                    for (&l, &side) in encoded[0].iter().zip(&encoded[1]) {
                        decoded_left.push(l);
                        decoded_right.push(l - side);
                    }
                }
                ChannelAssignment::RightSide => {
                    for (&side, &r) in encoded[0].iter().zip(&encoded[1]) {
                        decoded_right.push(r);
                        decoded_left.push(side + r);
                    }
                }
                ChannelAssignment::MidSide => {
                    for (&mid, &side) in encoded[0].iter().zip(&encoded[1]) {
                        let l = mid + ((side + (side & 1)) >> 1);
                        decoded_left.push(l);
                        decoded_right.push(l - side);
                    }
                }
                ChannelAssignment::Independent => unreachable!(),
            }
            assert_eq!(decoded_left, left);
            assert_eq!(decoded_right, right);
        }
    }
}
