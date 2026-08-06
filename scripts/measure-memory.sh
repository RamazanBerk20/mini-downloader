#!/usr/bin/env bash
# Measure proportional set size (PSS) for an isolated native controller/UI tree.
set -euo pipefail

mode="${1:-both}"
case "$mode" in
  hidden | visible | both) ;;
  *) printf 'usage: %s [hidden|visible|both]\n' "$0" >&2; exit 2 ;;
esac

project_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
binary="${MINIDL_BINARY:-$project_root/target/release/mini-downloader}"
desktop_runtime="${XDG_RUNTIME_DIR:-}"
desktop_wayland="${WAYLAND_DISPLAY:-}"
if [[ ! -x "$binary" ]]; then
  (cd "$project_root" && cargo build --locked --release -p minidl-native-desktop)
fi

active_pid=""
active_root=""
cleanup_active() {
  if [[ -n "$active_pid" ]]; then
    "$binary" --quit >/dev/null 2>&1 || true
    for _ in {1..20}; do
      kill -0 "$active_pid" 2>/dev/null || break
      sleep 0.05
    done
    if kill -0 "$active_pid" 2>/dev/null; then kill "$active_pid" 2>/dev/null || true; fi
    wait "$active_pid" 2>/dev/null || true
  fi
  if [[ -n "$active_root" && -d "$active_root" ]]; then
    rm -rf -- "$active_root"
  fi
  active_pid=""
  active_root=""
}
trap cleanup_active EXIT HUP INT TERM

tree_pids() {
  local root_pid="$1" current child
  local -a queue=("$root_pid")
  while ((${#queue[@]})); do
    current="${queue[0]}"
    queue=("${queue[@]:1}")
    [[ -d "/proc/$current" ]] || continue
    printf '%s\n' "$current"
    while IFS= read -r child; do
      [[ -n "$child" ]] && queue+=("$child")
    done < <(pgrep -P "$current" 2>/dev/null || true)
  done
}

sample_pss() {
  local controller_pid="$1" role="$2" pid pss_kib=0 value command
  printf '\n%s process tree\n' "$role"
  printf '%-8s %12s  %s\n' PID PSS_KiB COMMAND
  while IFS= read -r pid; do
    [[ -r "/proc/$pid/smaps_rollup" ]] || continue
    value="$(awk '/^Pss:/ { print $2; exit }' "/proc/$pid/smaps_rollup")"
    command="$(tr '\0' ' ' < "/proc/$pid/cmdline" | sed 's/[[:space:]]*$//')"
    printf '%-8s %12s  %s\n' "$pid" "${value:-0}" "$command"
    pss_kib=$((pss_kib + ${value:-0}))
  done < <(tree_pids "$controller_pid")
  awk -v kib="$pss_kib" 'BEGIN { printf "TOTAL_PSS_MIB=%.1f\n", kib / 1024 }'
  SAMPLE_PSS_KIB="$pss_kib"
}

run_case() {
  local role="$1" limit_mib="$2" bench_root controller_pid ui_found=false
  bench_root="$(mktemp -d "${TMPDIR:-/tmp}/minidl-memory.XXXXXX")"
  mkdir -p "$bench_root/config" "$bench_root/data" \
    "$bench_root/cache" "$bench_root/runtime"
  chmod 0700 "$bench_root/runtime"
  if [[ -n "$desktop_runtime" && -n "$desktop_wayland" \
      && "$desktop_wayland" != /* \
      && -S "$desktop_runtime/$desktop_wayland" ]]; then
    ln -s "$desktop_runtime/$desktop_wayland" "$bench_root/runtime/$desktop_wayland"
  fi

  export XDG_CONFIG_HOME="$bench_root/config"
  export XDG_DATA_HOME="$bench_root/data"
  export XDG_CACHE_HOME="$bench_root/cache"
  export XDG_RUNTIME_DIR="$bench_root/runtime"
  export MINIDL_BENCHMARK=1

  if [[ "$role" == hidden ]]; then
    "$binary" --background >/dev/null 2>"$bench_root/stderr.log" &
  else
    "$binary" >/dev/null 2>"$bench_root/stderr.log" &
  fi
  controller_pid=$!
  active_pid="$controller_pid"
  active_root="$bench_root"

  for _ in {1..80}; do
    kill -0 "$controller_pid" 2>/dev/null || {
      printf 'Controller exited during %s benchmark:\n' "$role" >&2
      sed -n '1,120p' "$bench_root/stderr.log" >&2
      cleanup_active
      return 1
    }
    if [[ "$role" == hidden ]]; then
      [[ -S "$bench_root/runtime/minidownloader/control.sock" ]] && break
    else
      while IFS= read -r pid; do
        if tr '\0' ' ' < "/proc/$pid/cmdline" 2>/dev/null | grep -q -- '--ui-client'; then
          ui_found=true
        fi
      done < <(tree_pids "$controller_pid")
      [[ "$ui_found" == true ]] && break
    fi
    sleep 0.05
  done
  if [[ "$role" == visible && "$ui_found" != true ]]; then
    printf 'The UI did not start. Run this benchmark inside an X11/Wayland session.\n' >&2
    sed -n '1,120p' "$bench_root/stderr.log" >&2
    cleanup_active
    return 1
  fi

  sleep 2
  sample_pss "$controller_pid" "$role"
  if ((SAMPLE_PSS_KIB > limit_mib * 1024)); then
    printf '%s PSS exceeded %s MiB target.\n' "$role" "$limit_mib" >&2
    cleanup_active
    return 1
  fi
  printf '%s target: PASS (<= %s MiB)\n' "$role" "$limit_mib"
  if [[ "$role" == visible ]]; then
    "$binary" --hide >/dev/null
    for _ in {1..40}; do
      ui_found=false
      while IFS= read -r pid; do
        if tr '\0' ' ' < "/proc/$pid/cmdline" 2>/dev/null | grep -q -- '--ui-client'; then
          ui_found=true
        fi
      done < <(tree_pids "$controller_pid")
      [[ "$ui_found" == false ]] && break
      sleep 0.05
    done
    if [[ "$ui_found" == true ]]; then
      printf 'Renderer process remained alive after --hide.\n' >&2
      cleanup_active
      return 1
    fi
    sleep 1
    sample_pss "$controller_pid" hidden-after-hide
    if ((SAMPLE_PSS_KIB > 50 * 1024)); then
      printf 'hidden-after-hide PSS exceeded 50 MiB target.\n' >&2
      cleanup_active
      return 1
    fi
    printf 'hidden-after-hide target: PASS (<= 50 MiB)\n'
  fi
  cleanup_active
}

status=0
if [[ "$mode" == hidden || "$mode" == both ]]; then
  run_case hidden 50 || status=1
fi
if [[ "$mode" == visible || "$mode" == both ]]; then
  run_case visible 100 || status=1
fi
exit "$status"
