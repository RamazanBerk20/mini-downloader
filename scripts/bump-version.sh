#!/usr/bin/env bash
# Bump the desktop app's version only. The browser extension has an independent
# store version and must be bumped/released separately. Run:
# ./scripts/bump-version.sh 1.2.3
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
  apps/desktop/src-tauri/tauri.conf.json

# Keep Cargo.lock in sync.
cargo check --workspace >/dev/null

echo "Bumped to $V."
echo "Next: after the GitHub release builds, update the stable AUR packages"
echo "with real checksums and regenerated .SRCINFO files."
