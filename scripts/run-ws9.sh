#!/bin/bash
# Launch the release build on Hyprland workspace 9 without switching to it (for testing).
# Usage: scripts/run-ws9.sh [VAR=value ...] [-- file]
here=$(cd "$(dirname "$0")/.." && pwd)
envs=""; file=""
while [ $# -gt 0 ]; do
  if [ "$1" = "--" ]; then file="$2"; break; fi
  envs="$envs $1"; shift
done
hyprctl eval "hl.exec_cmd(\"env$envs $here/target/release/docview '$file'\", { workspace = \"9 silent\" })" >/dev/null
