#!/usr/bin/env bash
# After a release is published: copy the source and binary tarball checksums
# from its AUR_CHECKSUMS asset into the stable AUR packages, so nobody builds
# them with `sha256sums=('SKIP')`.
set -euo pipefail

project_root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$project_root"
version="$(sed -n 's/^pkgver=//p' packaging/aur/mini-downloader/PKGBUILD)"
sums="$(gh release download "v$version" --repo RamazanBerk20/mini-downloader \
  --pattern AUR_CHECKSUMS --output -)"

fill() {
  local package="$1" pattern="$2" sum
  sum="$(printf '%s\n' "$sums" | awk -v pattern="$pattern" '$2 ~ pattern { print $1 }')"
  if ! [[ "$sum" =~ ^[0-9a-f]{64}$ ]]; then
    printf 'no checksum for %s in AUR_CHECKSUMS of v%s\n' "$package" "$version" >&2
    exit 1
  fi
  sed -i -E "s/^sha256sums=.*/sha256sums=('$sum')/" "packaging/aur/$package/PKGBUILD"
  sed -i -E "s/^(\tsha256sums = ).*/\1$sum/" "packaging/aur/$package/.SRCINFO"
}

fill mini-downloader "/archive/refs/tags/v${version}[.]tar[.]gz\$"
fill mini-downloader-bin "_linux-x86_64[.]tar[.]gz\$"
printf 'AUR checksums updated for v%s\n' "$version"
