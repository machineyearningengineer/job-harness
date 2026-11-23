#!/bin/sh
# One-line installer for Job Harness (macOS & Linux).
# Downloads the matching prebuilt release archive, verifies SHA256SUMS, and installs `job-harness` + `jh`.
set -eu

REPO="machineyearningengineer/job-harness"
PREFIX="${JOB_HARNESS_PREFIX:-$HOME/.local}"
BIN_DIR="$PREFIX/bin"
VERSION="${JOB_HARNESS_VERSION:-}"

need() {
  command -v "$1" >/dev/null 2>&1 || {
    printf 'Missing required tool: %s\n' "$1" >&2
    exit 1
  }
}

need curl
need tar

OS=$(uname -s)
ARCH=$(uname -m)
case "$OS/$ARCH" in
  Darwin/arm64|Darwin/aarch64) TARGET="aarch64-apple-darwin" ;;
  Darwin/x86_64)               TARGET="x86_64-apple-darwin" ;;
  Linux/aarch64|Linux/arm64)   TARGET="aarch64-unknown-linux-gnu" ;;
  Linux/x86_64|Linux/amd64)    TARGET="x86_64-unknown-linux-gnu" ;;
  *)
    printf 'No prebuilt binary for %s/%s. Install with Rust:\n  cargo install --git https://github.com/%s job-harness\n' "$OS" "$ARCH" "$REPO" >&2
    exit 1
    ;;
esac

if [ -z "$VERSION" ]; then
  VERSION=$(curl -fsSL "https://api.github.com/repos/$REPO/releases" \
    | sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' \
    | head -n 1)
fi
if [ -z "$VERSION" ]; then
  printf 'Could not determine latest release tag from GitHub. Install with:\n  cargo install --git https://github.com/%s job-harness\n' "$REPO" >&2
  exit 1
fi

ARCHIVE="job-harness-${VERSION}-${TARGET}.tar.gz"
BASE_URL="https://github.com/${REPO}/releases/download/${VERSION}"
TMPDIR=$(mktemp -d)
trap 'rm -rf "$TMPDIR"' EXIT

printf 'Downloading %s (%s)...\n' "$ARCHIVE" "$VERSION"
curl -fsSL "$BASE_URL/$ARCHIVE" -o "$TMPDIR/$ARCHIVE"
curl -fsSL "$BASE_URL/SHA256SUMS" -o "$TMPDIR/SHA256SUMS"

EXPECTED=$(awk -v f="$ARCHIVE" '$2 == f || $2 == ("*" f) { print $1; exit }' "$TMPDIR/SHA256SUMS")
if [ -z "$EXPECTED" ]; then
  printf 'Checksum entry for %s not found in SHA256SUMS.\n' "$ARCHIVE" >&2
  exit 1
fi
if command -v shasum >/dev/null 2>&1; then
  ACTUAL=$(shasum -a 256 "$TMPDIR/$ARCHIVE" | awk '{ print $1 }')
elif command -v sha256sum >/dev/null 2>&1; then
  ACTUAL=$(sha256sum "$TMPDIR/$ARCHIVE" | awk '{ print $1 }')
else
  printf 'Need shasum or sha256sum to verify download.\n' >&2
  exit 1
fi
if [ "$EXPECTED" != "$ACTUAL" ]; then
  printf 'SHA-256 mismatch for %s\nExpected: %s\nActual:   %s\n' "$ARCHIVE" "$EXPECTED" "$ACTUAL" >&2
  exit 1
fi

tar -xzf "$TMPDIR/$ARCHIVE" -C "$TMPDIR"
BIN_SRC=$(find "$TMPDIR" -type f -name "job-harness" | head -n 1)
if [ -z "$BIN_SRC" ]; then
  printf 'job-harness binary not found inside %s\n' "$ARCHIVE" >&2
  exit 1
fi

mkdir -p "$BIN_DIR"
rm -f "$BIN_DIR/job-harness" "$BIN_DIR/jh"
install -m 755 "$BIN_SRC" "$BIN_DIR/job-harness"
if [ "$OS" = "Darwin" ] && command -v codesign >/dev/null 2>&1; then
  codesign -s - --force "$BIN_DIR/job-harness" >/dev/null 2>&1 || true
fi
ln -sf "$BIN_DIR/job-harness" "$BIN_DIR/jh"

printf '\nInstalled %s (%s) and aliased `jh`.\n' "$BIN_DIR/job-harness" "$VERSION"
case ":$PATH:" in
  *":$BIN_DIR:"*) ;;
  *) printf 'Add to your PATH:\n  export PATH="%s:$PATH"\n' "$BIN_DIR" ;;
esac
printf 'Next steps:\n  jh setup --workspace "$HOME/Career/workspace"\n  jh --root "$HOME/Career/workspace" connect mcp\n'
