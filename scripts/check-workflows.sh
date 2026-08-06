#!/usr/bin/env bash
# Validate GitHub Actions syntax and embedded shell with a checksum-pinned tool.
set -euo pipefail

if command -v actionlint >/dev/null 2>&1; then
  exec actionlint "$@"
fi

version=1.7.12
archive="actionlint_${version}_linux_amd64.tar.gz"
checksum=8aca8db96f1b94770f1b0d72b6dddcb1ebb8123cb3712530b08cc387b349a3d8
temporary_directory="$(mktemp -d "${TMPDIR:-/tmp}/minidl-actionlint.XXXXXX")"
cleanup() {
  rm -rf -- "$temporary_directory"
}
trap cleanup EXIT

curl -fsSL --retry 3 --retry-all-errors --connect-timeout 15 --max-time 120 \
  "https://github.com/rhysd/actionlint/releases/download/v${version}/${archive}" \
  -o "$temporary_directory/$archive"
printf '%s  %s\n' "$checksum" "$temporary_directory/$archive" | sha256sum -c -
tar -xzf "$temporary_directory/$archive" -C "$temporary_directory" actionlint
"$temporary_directory/actionlint" "$@"
