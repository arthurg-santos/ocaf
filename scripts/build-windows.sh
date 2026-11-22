#!/usr/bin/env bash
# Build a portable Windows x86_64 release archive from Linux.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

target="x86_64-pc-windows-gnu"
version="$(sed -n 's/^version = "\([0-9][0-9.]*\)"$/\1/p' Cargo.toml | head -n1)"
if [[ -z "$version" ]]; then
    echo "error: could not read the workspace version from Cargo.toml" >&2
    exit 1
fi

if ! command -v rustup >/dev/null 2>&1; then
    echo "error: rustup is required to install the Windows target" >&2
    exit 1
fi
if ! rustup target list --installed | grep -qx "$target"; then
    rustup target add "$target"
fi
if ! command -v zip >/dev/null 2>&1; then
    echo "error: zip is required to create the release archive" >&2
    exit 1
fi
if ! command -v x86_64-w64-mingw32-gcc >/dev/null 2>&1; then
    cat >&2 <<'EOF'
error: x86_64-w64-mingw32-gcc was not found.
Install a MinGW-w64 toolchain first:
  Fedora:        sudo dnf install mingw64-gcc
  Debian/Ubuntu: sudo apt install gcc-mingw-w64-x86-64
EOF
    exit 1
fi

cargo build --release --target "$target"

staging="$(mktemp -d "${TMPDIR:-/tmp}/ocaf-windows.XXXXXX")"
trap 'rm -rf "$staging"' EXIT

cp "target/$target/release/ocaf.exe" "$staging/ocaf.exe"
cp LICENSE "$staging/LICENSE.txt"
cp packaging/windows/LEIA-ME.txt "$staging/LEIA-ME.txt"
{
    echo "OCAF $version"
    echo "target: $target"
    echo "built: $(date -u '+%Y-%m-%dT%H:%M:%SZ')"
    echo "commit: $(git rev-parse --short HEAD 2>/dev/null || echo unknown)"
    echo "rustc: $(rustc --version)"
} > "$staging/BUILD.txt"

(
    cd "$staging"
    sha256sum ocaf.exe > SHA256SUMS.txt
)

mkdir -p dist
archive="$root/dist/ocaf-$version-windows-x86_64.zip"
rm -f "$archive"
(
    cd "$staging"
    zip -q -9 "$archive" ocaf.exe LEIA-ME.txt LICENSE.txt SHA256SUMS.txt BUILD.txt
)

echo "Created $archive"
(
    cd "$root/dist"
    sha256sum "$(basename "$archive")"
)
