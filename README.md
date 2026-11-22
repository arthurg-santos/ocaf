# OCAF — Onood's Compressed Audio File

OCAF is a lossless audio container and predictive codec written in Rust. It
stores integer PCM (16, 24 and 32 bit) and IEEE floating-point audio (32 and
64 bit) **bit-exactly**, including every float NaN payload, infinity, subnormal
and signed zero. The format is documented from the first byte, uses CRC-32 at
the container, frame, metadata and PCM levels, and is designed so that older
decoders can safely skip features they do not understand.

This repository contains the reference implementation:

- `ocaf-core` — container, codec, checksums and WAV import/export (no external
  dependencies).
- `ocaf` — command-line encoder, decoder, inspector, verifier and benchmark
  tool.

**Status:** format 1.0, reference implementation 0.1.0. The codec is real,
tested and bit-exact, but it has not been security-reviewed for untrusted
media and is not yet a drop-in replacement for a mature codec such as FLAC.

## Highlights

- **Bit-perfect by construction.** Predictor coefficients are the only
  quantized values; they are stored in the stream, so decoder and encoder
  always compute the same prediction. No sample is ever rounded.
- **Exact floats.** Float values are mapped to order-preserving 64-bit keys.
  When possible, the encoder converts a block to an exact fixed-point integer
  sequence and predicts that; otherwise it stores raw keys, a fixed-point base
  plus sparse exceptions, or independent chunks.
- **Adaptive prediction.** Per channel block, the encoder compares constant,
  verbatim, fixed polynomial (orders 0–4) and LPC (orders 1–32) predictors,
  selects the cheapest, and codes residuals with per-partition Golomb–Rice
  parameters chosen by exact bit cost.
- **Stereo decorrelation.** Independent, left/side, side/right and mid/side
  are evaluated per frame; mid/side is disabled for float64 because the sum
  needs an extra bit.
- **Robust container.** 128-byte stream header, extensible metadata chunks,
  self-describing frames, per-frame header and payload CRC-32, per-frame PCM
  CRC-32, and a whole-stream PCM CRC-32.
- **Portable and small.** No external crates, no unsafe code, no SIMD or
  platform-specific tricks. The workspace builds with Rust 1.75 or newer.
- **Full CLI and test suite.** Encode, decode, info, verify, compare and
  benchmark commands; deterministic corpus generator; property tests over
  random formats, rates, channels, block sizes and content; corruption and
  version-compatibility tests.

## Supported audio

| Sample format | Bit depths | Channels | Notes |
| --- | --- | --- | --- |
| Signed integer PCM | 16, 24, 32 | 1–256 | Arbitrary sample rate up to 4 MHz |
| IEEE 754 float | 32, 64 | 1–256 | Stored bit-exactly, including special values |

WAV input may be plain PCM, IEEE float or `WAVE_FORMAT_EXTENSIBLE`. A
`LIST/INFO` metadata block is imported into OCAF metadata and written back on
decode. Decoded WAV files contain the same canonical interleaved PCM bytes as
the source; WAV container headers are regenerated rather than byte-copied.

## Install

Requirements: Rust 1.75+ and Cargo. No C compiler or system library is needed.

```sh
git clone git@github.com:arthurg-santos/ocaf.git
cd ocaf
cargo build --release
./target/release/ocaf --version

# Optional: install the `ocaf` binary into ~/.cargo/bin
cargo install --path crates/ocaf-cli
```

The release profile uses thin LTO and one codegen unit; debug builds are much
slower at encoding.

## Usage

```sh
# Encode, with optional metadata
ocaf encode music.wav music.ocaf \
  --title "Example" --artist "Arthur" --album "Demo" --tracknumber 1

# Decode and verify the bitstream
ocaf decode music.ocaf restored.wav
ocaf verify music.ocaf

# Inspect without decoding the audio
ocaf info music.ocaf

# Compare a WAV/OCAF pair
ocaf compare music.wav music.ocaf

# Real wall-clock encode/decode benchmark
ocaf benchmark music.wav --iterations 5
```

Useful encoder options:

```text
--block-size N          Samples per frame (default 16384, 1..=65535)
--max-lpc-order N       Maximum LPC order (default 12, 0..=32)
--max-partition-order N Maximum Rice partition order (default 6, 0..=8)
--stereo MODE           auto, independent, left-side, right-side, mid-side
--meta KEY=VALUE        Arbitrary metadata entry (repeatable)
--force                 Overwrite an existing output file
```

The `encode` and `decode` commands write through a temporary file and rename
atomically, so an interrupted run does not leave a half-written output behind.
Errors are reported with a non-zero exit code and a specific reason (invalid
WAV, corrupt CRC, unsupported version, bad argument, etc.).

## Library

```rust
use ocaf_core::{decode_to_wav, encode_wav, EncodeOptions};
use std::fs::File;

fn main() -> ocaf_core::Result<()> {
    let input = File::open("music.wav")?;
    let output = File::create("music.ocaf")?;
    encode_wav(input, output, &EncodeOptions::default())?;

    let input = File::open("music.ocaf")?;
    let output = File::create("restored.wav")?;
    decode_to_wav(input, output)?;
    Ok(())
}
```

`ocaf-core` is not yet published on crates.io. Depend on it with a path or a
Git dependency while the format and API are pre-1.0.

## Compression in one page

1. The WAV is split into independent frames (16384 samples by default).
2. For stereo, the encoder estimates the cost of independent, left/side,
   side/right and mid/side and stores the winner in the frame header.
3. Each channel becomes a subframe. The encoder tries:
   - constant;
   - verbatim;
   - fixed polynomial predictors of order 0–4;
   - LPC of order 1–32, computed with a Tukey window, Levinson–Durbin and
     quantized coefficients whose exact values are stored.

   Candidates are ranked by a fast Rice estimate; the best three get an exact
   partitioned cost analysis.
4. The winner removes bits that are zero in every sample, stores the
   warm-up samples and predictor parameters, then codes residuals with
   Golomb–Rice. Every partition chooses the Rice parameter with the lowest
   exact bit count, so rare extreme residuals cannot cause pathological unary
   runs.
5. CRC-32 checksums cover the frame header, payload, original PCM block and
   the entire PCM stream.

For floats, the exact bit pattern is preserved. The preferred path converts a
block to integers at a shared power-of-two scale and predicts those integers;
values that do not fit (tiny samples, NaN, infinity, subnormals, negative zero)
are listed as exceptions or left in the raw-key representation. See
[FORMAT.md](FORMAT.md) and [ARCHITECTURE.md](ARCHITECTURE.md) for the exact
rules.

## Benchmarks

Measured on an Intel Core i7-3610QM (4 cores / 8 threads), Fedora Linux 45,
Rust 1.99.1, `--release`. Sizes are KiB; speed is PCM MiB/s, best of three
runs including file I/O. FLAC (level 12) and ALAC were driven by ffmpeg 9.0.2;
WavPack 5.9.0 used the native CLI with `-hh -x4`. Only byte-identical decodes
are counted.

| File | PCM | OCAF | Ratio | Enc MB/s | Dec MB/s | FLAC | ALAC | WavPack |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| music16.wav | 1687.5 | 857.1 | 1.97× | 3.6 | 36.5 | 861.5 | 888.5 | 850.0 |
| sine16.wav | 937.5 | 142.2 | 6.59× | 4.5 | 41.8 | 155.9 | 264.1 | 191.4 |
| sine24_96k.wav | 1687.5 | 167.2 | 10.09× | 8.2 | 63.8 | 431.2 | 857.9 | 351.5 |
| sine32_mono.wav | 562.5 | 55.2 | 10.19× | 12.3 | 64.7 | lossy | lossy | 292.9 |
| tone_f32.wav | 1125.0 | 73.1 | 15.38× | 4.8 | 62.5 | 96.0 | 425.4 | 108.5 |
| sine_f32.wav | 1125.0 | 351.6 | 3.20× | 2.7 | 52.4 | lossy | lossy | 249.5 |
| sine_f64.wav | 2250.0 | 1312.8 | 1.71× | 4.3 | 90.8 | lossy | lossy | n/a |

On this synthetic corpus OCAF produced a smaller file than FLAC and ALAC on
every comparison where those codecs were bit-exact, and smaller than WavPack
on 6 of 10. WavPack's extra float32 mode wins on full-mantissa float32
(249.5 KiB versus 351.6 KiB), and it is slightly ahead on synthetic music and
pink noise. The result is not a universal claim: white noise barely compresses
(563.0 KiB for 562.5 KiB of PCM), float64 remains the weakest case, and the
encoder is much slower than mature codecs. Full results, methodology, caveats
and reproduction commands are in [BENCHMARKS.md](BENCHMARKS.md).

```sh
cargo build --release
python3 scripts/generate_corpus.py
python3 scripts/benchmark.py --iterations 3
```

## Tests

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace -- --test-threads=1
```

The integration suite covers every supported format, sample rate and channel
count, silence, noise, tones, synthetic music, extreme values, tiny and
incomplete final blocks, long streams, metadata, deliberate corruption,
version checks and randomized property round-trips. Tests are run
single-threaded because the encoder and decoder allocate large scratch buffers;
they are deterministic and independent.

## Project layout

```text
.
├── Cargo.toml                  workspace
├── Cargo.lock
├── crates
│   ├── ocaf-core
│   │   ├── Cargo.toml
│   │   ├── src
│   │   │   ├── lib.rs          public API
│   │   │   ├── error.rs        typed errors
│   │   │   ├── format.rs       header/frame structures
│   │   │   ├── metadata.rs     extensible metadata chunks
│   │   │   ├── crc32.rs        streaming CRC-32
│   │   │   ├── bitio.rs        MSB-first bit reader/writer
│   │   │   ├── rice.rs         Golomb–Rice entropy coder
│   │   │   ├── lpc.rs          fixed predictors, LPC, quantization
│   │   │   ├── subframe.rs     per-channel adaptive coding
│   │   │   ├── float.rs        exact float representations
│   │   │   ├── pcm.rs          WAV PCM ↔ planar samples
│   │   │   ├── wav.rs          WAV reader/writer
│   │   │   ├── encoder.rs      frame loop and stereo selection
│   │   │   └── decoder.rs      streaming decoder and verification
│   │   └── tests/roundtrip.rs  integration/property/corruption tests
│   └── ocaf-cli
│       ├── Cargo.toml
│       └── src
│           ├── main.rs         argument parsing and help
│           ├── commands.rs     command implementations
│           └── format.rs       human-readable formatting
├── scripts
│   ├── generate_corpus.py      deterministic benchmark WAV corpus
│   └── benchmark.py            reproducible size/speed harness
├── testdata/generated          generated corpus (not committed)
├── testdata/bench/benchmark.json  measured benchmark record
├── README.md
├── FORMAT.md
├── ARCHITECTURE.md
├── BENCHMARKS.md
├── CONTRIBUTING.md
├── CHANGELOG.md
└── LICENSE
```

## Limitations and roadmap

- The encoder is single-threaded. On the benchmark machine it reaches roughly
  2.7–12.4 MB/s; decoding is much faster (34–128 MB/s).
- There is no seek index yet. `info` reads only the header and metadata, but
  decoding an arbitrary position requires decoding from the start.
- WAV input and output require seekable files; there is no stdin/stdout pipe
  mode.
- Float64 audio is the worst compression case; it is bit-exact but may save
  only a few percent on full-mantissa material.
- Tags are limited to textual `INFO` key/value pairs. Cover art, ReplayGain
  and cue sheets are not implemented.
- No SIMD, assembly, multithreaded encode or hardware acceleration.
- The reference implementation has not been fuzz-tested or audited for
  adversarial media. Defensive limits and checks exist, but this is pre-1.0
  software.

Planned work: seek table, streaming API, adaptive frame sizes, better
float64 prediction, multithreaded encode, fuzzing, and comparison against
native FLAC/ALAC/WavPack command-line tools on a wider real-music corpus.

## Links

- [FORMAT.md](FORMAT.md) — byte-level format specification
- [ARCHITECTURE.md](ARCHITECTURE.md) — codec design and module map
- [BENCHMARKS.md](BENCHMARKS.md) — measured results and methodology
- [CONTRIBUTING.md](CONTRIBUTING.md) — build, test and contribution rules
- [CHANGELOG.md](CHANGELOG.md) — release history

## License

MIT. See [LICENSE](LICENSE).
