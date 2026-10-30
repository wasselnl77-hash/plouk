#!/usr/bin/env bash
# Installs Plouk for the current user: the binary, its icon, and an app-menu entry.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
bin="$here/dist/plouk"
if [ ! -x "$bin" ]; then
  echo "No build found at dist/plouk. Build it first:"
  echo "  (cd src-tauri && cargo build --release) && mkdir -p dist && cp src-tauri/target/release/plouk dist/"
  exit 1
fi

icon="$HOME/.local/share/icons/hicolor/512x512/apps/plouk.png"
install -Dm755 "$bin" "$HOME/.local/bin/plouk"
install -Dm644 "$here/src-tauri/icons/icon.png" "$icon"
install -Dm644 "$here/src-tauri/icons/128x128.png" "$HOME/.local/share/icons/hicolor/128x128/apps/plouk.png"
mkdir -p "$HOME/.local/share/applications"
cat > "$HOME/.local/share/applications/plouk.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=Plouk
Comment=Dark, minimal vinyl music player. Drop a Spotify playlist in.
Exec=$HOME/.local/bin/plouk %U
Icon=$icon
Terminal=false
Categories=AudioVideo;Audio;Player;
StartupWMClass=plouk
EOF
update-desktop-database "$HOME/.local/share/applications" 2>/dev/null || true
echo "Installed. Open Plouk from your app menu, or run: plouk"
