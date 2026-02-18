#!/bin/bash
# Test that the mount propagation fix works

set -e
cd "$(dirname "$0")/.." || exit 1

echo "==> Testing mount propagation fix"
echo ""

# Clean up any existing mounts first
echo "==> Cleaning existing mounts..."
sudo umount alpine-rootfs/proc 2>/dev/null || true
sudo umount alpine-rootfs/proc 2>/dev/null || true
sudo umount alpine-rootfs/sys 2>/dev/null || true

# Verify clean state
BEFORE=$(mount | grep alpine-rootfs | wc -l)
echo "Mounts before test: $BEFORE"

if [ "$BEFORE" != "0" ]; then
    echo "⚠️  Warning: Could not clean all mounts. Found $BEFORE remaining."
    mount | grep alpine-rootfs
    exit 1
fi

echo "✓ Clean state confirmed"
echo ""

# Build latest version
echo "==> Building latest code..."
cargo build --quiet

echo ""
echo "==> Launching container (will run 'ls / && exit')..."
echo ""

# Launch container that exits immediately
sudo -E ./target/debug/remora \
    --exe /init.sh \
    --rootfs ./alpine-rootfs \
    --uid 1000 \
    --gid 1000 <<'EOF'
ls /
exit
EOF

echo ""
echo "==> Container exited, checking for mount leaks..."
echo ""

# Check for leaked mounts
AFTER=$(mount | grep alpine-rootfs | wc -l)

if [ "$AFTER" = "0" ]; then
    echo "✅ SUCCESS! No mount leaks detected."
    echo ""
    exit 0
else
    echo "❌ FAILURE! Found $AFTER leaked mounts:"
    mount | grep alpine-rootfs
    echo ""
    echo "Mount propagation fix did not work."
    exit 1
fi
