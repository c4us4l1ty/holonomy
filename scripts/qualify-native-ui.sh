#!/usr/bin/env bash
# Run under Xvfb with the binaries already built. Test-only fixture uses the debug key schedule.
set -euxo pipefail
: "${HOLONOMY_NATIVE_SMOKE_PATH:?explicit smoke fixture path required}"
cargo test --locked -p holonomy-native --lib ui_smoke_fixture::create -- --exact --ignored
app_pid=''
cleanup() {
  if [[ -n "$app_pid" ]]; then kill "$app_pid" 2>/dev/null || true; fi
  rm -f "$HOLONOMY_NATIVE_SMOKE_PATH"
}
trap cleanup EXIT
trap 'xwininfo -root -tree; import -window root native-failure.png || true' ERR
# Select X11 explicitly; hosted CI may also advertise a nonfunctional Wayland display.
unset WAYLAND_DISPLAY
export GPUI_X11_SCALE_FACTOR="${GPUI_X11_SCALE_FACTOR:-1}"
target/debug/holonomy-native "$HOLONOMY_NATIVE_SMOKE_PATH" &
app_pid=$!
window=$(timeout 45 xdotool search --sync --onlyvisible --name '^Holonomy Native$' | head -1)
xdotool windowfocus --sync "$window"
xdotool key --window "$window" Tab
xdotool type --window "$window" --clearmodifiers --delay 35 'native smoke passphrase'
xdotool key --window "$window" Return
# The title changes only after the real encrypted session has opened.
timeout 45 xdotool search --sync --onlyvisible --name 'Holonomy Native — native-smoke.wavefunction' >/dev/null
xdotool key --window "$window" ctrl+a
xdotool type --window "$window" --clearmodifiers --delay 35 'Saved by native UI smoke test'
xdotool key --window "$window" ctrl+s
# Wait for at least one rendered frame before recording the specimen.
sleep 2
import -window "$window" native-smoke.png
xdotool key --window "$window" ctrl+q
# Bounded wait without relying on an indefinite shell wait if shutdown breaks.
timeout 20 tail --pid="$app_pid" -f /dev/null
wait "$app_pid"
app_pid=''
cargo test --locked -p holonomy-native --lib ui_smoke_fixture::verify -- --exact --ignored
