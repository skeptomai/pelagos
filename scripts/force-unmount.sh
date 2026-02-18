#!/bin/bash
# Force unmount all proc instances
cd "$(dirname "$0")/.." || exit 1

echo "==> Force unmounting all proc mounts..."
while mount | grep -q "alpine-rootfs/proc"; do
    sudo umount alpine-rootfs/proc 2>/dev/null || sudo umount -l alpine-rootfs/proc
    sleep 0.1
done

echo "==> Done. Checking..."
mount | grep alpine-rootfs || echo "✓ All clean"
