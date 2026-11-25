#!/usr/bin/env bash
set -euo pipefail

# Real Xvfb qualification gate for `holonomy-gui` (`x11.rs` + `canvas.rs` + `PixelSurface`).
# Boots a real headless X11 server (`Xvfb`), launches the compiled `holonomy-gui` desktop binary,
# injects real X11 KeyPress events via `xdotool` (passphrase prompt, typing prose, Ctrl+S save,
# Ctrl+H history drawer, Ctrl+Q quit), inspects a real `xwd` root framebuffer capture to verify
# non-background GUI rendering, and verifies the persisted `.wavefunction` container.

for bin in Xvfb xdotool xwd; do
    if ! command -v "$bin" >/dev/null 2>&1; then
        echo "error: missing required X11 qualification tool '$bin'" >&2
        exit 1
    fi
done

DISP=":99"
WORKDIR="$(mktemp -d /tmp/holonomy-xvfb.XXXXXX)"
DOC_PATH="$WORKDIR/xvfb-test.wavefunction"
XWD_PATH="$WORKDIR/frame.xwd"
XVFB_PID=""
GUI_PID=""

cleanup() {
    local status=$?
    if [[ $status -ne 0 && -f "$WORKDIR/gui.log" ]]; then
        echo "=== holonomy-gui stdout/stderr ===" >&2
        cat "$WORKDIR/gui.log" >&2 || true
    fi
    if [[ -n "$GUI_PID" ]] && kill -0 "$GUI_PID" 2>/dev/null; then
        kill "$GUI_PID" 2>/dev/null || true
        wait "$GUI_PID" 2>/dev/null || true
    fi
    if [[ -n "$XVFB_PID" ]] && kill -0 "$XVFB_PID" 2>/dev/null; then
        kill "$XVFB_PID" 2>/dev/null || true
        wait "$XVFB_PID" 2>/dev/null || true
    fi
    rm -rf "$WORKDIR"
    exit "$status"
}
trap cleanup EXIT

Xvfb "$DISP" -screen 0 1280x900x24 -ac >"$WORKDIR/xvfb.log" 2>&1 &
XVFB_PID=$!

export DISPLAY="$DISP"
for _ in $(seq 1 50); do
    if xdotool getdisplaygeometry >/dev/null 2>&1; then
        break
    fi
    sleep 0.1
done

cargo build --locked --release -p holonomy-gui --bin holonomy-gui --bin holonomy
GUI_BIN="target/release/holonomy-gui"
CLI_BIN="target/release/holonomy"

"$GUI_BIN" "$DOC_PATH" >"$WORKDIR/gui.log" 2>&1 &
GUI_PID=$!

# Wait for window mapping.
WIN_ID=""
for _ in $(seq 1 80); do
    WIN_ID="$(xdotool search --name 'Holonomy' 2>/dev/null | head -n1 || true)"
    if [[ -n "$WIN_ID" ]]; then
        break
    fi
    sleep 0.1
done
if [[ -z "$WIN_ID" ]]; then
    echo "error: holonomy-gui did not map an X11 window on $DISP" >&2
    cat "$WORKDIR/xvfb.log" >&2 || true
    exit 1
fi

xdotool windowfocus --sync "$WIN_ID"
sleep 0.3

# Create encrypted container via passphrase modal (single entry when path is provided).
xdotool type --clearmodifiers --delay 25 "xvfb-secret-pass"
xdotool key --clearmodifiers Return

# Wait for Argon2id calibration + container creation on disk before typing prose.
for _ in $(seq 1 150); do
    if [[ -s "$DOC_PATH" ]]; then
        break
    fi
    sleep 0.1
done
if [[ ! -s "$DOC_PATH" ]]; then
    echo "error: holonomy-gui did not create $DOC_PATH after passphrase submit" >&2
    exit 1
fi
sleep 0.3

xdotool type --clearmodifiers --delay 20 "Hello Xvfb Quiet Markdown"
xdotool key --clearmodifiers ctrl+a
sleep 0.1
xdotool key --clearmodifiers ctrl+k
sleep 0.15
xdotool type --clearmodifiers --delay 15 "https://holonomy.example"
xdotool key --clearmodifiers Return
sleep 0.15
xdotool key --clearmodifiers ctrl+s
sleep 0.3
xdotool key --clearmodifiers ctrl+shift+d
sleep 0.2
xdotool key --clearmodifiers ctrl+g
sleep 0.2
xdotool key --clearmodifiers Escape
sleep 0.15
xdotool key --clearmodifiers ctrl+h
sleep 0.3

# Capture actual X11 framebuffer and verify painted non-uniform pixels across toolbar/editor/drawer.
xwd -root -display "$DISP" -out "$XWD_PATH"
python3 - "$XWD_PATH" <<'PY'
import struct, sys
from collections import Counter

data = open(sys.argv[1], "rb").read()
header_size, file_version, pixmap_format, pixmap_depth, pixmap_width, pixmap_height = struct.unpack(">6I", data[:24])
bytes_per_line, bits_per_pixel = struct.unpack(">2I", data[48:56])
ncolors = struct.unpack(">I", data[76:80])[0]
offset = header_size + ncolors * 12
pixels = data[offset:]
# Sample across central editor + right history drawer rows.
counts = Counter()
for y in range(60, min(pixmap_height, 860), 4):
    row = pixels[y * bytes_per_line : y * bytes_per_line + pixmap_width * 4]
    for x in range(100, min(pixmap_width, 1200), 4):
        counts[row[x * 4 : x * 4 + 4]] += 1
distinct = len(counts)
print(f"HOLONOMY_XVFB_CAPTURE width={pixmap_width} height={pixmap_height} distinct_colors={distinct}")
assert distinct >= 8, f"expected rich painted UI/text framebuffer, got only {distinct} distinct sampled colors"
PY

# Close drawer, save, and quit cleanly via Ctrl+Q, then verify and inspect exported plaintext.
xdotool key --clearmodifiers Escape
sleep 0.15
xdotool key --clearmodifiers ctrl+s
sleep 0.3
xdotool key --clearmodifiers ctrl+q

wait "$GUI_PID" || true
GUI_PID=""

if [[ ! -f "$DOC_PATH" ]]; then
    echo "error: expected persisted document at $DOC_PATH" >&2
    exit 1
fi

printf 'xvfb-secret-pass\n' | "$CLI_BIN" verify "$DOC_PATH"
EXPORT_PATH="$WORKDIR/exported.txt"
printf 'xvfb-secret-pass\n' | "$CLI_BIN" export --format plain "$DOC_PATH" "$EXPORT_PATH"
cat "$EXPORT_PATH"
grep -q "Hello Xvfb Quiet Markdown" "$EXPORT_PATH"
echo "xvfb: PASS"
