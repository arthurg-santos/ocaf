//! Streaming RIFF/WAVE reader and writer.
//!
//! Supported inputs are uncompressed integer PCM (16, 24 and 32 bits) and IEEE
//! float PCM (32 and 64 bits), including WAVE_FORMAT_EXTENSIBLE. RF64 and
//! compressed WAV variants are rejected with an explicit error.

use crate::error::{Error, Result};
use crate::format::{SampleFormat, StreamInfo};
use crate::metadata::{
    Metadata, ALBUM, ARTIST, COMMENT, DATE, GENRE, SOFTWARE, TITLE, TRACKNUMBER,
};
use std::io::{Read, Seek, SeekFrom, Write};

const FORMAT_PCM: u16 = 1;
const FORMAT_FLOAT: u16 = 3;
const FORMAT_EXTENSIBLE: u16 = 0xFFFE;
const WAVE_EXTENSIBLE_GUID_TAIL: [u8; 12] = [
    0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xAA, 0x00, 0x38, 0x9B, 0x71,
];
const MAX_LIST_BYTES: u64 = 32 * 1024 * 1024;

/// Parsed information about a WAV file.
#[derive(Clone, Debug)]
pub struct WavInfo {
    pub spec: StreamInfo,
    pub data_offset: u64,
    pub data_len: u64,
    pub metadata: Metadata,
}

/// Sequential reader for uncompressed WAV files.
pub struct WavReader<R: Read + Seek> {
    reader: R,
    info: WavInfo,
    remaining_bytes: u64,
}

impl<R: Read + Seek> WavReader<R> {
    pub fn open(mut reader: R) -> Result<Self> {
        let file_len = reader.seek(SeekFrom::End(0))?;
        reader.seek(SeekFrom::Start(0))?;
        if file_len < 12 {
            return Err(Error::invalid_wav("file is shorter than a RIFF header"));
        }
        let mut riff = [0u8; 12];
        reader.read_exact(&mut riff)?;
        if &riff[0..4] == b"RF64" {
            return Err(Error::unsupported(
                "RF64/BW64 WAV files are not supported (4 GiB limit)",
            ));
        }
        if &riff[0..4] != b"RIFF" || &riff[8..12] != b"WAVE" {
            return Err(Error::invalid_wav("missing RIFF/WAVE signature"));
        }
        let riff_size = u32::from_le_bytes([riff[4], riff[5], riff[6], riff[7]]) as u64;
        if riff_size < 4 {
            return Err(Error::invalid_wav("RIFF size is invalid"));
        }
        let declared_end = (riff_size + 8).min(file_len);

        let mut position = 12u64;
        let mut fmt: Option<FmtChunk> = None;
        let mut data: Option<(u64, u64)> = None;
        let mut metadata = Metadata::new();
        while position + 8 <= declared_end {
            reader.seek(SeekFrom::Start(position))?;
            let mut header = [0u8; 8];
            reader.read_exact(&mut header)?;
            let id = &header[0..4];
            let size = u32::from_le_bytes([header[4], header[5], header[6], header[7]]) as u64;
            if id == b"data" && size == u32::MAX as u64 {
                return Err(Error::unsupported(
                    "RF64-style data chunks are not supported",
                ));
            }
            let data_offset = position + 8;
            let data_end = data_offset
                .checked_add(size)
                .ok_or_else(|| Error::invalid_wav("chunk size overflow"))?;
            if data_end > declared_end {
                return Err(Error::invalid_wav(format!(
                    "chunk {:?} extends beyond the RIFF file",
                    String::from_utf8_lossy(id)
                )));
            }
            match id {
                b"fmt " => {
                    if !(16..=4096).contains(&size) {
                        return Err(Error::invalid_wav("invalid fmt chunk size"));
                    }
                    let mut payload = vec![0u8; size as usize];
                    reader.read_exact(&mut payload)?;
                    fmt = Some(parse_fmt(&payload)?);
                }
                b"data" => {
                    if data.is_none() {
                        data = Some((data_offset, size));
                    }
                }
                b"LIST" if size <= MAX_LIST_BYTES => {
                    let mut payload = vec![0u8; size as usize];
                    reader.read_exact(&mut payload)?;
                    parse_info_list(&payload, &mut metadata)?;
                }
                _ => {}
            }
            position = data_end + (size & 1);
        }

        let fmt = fmt.ok_or_else(|| Error::invalid_wav("missing fmt chunk"))?;
        let (data_offset, data_len) =
            data.ok_or_else(|| Error::invalid_wav("missing data chunk"))?;
        if data_len % fmt.block_align as u64 != 0 {
            return Err(Error::invalid_wav(
                "data chunk is not a whole number of sample frames",
            ));
        }
        let total_samples = data_len / fmt.block_align as u64;
        let spec = StreamInfo {
            sample_rate: fmt.sample_rate,
            channels: fmt.channels,
            bits_per_sample: fmt.bits_per_sample,
            sample_format: fmt.sample_format,
            channel_mask: fmt.channel_mask,
            total_samples,
        };
        spec.validate()
            .map_err(|e| Error::invalid_wav(e.to_string()))?;
        reader.seek(SeekFrom::Start(data_offset))?;
        Ok(Self {
            reader,
            info: WavInfo {
                spec,
                data_offset,
                data_len,
                metadata,
            },
            remaining_bytes: data_len,
        })
    }

    pub fn info(&self) -> &WavInfo {
        &self.info
    }

    pub fn into_inner(self) -> R {
        self.reader
    }

    /// Read up to `frames` sample frames as canonical interleaved PCM bytes.
    pub fn read_frames(&mut self, frames: usize, out: &mut Vec<u8>) -> Result<usize> {
        let block_align = self.info.spec.block_align() as u64;
        if block_align == 0 {
            return Err(Error::invalid_wav("invalid block alignment"));
        }
        let max_frames = (self.remaining_bytes / block_align) as usize;
        let frames = frames.min(max_frames);
        let bytes = frames * block_align as usize;
        out.clear();
        out.resize(bytes, 0);
        if bytes > 0 {
            self.reader.read_exact(out)?;
            self.remaining_bytes -= bytes as u64;
        }
        Ok(frames)
    }

    /// Read any leftover bytes (normally none) so callers can detect anomalies.
    pub fn remaining_bytes(&self) -> u64 {
        self.remaining_bytes
    }
}

struct FmtChunk {
    sample_format: SampleFormat,
    channels: u16,
    sample_rate: u32,
    bits_per_sample: u16,
    block_align: u16,
    channel_mask: u32,
}

fn parse_fmt(payload: &[u8]) -> Result<FmtChunk> {
    if payload.len() < 16 {
        return Err(Error::invalid_wav("fmt chunk is too short"));
    }
    let mut tag = u16::from_le_bytes([payload[0], payload[1]]);
    let channels = u16::from_le_bytes([payload[2], payload[3]]);
    let sample_rate = u32::from_le_bytes([payload[4], payload[5], payload[6], payload[7]]);
    let block_align = u16::from_le_bytes([payload[12], payload[13]]);
    let bits_per_sample = u16::from_le_bytes([payload[14], payload[15]]);
    let mut channel_mask = 0u32;
    if tag == FORMAT_EXTENSIBLE {
        if payload.len() < 40 {
            return Err(Error::invalid_wav(
                "WAVE_FORMAT_EXTENSIBLE fmt chunk is too short",
            ));
        }
        let cb_size = u16::from_le_bytes([payload[16], payload[17]]);
        if cb_size < 22 {
            return Err(Error::invalid_wav("invalid extensible fmt extra size"));
        }
        let valid_bits = u16::from_le_bytes([payload[18], payload[19]]);
        channel_mask = u32::from_le_bytes([payload[20], payload[21], payload[22], payload[23]]);
        if payload[28..40] != WAVE_EXTENSIBLE_GUID_TAIL {
            return Err(Error::unsupported(
                "unknown WAVE_FORMAT_EXTENSIBLE subformat GUID",
            ));
        }
        tag = u16::from_le_bytes([payload[24], payload[25]]);
        if valid_bits != 0 && valid_bits != bits_per_sample {
            return Err(Error::unsupported(format!(
                "{bits_per_sample}-bit container with {valid_bits} valid bits"
            )));
        }
    }
    if channels == 0 {
        return Err(Error::invalid_wav("zero channels"));
    }
    let (sample_format, bytes) = match tag {
        FORMAT_PCM => match bits_per_sample {
            16 => (SampleFormat::Int, 2u16),
            24 => (SampleFormat::Int, 3),
            32 => (SampleFormat::Int, 4),
            8 => {
                return Err(Error::unsupported(
                    "8-bit WAV PCM is not supported (use 16, 24 or 32 bits)",
                ))
            }
            other => {
                return Err(Error::unsupported(format!(
                    "{other}-bit integer WAV PCM is not supported"
                )))
            }
        },
        FORMAT_FLOAT => match bits_per_sample {
            32 => (SampleFormat::Float, 4u16),
            64 => (SampleFormat::Float, 8),
            other => {
                return Err(Error::unsupported(format!(
                    "{other}-bit float WAV is not supported"
                )))
            }
        },
        other => {
            return Err(Error::unsupported(format!(
                "WAV format tag 0x{other:04x} is not uncompressed PCM"
            )))
        }
    };
    let expected_align = bytes
        .checked_mul(channels)
        .ok_or_else(|| Error::invalid_wav("channel count overflow"))?;
    if block_align != expected_align {
        return Err(Error::invalid_wav(format!(
            "block alignment {block_align} does not match {channels} channels x {bytes} bytes"
        )));
    }
    Ok(FmtChunk {
        sample_format,
        channels,
        sample_rate,
        bits_per_sample,
        block_align,
        channel_mask,
    })
}

fn parse_info_list(payload: &[u8], metadata: &mut Metadata) -> Result<()> {
    if payload.len() < 4 || &payload[0..4] != b"INFO" {
        return Ok(());
    }
    let mut position = 4usize;
    while position + 8 <= payload.len() {
        let id = &payload[position..position + 4];
        let size = u32::from_le_bytes([
            payload[position + 4],
            payload[position + 5],
            payload[position + 6],
            payload[position + 7],
        ]) as usize;
        position += 8;
        if position + size > payload.len() {
            return Err(Error::invalid_wav("truncated LIST/INFO chunk"));
        }
        let raw = &payload[position..position + size];
        let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
        let value = String::from_utf8_lossy(&raw[..end]).into_owned();
        if !value.is_empty() {
            if let Some(key) = info_id_to_key(id) {
                let _ = metadata.set(key, value);
            }
        }
        position += size + (size & 1);
    }
    Ok(())
}

fn info_id_to_key(id: &[u8]) -> Option<&'static str> {
    match id {
        b"INAM" => Some(TITLE),
        b"IART" => Some(ARTIST),
        b"IPRD" => Some(ALBUM),
        b"ICRD" => Some(DATE),
        b"IGNR" => Some(GENRE),
        b"ICMT" => Some(COMMENT),
        b"ITRK" => Some(TRACKNUMBER),
        b"ISFT" => Some(SOFTWARE),
        _ => None,
    }
}

fn key_to_info_id(key: &str) -> Option<[u8; 4]> {
    let id = match key.to_ascii_uppercase().as_str() {
        TITLE => *b"INAM",
        ARTIST => *b"IART",
        ALBUM => *b"IPRD",
        DATE => *b"ICRD",
        GENRE => *b"IGNR",
        COMMENT => *b"ICMT",
        TRACKNUMBER => *b"ITRK",
        SOFTWARE => *b"ISFT",
        _ => return None,
    };
    Some(id)
}

/// Streaming WAV writer. Call [`WavWriter::finish`] to patch lengths.
pub struct WavWriter<W: Write + Seek> {
    writer: W,
    spec: StreamInfo,
    expected_bytes: u64,
    written_bytes: u64,
    data_size_position: u64,
    riff_size_position: u64,
    data_start: u64,
}

impl<W: Write + Seek> WavWriter<W> {
    pub fn new(mut writer: W, spec: StreamInfo, metadata: &Metadata) -> Result<Self> {
        spec.validate()?;
        let data_bytes = spec
            .total_samples
            .checked_mul(spec.block_align() as u64)
            .ok_or_else(|| Error::unsupported("WAV file would exceed 4 GiB"))?;
        let use_extensible = spec.channels > 2
            || spec.bits_per_sample > 16
            || spec.sample_format == SampleFormat::Float && spec.bits_per_sample == 64
            || spec.channel_mask != 0;
        let fmt_size = if use_extensible { 40u32 } else { 16 };
        let fact_size = if spec.sample_format == SampleFormat::Float {
            12u32
        } else {
            0
        };
        let list = build_info_list(metadata)?;
        let list_total = if list.is_empty() {
            0
        } else {
            8 + list.len() as u64 + ((list.len() as u64) & 1)
        };
        let total = 4u64 // "WAVE"
            + (8 + fmt_size as u64)
            + fact_size as u64
            + list_total
            + 8
            + data_bytes;
        if total > u32::MAX as u64 {
            return Err(Error::unsupported(
                "WAV output would exceed the 4 GiB RIFF limit",
            ));
        }
        writer.seek(SeekFrom::Start(0))?;
        writer.write_all(b"RIFF")?;
        let riff_size_position = 4;
        writer.write_all(&0u32.to_le_bytes())?;
        writer.write_all(b"WAVE")?;

        writer.write_all(b"fmt ")?;
        writer.write_all(&fmt_size.to_le_bytes())?;
        writer.write_all(
            &(if use_extensible {
                FORMAT_EXTENSIBLE
            } else if spec.sample_format == SampleFormat::Float {
                FORMAT_FLOAT
            } else {
                FORMAT_PCM
            })
            .to_le_bytes(),
        )?;
        writer.write_all(&spec.channels.to_le_bytes())?;
        writer.write_all(&spec.sample_rate.to_le_bytes())?;
        let byte_rate = spec.sample_rate as u64 * spec.block_align() as u64;
        if byte_rate > u32::MAX as u64 {
            return Err(Error::unsupported("WAV byte rate exceeds 32 bits"));
        }
        writer.write_all(&(byte_rate as u32).to_le_bytes())?;
        writer.write_all(&(spec.block_align() as u16).to_le_bytes())?;
        writer.write_all(&spec.bits_per_sample.to_le_bytes())?;
        if use_extensible {
            writer.write_all(&22u16.to_le_bytes())?;
            writer.write_all(&spec.bits_per_sample.to_le_bytes())?;
            writer.write_all(&spec.channel_mask.to_le_bytes())?;
            let mut guid = [0u8; 16];
            guid[0..2].copy_from_slice(
                &(if spec.sample_format == SampleFormat::Float {
                    FORMAT_FLOAT
                } else {
                    FORMAT_PCM
                })
                .to_le_bytes(),
            );
            guid[4..16].copy_from_slice(&WAVE_EXTENSIBLE_GUID_TAIL);
            writer.write_all(&guid)?;
        }
        if fact_size > 0 {
            writer.write_all(b"fact")?;
            writer.write_all(&4u32.to_le_bytes())?;
            if spec.total_samples > u32::MAX as u64 {
                return Err(Error::unsupported("WAV sample count exceeds 32 bits"));
            }
            writer.write_all(&(spec.total_samples as u32).to_le_bytes())?;
        }
        if !list.is_empty() {
            writer.write_all(b"LIST")?;
            writer.write_all(&(list.len() as u32).to_le_bytes())?;
            writer.write_all(&list)?;
            if list.len() % 2 == 1 {
                writer.write_all(&[0u8])?;
            }
        }
        writer.write_all(b"data")?;
        let data_size_position = writer.stream_position()?;
        writer.write_all(&(data_bytes as u32).to_le_bytes())?;
        let data_start = writer.stream_position()?;
        Ok(Self {
            writer,
            spec,
            expected_bytes: data_bytes,
            written_bytes: 0,
            data_size_position,
            riff_size_position,
            data_start,
        })
    }

    pub fn spec(&self) -> &StreamInfo {
        &self.spec
    }

    pub fn write_frames(&mut self, bytes: &[u8]) -> Result<()> {
        let align = self.spec.block_align();
        if bytes.len() % align != 0 {
            return Err(Error::argument("PCM block is not a whole number of frames"));
        }
        if self.written_bytes + bytes.len() as u64 > self.expected_bytes {
            return Err(Error::argument("more PCM data written than declared"));
        }
        self.writer.write_all(bytes)?;
        self.written_bytes += bytes.len() as u64;
        Ok(())
    }

    pub fn finish(mut self) -> Result<W> {
        if self.written_bytes != self.expected_bytes {
            return Err(Error::argument(format!(
                "WAV writer received {} bytes, expected {}",
                self.written_bytes, self.expected_bytes
            )));
        }
        let end = self.writer.stream_position()?;
        let riff_size = end - 8;
        if riff_size > u32::MAX as u64 {
            return Err(Error::unsupported("WAV output exceeds 4 GiB"));
        }
        self.writer.seek(SeekFrom::Start(self.riff_size_position))?;
        self.writer.write_all(&(riff_size as u32).to_le_bytes())?;
        self.writer.seek(SeekFrom::Start(self.data_size_position))?;
        self.writer
            .write_all(&(self.expected_bytes as u32).to_le_bytes())?;
        self.writer.seek(SeekFrom::Start(end))?;
        self.writer.flush()?;
        Ok(self.writer)
    }

    pub fn written_bytes(&self) -> u64 {
        self.written_bytes
    }

    pub fn data_start(&self) -> u64 {
        self.data_start
    }
}

fn build_info_list(metadata: &Metadata) -> Result<Vec<u8>> {
    let mut entries: Vec<([u8; 4], &str)> = Vec::new();
    for entry in metadata.entries() {
        if let Some(id) = key_to_info_id(&entry.key) {
            entries.push((id, entry.value.as_str()));
        }
    }
    if entries.is_empty() {
        return Ok(Vec::new());
    }
    let mut list = Vec::new();
    list.extend_from_slice(b"INFO");
    for (id, value) in entries {
        let bytes = value.as_bytes();
        if bytes.len() >= u32::MAX as usize {
            return Err(Error::argument("WAV metadata value is too large"));
        }
        list.extend_from_slice(&id);
        list.extend_from_slice(&((bytes.len() + 1) as u32).to_le_bytes());
        list.extend_from_slice(bytes);
        list.push(0);
        if (bytes.len() + 1) % 2 == 1 {
            list.push(0);
        }
    }
    Ok(list)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn make_spec(format: SampleFormat, bits: u16, channels: u16, frames: u64) -> StreamInfo {
        StreamInfo {
            sample_rate: 44_100,
            channels,
            bits_per_sample: bits,
            sample_format: format,
            channel_mask: 0,
            total_samples: frames,
        }
    }

    #[test]
    fn wav_roundtrip_integer() {
        let spec = make_spec(SampleFormat::Int, 16, 2, 1000);
        let mut meta = Metadata::new();
        meta.set(TITLE, "Test title").unwrap();
        let mut out = Cursor::new(Vec::new());
        let mut writer = WavWriter::new(&mut out, spec, &meta).unwrap();
        let mut pcm = Vec::new();
        for i in 0..1000i32 {
            pcm.extend_from_slice(&((i % 32768) as i16).to_le_bytes());
            pcm.extend_from_slice(&((-(i % 1000)) as i16).to_le_bytes());
        }
        writer.write_frames(&pcm).unwrap();
        writer.finish().unwrap();
        let bytes = out.into_inner();
        let mut reader = WavReader::open(Cursor::new(bytes)).unwrap();
        assert_eq!(reader.info().spec, spec);
        assert_eq!(reader.info().metadata.get(TITLE), Some("Test title"));
        let mut read = Vec::new();
        let frames = reader.read_frames(1000, &mut read).unwrap();
        assert_eq!(frames, 1000);
        assert_eq!(read, pcm);
    }

    #[test]
    fn wav_roundtrip_float_multichannel() {
        let spec = make_spec(SampleFormat::Float, 32, 6, 500);
        let meta = Metadata::new();
        let mut out = Cursor::new(Vec::new());
        let mut writer = WavWriter::new(&mut out, spec, &meta).unwrap();
        let mut pcm = Vec::new();
        for i in 0..500u32 {
            for c in 0..6u32 {
                pcm.extend_from_slice(&(0x3f00_0000u32 + i * 7 + c).to_le_bytes());
            }
        }
        writer.write_frames(&pcm).unwrap();
        writer.finish().unwrap();
        let mut reader = WavReader::open(Cursor::new(out.into_inner())).unwrap();
        assert_eq!(reader.info().spec.channels, 6);
        assert_eq!(reader.info().spec.sample_format, SampleFormat::Float);
        let mut read = Vec::new();
        reader.read_frames(500, &mut read).unwrap();
        assert_eq!(read, pcm);
    }
}
