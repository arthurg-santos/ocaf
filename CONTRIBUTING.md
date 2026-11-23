# Contributing to OCAF

Thanks for your interest. OCAF is a pre-1.0 reference codec; changes that
improve correctness, robustness, documentation or reproducible benchmarks are
welcome.

## Ground rules

- Keep the decoder exact. A lossless codec is not allowed to "improve" a
  sample, round a float, or silently drop a bit pattern.
- Keep the format documented. A change to the bitstream requires a matching
  update to `FORMAT.md`, a test vector or round-trip test, and a note in
  `CHANGELOG.md`.
- Do not add external crates to `ocaf-core` without a strong reason. The core
  currently has zero dependencies by design.
- Do not use `unsafe`.
- Do not add fake benchmarks, synthetic "results" that were not measured, or
  comparisons against codecs that were not verified bit-exact.
- Preserve compatibility. A version-1 decoder must reject a stream it cannot
  decode rather than produce wrong audio.

## Build and test

Requirements: Rust 1.75 or newer.

```sh
cargo build
cargo build --release

cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace -- --test-threads=1
```

Tests are run single-threaded by convention because several tests allocate
large PCM buffers; they are deterministic and can be run with the default
thread count if desired. `cargo test --workspace` includes unit tests,
integration tests and the doc test.

For a quick end-to-end check:

```sh
cargo build --release
./target/release/ocaf benchmark /path/to/file.wav --iterations 3
./target/release/ocaf encode /path/to/file.wav /tmp/file.ocaf --force
./target/release/ocaf verify /tmp/file.ocaf
./target/release/ocaf decode /tmp/file.ocaf /tmp/file.decoded.wav --force
./target/release/ocaf compare /path/to/file.wav /tmp/file.ocaf
```

## Benchmarks

The corpus generator and comparison harness are part of the repository:

```sh
python3 scripts/generate_corpus.py --force
python3 scripts/benchmark.py --iterations 3
```

Only report external codec sizes when the decoded PCM is byte-identical.
`scripts/benchmark.py` enforces this and marks other results `lossy` or `n/a`.
If you add a codec or change corpus files, update `BENCHMARKS.md` with the
exact command and version. Do not commit generated `.wav`, `.ocaf` or
`testdata/bench` artifacts; they are ignored.

## Format changes

1. Decide whether the change is a new **minor** feature (skippable metadata,
   a new optional subframe type) or a **major** incompatibility.
2. Update `FORMAT.md` first. Include field offsets, widths, validation rules
   and an example if useful.
3. Update `format.rs` / `subframe.rs` / `float.rs` to match.
4. Add a unit test for the encoder/decoder symmetry and an integration test for
   the full stream.
5. Add a corruption test if the new structure has lengths, counts or indices.
6. Update `ARCHITECTURE.md` if the design changes.
7. Add a `CHANGELOG.md` entry.

New metadata chunks must be length- and CRC-delimited and safely skippable.
New subframe types must be rejected cleanly by older decoders, never guessed.

## Code style

- Run `cargo fmt --all`; `cargo fmt --all -- --check` must pass.
- `cargo clippy --workspace --all-targets -- -D warnings` must pass.
- Prefer explicit error messages with the frame/channel/sample context where
  available.
- Use `checked_*` or explicit wrapping arithmetic where extreme float keys or
  hostile lengths can reach integer limits. Do not rely on debug assertions for
  security checks.
- Keep public API docs current. `ocaf-core` is pre-1.0, but examples in
  `lib.rs` should compile.
- Avoid allocating in the decoder's hot loop when a reusable scratch buffer is
  practical. The encoder is allowed to favor clarity over speed.

## Commit and pull request conventions

- Keep commits focused: format, codec, CLI, docs, tests and benchmarks should
  not be mixed unless they are inseparable.
- Write commit messages in the imperative mood, for example:
  - `Add exact float exception subframe`
  - `Document frame and stream CRC layout`
  - `Fix 24-bit WAV sign extension`
- Include a short explanation of **why** in the commit body when the change is
  not obvious.
- Do not commit credentials, tokens, private keys, generated media or benchmark
  output.
- For a pull request, state what you tested and on which platform. Include the
  exact commands and, for compression changes, before/after sizes on the
  standard corpus.

## Releases

Every user-visible version gets a Git tag and a GitHub Release.

1. Update the workspace version in `Cargo.toml` and `CHANGELOG.md`.
2. Run the full format, Clippy and test suite.
3. Commit the version bump and push `main`.
4. Create an annotated tag and push it:

   ```sh
   git tag -a v0.2.0 -m "OCAF 0.2.0"
   git push origin v0.2.0
   ```

The `Release` workflow runs on every `v*` tag. It verifies that the tag matches
`Cargo.toml`, runs all checks, builds the Linux binary and the portable Windows
archive, writes a SHA-256 checksum file and publishes the GitHub Release with
those assets. Do not create a release from an untagged commit, and do not reuse
a version tag for different code.

## Reporting bugs

Please include:

- OCAF version (`ocaf --version`) and Rust version.
- The exact command line.
- Input format: sample rate, channels, bit depth, integer/float.
- Whether `ocaf verify` succeeds and the first error message if not.
- A small reproducing WAV if it can be shared; otherwise a description of how
  it was generated.
- For compression issues, the size and the encoder options.

Do not attach copyrighted audio you cannot redistribute. A generated or
truncated file is usually enough.

## License

By contributing, you agree that your contribution is licensed under the MIT
License, the same license as this project.
