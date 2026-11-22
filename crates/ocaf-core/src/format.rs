//! On-disk structures for the OCAF container.
//!
//! All integers are little-endian. The authoritative description lives in
//! `FORMAT.md`; this module is the implementation of that document.

use crate::crc32::crc32;
use crate::error::{Error, Result};
use std::io::{Read, Write};

pub const MAGIC: [u8; 4] = *b"OCAF";
pub const FORMAT_MAJOR: u16 = 1;
pub const FORMAT_MINOR: u16 = 0;
pub const HEADER_SIZE: u16 = 128;
pub const CODEC_PREDICTIVE: u16 = 1;

pub const MAX_CHANNELS: u16 = 256;
pub const MAX_SAMPLE_RATE: u32 = 4_000_000;
pub const MAX_METADATA_BYTES: u64 = 128 * 1024 * 1024;
pub const MAX_METADATA_ENTRIES: usize = 4096;

pub const FRAME_MAGIC: [u8; 4] = *b"OCFR";
pub const FRAME_HEADER_SIZE: u16 = 36;
pub const FRAME_HEADER_CRC_OFFSET: usize = 32;

/// How sample values are represented.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SampleFormat {
    /// Signed integer PCM.
    Int,
    /// IEEE 754 binary floating point, stored bit-exactly.
    Float,
}

impl SampleFormat {
    pub fn to_u8(self) -> u8 {
        match self {
            SampleFormat::Int => 1,
            SampleFormat::Float => 2,
        }
    }

    pub fn from_u8(value: u8) -> Result<Self> {
        match value {
            1 => Ok(SampleFormat::Int),
            2 => Ok(SampleFormat::Float),
            other => Err(Error::invalid_ocaf(format!(
                "unknown sample format {other}"
            ))),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            SampleFormat::Int => "integer PCM",
            SampleFormat::Float => "IEEE float",
        }
    }
}

/// Basic description of the audio carried by a stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StreamInfo {
    pub sample_rate: u32,
    pub channels: u16,
    pub bits_per_sample: u16,
    pub sample_format: SampleFormat,
    pub channel_mask: u32,
    pub total_samples: u64,
}

impl StreamInfo {
    pub fn validate(&self) -> Result<()> {
        if self.sample_rate == 0 || self.sample_rate > MAX_SAMPLE_RATE {
            return Err(Error::argument(format!(
                "sample rate {} is outside 1..={MAX_SAMPLE_RATE} Hz",
                self.sample_rate
            )));
        }
        if self.channels == 0 || self.channels > MAX_CHANNELS {
            return Err(Error::argument(format!(
                "channel count {} is outside 1..={MAX_CHANNELS}",
                self.channels
            )));
        }
        match (self.sample_format, self.bits_per_sample) {
            (SampleFormat::Int, 16 | 24 | 32) => {}
            (SampleFormat::Float, 32 | 64) => {}
            (fmt, bits) => {
                return Err(Error::unsupported(format!(
                    "{bits}-bit {} is not supported (integer PCM: 16/24/32, float: 32/64)",
                    fmt.name()
                )))
            }
        }
        Ok(())
    }

    pub fn bytes_per_sample(&self) -> usize {
        self.bits_per_sample as usize / 8
    }

    pub fn block_align(&self) -> usize {
        self.bytes_per_sample() * self.channels as usize
    }

    /// Duration in seconds, or zero for an empty stream.
    pub fn duration_seconds(&self) -> f64 {
        if self.sample_rate == 0 {
            0.0
        } else {
            self.total_samples as f64 / self.sample_rate as f64
        }
    }

    /// Number of bits needed to represent one encoded channel of a frame.
    pub fn encoded_bps(&self, assignment: ChannelAssignment, channel: usize) -> u32 {
        if self.sample_format == SampleFormat::Float {
            return 64;
        }
        match assignment {
            ChannelAssignment::Independent => self.bits_per_sample as u32,
            ChannelAssignment::LeftSide => {
                if channel == 1 {
                    self.bits_per_sample as u32 + 1
                } else {
                    self.bits_per_sample as u32
                }
            }
            ChannelAssignment::RightSide => {
                if channel == 0 {
                    self.bits_per_sample as u32 + 1
                } else {
                    self.bits_per_sample as u32
                }
            }
            ChannelAssignment::MidSide => {
                if channel == 1 {
                    self.bits_per_sample as u32 + 1
                } else {
                    self.bits_per_sample as u32
                }
            }
        }
    }

    /// Sample rate and channel count formatted for humans.
    pub fn short_description(&self) -> String {
        let layout = match self.channels {
            1 => "mono".to_string(),
            2 => "stereo".to_string(),
            n => format!("{n} channels"),
        };
        format!(
            "{} Hz, {layout}, {}-bit {}",
            self.sample_rate,
            self.bits_per_sample,
            self.sample_format.name()
        )
    }
}

/// Fixed stream header stored at offset 0 of every OCAF file.
#[derive(Clone, Debug)]
pub struct StreamHeader {
    pub info: StreamInfo,
    pub codec: u16,
    pub frame_count: u64,
    pub min_block_size: u16,
    pub max_block_size: u16,
    pub audio_offset: u64,
    pub metadata_len: u64,
    pub pcm_crc32: u32,
    /// Minor version read from the file. The encoder writes `FORMAT_MINOR`.
    pub minor_version: u16,
}

impl StreamHeader {
    pub fn new(info: StreamInfo, metadata_len: u64) -> Self {
        Self {
            info,
            codec: CODEC_PREDICTIVE,
            frame_count: 0,
            min_block_size: 0,
            max_block_size: 0,
            audio_offset: HEADER_SIZE as u64 + metadata_len,
            metadata_len,
            pcm_crc32: 0,
            minor_version: FORMAT_MINOR,
        }
    }

    pub fn validate(&self) -> Result<()> {
        self.info.validate()?;
        if self.codec != CODEC_PREDICTIVE {
            return Err(Error::unsupported(format!(
                "codec id {} is not known to this build",
                self.codec
            )));
        }
        if self.metadata_len > MAX_METADATA_BYTES {
            return Err(Error::limit("metadata section is too large"));
        }
        if self.audio_offset < HEADER_SIZE as u64 + self.metadata_len {
            return Err(Error::invalid_ocaf(
                "audio offset overlaps the header or metadata",
            ));
        }
        if self.frame_count > 0 {
            if self.min_block_size == 0
                || self.max_block_size == 0
                || self.min_block_size > self.max_block_size
            {
                return Err(Error::invalid_ocaf("invalid block size bounds"));
            }
        } else if self.info.total_samples != 0 {
            return Err(Error::invalid_ocaf(
                "stream declares samples but has no frames",
            ));
        }
        Ok(())
    }

    pub fn to_bytes(&self) -> [u8; HEADER_SIZE as usize] {
        let mut b = [0u8; HEADER_SIZE as usize];
        b[0..4].copy_from_slice(&MAGIC);
        put_u16(&mut b, 4, FORMAT_MAJOR);
        put_u16(&mut b, 6, self.minor_version);
        put_u16(&mut b, 8, self.codec);
        put_u16(&mut b, 10, HEADER_SIZE);
        put_u32(&mut b, 12, self.info.sample_rate);
        put_u16(&mut b, 16, self.info.channels);
        put_u16(&mut b, 18, self.info.bits_per_sample);
        b[20] = self.info.sample_format.to_u8();
        b[21] = 0; // flags, reserved
        put_u32(&mut b, 22, self.info.channel_mask);
        put_u16(&mut b, 26, 0); // reserved
        put_u64(&mut b, 28, self.info.total_samples);
        put_u64(&mut b, 36, self.frame_count);
        put_u16(&mut b, 44, self.min_block_size);
        put_u16(&mut b, 46, self.max_block_size);
        put_u64(&mut b, 48, self.audio_offset);
        put_u64(&mut b, 56, self.metadata_len);
        put_u32(&mut b, 64, self.pcm_crc32);
        // Bytes 68..124 are reserved and must be zero.
        let crc = crc32(&b[..124]);
        put_u32(&mut b, 124, crc);
        b
    }

    pub fn write_to<W: Write>(&self, writer: &mut W) -> Result<()> {
        writer.write_all(&self.to_bytes())?;
        Ok(())
    }

    pub fn read_from<R: Read>(reader: &mut R) -> Result<Self> {
        let mut b = [0u8; HEADER_SIZE as usize];
        reader.read_exact(&mut b).map_err(|e| {
            if e.kind() == std::io::ErrorKind::UnexpectedEof {
                Error::invalid_ocaf("file is shorter than the OCAF header")
            } else {
                Error::Io(e)
            }
        })?;
        Self::parse(&b)
    }

    pub fn parse(b: &[u8; HEADER_SIZE as usize]) -> Result<Self> {
        if b[0..4] != MAGIC {
            return Err(Error::invalid_ocaf("bad magic bytes (expected \"OCAF\")"));
        }
        let major = get_u16(b, 4);
        let minor = get_u16(b, 6);
        if major != FORMAT_MAJOR {
            return Err(Error::unsupported(format!(
                "container major version {major} (this build supports {FORMAT_MAJOR})"
            )));
        }
        let expected_crc = get_u32(b, 124);
        let actual_crc = crc32(&b[..124]);
        if expected_crc != actual_crc {
            return Err(Error::corrupt("stream header CRC-32 mismatch"));
        }
        let header_size = get_u16(b, 10);
        if header_size != HEADER_SIZE {
            return Err(Error::unsupported(format!(
                "header size {header_size} (expected {HEADER_SIZE})"
            )));
        }
        let sample_format = SampleFormat::from_u8(b[20])?;
        let info = StreamInfo {
            sample_rate: get_u32(b, 12),
            channels: get_u16(b, 16),
            bits_per_sample: get_u16(b, 18),
            sample_format,
            channel_mask: get_u32(b, 22),
            total_samples: get_u64(b, 28),
        };
        let header = StreamHeader {
            info,
            codec: get_u16(b, 8),
            frame_count: get_u64(b, 36),
            min_block_size: get_u16(b, 44),
            max_block_size: get_u16(b, 46),
            audio_offset: get_u64(b, 48),
            metadata_len: get_u64(b, 56),
            pcm_crc32: get_u32(b, 64),
            minor_version: minor,
        };
        header.validate()?;
        Ok(header)
    }
}

/// Stereo inter-channel decorrelation applied to a frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChannelAssignment {
    /// Channels are coded as-is.
    Independent = 0,
    /// Channel 0 = left, channel 1 = left - right.
    LeftSide = 1,
    /// Channel 0 = left - right, channel 1 = right.
    RightSide = 2,
    /// Channel 0 = floor((left + right) / 2), channel 1 = left - right.
    MidSide = 3,
}

impl ChannelAssignment {
    pub fn from_u8(value: u8) -> Result<Self> {
        match value {
            0 => Ok(ChannelAssignment::Independent),
            1 => Ok(ChannelAssignment::LeftSide),
            2 => Ok(ChannelAssignment::RightSide),
            3 => Ok(ChannelAssignment::MidSide),
            other => Err(Error::invalid_ocaf(format!(
                "unknown channel assignment {other}"
            ))),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            ChannelAssignment::Independent => "independent",
            ChannelAssignment::LeftSide => "left/side",
            ChannelAssignment::RightSide => "side/right",
            ChannelAssignment::MidSide => "mid/side",
        }
    }
}

/// Header of one compressed audio frame.
#[derive(Clone, Debug)]
pub struct FrameHeader {
    pub index: u32,
    pub first_sample: u64,
    pub block_size: u16,
    pub channel_assignment: ChannelAssignment,
    pub frame_len: u32,
    pub pcm_crc32: u32,
}

impl FrameHeader {
    pub fn to_bytes(&self) -> [u8; FRAME_HEADER_SIZE as usize] {
        let mut b = [0u8; FRAME_HEADER_SIZE as usize];
        b[0..4].copy_from_slice(&FRAME_MAGIC);
        put_u16(&mut b, 4, FRAME_HEADER_SIZE);
        put_u32(&mut b, 6, self.index);
        put_u64(&mut b, 10, self.first_sample);
        put_u16(&mut b, 18, self.block_size);
        b[20] = self.channel_assignment as u8;
        b[21] = 0; // flags
        put_u16(&mut b, 22, 0); // reserved
        put_u32(&mut b, 24, self.frame_len);
        put_u32(&mut b, 28, self.pcm_crc32);
        let crc = crc32(&b[..FRAME_HEADER_CRC_OFFSET]);
        put_u32(&mut b, FRAME_HEADER_CRC_OFFSET, crc);
        b
    }

    pub fn parse(b: &[u8]) -> Result<Self> {
        if b.len() < FRAME_HEADER_SIZE as usize {
            return Err(Error::invalid_ocaf("truncated frame header"));
        }
        if b[0..4] != FRAME_MAGIC {
            return Err(Error::invalid_ocaf("bad frame magic"));
        }
        let header_size = get_u16(b, 4);
        if header_size != FRAME_HEADER_SIZE {
            return Err(Error::unsupported(format!(
                "frame header size {header_size}"
            )));
        }
        let expected_crc = get_u32(b, FRAME_HEADER_CRC_OFFSET);
        let actual_crc = crc32(&b[..FRAME_HEADER_CRC_OFFSET]);
        if expected_crc != actual_crc {
            return Err(Error::corrupt("frame header CRC-32 mismatch"));
        }
        if b[21] != 0 || get_u16(b, 22) != 0 {
            return Err(Error::invalid_ocaf(
                "frame flags/reserved bytes are non-zero",
            ));
        }
        let frame_len = get_u32(b, 24);
        if frame_len < FRAME_HEADER_SIZE as u32 + 4 {
            return Err(Error::invalid_ocaf("frame length is too small"));
        }
        Ok(FrameHeader {
            index: get_u32(b, 6),
            first_sample: get_u64(b, 10),
            block_size: get_u16(b, 18),
            channel_assignment: ChannelAssignment::from_u8(b[20])?,
            frame_len,
            pcm_crc32: get_u32(b, 28),
        })
    }
}

// Little-endian helpers.

pub(crate) fn put_u16(buf: &mut [u8], offset: usize, value: u16) {
    buf[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

pub(crate) fn put_u32(buf: &mut [u8], offset: usize, value: u32) {
    buf[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

pub(crate) fn put_u64(buf: &mut [u8], offset: usize, value: u64) {
    buf[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

pub(crate) fn get_u16(buf: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([buf[offset], buf[offset + 1]])
}

pub(crate) fn get_u32(buf: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        buf[offset],
        buf[offset + 1],
        buf[offset + 2],
        buf[offset + 3],
    ])
}

pub(crate) fn get_u64(buf: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes([
        buf[offset],
        buf[offset + 1],
        buf[offset + 2],
        buf[offset + 3],
        buf[offset + 4],
        buf[offset + 5],
        buf[offset + 6],
        buf[offset + 7],
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_info() -> StreamInfo {
        StreamInfo {
            sample_rate: 44_100,
            channels: 2,
            bits_per_sample: 16,
            sample_format: SampleFormat::Int,
            channel_mask: 3,
            total_samples: 123_456,
        }
    }

    #[test]
    fn header_roundtrip() {
        let mut h = StreamHeader::new(test_info(), 42);
        h.frame_count = 31;
        h.min_block_size = 100;
        h.max_block_size = 4096;
        h.pcm_crc32 = 0xDEAD_BEEF;
        let bytes = h.to_bytes();
        let parsed = StreamHeader::parse(&bytes).unwrap();
        assert_eq!(parsed.info, h.info);
        assert_eq!(parsed.frame_count, 31);
        assert_eq!(parsed.pcm_crc32, 0xDEAD_BEEF);
        assert_eq!(parsed.audio_offset, 128 + 42);
    }

    #[test]
    fn header_corruption_detected() {
        let h = StreamHeader::new(test_info(), 0);
        let mut bytes = h.to_bytes();
        bytes[20] = 2;
        assert!(StreamHeader::parse(&bytes).is_err());
    }

    #[test]
    fn unknown_major_rejected() {
        let h = StreamHeader::new(test_info(), 0);
        let mut bytes = h.to_bytes();
        put_u16(&mut bytes, 4, 2);
        let crc = crc32(&bytes[..124]);
        put_u32(&mut bytes, 124, crc);
        assert!(matches!(
            StreamHeader::parse(&bytes),
            Err(Error::Unsupported(_))
        ));
    }

    #[test]
    fn frame_header_roundtrip() {
        let h = FrameHeader {
            index: 7,
            first_sample: 28672,
            block_size: 4096,
            channel_assignment: ChannelAssignment::MidSide,
            frame_len: 9000,
            pcm_crc32: 0x1234_5678,
        };
        let bytes = h.to_bytes();
        let parsed = FrameHeader::parse(&bytes).unwrap();
        assert_eq!(parsed.index, 7);
        assert_eq!(parsed.first_sample, 28672);
        assert_eq!(parsed.block_size, 4096);
        assert_eq!(parsed.channel_assignment, ChannelAssignment::MidSide);
        assert_eq!(parsed.frame_len, 9000);
        assert_eq!(parsed.pcm_crc32, 0x1234_5678);
    }
}
