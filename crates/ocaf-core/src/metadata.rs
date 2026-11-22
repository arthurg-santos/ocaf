//! Extensible metadata chunks.
//!
//! A metadata section is a sequence of chunks. Unknown chunk types are skipped
//! by readers, which allows future revisions to add metadata without breaking
//! existing decoders. Text values are UTF-8 key/value pairs carried in `INFO`
//! chunks.

use crate::crc32::crc32;
use crate::error::{Error, Result};
use crate::format::{get_u32, MAX_METADATA_BYTES, MAX_METADATA_ENTRIES};

pub const CHUNK_INFO: [u8; 4] = *b"INFO";
pub const CHUNK_PAD: [u8; 4] = *b"PAD ";
const CHUNK_HEADER_SIZE: usize = 16;

pub const TITLE: &str = "TITLE";
pub const ARTIST: &str = "ARTIST";
pub const ALBUM: &str = "ALBUM";
pub const DATE: &str = "DATE";
pub const GENRE: &str = "GENRE";
pub const COMMENT: &str = "COMMENT";
pub const TRACKNUMBER: &str = "TRACKNUMBER";
pub const SOFTWARE: &str = "SOFTWARE";
pub const ENCODER: &str = "ENCODER";

/// One textual metadata field.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MetadataEntry {
    pub key: String,
    pub value: String,
}

impl MetadataEntry {
    pub fn new(key: impl Into<String>, value: impl Into<String>) -> Result<Self> {
        let key = key.into();
        let value = value.into();
        validate_key(&key)?;
        if value.len() > 16 * 1024 * 1024 {
            return Err(Error::argument("metadata value is too large"));
        }
        Ok(Self { key, value })
    }
}

fn validate_key(key: &str) -> Result<()> {
    if key.is_empty() || key.len() > 64 {
        return Err(Error::argument("metadata key must have 1 to 64 characters"));
    }
    if !key
        .bytes()
        .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
    {
        return Err(Error::argument(format!(
            "metadata key {key:?} may only contain A-Z, 0-9, '_' and '-'"
        )));
    }
    Ok(())
}

/// Serialize text entries into an OCAF metadata section.
pub fn encode_metadata(entries: &[MetadataEntry]) -> Result<Vec<u8>> {
    if entries.len() > MAX_METADATA_ENTRIES {
        return Err(Error::argument("too many metadata entries"));
    }
    let mut out = Vec::new();
    for entry in entries {
        validate_key(&entry.key)?;
        let key = entry.key.as_bytes();
        let value = entry.value.as_bytes();
        let payload_len = 2usize
            .checked_add(key.len())
            .and_then(|n| n.checked_add(value.len()))
            .ok_or_else(|| Error::argument("metadata entry is too large"))?;
        if payload_len > u32::MAX as usize {
            return Err(Error::argument("metadata entry is too large"));
        }
        out.extend_from_slice(&CHUNK_INFO);
        out.extend_from_slice(&(payload_len as u32).to_le_bytes());
        let mut crc_input = Vec::with_capacity(payload_len);
        crc_input.extend_from_slice(&(key.len() as u16).to_le_bytes());
        crc_input.extend_from_slice(key);
        crc_input.extend_from_slice(value);
        out.extend_from_slice(&crc32(&crc_input).to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes()); // flags
        out.extend_from_slice(&crc_input);
        if out.len() as u64 > MAX_METADATA_BYTES {
            return Err(Error::argument("metadata section is too large"));
        }
    }
    Ok(out)
}

/// Parse a metadata section, skipping chunk kinds this version does not know.
pub fn decode_metadata(mut data: &[u8]) -> Result<Vec<MetadataEntry>> {
    if data.len() as u64 > MAX_METADATA_BYTES {
        return Err(Error::limit("metadata section is too large"));
    }
    let mut entries = Vec::new();
    while !data.is_empty() {
        if data.len() < CHUNK_HEADER_SIZE {
            return Err(Error::invalid_ocaf("truncated metadata chunk header"));
        }
        let id = &data[0..4];
        let length = get_u32(data, 4) as usize;
        let expected_crc = get_u32(data, 8);
        let _flags = get_u32(data, 12);
        if data.len() < CHUNK_HEADER_SIZE + length {
            return Err(Error::invalid_ocaf("metadata chunk exceeds section"));
        }
        let payload = &data[CHUNK_HEADER_SIZE..CHUNK_HEADER_SIZE + length];
        if crc32(payload) != expected_crc {
            return Err(Error::corrupt("metadata chunk CRC-32 mismatch"));
        }
        if id == CHUNK_INFO {
            if payload.len() < 2 {
                return Err(Error::invalid_ocaf("truncated INFO chunk"));
            }
            let key_len = u16::from_le_bytes([payload[0], payload[1]]) as usize;
            if key_len == 0 || key_len > 64 || payload.len() < 2 + key_len {
                return Err(Error::invalid_ocaf("invalid INFO key length"));
            }
            let key = std::str::from_utf8(&payload[2..2 + key_len])
                .map_err(|_| Error::invalid_ocaf("metadata key is not valid UTF-8"))?
                .to_string();
            validate_key(&key).map_err(|_| Error::invalid_ocaf("invalid metadata key"))?;
            let value = std::str::from_utf8(&payload[2 + key_len..])
                .map_err(|_| Error::invalid_ocaf("metadata value is not valid UTF-8"))?
                .to_string();
            if entries.len() >= MAX_METADATA_ENTRIES {
                return Err(Error::limit("too many metadata entries"));
            }
            entries.push(MetadataEntry { key, value });
        }
        // PAD and unknown chunks are intentionally ignored.
        data = &data[CHUNK_HEADER_SIZE + length..];
    }
    Ok(entries)
}

/// Small helper around a list of metadata entries.
#[derive(Clone, Debug, Default)]
pub struct Metadata {
    entries: Vec<MetadataEntry>,
}

impl Metadata {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_entries(entries: Vec<MetadataEntry>) -> Self {
        Self { entries }
    }

    pub fn entries(&self) -> &[MetadataEntry] {
        &self.entries
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.entries
            .iter()
            .find(|e| e.key.eq_ignore_ascii_case(key))
            .map(|e| e.value.as_str())
    }

    pub fn set(&mut self, key: impl Into<String>, value: impl Into<String>) -> Result<()> {
        let mut key = key.into().to_ascii_uppercase();
        if key.len() > 64 {
            key.truncate(64);
        }
        let entry = MetadataEntry::new(key, value)?;
        if let Some(existing) = self
            .entries
            .iter_mut()
            .find(|e| e.key.eq_ignore_ascii_case(&entry.key))
        {
            *existing = entry;
        } else {
            if self.entries.len() >= MAX_METADATA_ENTRIES {
                return Err(Error::argument("too many metadata entries"));
            }
            self.entries.push(entry);
        }
        Ok(())
    }

    pub fn remove(&mut self, key: &str) -> Option<MetadataEntry> {
        let position = self
            .entries
            .iter()
            .position(|e| e.key.eq_ignore_ascii_case(key))?;
        Some(self.entries.remove(position))
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        encode_metadata(&self.entries)
    }

    pub fn from_bytes(data: &[u8]) -> Result<Self> {
        Ok(Self::from_entries(decode_metadata(data)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_roundtrip() {
        let mut meta = Metadata::new();
        meta.set(TITLE, "Example").unwrap();
        meta.set(ARTIST, "Someone").unwrap();
        meta.set(COMMENT, "ação, acentuação e 日本語").unwrap();
        let bytes = meta.to_bytes().unwrap();
        let decoded = Metadata::from_bytes(&bytes).unwrap();
        assert_eq!(decoded.get(TITLE), Some("Example"));
        assert_eq!(decoded.get(ARTIST), Some("Someone"));
        assert_eq!(decoded.get(COMMENT), Some("ação, acentuação e 日本語"));
    }

    #[test]
    fn corrupted_metadata_detected() {
        let mut meta = Metadata::new();
        meta.set(TITLE, "Example").unwrap();
        let mut bytes = meta.to_bytes().unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 0xff;
        assert!(Metadata::from_bytes(&bytes).is_err());
    }

    #[test]
    fn unknown_chunks_are_skipped() {
        let mut data = Vec::new();
        data.extend_from_slice(b"FUTR");
        data.extend_from_slice(&3u32.to_le_bytes());
        data.extend_from_slice(&crc32(b"abc").to_le_bytes());
        data.extend_from_slice(&0u32.to_le_bytes());
        data.extend_from_slice(b"abc");
        let entries = decode_metadata(&data).unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn invalid_key_rejected() {
        assert!(MetadataEntry::new("lower", "x").is_err());
        assert!(MetadataEntry::new("", "x").is_err());
        assert!(MetadataEntry::new("BAD KEY", "x").is_err());
    }
}
