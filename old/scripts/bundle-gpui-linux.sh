#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

target_dir="${CARGO_TARGET_DIR:-target}"
version="$(cargo metadata --no-deps --format-version 1 | jq -r '.packages[] | select(.name == "threadlane-gpui") | .version')"
architecture="$(uname -m)"
stage="$target_dir/release/threadlane-linux"
archive="$target_dir/release/Threadlane-${version}-linux-${architecture}.tar.gz"

cargo build --locked --release --bin threadlane-gpui
rm -rf "$stage" "$archive"
mkdir -p "$stage"

install -m755 "$target_dir/release/threadlane-gpui" "$stage/threadlane"
install -m644 resources/icon_512.png "$stage/threadlane.png"

# The desktop entry is generated at install time so Exec resolves to the
# absolute installed path; a shipped .desktop cannot locate adjacent files.
cat > "$stage/install.sh" <<'INSTALLER'
#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"

bin_dir="${HOME}/.local/bin"
data_dir="${XDG_DATA_HOME:-${HOME}/.local/share}"
mkdir -p "$bin_dir" "$data_dir/icons/hicolor/512x512/apps" "$data_dir/applications"
install -m755 threadlane "$bin_dir/threadlane"
install -m644 threadlane.png "$data_dir/icons/hicolor/512x512/apps/threadlane.png"
cat > "$data_dir/applications/threadlane.desktop" <<DESKTOP
[Desktop Entry]
Type=Application
Name=Threadlane
Exec=$bin_dir/threadlane
Icon=threadlane
Terminal=false
Categories=Development;
DESKTOP
printf 'Installed Threadlane to %s\n' "$bin_dir/threadlane"
INSTALLER
chmod +x "$stage/install.sh"

tar -czf "$archive" -C "$stage" .
printf 'Created %s\n' "$archive"
