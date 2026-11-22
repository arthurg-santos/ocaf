# Windows builds

OCAF is a portable Rust program. The same codec and format run on Linux,
macOS and Windows; only the command-line invocation differs. The Windows
release is a single `ocaf.exe` with no installer and no MinGW runtime DLLs.

## Requirements

- Windows 10 or 11, 64-bit.
- For building: Rust 1.75 or newer with Cargo.
- For cross-compiling from Linux: the `x86_64-pc-windows-gnu` Rust target and
  a MinGW-w64 linker.

## Build natively on Windows

```powershell
git clone git@github.com:arthurg-santos/ocaf.git
cd ocaf
cargo build --release
.\target\release\ocaf.exe --version
```

The MSVC target is the default on Windows and is built by the standard Rust
toolchain. No Visual Studio project is needed.

## Cross-compile from Linux

Fedora:

```sh
sudo dnf install mingw64-gcc
rustup target add x86_64-pc-windows-gnu
./scripts/build-windows.sh
```

Debian/Ubuntu:

```sh
sudo apt install gcc-mingw-w64-x86-64
rustup target add x86_64-pc-windows-gnu
./scripts/build-windows.sh
```

The script builds `ocaf.exe`, creates a portable zip in `dist/`, and writes a
SHA-256 checksum. The current archive is:

```text
dist/ocaf-0.1.1-windows-x86_64.zip
```

## Test on Linux with Wine

Wine is optional, but it lets you run the Windows binary and the Windows test
suite on a Linux machine:

```sh
export WINEPREFIX=/tmp/ocaf-wine
export WINEDEBUG=-all
export CARGO_TARGET_X86_64_PC_WINDOWS_GNU_RUNNER=wine

rustup target add x86_64-pc-windows-gnu
cargo test --target x86_64-pc-windows-gnu --workspace -- --test-threads=1
wine target/x86_64-pc-windows-gnu/release/ocaf.exe --version
```

The release binary imports only standard Windows system DLLs
(`KERNEL32.dll`, `msvcrt.dll`, `ntdll.dll` and
`api-ms-win-core-synch-l1-2-0.dll`); it does not need
`libgcc_s_seh-1.dll` or `libwinpthread-1.dll`.

## Package contents

`scripts/build-windows.sh` creates:

```text
ocaf.exe
LEIA-ME.txt      Portuguese quick-start for testers
LICENSE.txt      MIT license
SHA256SUMS.txt   checksum for ocaf.exe
BUILD.txt        version, target and source commit
```

## Sending it to testers

Send the zip, not the bare `.exe`, so the recipient also gets the checksum and
usage notes. The executable is not code-signed, so Windows SmartScreen may
show a warning:

1. Click **More info**.
2. Click **Run anyway**.

Only distribute the file to people who trust the build. To verify a download:

```powershell
Get-FileHash .\ocaf.exe -Algorithm SHA256
```

Compare the result with `SHA256SUMS.txt`.

## Limits

- The prebuilt archive is `x86_64` only. It does not run on 32-bit Windows.
- OCAF accepts WAV input, not MP3/AAC/FLAC directly. Convert those to WAV
  first if needed.
- Decoding regenerates a canonical WAV header; the PCM audio is bit-exact.
- The binary is portable but unsigned. There is no MSI installer or
  auto-updater.

## GitHub Actions

`.github/workflows/windows.yml` builds and tests the project on
`windows-latest` and uploads a zip artifact on every push and pull request.
The workflow artifact is visible to users who have access to this private
repository.
