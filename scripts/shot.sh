#!/bin/bash
# Screenshot the real app without touching the desktop: it runs inside a rootful Xwayland
# parked on Hyprland workspace 9 (GTK's X11 frame clock ticks there; a hidden Wayland window
# gets no frame callbacks, so its styles never update). Config and state go to a temp dir.
# Usage: scripts/shot.sh out.png [file] ['cmd;cmd;type:text;enter:'] [WxH] [delay-ms]
# Set KEEP_STATE=dir to reuse a config/state dir between shots, CONFIG_HOME to pick the config dir.
here=$(cd "$(dirname "$0")/.." && pwd)
out=$(realpath -m "$1"); file=${2:+$(realpath "$2")}; run=$3; size=${4:-1100x740}; delay=${5:-1500}
disp=:93
if ! pgrep -f "^Xwayland $disp " >/dev/null; then
  hyprctl eval "hl.exec_cmd(\"Xwayland $disp -geometry 2400x1600 -noreset\", { workspace = \"9 silent\" })" >/dev/null
  sleep 1.2
fi
state=${KEEP_STATE:-$(mktemp -d "${TMPDIR:-/tmp}/docview-shot.XXXXXX")}
mkdir -p "$state"
rm -f "$out"
XDG_CONFIG_HOME="${CONFIG_HOME:-$state/config}" XDG_STATE_HOME="$state/state" GDK_SCALE=1 GDK_BACKEND=x11 DISPLAY=$disp \
  DOCVIEW_SIZE="$size" DOCVIEW_SNAPSHOT="$out" DOCVIEW_SNAPSHOT_DELAY="$delay" DOCVIEW_SNAPSHOT_QUIT=1 \
  DOCVIEW_RUN="$run" timeout 20 "$here/target/release/docview" ${file:+"$file"} 2>&1 | grep -v "^$"
[ -z "$KEEP_STATE" ] && rm -rf "$state"
[ -f "$out" ] && echo "wrote $out" || { echo "no screenshot"; exit 1; }
