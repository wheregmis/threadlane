#!/usr/bin/env bash
# Runs the GPUI app from a development app bundle.
#
# macOS refuses some framework calls to a process that is not inside a bundle.
# `gpui_component::init` reaches `UNUserNotificationCenter`, which raises
# `bundleProxyForCurrentProcess is nil` and aborts the process, so
# `cargo run -p threadlane-gpui` cannot start the app on macOS.
#
# The same binary works unchanged once it lives at
# `Threadlane-dev.app/Contents/MacOS/`, because that is what lets AppKit resolve
# a main bundle. This assembles that bundle around the freshly built binary and
# executes it in the foreground, so stdout, `RUST_LOG`, and Ctrl-C behave the
# way they do under `cargo run`.
#
# Usage:
#   scripts/run-gpui-macos.sh [--release] [--restart-daemon] [-- <app args>]
#
# The bundle deliberately keeps a stable path: macOS ties permission grants for
# an unsigned app to its location, so a moving path would re-prompt on every
# run. Set THREADLANE_DEV_SIGN=1 to ad-hoc sign it as well; nothing here needs
# it, but a signature is required once an entitlement is.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

# Development runs should expose the OAuth and daemon diagnostics needed when
# investigating provider login failures. Callers can still override these.
export RUST_LOG="${RUST_LOG:-debug}"
export RUST_BACKTRACE="${RUST_BACKTRACE:-1}"
export THREADLANE_DAEMON_STDIO="${THREADLANE_DAEMON_STDIO:-inherit}"
printf 'Threadlane tracing: RUST_LOG=%s RUST_BACKTRACE=%s\n' "$RUST_LOG" "$RUST_BACKTRACE"

profile="debug"
restart_daemon=0
cargo_args=()
while [[ $# -gt 0 ]]; do
  case "$1" in
    --release)
      profile="release"
      cargo_args+=("--release")
      shift
      ;;
    --restart-daemon)
      restart_daemon=1
      shift
      ;;
    --)
      shift
      break
      ;;
    *)
      printf 'Unknown option: %s\n' "$1" >&2
      printf 'Usage: scripts/run-gpui-macos.sh [--release] [--restart-daemon] [-- <app args>]\n' >&2
      exit 2
      ;;
  esac
done

if [[ "$(uname -s)" != "Darwin" ]]; then
  printf 'This script is macOS-only; on other platforms run: cargo run -p threadlane-gpui\n' >&2
  exit 2
fi

target_dir="${CARGO_TARGET_DIR:-target}"
if [[ "$target_dir" != /* ]]; then
  target_dir="$root/$target_dir"
fi
binary="$target_dir/$profile/threadlane-gpui"
daemon_binary="$target_dir/$profile/threadlane-daemon"
app="$target_dir/$profile/Threadlane-dev.app"
contents="$app/Contents"

# macOS ships bash 3.2, where an empty array expands as unset under `set -u`.
cargo build -p threadlane-gpui --bin threadlane-gpui ${cargo_args[@]+"${cargo_args[@]}"}
cargo build -p threadlane-daemon --bin threadlane-daemon ${cargo_args[@]+"${cargo_args[@]}"}

mkdir -p "$contents/MacOS" "$contents/Resources"
cp packaging/Info.plist "$contents/Info.plist"
plutil -replace CFBundleExecutable -string threadlane "$contents/Info.plist"
# Distinguishes the dev bundle from an installed release in the Dock and in
# notification settings, so the two do not share permission state.
plutil -replace CFBundleName -string "Threadlane (dev)" "$contents/Info.plist"
plutil -replace CFBundleDisplayName -string "Threadlane (dev)" "$contents/Info.plist"
plutil -replace CFBundleIdentifier -string dev.threadlane.app.dev "$contents/Info.plist"

if [[ "$restart_daemon" == "1" ]]; then
  daemon_socket="$HOME/.threadlane/daemon.sock"
  if [[ -S "$daemon_socket" ]]; then
    daemon_pids="$(lsof -t "$daemon_socket" 2>/dev/null || true)"
    if [[ -z "$daemon_pids" ]]; then
      printf 'Removing stale daemon socket: %s\n' "$daemon_socket" >&2
      rm -f "$daemon_socket"
    else
      for pid in $daemon_pids; do
        command_line="$(ps -p "$pid" -o command= 2>/dev/null || true)"
        if [[ "$command_line" != "$contents/MacOS/threadlane-daemon"* ]]; then
          printf 'Refusing to stop non-development daemon PID %s: %s\n' "$pid" "$command_line" >&2
          exit 2
        fi
      done
      for pid in $daemon_pids; do
        printf 'Stopping previous development daemon PID %s for debug tracing.\n' "$pid"
        kill "$pid" 2>/dev/null || true
      done
      for attempt in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20; do
        if ! lsof -t "$daemon_socket" >/dev/null 2>&1; then
          break
        fi
        sleep 0.1
      done
      if lsof -t "$daemon_socket" >/dev/null 2>&1; then
        printf 'Timed out waiting for daemon socket to close: %s\n' "$daemon_socket" >&2
        exit 1
      fi
      rm -f "$daemon_socket"
    fi
  fi
fi

# `cp` over a running app's executable fails with ETXTBSY, and cargo replaces
# the build output by rename, so the copy is always taken fresh.
cp -f "$binary" "$contents/MacOS/threadlane"
cp -f "$daemon_binary" "$contents/MacOS/threadlane-daemon"

if [[ "${THREADLANE_DEV_SIGN:-0}" == "1" ]]; then
  codesign --force --sign - "$app"
fi

exec "$contents/MacOS/threadlane" "$@"
