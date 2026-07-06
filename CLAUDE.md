# Mini Downloader — developer guide

IDM/JDownloader-style download manager for Linux. **Rust + Tauri v2** GUI that
wraps **aria2** (engine) and **yt-dlp** (video), with a **Firefox** extension that
captures downloads over native messaging.

## Workspace layout

- `crates/minidl-ipc` — shared contract: `CaptureJob` DTO, bridge envelope, UDS path, protocol version.
- `crates/minidl-core` — GUI-agnostic engine: `aria2` (process + HTTP RPC + WebSocket notifications), `db` (rusqlite), `ytdlp` (probe), `grabber`, `categories`, `model`, `paths`.
- `crates/minidl-native-host` — tiny stdio binary Firefox launches; forwards jobs to the app over a Unix socket.
- `apps/desktop` — Tauri app. `src/` = Svelte 5 + Vite frontend; `src-tauri/src/` = commands, `ingest` (routing), `sync` (poller + notifications + reconcile + auto-organize), `ytdlp` (download driver), `nativehost` (UDS listener), `scheduler`, `clipboard`, `tray`.
- `extension/` — Firefox MV3 WebExtension.
- `packaging/`, `scripts/` — sidecar staging, native-host manifest, GPL notes.

## Architecture notes

- aria2 runs on a **random loopback port + per-launch secret** (never 6800). Request/reply over HTTP; a **read-only WebSocket** receives push notifications.
- The DB owns durable state; aria2 owns live transfer state. GIDs persist across restart via `--save-session`/`--input-file`; `sync::reconcile` re-maps on startup.
- Every download (aria2 or yt-dlp) is a DB row with a stable `id`; live progress `downloads:tick` events are **keyed by id**.
- Browser capture: extension → `minidl-native-host` (stdio) → **UDS** (`$XDG_RUNTIME_DIR/ldm/bridge.sock`, 0600) → app `ingest()`. Cookies/headers travel in `CaptureJob`.
- Sidecar resolution: bundled binary next to the app exe, else system `PATH`. In dev there are no sidecars, so system `aria2c`/`yt-dlp`/`ffmpeg` are used.

## Commands

```sh
# Setup (fresh Arch/CachyOS)
./scripts/install-arch.sh

# Dev
cd apps/desktop && pnpm install && pnpm tauri dev

# Build / test
cargo build                       # whole workspace
cargo test -p minidl-core            # unit tests
cargo test -p minidl-core --test engine_e2e -- --ignored   # live aria2 download (needs network)
apps/desktop && ./node_modules/.bin/svelte-check --tsconfig ./tsconfig.json
apps/desktop && ./node_modules/.bin/vite build

# Bundle (native)
cargo build --release -p minidl-native-host
cd apps/desktop && cargo tauri build --bundles deb,rpm,appimage
# For a portable AppImage, stage static sidecars first: ./scripts/stage-sidecars.sh
```

## Gotchas for automated work

- The Bash tool runs under **zsh**, not bash — write shell scripts to a file and run with `bash file.sh` (inline `for`/functions/`seq` can misbehave).
- Launching the GUI app from the Bash tool reports **exit 144** (harness signals the long-running process); the app is actually fine — check `pgrep`/logs, not the exit code. Avoid `pkill minidl-desktop` inside a compound command (it aborts the whole command).
- Frontend uses **plain Vite + Svelte 5** (not SvelteKit). `apps/desktop/.npmrc` sets `verify-deps-before-run=false` so esbuild's ignored build script doesn't fail `pnpm build`.
- **Build the run binary with `cargo tauri build` (or `--no-bundle`), NOT `cargo build --release`.** A plain `cargo build --release` binary loads the dev-server URL (`devUrl`, localhost:1420) instead of the embedded frontend — it shows a white "connection refused" page unless a `vite` dev server happens to be running. For iterating use `pnpm tauri dev`; for a standalone binary use `cargo tauri build`.
- **After moving/renaming the project folder, run `cargo clean`.** cargo/tauri-build bake absolute paths into the build cache; a stale cache causes build failures referencing the old path (e.g. `.../out/permissions/.../*.toml: No such file`).
- The extension is cross-browser: `extension/manifest.json` (Firefox: event page + blocking webRequest Path A + `allowed_extensions`) and `extension/manifest.chrome.json` (Chromium: service worker + Path B only, since Chromium MV3 dropped blocking webRequest; stable id via `key`). `scripts/build-extension.sh` assembles `dist-ext/{firefox,chrome}`. Firefox: load `manifest.json` via `about:debugging`. Chromium: Load unpacked `dist-ext/chrome`. The native-messaging host manifest is installed for every detected browser at first run (`install_browser_integration` in `nativehost.rs`) — Firefox family (`~/.mozilla`, `~/.zen`, …) with `allowed_extensions`, Chromium family (`~/.config/{google-chrome,chromium,BraveSoftware/Brave-Browser,…}/NativeMessagingHosts`) with `allowed_origins`.
- Changing the app icon: run `cargo tauri icon packaging/Mini Downloader.png`, then **force a recompile** (`touch apps/desktop/src-tauri/src/lib.rs`) before building — icon-file-only changes don't trigger `generate_context!` to re-embed.
