#!/usr/bin/env bash
# Run the M4 scroll verification inside a real Tauri window on this engine.
#
# # What this is for
#
# Every number in M0/M1/M4 comes from headless Chromium. The strategy they justify
# is unverified on the engine that actually ships on Linux. This runs the same
# assertions — from `app/src/core/verify.ts`, the same file the browser suite uses
# — inside a webkit2gtk window and exits non-zero on failure.
#
# # Why the tests run inside the webview
#
# Playwright cannot attach to a Tauri window, and webkit2gtk's remote-debugging
# path is not reliable enough to build on: earlier rounds went through a WebDriver
# whose `POST /session` hung on this machine. Running in the engine removes the
# driver from the equation entirely.
#
# # Usage
#
#   scripts/verify-engine.sh                 # verify on the current platform
#   scripts/verify-engine.sh --keep-open     # leave the window up afterwards
#
# Env:
#   HOLO_VERIFY_OUT   where to write the JSON report (default target/verification-report.json)
#   VITE_PORT         port for the dev server (default 5184)

set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PORT="${VITE_PORT:-5184}"
OUT="${HOLO_VERIFY_OUT:-target/verification-report.json}"
LOG="/tmp/holonomy-verify-$$.log"
KEEP_OPEN=0
[ "${1:-}" = "--keep-open" ] && KEEP_OPEN=1

cd "$ROOT"

# The webkit sandbox is the known blocker on this host (DOCTRINE.md; also
# `spikes/m0-section-seam/FINDINGS.md`). Without it the content processes cannot
# start at all. It is set rather than assumed, so the script is honest about
# needing it and fails visibly if a future machine does.
export WEBKIT_DISABLE_SANDBOX_THIS_IS_DANGEROUS=1
# A Wayland session with no seat management; X11 is not available here.
export GDK_BACKEND=wayland

cleanup() {
  [ -n "${VITE_PID:-}" ] && kill "$VITE_PID" 2>/dev/null
  return 0
}
trap cleanup EXIT

if ! curl -sf -m 3 -o /dev/null "http://localhost:$PORT/verify.html"; then
  echo "starting vite on :$PORT"
  (cd app && npx vite --port "$PORT" --strictPort > "$LOG.vite" 2>&1) &
  VITE_PID=$!
  for _ in $(seq 1 30); do
    curl -sf -m 2 -o /dev/null "http://localhost:$PORT/verify.html" && break
    sleep 1
  done
fi

if ! curl -sf -m 3 -o /dev/null "http://localhost:$PORT/verify.html"; then
  echo "FAIL: vite did not come up on :$PORT (see $LOG.vite)"
  exit 2
fi

echo "running verification in a Tauri window on $(uname -s)..."
rm -f "$ROOT/$OUT"

# `cargo tauri dev`, not `cargo run`. Tauri's `--config` is a `cargo tauri` flag;
# `cargo run --config` is Cargo's own (unrelated) configuration, which fails on a
# JSON file. Two configs are merged: the base one, which deliberately keeps devUrl
# on the *product* surface, and the verify override, which points it at the
# verification page and clears `beforeDevCommand` so it does not start a second
# dev server on the port this script already owns.
#
# Ordinary `cargo tauri dev` therefore runs the app and this runs the assertions —
# without a runner being able to test one while believing it tested the other.
HOLO_VERIFY_OUT="$ROOT/$OUT" \
  cargo tauri dev \
  --config crates/holonomy-shell/tauri.conf.json \
  --config crates/holonomy-shell/tauri.verify.json \
  --no-watch \
  > "$LOG" 2>&1
STATUS=$?

echo
sed -n '/\[verify\]/p' "$LOG"

if [ "$KEEP_OPEN" = "1" ]; then
  echo "(--keep-open: window left running)"
  exit "$STATUS"
fi

if [ "$STATUS" -ne 0 ]; then
  echo
  echo "FAIL: verification exited $STATUS"
  [ -f "$ROOT/$OUT" ] && echo "report: $OUT"
  echo "full log: $LOG"
  exit "$STATUS"
fi

echo
echo "PASS: engine verification on $(uname -s)"
echo "report: $OUT"