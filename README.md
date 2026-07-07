# Mini Downloader

[![build](https://github.com/RamazanBerk20/mini-downloader/actions/workflows/build.yml/badge.svg)](https://github.com/RamazanBerk20/mini-downloader/actions/workflows/build.yml)
[![release](https://img.shields.io/github/v/release/RamazanBerk20/mini-downloader?include_prereleases)](https://github.com/RamazanBerk20/mini-downloader/releases)
[![license](https://img.shields.io/badge/license-GPL--3.0--or--later-blue)](LICENSE)
[![sponsor](https://img.shields.io/badge/sponsor-%E2%9D%A4-e0245e)](https://github.com/sponsors/RamazanBerk20)

An IDM/JDownloader-style download manager for **Linux and Windows**, built with
Rust + Tauri. aria2 does the heavy lifting (multi-connection segmented
downloads, torrents, metalinks), yt-dlp grabs video (HLS/DASH, YouTube and
friends), and a browser extension captures downloads straight from Firefox and
Chromium — cookies, referer and all.

![Mini Downloader](docs/screenshot.png)

## Features

- **Multi-connection downloads** — up to 16 segments / 16 connections per
  server via aria2, with per-download pause/resume and global speed limits
- **Browser capture** — Firefox & Chromium extension hands downloads (with
  cookies/referer/user-agent) to the app through native messaging
- **Video grabbing** — yt-dlp integration: probe formats, pick quality, mux
  with ffmpeg; in-page media sniffing (HLS/DASH)
- **Torrents & magnets** — aria2's BitTorrent engine + magnet deep links
- **Link grabber** — paste text/HTML, extract and batch-add links
- **Scheduler** — pause/resume-all or speed-limit rules per weekday/time
- **Categories** — auto-organize finished files (Archives, Audio, Video, …)
  into localized user folders
- **Clipboard watcher**, system tray, autostart, desktop notifications
- **10 languages** — English, Türkçe, Español, Français, Deutsch, Русский,
  العربية (RTL), 中文, 日本語, 한국어
- Warm-monochrome minimal UI with full keyboard navigation and screen-reader
  support

## Install

### Linux

Grab from [Releases](https://github.com/RamazanBerk20/mini-downloader/releases):

- **Debian/Ubuntu**: `sudo apt install ./Mini.Downloader_*_amd64.deb`
- **Fedora/openSUSE**: `sudo rpm -i Mini.Downloader-*.x86_64.rpm`
- **AppImage**: `chmod +x Mini*Downloader*.AppImage && ./Mini*Downloader*.AppImage`
  (needs `aria2` installed; `yt-dlp` + `ffmpeg` for video)

deb/rpm depend on your distro's `aria2` + `ffmpeg` and recommend `yt-dlp`.

- **Arch (AUR)**: `paru -S mini-downloader-bin` (or `mini-downloader` to build
  from source) — see `packaging/aur/`

### Windows

Download the NSIS `.exe` installer (or `.msi`) from
[Releases](https://github.com/RamazanBerk20/mini-downloader/releases). aria2
and yt-dlp are bundled; install [ffmpeg](https://ffmpeg.org/download.html) and
put it on `PATH` if you want video muxing.

### Browser extension

Once published to the stores, install in one click from **Firefox Add-ons** /
the **Chrome Web Store** (buttons appear in Settings → Browser integration).
Until then, load it manually:

1. Download `mini-downloader-firefox-*.zip` / `mini-downloader-chrome-*.zip`
   from Releases (or build with `./scripts/build-extension.sh`).
2. **Firefox**: `about:debugging` → This Firefox → Load Temporary Add-on →
   pick `manifest.json` from the unzipped folder. For a *permanent* add-on,
   install the signed `.xpi` from the release if present (see
   `scripts/EXTENSION-PUBLISHING.md`).
   **Chromium/Chrome/Edge/Brave**: `chrome://extensions` → Developer mode →
   Load unpacked → the unzipped folder.
3. Start the app once — it registers the native-messaging host for every
   detected browser automatically (Settings → "Install native-messaging host"
   re-runs it).

## Build from source

```sh
# deps: rust, nodejs >= 22, pnpm, aria2, ffmpeg, yt-dlp, webkit2gtk-4.1 (Linux)
git clone https://github.com/RamazanBerk20/mini-downloader
cd mini-downloader/apps/desktop
pnpm install
pnpm tauri dev            # development
pnpm tauri build          # release bundles for your platform
```

`scripts/install-arch.sh` installs everything needed on Arch/CachyOS.
`docker compose run --rm build` produces Linux bundles reproducibly (Ubuntu
22.04 glibc floor).

## Architecture

```
extension (Firefox/Chromium MV3)
   └─ native messaging (stdio) ─ minidl-native-host
        └─ local socket (UDS / named pipe) ─ minidl-desktop (Tauri + Svelte 5)
             ├─ minidl-core: aria2 JSON-RPC engine, SQLite, categories, i18n
             ├─ aria2c subprocess (segmented HTTP, BitTorrent, metalink)
             └─ yt-dlp subprocess (HLS/DASH probe + download, ffmpeg mux)
```

## Sponsor

If Mini Downloader is useful to you, consider
[sponsoring development ❤](https://github.com/sponsors/RamazanBerk20).

## License

GPL-3.0-or-later. Bundles/uses [aria2](https://aria2.github.io/) (GPLv2),
[yt-dlp](https://github.com/yt-dlp/yt-dlp) (Unlicense) and
[ffmpeg](https://ffmpeg.org/) (LGPL/GPL).
