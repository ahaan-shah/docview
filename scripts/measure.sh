#!/bin/bash
# Memory actually used (PSS) and CPU time of a running docview.
# Usage: scripts/measure.sh [pid]
pid=${1:-$(pgrep -x docview | head -1)}
[ -z "$pid" ] && { echo "docview is not running"; exit 1; }
awk '/^Pss:/{printf "PSS   %7.1f MB\n", $2/1024} /^Rss:/{printf "RSS   %7.1f MB\n", $2/1024}' "/proc/$pid/smaps_rollup"
awk '{printf "CPU   %.2f s (utime+stime)\n", ($14+$15)/100}' "/proc/$pid/stat"
