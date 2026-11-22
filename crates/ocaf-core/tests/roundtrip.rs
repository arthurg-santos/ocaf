//! End-to-end round-trip, corruption and property tests for the public API.

use ocaf_core::crc32::crc32;
use ocaf_core::decoder::{decode_to_wav, probe, verify};
use ocaf_core::encoder::{encode_wav, EncodeOptions};
use ocaf_core::format::{SampleFormat, StreamInfo};
use ocaf_core::metadata::Metadata;
use ocaf_core::pcm;
use ocaf_core::wav::{WavReader, WavWriter};
use std::io::Cursor;

struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed | 1)
    }

    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, limit: u64) -> u64 {
        self.next() % limit
    }
}

fn info(
    sample_rate: u32,
    channels: u16,
    bits: u16,
    format: SampleFormat,
    total_samples: u64,
) -> StreamInfo {
    StreamInfo {
        sample_rate,
        channels,
        bits_per_sample: bits,
        sample_format: format,
        channel_mask: 0,
        total_samples,
    }
}

fn make_wav(spec: StreamInfo, planar: &[Vec<i64>], metadata: &Metadata) -> Vec<u8> {
    let frames = if planar.is_empty() {
        0
    } else {
        planar[0].len()
    };
    let mut output = Cursor::new(Vec::new());
    {
        let mut writer = WavWriter::new(&mut output, spec, metadata).unwrap();
        let mut bytes = Vec::new();
        pcm::interleaved_from_planar(&spec, planar, frames, &mut bytes).unwrap();
        writer.write_frames(&bytes).unwrap();
        writer.finish().unwrap();
    }
    output.into_inner()
}

fn canonical_pcm(wav: &[u8]) -> (StreamInfo, Vec<u8>) {
    let mut reader = WavReader::open(Cursor::new(wav.to_vec())).unwrap();
    let spec = reader.info().spec;
    let mut block = Vec::new();
    let mut all = Vec::new();
    loop {
        let frames = reader.read_frames(1024, &mut block).unwrap();
        if frames == 0 {
            break;
        }
        all.extend_from_slice(&block);
    }
    (spec, all)
}

fn encode(wav: &[u8], options: &EncodeOptions) -> Vec<u8> {
    let mut output = Cursor::new(Vec::new());
    encode_wav(Cursor::new(wav.to_vec()), &mut output, options).unwrap();
    output.into_inner()
}

fn decode(ocaf: &[u8]) -> Vec<u8> {
    let mut output = Cursor::new(Vec::new());
    decode_to_wav(Cursor::new(ocaf.to_vec()), &mut output).unwrap();
    output.into_inner()
}

fn assert_roundtrip(wav: &[u8], options: &EncodeOptions) {
    let (original_spec, original_pcm) = canonical_pcm(wav);
    let ocaf = encode(wav, options);
    let report = verify(Cursor::new(ocaf.clone())).unwrap();
    assert_eq!(report.info, original_spec);
    assert_eq!(report.pcm_bytes, original_pcm.len() as u64);
    let decoded_wav = decode(&ocaf);
    let (decoded_spec, decoded_pcm) = canonical_pcm(&decoded_wav);
    assert_eq!(decoded_spec, original_spec);
    assert_eq!(decoded_pcm, original_pcm);
}

fn signal(kind: u8, frames: usize, channels: usize, bits: u16, seed: u64) -> Vec<Vec<i64>> {
    let limit = 1i64 << (bits - 1);
    let mut rng = Rng::new(seed);
    (0..channels)
        .map(|channel| {
            (0..frames)
                .map(|i| {
                    let t = i as f64;
                    let sample = match kind {
                        0 => 0.0,
                        1 => (rng.next() as i32) as f64 / i32::MAX as f64,
                        2 => {
                            let frequency = 110.0 * (channel as f64 + 1.0);
                            0.7 * (std::f64::consts::TAU * frequency * t / 48_000.0).sin()
                        }
                        3 => {
                            let envelope = 0.3 + 0.7 * (t * 0.0007).sin().abs();
                            envelope
                                * (0.4 * (std::f64::consts::TAU * 220.0 * t / 48_000.0).sin()
                                    + 0.25 * (std::f64::consts::TAU * 331.0 * t / 48_000.0).sin()
                                    + 0.15 * (std::f64::consts::TAU * 997.0 * t / 48_000.0).sin())
                        }
                        _ => {
                            if i % 7 == 0 {
                                -1.0
                            } else if i % 11 == 0 {
                                1.0
                            } else {
                                0.0
                            }
                        }
                    };
                    (sample * (limit - 1) as f64) as i64
                })
                .collect()
        })
        .collect()
}

fn float_signal(frames: usize, channels: usize, bits: u16, seed: u64) -> Vec<Vec<i64>> {
    let mut rng = Rng::new(seed);
    (0..channels)
        .map(|channel| {
            (0..frames)
                .map(|i| {
                    let t = i as f64;
                    let value = if i < 8 {
                        match i {
                            0 => 0.0f64,
                            1 => -0.0,
                            2 => f64::INFINITY,
                            3 => f64::NEG_INFINITY,
                            4 => f64::NAN,
                            _ => (rng.next() as i32) as f64 / 1.0e9,
                        }
                    } else {
                        0.6 * (std::f64::consts::TAU * (220.0 + channel as f64 * 55.0) * t
                            / 48_000.0)
                            .sin()
                    };
                    if bits == 32 {
                        pcm::float32_bits_to_key((value as f32).to_bits())
                    } else {
                        pcm::float64_bits_to_key(value.to_bits())
                    }
                })
                .collect()
        })
        .collect()
}

#[test]
fn integer_formats_rates_channels_roundtrip() {
    let mut options = EncodeOptions {
        block_size: 512,
        max_lpc_order: 8,
        max_partition_order: 4,
        ..EncodeOptions::default()
    };
    for &bits in &[16u16, 24, 32] {
        for &rate in &[8_000u32, 44_100, 96_000] {
            for &channels in &[1u16, 2, 6] {
                let frames = 1_300;
                let planar = signal(
                    3,
                    frames,
                    channels as usize,
                    bits,
                    rate as u64 + bits as u64,
                );
                let spec = info(rate, channels, bits, SampleFormat::Int, frames as u64);
                let wav = make_wav(spec, &planar, &Metadata::new());
                options.max_lpc_order = if channels > 2 { 6 } else { 8 };
                assert_roundtrip(&wav, &options);
            }
        }
    }
}

#[test]
fn float_formats_rates_channels_roundtrip() {
    let options = EncodeOptions {
        block_size: 512,
        max_lpc_order: 8,
        max_partition_order: 4,
        ..EncodeOptions::default()
    };
    for &bits in &[32u16, 64] {
        for &rate in &[44_100u32, 48_000] {
            for &channels in &[1u16, 2, 6] {
                let frames = 700;
                let planar = float_signal(frames, channels as usize, bits, rate as u64);
                let spec = info(rate, channels, bits, SampleFormat::Float, frames as u64);
                let wav = make_wav(spec, &planar, &Metadata::new());
                assert_roundtrip(&wav, &options);
            }
        }
    }
}

#[test]
fn silence_noise_tones_music_and_extremes() {
    let options = EncodeOptions {
        block_size: 400,
        max_lpc_order: 12,
        max_partition_order: 6,
        ..EncodeOptions::default()
    };
    for kind in 0..=4u8 {
        for &(bits, format) in &[
            (16u16, SampleFormat::Int),
            (24, SampleFormat::Int),
            (32, SampleFormat::Int),
            (32, SampleFormat::Float),
            (64, SampleFormat::Float),
        ] {
            let frames = 1_234;
            let planar = if format == SampleFormat::Float {
                float_signal(frames, 2, bits, kind as u64 + 99)
            } else {
                signal(kind, frames, 2, bits, kind as u64 + 1234)
            };
            let spec = info(48_000, 2, bits, format, frames as u64);
            let wav = make_wav(spec, &planar, &Metadata::new());
            assert_roundtrip(&wav, &options);
        }
    }
}

#[test]
fn tiny_and_incomplete_blocks_roundtrip() {
    for &frames in &[0usize, 1, 2, 3, 7, 17, 31, 127, 1_111] {
        for &block in &[1u16, 2, 4, 16, 64, 512, 4096] {
            let planar = signal(2, frames.max(1), 1, 16, frames as u64 + block as u64);
            let planar = if frames == 0 {
                vec![Vec::new()]
            } else {
                planar
            };
            let spec = info(44_100, 1, 16, SampleFormat::Int, frames as u64);
            let wav = make_wav(spec, &planar, &Metadata::new());
            let options = EncodeOptions {
                block_size: block,
                max_lpc_order: 8,
                max_partition_order: 4,
                ..EncodeOptions::default()
            };
            assert_roundtrip(&wav, &options);
        }
    }
}

#[test]
fn long_stream_roundtrip() {
    let frames = 120_000;
    let planar = signal(3, frames, 2, 16, 42);
    let spec = info(48_000, 2, 16, SampleFormat::Int, frames as u64);
    let wav = make_wav(spec, &planar, &Metadata::new());
    let options = EncodeOptions {
        block_size: 4096,
        max_lpc_order: 10,
        max_partition_order: 6,
        ..EncodeOptions::default()
    };
    assert_roundtrip(&wav, &options);
}

#[test]
fn metadata_and_probe_roundtrip() {
    let frames = 333;
    let planar = signal(2, frames, 2, 24, 7);
    let spec = info(96_000, 2, 24, SampleFormat::Int, frames as u64);
    let mut metadata = Metadata::new();
    metadata.set("TITLE", "Roundtrip").unwrap();
    metadata.set("ARTIST", "OCAF tests").unwrap();
    let wav = make_wav(spec, &planar, &metadata);
    let mut options = EncodeOptions::default();
    options.metadata.set("CUSTOM_FIELD", "válue ✓").unwrap();
    let ocaf = encode(&wav, &options);
    let probed = probe(Cursor::new(ocaf.clone())).unwrap();
    assert_eq!(probed.header.info, spec);
    assert_eq!(probed.metadata.get("TITLE"), Some("Roundtrip"));
    assert_eq!(probed.metadata.get("ARTIST"), Some("OCAF tests"));
    assert_eq!(probed.metadata.get("CUSTOM_FIELD"), Some("válue ✓"));
    assert!(probed.metadata.get("ENCODER").is_some());
    let (_, original_pcm) = canonical_pcm(&wav);
    let decoded = decode(&ocaf);
    let (_, decoded_pcm) = canonical_pcm(&decoded);
    assert_eq!(decoded_pcm, original_pcm);
    let decoded_metadata = WavReader::open(Cursor::new(decoded))
        .unwrap()
        .info()
        .metadata
        .clone();
    assert_eq!(decoded_metadata.get("TITLE"), Some("Roundtrip"));
}

#[test]
fn corruption_is_detected_everywhere() {
    let frames = 600;
    let planar = signal(3, frames, 2, 24, 99);
    let spec = info(48_000, 2, 24, SampleFormat::Int, frames as u64);
    let wav = make_wav(spec, &planar, &Metadata::new());
    let ocaf = encode(&wav, &EncodeOptions::default());
    let probed = probe(Cursor::new(ocaf.clone())).unwrap();
    let metadata_len = probed.header.metadata_len as usize;
    let audio_offset = probed.header.audio_offset as usize;

    let mut cases = Vec::new();
    let mut bad_magic = ocaf.clone();
    bad_magic[0] ^= 0xff;
    cases.push(("magic", bad_magic));
    let mut bad_header_crc = ocaf.clone();
    bad_header_crc[124] ^= 0xff;
    cases.push(("header crc", bad_header_crc));
    let mut bad_metadata = ocaf.clone();
    bad_metadata[128 + metadata_len / 2] ^= 0xff;
    cases.push(("metadata", bad_metadata));
    let mut bad_frame_header = ocaf.clone();
    bad_frame_header[audio_offset + 10] ^= 0xff;
    cases.push(("frame header", bad_frame_header));
    let mut bad_payload = ocaf.clone();
    bad_payload[audio_offset + 48] ^= 0xff;
    cases.push(("frame payload", bad_payload));
    let mut bad_frame_crc = ocaf.clone();
    let last = bad_frame_crc.len() - 1;
    bad_frame_crc[last] ^= 0xff;
    cases.push(("frame crc", bad_frame_crc));
    let mut trailing = ocaf.clone();
    trailing.push(0);
    cases.push(("trailing byte", trailing));
    cases.push(("truncated", ocaf[..ocaf.len() - 1].to_vec()));

    for (name, bytes) in cases {
        assert!(
            verify(Cursor::new(bytes)).is_err(),
            "corruption {name} was not detected"
        );
    }
}

#[test]
fn whole_stream_crc_is_checked() {
    let frames = 300;
    let planar = signal(3, frames, 1, 16, 5);
    let spec = info(44_100, 1, 16, SampleFormat::Int, frames as u64);
    let wav = make_wav(spec, &planar, &Metadata::new());
    let mut ocaf = encode(&wav, &EncodeOptions::default());
    ocaf[64] ^= 0xff;
    let crc = crc32(&ocaf[..124]);
    ocaf[124..128].copy_from_slice(&crc.to_le_bytes());
    assert!(verify(Cursor::new(ocaf)).is_err());
}

#[test]
fn version_compatibility() {
    let frames = 200;
    let planar = signal(2, frames, 1, 16, 11);
    let spec = info(48_000, 1, 16, SampleFormat::Int, frames as u64);
    let wav = make_wav(spec, &planar, &Metadata::new());
    let ocaf = encode(&wav, &EncodeOptions::default());

    let mut future_minor = ocaf.clone();
    future_minor[6..8].copy_from_slice(&1u16.to_le_bytes());
    let crc = crc32(&future_minor[..124]);
    future_minor[124..128].copy_from_slice(&crc.to_le_bytes());
    assert!(verify(Cursor::new(future_minor)).is_ok());

    let mut future_major = ocaf.clone();
    future_major[4..6].copy_from_slice(&2u16.to_le_bytes());
    let crc = crc32(&future_major[..124]);
    future_major[124..128].copy_from_slice(&crc.to_le_bytes());
    assert!(verify(Cursor::new(future_major)).is_err());
}

#[test]
fn property_random_roundtrips() {
    let mut rng = Rng::new(0x5eed_1234_abcd_ef01);
    for _ in 0..20 {
        let format = if rng.below(3) == 0 {
            SampleFormat::Float
        } else {
            SampleFormat::Int
        };
        let bits = match (format, rng.below(3)) {
            (SampleFormat::Int, 0) => 16,
            (SampleFormat::Int, 1) => 24,
            (SampleFormat::Int, _) => 32,
            (SampleFormat::Float, 0) => 32,
            (SampleFormat::Float, _) => 64,
        };
        let channels = 1 + rng.below(6) as u16;
        let rate = 8_000 + rng.below(100_000) as u32;
        let frames = rng.below(2_000) as usize;
        let mut planar = Vec::new();
        for _ in 0..channels {
            let mut channel = Vec::with_capacity(frames);
            for _ in 0..frames {
                channel.push(if format == SampleFormat::Float {
                    if bits == 32 {
                        pcm::float32_bits_to_key(rng.next() as u32)
                    } else {
                        pcm::float64_bits_to_key(rng.next())
                    }
                } else {
                    let raw = rng.next();
                    match bits {
                        16 => (raw as i16) as i64,
                        24 => ((raw << 40) as i64) >> 40,
                        _ => (raw as i32) as i64,
                    }
                });
            }
            planar.push(channel);
        }
        let spec = info(rate, channels, bits, format, frames as u64);
        let wav = make_wav(spec, &planar, &Metadata::new());
        let options = EncodeOptions {
            block_size: [1, 3, 16, 256, 1024][rng.below(5) as usize],
            max_lpc_order: rng.below(17) as u8,
            max_partition_order: rng.below(7) as u8,
            ..EncodeOptions::default()
        };
        assert_roundtrip(&wav, &options);
    }
}
