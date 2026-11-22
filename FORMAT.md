# OCAF format 1.0

This document is the authoritative byte-level description of OCAF (Onood's
Compressed Audio File), version 1.0. The Rust implementation in
`crates/ocaf-core` follows this document. Where the implementation and this
document disagree, the document is intended to win and the implementation is a
bug.

## 1. Conventions

- All fixed-width integers are **little-endian** unless stated otherwise.
- Signed integers use two's-complement representation.
- Bitstreams are written **most-significant bit first**. The first bit written
  occupies bit 7 of the first byte.
- A "sample" means one channel value for one frame. A "frame" is a group of
  consecutive samples, one per channel, stored as one OCAF audio frame.
- `u8`, `u16`, `u32`, `u64` are unsigned; `i32`, `i64` are signed.
- `CRC-32` is the IEEE 802.3 reflected CRC-32 (polynomial `0xEDB88320`,
  initial value `0xFFFFFFFF`, final XOR `0xFFFFFFFF`, check value for
  `"123456789"` is `0xCBF43926`).
- "Canonical PCM" is the byte sequence that would appear in a WAV `data`
  chunk for the stream's format: interleaved, little-endian, with 24-bit
  integer samples stored in three bytes, 32/64-bit floats stored as their raw
  IEEE bit patterns, and no per-frame padding.
- The key words MUST, MUST NOT, SHOULD and MAY are used as in RFC 2119.

## 2. File overview

An OCAF file is:

```text
+--------------------+  offset 0
| Stream header      |  128 bytes
+--------------------+
| Metadata section   |  metadata_len bytes (may be zero)
+--------------------+  audio_offset
| Audio frame 0      |  frame_len bytes
| Audio frame 1      |
| ...                |
| Audio frame N-1    |
+--------------------+  end of file
```

`audio_offset = 128 + metadata_len`. Frames are self-contained and are stored
back-to-back with no padding between them. A file with zero samples has
`frame_count = 0` and no audio frames.

## 3. Stream header (128 bytes)

| Offset | Size | Type | Field | Value / meaning |
| ---: | ---: | --- | --- | --- |
| 0 | 4 | bytes | magic | `"OCAF"` (`4F 43 41 46`) |
| 4 | 2 | u16 | major version | `1` |
| 6 | 2 | u16 | minor version | `0` for files written by this implementation |
| 8 | 2 | u16 | codec | `1` = predictive lossless |
| 10 | 2 | u16 | header size | `128` |
| 12 | 4 | u32 | sample rate | 1..4,000,000 Hz |
| 16 | 2 | u16 | channels | 1..256 |
| 18 | 2 | u16 | bits per sample | integer: 16/24/32; float: 32/64 |
| 20 | 1 | u8 | sample format | `1` integer PCM, `2` IEEE float |
| 21 | 1 | u8 | flags | `0` in version 1.0 |
| 22 | 4 | u32 | channel mask | WAV-style speaker mask; `0` = unspecified |
| 26 | 2 | u16 | reserved | `0` |
| 28 | 8 | u64 | total samples | per-channel frame count |
| 36 | 8 | u64 | frame count | number of audio frames |
| 44 | 2 | u16 | minimum block size | minimum `block_size` of any frame; `0` if none |
| 46 | 2 | u16 | maximum block size | maximum `block_size` of any frame; `0` if none |
| 48 | 8 | u64 | audio offset | byte offset of frame 0; `>= 128 + metadata_len` |
| 56 | 8 | u64 | metadata length | bytes from offset 128 to `audio_offset` |
| 64 | 4 | u32 | PCM CRC-32 | CRC of the entire canonical PCM stream |
| 68 | 56 | bytes | reserved | all zero in version 1.0 |
| 124 | 4 | u32 | header CRC-32 | CRC of bytes `0..124` |

Validation rules:

- The magic MUST match, the header size MUST be 128, and the header CRC MUST
  match.
- `major version` MUST be 1 for this specification. A decoder MUST reject a
  different major version rather than guess.
- `codec` MUST be 1. Unknown codec IDs are unsupported, not corrupt.
- `sample rate` MUST be in 1..=4,000,000.
- `channels` MUST be in 1..=256.
- `(sample format, bits)` MUST be `(1, 16)`, `(1, 24)`, `(1, 32)`, `(2, 32)`
  or `(2, 64)`.
- If `frame_count > 0`, both block bounds MUST be non-zero and
  `min_block_size <= max_block_size <= 65535`.
- If `frame_count == 0`, `total_samples` MUST be zero.
- `audio_offset` MUST be at least `128 + metadata_len`; the implementation also
  requires it to be within the file.

A higher minor version is accepted by this implementation as long as the
structures it encounters are known and valid. See section 12.

## 4. Metadata section

The metadata section is a sequence of chunks. There is no section-level
padding: the next chunk starts immediately after the previous chunk's payload.

### 4.1 Chunk header (16 bytes)

| Offset | Size | Type | Field |
| ---: | ---: | --- | --- |
| 0 | 4 | bytes | chunk ID |
| 4 | 4 | u32 | payload length in bytes |
| 8 | 4 | u32 | CRC-32 of the payload |
| 12 | 4 | u32 | flags; `0` when written by this implementation |

Each chunk's payload is `length` bytes. Unknown chunk IDs are skipped after
their payload CRC has been checked, which makes it safe to add new metadata
kinds in a minor version. The total metadata section is limited to 128 MiB by
this implementation.

### 4.2 `INFO` chunk

An `INFO` chunk carries one UTF-8 key/value pair:

```text
u16 key_length
u8  key[key_length]
u8  value[payload_length - 2 - key_length]
```

- `key_length` is 1..=64.
- Keys contain only `A`–`Z`, `0`–`9`, `_` and `-`.
- The value is the remaining bytes and MUST be valid UTF-8. It may be empty.
- A stream may contain at most 4096 `INFO` chunks in this implementation.

Recommended keys include `TITLE`, `ARTIST`, `ALBUM`, `DATE`, `GENRE`,
`COMMENT`, `TRACKNUMBER`, `SOFTWARE` and `ENCODER`. The encoder always writes
an `ENCODER` value of the form `ocaf <version>`. Duplicate keys are allowed at the
format level; the reference `Metadata` helper replaces an existing key.

### 4.3 `PAD ` chunk

`PAD ` has no defined payload semantics. Writers may use it to reserve space;
readers MUST validate its CRC and MUST otherwise ignore it. Unknown chunk IDs
use the same rule.

## 5. Audio frames

### 5.1 Frame header (36 bytes)

| Offset | Size | Type | Field |
| ---: | ---: | --- | --- |
| 0 | 4 | bytes | magic `"OCFR"` |
| 4 | 2 | u16 | header size = 36 |
| 6 | 4 | u32 | frame index, starting at 0 |
| 10 | 8 | u64 | first sample index in the stream |
| 18 | 2 | u16 | block size in samples (1..=65535) |
| 20 | 1 | u8 | channel assignment (0..3) |
| 21 | 1 | u8 | flags = 0 |
| 22 | 2 | u16 | reserved = 0 |
| 24 | 4 | u32 | frame length, including this header and the trailing CRC |
| 28 | 4 | u32 | CRC-32 of the frame's canonical PCM block |
| 32 | 4 | u32 | CRC-32 of header bytes `0..32` |

The frame payload starts at byte 36 and ends at byte `frame_len - 4`. The last
four bytes are a CRC-32 over the frame header and the payload (everything from
byte 0 through `frame_len - 5`).

Frames MUST appear in order. For frame `i`, `index` MUST equal `i` and
`first_sample` MUST equal the sum of the block sizes of all previous frames.
The last frame may be shorter than earlier frames; `first_sample + block_size`
MUST NOT exceed `total_samples`. The implementation limits a compressed frame
to 256 MiB, but this is a defensive implementation limit rather than a format
limit.

### 5.2 Channel assignments

Only stereo (2-channel) streams may use assignments other than 0.

| ID | Name | Channel 0 | Channel 1 |
| ---: | --- | --- | --- |
| 0 | Independent | left | right |
| 1 | Left/side | left | left − right |
| 2 | Side/right | left − right | right |
| 3 | Mid/side | `(left + right) >> 1` | left − right |

Arithmetic is two's-complement wrapping arithmetic on `i64`, and `>>` is an
arithmetic shift. The side channel needs one extra bit of range, so its encoded
sample width is `bits_per_sample + 1` for integer streams. For float streams
the side channels are encoded as 64-bit integers (see section 8).

Inverse transforms for a stereo pair `(a, b)`:

- Left/side: `left = a`, `right = a − b`.
- Side/right: `left = a + b`, `right = b`.
- Mid/side: `left = a + ((b + (b & 1)) >> 1)`, `right = left − b`.

Mid/side is not defined for 64-bit float streams in version 1.0 because adding
two 64-bit ordered keys can require 65 bits. Encoders MUST choose another
assignment for such streams; decoders MUST reject assignment 3 in a float64
stream.

## 6. Frame payload

The payload is a bitstream containing one subframe per channel, in channel
order (0, 1, 2, ...). It is not byte-aligned between subframes. After the last
subframe, the writer pads with zero bits to the next byte boundary. A decoder
MUST reject a payload with 8 or more unused bits or with any non-zero padding
bit.

## 7. Integer subframes

Every integer subframe starts with a one-byte type:

| Type | Name |
| ---: | --- |
| 0 | Constant |
| 1 | Fixed predictor |
| 2 | LPC |
| 3 | Verbatim |
| 4 | Float fixed-point (float channels only) |
| 5 | Float group (float channels only) |
| 6 | Float base plus exceptions (float channels only) |

Types 0–3 are used for integer PCM and as the raw-key fallback for floats.
Types 4–6 are described in section 8.

### 7.1 Wasted bits

After the type byte, every type 0–3 subframe has:

```text
u8 wasted
```

`wasted` is the number of low-order zero bits shared by every sample in the
subframe. It MUST be less than the subframe's encoded bit width. The stored
samples are `sample >> wasted` (arithmetic shift). The decoder shifts the
restored samples left by `wasted`; this is exact because the removed bits were
zero. `wasted` may be zero. The effective width is `bps - wasted`.

For a signed value of effective width `w`, encoded values occupy exactly `w`
bits in two's-complement form, most-significant bit first.

### 7.2 Constant subframe (type 0)

```text
u8  type = 0
u8  wasted
i{w} value          // w = bps - wasted
```

The value is repeated for every sample in the block. `w` MUST be at least 1.

### 7.3 Verbatim subframe (type 3)

```text
u8  type = 3
u8  wasted
i{w} samples[block_size]
```

The samples are stored directly. This is the fallback for incompressible data.

### 7.4 Fixed predictor subframe (type 1)

```text
u8  type = 1
u8  wasted
u8  order                  // 0..=4
i{w} warmup[order]         // w = bps - wasted
u8  partition_order        // 0..=15, implementation accepts any that divides
u8  rice_parameters[2^partition_order]
... Rice-coded residuals ...
```

The predictor for sample `i >= order` is:

```text
prediction = 0
for j in 0..order:
    prediction += coeff[j] * x[i-1-j]
x[i] = prediction + residual[i-order]
```

All arithmetic is wrapping `i64`. The coefficients for order 0–4 are fixed:

| Order | Coefficients |
| ---: | --- |
| 0 | none; `prediction = 0` |
| 1 | `[1]` |
| 2 | `[2, -1]` |
| 3 | `[3, -3, 1]` |
| 4 | `[4, -6, 4, -1]` |

The first `order` samples are stored verbatim as warm-up values. Order 0 has
no warm-up values. If `order >= block_size`, the subframe is invalid.

### 7.5 LPC subframe (type 2)

```text
u8  type = 2
u8  wasted
u8  order                  // 1..=32
u8  precision              // 1..=30
i32 coefficients[order]    // signed 32-bit fixed-point
i{w} warmup[order]         // w = bps - wasted
u8  partition_order
u8  rice_parameters[2^partition_order]
... Rice-coded residuals ...
```

The predictor is:

```text
prediction = 0
for j in 0..order:
    prediction += coefficients[j] * x[i-1-j]
prediction >>= precision       // arithmetic shift
x[i] = prediction + residual[i-order]
```

The accumulator is at least 128 bits wide for exactness when `precision > 15`;
the reference implementation uses `i128`. Values are reduced to `i64` only after
the shift. The coefficients themselves are stored exactly as used by the
decoder; the encoder may derive them however it likes.

### 7.6 Residual coding

For a subframe with `n = block_size` and predictor order `p`, there are
`n - p` residuals, one for each sample after the warm-up region. They are
split into `parts = 2^partition_order` partitions over the original sample
index space:

```text
start = max(0, part * part_size - p)
end   = min(n - p, (part + 1) * part_size - p)
```

where `part_size = n / parts`. The decoder reads exactly `end - start` Rice
values for that partition, appending them in partition order. The encoder uses
the same ranges. Partitions must evenly divide the block size. `parts` must not
exceed the block size.

Each partition is independently coded with its own `k` (0..=63). A residual
`r` is mapped to an unsigned value:

```text
u = (r << 1) ^ (r >> 63)        // zig-zag, 64-bit arithmetic
```

Then for parameter `k`:

```text
quotient  = u >> k
remainder = u & ((1 << k) - 1)
code = quotient zero bits, one 1 bit, then remainder as k bits (MSB first)
```

When `k = 0` no remainder bits follow the terminating one. The `k` byte is
stored for every partition even when the partition is empty. A decoder MUST
limit the unary quotient to the number of remaining payload bits; an
overlong unary run is a corrupt/truncated bitstream, not a huge allocation.

The reference encoder chooses `k` by exact bit cost:

```text
cost(k) = number_of_values * (k + 1) + sum(u >> k)
```

and finds the true optimum in O(n + 64) using a bit histogram.

## 8. Floating-point subframes

OCAF is bit-exact for IEEE 754 binary32 and binary64. It never approximates a
float value. There are four representations.

### 8.1 Ordered keys

Raw float bits are first mapped to an order-preserving signed 64-bit key. This
mapping is a bijection and preserves every bit, including NaN payloads and
signed zero.

For binary32, with `b` the 32 raw bits:

```text
key = if b & 0x80000000 != 0 { !b } else { b ^ 0x80000000 }   // as u32, then i64
```

For binary64, with `b` the 64 raw bits:

```text
key = if b & 0x8000000000000000 != 0 { !b } else { b ^ 0x8000000000000000 }   // as i64
```

The inverse mapping is:

```text
// binary32
u = key as u32
b = if u & 0x80000000 != 0 { u ^ 0x80000000 } else { !u }

// binary64
u = key as u64
b = if u & 0x8000000000000000 != 0 { u ^ 0x8000000000000000 } else { !u }
```

### 8.2 Raw-key subframe

The simplest float representation is an ordinary integer subframe of types
0–3 with an encoded width of 64 bits over the ordered keys. The decoder reads
the keys and maps them back to raw float bits. This path is always available
and handles every bit pattern, including NaN, infinity, subnormal values and
negative zero. It is used as a fallback and as the inner representation of a
group.

### 8.3 Fixed-point subframe (type 4)

```text
u8  type = 4
u8  float_width          // 32 or 64
u8  scale                // 0..=62
u8  integer_bps          // 1..=64
u32 body_bits
... body: one integer subframe of width integer_bps, exactly body_bits bits ...
```

The body is a self-contained integer subframe (`constant`, `verbatim`, `fixed`
or `LPC`) whose samples are integers. Given an integer `v`, the decoder
reconstructs the float raw bits for `v / 2^scale` exactly. It does not round.
The encoder may use type 4 only when every sample in the block is one of:

- a finite normal float, or
- positive zero.

For a normal float, reduce its significand to an odd integer `s` and write the
value as `(-1)^sign * s * 2^E`. For example, binary32 `1.5` has `s = 3` and
`E = -1`; the smallest normal value has `s = 1` and `E = -126`. It is
representable exactly when:

```text
E + scale >= 0
and the final signed integer fits in integer_bps bits
```

Then:

```text
integer = (-1)^sign * s << (E + scale)
```

Positive zero maps to integer 0. Negative zero, subnormals, infinities and NaNs
are not representable and MUST use another representation. A decoder MUST
reject a type-4 body that reconstructs an unrepresentable value or overflows
the declared width. On decode, zero reconstructs to positive zero; the encoder
MUST NOT use type 4 for a block containing negative zero.

`body_bits` is the exact number of bits occupied by the body subframe. It
allows the decoder to skip the body without relying on byte alignment and lets
the encoder use non-byte-aligned headers. The body MUST consume exactly
`body_bits` and MUST produce exactly `block_size` integers.

### 8.4 Base plus exceptions (type 6)

```text
u8  type = 6
u8  float_width
u8  scale
u8  integer_bps
u32 base_bits
... base: one integer subframe of width integer_bps, exactly base_bits bits ...
u32 exception_count
repeat exception_count times:
    u16 position              // strictly increasing, < block_size
    u{float_width} raw_key    // 32 or 64 raw ordered-key bits
```

The base is built like a type-4 body, but any sample that is not exactly
representable is replaced by integer 0. `exception_count` samples are then
overwritten with their exact ordered keys. Positions are 16-bit and MUST be
strictly increasing and less than the block size. This representation is the
normal path for float blocks that contain quiet samples near a zero crossing:
the base uses a scale suitable for the loud samples, and only the samples that
need more fractional precision are listed.

The decoder first reconstructs every base integer to a raw float key, then
applies the exceptions. It MUST reject duplicate or out-of-order positions and
a count larger than the block.

### 8.5 Grouped float subframe (type 5)

```text
u8  type = 5
u8  float_width            // 32 or 64
u8  chunk_shift            // 1..=16; chunk size = 1 << chunk_shift
repeat until block_size samples are produced:
    u32 chunk_bits
    ... one self-contained float subframe payload of exactly chunk_bits bits ...
```

Each chunk is a type 4, type 6 or raw-key representation with the same float
width; nested groups are invalid. The final chunk may be shorter than
`1 << chunk_shift`. The encoder only tries this representation when a whole
block would otherwise use a raw-key subframe and the block is at least two
chunks long; it compares the exact bit cost and keeps the smaller result.
`chunk_bits` is the exact number of bits, so no byte-alignment padding is
present between chunks.

Type 5 is a compression heuristic, not a different numerical representation.
Every chunk still reconstructs exact float bits.

### 8.6 Float stereo

For left/side and side/right assignments, the key channel (left or right) is a
float subframe and the difference channel is a 64-bit integer subframe. For
mid/side, both channels are 64-bit integer subframes over transformed ordered
keys. For float64, mid/side is forbidden as described in section 5.2.

## 9. Checksums

OCAF uses CRC-32 for integrity, not authentication.

1. **Stream header CRC**: CRC of stream header bytes `0..124`, stored at
   124.
2. **Metadata chunk CRC**: CRC of the chunk payload, stored in the chunk
   header.
3. **Frame header CRC**: CRC of frame header bytes `0..32`, stored at 32.
4. **Frame CRC**: CRC of the complete frame header plus payload, stored in the
   last four bytes of the frame.
5. **Frame PCM CRC**: CRC of the canonical PCM bytes for that frame's samples,
   stored in the frame header at offset 28.
6. **Stream PCM CRC**: CRC of the concatenation of every frame's canonical PCM
   bytes, stored in the stream header at offset 64.

The decoder checks every checksum and also verifies that frame indices and
sample positions are contiguous, that the declared sample count is reached
exactly, that subframe lengths and predictor orders are sane, that decoded
integer samples fit their declared depth, and that frame padding is zero.

## 10. WAV mapping

OCAF stores audio parameters, not a WAV file. The reference WAV reader accepts:

- PCM integer WAV (`fmt` tag 1) with 16/24/32 bits per sample.
- IEEE float WAV (`fmt` tag 3) with 32/64 bits per sample.
- `WAVE_FORMAT_EXTENSIBLE` (`fmt` tag `0xFFFE`) with PCM or IEEE-float
  subformats, channel masks and >2 channels.
- `LIST/INFO` metadata, imported as `INFO` chunks.

The WAV `data` bytes, interpreted as canonical interleaved PCM, are the exact
PCM that the stream CRC covers. WAV headers are not stored. On decode, the
writer emits a canonical WAV header, the original stream parameters, the
metadata as a `LIST/INFO` block, and the exact canonical PCM bytes. A decoded
WAV is therefore bit-identical in audio content but not necessarily
byte-identical as a whole file.

## 11. Limits

Format-level limits:

- 256 channels.
- Sample rate 1..4,000,000 Hz.
- Block size 1..65535 samples.
- Frame length up to `u32::MAX` bytes in principle.
- Metadata section up to 128 MiB.
- Up to 4096 `INFO` entries.
- LPC order 1..32; coefficient precision 1..30.
- Rice parameter 0..63; partition order 0..15 in the bitstream (the reference
  encoder uses 0..8).

Implementation defensiveness (not format requirements): a single compressed
frame is capped at 256 MiB, and the decoder validates every length before
allocating. These caps may be raised in a future implementation without
changing the format.

## 12. Versioning and extension

- **Major version** changes when a decoder that understands version 1 cannot
  correctly decode the stream. A version-1 decoder MUST reject a different
  major version with an `unsupported` error.
- **Minor version** changes for backward-compatible additions, such as new
  metadata chunk IDs or new optional subframe types that an older decoder can
  reject cleanly. The current decoder accepts higher minor versions and relies
  on per-structure validation; it rejects unknown codec IDs and unknown
  subframe types rather than guessing.
- Future metadata chunks MUST carry their own length and CRC and MUST be
  skippable by readers that do not understand them.
- Reserved header and frame bytes are zero in version 1.0. Writers MUST keep
  them zero. A future minor version may assign meaning to a reserved bit only
  if the resulting stream remains safely rejectable by version-1 readers.
- Codec ID 1 is the predictive lossless codec described here. A different
  codec (for example, a future transform codec) gets a new ID.
- There is no encryption, DRM or authentication in version 1.0. CRC-32 detects
  accidental corruption, not tampering.

## 13. Decoder checklist

A conforming decoder should perform these checks, in roughly this order:

1. Read 128 bytes; verify magic, header size, version major, header CRC.
2. Validate sample rate, channels, format/bit-depth combination, block bounds,
   metadata length and audio offset.
3. Read and CRC-check every metadata chunk; skip unknown IDs and retain `INFO`.
4. Seek to `audio_offset`.
5. For each expected frame:
   - read and CRC-check the 36-byte frame header;
   - verify index, first sample, block size and assignment;
   - read the declared payload and verify the frame CRC;
   - decode one subframe per channel, rejecting unknown types, invalid
     orders/precisions, truncated Rice codes and out-of-range samples;
   - verify that only less than one byte of zero padding remains;
   - reconstruct canonical PCM and verify the frame PCM CRC.
6. Verify that exactly `total_samples` samples were produced and that the
   stream PCM CRC matches the header.

Any mismatch is a corrupt or unsupported stream. A decoder should report which
check failed and at which frame/sample, as the reference implementation does.
