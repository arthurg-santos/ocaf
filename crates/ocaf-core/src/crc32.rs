//! CRC-32 (IEEE 802.3, reflected polynomial 0xEDB88320).
//!
//! OCAF uses CRC-32 on the container header, metadata chunks, frames and on
//! canonical PCM data. CRC-32 is used for corruption detection, not as a
//! cryptographic authenticator.

const POLY: u32 = 0xEDB8_8320;

const fn make_table() -> [[u32; 256]; 8] {
    let mut tables = [[0u32; 256]; 8];
    let mut i = 0usize;
    while i < 256 {
        let mut crc = i as u32;
        let mut bit = 0;
        while bit < 8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ POLY
            } else {
                crc >> 1
            };
            bit += 1;
        }
        tables[0][i] = crc;
        i += 1;
    }
    let mut t = 1usize;
    while t < 8 {
        let mut i = 0usize;
        while i < 256 {
            let prev = tables[t - 1][i];
            tables[t][i] = (prev >> 8) ^ tables[0][(prev & 0xff) as usize];
            i += 1;
        }
        t += 1;
    }
    tables
}

static TABLES: [[u32; 256]; 8] = make_table();

/// Incremental CRC-32 calculator.
#[derive(Clone, Copy, Debug)]
pub struct Crc32 {
    state: u32,
}

impl Crc32 {
    pub const fn new() -> Self {
        Self { state: 0xFFFF_FFFF }
    }

    pub fn update(&mut self, mut data: &[u8]) {
        let mut crc = self.state;
        while data.len() >= 8 {
            let first = u32::from_le_bytes([data[0], data[1], data[2], data[3]]) ^ crc;
            crc = TABLES[7][(first & 0xff) as usize]
                ^ TABLES[6][((first >> 8) & 0xff) as usize]
                ^ TABLES[5][((first >> 16) & 0xff) as usize]
                ^ TABLES[4][((first >> 24) & 0xff) as usize]
                ^ TABLES[3][data[4] as usize]
                ^ TABLES[2][data[5] as usize]
                ^ TABLES[1][data[6] as usize]
                ^ TABLES[0][data[7] as usize];
            data = &data[8..];
        }
        for &byte in data {
            crc = (crc >> 8) ^ TABLES[0][((crc ^ byte as u32) & 0xff) as usize];
        }
        self.state = crc;
    }

    pub const fn finalize(self) -> u32 {
        !self.state
    }
}

impl Default for Crc32 {
    fn default() -> Self {
        Self::new()
    }
}

/// One-shot CRC-32.
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = Crc32::new();
    crc.update(data);
    crc.finalize()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_vectors() {
        assert_eq!(crc32(b""), 0);
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(
            crc32(b"The quick brown fox jumps over the lazy dog"),
            0x414F_A339
        );
    }

    #[test]
    fn incremental_matches_one_shot() {
        let data: Vec<u8> = (0..=255).cycle().take(1001).collect();
        let mut c = Crc32::new();
        for chunk in data.chunks(7) {
            c.update(chunk);
        }
        assert_eq!(c.finalize(), crc32(&data));
    }
}
