#!/usr/bin/env bash
# Runs INSIDE Fedora: validate the exact downloadable binaries, no Rust/C compiler installed.
set -euxo pipefail
dnf install -y python3 xorg-x11-server-Xvfb /usr/bin/xwininfo /usr/bin/xdpyinfo xdotool ImageMagick \
  vulkan-loader mesa-vulkan-drivers libxkbcommon-x11 fontconfig alsa-lib openssl-libs \
  libX11-xcb libXcursor libXi libXrandr libXinerama libXrender \
  'libwayland-client.so.0()(64bit)' 'libwayland-cursor.so.0()(64bit)' \
  'libwayland-egl.so.1()(64bit)' 'libssl.so.3()(64bit)' 'libcrypto.so.3()(64bit)'  \
  procps-ng coreutils findutils diffutils
cat /etc/os-release
rpm -qa | sort
! command -v cargo
! command -v gcc
chmod +x target/debug/holonomy-gui target/debug/holonomy-native target/debug/native-fixture
export HOLONOMY_NATIVE_FIXTURE_BIN="$PWD/target/debug/native-fixture"
export HOLONOMY_NATIVE_SMOKE_PATH=/tmp/native-smoke.wavefunction
export DISPLAY=:99
export XDG_RUNTIME_DIR=/tmp/holonomy-runtime
mkdir -p "$XDG_RUNTIME_DIR"
chmod 700 "$XDG_RUNTIME_DIR"
Xvfb "$DISPLAY" -screen 0 2560x1800x24 -nolisten tcp &
xvfb_pid=$!
trap 'kill "$xvfb_pid" 2>/dev/null || true' EXIT
# Wait for the server, not an arbitrary sleep, before creating a graphics connection.
for attempt in {1..100}; do
  if xdpyinfo >/dev/null 2>&1; then break; fi
  sleep 0.1
done
xdpyinfo >/dev/null
ldd target/debug/holonomy-native
for scale in 1 2; do
  GPUI_X11_SCALE_FACTOR="$scale" bash scripts/qualify-native-ui.sh
  mv native-smoke.png "native-smoke-fedora-${scale}.png"
done
echo "HOLONOMY_FEDORA_RUNTIME encrypted_roundtrip=true compiler_installed=false scales=1,2"
