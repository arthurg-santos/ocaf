//! Command-line interface for OCAF.

mod commands;
mod format;

use std::env;
use std::ffi::OsString;
use std::process::ExitCode;

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn main() -> ExitCode {
    let mut args: Vec<OsString> = env::args_os().skip(1).collect();
    if args.is_empty() {
        print_help();
        return ExitCode::SUCCESS;
    }
    let command = args.remove(0).to_string_lossy().to_string();
    if matches!(command.as_str(), "-h" | "--help" | "help") {
        if let Some(topic) = args.first() {
            print_command_help(&topic.to_string_lossy());
        } else {
            print_help();
        }
        return ExitCode::SUCCESS;
    }
    if matches!(command.as_str(), "-V" | "--version" | "version") {
        println!("ocaf {VERSION} (OCAF format 1.0)");
        return ExitCode::SUCCESS;
    }
    if args.iter().any(|arg| arg == "-h" || arg == "--help") {
        print_command_help(&command);
        return ExitCode::SUCCESS;
    }
    let result = match command.as_str() {
        "encode" => commands::encode(&args),
        "decode" => commands::decode(&args),
        "info" => commands::info(&args),
        "verify" => commands::verify_cmd(&args),
        "compare" => commands::compare(&args),
        "benchmark" | "bench" => commands::benchmark(&args),
        other => {
            eprintln!("ocaf: error: unknown command {other:?}");
            eprintln!("Run `ocaf help` to see the available commands.");
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("ocaf: error: {error}");
            ExitCode::FAILURE
        }
    }
}

fn print_help() {
    println!(
        "ocaf {VERSION} - Onood's Compressed Audio File (lossless audio codec)\n\
\n\
Usage:\n\
  ocaf encode <input.wav> <output.ocaf> [options]   Encode a WAV file\n\
  ocaf decode <input.ocaf> <output.wav> [options]   Decode to WAV\n\
  ocaf info <file.ocaf>                              Show container information\n\
  ocaf verify <file.ocaf>                            Decode and verify all checksums\n\
  ocaf compare <input.wav> <file.ocaf>               Compare sizes and formats\n\
  ocaf benchmark <input.wav> [--iterations N]        Measure speed and compression\n\
\n\
Commands:\n\
  encode      Encode uncompressed WAV (PCM 16/24/32, float 32/64) to OCAF\n\
  decode      Decode OCAF to a bit-identical WAV file\n\
  info        Read the header and metadata without decoding audio\n\
  verify      Fully decode the stream and check frame and PCM checksums\n\
  compare     Compare a WAV/OCAF pair\n\
  benchmark   Run encode/decode benchmarks on a WAV file\n\
\n\
General options:\n\
  -h, --help       Show help (use `ocaf help <command>` for details)\n\
  -V, --version    Show version\n\
\n\
Examples:\n\
  ocaf encode music.wav music.ocaf\n\
  ocaf decode music.ocaf restored.wav\n\
  ocaf info music.ocaf\n\
  ocaf verify music.ocaf\n\
  ocaf benchmark music.wav --iterations 5\n"
    );
}

fn print_command_help(command: &str) {
    match command {
        "encode" => println!(
            "Usage: ocaf encode <input.wav> <output.ocaf> [options]\n\
\n\
Options:\n\
  --block-size N              Samples per frame (default 16384, 1..=65535)\n\
  --max-lpc-order N           Maximum LPC order (default 12, 0..=32)\n\
  --max-partition-order N     Maximum Rice partition order (default 6, 0..=8)\n\
  --stereo MODE               auto, independent, left-side, right-side, mid-side\n\
  --title TEXT                Set the TITLE metadata field\n\
  --artist TEXT               Set the ARTIST metadata field\n\
  --album TEXT                Set the ALBUM metadata field\n\
  --date TEXT                 Set the DATE metadata field\n\
  --genre TEXT                Set the GENRE metadata field\n\
  --comment TEXT              Set the COMMENT metadata field\n\
  --tracknumber TEXT          Set the TRACKNUMBER metadata field\n\
  --meta KEY=VALUE            Set an arbitrary metadata field\n\
  --force                     Overwrite an existing output file\n\
  -h, --help                  Show this help\n"
        ),
        "decode" => println!(
            "Usage: ocaf decode <input.ocaf> <output.wav> [--force]\n\
\n\
Decoding verifies every frame checksum and the whole-stream PCM checksum.\n\
Metadata fields that WAV can represent are written into a LIST/INFO chunk.\n"
        ),
        "info" => println!(
            "Usage: ocaf info <file.ocaf>\n\
\n\
Reads the container header and metadata and validates their CRC-32 values.\n\
Use `ocaf verify` to decode the whole stream.\n"
        ),
        "verify" => println!(
            "Usage: ocaf verify <file.ocaf>\n\n\
Fully decodes the file, validates every frame and PCM checksum, and exits\n\
with a non-zero status if any corruption is found.\n"
        ),
        "compare" => println!(
            "Usage: ocaf compare <input.wav> <file.ocaf>\n\
\n\
Reports file sizes, PCM size and compression ratio for a matching pair.\n"
        ),
        "benchmark" | "bench" => println!(
            "Usage: ocaf benchmark <input.wav> [--iterations N]\n\
\n\
Runs the default encoder and decoder, reports real wall-clock throughput,\n\
verifies all checksums and compares decoded PCM with the original WAV.\n\
Timings include WAV/OCAF file I/O. Default iterations: 3 (1..=100).\n"
        ),
        _ => {
            eprintln!("ocaf: error: no help available for {command:?}");
            print_help();
        }
    }
}
