#!/usr/bin/env python3
"""Generate the deterministic WAV corpus used by the OCAF benchmarks.

The corpus is deliberately small and fully synthetic so that every number in
BENCHMARKS.md can be reproduced on any machine with Python 3 and no third-party
packages. Files are written with plain PCM or IEEE-float WAV chunks; files with
more than two channels use WAVE_FORMAT_EXTENSIBLE.

Usage:
    python3 scripts/generate_corpus.py [--output DIR] [--force]
"""

from __future__ import annotations

import argparse
import math
import os
import struct
import sys

RATE = 48_000


class Rng:
    """Small deterministic xorshift generator, independent of Python version."""

    def __init__(self, seed: int):
        self.state = seed & 0xFFFFFFFF or 0x12345678

    def next_u32(self) -> int:
        x = self.state
        x ^= (x << 13) & 0xFFFFFFFF
        x ^= x >> 17
        x ^= (x << 5) & 0xFFFFFFFF
        self.state = x & 0xFFFFFFFF
        return self.state

    def uniform(self) -> float:
        return self.next_u32() / 0xFFFFFFFF


RNG = Rng(0x0CAF_2026)


def clamp(value: float, low: float, high: float) -> float:
    return low if value < low else high if value > high else value


def quantize_int(value: float, bits: int) -> int:
    scale = float(1 << (bits - 1))
    return max(-(1 << (bits - 1)), min((1 << (bits - 1)) - 1, int(round(value * scale))))


def pack_int(value: int, bits: int) -> bytes:
    width = bits // 8
    if bits == 24:
        return (value & 0xFFFFFF).to_bytes(3, "little", signed=False)
    return int(value).to_bytes(width, "little", signed=True)


def write_wav(
    path: str,
    channels: int,
    rate: int,
    bits: int,
    is_float: bool,
    data: bytes,
) -> None:
    width = bits // 8
    block_align = width * channels
    byte_rate = rate * block_align
    if channels > 2:
        # WAVE_FORMAT_EXTENSIBLE with a proper channel mask.
        mask = (1 << channels) - 1
        guid = (
            b"\x01\x00\x00\x00\x00\x00\x10\x00\x80\x00\x00\xaa\x00\x38\x9b\x71"
            if not is_float
            else b"\x03\x00\x00\x00\x00\x00\x10\x00\x80\x00\x00\xaa\x00\x38\x9b\x71"
        )
        fmt = struct.pack(
            "<HHIIHHHHI",
            0xFFFE,
            channels,
            rate,
            byte_rate,
            block_align,
            bits,
            22,
            bits,
            mask,
        ) + guid
    else:
        fmt = struct.pack(
            "<HHIIHH",
            3 if is_float else 1,
            channels,
            rate,
            byte_rate,
            block_align,
            bits,
        )
    body = (
        b"WAVE"
        + b"fmt "
        + struct.pack("<I", len(fmt))
        + fmt
        + b"data"
        + struct.pack("<I", len(data))
        + data
    )
    with open(path, "wb") as handle:
        handle.write(b"RIFF" + struct.pack("<I", len(body)) + body)


def write_int(
    path: str, channels: int, rate: int, bits: int, frame_count: int, sample, name: str
) -> None:
    data = bytearray()
    for frame in range(frame_count):
        for channel in range(channels):
            data += pack_int(quantize_int(sample(frame, channel), bits), bits)
    write_wav(path, channels, rate, bits, False, bytes(data))
    print(
        f"  {name}: {len(data)} bytes, {frame_count / rate:.2f} s, "
        f"{channels} ch, {bits}-bit int"
    )


def write_float(
    path: str, channels: int, rate: int, bits: int, frame_count: int, sample, name: str
) -> None:
    code = "<f" if bits == 32 else "<d"
    data = bytearray()
    for frame in range(frame_count):
        for channel in range(channels):
            data += struct.pack(code, sample(frame, channel))
    write_wav(path, channels, rate, bits, True, bytes(data))
    print(
        f"  {name}: {len(data)} bytes, {frame_count / rate:.2f} s, "
        f"{channels} ch, {bits}-bit float"
    )


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", default="testdata/generated", help="corpus directory")
    parser.add_argument("--force", action="store_true", help="overwrite existing files")
    args = parser.parse_args()
    out = args.output
    os.makedirs(out, exist_ok=True)

    def path(name: str) -> str:
        return os.path.join(out, name)

    def missing(name: str) -> bool:
        return args.force or not os.path.exists(path(name))

    print(f"writing corpus to {out}")

    if missing("silence16.wav"):
        write_int(path("silence16.wav"), 2, RATE, 16, 2 * RATE, lambda i, c: 0.0, "silence16.wav")

    if missing("noise16.wav"):
        write_int(
            path("noise16.wav"),
            2,
            RATE,
            16,
            3 * RATE,
            lambda i, c: RNG.uniform() * 2.0 - 1.0,
            "noise16.wav",
        )

    if missing("pinknoise16.wav"):
        # Paul Kellet's refined pink-noise filter, one independent filter per
        # channel. The output is normalised to roughly -6 dBFS.
        states = [[0.0] * 7 for _ in range(2)]

        def pink(i: int, c: int) -> float:
            white = RNG.uniform() * 2.0 - 1.0
            s = states[c]
            s[0] = 0.99886 * s[0] + white * 0.0555179
            s[1] = 0.99332 * s[1] + white * 0.0750759
            s[2] = 0.96900 * s[2] + white * 0.1538520
            s[3] = 0.86650 * s[3] + white * 0.3104856
            s[4] = 0.55000 * s[4] + white * 0.5329522
            s[5] = -0.7616 * s[5] - white * 0.0168980
            value = s[0] + s[1] + s[2] + s[3] + s[4] + s[5] + s[6] + white * 0.5362
            s[6] = white * 0.115926
            return value * 0.12

        write_int(
            path("pinknoise16.wav"),
            2,
            RATE,
            16,
            5 * RATE,
            pink,
            "pinknoise16.wav",
        )

    if missing("sine16.wav"):
        write_int(
            path("sine16.wav"),
            2,
            RATE,
            16,
            5 * RATE,
            lambda i, c: 0.45 * math.sin(math.tau * 440.0 * i / RATE + c * 0.15)
            + 0.25 * math.sin(math.tau * 554.365 * i / RATE + c * 0.4)
            + 0.12 * math.sin(math.tau * 880.0 * i / RATE),
            "sine16.wav",
        )

    if missing("music16.wav"):
        # A deterministic chord progression with overlapping plucked notes and
        # a little stereo detune. It exercises transients, sustained tones and
        # several simultaneous partials.
        duration = 9.0
        total = int(duration * RATE)
        chords = [
            [261.63, 329.63, 392.00],
            [220.00, 261.63, 329.63],
            [349.23, 440.00, 523.25],
            [196.00, 246.94, 293.66],
        ]
        notes = []
        step = 0.75
        index = 0
        while index * step < duration:
            chord = chords[(index // 4) % len(chords)]
            for note_index, base in enumerate(chord):
                start = index * step + note_index * 0.015
                notes.append((start, 1.1, base, 0.20, note_index))
            index += 1

        def music(i: int, c: int) -> float:
            t = i / RATE
            value = 0.0
            for start, length, base, amp, note_index in notes:
                local = t - start
                if 0.0 <= local < length:
                    attack = min(1.0, local / 0.02)
                    release = min(1.0, (length - local) / 0.25)
                    env = attack * release * math.exp(-2.5 * local)
                    frequency = base * (1.0 + (c * 2 - 1) * 0.0015)
                    value += amp * env * (
                        math.sin(math.tau * frequency * local)
                        + 0.35 * math.sin(math.tau * 2.0 * frequency * local)
                        + 0.15 * math.sin(math.tau * 3.0 * frequency * local)
                    )
            value += (RNG.uniform() * 2.0 - 1.0) * 0.002
            return clamp(value, -0.98, 0.98)

        write_int(path("music16.wav"), 2, RATE, 16, total, music, "music16.wav")

    if missing("sine24_96k.wav"):
        write_int(
            path("sine24_96k.wav"),
            2,
            96_000,
            24,
            3 * 96_000,
            lambda i, c: 0.5 * math.sin(math.tau * 1000.0 * i / 96_000 + c * 0.2),
            "sine24_96k.wav",
        )

    if missing("sine32_mono.wav"):
        write_int(
            path("sine32_mono.wav"),
            1,
            RATE,
            32,
            3 * RATE,
            lambda i, c: 0.6 * math.sin(math.tau * 440.0 * i / RATE),
            "sine32_mono.wav",
        )

    if missing("tone_f32.wav"):
        # A float file produced from 16-bit PCM: every value is an exact
        # multiple of 2^-15. This is a common real-world float layout.
        write_float(
            path("tone_f32.wav"),
            2,
            RATE,
            32,
            3 * RATE,
            lambda i, c: quantize_int(
                0.5 * math.sin(math.tau * 440.0 * i / RATE + c * 0.2), 16
            )
            / 32768.0,
            "tone_f32.wav",
        )

    if missing("tone_f64.wav"):
        write_float(
            path("tone_f64.wav"),
            2,
            RATE,
            64,
            3 * RATE,
            lambda i, c: quantize_int(
                0.5 * math.sin(math.tau * 440.0 * i / RATE + c * 0.2), 16
            )
            / 32768.0,
            "tone_f64.wav",
        )

    if missing("sine_f32.wav"):
        # Full-mantissa float32: generated in double precision and rounded to
        # float. This is the hard case for exact float compression.
        def full_f32(i: int, c: int) -> float:
            return struct.unpack(
                "<f",
                struct.pack(
                    "<f",
                    0.7 * math.sin(math.tau * 440.0 * i / RATE + c * 0.1),
                ),
            )[0]

        write_float(
            path("sine_f32.wav"), 2, RATE, 32, 3 * RATE, full_f32, "sine_f32.wav"
        )

    if missing("sine_f64.wav"):
        write_float(
            path("sine_f64.wav"),
            2,
            RATE,
            64,
            3 * RATE,
            lambda i, c: 0.7 * math.sin(math.tau * 440.0 * i / RATE + c * 0.1),
            "sine_f64.wav",
        )

    if missing("surround24_6ch.wav"):
        write_int(
            path("surround24_6ch.wav"),
            6,
            RATE,
            24,
            2 * RATE,
            lambda i, c: 0.3
            * math.sin(math.tau * (220.0 + 110.0 * c) * i / RATE + c * 0.3),
            "surround24_6ch.wav",
        )

    print("done")
    return 0


if __name__ == "__main__":
    sys.exit(main())
