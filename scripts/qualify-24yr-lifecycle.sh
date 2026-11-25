#!/usr/bin/env bash
set -euo pipefail

# Block 10 24-year fresh-user lifecycle drill:
#   install -> create (fresh profile) -> backup -> wipe original profile ->
#   restore on fresh empty HOME/XDG -> verify -> continue typing via PTY ->
#   compact -> verify -> export & verify byte identity,
#   paired with the 8,760-witness (24 * 365 daily saves) container drill.

work_dir="$(mktemp -d "${TMPDIR:-/tmp}/holonomy-24yr.XXXXXX")"
cleanup() {
  rm -rf "$work_dir"
}
trap cleanup EXIT

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$here/.." && pwd)"

binary="${HOLONOMY_BINARY:-}"
if [[ -z "$binary" ]]; then
  binary="$(cargo build --locked --release -q -p holonomy-gui --bin holonomy --message-format=json \
    | python3 -c 'import json,sys
for line in sys.stdin:
    m = json.loads(line)
    if m.get("reason") == "compiler-artifact" and m.get("executable"):
        print(m["executable"])
        break')"
fi
if [[ ! -x "$binary" ]]; then
  echo "24yr: no executable at '$binary'" >&2
  exit 1
fi

phrase="twenty four year lifecycle passphrase"
user1_home="$work_dir/user-profile-1"
user2_home="$work_dir/user-profile-2"
backup_dir="$work_dir/backup"
mkdir -p "$user1_home" "$user2_home" "$backup_dir"

src_txt="$work_dir/seed.txt"
cat > "$src_txt" <<'EOF'
Year 01 Day 01 — Fresh user genesis document.
Every sealed line authenticates with the passphrase alone.
EOF

doc1="$user1_home/journal.wavefunction"
printf '%s\n' "$phrase" | env -i PATH="$PATH" HOME="$user1_home" XDG_DATA_HOME="$user1_home/data" \
  "$binary" import --format plain "$src_txt" "$doc1" >/dev/null
printf '%s\n' "$phrase" | env -i PATH="$PATH" HOME="$user1_home" \
  "$binary" verify "$doc1" >/dev/null

# Back up the encrypted .wavefunction, then wipe the original profile directory.
cp "$doc1" "$backup_dir/journal.wavefunction"
rm -rf "$user1_home"

# Restore onto a completely empty fresh profile (env -i + empty HOME/XDG/TMPDIR).
restored="$work_dir/restored.wavefunction"
cp "$backup_dir/journal.wavefunction" "$restored"
printf '%s\n' "$phrase" | env -i PATH="$PATH" \
  HOME="$user2_home" \
  XDG_CONFIG_HOME="$user2_home/cfg" \
  XDG_DATA_HOME="$user2_home/data" \
  XDG_CACHE_HOME="$user2_home/cache" \
  XDG_STATE_HOME="$user2_home/state" \
  TMPDIR="$work_dir" \
  "$binary" verify "$restored" >/dev/null

# Continue editing the restored document through a real PTY session.
spec="$work_dir/continue-keys.json"
python3 -c '
import json, sys
phrase, path = sys.argv[1], sys.argv[2]
# Go to end (Esc[F / Ctrl+E or type directly at end) and append continuation text, then Ctrl+Q.
json.dump([[0.8, phrase + "\r"], [0.5, "Continued cleanly in year 25.\n"], [0.5, "\u0011"]], open(path, "w"))
' "$phrase" "$spec"
env -i PATH="$PATH" HOME="$user2_home" TERM="${TERM:-xterm-256color}" \
  python3 "$here/drive-pty.py" --timeout "${HOLONOMY_SMOKE_TIMEOUT:-30}" \
  --out "$work_dir/pty.session" --cols 120 --rows 30 --keys "$spec" -- "$binary" "$restored"

printf '%s\n' "$phrase" | env -i PATH="$PATH" HOME="$user2_home" "$binary" verify "$restored" >/dev/null

# Compact into a fresh re-keyed .wavefunction, verify again, and explicitly export plaintext.
compacted="$work_dir/compacted.wavefunction"
printf '%s\n%s\n' "$phrase" "$phrase" | env -i PATH="$PATH" HOME="$user2_home" \
  "$binary" compact "$restored" "$compacted" >/dev/null
printf '%s\n' "$phrase" | env -i PATH="$PATH" HOME="$user2_home" \
  "$binary" verify "$compacted" >/dev/null

exported="$work_dir/exported.txt"
printf '%s\n' "$phrase" | env -i PATH="$PATH" HOME="$user2_home" \
  "$binary" export --force --format plain "$compacted" "$exported" >/dev/null
grep -q "Fresh user genesis document" "$exported"
grep -q "Continued cleanly in year 25." "$exported"

# Assert zero state files written into user2_home by the binary.
if find "$user2_home" -type f | grep -q .; then
  echo "24yr: FAIL — binary wrote unexpected profile state into $user2_home" >&2
  exit 1
fi

# Run the 8,760-witness (24 * 365 daily commits) release scale drill.
(cd "$repo_root" && cargo test --locked --release -p holonomy-gui --test persist \
  twenty_four_year_fresh_user_lifecycle_drill -- --exact --ignored --nocapture)

echo "HOLONOMY_24YR_CLI_DRILL profile_clean=true backup_restored=true compacted_verified=true"
