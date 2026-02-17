#!/bin/bash
# Build Alpine Linux rootfs by extracting from Docker image
# Requires: Docker
# Advantage: Always gets the latest Alpine version
# Disadvantage: Requires Docker daemon running

set -e

echo "========================================"
echo "  Alpine Rootfs Builder (Docker)"
echo "========================================"
echo ""

# Check if Docker is available
if ! command -v docker &> /dev/null; then
    echo "❌ Error: Docker is not installed or not in PATH"
    echo ""
    echo "Install Docker first, or use build-rootfs-tarball.sh instead"
    exit 1
fi

# Check if Docker daemon is running
if ! docker info &> /dev/null; then
    echo "❌ Error: Docker daemon is not running"
    echo ""
    echo "Start Docker first, or use build-rootfs-tarball.sh instead"
    exit 1
fi

echo "==> Cleaning old rootfs (requires sudo)..."
sudo umount alpine-rootfs/sys 2>/dev/null || true
sudo umount alpine-rootfs/proc 2>/dev/null || true
sudo umount alpine-rootfs/dev 2>/dev/null || true
sudo rm -rf alpine-rootfs

echo "==> Creating fresh Alpine rootfs directory..."
mkdir alpine-rootfs

echo "==> Pulling Alpine Linux image..."
docker pull alpine:latest

echo "==> Extracting rootfs from Docker image..."
docker export $(docker create alpine:latest) | tar -C alpine-rootfs -xf -

echo "==> Verifying architecture..."
file alpine-rootfs/bin/busybox

echo "==> Verifying essential tools..."
ls -lh alpine-rootfs/bin/ash alpine-rootfs/bin/busybox

echo ""
echo "✅ Success! Alpine Linux rootfs created via Docker"
echo ""
echo "Location: $(pwd)/alpine-rootfs"
echo "Size: $(du -sh alpine-rootfs | cut -f1)"
echo ""
echo "Test with:"
echo "  sudo -E ./target/debug/remora --exe /bin/ash --rootfs ./alpine-rootfs --uid 1000 --gid 1000"
echo ""
echo "Or run the seccomp demo:"
echo "  sudo -E cargo run --example seccomp_demo"
echo ""
