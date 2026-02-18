#!/bin/bash
# Cleanup any leftover mounts in the rootfs
cd "$(dirname "$0")/.." || exit 1

echo "==> Checking for leftover mounts in alpine-rootfs..."
mount | grep alpine-rootfs

echo ""
echo "==> Unmounting filesystems..."
sudo umount alpine-rootfs/proc 2>/dev/null && echo "✓ Unmounted proc" || echo "  (proc not mounted)"
sudo umount alpine-rootfs/sys 2>/dev/null && echo "✓ Unmounted sys" || echo "  (sys not mounted)"
sudo umount alpine-rootfs/dev 2>/dev/null && echo "✓ Unmounted dev" || echo "  (dev not mounted)"

echo ""
echo "==> Checking again..."
mount | grep alpine-rootfs || echo "✓ No remaining mounts"
