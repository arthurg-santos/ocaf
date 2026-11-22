#!/usr/bin/env python3
"""Reproducible size and speed benchmark for OCAF.

The harness generates/uses the corpus from scripts/generate_corpus.py, measures
OCAF encode/decode wall-clock time, and compares against FLAC, ALAC and WavPack
through ffmpeg. An external result is reported only when decoding it produces
byte-identical PCM; lossy conversions are labelled and excluded from the
comparison.

Usage:
    cargo build --release
    python3 scripts/benchmark.py [--corpus DIR] [--iterations 3] [--out DIR]

Outputs Markdown to stdout and a JSON record in the work directory.
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import struct
import subprocess
import sys
import tempfile
import time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
OCAF = os.path.join(ROOT, "target", "release", "ocaf")
CORPUS = os.path.join(ROOT, "testdata", "generated")

FILES = [
    ("silence16.wav", "int"),
    ("noise16.wav", "int"),
    ("pinknoise16.wav", "int"),
    ("sine16.wav", "int"),
    ("music16.wav", "int"),
    ("sine24_96k.wav", "int"),
    ("sine32_mono.wav", "int"),
    ("surround24_6ch.wav", "int"),
    ("tone_f32.wav", "float"),
    ("tone_f64.wav", "float"),
    ("sine_f32.wav", "float"),
    ("sine_f64.wav", "float"),
]


def parse_wav(path: str) -> dict:
    with open(path, "rb") as handle:
        blob = handle.read()
    if blob[:4] != b"RIFF" or blob[8:12] != b"WAVE":
        raise RuntimeError(f"{path}: not a RIFF/WAVE file")
    pos = 12
    info = {"channels": None, "rate": None, "bits": None, "float": False, "data": b""}
    while pos + 8 <= len(blob):
        chunk = blob[pos : pos + 4]
        size = struct.unpack("<I", blob[pos + 4 : pos + 8])[0]
        payload = blob[pos + 8 : pos + 8 + size]
        if chunk == b"fmt ":
            tag, channels, rate, _, _, bits = struct.unpack("<HHIIHH", payload[:16])
            if tag == 0xFFFE and len(payload) >= 40:
                subtag = struct.unpack("<H", payload[24:26])[0]
                tag = subtag
            info.update(
                channels=channels,
                rate=rate,
                bits=bits,
                float=tag == 3,
            )
        elif chunk == b"data":
            info["data"] = payload
        pos += 8 + size + (size & 1)
    if info["channels"] is None:
        raise RuntimeError(f"{path}: missing fmt chunk")
    return info


def pcm_codec(info: dict) -> str:
    if info["float"]:
        return "pcm_f32le" if info["bits"] == 32 else "pcm_f64le"
    return {16: "pcm_s16le", 24: "pcm_s24le", 32: "pcm_s32le"}[info["bits"]]


def run(command: list[str]) -> subprocess.CompletedProcess:
    return subprocess.run(command, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)


def timed(command: list[str], iterations: int) -> float:
    best = float("inf")
    for _ in range(iterations):
        start = time.perf_counter()
        result = run(command)
        elapsed = time.perf_counter() - start
        if result.returncode != 0:
            raise RuntimeError(f"{' '.join(command)} failed:\n{result.stderr[-2000:]}")
        best = min(best, elapsed)
    return best


def ffmpeg_has(encoder: str) -> bool:
    result = run(["ffmpeg", "-hide_banner", "-encoders"])
    if result.returncode != 0:
        return False
    return any(
        line.split()[1] == encoder
        for line in result.stdout.splitlines()
        if len(line.split()) > 1
    )


EXTERNAL = {
    "FLAC": ("flac", ".flac", ["-c:a", "flac", "-compression_level", "12"]),
    "ALAC": ("alac", ".m4a", ["-c:a", "alac"]),
    "WavPack": ("wavpack", ".wv", ["-c:a", "wavpack"]),
}


def encode_external(name: str, source: str, target: str, native_wavpack: bool) -> None:
    if name == "WavPack" and native_wavpack:
        # Very high quality plus extra mode 4. Mode 6 is slower and, on some
        # float material, produces larger files; mode 4 is deterministic here
        # and is the strongest reproducible setting on this corpus.
        result = run(["wavpack", "-q", "-y", "-hh", "-x4", source, "-o", target])
        if result.returncode != 0:
            raise RuntimeError(result.stderr.strip() or "wavpack failed")
        return
    _, _, options = EXTERNAL[name]
    result = run(["ffmpeg", "-y", "-v", "error", "-i", source, *options, target])
    if result.returncode != 0:
        raise RuntimeError(result.stderr.strip() or "ffmpeg failed")


def decode_external(
    name: str, encoded: str, target: str, info: dict, native_wavpack: bool
) -> None:
    if name == "WavPack" and native_wavpack:
        result = run(["wvunpack", "-q", "-y", encoded, "-o", target])
        if result.returncode != 0:
            raise RuntimeError(result.stderr.strip() or "wvunpack failed")
        return
    result = run(
        [
            "ffmpeg",
            "-y",
            "-v",
            "error",
            "-i",
            encoded,
            "-c:a",
            pcm_codec(info),
            target,
        ]
    )
    if result.returncode != 0:
        raise RuntimeError(result.stderr.strip() or "ffmpeg failed")


def kib(value: int) -> str:
    return f"{value / 1024.0:.1f}"


def mbps(pcm_bytes: int, seconds: float) -> float:
    return pcm_bytes / (1024.0 * 1024.0) / seconds if seconds > 0 else 0.0


def measure_ocaf(
    source: str, info: dict, work: str, iterations: int
) -> tuple[int, float, float]:
    stream = os.path.join(work, "out.ocaf")
    decoded = os.path.join(work, "decoded.wav")
    enc = timed([OCAF, "encode", source, stream, "--force"], iterations)
    size = os.path.getsize(stream)
    dec = timed([OCAF, "decode", stream, decoded, "--force"], iterations)
    if parse_wav(decoded)["data"] != info["data"]:
        raise RuntimeError(f"{source}: OCAF decode is not bit-exact")
    verify = run([OCAF, "verify", stream])
    if verify.returncode != 0:
        raise RuntimeError(f"{source}: OCAF verify failed:\n{verify.stderr}")
    return size, enc, dec


def measure_external(
    name: str,
    source: str,
    info: dict,
    work: str,
    iterations: int,
    native_wavpack: bool,
) -> dict:
    _, extension, _ = EXTERNAL[name]
    encoded = os.path.join(work, f"out{extension}")
    decoded = os.path.join(work, "decoded_ext.wav")
    for path in (encoded, decoded):
        if os.path.exists(path):
            os.remove(path)
    method = "native -hh -x4" if name == "WavPack" and native_wavpack else "ffmpeg"
    try:
        encode_external(name, source, encoded, native_wavpack)
        size = os.path.getsize(encoded)
        dec = timed(
            [
                "wvunpack",
                "-q",
                "-y",
                encoded,
                "-o",
                decoded,
            ]
            if name == "WavPack" and native_wavpack
            else [
                "ffmpeg",
                "-y",
                "-v",
                "error",
                "-i",
                encoded,
                "-c:a",
                pcm_codec(info),
                decoded,
            ],
            max(1, min(iterations, 2)),
        )
        exact = parse_wav(decoded)["data"] == info["data"]
        return {"size": size, "decode": dec, "exact": exact, "method": method}
    except RuntimeError as exc:
        return {"error": str(exc), "exact": False, "method": method}


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--corpus", default=CORPUS)
    parser.add_argument("--iterations", type=int, default=3)
    parser.add_argument("--out", default=os.path.join(ROOT, "testdata", "bench"))
    parser.add_argument("--no-external", action="store_true")
    args = parser.parse_args()

    if not os.path.exists(OCAF):
        print(f"missing {OCAF}; run `cargo build --release` first", file=sys.stderr)
        return 1
    if args.iterations < 1:
        print("iterations must be at least 1", file=sys.stderr)
        return 1
    missing = [name for name, _ in FILES if not os.path.exists(os.path.join(args.corpus, name))]
    if missing:
        print(
            "missing corpus files: "
            + ", ".join(missing)
            + "\nrun `python3 scripts/generate_corpus.py` first",
            file=sys.stderr,
        )
        return 1
    os.makedirs(args.out, exist_ok=True)
    work = tempfile.mkdtemp(prefix="ocaf-bench-")
    ffmpeg = shutil.which("ffmpeg")
    native_wavpack = bool(shutil.which("wavpack") and shutil.which("wvunpack"))
    available = {}
    if not args.no_external:
        for name, (encoder, _, _) in EXTERNAL.items():
            if name == "WavPack":
                available[name] = native_wavpack or bool(ffmpeg and ffmpeg_has(encoder))
            else:
                available[name] = bool(ffmpeg and ffmpeg_has(encoder))
    else:
        available = {name: False for name in EXTERNAL}

    records = []
    for name, kind in FILES:
        source = os.path.join(args.corpus, name)
        info = parse_wav(source)
        pcm_bytes = len(info["data"])
        ocaf_size, enc, dec = measure_ocaf(source, info, work, args.iterations)
        row = {
            "file": name,
            "kind": kind,
            "channels": info["channels"],
            "rate": info["rate"],
            "bits": info["bits"],
            "float": info["float"],
            "pcm_bytes": pcm_bytes,
            "ocaf_bytes": ocaf_size,
            "ocaf_encode_s": enc,
            "ocaf_decode_s": dec,
            "external": {},
        }
        if any(available.values()):
            for ext_name in EXTERNAL:
                if not available[ext_name]:
                    row["external"][ext_name] = {"error": "encoder not available"}
                    continue
                row["external"][ext_name] = measure_external(
                    ext_name, source, info, work, args.iterations, native_wavpack
                )
        records.append(row)
        ext_text = []
        for ext_name in EXTERNAL:
            result = row["external"].get(ext_name)
            if not result:
                continue
            if "error" in result:
                ext_text.append(f"{ext_name}=unsupported")
            elif result["exact"]:
                ratio = pcm_bytes / result["size"]
                ext_text.append(f"{ext_name}={kib(result['size'])} KiB ({ratio:.2f}x)")
            else:
                ext_text.append(f"{ext_name}=lossy")
        print(
            f"{name}: {kib(pcm_bytes)} KiB -> {kib(ocaf_size)} KiB OCAF "
            f"({pcm_bytes / ocaf_size:.2f}x), enc {mbps(pcm_bytes, enc):.1f} MB/s, "
            f"dec {mbps(pcm_bytes, dec):.1f} MB/s"
            + (f"; {', '.join(ext_text)}" if ext_text else ""),
            flush=True,
        )

    record_path = os.path.join(args.out, "benchmark.json")
    with open(record_path, "w", encoding="utf-8") as handle:
        json.dump(records, handle, indent=2)
    shutil.rmtree(work, ignore_errors=True)

    print()
    print("Sizes are KiB of the file; speed is decoded/encoded PCM MiB per second.")
    print()
    print("| File | PCM | OCAF | Ratio | Enc MB/s | Dec MB/s | FLAC | ALAC | WavPack |")
    print("| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |")
    for row in records:
        cells = []
        for ext_name in EXTERNAL:
            result = row["external"].get(ext_name)
            if not result:
                cells.append("—")
            elif "error" in result:
                cells.append("n/a")
            elif result["exact"]:
                cells.append(f"{kib(result['size'])} ({row['pcm_bytes'] / result['size']:.2f}x)")
            else:
                cells.append("lossy")
        print(
            "| {file} | {pcm} | {ocaf} | {ratio:.2f}x | {enc:.1f} | {dec:.1f} | "
            "{flac} | {alac} | {wavpack} |".format(
                file=row["file"],
                pcm=kib(row["pcm_bytes"]),
                ocaf=kib(row["ocaf_bytes"]),
                ratio=row["pcm_bytes"] / row["ocaf_bytes"],
                enc=mbps(row["pcm_bytes"], row["ocaf_encode_s"]),
                dec=mbps(row["pcm_bytes"], row["ocaf_decode_s"]),
                flac=cells[0],
                alac=cells[1],
                wavpack=cells[2],
            )
        )
    print()
    print(f"JSON record: {record_path}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
