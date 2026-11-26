#!/usr/bin/env bash
set -euo pipefail

# Binary-level crash-recovery drill: kill the shipped editor with SIGKILL *after* it has
# committed a witness, then prove a fresh process reopens the same file and shows the committed
# text. This is the part of L-6 a sandbox can exercise: abrupt process death, not a physical
# power cut, controller-cache behaviour or an arbitrary disk-sector failure. L-6 stays `spec`.
#
# The unit-level counterpart is `Document::trailing_bytes` in persist.rs (a torn append tail is
# discarded); this script complements it by driving the actual binary the user installs, through
# a real pty, including its passphrase prompt, save path and reopen path.
#
# What is deliberately NOT claimed: that text typed after the last commit survives. The open
# typing buffer is not durable until a save; the driver kills before autosave and the script says
# so in its marker. Killing at an arbitrary instant in the *unsaved* window is not a recovery
# guarantee, and neither is SIGKILL a substitute for cutting power to the device.

work_dir="$(mktemp -d "${TMPDIR:-/tmp}/holonomy-crash.XXXXXX")"
home_dir="$work_dir/home"
mkdir -p "$home_dir"

cleanup() {
  rm -rf "$work_dir"
}
trap cleanup EXIT

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
binary="${HOLONOMY_BINARY:-}"
if [[ -z "$binary" ]]; then
  binary="$(cargo build --locked -q -p holonomy-gui --bin holonomy --message-format=json \
    | python3 -c 'import json,sys
for line in sys.stdin:
    m = json.loads(line)
    if m.get("reason") == "compiler-artifact" and m.get("executable"):
        print(m["executable"])
        break')"
fi
if [[ ! -x "$binary" ]]; then
  echo "crash: no executable at '$binary'" >&2
  exit 1
fi

phrase="correct horse battery staple"

# Type a passphrase, then one distinctive sentence and an explicit Ctrl+S. `kill_after` is
# measured from process start: 0.8 s for the prompt, 0.5 s for the sentence and save, then the
# caller's extra delay. The pty driver sends the deliberate SIGKILL and marks it as intentional.
crash_once() {
  local index="$1"
  local extra_delay="$2"
  local document="$work_dir/trial-$index.wavefunction"
  local transcript="$work_dir/crash-$index.session"
  local keys="$work_dir/crash-keys-$index.json"
  local sentence="committed trial $index survive sigkill alpha bravo charlie"
  local kill_after
  kill_after="$(python3 -c 'import sys; print(1.3 + float(sys.argv[1]))' "$extra_delay")"

  python3 -c '
import json, sys
phrase, sentence, path = sys.argv[1], sys.argv[2], sys.argv[3]
json.dump([[0.8, phrase + "\r"], [0.5, sentence + "\u0013"]], open(path, "w"))
' "$phrase" "$sentence" "$keys"

  python3 "$here/drive-pty.py" \
    --timeout "${HOLONOMY_CRASH_TIMEOUT:-30}" \
    --out "$transcript" \
    --keys "$keys" \
    --kill-after "$kill_after" \
    --expect-kill \
    -- "$binary" "$document" >/dev/null

  # The reopen has no kill: a clean Ctrl+Q is itself a successful save of an unchanged file.
  local reopened="$work_dir/reopen-$index.session"
  local reopen_keys="$work_dir/reopen-keys-$index.json"
  python3 -c '
import json, sys
json.dump([[0.8, sys.argv[1] + "\r"], [0.5, "\u0011"]], open(sys.argv[2], "w"))
' "$phrase" "$reopen_keys"
  python3 "$here/drive-pty.py" \
    --timeout "${HOLONOMY_CRASH_TIMEOUT:-30}" \
    --out "$reopened" \
    --keys "$reopen_keys" \
    -- "$binary" "$document" >/dev/null

  if ! sed 's/\x1b\[[0-9;?]*[A-Za-z]//g' "$reopened" | tr -d '\r' | grep -qF "$sentence"; then
    echo "crash: FAIL — committed text '$sentence' was lost after SIGKILL (trial $index)" >&2
    sed 's/\x1b\[[0-9;?]*[A-Za-z]//g' "$reopened" | tr -d '\r' | tail -30 >&2 || true
    return 1
  fi

  # Authentication is checked by the shipped CLI, not inferred from the screen.
  local verify_out
  verify_out="$(printf '%s\n' "$phrase" | "$binary" verify "$document")"
  if [[ "$verify_out" != *"verified ("* ]]; then
    echo "crash: FAIL — reopened world-line failed verify (trial $index): $verify_out" >&2
    return 1
  fi
  echo "crash: ok — trial $index committed text survived SIGKILL and verify passed"
}

# Three different points after the same commit boundary, so a race in the save barrier cannot
# hide behind one lucky delay. Delays are measured after Ctrl+S was delivered; the shortest still
# leaves time for the witness append and its sync to return before the kill.
trials=0
for delay in 0.50 1.00 2.00; do
  trials=$((trials + 1))
  crash_once "$trials" "$delay"
done

echo "HOLONOMY_CRASH_RECOVERY trials=$trials signal=SIGKILL committed_after_kill=yes \
verify=yes scope=committed_witness_only_physical_power_loss_not_tested"
echo "crash: PASS"
