#!/bin/bash
# Run docview with event, frame and command logging (for diagnosing input / redraw problems).
# Usage: scripts/debug-run.sh [file.pdf]   then use the app, close it, and read the log.
here=$(cd "$(dirname "$0")/.." && pwd)
log=${DOCVIEW_LOG:-${TMPDIR:-/tmp}/docview-debug.log}
GDK_DEBUG=frames DOCVIEW_DEBUG=1 "$here/target/release/docview" "${1:-$here/tests/fixtures/sample.pdf}" >"$log" 2>&1
echo "log: $log ($(wc -l <"$log") lines)"
grep -v "Gdk-Message" "$log" | tail -40
