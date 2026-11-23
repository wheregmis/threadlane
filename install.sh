#!/bin/sh
# Install or update Threadlane from a GitHub release.
#   curl -fsSL https://raw.githubusercontent.com/wheregmis/threadlane/main/install.sh | sh
#   curl -fsSL .../install.sh | sh -s -- 0.1.55      # a specific version
# Env: THREADLANE_BIN_DIR (default ~/.local/bin), THREADLANE_APP_DIR (macOS, default ~/Applications).
# Only program files are replaced; hub data and client config are never touched.
set -eu

REPO=wheregmis/threadlane
BIN_DIR=${THREADLANE_BIN_DIR:-$HOME/.local/bin}
APP_DIR=${THREADLANE_APP_DIR:-$HOME/Applications}

die() { echo "install.sh: $*" >&2; exit 1; }

case "$(uname -s)-$(uname -m)" in
  Darwin-arm64) platform=macos-arm64; ext=zip ;;
  Linux-x86_64) platform=linux-x86_64; ext=tar.gz ;;
  Linux-aarch64 | Linux-arm64) platform=linux-aarch64; ext=tar.gz ;;
  *) die "unsupported platform $(uname -s) $(uname -m); releases cover macOS arm64, Linux x86_64 and Linux aarch64" ;;
esac

command -v curl >/dev/null 2>&1 || die "curl is required"
if [ "$ext" = zip ]; then command -v unzip >/dev/null 2>&1 || die "unzip is required"
else command -v tar >/dev/null 2>&1 || die "tar is required"; fi

if [ $# -gt 0 ]; then
  version=${1#v}
else
  tag=$(curl -fsSLI -o /dev/null -w '%{url_effective}' "https://github.com/$REPO/releases/latest") \
    || die "could not look up the latest release"
  version=${tag##*/v}
fi
case "$version" in
  [0-9]*.[0-9]*.[0-9]*) ;;
  *) die "bad version '$version'; expected something like 0.1.56" ;;
esac

asset="Threadlane-$version-$platform.$ext"
base="https://github.com/$REPO/releases/download/v$version"
tmp=$(mktemp -d) || die "cannot make a temporary directory"
trap 'rm -rf "$tmp"' EXIT INT TERM

echo "Downloading Threadlane $version for $platform..."
curl -fSL --progress-bar -o "$tmp/$asset" "$base/$asset" || die "download failed: $base/$asset"

if curl -fsSL -o "$tmp/SHA256SUMS" "$base/SHA256SUMS"; then
  want=$(awk -v f="$asset" '$2 == f { print $1 }' "$tmp/SHA256SUMS")
  [ -n "$want" ] || die "SHA256SUMS has no entry for $asset"
  if command -v sha256sum >/dev/null 2>&1; then got=$(sha256sum "$tmp/$asset" | awk '{ print $1 }')
  elif command -v shasum >/dev/null 2>&1; then got=$(shasum -a 256 "$tmp/$asset" | awk '{ print $1 }')
  else die "sha256sum or shasum is required to verify the download"; fi
  [ "$got" = "$want" ] || die "checksum mismatch for $asset"
  echo "Checksum verified."
else
  die "release v$version has no SHA256SUMS; refusing to install unverified files"
fi

mkdir -p "$tmp/x"
if [ "$ext" = zip ]; then unzip -q "$tmp/$asset" -d "$tmp/x"; else tar -xzf "$tmp/$asset" -C "$tmp/x"; fi

mkdir -p "$BIN_DIR"
# Move into place with a rename so a running hub keeps its old inode until restarted.
for f in threadlane threadlane-hub threadlane-desktop; do
  [ -f "$tmp/x/$f" ] || continue
  cp "$tmp/x/$f" "$BIN_DIR/.$f.new" && chmod 755 "$BIN_DIR/.$f.new" && mv -f "$BIN_DIR/.$f.new" "$BIN_DIR/$f"
  echo "Installed $BIN_DIR/$f"
done

if [ -d "$tmp/x/Threadlane.app" ]; then
  mkdir -p "$APP_DIR"
  rm -rf "$APP_DIR/Threadlane.app"
  cp -R "$tmp/x/Threadlane.app" "$APP_DIR/Threadlane.app"
  echo "Installed $APP_DIR/Threadlane.app"
fi

echo "Threadlane $version installed. Restart a running hub or app to use it."
case ":$PATH:" in
  *":$BIN_DIR:"*) ;;
  *) echo "Add $BIN_DIR to your PATH to run threadlane and threadlane-hub." ;;
esac
