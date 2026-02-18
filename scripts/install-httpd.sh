#!/bin/bash
# Install busybox-extras (httpd, telnet, etc.) into the Alpine rootfs
# using remora itself — demonstrates NAT + DNS working end-to-end.
#
# Usage: sudo -E ./install-httpd.sh

set -e
cd "$(dirname "$0")/.." || exit 1

echo "=== Installing busybox-extras via remora ==="
echo ""

# Register the rootfs so we can refer to it by name
cargo run -- rootfs import alpine ./alpine-rootfs 2>/dev/null || true

cargo run -- run \
    --network bridge \
    --nat \
    --dns 8.8.8.8 \
    alpine \
    apk add --no-cache busybox-extras

echo ""
echo "Verifying httpd is now available..."
ls -la alpine-rootfs/usr/sbin/httpd 2>/dev/null && echo "httpd installed!" || echo "ERROR: httpd not found"
