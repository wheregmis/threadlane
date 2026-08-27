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
#   scripts/run-gpui-macos.sh [--release] [-- <app args>]
#
# The bundle deliberately keeps a stable path: macOS ties permission grants for
# an unsigned app to its location, so a moving path would re-prompt on every
# run. Set THREADLANE_DEV_SIGN=1 to ad-hoc sign it as well; nothing here needs
# it, but a signature is required once an entitlement is.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

profile="debug"
cargo_args=()
while [[ $# -gt 0 ]]; do
  case "$1" in
    --release)
      profile="release"
      cargo_args+=("--release")
      shift
      ;;
    --)
      shift
      break
      ;;
    *)
      printf 'Unknown option: %s\n' "$1" >&2
      printf 'Usage: scripts/run-gpui-macos.sh [--release] [-- <app args>]\n' >&2
      exit 2
      ;;
  esac
done

if [[ "$(uname -s)" != "Darwin" ]]; then
  printf 'This script is macOS-only; on other platforms run: cargo run -p threadlane-gpui\n' >&2
  exit 2
fi

target_dir="${CARGO_TARGET_DIR:-target}"
binary="$target_dir/$profile/threadlane-gpui"
app="$target_dir/$profile/Threadlane-dev.app"
contents="$app/Contents"

# macOS ships bash 3.2, where an empty array expands as unset under `set -u`.
cargo build -p threadlane-gpui --bin threadlane-gpui ${cargo_args[@]+"${cargo_args[@]}"}

mkdir -p "$contents/MacOS" "$contents/Resources"
cp packaging/Info.plist "$contents/Info.plist"
plutil -replace CFBundleExecutable -string threadlane "$contents/Info.plist"
# Distinguishes the dev bundle from an installed release in the Dock and in
# notification settings, so the two do not share permission state.
plutil -replace CFBundleName -string "Threadlane (dev)" "$contents/Info.plist"
plutil -replace CFBundleDisplayName -string "Threadlane (dev)" "$contents/Info.plist"
plutil -replace CFBundleIdentifier -string dev.threadlane.app.dev "$contents/Info.plist"

# `cp` over a running app's executable fails with ETXTBSY, and cargo replaces
# the build output by rename, so the copy is always taken fresh.
cp -f "$binary" "$contents/MacOS/threadlane"

if [[ "${THREADLANE_DEV_SIGN:-0}" == "1" ]]; then
  codesign --force --sign - "$app"
fi

exec "$contents/MacOS/threadlane" "$@"
