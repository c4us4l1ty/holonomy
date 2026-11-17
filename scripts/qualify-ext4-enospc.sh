#!/usr/bin/env bash
set -euo pipefail

# Exercise the real holonomy-line append path on a bounded ext4 loop filesystem. This provides
# ext4 ENOSPC evidence only; it is not a device power-cut or hardware-cache qualification.
work_dir="$(mktemp -d /tmp/holonomy-ext4-qualification.XXXXXX)"
image="$work_dir/ext4.img"
mount_dir="$work_dir/mnt"
mkdir -p "$mount_dir"

cleanup() {
  if mountpoint -q "$mount_dir"; then
    sudo umount "$mount_dir"
  fi
  rm -rf "$work_dir"
}
trap cleanup EXIT

truncate -s 128M "$image"
mkfs.ext4 -F -m 0 "$image"
sudo mount -o loop "$image" "$mount_dir"
sudo chown "$(id -u):$(id -g)" "$mount_dir"

fs_type="$(findmnt -n -o FSTYPE --target "$mount_dir")"
[[ "$fs_type" == "ext4" ]]
loop_device="$(sudo losetup -j "$image" | cut -d: -f1)"
initial_free_bytes="$(df -B1 --output=avail "$mount_dir" | tail -n 1 | tr -d '[:space:]')"
e2fsprogs_version="$(mkfs.ext4 -V 2>&1 | tr '\n' ' ')"
printf 'HOLONOMY_EXT4_ENV kernel=%s; e2fsprogs=%s; filesystem=%s; image_bytes=%s; loop_device=%s; backing_image=%s; initial_free_bytes=%s\n' \
  "$(uname -srmo)" "$e2fsprogs_version" "$fs_type" "$(stat -c %s "$image")" \
  "$loop_device" "$image" "$initial_free_bytes"
printf 'HOLONOMY_EXT4_MOUNT %s\n' "$(findmnt -n -o SOURCE,FSTYPE,OPTIONS --target "$mount_dir")"

HOLONOMY_ENOSPC_DIR="$mount_dir" \
  cargo test --locked -p holonomy-line --lib \
    tests::real_enospc_preserves_an_authenticated_witness_on_ext4 \
    -- --exact --ignored --nocapture
