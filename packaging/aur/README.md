# Publishing to the AUR

The repository maintains three native packages:

- `mini-downloader-bin` installs the prebuilt native release archive.
- `mini-downloader` builds a tagged source release with Cargo.
- `mini-downloader-git` builds `main` and derives its version from Git.

The native Slint build has no Node, WebKitGTK, GTK, or AppIndicator dependency.
Runtime downloads use system `aria2`. `yt-dlp` enables media downloads, while
`ffmpeg` enables stream merging, embedding, and audio conversion; both are
optional dependencies.

Before publishing a stable package:

```sh
cd packaging/aur/mini-downloader-bin
# Set pkgver/pkgrel, then replace SKIP with the released asset checksum.
updpkgsums
makepkg -si
namcap PKGBUILD ./*.pkg.tar.zst
makepkg --printsrcinfo > .SRCINFO
```

Repeat for `mini-downloader`, then commit only `PKGBUILD` and `.SRCINFO` to the
corresponding AUR repositories. Never publish a stable package with `SKIP`.
