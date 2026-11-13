# Changelog

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
