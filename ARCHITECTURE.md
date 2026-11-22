# OCAF architecture

OCAF is a small, dependency-free lossless audio codec. This document explains
how the reference implementation works, why the pieces are arranged the way
they are, and where the current limits are. The byte-level contract is in
[FORMAT.md](FORMAT.md); this file is about the implementation.

## 1. Goals and non-goals

**Goals**

- Bit-perfect reconstruction for every supported input, including all IEEE
  float bit patterns.
- A format that can be parsed and validated without trusting the encoder.
- Predictable behavior on extreme values, tiny blocks and corrupt files.
- A readable reference implementation with no C dependencies, no unsafe code
  and no platform tricks.
- Competitive compression on tonal, high-resolution and multichannel audio,
  and acceptable behavior on noise.

**Non-goals (for version 0.1.0)**

- Beating FLAC/WavPack on every possible signal.
- Low-latency live streaming. The current encoder works on whole frames and
  the WAV reader needs a seekable input.
- Maximum encode speed. The encoder intentionally spends time evaluating
  multiple predictors and stereo layouts.
- Encryption, DRM or cryptographic authentication.
- A stable public API. `ocaf-core` is pre-1.0.

## 2. Workspace and module map

```text
ocaf-core
├── lib.rs        public API and re-exports
├── error.rs      typed errors: I/O, invalid WAV, invalid OCAF, unsupported,
│                 corrupt, invalid argument, limit
├── format.rs     stream header, frame header, channel assignments, limits
├── metadata.rs   INFO/PAD chunks, UTF-8 key/value metadata
├── crc32.rs      slicing-by-8 CRC-32 (IEEE, reflected 0xEDB88320)
├── bitio.rs      MSB-first BitWriter/BitReader with strict bounds checks
├── rice.rs       zig-zag mapping, exact Rice parameter search, coder
├── lpc.rs        fixed predictors, Tukey window, autocorrelation,
│                 Levinson–Durbin, coefficient quantization, residual/restore
├── subframe.rs   per-channel candidate search, partition cost, bitstream I/O
├── float.rs      ordered keys, exact fixed-point, exception and group forms
├── pcm.rs        canonical WAV PCM ↔ planar i64 samples and float keys
├── wav.rs        streaming WAV reader/writer, RIFF chunks, LIST/INFO
├── encoder.rs    WAV → OCAF: frame loop, stereo selection, header patching
└── decoder.rs    OCAF → WAV: streaming decode, checksums, verification

ocaf-cli
├── main.rs       argument parsing, help, version, exit codes
├── commands.rs   encode/decode/info/verify/compare/benchmark
└── format.rs     human-readable bytes, duration, ratios, throughput
```

`ocaf-core` has no dependencies. `ocaf-cli` depends only on `ocaf-core`.
The workspace builds with Rust 1.75 or newer because it avoids newer language
features and standard-library APIs.

## 3. Encoding pipeline

### 3.1 WAV import

`WavReader::open` parses the RIFF/WAVE structure with bounded reads. It accepts
plain PCM, IEEE float and `WAVE_FORMAT_EXTENSIBLE`, validates the block
alignment and data length, and imports `LIST/INFO` strings. It never buffers
the entire file: `read_frames` returns at most the requested number of
interleaved frames.

The encoder adds or replaces metadata supplied on the command line, then adds
`ENCODER = ocaf <version>`. It writes a provisional 128-byte header, the
metadata, then frames. At the end it seeks back and patches the frame count,
block bounds, PCM byte count and stream CRC. All output is written to a
temporary file and renamed by the CLI, so a failed encode does not leave a
half-written destination.

### 3.2 Frames and stereo selection

The encoder splits the stream into blocks of `block_size` samples (16384 by
default). The last block may be shorter. For each block:

1. Convert interleaved PCM to planar `i64` channels. Integer samples are
   sign-extended. Float samples are mapped to ordered keys (section 5).
2. Choose a stereo assignment. `Auto` estimates the cost of independent,
   left/side, side/right and mid/side by applying the transform and measuring
   a cheap fixed-predictor Rice cost for each channel. Float64 omits mid/side.
   An explicit `--stereo` mode restricts the candidates.
3. Encode one subframe per channel with the assignment's encoded bit widths.
4. Write the frame header, payload and trailing CRC; update the running PCM
   CRC and block bounds.

The assignment is stored per frame, so a track that changes from correlated
to independent stereo does not pay for a global decision.

### 3.3 Subframe candidate search

For every channel block, `subframe::encode` evaluates:

- **Constant**: all samples equal.
- **Verbatim**: no prediction.
- **Fixed polynomial predictors**, orders 0–4. Order 0 is Rice-only coding;
  orders 1–4 use the standard finite differences. Fixed coefficients are
  implicit and cost no header bytes.
- **LPC**, orders 1..=`max_lpc_order` (12 by default, 32 maximum), limited by
  the block size.

The search has two stages to keep the cost reasonable:

1. Every candidate is scored with `quick_candidate_cost`: one Rice partition
   over all residuals, using a mean-based parameter estimate, plus the exact
   predictor header cost.
2. The best `FINALIST_COUNT = 3` candidates are re-scored with the real
   partitioned Rice cost. The winner is written.

This preserves the benefit of a large predictor search without running the
O(n × partitions) analysis for every order. It also means the encoder is not
guaranteed to find the global optimum among all predictors, but the final
writer always uses the exact cost of the plan it emits.

### 3.4 LPC analysis

For blocks of at least 8 samples and when LPC is enabled:

1. Apply a Tukey(0.5) window to reduce block-boundary bias. The window is
   used only for the autocorrelation; residuals are computed from the
   unmodified samples.
2. Compute autocorrelation lags `r[0..max_order]` in f64.
3. Solve the Yule–Walker equations with Levinson–Durbin for each order.
4. Quantize coefficients to signed 32-bit fixed point with a precision chosen
   from the encoded width:
   - width ≤ 20: 15 fractional bits,
   - width 21–25: 21 fractional bits,
   - width > 25: 28 fractional bits.
5. Apply a DC-gain guard: if the sum of quantized coefficients exceeds
   `1 << precision`, reduce the largest coefficients in one bulk correction.
   This avoids a predictor with excessive DC gain amplifying quantization
   noise, without the pathological per-unit loop that a naive implementation
   would need.
6. Compute residuals with `i128` accumulation when `precision > 15`; i64
   arithmetic is used for smaller shifts. All operations are explicitly
   wrapping where the format permits extreme 64-bit float keys.

The decoder does not repeat any analysis. It reads the order, precision and
coefficients and reconstructs with the same equation. That is why coefficient
quantization does not break losslessness.

### 3.5 Entropy coding

Residuals are zig-zag mapped and split into power-of-two partitions. Each
partition gets a Rice parameter. Rather than estimating `k` from the mean (which
can be catastrophic when a partition contains a rare huge outlier), the encoder
builds a 64-bin histogram of set bits:

```text
counts[bit] = number of values with that bit set
suffix recurrence -> sum(value >> k) for every k
cost(k) = n * (k + 1) + sum(value >> k)
```

This finds the exact optimum in O(n + 64) and never iterates over an enormous
unary quotient while choosing `k`. The coder itself does emit unary quotients;
the decoder bounds them by the remaining payload bits.

### 3.6 Why not simply use a transform codec?

A predictive codec is a good fit for lossless audio because:

- Prediction is reversible with integer arithmetic.
- It degrades gracefully on noise: the encoder can always fall back to
  verbatim or a low-order predictor.
- The decoder is simple and fast.
- It naturally supports exact float keys and arbitrary channel counts.

The architecture deliberately avoids floating-point arithmetic in the decode
path except for LPC coefficient estimation on the encoder side. The decoder
does only integer operations.

## 4. Decoding pipeline

`Decoder::open` reads and validates the stream header and metadata, then seeks
to the audio offset. `next_block`:

1. Reads and CRC-checks the frame header.
2. Verifies frame index, first sample, block size and assignment.
3. Reads the declared payload, verifies the frame CRC.
4. Decodes one subframe per channel into planar `i64` values.
5. Inverts the stereo transform.
6. Converts planar samples to canonical interleaved PCM and verifies the
   frame PCM CRC.
7. Feeds the PCM bytes to the running stream CRC.

`finish` (used by `decode_to_wav` and `verify`) checks that exactly
`total_samples` samples were produced, that `frame_count` frames were consumed,
and that the whole-stream PCM CRC matches the header. The decoder also enforces
the format's sample-width bounds, predictor orders, partition divisibility,
Rice parameter range, padding rules and frame limits. Errors include the frame
and channel where possible.

`probe` reads only the header and metadata; it is what `ocaf info` uses. It
does not decode audio and therefore cannot claim the audio is intact.

## 5. Exact floating-point design

Floating-point audio is the hard part of a lossless codec: a "sample" is a
24- or 53-bit significand plus exponent and sign, and adjacent samples do not
obey a simple linear recurrence after rounding.

OCAF uses four layers:

1. **Ordered keys.** Raw bits are mapped to a monotonic signed integer. The
   mapping is a bijection, so no information is lost. It also gives the
   predictor a chance to exploit monotonicity.
2. **Raw-key subframe.** An ordinary integer subframe over 64-bit keys. This is
   the universal fallback and handles every bit pattern.
3. **Exact fixed point.** If every sample in a block is a finite normal (or
   positive zero) and can be written as `integer / 2^scale` with a shared
   `scale <= 62`, the block is encoded as integers and predicted with the
   normal integer subframe machinery. Values that do not fit are moved to a
   sparse exception list (type 6); the base block uses zero in their place.
   A separate type 4 form is used when no exceptions are needed.
4. **Groups.** If a whole block still prefers raw keys, the encoder tries
   independent 512-sample chunks (type 5), each of which can choose fixed
   point, exceptions or raw keys. This localizes the "one tiny sample forces a
   huge shared scale" problem.

The key property is that the decoder never approximates. Integer
reconstruction is exact, and the conversion back to float bits checks the
representability conditions. NaN payloads, infinities, subnormals and negative
zero are preserved by the raw-key or exception paths. The encoder chooses the
representation by exact bit cost, so a representation that is not smaller is
not emitted.

Float stereo needs extra care:

- Left/side and side/right store keys in one channel and a 64-bit difference
  in the other.
- Mid/side transforms keys as 64-bit integers; this is allowed for float32
  because the integer subframe path has 64 bits.
- Mid/side is forbidden for float64 because the sum of two 64-bit keys does
  not fit in 64 bits. The encoder falls back to left/side; the decoder rejects
  a float64 mid/side frame.

The current float64 path is correct but not very strong on full-mantissa
material. A future version could store a low-bit correction stream separately
from a decorrelated high part, or use wider predictor coefficients. That is a
format/decoder extension, not a small tuning change.

## 6. Integrity and robustness

Checksums are layered so a decoder can distinguish a bad header from a bad
frame and a bad PCM block:

- stream header CRC,
- metadata chunk CRCs,
- frame header CRC,
- frame payload CRC,
- per-frame PCM CRC,
- whole-stream PCM CRC.

All reads are bounds-checked. `BitReader` refuses to read past the payload;
the Rice decoder bounds unary runs by the number of remaining bits; the
decoder caps frames at 256 MiB and metadata at 128 MiB before allocating. The
encoder uses `wrapping_add`/`wrapping_sub` for stereo and predictor arithmetic
where 64-bit float keys can reach the integer extremes, then validates decoded
values against the declared sample width.

There is no unsafe code and no external parser. Malformed WAV input is
rejected rather than guessed. A CRC mismatch is reported as corruption; a
valid but unknown feature is reported as unsupported. This distinction is
useful for tooling and for future format versions.

What is **not** covered: CRC-32 is not a cryptographic authenticator; an
attacker can rewrite the audio and all checksums. The implementation has not
been fuzzed or audited. Treat it as a pre-1.0 reference codec.

## 7. Complexity and performance

Let `n` be the number of samples per channel block and `p` the predictor order.

- Fixed predictor search: O(5 × n).
- LPC analysis: autocorrelation O(p × n), Levinson O(p²), residual O(p × n)
  per order, so roughly O(max_order² × n) in the worst case.
- Exact partition scoring: O(3 finalists × n × partition_orders).
- Rice coding: O(n + 64) per partition for parameter selection, O(n) to write.
- Decoder: O(n × p) for prediction plus O(n) for Rice decoding.

The encoder is single-threaded. On the benchmark machine it encodes at roughly
2.7–12.4 MB/s and decodes at 34–128 MB/s, depending on content. Decode is fast
because it does no search: it reads a plan and applies it. Encoding a float
block can be several times slower than integer encoding because it may try a
raw key, several fixed-point scales, exceptions and groups.

Memory use is proportional to the block size and channel count. Planar
buffers, candidate residuals and the output bitstream are reused across
frames. The current implementation still allocates some temporary vectors per
subframe; a future optimization is to keep a scratch arena.

## 8. Testing strategy

The test suite has three layers:

1. **Unit tests** in each module: CRC vectors, bit I/O bounds, Rice round-trips,
   predictor exactness, wasted bits, float special values, WAV round-trips and
   header parsing.
2. **Integration tests** in `crates/ocaf-core/tests/roundtrip.rs`: every
   supported integer and float format, multiple sample rates, mono/stereo/6
   channels, silence, white and pink noise, tones, synthetic music, extreme
   values, tiny and incomplete final blocks, long streams, metadata, deliberate
   corruption, version compatibility and whole-stream CRC.
3. **Property tests**: randomized formats, rates, channels, block sizes,
   content and predictor settings, compared sample-for-sample after a full
   encode/decode cycle.

`cargo test --workspace -- --test-threads=1` is the supported invocation. The
codec is deterministic for a given input and set of options; the external
benchmark harness is deterministic except for timing.

## 9. Design decisions and trade-offs

| Decision | Why | Cost |
| --- | --- | --- |
| Own container, no FLAC/Matroska reuse | Full control over checksums, float representation and versioning | More code to test |
| Per-frame stereo assignment | Adapts to changing correlation | One byte per frame and a search |
| Fixed + LPC predictors | Fixed predictors are cheap and robust; LPC handles tonal material | Encoder search time |
| 32-bit LPC coefficients | Compact and sufficient for ≤32-bit integer blocks | Quantization noise limits very high effective widths; float64 fixed-point bodies cannot use LPC well |
| Exact per-partition Rice search | Prevents rare-outlier blowups | O(n + 64) instead of a mean estimate |
| CRC-32 per frame and per stream | Fast, simple, catches corruption at useful granularity | Not cryptographic |
| Ordered float keys + fixed point + exceptions | Exact for all bit patterns and good on real float audio | Complex encoder; float32 still loses to WavPack's best mode |
| No dependencies | Auditability, portability, small build | Manual WAV/CLI/checksum code |

## 10. Future work

In rough priority order:

1. **Seek table** for random access and faster `info`/partial decode.
2. **Streaming API** that does not require a seekable WAV, plus `-` pipes.
3. **Better float64 and full-mantissa float32 compression**, likely using a
   split high/low representation and wider predictor coefficients.
4. **Adaptive frame sizing** and a frame-size decision based on content.
5. **Multithreaded encoding** across frames, with deterministic output.
6. **SIMD/autovectorized residual loops** and a scratch arena to reduce
   allocations.
7. **Fuzzing and a security review**, especially of WAV parsing, metadata,
   float exception lists and Rice bounds.
8. **More metadata**: cover art, ReplayGain, cue sheets, chapters.
9. **Broader benchmarks** on real music, speech, field recordings and
   high-channel-count material.
