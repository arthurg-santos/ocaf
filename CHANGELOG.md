# Changelog

All notable changes to OCAF are documented here. The format version and the
implementation version are separate: this release implements OCAF format 1.0
with the `ocaf` 0.1.1 reference implementation.

## [0.1.1] - 2026-11-22

### Fixed

- `ocaf encode` and `ocaf decode` now accept a relative output filename with
  no directory component, such as `ocaf encode musica.wav musica.ocaf`. The
  empty parent returned by the path API was treated as a missing directory;
  it now correctly resolves to the current directory.
- Added a CLI regression test that encodes, decodes and verifies a file using
  only relative output paths.

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
