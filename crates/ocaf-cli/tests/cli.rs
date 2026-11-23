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

#[cfg(unix)]
#[test]
fn play_uses_the_explicit_player_and_removes_the_temp_file() {
    use std::os::unix::fs::PermissionsExt;

    let dir = scratch_dir("play-explicit");
    let wav_path = dir.join("input.wav");
    let original_pcm = write_test_wav(&wav_path);
    let ocaf_path = dir.join("audio.ocaf");
    let status = Command::new(env!("CARGO_BIN_EXE_ocaf"))
        .arg("encode")
        .arg(&wav_path)
        .arg(&ocaf_path)
        .status()
        .unwrap();
    assert!(status.success(), "encode failed");

    let capture = dir.join("captured.wav");
    let player = dir.join("fake-player.sh");
    fs::write(&player, "#!/bin/sh\ncp \"$1\" \"$OCAF_TEST_CAPTURE\"\n").unwrap();
    let mut permissions = fs::metadata(&player).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&player, permissions).unwrap();

    let temp = dir.join("tmp");
    fs::create_dir(&temp).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_ocaf"))
        .arg("play")
        .arg(&ocaf_path)
        .arg("--player")
        .arg(&player)
        .env("TMPDIR", &temp)
        .env("OCAF_TEST_CAPTURE", &capture)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "play failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Playing"));
    assert!(stdout.contains("Finished"));

    let mut reader = WavReader::open(File::open(&capture).unwrap()).unwrap();
    let mut decoded_pcm = Vec::new();
    reader.read_frames(4_800, &mut decoded_pcm).unwrap();
    assert_eq!(decoded_pcm, original_pcm);

    let leftovers: Vec<_> = fs::read_dir(&temp).unwrap().collect();
    assert!(
        leftovers.is_empty(),
        "temporary directory was not cleaned up: {leftovers:?}"
    );
    fs::remove_dir_all(&dir).ok();
}

#[cfg(unix)]
#[test]
fn play_auto_detects_a_player_on_path() {
    use std::os::unix::fs::PermissionsExt;

    let dir = scratch_dir("play-auto");
    let wav_path = dir.join("input.wav");
    let original_pcm = write_test_wav(&wav_path);
    let ocaf_path = dir.join("audio.ocaf");
    let status = Command::new(env!("CARGO_BIN_EXE_ocaf"))
        .arg("encode")
        .arg(&wav_path)
        .arg(&ocaf_path)
        .status()
        .unwrap();
    assert!(status.success(), "encode failed");

    let bin = dir.join("bin");
    fs::create_dir(&bin).unwrap();
    let capture = dir.join("captured.wav");
    let player = bin.join("pw-play");
    fs::write(&player, "#!/bin/sh\ncp \"$1\" \"$OCAF_TEST_CAPTURE\"\n").unwrap();
    let mut permissions = fs::metadata(&player).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&player, permissions).unwrap();

    let temp = dir.join("tmp");
    fs::create_dir(&temp).unwrap();
    let path = format!("{}:/usr/bin:/bin", bin.display());
    let output = Command::new(env!("CARGO_BIN_EXE_ocaf"))
        .arg("play")
        .arg(&ocaf_path)
        .env("PATH", path)
        .env("TMPDIR", &temp)
        .env("OCAF_TEST_CAPTURE", &capture)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "play failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("Player:  pw-play"));

    let mut reader = WavReader::open(File::open(&capture).unwrap()).unwrap();
    let mut decoded_pcm = Vec::new();
    reader.read_frames(4_800, &mut decoded_pcm).unwrap();
    assert_eq!(decoded_pcm, original_pcm);
    fs::remove_dir_all(&dir).ok();
}

#[cfg(unix)]
#[test]
fn play_reports_when_no_player_is_available() {
    let dir = scratch_dir("play-missing");
    let wav_path = dir.join("input.wav");
    write_test_wav(&wav_path);
    let ocaf_path = dir.join("audio.ocaf");
    let status = Command::new(env!("CARGO_BIN_EXE_ocaf"))
        .arg("encode")
        .arg(&wav_path)
        .arg(&ocaf_path)
        .status()
        .unwrap();
    assert!(status.success(), "encode failed");

    let empty = dir.join("empty");
    fs::create_dir(&empty).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_ocaf"))
        .arg("play")
        .arg(&ocaf_path)
        .env("PATH", &empty)
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("no audio player found"), "{stderr}");
    assert!(stderr.contains("--player"), "{stderr}");
    fs::remove_dir_all(&dir).ok();
}
