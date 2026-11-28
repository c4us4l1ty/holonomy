#!/usr/bin/env bash
# Run under Xvfb with the binaries already built. Test-only fixture uses the debug key schedule.
set -euxo pipefail
: "${HOLONOMY_NATIVE_SMOKE_PATH:?explicit smoke fixture path required}"
cargo test --locked -p holonomy-native --lib ui_smoke_fixture::create -- --exact --ignored
app_pid=''
held_path="${HOLONOMY_NATIVE_SMOKE_PATH}.held-$$"
held=false
cleanup() {
  if [[ -n "$app_pid" ]]; then kill "$app_pid" 2>/dev/null || true; fi
  rm -f "$HOLONOMY_NATIVE_SMOKE_PATH"
  if $held; then rm -f "$held_path"; fi
}
trap cleanup EXIT
trap 'xwininfo -root -tree || true; import -window root native-failure.png || true' ERR
# Select X11 explicitly; hosted CI may also advertise a nonfunctional Wayland display.
unset WAYLAND_DISPLAY
export GPUI_X11_SCALE_FACTOR="${GPUI_X11_SCALE_FACTOR:-1}"
target/debug/holonomy-gui "$HOLONOMY_NATIVE_SMOKE_PATH" &
app_pid=$!
window=$(timeout 45 xdotool search --sync --onlyvisible --name '^Holonomy Native$' | head -1)
xdotool windowfocus --sync "$window"
xdotool key Tab
before=$(sha256sum "$HOLONOMY_NATIVE_SMOKE_PATH")
xdotool type --clearmodifiers --delay 35 'wrong fixture passphrase'
xdotool key Return
sleep 1
[[ "$(xdotool getwindowname "$window")" == "Holonomy Native" ]]
[[ "$(sha256sum "$HOLONOMY_NATIVE_SMOKE_PATH")" == "$before" ]]
xdotool type --clearmodifiers --delay 35 'native smoke passphrase'
sleep 1
import -window "$window" native-unlock.png
xdotool key Return
# The title changes only after the real encrypted session has opened.
timeout 45 xdotool search --sync --onlyvisible --name 'native-smoke.wavefunction' >/dev/null
xdotool key ctrl+a
xdotool type --clearmodifiers --delay 35 'Saved by native UI smoke test'
xdotool key ctrl+z
xdotool key ctrl+y
xdotool key ctrl+f
xdotool type --clearmodifiers --delay 35 'native'
xdotool key Return Escape
xdotool key ctrl+b
xdotool key ctrl+s
sleep 1
import -window "$window" native-smoke.png
# Removing the pathname makes the next append fail before any bytes can be written. Closing must
# leave the process/window and pending text alive. A failed append poisons the handle by design:
# do not clear that fence or pretend a retry is guaranteed to recover it.
xdotool key ctrl+End
xdotool type --clearmodifiers --delay 35 ' pending'
[[ ! -e "$held_path" ]]
mv "$HOLONOMY_NATIVE_SMOKE_PATH" "$held_path"
held=true
xdotool key ctrl+q
sleep 1
kill -0 "$app_pid"
xdotool getwindowname "$window" | grep -F 'native-smoke.wavefunction'
mv "$held_path" "$HOLONOMY_NATIVE_SMOKE_PATH"
held=false
xdotool key ctrl+q
sleep 1
kill -0 "$app_pid"
import -window "$window" native-refused-save.png
# Only an explicit emergency wipe discards the pending text. Reopen below must still see the
# previous committed version, never a partial or silently advanced save.
xdotool key ctrl+shift+Escape
timeout 45 xdotool search --sync --onlyvisible --name '^Holonomy Native$' >/dev/null
xdotool key ctrl+q
# Bounded wait without relying on an indefinite shell wait if shutdown breaks.
timeout 20 tail --pid="$app_pid" -f /dev/null
wait "$app_pid"
app_pid=''
cargo test --locked -p holonomy-native --lib ui_smoke_fixture::verify -- --exact --ignored
