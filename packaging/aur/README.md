# Publishing to the AUR

Three packages:

- `mini-downloader-bin` — repackages the released `.deb` (fast install)
- `mini-downloader` — builds from the release tarball (rust + pnpm)
- `mini-downloader-git` — builds from `main` HEAD (`pkgver()` derived from `git describe`)

## One-time setup

1. Create an account on https://aur.archlinux.org and add your SSH public key.
2. `git clone ssh://aur@aur.archlinux.org/mini-downloader-bin.git` (an empty
   repo is created on first push; same for `mini-downloader`).

## Publish / update

```sh
cd packaging/aur/mini-downloader-bin
# 1. bump pkgver to match the GitHub release; reset pkgrel to 1
# 2. replace sha256sums=('SKIP') with the real hash:
updpkgsums                     # (pacman-contrib) downloads + fills sha256
# 3. verify the asset filename matches the release (tauri derives it from the
#    product name) and that it builds:
makepkg -si
namcap PKGBUILD *.pkg.tar.zst  # lint
# 4. regenerate metadata and push to the AUR repo:
makepkg --printsrcinfo > .SRCINFO
git add PKGBUILD .SRCINFO && git commit -m "v<version>" && git push
```

Same flow for `mini-downloader` (source package).

Notes:
- Never commit built artifacts to the AUR repo — only `PKGBUILD` + `.SRCINFO`.
- `sha256sums=('SKIP')` is fine for local testing but MUST be a real checksum
  when pushed.
