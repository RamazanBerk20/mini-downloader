#!/usr/bin/env bash
# Single-source version bump across every file that hard-codes it, so an
# extension manifest can't silently drift from the app version (which would
# break native-messaging version checks). Run: ./scripts/bump-version.sh 1.2.3
set -euo pipefail

V="${1:?usage: bump-version.sh X.Y.Z}"
if ! [[ "$V" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "version must be X.Y.Z" >&2
  exit 1
fi
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"

sed -i -E 's/^version = "[0-9]+\.[0-9]+\.[0-9]+"/version = "'"$V"'"/' Cargo.toml
sed -i -E 's/("version": ")[0-9]+\.[0-9]+\.[0-9]+"/\1'"$V"'"/' \
  apps/desktop/package.json \
  apps/desktop/src-tauri/tauri.conf.json \
  extension/manifest.json \
  extension/manifest.chrome.json
sed -i -E 's/^pkgver=[0-9]+\.[0-9]+\.[0-9]+/pkgver='"$V"'/' packaging/aur/*/PKGBUILD
sed -i -E 's/^pkgrel=[0-9]+/pkgrel=1/' packaging/aur/*/PKGBUILD

# Keep Cargo.lock in sync.
cargo update -w >/dev/null 2>&1 || cargo check --workspace >/dev/null 2>&1 || true

echo "Bumped to $V."
echo "Next: after the GitHub release builds, run updpkgsums + 'makepkg --printsrcinfo > .SRCINFO'"
echo "in each packaging/aur/* dir and push to the AUR."
