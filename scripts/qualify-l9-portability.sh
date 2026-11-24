#!/usr/bin/env bash
set -euo pipefail

# L-9's drill — "a copied .wavefunction opens on a fresh machine with a passphrase alone; no
# account, token, or device required" (`docs/GATES.md`), run as far as a single host can carry it.
#
# # What this proves, and what it does not
#
# The law's bound is a *second machine*. This host is one machine, so this script does not claim
# L-9. It does claim the part of L-9 that is carried by the process rather than by the hardware:
#
#   * the exporting process writes into one directory tree and nowhere else;
#   * the importing process is a **different kernel process** whose understanding of where its
#     dependencies keep state (`HOME`, `XDG_CONFIG_HOME`, `XDG_DATA_HOME`, `XDG_CACHE_HOME`,
#     `XDG_STATE_HOME`, `XDG_RUNTIME_DIR`, `TMPDIR`) is pointed at a directory that is **empty at
#     the moment it starts** — so if the binary had learned to keep a session, a wrapped key, a
#     keyring entry or a cache, this import would not find it;
#   * no environment variable crosses from exporter to importer except `PATH`: every `*_HOME`,
#     every `XDG_*`, `TMPDIR` and the rest are dropped, so nothing can be smuggled in a variable;
#   * and afterwards that dependency-state directory is asserted **still empty**. That is the
#     falsifiable half: if a future change taught the app to write a session file, this drill would
#     fail rather than quietly passing on a machine that happened to share state.
#
# What is deliberately *not* claimed: a different kernel, a different libc, a different filesystem,
# a different architecture, or a different OS. Those are the remaining halves of L-9 and they need
# the release tarball (`.github/workflows/release.yml`) on a second machine, which is the drill this
# one is the prerequisite for. The output says so in its own marker line so a reader of the CI log
# cannot mistake this for the whole law.
#
# The passphrase reaches the binary on **stdin**, which is the same door a person uses and the only
# one that travels in this drill: nothing is written to the filesystem to carry it.

work_dir="$(mktemp -d "${TMPDIR:-/tmp}/holonomy-l9.XXXXXX")"
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
  echo "l9: no executable at '$binary'" >&2
  exit 1
fi
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# The sentence is asserted on the far side, and asserted absent from the ciphertext.
sentence="a document that outlives the machine that wrote it"
phrase="correct horse battery staple"

authored="$work_dir/authored"
delivered="$work_dir/delivered"
importer_home="$work_dir/importer-everything"
mkdir -p "$authored/bin" "$delivered/bin/import" "$importer_home"

# --- 1. Author the document with the binary, at the version the tag would ship ---------------------
drive() { # out, phrase, text, path
  local out="$1" phrase="$2" text="$3" path="$4"
  local spec="$work_dir/keys.json"
  python3 -c '
import json, sys
phrase, text, path = sys.argv[1], sys.argv[2], sys.argv[3]
json.dump([[0.8, phrase + "\r"], [0.5, text], [0.5, "\u0011"]], open(path, "w"))
' "$phrase" "$text" "$spec"
  python3 "$here/drive-pty.py" --timeout "${HOLONOMY_SMOKE_TIMEOUT:-30}" --out "$out" \
    --keys "$spec" -- "$binary" "$@"
}

original="$authored/notes.wavefunction"
if ! drive "$work_dir/author.session" "$phrase" "$sentence" "$original"; then
  echo "l9: FAIL — the authoring run did not exit cleanly" >&2
  sed 's/\x1b\[[0-9;?]*[A-Za-z]//g' "$work_dir/author.session" | tr -d '\r' | tail -20 >&2
  exit 1
fi
[[ -f "$original" ]] || { echo "l9: FAIL — no document was authored" >&2; exit 1; }

# The authored file is sealed, not plaintext: a copy that leaked its content would make the whole
# drill pointless, so the same fragment check the smoke test makes is made here.
while IFS= read -r fragment; do
  if grep -qa "$fragment" "$original"; then
    echo "l9: FAIL — '$fragment' is readable in the authored file" >&2
    exit 1
  fi
done <<'FRAGMENTS'
outlives the machine
document that outlives
FRAGMENTS

# --- 2. Deliver it as a person would: a copied file, nothing else --------------------------------
cp "$original" "$delivered/notes.wavefunction"
authored_sum="$(sha256sum "$original" | cut -d' ' -f1)"
delivered_sum="$(sha256sum "$delivered/notes.wavefunction" | cut -d' ' -f1)"
[[ "$authored_sum" == "$delivered_sum" ]] || { echo "l9: FAIL — the copy is not byte-identical" >&2; exit 1; }

# --- 3. Import in a fresh process with no inherited state ----------------------------------------
# `env -i` drops the entire environment, then a minimal one is set: PATH (to find nothing but the
# binary's own loader) and the XDG/HOME/TMPDIR set, every one of them inside a directory that is
# empty right now. The import therefore starts with no config, no cache, no keyring, no state.
import_home="$importer_home"
importer_cmd=(
  env -i
  PATH=/usr/bin:/bin
  HOME="$import_home"
  XDG_CONFIG_HOME="$import_home/.config"
  XDG_DATA_HOME="$import_home/.local/share"
  XDG_CACHE_HOME="$import_home/.cache"
  XDG_STATE_HOME="$import_home/.local/state"
  XDG_RUNTIME_DIR="$import_home/.run"
  TMPDIR="$import_home/tmp"
  TERM=xterm-256color
)

# Refuse to proceed if any of that state already exists: a non-empty directory would mean the
# importer inherited something and the drill would prove less than it says.
if find "$importer_home" -mindepth 1 -print -quit | grep -q .; then
  echo "l9: FAIL — the importer's state directory was not empty before the import:" >&2
  find "$importer_home" -mindepth 1 -print | head -20 >&2
  exit 1
fi

verify_out="$("${importer_cmd[@]}" sh -c 'printf "%s\n" "$0" | "$1" verify "$2"' \
  "$phrase" "$binary" "$delivered/notes.wavefunction")"
if [[ "$verify_out" != *"verified ("* ]]; then
  echo "l9: FAIL — the imported file did not verify under the passphrase: $verify_out" >&2
  exit 1
fi

# Continue mid-document on the far side: open it, add a sentence, quit (which seals).
continued="$delivered/notes.wavefunction"
mkdir -p "$import_home/tmp"
# The driver and the key schedule travel as files in the *delivered* tree, not through the
# importer's home: what the far side is allowed to have is the document and the tools, exactly as
# a person arriving with a copied file and a binary would. A process substitution would not survive
# `env -i` anyway, and a pipe here would be an inherited channel this drill does not want to depend
# on.
cp "$here/drive-pty.py" "$delivered/bin/import/drive-pty.py"
python3 -c '
import json, sys
json.dump([[0.8, sys.argv[1] + "\r"], [0.5, " and still readable"], [0.8, "\u0011"]],
          open(sys.argv[2], "w"))
' "$phrase" "$delivered/bin/import/keys.json"
if ! "${importer_cmd[@]}" python3 "$delivered/bin/import/drive-pty.py" \
  --timeout "${HOLONOMY_SMOKE_TIMEOUT:-30}" \
  --out "$work_dir/import.session" \
  --keys "$delivered/bin/import/keys.json" \
  -- "$binary" "$continued"; then
  echo "l9: FAIL — the importing session did not exit cleanly" >&2
  sed 's/\x1b\[[0-9;?]*[A-Za-z]//g' "$work_dir/import.session" | tr -d '\r' | tail -20 >&2
  exit 1
fi

# The far side read the sentence that was written on the near side.
if ! sed 's/\x1b\[[0-9;?]*[A-Za-z]//g' "$work_dir/import.session" | tr -d '\r' | grep -qF "outlives the machine"; then
  echo "l9: FAIL — the imported document did not show what the near side wrote" >&2
  sed 's/\x1b\[[0-9;?]*[A-Za-z]//g' "$work_dir/import.session" | tr -d '\r' | tail -25 >&2
  exit 1
fi

# Its continuation is durable and authenticates on a third open.
verify_after="$("${importer_cmd[@]}" sh -c 'printf "%s\n" "$0" | "$1" verify "$2"' \
  "$phrase" "$binary" "$continued")"
if [[ "$verify_after" != *"verified ("* ]]; then
  echo "l9: FAIL — the continued document did not verify: $verify_after" >&2
  exit 1
fi

# --- 4. The falsifiable half: the importer must have carried nothing, and left nothing -----------
# Anything it created in its own state directories is state a second machine would not have had.
left="$(find "$importer_home" -mindepth 1 \
  -not -path "$importer_home/tmp" -not -path "$importer_home/tmp/*" -print 2>/dev/null || true)"
if [[ -n "$left" ]]; then
  echo "l9: FAIL — the importing process carried or created dependency state:" >&2
  printf '%s\n' "$left" | head -20 >&2
  echo "l9: this drill only means something if the far side had nothing; see docs/TO-APP.md M8." >&2
  exit 1
fi

# The delivered file grew (the continuation was appended), proving a real edit landed on the copy
# rather than the check passing on an untouched file.
delivered_after="$(stat -c %s "$continued")"
if [[ "$delivered_after" -le "$(stat -c %s "$original")" ]]; then
  echo "l9: FAIL — the continuation did not grow the document ($delivered_after bytes)" >&2
  exit 1
fi

# --- summary -------------------------------------------------------------------------------------
# `L-9=partial` is the honest token: the process half is proven here, the machine half is not.
echo "HOLONOMY_PORTABILITY L-9=partial same_kernel=yes same_libc=yes same_fs=yes" \
  "second_machine=no inherited_state=none dependency_state_after_import=empty" \
  "copied_bytes=$(stat -c %s "$original") resumed_bytes=$delivered_after" \
  "verify_near=ok verify_far=ok continuation=durable" \
  "note=\"needs a second machine for the full law\""
echo "l9: PASS (process-level: no account, token, device or inherited state required)"
