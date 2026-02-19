#!/usr/bin/env bash
#
# Build remora in release mode and install to /usr/local/bin.
#
set -euo pipefail

INSTALL_DIR="${1:-/usr/local/bin}"

echo "Building remora (release)..."
cargo build --release

echo "Installing to ${INSTALL_DIR}/remora..."
sudo install -m 755 target/release/remora "${INSTALL_DIR}/remora"

echo "Done. $(remora --version 2>/dev/null || echo 'remora installed')"
