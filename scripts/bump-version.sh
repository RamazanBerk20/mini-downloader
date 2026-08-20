#!/usr/bin/env bash
# Keep every shipped manifest and packaging template on one release version.
set -euo pipefail

version="${1:?usage: bump-version.sh X.Y.Z}"
if ! [[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  printf 'version must be X.Y.Z\n' >&2
  exit 1
fi
if ! command -v cargo-about >/dev/null 2>&1; then
  printf 'cargo-about 0.9.1 is required to refresh THIRD_PARTY_LICENSES.txt.\n' >&2
  printf 'Install it with: cargo install cargo-about --locked --version 0.9.1 --features cli\n' >&2
  exit 1
fi
if ! command -v npm >/dev/null 2>&1; then
  printf 'npm is required to update the pinned extension publisher lockfile.\n' >&2
  exit 1
fi
project_root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$project_root"

sed -i -E \
  's/^version = "[0-9]+\.[0-9]+\.[0-9]+"/version = "'"$version"'"/' \
  Cargo.toml
sed -i -E 's/"version": "[0-9]+\.[0-9]+\.[0-9]+"/"version": "'"$version"'"/' \
  extension/manifest.json extension/manifest.chrome.json
npm version --prefix tools/extension-publisher --no-git-tag-version \
  --allow-same-version "$version" >/dev/null
for package in packaging/aur/mini-downloader{,-bin,-git}/PKGBUILD; do
  sed -i -E 's/^pkgver=.*/pkgver='"$version"'/' "$package"
  sed -i -E 's/^pkgrel=.*/pkgrel=1/' "$package"
done
for metadata in packaging/aur/mini-downloader{,-bin,-git}/.SRCINFO; do
  sed -i -E 's/^(\s*pkgver = ).*/\1'"$version"'/' "$metadata"
done
release_date="$(date -u +%F)"
sed -i -E '0,/<release version="[0-9]+\.[0-9]+\.[0-9]+" date="[0-9]{4}-[0-9]{2}-[0-9]{2}">/s//<release version="'"$version"'" date="'"$release_date"'">/' \
  packaging/linux/io.minidownloader.app.metainfo.xml

cargo check --workspace >/dev/null
cargo check --workspace --locked >/dev/null
cargo about generate --workspace --locked --fail about.hbs \
  -o THIRD_PARTY_LICENSES.txt

if command -v makepkg >/dev/null 2>&1; then
  for directory in packaging/aur/mini-downloader{,-bin,-git}; do
    (cd "$directory" && makepkg --printsrcinfo > .SRCINFO)
  done
fi

printf 'Bumped application, extension, and packaging manifests to %s.\n' "$version"
printf 'Stable AUR checksums must be filled from the published release assets before AUR submission.\n'
