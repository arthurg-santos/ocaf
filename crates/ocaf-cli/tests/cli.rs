use ocaf_core::wav::{WavReader, WavWriter};
use ocaf_core::{Metadata, SampleFormat, StreamInfo};
use std::fs::{self, File};
use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn scratch_dir(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path =
        std::env::temp_dir().join(format!("ocaf-cli-{label}-{}-{nanos}", std::process::id()));
    fs::create_dir_all(&path).unwrap();
    path
}

fn write_test_wav(path: &std::path::Path) -> Vec<u8> {
    let spec = StreamInfo {
        sample_rate: 48_000,
        channels: 2,
        bits_per_sample: 16,
        sample_format: SampleFormat::Int,
        channel_mask: 3,
        total_samples: 4_800,
    };
    let mut pcm = Vec::with_capacity(4_800 * 4);
    for frame in 0..4_800i32 {
        let sample = ((frame as f64 * 0.05).sin() * 20_000.0) as i16;
        pcm.extend_from_slice(&sample.to_le_bytes());
        pcm.extend_from_slice(&(sample / 2).to_le_bytes());
    }
    let file = File::create(path).unwrap();
    let mut writer = WavWriter::new(file, spec, &Metadata::new()).unwrap();
    writer.write_frames(&pcm).unwrap();
    writer.finish().unwrap();
    pcm
}

#[test]
fn relative_output_paths_use_the_current_directory() {
    let dir = scratch_dir("relative");
    let wav_path = dir.join("input.wav");
    let original_pcm = write_test_wav(&wav_path);

    let status = Command::new(env!("CARGO_BIN_EXE_ocaf"))
        .current_dir(&dir)
        .args(["encode", "input.wav", "output.ocaf"])
        .status()
        .unwrap();
    assert!(status.success(), "relative encode failed");
    assert!(dir.join("output.ocaf").is_file());

    let status = Command::new(env!("CARGO_BIN_EXE_ocaf"))
        .current_dir(&dir)
        .args(["decode", "output.ocaf", "decoded.wav"])
        .status()
        .unwrap();
    assert!(status.success(), "relative decode failed");

    let status = Command::new(env!("CARGO_BIN_EXE_ocaf"))
        .current_dir(&dir)
        .args(["verify", "output.ocaf"])
        .status()
        .unwrap();
    assert!(status.success(), "verify failed");

    let mut reader = WavReader::open(File::open(dir.join("decoded.wav")).unwrap()).unwrap();
    let mut decoded_pcm = Vec::new();
    reader.read_frames(4_800, &mut decoded_pcm).unwrap();
    assert_eq!(decoded_pcm, original_pcm);

    fs::remove_dir_all(&dir).ok();
}
