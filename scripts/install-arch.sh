#!/usr/bin/env bash
# LDM dev environment setup — CachyOS / Arch-family
# Installs everything needed to build the Rust+Tauri app, the aria2/yt-dlp/ffmpeg
# engine, and the Firefox extension tooling.
set -euo pipefail

echo "==> System update"
sudo pacman -Syu --noconfirm

echo "==> Base build tools + git"
sudo pacman -S --needed --noconfirm base-devel git openssl curl wget file

echo "==> Tauri v2 Linux dependencies"
# Tray icon uses libayatana-appindicator (maintained). If a Tauri build later
# complains about appindicator, swap for the AUR pkg: paru -S libappindicator-gtk3
sudo pacman -S --needed --noconfirm \
  webkit2gtk-4.1 \
  gtk3 \
  librsvg \
  libayatana-appindicator \
  appmenu-gtk-module \
  patchelf

echo "==> Download engine + media tools"
sudo pacman -S --needed --noconfirm aria2 yt-dlp ffmpeg

echo "==> Node toolchain (frontend)"
sudo pacman -S --needed --noconfirm nodejs npm pnpm

echo "==> Rust toolchain (rustup)"
if ! command -v rustup >/dev/null 2>&1; then
  sudo pacman -S --needed --noconfirm rustup
fi
rustup default stable
rustup update stable

echo "==> Tauri CLI (cargo) + Firefox extension tooling (web-ext)"
cargo install tauri-cli --version "^2" --locked
sudo npm install -g web-ext

echo "==> Git identity"
git config --global user.name  "RamazanBerk20"
git config --global user.email "ramazanberksirin@protonmail.com"
git config --global init.defaultBranch main

echo
echo "==> Installed versions:"
rustc --version
cargo --version
node --version
pnpm --version
aria2c --version | head -1
yt-dlp --version
ffmpeg -version | head -1
cargo tauri --version || true
echo "==> Done. Dev environment ready."
