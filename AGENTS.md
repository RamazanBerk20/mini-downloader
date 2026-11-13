# Working on Mini Downloader

Guide for coding agents and new contributors. `README.md` covers what the
product is; this file covers how to change it safely.

## Layout

| Path | Role |
| --- | --- |
| `crates/minidl-ipc` | Wire types only: `CaptureJob`, `BridgeRequest`, `ControlCommand`, socket paths. No logic. |
| `crates/minidl-core` | SQLite (`db/`), aria2 process + RPC (`aria2/`), yt-dlp probe, categories, link grabber. No UI, no event bus. |
| `crates/minidl-app` | The controller: commands (`controller/`), ingest validation (`ingest.rs`), socket servers + scheduler (`server.rs`), OS integration (`platform.rs`, `connector.rs`), updater. |
| `crates/minidl-native-host` | Browser native-messaging bridge. Short-lived, forwards one message to the controller. |
| `apps/native-desktop` | Binary `mini-downloader`: controller process, tray, and the disposable Slint UI child (`--ui-client`). UI markup in `ui/*.slint`. |
| `apps/native-desktop/src/dragout` | Dragging finished files out to other apps, and Ctrl+C on native Wayland. `mod.rs` holds the tested decisions (gesture, payload, URI list); `wayland.rs` (own queue on winit's display), `x11.rs` (XDND source) and `windows.rs` (`SHDoDragDrop`) do the I/O. End-to-end: `tests/dragout_e2e.rs` (nested KWin, `--ignored`). |
| `extension/` | Browser connector (Firefox + Chromium, MV3). Plain JS, no build step beyond `scripts/build-extension.sh`. |
| `assets/locales/*.json` | UI strings. Every locale must have exactly the keys of `en.json`; a test enforces it. |

One controller process owns all state. The UI is a separate process that talks
to it over a user-private control socket and can be killed at any time.

## Check your work

```sh
./scripts/check.sh
```

Runs what CI runs: `cargo fmt --check`, clippy with `-D warnings`, all tests,
the real-aria2 end-to-end tests (when `aria2c` is installed), extension tests
(when `node` is installed) and shellcheck. Finish with this passing.

Windows-only code (`#[cfg(windows)]`) is not compiled on Linux. Keep such code
to a few std-only lines, put the logic in a function compiled under
`#[cfg(any(windows, test))]`, and unit-test that function.

## Drive a running app: `ctl`

`mini-downloader ctl <command> [json-args]` sends one `ControlCommand` to the
running controller and prints the JSON result. This is the way to observe or
script the app without the GUI.

```sh
mini-downloader ctl snapshot                       # everything: downloads, settings, categories
mini-downloader ctl ingest_url '{"url":"https://example.org/f.iso","checksum":null}'
mini-downloader ctl list_downloads '{"status":"active"}'
mini-downloader ctl pause '{"id":3}'
mini-downloader ctl subscribe                      # live events, one JSON object per line
```

- Commands are the snake_case variants of `ControlCommand` in
  `crates/minidl-ipc/src/lib.rs`; arguments are that variant's fields. An
  unknown command prints the full list.
- Exit code 0 = success (result on stdout), 1 = the controller reported an
  error (message on stderr), 2 = bad usage.
- `ctl` is honoured only as the first argument. Links from the browser always
  arrive as the first argument, so a crafted link can never reach it. Keep it
  that way.

### Run an isolated instance

Never test against the user's real profile. This starts a throwaway controller
that touches nothing outside `$T`:

```sh
T=$(mktemp -d)
export HOME=$T/home XDG_DATA_HOME=$T/data XDG_CONFIG_HOME=$T/config XDG_RUNTIME_DIR=$T/run
export MINIDL_BENCHMARK=1        # skips desktop-entry, URL-scheme and browser-manifest registration
mkdir -p -m 700 "$XDG_RUNTIME_DIR"
cargo run -p minidl-native-desktop -- --background &
mini-downloader ctl ping         # retry until it answers
mini-downloader ctl quit
```

`XDG_RUNTIME_DIR` must be short: a Unix socket path is limited to about 100
bytes.

### Drive the real UI: `ui-mcp` (development only)

`ctl` drives the controller; the `ui-mcp` cargo feature lets an agent drive the
Slint window itself through Slint's built-in MCP server (list the element
tree, click, type, take screenshots). It is off by default, refuses to compile
without `debug_assertions`, and listens on `127.0.0.1` only.

```sh
# in the isolated instance above; the UI child inherits the environment
export SLINT_MCP_PORT=8765
export SLINT_BACKEND=headless    # optional: no display needed (software renderer)
cargo run -p minidl-native-desktop --features ui-mcp
# MCP endpoint (Streamable HTTP): http://127.0.0.1:8765/mcp
```

Useful element ids: rows are `DownloadCard`s; their pointer handling is the
single `card-touch` TouchArea in `ui/downloads.slint` (click selects,
double-click opens, press-and-move starts a drag-out through `src/dragout`).

## Rules that are easy to break

Security and privacy:

- Everything from the browser or a URL is untrusted. It enters through
  `ingest::validate_capture_job`; new job fields get a length and
  control-character check there.
- A remote filename never reaches the filesystem unreduced:
  `minidl_core::files::safe_out_name` (aria2 `out`, capture names),
  `completion::downloaded_path`, `deletion_target`.
- aria2 writes HTTP/metalink jobs into `<dir>/.minidl-staging/<id>`; the
  finished file is moved into place with `files::commit_staged`, which never
  replaces an existing file. Never give aria2 `continue=true`. Every path to
  `complete` goes through `completion::complete`; every removal path empties
  the row's staging folder through `Controller::release_stage`. Download ids
  are never reused (`AUTOINCREMENT`).
- Captured cookies/headers are valid for one origin. aria2 gets them only via
  `redirect::resolve` (redirects followed in the app), and `build_add_options`
  sets `max-redirect=0` whenever they are present.
- Secrets (cookies, auth headers, proxy credentials, the aria2 RPC token) go
  through 0600 files or stdin, never argv. `DownloadView` must not grow a field
  that carries them; a test checks the serialized view.
- Files and directories under the data dir are created 0600 / 0700.
- In the extension, only the connector's own pages may send `ldm-grab` and
  `ldm-get-media`. Page-controlled strings go into the DOM with `textContent`.
- A referrer the extension invents from the tab goes through
  `defaultReferrer` (the browser's default policy), never the raw tab URL.

Release pipeline (`.github/workflows/release.yml`):

- Jobs that run third-party code (cargo, gems, npm tools, AppImage tools) keep
  the read-only token and hand files over as artifacts. Only `prepare`,
  `upload`, `checksums` and `publish` may write to the release.
- The Authenticode key exists only in the `windows` job, which compiles
  nothing. Steps of one job share a machine, so reordering steps does not
  isolate a secret; a separate job does.
- Every downloaded tool is pinned by version in the URL and by SHA-256. A
  "latest" or "continuous" URL with a fixed hash breaks on the next upstream
  build.
- Validate workflow edits with `./scripts/check-workflows.sh`.

Behaviour:

- aria2 is started lazily and stopped after 30 idle seconds. Anything that must
  outlive that (a global speed cap, for example) belongs in settings and
  `LaunchOptions`, not only in a live RPC call.
- Download status changes go through `Db::transition` / `transition_if` /
  `fail` with a cause; the lifecycle table `DownloadStatus::can_transition_to`
  refuses backward moves, and every change is journaled (`ctl why '{"id":N}'`).
- Record an aria2 job (GID made by `new_gid`, status) before calling aria2,
  so an early completion or error always finds its row.
- "Running" includes yt-dlp downloads, which aria2 knows nothing about. A
  queue-wide command must handle `row.is_ytdlp()` rows itself.
- A reply on the control socket may take minutes (media probe, update
  download). Do not put a deadline on waiting for it.
- User-visible text lives in `assets/locales`; add the key to all ten files.

## Conventions

- Unit tests sit next to the code (`mod tests` or the crate's `tests.rs`);
  tests that need a real `aria2c` are `#[ignore]`d under `tests/`.
- Controller errors are `AppError`; `AppError::from` keeps the full cause chain.
- Versions are kept in sync by `scripts/bump-version.sh` (workspace
  `Cargo.toml`, both extension manifests, the publisher `package.json`);
  `extension/tests/package.test.js` fails when they drift.
