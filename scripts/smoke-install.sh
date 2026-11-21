#!/usr/bin/env bash
set -euo pipefail

# M8's smoke test: the shipped binary, driven through a real terminal, under a throwaway HOME.
#
# This is the only check in the project that runs *the binary a user runs* rather than a library
# under it. Everything else in CI calls `holonomy-text`, `holonomy-eye` or `holonomy-gui` directly,
# so a regression in `main` — argument handling, raw mode, the passphrase prompt, the save-on-quit
# path — would be invisible to all of them. That is the gap M8 exists to close.
#
# Why a pty: the window puts the terminal in raw mode with `tcgetattr(0)`, which fails against a
# pipe. `script(1)` gives the child a real terminal, so what runs here is the same code path a
# person runs, not a stub of it.
#
# What is asserted, end to end:
#   1. a document is created by typing a passphrase and some text, and quitting saves it;
#   2. the same file opens again under the same passphrase and still says what was typed;
#   3. the file is a real `.wavefunction` — it has a header, it is not plaintext, and the typed
#      sentence does not appear in the bytes;
#   4. a wrong passphrase is refused, and the file is unchanged by the attempt;
#   5. an empty passphrase is refused (D-63), and no file is created.
#
# Nothing here measures a budget. It answers one question — "does the thing a user installs
# work?" — and any answer other than yes fails the build.

work_dir="$(mktemp -d "${TMPDIR:-/tmp}/holonomy-smoke.XXXXXX")"
# A throwaway HOME, so a run cannot read or write anything in the developer's own.
home="$work_dir/home"
mkdir -p "$home"
document="$work_dir/notes.wavefunction"
log="$work_dir"

cleanup() {
  rm -rf "$work_dir"
}
trap cleanup EXIT

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
  echo "smoke: no executable at '$binary'" >&2
  exit 1
fi

phrase="correct horse battery staple"
# The sentence is deliberately distinctive: it is asserted present when the document is reopened,
# and asserted absent from the file's bytes.
sentence="the quick brown fox jumps over the lazy dog"
ctrl_q=$'\x11'

echo "smoke: binary=$binary"
echo "smoke: home=$home"

# --- 1. create, type, save on quit ------------------------------------------------------------
first="$log/first.session"
keys="$phrase\r$sentence$ctrl_q"
first_status=0
HOME="$home" TERM=xterm-256color \
  printf '%b' "$keys" \
  | HOME="$home" TERM=xterm-256color script -qec "$binary $document" "$first" >/dev/null 2>&1 \
  || first_status=$?

if [[ ! -f "$document" ]]; then
  echo "smoke: FAIL — no document was created at $document" >&2
  echo "--- session transcript (last 40 lines, escapes stripped):" >&2
  sed 's/\x1b\[[0-9;?]*[A-Za-z]//g' "$first" | tr -d '\r' | tail -40 >&2 || true
  exit 1
fi
# A document existing is not the same as a clean exit. `Ctrl+Q` on a document with open bytes seals
# them, and a refused seal exits 1 (D-49) — so a non-zero status here is a real failure, not
# noise.
if [[ $first_status -ne 0 ]]; then
  echo "smoke: FAIL — the first run exited $first_status" >&2
  sed 's/\x1b\[[0-9;?]*[A-Za-z]//g' "$first" | tr -d '\r' | tail -30 >&2 || true
  exit 1
fi
size_first="$(stat -c %s "$document")"
echo "smoke: created $document ($size_first bytes)"

# --- 2. the file is a real sealed container, not plaintext ------------------------------------
# `HEADER_MAGIC` in `crates/holonomy-line/src/format.rs` is the eight bytes `HOLWAV6\0` — asserted
# here as bytes rather than as text, so the check is the same one the format itself makes.
# `cmp` against the eight header bytes, so the trailing NUL is compared rather than dropped by a
# command substitution (bash removes it, and warns).
if printf 'HOLWAV6\0' | cmp -s - <(head -c 8 "$document"); then
  echo "smoke: ok — the file begins with the HOLWAV6 header magic"
else
  echo "smoke: FAIL — the document has no HOLWAV6 header" >&2
  head -c 32 "$document" | od -c >&2
  exit 1
fi
if grep -qa "$sentence" "$document"; then
  echo "smoke: FAIL — the typed sentence is readable in the file's bytes" >&2
  exit 1
fi
echo "smoke: ok — the typed sentence does not appear in the ciphertext"

# --- 3. reopen and read it back ---------------------------------------------------------------
second="$log/second.session"
# Open, then quit at once: `Ctrl+Q` with nothing pending writes nothing, which is the case D-51
# made reachable. The assertion is that the phrase is accepted and the run ends cleanly.
HOME="$home" TERM=xterm-256color \
  printf '%b' "$phrase\r$ctrl_q" \
  | HOME="$home" TERM=xterm-256color script -qec "$binary $document" "$second" >/dev/null 2>&1
status_second=$?
if [[ $status_second -ne 0 ]]; then
  echo "smoke: FAIL — reopening the document exited $status_second" >&2
  sed 's/\x1b\[[0-9;?]*[A-Za-z]//g' "$second" | tr -d '\r' | tail -30 >&2 || true
  exit 1
fi
# The transcript is the screen; the sentence has to be on it, which is what "reopened" means to a
# reader. Matching after stripping escapes, because the renderer positions the cursor between rows.
if ! sed 's/\x1b\[[0-9;?]*[A-Za-z]//g' "$second" | tr -d '\r' | grep -qF "quick brown fox"; then
  echo "smoke: FAIL — the reopened document did not show the typed sentence" >&2
  sed 's/\x1b\[[0-9;?]*[A-Za-z]//g' "$second" | tr -d '\r' | tail -30 >&2 || true
  exit 1
fi
echo "smoke: ok — the same passphrase reopened the document and it showed the typed sentence"

# --- 4. a wrong passphrase is refused, and changes nothing ------------------------------------
before_bytes="$(stat -c %s "$document")"
before_sum="$(sha256sum "$document" | cut -d' ' -f1)"
wrong="$log/wrong.session"
if HOME="$home" TERM=xterm-256color \
  printf '%b' "the wrong phrase\r" \
  | HOME="$home" TERM=xterm-256color script -qec "$binary $document" "$wrong" >/dev/null 2>&1; then
  echo "smoke: FAIL — a wrong passphrase was accepted" >&2
  exit 1
fi
after_sum="$(sha256sum "$document" | cut -d' ' -f1)"
if [[ "$before_sum" != "$after_sum" || "$before_bytes" != "$(stat -c %s "$document")" ]]; then
  echo "smoke: FAIL — a refused open modified the document" >&2
  exit 1
fi
echo "smoke: ok — a wrong passphrase was refused and the file is byte-identical"

# --- 5. an empty passphrase is refused (D-63) and creates nothing ------------------------------
uncreated="$work_dir/never.wavefunction"
empty="$log/empty.session"
if HOME="$home" TERM=xterm-256color \
  printf '\r' \
  | HOME="$home" TERM=xterm-256color script -qec "$binary $uncreated" "$empty" >/dev/null 2>&1; then
  echo "smoke: FAIL — an empty passphrase was accepted" >&2
  exit 1
fi
if [[ -e "$uncreated" ]]; then
  echo "smoke: FAIL — an empty passphrase created a document" >&2
  exit 1
fi
echo "smoke: ok — an empty passphrase was refused and created no file"

# --- summary ----------------------------------------------------------------------------------
# The marker is what CI greps, following the other measured jobs. The numbers are the ones this run
# actually saw, so a failure is a diff and a pass is a record.
ciphertext_bytes="$size_first"
echo "HOLONOMY_SMOKE binary=$(basename "$binary") document_bytes=$ciphertext_bytes" \
  "sentence_in_ciphertext=no wrong_phrase=refused empty_phrase=refused reopen=ok"
echo "smoke: PASS"
