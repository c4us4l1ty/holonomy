#!/usr/bin/env bash
# Regenerate the Rust -> TypeScript bridge contract.
#
# Two files, two owners:
#   app/src/core/generated-calibration.ts  <- holonomy-core (GeometryCalibration)
#   app/src/core/generated-bridge.ts       <- holonomy-shell (the six payload types)
#
# `ts-rs` overwrites its target rather than merging across writers, so one file with
# two owners is a race: whichever export finished last decided the file's contents.
# Both runs reported six passing export tests and shipped one type. Disjoint paths
# make `cargo test` idempotent, which is what a generated artifact should be.
#
# Order here is not load-bearing. It is fixed anyway, because a sequence that cannot
# matter should not look like it might.
#
# `crates/holonomy-shell/tests/bridge-contract.rs` fails if the checked-in files stop
# matching the Rust definitions.
set -euo pipefail

cd "$(dirname "$0")/.."

cargo test -p holonomy-core  --lib export_bindings -- --test-threads=1
cargo test -p holonomy-shell --lib export_bindings -- --test-threads=1

bridge=app/src/core/generated-bridge.ts
calibration=app/src/core/generated-calibration.ts
for f in "$bridge" "$calibration"; do
  if [ ! -s "$f" ]; then
    echo "$f is missing or empty; the exports above did not land" >&2
    exit 1
  fi
done

printf '%s: %s types\n' "$bridge" "$(grep -c '^export type' "$bridge")"
printf '%s: %s types\n' "$calibration" "$(grep -c '^export type' "$calibration")"

if ! git diff --quiet -- "$bridge" "$calibration" 2>/dev/null; then
  echo
  echo "generated files changed; review and commit them:"
  git --no-pager diff --stat -- "$bridge" "$calibration"
fi
