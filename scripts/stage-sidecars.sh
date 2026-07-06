#!/usr/bin/env bash
# Stage the aria2c / yt-dlp / ffmpeg sidecars for Tauri externalBin bundling.
# Downloads pinned binaries, verifies them, and renames with the Rust target
# triple suffix that Tauri expects (e.g. aria2c-x86_64-unknown-linux-gnu).
#
# For the CachyOS dev box the simplest source is the system packages you just
# installed — this script copies those and renames them. CI should instead
# download pinned release tarballs + verify SHA256 (see comments).
set -euo pipefail

BIN_DIR="apps/desktop/src-tauri/binaries"
TRIPLE="$(rustc -vV | awk -F': ' '/^host:/{print $2}')"
mkdir -p "$BIN_DIR"

echo "==> Target triple: $TRIPLE"
echo "==> Staging into: $BIN_DIR"

copy_bin() {
  local name="$1" src
  src="$(command -v "$name")" || { echo "!! $name not found — run scripts/install-arch.sh first"; exit 1; }
  install -m 0755 "$src" "$BIN_DIR/${name}-${TRIPLE}"
  echo "   staged $name -> ${name}-${TRIPLE}"
}

copy_bin aria2c
copy_bin yt-dlp
copy_bin ffmpeg

echo "==> Done."
# NOTE (CI/release): replace copy_bin with pinned downloads, e.g.
#   aria2c : build static (or fetch a reproducible static build), verify SHA256
#   yt-dlp : curl -L https://github.com/yt-dlp/yt-dlp/releases/download/<tag>/yt-dlp
#   ffmpeg : fetch a static build (e.g. johnvansickle), verify SHA256
# Always verify checksums and fail the build on mismatch.
