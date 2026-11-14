# Changelog

## [3.4.0] - 2026-11-14

### Upgrade notes

- The minimum window width went from 720 px to 860 px so the new title bar fits.
- Not tested on real hardware: the frameless window was never compiled or run
  on Windows (the Windows build only gets its system shadow through
  `with_undecorated_shadow`); it was tested on Linux (KWin, X11 and Wayland).

### Changed

- The window draws its own title bar instead of the system's: the sidebar
  wordmark and the page's top row are the bar (drag to move, double-click to
  maximise), with minimise, maximise/restore and close at its outer end
  (mirrored in Arabic). Edges and corners still resize. Close behaves exactly
  as the system close button did (close to tray when enabled).
- While something downloads, the total rate shows under the wordmark, with the
  global speed cap beside it when one is set. Nothing is shown when idle.

## [3.3.0] - 2026-11-13

### Upgrade notes (read first)

- The database is migrated to schema v6 on first start. **Once 3.3.0 has started,
  you cannot go back to 3.2.0**: the older version will not open the migrated
  database. Back up your data directory if you may want to roll back.
- The browser extension changed (new version 3.3.0 in both manifests); update it
  from the Firefox/Chrome store when the new version appears.
- Not tested on real hardware: Windows code paths were compiled and unit-tested
  but not run; dragging a finished download out to other apps works on Wayland
  and X11 and is untested on Windows.

### Added

- File-type icons on download rows.
- Drag a finished download out of the window into other apps (Wayland, X11;
  Windows untested). Show in folder and copy as file are available too.
- Browser extension: captures the final URL after redirects and checks the
  blacklist against both the original and the final URL.
- `ctl why` shows the journal of state transitions for a download.

### Changed

- Single click selects a download; double-click opens it.
- Downloads are written to a hidden `.minidl-staging` folder and moved into
  place when finished. An existing file is never overwritten; the new file gets
  a free name instead.
- Redirects are resolved by the app with a vetted resolver.
- Download state changes are applied atomically and reconciled after the
  download engine restarts.

### Fixed

- Downloads can no longer replace a file that already exists at the target path.
- State races between UI actions and the download engine.

### Security

- Cookies captured from the browser are dropped when a redirect leaves the
  original origin, so credentials stay on the site they were issued for.
- Dependency updates past new advisories (SQLite, TLS, QUIC) and extension
  publishing tools (already on main).
