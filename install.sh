#!/bin/sh
# docview installer for Linux (per user, no root).
#
#   curl -fsSL https://raw.githubusercontent.com/ahaan-shah/docview/main/install.sh | sh
#
# Make docview the default app for PDF files too:
#
#   curl -fsSL https://raw.githubusercontent.com/ahaan-shah/docview/main/install.sh | sh -s -- --default
#
# Uninstall (your settings are kept):
#
#   curl -fsSL https://raw.githubusercontent.com/ahaan-shah/docview/main/install.sh | sh -s -- --uninstall
#
# Environment:
#   DOCVIEW_VERSION   install a specific version (e.g. 0.1.0) instead of the latest
set -eu

REPO="ahaan-shah/docview"

say() { printf '\033[1;34mdocview\033[0m %s\n' "$*"; }
die() { printf '\033[1;31merror\033[0m %s\n' "$*" >&2; exit 1; }
need() { command -v "$1" >/dev/null 2>&1 || die "this installer needs '$1'"; }

BIN_DIR="$HOME/.local/bin"
DATA="${XDG_DATA_HOME:-$HOME/.local/share}"
BIN="$BIN_DIR/docview"
ICON="$DATA/icons/hicolor/scalable/apps/docview.svg"
DESKTOP="$DATA/applications/docview.desktop"

refresh() {
    command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database -q "$DATA/applications" || true
    command -v gtk-update-icon-cache >/dev/null 2>&1 && gtk-update-icon-cache -q -t "$DATA/icons/hicolor" 2>/dev/null || true
}

[ "$(uname -s)" = Linux ] || die "docview is for Linux"

MAKE_DEFAULT=""
case "${1:-}" in
"") ;;
--default) MAKE_DEFAULT=1 ;;
--uninstall)
    [ -e "$BIN" ] || [ -e "$DESKTOP" ] || die "docview isn't installed in $BIN_DIR"
    rm -f "$BIN" "$DESKTOP" "$ICON"
    refresh
    say "uninstalled docview"
    say "your settings are still in ~/.config/docview (delete it to remove them too)"
    exit 0
    ;;
*) die "unknown option: $1 (options: --default, --uninstall)" ;;
esac

need curl
need tar

case "$(uname -m)" in
x86_64 | amd64) ARCH=x86_64 ;;
aarch64 | arm64) ARCH=aarch64 ;;
*) die "unsupported architecture: $(uname -m) (build from source: https://github.com/$REPO#building-from-source)" ;;
esac

if [ -n "${DOCVIEW_VERSION:-}" ]; then
    TAG="v${DOCVIEW_VERSION#v}"
else
    latest="$(curl -fsSLI -o /dev/null -w '%{url_effective}' "https://github.com/$REPO/releases/latest")" ||
        die "couldn't reach GitHub"
    TAG="${latest##*/}"
    case "$TAG" in
    v[0-9]*) ;;
    *) die "couldn't find the latest release (set DOCVIEW_VERSION to pick one)" ;;
    esac
fi
BASE="https://github.com/$REPO/releases/download/$TAG"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM

STEM="docview-$TAG-$ARCH-linux"
say "downloading $STEM.tar.gz"
curl -fL --progress-bar "$BASE/$STEM.tar.gz" -o "$TMP/$STEM.tar.gz" || die "couldn't download $BASE/$STEM.tar.gz"
curl -fsL "$BASE/checksums.txt" -o "$TMP/checksums.txt" || die "couldn't download the release's checksums"
expected="$(grep " $STEM.tar.gz\$" "$TMP/checksums.txt" | cut -d' ' -f1)"
[ -n "$expected" ] || die "$STEM.tar.gz isn't listed in the release's checksums"
[ "$expected" = "$(sha256sum "$TMP/$STEM.tar.gz" | cut -d' ' -f1)" ] || die "checksum mismatch for $STEM.tar.gz"
say "checksum ok"
tar -xzf "$TMP/$STEM.tar.gz" -C "$TMP"
SRC="$TMP/$STEM"

mkdir -p "$BIN_DIR" "$DATA/applications" "$(dirname "$ICON")"
# Replace, never write into, a running copy's file.
install -m 755 "$SRC/docview" "$BIN_DIR/.docview.new"
mv -f "$BIN_DIR/.docview.new" "$BIN"
install -m 644 "$SRC/docview.svg" "$ICON"
# Launchers often run without ~/.local/bin on PATH, so the menu entry gets the full path.
sed "s|^Exec=docview|Exec=$BIN|" "$SRC/docview.desktop" >"$DESKTOP"
chmod 644 "$DESKTOP"
refresh

if [ -n "$MAKE_DEFAULT" ]; then
    command -v xdg-mime >/dev/null 2>&1 && xdg-mime default docview.desktop application/pdf && say "docview is now the default for PDF files"
fi

say "installed docview ${TAG#v} to $BIN"
case ":$PATH:" in
*":$BIN_DIR:"*) ;;
*) say "note: $BIN_DIR isn't on your PATH. Add it, or open docview from your app menu" ;;
esac
libs="$({ ldconfig -p 2>/dev/null || /sbin/ldconfig -p 2>/dev/null || true; })"
echo "$libs" | grep -q 'libgtk-4\.so\.1' && echo "$libs" | grep -q 'libpoppler-glib\.so\.8' ||
    say "note: docview needs GTK 4 and poppler-glib (Arch: gtk4 poppler-glib, Debian/Ubuntu: libgtk-4-1 libpoppler-glib8, Fedora: gtk4 poppler-glib)"
say "run it with: docview [file]"
