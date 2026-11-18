#!/bin/bash
# Screenshot the real app without touching the desktop: it runs natively on Hyprland's
# workspace 9 (silent) and the DOCVIEW_SNAPSHOT hook lays out and renders the window offscreen
# at DOCVIEW_SIZE. Config and state go to a temp dir.
# Usage: scripts/shot.sh out.png [file] ['cmd;cmd;type:text;enter:'] [WxH] [delay-ms]
# Set KEEP_STATE=dir to reuse a config/state dir between shots, CONFIG_HOME to pick the config dir.
here=$(cd "$(dirname "$0")/.." && pwd)
out=$(realpath -m "$1"); file=${2:+$(realpath "$2")}; run=$3; size=${4:-1100x740}; delay=${5:-1500}
state=${KEEP_STATE:-$(mktemp -d "${TMPDIR:-/tmp}/docview-shot.XXXXXX")}
mkdir -p "$state"
rm -f "$out"
log="$state/log"
cmd="env XDG_CONFIG_HOME='${CONFIG_HOME:-$state/config}' XDG_STATE_HOME='$state/state' DOCVIEW_SIZE='$size' \
DOCVIEW_SNAPSHOT='$out' DOCVIEW_SNAPSHOT_DELAY='$delay' DOCVIEW_SNAPSHOT_QUIT=1 DOCVIEW_RUN='$run' \
timeout 20 '$here/target/release/docview' ${file:+"'$file'"} > '$log' 2>&1"
hyprctl eval "hl.exec_cmd(\"${cmd//\"/\\\"}\", { workspace = \"9 silent\" })" >/dev/null
for _ in $(seq 1 40); do [ -f "$out" ] && break; sleep 0.25; done
sleep 0.3
grep -v "^$" "$log" 2>/dev/null
[ -z "$KEEP_STATE" ] && rm -rf "$state"
[ -f "$out" ] && echo "wrote $out" || { echo "no screenshot"; exit 1; }
