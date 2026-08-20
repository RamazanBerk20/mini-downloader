#!/usr/bin/env bash
# Build native Linux release archives. If fpm and linuxdeploy are present,
# distro packages and an AppImage are produced as well.
set -euo pipefail

project_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$project_root"

version="$(sed -nE 's/^version = "([0-9]+\.[0-9]+\.[0-9]+)"/\1/p' Cargo.toml | head -1)"
[[ -n "$version" ]] || { printf 'Could not read workspace version.\n' >&2; exit 1; }
[[ "$(uname -m)" == "x86_64" ]] || {
  printf 'This packaging script currently produces x86_64 artifacts only.\n' >&2
  exit 1
}

artifact_dir="${MINIDL_ARTIFACT_DIR:-$project_root/dist-native}"
stage_dir="$artifact_dir/linux-stage"
app_dir="$artifact_dir/Mini_Downloader.AppDir"

# Keep release panic metadata and object files independent of the builder's
# username and checkout location. CARGO_ENCODED_RUSTFLAGS preserves arguments
# even when a path contains whitespace.
encoded_separator=$'\x1f'
cargo_home="${CARGO_HOME:-${HOME:?}/.cargo}"
release_rustflags="${CARGO_ENCODED_RUSTFLAGS:-}"
if [[ -n "$release_rustflags" ]]; then
  release_rustflags+="$encoded_separator"
fi
release_rustflags+="--remap-path-prefix=$project_root=."
release_rustflags+="$encoded_separator--remap-path-prefix=$cargo_home=/cargo"
export CARGO_ENCODED_RUSTFLAGS="$release_rustflags"

cargo build --locked --release -p minidl-native-desktop -p minidl-native-host

rm -rf -- "$stage_dir" "$app_dir"
install -Dm755 target/release/mini-downloader "$stage_dir/usr/bin/mini-downloader"
install -Dm755 target/release/minidl-native-host "$stage_dir/usr/bin/minidl-native-host"
install -Dm644 assets/icons/512x512.png \
  "$stage_dir/usr/share/icons/hicolor/512x512/apps/mini-downloader.png"
install -Dm644 packaging/linux/io.minidownloader.app.desktop \
  "$stage_dir/usr/share/applications/io.minidownloader.app.desktop"
install -Dm644 packaging/linux/io.minidownloader.app.metainfo.xml \
  "$stage_dir/usr/share/metainfo/io.minidownloader.app.metainfo.xml"
install -Dm644 packaging/com.minidownloader.host.json \
  "$stage_dir/usr/lib/mozilla/native-messaging-hosts/com.minidownloader.host.json"
install -Dm644 LICENSE \
  "$stage_dir/usr/share/licenses/mini-downloader/LICENSE"
install -Dm644 PRIVACY.md SECURITY.md THIRD_PARTY_NOTICES.md THIRD_PARTY_LICENSES.txt \
  -t "$stage_dir/usr/share/doc/mini-downloader"

mkdir -p "$artifact_dir"
source_date_epoch="${SOURCE_DATE_EPOCH:-$(git log -1 --format=%ct 2>/dev/null || date +%s)}"
[[ "$source_date_epoch" =~ ^[0-9]+$ ]] || {
  printf 'SOURCE_DATE_EPOCH must be an integer.\n' >&2
  exit 1
}
tar --sort=name --mtime="@$source_date_epoch" --owner=0 --group=0 \
  --numeric-owner -C "$stage_dir" -cf - . \
  | gzip -n > "$artifact_dir/Mini.Downloader_${version}_linux-x86_64.tar.gz"

if command -v fpm >/dev/null 2>&1; then
  common=(
    --input-type dir
    --name mini-downloader
    --version "$version"
    --iteration 1
    --architecture x86_64
    --license GPL-3.0-or-later
    --description "Fast, local-first download manager powered by aria2 and yt-dlp"
    --url https://github.com/RamazanBerk20/mini-downloader
    --maintainer "RamazanBerk20 <ramazanberksirin@protonmail.com>"
    --depends xdg-utils
    --after-install packaging/linux/postinstall.sh
    --after-remove packaging/linux/postremove.sh
    --package "$artifact_dir"
    --chdir "$stage_dir"
  )
  fpm "${common[@]}" --output-type deb \
    --depends aria2 --depends libfontconfig1 --depends libx11-6 \
    --depends libx11-xcb1 --depends libxcb1 --depends libxcursor1 \
    --depends libxrandr2 --depends libxi6 --depends libxkbcommon0 \
    --depends libxkbcommon-x11-0 --depends libwayland-client0 \
    .
  fpm "${common[@]}" --output-type rpm \
    --depends aria2 --depends fontconfig --depends libX11 \
    --depends libX11-xcb --depends libxcb --depends libXcursor \
    --depends libXrandr --depends libXi --depends libxkbcommon \
    --depends wayland-libs \
    .
fi

if command -v linuxdeploy >/dev/null 2>&1; then
  cp -a "$stage_dir" "$app_dir"
  export OUTPUT="$artifact_dir/Mini.Downloader_${version}_x86_64.AppImage"
  linuxdeploy \
    --appdir "$app_dir" \
    --desktop-file packaging/linux/io.minidownloader.app.desktop \
    --icon-file "$stage_dir/usr/share/icons/hicolor/512x512/apps/mini-downloader.png" \
    --output appimage
fi

printf 'Native Linux artifacts: %s\n' "$artifact_dir"
