//! Implementations of every `ocaf` subcommand.

use crate::format::{bytes, channel_layout, duration, number, throughput};
use ocaf_core::wav::WavReader;
use ocaf_core::{decode_to_wav, encode_wav, probe, verify, EncodeOptions, StereoMode};
use std::env;
use std::ffi::OsString;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

type CmdResult<T> = Result<T, String>;

fn err<E: std::fmt::Display>(error: E) -> String {
    error.to_string()
}

struct EncodeArgs {
    input: PathBuf,
    output: PathBuf,
    options: EncodeOptions,
    force: bool,
}

struct DecodeArgs {
    input: PathBuf,
    output: PathBuf,
    force: bool,
}

struct BenchmarkArgs {
    input: PathBuf,
    iterations: u32,
}

pub fn encode(args: &[OsString]) -> CmdResult<()> {
    let parsed = parse_encode_args(args)?;
    let started = Instant::now();
    let options = parsed.options.clone();
    options.validate().map_err(err)?;
    let summary = write_atomically(&parsed.output, parsed.force, |file| {
        let input = File::open(&parsed.input)
            .map_err(|e| format!("cannot open {}: {e}", parsed.input.display()))?;
        encode_wav(input, &mut *file, &options).map_err(err)
    })?;
    let elapsed = started.elapsed().as_secs_f64();
    println!(
        "Encoded {} -> {}",
        parsed.input.display(),
        parsed.output.display()
    );
    println!(
        "  Audio:   {} Hz, {}, {}-bit {}",
        summary.info.sample_rate,
        channel_layout(summary.info.channels, summary.info.channel_mask),
        summary.info.bits_per_sample,
        summary.info.sample_format.name()
    );
    println!(
        "  Samples: {} ({})",
        number(summary.info.total_samples),
        duration(summary.info.duration_seconds())
    );
    println!(
        "  Sizes:   {} PCM -> {} OCAF ({})",
        bytes(summary.pcm_bytes),
        bytes(summary.ocaf_bytes),
        ratio_text(summary.pcm_bytes, summary.ocaf_bytes)
    );
    println!(
        "  Frames:  {} (block size {})",
        number(summary.frame_count),
        summary.max_block_size
    );
    println!(
        "  Speed:   {} ({:.3} s)",
        throughput(summary.pcm_bytes, elapsed),
        elapsed
    );
    Ok(())
}

pub fn decode(args: &[OsString]) -> CmdResult<()> {
    let parsed = parse_decode_args(args)?;
    let started = Instant::now();
    let summary = write_atomically(&parsed.output, parsed.force, |file| {
        let input = File::open(&parsed.input)
            .map_err(|e| format!("cannot open {}: {e}", parsed.input.display()))?;
        decode_to_wav(input, &mut *file).map_err(err)
    })?;
    let elapsed = started.elapsed().as_secs_f64();
    println!(
        "Decoded {} -> {}",
        parsed.input.display(),
        parsed.output.display()
    );
    println!(
        "  Audio:   {} Hz, {}, {}-bit {}",
        summary.info.sample_rate,
        channel_layout(summary.info.channels, summary.info.channel_mask),
        summary.info.bits_per_sample,
        summary.info.sample_format.name()
    );
    println!(
        "  Samples: {} ({})",
        number(summary.info.total_samples),
        duration(summary.info.duration_seconds())
    );
    println!(
        "  Sizes:   {} OCAF -> {} PCM",
        bytes(summary.ocaf_bytes),
        bytes(summary.pcm_bytes)
    );
    println!(
        "  Speed:   {} ({:.3} s), all checksums verified",
        throughput(summary.pcm_bytes, elapsed),
        elapsed
    );
    Ok(())
}

pub fn info(args: &[OsString]) -> CmdResult<()> {
    let (input, _force) = parse_single_path(args, "info")?;
    let file = File::open(&input).map_err(|e| format!("cannot open {}: {e}", input.display()))?;
    let probe = probe(file).map_err(err)?;
    if probe.file_len < probe.header.audio_offset {
        return Err(format!(
            "{} is truncated: audio starts at byte {} but the file has only {} bytes",
            input.display(),
            probe.header.audio_offset,
            probe.file_len
        ));
    }
    let info = probe.header.info;
    let ratio = if probe.file_len > 0 {
        probe.pcm_bytes as f64 / probe.file_len as f64
    } else {
        0.0
    };
    println!("File:    {}", input.display());
    println!("Size:    {}", bytes(probe.file_len));
    println!(
        "Format:  OCAF 1.{} (codec {}: predictive lossless)",
        probe.header.minor_version, probe.header.codec
    );
    println!(
        "Audio:   {} Hz, {}, {}-bit {}",
        info.sample_rate,
        channel_layout(info.channels, info.channel_mask),
        info.bits_per_sample,
        info.sample_format.name()
    );
    if info.channel_mask != 0 {
        println!("Mask:    0x{:08x}", info.channel_mask);
    }
    println!(
        "Samples: {} ({})",
        number(info.total_samples),
        duration(info.duration_seconds())
    );
    println!(
        "Frames:  {} (blocks {}{})",
        number(probe.header.frame_count),
        probe.header.min_block_size,
        if probe.header.max_block_size != probe.header.min_block_size {
            format!("..{}", probe.header.max_block_size)
        } else {
            String::new()
        }
    );
    println!("PCM:     {} ({:.2}:1)", bytes(probe.pcm_bytes), ratio);
    println!(
        "Metadata: {} bytes, {} entr{}",
        number(probe.header.metadata_len),
        probe.metadata.entries().len(),
        if probe.metadata.entries().len() == 1 {
            "y"
        } else {
            "ies"
        }
    );
    for entry in probe.metadata.entries() {
        println!("  {} = {}", entry.key, entry.value);
    }
    println!("CRC-32:  0x{:08x} (PCM)", probe.header.pcm_crc32);
    println!("Header:  OK (container and metadata checksums)");
    println!("Note:    run `ocaf verify` to decode and validate every frame.");
    Ok(())
}

pub fn verify_cmd(args: &[OsString]) -> CmdResult<()> {
    let (input, _force) = parse_single_path(args, "verify")?;
    let file = File::open(&input).map_err(|e| format!("cannot open {}: {e}", input.display()))?;
    let started = Instant::now();
    let report = verify(file).map_err(err)?;
    let elapsed = started.elapsed().as_secs_f64();
    println!("OK: {}", input.display());
    println!(
        "  {} samples, {} frames, {}",
        number(report.info.total_samples),
        number(report.frame_count),
        bytes(report.pcm_bytes)
    );
    println!(
        "  PCM CRC-32 0x{:08x} matched; all frame and metadata checksums valid",
        report.pcm_crc32
    );
    println!(
        "  Decoded in {:.3} s ({})",
        elapsed,
        throughput(report.pcm_bytes, elapsed)
    );
    Ok(())
}

pub fn compare(args: &[OsString]) -> CmdResult<()> {
    if args.len() != 2 {
        return Err("compare expects a WAV file and an OCAF file".to_string());
    }
    let wav_path = PathBuf::from(&args[0]);
    let ocaf_path = PathBuf::from(&args[1]);
    let wav_file =
        File::open(&wav_path).map_err(|e| format!("cannot open {}: {e}", wav_path.display()))?;
    let wav_reader = WavReader::open(wav_file).map_err(err)?;
    let wav_info = wav_reader.info().clone();
    let ocaf_file =
        File::open(&ocaf_path).map_err(|e| format!("cannot open {}: {e}", ocaf_path.display()))?;
    let ocaf = probe(ocaf_file).map_err(err)?;
    let wav_size = fs::metadata(&wav_path).map_err(err)?.len();
    let ocaf_size = fs::metadata(&ocaf_path).map_err(err)?.len();
    let same_audio = wav_info.spec == ocaf.header.info;
    println!(
        "WAV:   {} ({}, PCM {})",
        wav_path.display(),
        bytes(wav_size),
        bytes(wav_info.data_len)
    );
    println!(
        "OCAF:  {} ({}, audio {} + container overhead {})",
        ocaf_path.display(),
        bytes(ocaf_size),
        bytes(ocaf_size.saturating_sub(ocaf.header.audio_offset)),
        bytes(ocaf.header.audio_offset)
    );
    println!(
        "Audio: {} Hz, {}, {}-bit {}",
        wav_info.spec.sample_rate,
        channel_layout(wav_info.spec.channels, wav_info.spec.channel_mask),
        wav_info.spec.bits_per_sample,
        wav_info.spec.sample_format.name()
    );
    println!("PCM:   {} original", bytes(wav_info.data_len));
    println!(
        "Ratio: {} of PCM size ({:.2}:1 against the WAV file)",
        ratio_text(wav_info.data_len, ocaf_size),
        wav_size as f64 / ocaf_size as f64
    );
    println!(
        "Check: audio parameters {}",
        if same_audio {
            "match"
        } else {
            "DIFFER (this pair may not belong together)"
        }
    );
    Ok(())
}

pub fn benchmark(args: &[OsString]) -> CmdResult<()> {
    let parsed = parse_benchmark_args(args)?;
    if parsed.iterations == 0 {
        return Err("iterations must be at least 1".to_string());
    }
    let input = File::open(&parsed.input)
        .map_err(|e| format!("cannot open {}: {e}", parsed.input.display()))?;
    let wav_info = WavReader::open(input).map_err(err)?.info().clone();
    let pcm_bytes = wav_info.data_len;
    if pcm_bytes == 0 {
        return Err("cannot benchmark an empty WAV file".to_string());
    }
    let ocaf_temp = TempPath::new("bench", "ocaf");
    let wav_temp = TempPath::new("bench", "wav");
    let options = EncodeOptions::default();

    let mut encode_times = Vec::new();
    for _ in 0..parsed.iterations {
        let started = Instant::now();
        let output = File::create(&ocaf_temp.0).map_err(err)?;
        let input = File::open(&parsed.input).map_err(err)?;
        encode_wav(input, output, &options).map_err(err)?;
        encode_times.push(started.elapsed().as_secs_f64());
    }
    let ocaf_size = fs::metadata(&ocaf_temp.0).map_err(err)?.len();

    let mut decode_times = Vec::new();
    for _ in 0..parsed.iterations {
        let started = Instant::now();
        let output = File::create(&wav_temp.0).map_err(err)?;
        let input = File::open(&ocaf_temp.0).map_err(err)?;
        decode_to_wav(input, output).map_err(err)?;
        decode_times.push(started.elapsed().as_secs_f64());
    }

    let verify_started = Instant::now();
    let report = verify(File::open(&ocaf_temp.0).map_err(err)?).map_err(err)?;
    let verify_time = verify_started.elapsed().as_secs_f64();
    let bit_perfect = wav_pcm_equal(&parsed.input, &wav_temp.0)?;
    if !bit_perfect {
        return Err("bit-perfect comparison failed after benchmark".to_string());
    }

    let encode_avg = average(&encode_times);
    let decode_avg = average(&decode_times);
    let encode_best = best(&encode_times);
    let decode_best = best(&decode_times);

    println!("Benchmark: {}", parsed.input.display());
    println!(
        "  Audio:    {} Hz, {}, {}-bit {}, {} ({})",
        wav_info.spec.sample_rate,
        channel_layout(wav_info.spec.channels, wav_info.spec.channel_mask),
        wav_info.spec.bits_per_sample,
        wav_info.spec.sample_format.name(),
        number(wav_info.spec.total_samples),
        duration(wav_info.spec.duration_seconds())
    );
    println!(
        "  Codec:    block {} samples, max LPC order {}, partitions <= {}, stereo auto",
        options.block_size, options.max_lpc_order, options.max_partition_order
    );
    println!(
        "  Sizes:    {} PCM -> {} OCAF ({})",
        bytes(pcm_bytes),
        bytes(ocaf_size),
        ratio_text(pcm_bytes, ocaf_size)
    );
    println!(
        "  Encode:   avg {:.3} s ({}), best {:.3} s, {} runs",
        encode_avg,
        throughput(pcm_bytes, encode_avg),
        encode_best,
        parsed.iterations
    );
    println!(
        "  Decode:   avg {:.3} s ({}), best {:.3} s, {} runs",
        decode_avg,
        throughput(pcm_bytes, decode_avg),
        decode_best,
        parsed.iterations
    );
    println!(
        "  Verify:   {:.3} s, PCM CRC-32 0x{:08x}, bit-perfect PCM comparison OK",
        verify_time, report.pcm_crc32
    );
    println!("  Note:     timings include WAV/OCAF file I/O and process-local buffers.");
    Ok(())
}

fn parse_encode_args(args: &[OsString]) -> CmdResult<EncodeArgs> {
    let mut positionals = Vec::new();
    let mut options = EncodeOptions::default();
    let mut force = false;
    let mut index = 0;
    while index < args.len() {
        let text = args[index].to_string_lossy().to_string();
        let (name, inline) = split_option(&text);
        match name.as_str() {
            "--force" => {
                ensure_no_inline(&name, &inline)?;
                force = true;
            }
            "--block-size" => {
                let value = take_value(args, &mut index, &name, inline)?;
                options.block_size = value.parse::<u16>().map_err(|_| {
                    "--block-size must be an integer between 1 and 65535".to_string()
                })?;
            }
            "--max-lpc-order" => {
                let value = take_value(args, &mut index, &name, inline)?;
                options.max_lpc_order = value.parse::<u8>().map_err(|_| {
                    "--max-lpc-order must be an integer between 0 and 32".to_string()
                })?;
            }
            "--max-partition-order" => {
                let value = take_value(args, &mut index, &name, inline)?;
                options.max_partition_order = value.parse::<u8>().map_err(|_| {
                    "--max-partition-order must be an integer between 0 and 8".to_string()
                })?;
            }
            "--stereo" => {
                let value = take_value(args, &mut index, &name, inline)?;
                options.stereo_mode = StereoMode::parse(&value).map_err(err)?;
            }
            "--title" => {
                let value = take_value(args, &mut index, &name, inline)?;
                options.metadata.set("TITLE", value).map_err(err)?;
            }
            "--artist" => {
                let value = take_value(args, &mut index, &name, inline)?;
                options.metadata.set("ARTIST", value).map_err(err)?;
            }
            "--album" => {
                let value = take_value(args, &mut index, &name, inline)?;
                options.metadata.set("ALBUM", value).map_err(err)?;
            }
            "--date" => {
                let value = take_value(args, &mut index, &name, inline)?;
                options.metadata.set("DATE", value).map_err(err)?;
            }
            "--genre" => {
                let value = take_value(args, &mut index, &name, inline)?;
                options.metadata.set("GENRE", value).map_err(err)?;
            }
            "--comment" => {
                let value = take_value(args, &mut index, &name, inline)?;
                options.metadata.set("COMMENT", value).map_err(err)?;
            }
            "--tracknumber" => {
                let value = take_value(args, &mut index, &name, inline)?;
                options.metadata.set("TRACKNUMBER", value).map_err(err)?;
            }
            "--meta" => {
                let value = take_value(args, &mut index, &name, inline)?;
                let (key, value) = value.split_once('=').ok_or_else(|| {
                    "--meta expects KEY=VALUE with an uppercase metadata key".to_string()
                })?;
                options.metadata.set(key, value).map_err(err)?;
            }
            other if other.starts_with('-') => {
                return Err(format!("unknown option {other} for `ocaf encode`"));
            }
            _ => positionals.push(PathBuf::from(&args[index])),
        }
        index += 1;
    }
    if positionals.len() != 2 {
        return Err("encode expects INPUT.wav and OUTPUT.ocaf".to_string());
    }
    options.validate().map_err(err)?;
    let input = positionals.remove(0);
    let output = positionals.remove(0);
    if same_path(&input, &output) {
        return Err("input and output must be different files".to_string());
    }
    Ok(EncodeArgs {
        input,
        output,
        options,
        force,
    })
}

fn parse_decode_args(args: &[OsString]) -> CmdResult<DecodeArgs> {
    let mut positionals = Vec::new();
    let mut force = false;
    let mut index = 0;
    while index < args.len() {
        let text = args[index].to_string_lossy().to_string();
        let (name, inline) = split_option(&text);
        match name.as_str() {
            "--force" => {
                ensure_no_inline(&name, &inline)?;
                force = true;
            }
            other if other.starts_with('-') => {
                return Err(format!("unknown option {other} for `ocaf decode`"));
            }
            _ => positionals.push(PathBuf::from(&args[index])),
        }
        index += 1;
    }
    if positionals.len() != 2 {
        return Err("decode expects INPUT.ocaf and OUTPUT.wav".to_string());
    }
    let input = positionals.remove(0);
    let output = positionals.remove(0);
    if same_path(&input, &output) {
        return Err("input and output must be different files".to_string());
    }
    Ok(DecodeArgs {
        input,
        output,
        force,
    })
}

fn parse_single_path(args: &[OsString], command: &str) -> CmdResult<(PathBuf, bool)> {
    let mut positionals = Vec::new();
    let mut force = false;
    let mut index = 0;
    while index < args.len() {
        let text = args[index].to_string_lossy().to_string();
        let (name, inline) = split_option(&text);
        match name.as_str() {
            "--force" => {
                ensure_no_inline(&name, &inline)?;
                force = true;
            }
            other if other.starts_with('-') => {
                return Err(format!("unknown option {other} for `ocaf {command}`"));
            }
            _ => positionals.push(PathBuf::from(&args[index])),
        }
        index += 1;
    }
    if positionals.len() != 1 {
        return Err(format!("{command} expects exactly one file"));
    }
    Ok((positionals.remove(0), force))
}

fn parse_benchmark_args(args: &[OsString]) -> CmdResult<BenchmarkArgs> {
    let mut positionals = Vec::new();
    let mut iterations = 3u32;
    let mut index = 0;
    while index < args.len() {
        let text = args[index].to_string_lossy().to_string();
        let (name, inline) = split_option(&text);
        match name.as_str() {
            "--iterations" => {
                let value = take_value(args, &mut index, &name, inline)?;
                iterations = value
                    .parse::<u32>()
                    .map_err(|_| "--iterations must be an integer".to_string())?;
                if iterations == 0 || iterations > 100 {
                    return Err("--iterations must be between 1 and 100".to_string());
                }
            }
            other if other.starts_with('-') => {
                return Err(format!("unknown option {other} for `ocaf benchmark`"));
            }
            _ => positionals.push(PathBuf::from(&args[index])),
        }
        index += 1;
    }
    if positionals.len() != 1 {
        return Err("benchmark expects exactly one input WAV file".to_string());
    }
    Ok(BenchmarkArgs {
        input: positionals.remove(0),
        iterations,
    })
}

fn split_option(text: &str) -> (String, Option<String>) {
    if text.starts_with("--") {
        if let Some((name, value)) = text.split_once('=') {
            return (name.to_string(), Some(value.to_string()));
        }
    }
    (text.to_string(), None)
}

fn ensure_no_inline(name: &str, inline: &Option<String>) -> CmdResult<()> {
    if inline.is_some() {
        return Err(format!("{name} does not take a value"));
    }
    Ok(())
}

fn take_value(
    args: &[OsString],
    index: &mut usize,
    name: &str,
    inline: Option<String>,
) -> CmdResult<String> {
    if let Some(value) = inline {
        return Ok(value);
    }
    *index += 1;
    if *index >= args.len() {
        return Err(format!("{name} expects a value"));
    }
    Ok(args[*index].to_string_lossy().to_string())
}

fn ratio_text(pcm_bytes: u64, ocaf_bytes: u64) -> String {
    if ocaf_bytes == 0 {
        return "-".to_string();
    }
    let ratio = pcm_bytes as f64 / ocaf_bytes as f64;
    let difference = (ocaf_bytes as f64 / pcm_bytes as f64 - 1.0) * 100.0;
    if difference <= 0.0 {
        format!("{ratio:.2}:1, {:.1}% smaller", -difference)
    } else {
        format!("{ratio:.2}:1, {difference:.1}% larger")
    }
}

fn average(values: &[f64]) -> f64 {
    values.iter().sum::<f64>() / values.len() as f64
}

fn best(values: &[f64]) -> f64 {
    values.iter().copied().fold(f64::INFINITY, f64::min)
}

fn output_parent(output: &Path) -> &Path {
    output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

fn same_path(input: &Path, output: &Path) -> bool {
    let Ok(input) = fs::canonicalize(input) else {
        return false;
    };
    let output = if output.exists() {
        fs::canonicalize(output).ok()
    } else {
        fs::canonicalize(output_parent(output))
            .ok()
            .map(|parent| parent.join(output.file_name().unwrap_or_default()))
    };
    output.is_some_and(|output| output == input)
}

fn write_atomically<T>(
    output: &Path,
    force: bool,
    write: impl FnOnce(&mut File) -> CmdResult<T>,
) -> CmdResult<T> {
    if output.exists() && !force {
        return Err(format!(
            "output {} already exists (use --force to overwrite)",
            output.display()
        ));
    }
    if output.is_dir() {
        return Err(format!("{} is a directory", output.display()));
    }
    let parent = output_parent(output);
    if !parent.exists() {
        return Err(format!(
            "output directory {} does not exist",
            parent.display()
        ));
    }
    let file_name = output
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("output");
    let temp = parent.join(format!(
        ".{file_name}.ocaf-tmp-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let result = (|| {
        let mut file = File::create(&temp)
            .map_err(|e| format!("cannot create temporary output {}: {e}", temp.display()))?;
        let value = write(&mut file)?;
        file.flush().map_err(err)?;
        file.sync_all().map_err(err)?;
        Ok(value)
    })();
    match result {
        Ok(value) => {
            if force && output.exists() {
                fs::remove_file(output)
                    .map_err(|e| format!("cannot replace {}: {e}", output.display()))?;
            }
            fs::rename(&temp, output).map_err(|e| {
                let _ = fs::remove_file(&temp);
                format!("cannot move output into {}: {e}", output.display())
            })?;
            Ok(value)
        }
        Err(error) => {
            let _ = fs::remove_file(&temp);
            Err(error)
        }
    }
}

fn unique_suffix() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0)
}

struct TempPath(PathBuf);

impl TempPath {
    fn new(tag: &str, extension: &str) -> Self {
        let path = env::temp_dir().join(format!(
            "ocaf-{tag}-{}-{}.{}",
            std::process::id(),
            unique_suffix(),
            extension
        ));
        Self(path)
    }
}

impl Drop for TempPath {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn wav_pcm_equal(original: &Path, decoded: &Path) -> CmdResult<bool> {
    let mut original_reader = WavReader::open(
        File::open(original).map_err(|e| format!("cannot reopen {}: {e}", original.display()))?,
    )
    .map_err(err)?;
    let mut decoded_reader = WavReader::open(
        File::open(decoded).map_err(|e| format!("cannot reopen {}: {e}", decoded.display()))?,
    )
    .map_err(err)?;
    if original_reader.info().spec != decoded_reader.info().spec {
        return Ok(false);
    }
    let mut original_block = Vec::new();
    let mut decoded_block = Vec::new();
    loop {
        let a = original_reader
            .read_frames(65_536, &mut original_block)
            .map_err(err)?;
        let b = decoded_reader
            .read_frames(65_536, &mut decoded_block)
            .map_err(err)?;
        if a != b {
            return Ok(false);
        }
        if original_block != decoded_block {
            return Ok(false);
        }
        if a == 0 {
            break;
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::output_parent;
    use std::path::Path;

    #[test]
    fn relative_outputs_use_the_current_directory() {
        assert_eq!(output_parent(Path::new("musica.ocaf")), Path::new("."));
        assert_eq!(output_parent(Path::new("./musica.ocaf")), Path::new("."));
        assert_eq!(
            output_parent(Path::new("album/musica.ocaf")),
            Path::new("album")
        );
        assert_eq!(
            output_parent(Path::new("/tmp/musica.ocaf")),
            Path::new("/tmp")
        );
    }
}
