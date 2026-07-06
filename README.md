# Mini Downloader (Mini Downloader)

An IDM/JDownloader-class download manager for Linux, with a Firefox extension
that catches downloads. Rust + Tauri v2 GUI wrapping **aria2** as the engine,
plus **yt-dlp** for video/stream grab.

- Segmented HTTP(S) downloads with resume (aria2)
- BitTorrent + magnet + Metalink (aria2)
- Firefox download capture with full cookie/header fidelity (native messaging)
- Video/stream grab (yt-dlp + ffmpeg)
- JDownloader-style link grabber, scheduler, speed limits, clipboard monitor
- Category auto-organize of finished files

See the full architecture + roadmap in the plan:
`~/.claude/plans/internet-download-manager-or-stateless-hopper.md`

## Development setup

**CachyOS / Arch host:**
```sh
./scripts/install-arch.sh        # system deps, rust, node, aria2/yt-dlp/ffmpeg, tauri-cli, web-ext
./scripts/stage-sidecars.sh      # stage aria2c/yt-dlp/ffmpeg into src-tauri/binaries/
```

**Reproducible bundle builds (Docker):**
```sh
docker compose run --rm build    # produces AppImage + .deb + .rpm
```
Docker is for builds/CI only — test Firefox native-messaging on the host.

## License

GPL-3.0-or-later (bundles GPLv2 aria2; see `packaging/aria2/`).
