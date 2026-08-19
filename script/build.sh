#!/usr/bin/env bash
# Build and package cmake-tui-tool for Linux / macOS.
# Usage: ./script/build.sh
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

NAME="cmake-tui-tool"
OS="$(uname -s)"
ARCH="$(uname -m)"

case "$OS" in
    Linux)  PLATFORM="linux" ;;
    Darwin) PLATFORM="macos" ;;
    *)
        echo "Unsupported OS: $OS" >&2
        exit 1
        ;;
esac

VERSION="$(grep -m1 '^version' Cargo.toml | sed -E 's/.*"([^"]+)".*/\1/')"

echo "Building $NAME v$VERSION (release) for $PLATFORM/$ARCH ..."
cargo build --release

BIN="$ROOT/target/release/$NAME"
if [ ! -f "$BIN" ]; then
    echo "Build finished but executable not found: $BIN" >&2
    exit 1
fi
echo "Executable: $BIN"

DIST="$ROOT/target/dist"
mkdir -p "$DIST"
ARCHIVE="$DIST/${NAME}-${VERSION}-${PLATFORM}-${ARCH}.tar.gz"
tar -czf "$ARCHIVE" -C "$ROOT/target/release" "$NAME"
echo "Packaged: $ARCHIVE"
