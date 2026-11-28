#!/usr/bin/env bash
# Build and run the canonical GPUI desktop from a source checkout.
set -euo pipefail
cd "$(dirname "$0")/.."
cargo build --locked -p holonomy-native --features native-ui
cargo build --locked -p holonomy-gui --bin holonomy-gui
exec target/debug/holonomy-gui "$@"
