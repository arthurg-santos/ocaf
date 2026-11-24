# Changelog

All notable changes to OCAF are documented here. The format version and the
implementation version are separate: this release implements OCAF format 1.0
with the `ocaf` 0.3.0 reference implementation.

## [0.3.0] - 2026-11-24

### Added

- `ocaf encode` accepts MP3, FLAC, ALAC, AAC/M4A, Ogg Vorbis, Opus, WMA,
  AIFF, CAF and the audio track of video files through an installed FFmpeg.
  `ffmpeg` and `ffprobe` are located in `PATH`, or selected with `--ffmpeg`
  and `--ffprobe`.
- External inputs are decoded to raw PCM and streamed directly into the
  encoder; no temporary WAV file is created.
- `--pcm-format auto|s16|s24|s32|f32|f64` controls the stored PCM format for
  external inputs. `auto` keeps 16/24/32-bit integer sources at their native
  depth and preserves the decoder's float output for lossy sources.
- Automatic channel-mask mapping from FFmpeg channel layouts, and a
  `SOURCE_CODEC` metadata entry for non-WAV inputs.
- `ocaf_core::encode_pcm`, a public streaming encoder for canonical
  interleaved PCM that discovers the sample count at EOF.
- Compressed WAV variants and RF64 files rejected by the native WAV reader
  fall back to FFmpeg decoding.
- CLI integration tests for bit-exact FLAC, MP3 float and MP3 s16 encoding,
  missing FFmpeg diagnostics and the new options.

### Changed

- WAV files are still parsed natively and require no external tools.
  `--pcm-format` is rejected for native WAV input, which keeps its own sample
  format.
- The Windows CI workflow installs FFmpeg so the external-decoding path is
  exercised on Windows as well.

The codec, bitstream format and compression behavior are unchanged from
0.2.0. The benchmark measurements in `BENCHMARKS.md` remain representative.

## [0.2.0] - 2026-11-23

### Added

- `ocaf play <file.ocaf>` decodes the stream to a temporary WAV file, plays it
  through an installed system player, and removes the temporary file when
  playback ends. Every checksum is verified before playback starts.
- Player auto-detection for `pw-play`, `paplay`, `aplay`, `ffplay`, `mpv`,
  `vlc` and SoX `play` on Linux; `afplay`, `ffplay`, `mpv` and `vlc` on macOS;
  and `ffplay`, `mpv`, `vlc` or the PowerShell WAV player on Windows.
- `--player PATH` selects a specific player executable.
- CLI integration tests for explicit-player playback, PATH auto-detection,
  temporary-file cleanup, bit-exact playback data and the missing-player
  error.
- A tag-triggered release workflow that verifies the version, runs the full
  test suite, builds Linux and Windows archives and publishes the GitHub
  Release with SHA-256 checksums.

The codec, bitstream format and compression behavior are unchanged from
0.1.0. The benchmark measurements in `BENCHMARKS.md` were taken with 0.1.0 and
remain representative.

## [0.1.1] - 2026-11-22

### Fixed

- `ocaf encode` and `ocaf decode` now accept a relative output filename with
  no directory component, such as `ocaf encode musica.wav musica.ocaf`. The
  empty parent returned by the path API was treated as a missing directory;
  it now correctly resolves to the current directory.
- Added a CLI regression test that encodes, decodes and verifies a file using
  only relative output paths.

### Added

- Windows x86_64 support: cross-compilation with `x86_64-pc-windows-gnu`,
  `scripts/build-windows.sh` packaging, a portable zip with a Portuguese
  tester guide and SHA-256 checksum, and `WINDOWS.md` instructions.
- A GitHub Actions workflow that builds, tests, packages and uploads the
  Windows archive on pushes and pull requests.

The codec, bitstream format and compression behavior are unchanged from
0.1.0. The benchmark measurements in `BENCHMARKS.md` were taken with 0.1.0 and
remain representative.

## [0.1.0] - 2026-11-22

Initial reference release.

### Format

- OCAF format 1.0: 128-byte stream header, extensible metadata chunks,
  36-byte audio frame headers and self-describing subframes.
- Integer PCM 16/24/32-bit, mono through 256 channels, arbitrary sample rates
  up to 4 MHz.
- Bit-exact IEEE float32/float64, including NaN payloads, infinities,
  subnormals and signed zero.
- Per-frame channel assignments: independent, left/side, side/right and
  mid/side.
- CRC-32 on the stream header, metadata chunks, frame headers, frame payloads,
  per-frame PCM and the whole PCM stream.
- Extensible metadata via length/CRC-delimited `INFO` and `PAD ` chunks;
  unknown chunks are skipped by readers.

### Codec

- Constant, verbatim, fixed polynomial (orders 0–4) and LPC (orders 1–32)
  predictors, selected per channel block by predicted and exact bit cost.
- Tukey-windowed autocorrelation, Levinson–Durbin, coefficient quantization
  with a DC-gain guard, and `i128` residual arithmetic for high precision.
- Per-partition Golomb–Rice coding with exact optimal parameter search in
  O(n + 64).
- Wasted-bit removal shared by constant, verbatim and predicted subframes.
- Exact float fixed-point representation, sparse exceptions and grouped
  fallback; raw ordered keys remain the universal lossless fallback.
- Per-frame stereo assignment selected by a fast transform cost estimate.

### CLI

- `ocaf encode`, `decode`, `info`, `verify`, `compare` and `benchmark`.
- Metadata flags (`--title`, `--artist`, `--album`, `--date`, `--genre`,
  `--comment`, `--tracknumber`, `--meta KEY=VALUE`).
- Encoder tuning flags (`--block-size`, `--max-lpc-order`,
  `--max-partition-order`, `--stereo`).
- Atomic output files and `--force` overwrite protection.
- Human-readable sizes, durations, ratios and throughput.

### Tests and tooling

- Unit tests for CRC, bit I/O, Rice coding, LPC, subframes, floats and WAV.
- Integration suite covering all formats, multiple rates and channel counts,
  silence, noise, tones, synthetic music, extremes, tiny and incomplete blocks,
  long streams, metadata, corruption, version checks and whole-stream CRCs.
- 20 randomized property round-trips.
- Deterministic corpus generator (`scripts/generate_corpus.py`).
- Reproducible size/speed benchmark harness (`scripts/benchmark.py`) that
  refuses to report non-bit-exact external results as comparisons.
- `cargo fmt` and `cargo clippy -D warnings` clean; documentation for format,
  architecture, benchmarks, contribution and licensing.

### Known limitations

- Reference encoder is single-threaded and slower than mature codecs.
- No seek index; decoding is sequential from the start.
- WAV input/output requires seekable files; no stdin/stdout pipes.
- Full-mantissa float32/float64 compression is weaker than integer and
  16-bit-derived float material; float64 is the weakest case.
- Text metadata only; no cover art, ReplayGain or cue sheets.
- No SIMD, assembly, multithreaded encode or hardware acceleration.
- Pre-1.0 software: not fuzz-tested or security-audited.
