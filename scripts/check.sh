#!/usr/bin/env bash
# The gates CI enforces (.github/workflows/build.yml) as one local command,
# for people and coding agents alike. Tools that are not installed are skipped
# with a notice instead of failing, so the Rust gates always run.
set -euo pipefail

cd "$(dirname "$0")/.."

skipped() {
  printf 'skipped: %s\n' "$1" >&2
}

cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked

if command -v aria2c >/dev/null 2>&1; then
  cargo test -p minidl-core --test engine_e2e --locked -- --ignored
  cargo test -p minidl-app --test controller_e2e --locked -- --ignored
else
  skipped "real-engine tests (aria2c is not on PATH)"
fi

if command -v node >/dev/null 2>&1; then
  for script in extension/*.js; do
    node --check "$script"
  done
  node --test extension/tests/*.test.js
else
  skipped "extension tests (node is not installed)"
fi

if command -v shellcheck >/dev/null 2>&1; then
  shellcheck scripts/*.sh packaging/linux/*.sh
else
  skipped "shell lint (shellcheck is not installed)"
fi

echo "all checks passed"
