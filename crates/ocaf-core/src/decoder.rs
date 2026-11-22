//! OCAF stream decoder.
//!
//! Decoding validates every structural invariant that can be checked locally:
//! header/frame CRC, frame ordering, declared lengths, subframe bounds, PCM
//! block CRC and the whole-stream PCM CRC.

use crate::bitio::BitReader;
use crate::crc32::{crc32, Crc32};
use crate::error::{Error, Result};
use crate::format::{
    get_u32, ChannelAssignment, FrameHeader, SampleFormat, StreamHeader, StreamInfo,
    FRAME_HEADER_SIZE, MAX_METADATA_BYTES,
};
use crate::metadata::{Metadata, MetadataEntry};
use crate::pcm;
use crate::subframe;
use crate::wav::WavWriter;
use std::io::{Read, Seek, SeekFrom, Write};

/// Defensive cap for one compressed frame (the format allows larger frames in
/// principle, but this prevents a hostile header from causing a huge allocation).
pub const MAX_FRAME_BYTES: u64 = 256 * 1024 * 1024;

/// Summary returned by decoding and verification.
#[derive(Clone, Debug)]
pub struct DecodeSummary {
    pub info: StreamInfo,
    pub metadata: Vec<MetadataEntry>,
    pub pcm_bytes: u64,
    pub ocaf_bytes: u64,
    pub frame_count: u64,
    pub pcm_crc32: u32,
}

/// Result of verifying an OCAF file.
#[derive(Clone, Debug)]
pub struct VerifyReport {
    pub info: StreamInfo,
    pub metadata_entries: usize,
    pub frame_count: u64,
    pub pcm_bytes: u64,
    pub pcm_crc32: u32,
}

/// Streaming OCAF decoder.
pub struct Decoder<R: Read + Seek> {
    reader: R,
    header: StreamHeader,
    metadata: Metadata,
    file_len: u64,
    frames_read: u64,
    next_sample: u64,
    total_crc: Crc32,
    encoded_channels: Vec<Vec<i64>>,
    source_channels: Vec<Vec<i64>>,
}

impl<R: Read + Seek> Decoder<R> {
    pub fn open(mut reader: R) -> Result<Self> {
        let file_len = reader.seek(SeekFrom::End(0))?;
        reader.seek(SeekFrom::Start(0))?;
        let header = StreamHeader::read_from(&mut reader)?;
        if header.metadata_len > MAX_METADATA_BYTES {
            return Err(Error::limit("metadata section is too large"));
        }
        let mut metadata_bytes = vec![0u8; header.metadata_len as usize];
        reader.read_exact(&mut metadata_bytes).map_err(|e| {
            if e.kind() == std::io::ErrorKind::UnexpectedEof {
                Error::invalid_ocaf("truncated metadata section")
            } else {
                Error::Io(e)
            }
        })?;
        let metadata = Metadata::from_bytes(&metadata_bytes)?;
        if header.audio_offset > file_len {
            return Err(Error::invalid_ocaf(
                "audio offset points beyond the end of the file",
            ));
        }
        reader.seek(SeekFrom::Start(header.audio_offset))?;
        if header.info.total_samples > 0 && header.frame_count == 0 {
            return Err(Error::invalid_ocaf("missing audio frames"));
        }
        let channels = header.info.channels as usize;
        Ok(Self {
            reader,
            header,
            metadata,
            file_len,
            frames_read: 0,
            next_sample: 0,
            total_crc: Crc32::new(),
            encoded_channels: pcm::planar_channels(channels, 0),
            source_channels: pcm::planar_channels(channels, 0),
        })
    }

    pub fn header(&self) -> &StreamHeader {
        &self.header
    }

    pub fn info(&self) -> &StreamInfo {
        &self.header.info
    }

    pub fn metadata(&self) -> &Metadata {
        &self.metadata
    }

    pub fn file_len(&self) -> u64 {
        self.file_len
    }

    /// Decode the next frame to canonical interleaved PCM bytes.
    ///
    /// Returns `Ok(None)` after the last frame. Call [`Decoder::finish`] after
    /// the loop to validate the whole-stream checksum.
    pub fn next_block(&mut self, pcm_out: &mut Vec<u8>) -> Result<Option<usize>> {
        if self.frames_read >= self.header.frame_count {
            return Ok(None);
        }
        let mut header_bytes = [0u8; FRAME_HEADER_SIZE as usize];
        self.reader.read_exact(&mut header_bytes).map_err(|e| {
            if e.kind() == std::io::ErrorKind::UnexpectedEof {
                Error::invalid_ocaf(format!(
                    "file ends before frame {} of {}",
                    self.frames_read, self.header.frame_count
                ))
            } else {
                Error::Io(e)
            }
        })?;
        let frame = FrameHeader::parse(&header_bytes)?;
        if frame.index as u64 != self.frames_read {
            return Err(Error::invalid_ocaf(format!(
                "frame index {} out of sequence (expected {})",
                frame.index, self.frames_read
            )));
        }
        if frame.first_sample != self.next_sample {
            return Err(Error::invalid_ocaf(format!(
                "frame starts at sample {} instead of {}",
                frame.first_sample, self.next_sample
            )));
        }
        if frame.block_size == 0 {
            return Err(Error::invalid_ocaf("zero-sized frame"));
        }
        let block_size = frame.block_size as usize;
        if self.next_sample + block_size as u64 > self.header.info.total_samples {
            return Err(Error::invalid_ocaf(
                "frame extends beyond the declared sample count",
            ));
        }
        if frame.channel_assignment != ChannelAssignment::Independent
            && self.header.info.channels != 2
        {
            return Err(Error::invalid_ocaf(
                "channel assignment requires a stereo stream",
            ));
        }
        if frame.channel_assignment == ChannelAssignment::MidSide
            && self.header.info.sample_format == SampleFormat::Float
            && self.header.info.bits_per_sample == 64
        {
            return Err(Error::unsupported(
                "mid/side is not representable for 64-bit float keys",
            ));
        }
        if frame.frame_len as u64 > MAX_FRAME_BYTES {
            return Err(Error::limit("compressed frame is unreasonably large"));
        }
        let rest_len = frame.frame_len as usize - FRAME_HEADER_SIZE as usize;
        if rest_len < 4 {
            return Err(Error::invalid_ocaf("invalid frame length"));
        }
        let current = self.reader.stream_position()?;
        if current + rest_len as u64 > self.file_len {
            return Err(Error::invalid_ocaf(format!(
                "frame {} is truncated",
                self.frames_read
            )));
        }
        let mut rest = vec![0u8; rest_len];
        self.reader.read_exact(&mut rest).map_err(|e| {
            if e.kind() == std::io::ErrorKind::UnexpectedEof {
                Error::invalid_ocaf(format!("frame {} is truncated", self.frames_read))
            } else {
                Error::Io(e)
            }
        })?;
        let expected_crc = get_u32(&rest, rest_len - 4);
        let payload_len = rest_len - 4;
        let mut frame_crc = Crc32::new();
        frame_crc.update(&header_bytes);
        frame_crc.update(&rest[..payload_len]);
        if frame_crc.finalize() != expected_crc {
            return Err(Error::corrupt(format!(
                "frame {} CRC-32 mismatch",
                self.frames_read
            )));
        }

        let payload = &rest[..payload_len];
        let mut bits = BitReader::new(payload);
        let channels = self.header.info.channels as usize;
        for channel in 0..channels {
            let keys_are_floats = self.header.info.sample_format == SampleFormat::Float
                && match frame.channel_assignment {
                    ChannelAssignment::Independent => true,
                    ChannelAssignment::LeftSide => channel == 0,
                    ChannelAssignment::RightSide => channel == 1,
                    ChannelAssignment::MidSide => false,
                };
            if keys_are_floats {
                crate::float::decode(
                    &mut bits,
                    &mut self.encoded_channels[channel],
                    block_size,
                    self.header.info.bits_per_sample as u32,
                )
                .map_err(|e| {
                    Error::invalid_ocaf(format!(
                        "frame {}, channel {channel}: {e}",
                        self.frames_read
                    ))
                })?;
                continue;
            }
            let bps = if self.header.info.sample_format == SampleFormat::Float {
                64
            } else {
                self.header
                    .info
                    .encoded_bps(frame.channel_assignment, channel)
            };
            subframe::decode(
                &mut bits,
                &mut self.encoded_channels[channel],
                block_size,
                bps,
            )
            .map_err(|e| {
                Error::invalid_ocaf(format!(
                    "frame {}, channel {channel}: {e}",
                    self.frames_read
                ))
            })?;
        }
        let remaining = bits.remaining_bits();
        if remaining >= 8 {
            return Err(Error::invalid_ocaf(format!(
                "frame {} has {} unused payload bits",
                self.frames_read, remaining
            )));
        }
        if remaining > 0 && bits.read_bits(remaining as u32)? != 0 {
            return Err(Error::invalid_ocaf("non-zero frame padding bits"));
        }

        self.invert_assignment(frame.channel_assignment, block_size)?;
        if self.header.info.sample_format == SampleFormat::Float
            && self.header.info.bits_per_sample == 32
        {
            for channel in &self.source_channels {
                if channel
                    .iter()
                    .any(|&key| !(0..=(u32::MAX as i64)).contains(&key))
                {
                    return Err(Error::invalid_ocaf(
                        "decoded float32 key is outside the 32-bit range",
                    ));
                }
            }
        }
        pcm::interleaved_from_planar(
            &self.header.info,
            &self.source_channels,
            block_size,
            pcm_out,
        )?;
        let block_crc = crc32(pcm_out);
        if block_crc != frame.pcm_crc32 {
            return Err(Error::corrupt(format!(
                "frame {} PCM CRC-32 mismatch",
                self.frames_read
            )));
        }
        self.total_crc.update(pcm_out);
        self.frames_read += 1;
        self.next_sample += block_size as u64;
        Ok(Some(block_size))
    }

    fn invert_assignment(
        &mut self,
        assignment: ChannelAssignment,
        block_size: usize,
    ) -> Result<()> {
        let channels = self.header.info.channels as usize;
        if assignment == ChannelAssignment::Independent || channels != 2 {
            for channel in 0..channels {
                if self.encoded_channels[channel].len() != block_size {
                    return Err(Error::invalid_ocaf("decoded channel length mismatch"));
                }
                self.source_channels[channel].clear();
                self.source_channels[channel].extend_from_slice(&self.encoded_channels[channel]);
            }
            return Ok(());
        }
        for channel in 0..2 {
            if self.encoded_channels[channel].len() != block_size {
                return Err(Error::invalid_ocaf("decoded channel length mismatch"));
            }
            self.source_channels[channel].clear();
            self.source_channels[channel].reserve(block_size);
        }
        match assignment {
            ChannelAssignment::LeftSide => {
                for i in 0..block_size {
                    let left = self.encoded_channels[0][i];
                    let side = self.encoded_channels[1][i];
                    self.source_channels[0].push(left);
                    self.source_channels[1].push(left.wrapping_sub(side));
                }
            }
            ChannelAssignment::RightSide => {
                for i in 0..block_size {
                    let side = self.encoded_channels[0][i];
                    let right = self.encoded_channels[1][i];
                    self.source_channels[0].push(side.wrapping_add(right));
                    self.source_channels[1].push(right);
                }
            }
            ChannelAssignment::MidSide => {
                for i in 0..block_size {
                    let mid = self.encoded_channels[0][i];
                    let side = self.encoded_channels[1][i];
                    let left = mid.wrapping_add((side.wrapping_add(side & 1)) >> 1);
                    self.source_channels[0].push(left);
                    self.source_channels[1].push(left.wrapping_sub(side));
                }
            }
            ChannelAssignment::Independent => unreachable!(),
        }
        Ok(())
    }

    /// Validate stream completion and trailing data.
    pub fn finish(mut self) -> Result<DecodeSummary> {
        if self.frames_read != self.header.frame_count {
            return Err(Error::invalid_ocaf(format!(
                "expected {} frames, decoded {}",
                self.header.frame_count, self.frames_read
            )));
        }
        if self.next_sample != self.header.info.total_samples {
            return Err(Error::invalid_ocaf(format!(
                "decoded {} samples, expected {}",
                self.next_sample, self.header.info.total_samples
            )));
        }
        let actual_crc = self.total_crc.finalize();
        if actual_crc != self.header.pcm_crc32 {
            return Err(Error::corrupt("whole-stream PCM CRC-32 mismatch"));
        }
        let position = self.reader.stream_position()?;
        if position != self.file_len {
            return Err(Error::invalid_ocaf(format!(
                "{} trailing bytes after the last frame",
                self.file_len - position
            )));
        }
        Ok(DecodeSummary {
            info: self.header.info,
            metadata: self.metadata.entries().to_vec(),
            pcm_bytes: self.header.info.total_samples * self.header.info.block_align() as u64,
            ocaf_bytes: self.file_len,
            frame_count: self.header.frame_count,
            pcm_crc32: actual_crc,
        })
    }
}

/// Decode an OCAF file to a WAV stream.
pub fn decode_to_wav<R: Read + Seek, W: Write + Seek>(
    input: R,
    output: W,
) -> Result<DecodeSummary> {
    let mut decoder = Decoder::open(input)?;
    let info = *decoder.info();
    let metadata = decoder.metadata().clone();
    let mut writer = WavWriter::new(output, info, &metadata)?;
    let mut pcm = Vec::new();
    while decoder.next_block(&mut pcm)?.is_some() {
        writer.write_frames(&pcm)?;
    }
    let summary = decoder.finish()?;
    writer.finish()?;
    Ok(summary)
}

/// Fully decode an OCAF file and verify all checksums, without writing audio.
pub fn verify<R: Read + Seek>(input: R) -> Result<VerifyReport> {
    let mut decoder = Decoder::open(input)?;
    let mut pcm = Vec::new();
    while decoder.next_block(&mut pcm)?.is_some() {}
    let summary = decoder.finish()?;
    Ok(VerifyReport {
        info: summary.info,
        metadata_entries: summary.metadata.len(),
        frame_count: summary.frame_count,
        pcm_bytes: summary.pcm_bytes,
        pcm_crc32: summary.pcm_crc32,
    })
}

/// Read only the header and metadata of an OCAF file.
pub fn probe<R: Read + Seek>(mut input: R) -> Result<ProbeInfo> {
    let file_len = input.seek(SeekFrom::End(0))?;
    input.seek(SeekFrom::Start(0))?;
    let header = StreamHeader::read_from(&mut input)?;
    let mut metadata_bytes = vec![0u8; header.metadata_len as usize];
    input.read_exact(&mut metadata_bytes)?;
    let metadata = Metadata::from_bytes(&metadata_bytes)?;
    let pcm_bytes = header.info.total_samples * header.info.block_align() as u64;
    Ok(ProbeInfo {
        header,
        metadata,
        file_len,
        pcm_bytes,
    })
}

/// Information available without decoding audio frames.
#[derive(Clone, Debug)]
pub struct ProbeInfo {
    pub header: StreamHeader,
    pub metadata: Metadata,
    pub file_len: u64,
    pub pcm_bytes: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encoder::{encode_wav, EncodeOptions};
    use crate::format::{SampleFormat, StreamInfo};
    use crate::wav::{WavReader, WavWriter};
    use std::io::Cursor;

    fn make_wav(samples: &[Vec<i64>], info: StreamInfo) -> Vec<u8> {
        let mut output = Cursor::new(Vec::new());
        let mut writer = WavWriter::new(&mut output, info, &Metadata::new()).unwrap();
        let frames = samples[0].len();
        let mut bytes = Vec::new();
        pcm::interleaved_from_planar(&info, samples, frames, &mut bytes).unwrap();
        writer.write_frames(&bytes).unwrap();
        writer.finish().unwrap();
        output.into_inner()
    }

    #[test]
    fn roundtrip_stereo_mid_side() {
        let info = StreamInfo {
            sample_rate: 44_100,
            channels: 2,
            bits_per_sample: 16,
            sample_format: SampleFormat::Int,
            channel_mask: 3,
            total_samples: 10_000,
        };
        let mut left = Vec::new();
        let mut right = Vec::new();
        for i in 0..10_000i64 {
            left.push((i as f64 * 0.03).sin().mul_add(20000.0, 0.0) as i64);
            right.push(left[i as usize] / 2 + (i % 7));
        }
        let wav = make_wav(&[left.clone(), right.clone()], info);
        let mut ocaf = Cursor::new(Vec::new());
        let options = EncodeOptions {
            block_size: 1000,
            ..Default::default()
        };
        encode_wav(Cursor::new(wav), &mut ocaf, &options).unwrap();
        ocaf.set_position(0);
        let mut decoded = Cursor::new(Vec::new());
        decode_to_wav(&mut ocaf, &mut decoded).unwrap();
        let mut reader = WavReader::open(Cursor::new(decoded.into_inner())).unwrap();
        let mut pcm = Vec::new();
        reader.read_frames(10_000, &mut pcm).unwrap();
        let mut expected = Vec::new();
        pcm::interleaved_from_planar(&info, &[left, right], 10_000, &mut expected).unwrap();
        assert_eq!(pcm, expected);
    }
}
