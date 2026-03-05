#!/usr/bin/env bash
#
# Build pelagos in release mode and install to /usr/local/bin.
#
# Usage:  ./scripts/install.sh [INSTALL_DIR]
#
# If run as a normal user, builds with your toolchain and uses sudo
# only to copy the binary. If run as root (e.g. in CI), skips sudo.
#
set -euo pipefail

INSTALL_DIR="${1:-/usr/local/bin}"

do_install() {
    local dst="$1"
    install -m 755 target/release/pelagos           "${dst}/pelagos"
    install -m 755 target/release/pelagos-dns       "${dst}/pelagos-dns"
    # Install the shim under both its Cargo name and the containerd-expected name.
    install -m 755 target/release/pelagos-shim-wasm "${dst}/pelagos-shim-wasm"
    install -m 755 target/release/pelagos-shim-wasm "${dst}/containerd-shim-pelagos-wasm-v1"
}

# If we're root via sudo (not a true root session like CI), the user's
# rustup/cargo may not be on root's PATH. Build as the invoking user.
if [ "$(id -u)" -eq 0 ] && [ -n "${SUDO_USER:-}" ]; then
    echo "Building pelagos (release) as ${SUDO_USER}..."
    sudo -u "$SUDO_USER" cargo build --release
    echo "Installing binaries to ${INSTALL_DIR}..."
    do_install "${INSTALL_DIR}"
elif [ "$(id -u)" -eq 0 ]; then
    # True root (CI, container, etc.) — just build and install directly.
    echo "Building pelagos (release)..."
    cargo build --release
    echo "Installing binaries to ${INSTALL_DIR}..."
    do_install "${INSTALL_DIR}"
else
    # Normal user — build, then sudo for the install step.
    echo "Building pelagos (release)..."
    cargo build --release
    echo "Installing binaries to ${INSTALL_DIR} (may prompt for sudo)..."
    sudo bash -c "$(declare -f do_install); do_install '${INSTALL_DIR}'"
fi

echo "Done. $(pelagos --version 2>/dev/null || echo 'pelagos installed')"
