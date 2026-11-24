# Windows builds

OCAF is a portable Rust program. The same codec and format run on Linux,
macOS and Windows; only the command-line invocation differs. The Windows
release is a single `ocaf.exe` with no installer and no MinGW runtime DLLs.

## Requirements

- Windows 10 or 11, 64-bit.
- For building: Rust 1.75 or newer with Cargo.
- For cross-compiling from Linux: the `x86_64-pc-windows-gnu` Rust target and
  a MinGW-w64 linker.
- For encoding MP3, FLAC, AAC/M4A and other non-WAV inputs: FFmpeg
  (`ffmpeg.exe` and `ffprobe.exe`) installed and available in `PATH`.

## Build natively on Windows

```powershell
git clone https://github.com/arthurg-santos/ocaf.git
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
dist/ocaf-0.3.0-windows-x86_64.zip
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

The external-format tests skip themselves when `ffmpeg` and `ffprobe` are not
available. To run them under Wine, point `WINEPATH` at a Windows FFmpeg build:

```sh
export WINEPATH="/path/to/ffmpeg/bin"
cargo test --target x86_64-pc-windows-gnu -p ocaf-cli --test cli -- --test-threads=1
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

## Encoding MP3, FLAC and other formats

`ocaf.exe` reads WAV natively. For other formats it uses FFmpeg to decode the
audio to PCM and then encodes that PCM into OCAF. Install FFmpeg so that
`ffmpeg.exe` and `ffprobe.exe` are in `PATH`:

```powershell
winget install Gyan.FFmpeg
# or: choco install ffmpeg -y
```

Open a new terminal after installing, then:

```powershell
ocaf.exe encode musica.flac musica.ocaf
ocaf.exe encode musica.mp3 musica.ocaf
ocaf.exe encode musica.mp3 musica-s16.ocaf --pcm-format s16
ocaf.exe encode "C:\Videos\show.mp4" audio.ocaf
```

If FFmpeg is not in `PATH`, pass the executables explicitly:

```powershell
ocaf.exe encode musica.flac musica.ocaf `
  --ffmpeg "C:\ffmpeg\bin\ffmpeg.exe" `
  --ffprobe "C:\ffmpeg\bin\ffprobe.exe"
```

OCAF stores the decoded PCM, not the original compressed bitstream. FLAC,
ALAC and AIFF keep their PCM exactly. Lossy sources default to the decoder's
32-bit float output, which is bit-exact relative to that decode but can be
much larger than the source. Use `--pcm-format s16` for a smaller archive;
that conversion is explicitly lossy. Container tags are not imported
automatically, so use `--title`, `--artist`, `--album` and `--meta` if needed.

The portable zip still contains only `ocaf.exe`; FFmpeg is a separate
installation and is not redistributed with OCAF.

## Playing audio

`ocaf.exe play musica.ocaf` decodes the file and hands it to an installed
player. OCAF tries `ffplay`, `mpv` and `vlc` first, then falls back to the
PowerShell `SoundPlayer`. The PowerShell fallback supports integer PCM WAV but
not floating-point WAV; install `ffplay`, `mpv` or `vlc` to play float files.
Use `--player PATH` to select a different executable.

Playback starts only after the whole stream has been decoded and verified, so
there is a delay before sound starts on long files.

## Limits

- The prebuilt archive is `x86_64` only. It does not run on 32-bit Windows.
- Non-WAV input needs a separate FFmpeg installation with both `ffmpeg.exe`
  and `ffprobe.exe`. Without FFmpeg, only WAV input works.
- Decoding regenerates a canonical WAV header; the PCM audio is bit-exact.
- `play` needs an installed system player and is not a streaming player.
- The binary is portable but unsigned. There is no MSI installer or
  auto-updater.

## GitHub Actions

`.github/workflows/windows.yml` builds and tests the project on
`windows-latest` and uploads a zip artifact on every push and pull request.
The artifact is available from the repository's Actions page; GitHub may
require an account to download it. Artifacts expire according to GitHub's
retention policy, so keep your own copy of the zip if you need a permanent
build.
