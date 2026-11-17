#!/bin/bash
# Per-user install of a local build (no root): binary, icon and .desktop entry under ~/.local.
# Pass --default to also make docview the default app for PDF files.
set -e
here=$(cd "$(dirname "$0")/.." && pwd)
bin="$here/target/release/docview"
[ -x "$bin" ] || { echo "Build first: cargo build --release"; exit 1; }
mkdir -p "$HOME/.local/bin"
# Replace, never write into, a running copy's file.
install -m755 "$bin" "$HOME/.local/bin/.docview.new" && mv -f "$HOME/.local/bin/.docview.new" "$HOME/.local/bin/docview"
# Launchers often run without ~/.local/bin on PATH, so the menu entry gets the full path.
mkdir -p "$HOME/.local/share/applications"
sed "s|^Exec=docview|Exec=$HOME/.local/bin/docview|" "$here/packaging/docview.desktop" > "$HOME/.local/share/applications/docview.desktop"
install -Dm644 "$here/logos/pdf-viewer-blue.svg" "$HOME/.local/share/icons/hicolor/scalable/apps/docview.svg"
update-desktop-database "$HOME/.local/share/applications" 2>/dev/null || true
gtk-update-icon-cache -q -t "$HOME/.local/share/icons/hicolor" 2>/dev/null || true
if [ "$1" = "--default" ]; then
  xdg-mime default docview.desktop application/pdf
  echo "docview is now the default for PDF files"
fi
echo "Installed to ~/.local/bin/docview"
