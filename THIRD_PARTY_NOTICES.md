# Third-party notices

Mini Downloader is built from Rust crates under their respective licenses. The
complete dependency attribution and license text bundle is in
`THIRD_PARTY_LICENSES.txt`; CI regenerates it from the locked dependency graph
and rejects stale output.

Linux packages use the system-provided `aria2`, `yt-dlp`, and `ffmpeg` tools.
The Windows packages bundle these unmodified upstream executables:

- aria2 1.37.0, licensed under GPL-2.0-or-later. Its `COPYING` and OpenSSL
  notice are included in the package, and the corresponding source archive is
  attached to the same GitHub release.
- yt-dlp 2026.07.04, dedicated to the public domain under the Unlicense. Its
  license is included in the package; its source archive is attached to the
  same GitHub release.

The app does not replace or re-sign the upstream aria2 or yt-dlp signatures or
publisher identity. Source and releases are available from:

- https://github.com/aria2/aria2
- https://github.com/yt-dlp/yt-dlp

FFmpeg is not bundled. Its exact license depends on the build installed by the
user or operating-system distributor.
