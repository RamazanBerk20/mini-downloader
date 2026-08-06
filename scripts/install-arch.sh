#!/usr/bin/env bash
# Mini Downloader native development setup for Arch-family distributions.
# Review before running: this updates the system and installs packages.
set -euo pipefail

printf '==> Updating packages\n'
sudo pacman -Syu --noconfirm

printf '==> Installing native build/runtime dependencies\n'
sudo pacman -S --needed --noconfirm \
  base-devel \
  git \
  rustup \
  pkgconf \
  fontconfig \
  libxkbcommon \
  wayland \
  libx11 \
  libxcursor \
  libxrandr \
  libxi \
  libxcb \
  xdg-utils \
  appstream \
  desktop-file-utils \
  shellcheck \
  zip \
  unzip \
  aria2 \
  ffmpeg \
  yt-dlp

rustup toolchain install 1.97.1 --profile minimal --component clippy,rustfmt

if ! command -v cargo-audit >/dev/null 2>&1; then
  cargo install cargo-audit --locked --version 0.22.2
fi
if ! command -v cargo-about >/dev/null 2>&1; then
  cargo install cargo-about --locked --version 0.9.1 --features cli
fi

printf '\n==> Installed versions\n'
rustc --version
cargo --version
aria2c --version | head -1
yt-dlp --version
ffmpeg -version | head -1
printf '==> Native development environment ready.\n'
