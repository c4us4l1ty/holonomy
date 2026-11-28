#!/usr/bin/env bash
# Run under Xvfb with the binaries already built. Test-only fixture uses the debug key schedule.
set -euxo pipefail
: "${HOLONOMY_NATIVE_SMOKE_PATH:?explicit smoke fixture path required}"
fixture() {
  if [[ -n "${HOLONOMY_NATIVE_FIXTURE_BIN:-}" ]]; then
    "$HOLONOMY_NATIVE_FIXTURE_BIN" "ui_smoke_fixture::$1" --exact --ignored
  else
    cargo test --locked -p holonomy-native --lib "ui_smoke_fixture::$1" -- --exact --ignored
  fi
}
# Isolate the library so mouse navigation has a deterministic directory/file ordering.
library_root="${HOLONOMY_NATIVE_SMOKE_PATH}.library-$$"
mkdir -p "$library_root/Folder"
export HOLONOMY_NATIVE_SMOKE_PATH="$library_root/native-smoke.wavefunction"
fixture create
app_pid=''
held_path="${HOLONOMY_NATIVE_SMOKE_PATH}.held-$$"
held=false
export HOLONOMY_NATIVE_CREATE_PATH="${HOLONOMY_NATIVE_SMOKE_PATH}.created.wavefunction"
cleanup() {
  if [[ -n "$app_pid" ]]; then kill "$app_pid" 2>/dev/null || true; fi
  rm -f "$HOLONOMY_NATIVE_SMOKE_PATH" "$HOLONOMY_NATIVE_CREATE_PATH"
  if $held; then rm -f "$held_path"; fi
  rmdir "$library_root/Folder" "$library_root" 2>/dev/null || true
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
printf %s 'native smoke passphrase' | xclip -selection clipboard
xdotool key ctrl+v
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
printf %s 'native' | xclip -selection clipboard
xdotool key ctrl+v
xdotool key Return Escape
xdotool key ctrl+b
xdotool key ctrl+s
sleep 1
import -window "$window" native-smoke.png
# Real library clicks: enter Folder, go Up, choose the saved encrypted document, unlock.
# Coordinates are logical GPUI units scaled to the actual X11 surface.
library_click() {
  local x y
  x=$(awk "BEGIN {printf \"%.0f\", $1 * $GPUI_X11_SCALE_FACTOR}")
  y=$(awk "BEGIN {printf \"%.0f\", $2 * $GPUI_X11_SCALE_FACTOR}")
  xdotool mousemove --window "$window" "$x" "$y" click 1
  sleep 0.4
}
library_click 70 126
library_click 28 90
library_click 90 168
timeout 10 xdotool search --sync --onlyvisible --name '^Holonomy Native$' >/dev/null
printf %s 'native smoke passphrase' | xclip -selection clipboard
xdotool key ctrl+v Return
timeout 45 xdotool search --sync --onlyvisible --name 'native-smoke.wavefunction' >/dev/null
import -window "$window" native-library.png
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
xdotool getwindowname "$window" | grep -F 'SAVE FAILED'
mv "$held_path" "$HOLONOMY_NATIVE_SMOKE_PATH"
held=false
# Settings is an in-app page: same X11 surface, guarded close and focused wipe.
xdotool key ctrl+comma
settings_window=$(timeout 15 xdotool search --sync --onlyvisible --name '^Holonomy Settings' | head -1)
[[ "$settings_window" == "$window" ]]
xdotool key ctrl+q
sleep 1
kill -0 "$app_pid"
xdotool getwindowname "$window" | grep -F 'SAVE FAILED'
import -window "$window" native-refused-save.png
xdotool key ctrl+shift+Escape
timeout 45 xdotool search --sync --onlyvisible --name '^Holonomy Native$' >/dev/null
import -window "$window" native-home.png
# Metadata search uses the existing native surface and routes selection back to unlock.
xdotool key ctrl+j
xdotool type --clearmodifiers --delay 20 'native-smoke'
import -window "$window" native-search.png
xdotool key Return
xdotool key Escape
# Return Home after cancelling the unlock form.
xdotool windowsize --sync "$window" 640 560
xdotool key ctrl+shift+t
sleep 1
kill -0 "$app_pid"
import -window "$window" native-compact.png
xdotool windowsize --sync "$window" "$(awk "BEGIN {printf \"%.0f\", 1100 * $GPUI_X11_SCALE_FACTOR}")" "$(awk "BEGIN {printf \"%.0f\", 800 * $GPUI_X11_SCALE_FACTOR}")"
xdotool key ctrl+comma
settings_window=$(timeout 15 xdotool search --sync --onlyvisible --name '^Holonomy Settings' | head -1)
[[ "$settings_window" == "$window" ]]
[[ $(xdotool search --onlyvisible --name '^Holonomy' | wc -l) -eq 1 ]]
import -window "$window" native-settings.png
xdotool key Down
sleep 0.3
import -window "$window" native-settings-appearance.png
xdotool key Down
sleep 0.3
import -window "$window" native-settings-shortcuts.png
xdotool key Down
sleep 0.3
import -window "$window" native-settings-documents.png
xdotool key Down Down
sleep 0.3
import -window "$window" native-settings-about.png
xdotool key Escape
timeout 15 xdotool search --sync --onlyvisible --name '^Holonomy Native$' >/dev/null
kill -0 "$app_pid"
xdotool key ctrl+comma
timeout 15 xdotool search --sync --onlyvisible --name '^Holonomy Settings' >/dev/null
xdotool key ctrl+q
timeout 20 tail --pid="$app_pid" -f /dev/null
wait "$app_pid"
app_pid=''
fixture verify

# Reopen the actual GPUI binary, add an unsaved suffix and kill the process abruptly.
# An independent authenticated reopen must recover the earlier explicitly saved text,
# not the uncommitted suffix. This is a process-crash test, not a power-cut test.
target/debug/holonomy-gui "$HOLONOMY_NATIVE_SMOKE_PATH" &
app_pid=$!
window=$(timeout 45 xdotool search --sync --onlyvisible --name '^Holonomy Native$' | head -1)
xdotool windowfocus --sync "$window"
xdotool key Tab
xdotool type --clearmodifiers --delay 35 'native smoke passphrase'
xdotool key Return
timeout 45 xdotool search --sync --onlyvisible --name 'native-smoke.wavefunction' >/dev/null
xdotool key ctrl+End
xdotool type --clearmodifiers --delay 35 ' unsaved after reopen'
# Require the editor to report pending edits before killing it; no save or close is requested.
timeout 45 xdotool search --sync --onlyvisible --name 'native-smoke.wavefunction.*unsaved' >/dev/null
kill -KILL "$app_pid"
# wait must observe SIGKILL, not an earlier exit or a clean shutdown.
set +e
wait "$app_pid"
crash_status=$?
set -e
[[ "$crash_status" -eq 137 ]]
app_pid=''
fixture verify
echo "HOLONOMY_NATIVE_CRASH_RECOVERY committed_text=verified unsaved_suffix=absent signal=SIGKILL physical_power_loss=not_tested"

# At 200%, exercise REAL calibrated create, not the cheap test key schedule. A theme
# repaint while the title is still Working proves the window processes input during KDF.
if [[ "$GPUI_X11_SCALE_FACTOR" == 2 ]]; then
  [[ ! -e "$HOLONOMY_NATIVE_CREATE_PATH" ]]
  target/debug/holonomy-gui &
  app_pid=$!
  window=$(timeout 45 xdotool search --sync --onlyvisible --name '^Holonomy Native$' | head -1)
  xdotool windowfocus --sync "$window"
  xdotool key ctrl+n
  xdotool type --clearmodifiers --delay 5 "$HOLONOMY_NATIVE_CREATE_PATH"
  xdotool key Tab
  xdotool type --clearmodifiers --delay 5 'native responsive create passphrase'
  xdotool key Return
  timeout 20 xdotool search --sync --onlyvisible --name 'Working - Light$' >/dev/null
  xdotool key ctrl+shift+t
  timeout 20 xdotool search --sync --onlyvisible --name 'Working - Dark$' >/dev/null
  import -window "$window" native-busy.png
  timeout 60 xdotool search --sync --onlyvisible --name 'native-smoke.wavefunction.created.wavefunction' >/dev/null
  xdotool type --clearmodifiers --delay 20 'Created without blocking the window'
  xdotool key ctrl+s
  xdotool key ctrl+q
  timeout 20 tail --pid="$app_pid" -f /dev/null
  wait "$app_pid"
  app_pid=''
  fixture verify_created
fi
