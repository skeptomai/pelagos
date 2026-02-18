#!/bin/bash
# Fix Alpine rootfs - replace ARM version with x86_64 version

set -e
cd "$(dirname "$0")/.." || exit 1

echo "==> Unmounting any mounted filesystems in old rootfs..."
sudo umount alpine-rootfs/sys 2>/dev/null || true
sudo umount alpine-rootfs/proc 2>/dev/null || true
sudo umount alpine-rootfs/dev 2>/dev/null || true

echo "==> Cleaning old ARM rootfs (requires sudo)..."
sudo rm -rf alpine-rootfs

echo "==> Creating fresh x86_64 Alpine rootfs from Docker..."
mkdir alpine-rootfs

echo "==> Pulling Alpine Linux image..."
docker pull alpine:latest

echo "==> Extracting rootfs..."
docker export $(docker create alpine:latest) | tar -C alpine-rootfs -xf -

echo "==> Verifying architecture..."
file alpine-rootfs/bin/busybox

echo ""
echo "✅ Success! x86_64 Alpine rootfs created."
echo ""
echo "Now test with:"
echo "  sudo -E ./target/debug/remora --exe /bin/ash --rootfs ./alpine-rootfs --uid 1000 --gid 1000"
