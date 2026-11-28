#!/usr/bin/env bash
# Build both GPUI desktop executables once, preserving the caller's working directory.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
profile="${HOLONOMY_BUILD_PROFILE:-dev}"
case "$profile" in
  dev) output=debug ;;
  release) output=release ;;
  *) echo 'HOLONOMY_BUILD_PROFILE must be dev or release' >&2; exit 2 ;;
esac
# An explicit target dir keeps the executable location deterministic even when Cargo's
# target-dir is customized. Do not cd: relative document paths belong to the caller.
cargo build --locked --manifest-path "$root/Cargo.toml" --target-dir "$root/target" \
  -p holonomy-native -p holonomy-gui --bin holonomy-native --bin holonomy-gui \
  --features holonomy-native/native-ui --profile "$profile"
exec "$root/target/$output/holonomy-gui" "$@"
