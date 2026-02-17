#!/bin/bash
# Build Alpine Linux rootfs by downloading official minirootfs tarball
# Requires: curl or wget, tar, sudo
# Advantage: No Docker required
# Disadvantage: Need to specify Alpine version

set -e

echo "========================================"
echo "  Alpine Rootfs Builder (Tarball)"
echo "========================================"
echo ""

# Alpine version to download
ALPINE_VERSION="3.21"  # Latest stable as of Feb 2026
ALPINE_MINOR="3.21.0"  # Full version with patch number

# Detect architecture
ARCH=$(uname -m)
case "$ARCH" in
    x86_64)
        ALPINE_ARCH="x86_64"
        ;;
    aarch64)
        ALPINE_ARCH="aarch64"
        ;;
    *)
        echo "❌ Error: Unsupported architecture: $ARCH"
        echo "Supported: x86_64, aarch64"
        exit 1
        ;;
esac

ALPINE_URL="https://dl-cdn.alpinelinux.org/alpine/v${ALPINE_VERSION}/releases/${ALPINE_ARCH}/alpine-minirootfs-${ALPINE_MINOR}-${ALPINE_ARCH}.tar.gz"
TARBALL="alpine-minirootfs.tar.gz"

echo "Alpine version: ${ALPINE_MINOR}"
echo "Architecture: ${ALPINE_ARCH}"
echo "Download URL: ${ALPINE_URL}"
echo ""

# Check for download tool
if command -v curl &> /dev/null; then
    DOWNLOAD_CMD="curl -L -o"
elif command -v wget &> /dev/null; then
    DOWNLOAD_CMD="wget -O"
else
    echo "❌ Error: Neither curl nor wget found"
    echo "Install one of them first"
    exit 1
fi

echo "==> Cleaning old rootfs (requires sudo)..."
sudo umount alpine-rootfs/sys 2>/dev/null || true
sudo umount alpine-rootfs/proc 2>/dev/null || true
sudo umount alpine-rootfs/dev 2>/dev/null || true
sudo rm -rf alpine-rootfs

echo "==> Downloading Alpine minirootfs tarball..."
$DOWNLOAD_CMD "$TARBALL" "$ALPINE_URL"

echo "==> Verifying download..."
if [ ! -f "$TARBALL" ]; then
    echo "❌ Error: Download failed"
    exit 1
fi

echo "==> Creating rootfs directory..."
mkdir alpine-rootfs

echo "==> Extracting tarball (requires sudo for proper permissions)..."
sudo tar -C alpine-rootfs -xzf "$TARBALL"

echo "==> Setting ownership to current user..."
sudo chown -R $(id -u):$(id -g) alpine-rootfs

echo "==> Cleaning up tarball..."
rm "$TARBALL"

echo "==> Verifying architecture..."
file alpine-rootfs/bin/busybox

echo "==> Verifying essential tools..."
ls -lh alpine-rootfs/bin/ash alpine-rootfs/bin/busybox

echo ""
echo "✅ Success! Alpine Linux rootfs created from tarball"
echo ""
echo "Location: $(pwd)/alpine-rootfs"
echo "Size: $(du -sh alpine-rootfs | cut -f1)"
echo "Version: Alpine Linux ${ALPINE_MINOR} (${ALPINE_ARCH})"
echo ""
echo "Test with:"
echo "  sudo -E ./target/debug/remora --exe /bin/ash --rootfs ./alpine-rootfs --uid 1000 --gid 1000"
echo ""
echo "Or run the seccomp demo:"
echo "  sudo -E cargo run --example seccomp_demo"
echo ""
