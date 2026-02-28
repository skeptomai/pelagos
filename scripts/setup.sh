#!/usr/bin/env bash
#
# remora system setup — creates the remora group, initialises /var/lib/remora/,
# and optionally adds a user to the remora group.
#
# Run this once after installation (or from a package manager postinst hook).
# The script is idempotent: safe to run multiple times.
#
# Usage:
#   sudo ./scripts/setup.sh               # auto-adds SUDO_USER to remora group
#   sudo ./scripts/setup.sh --add-user cb # adds a specific user
#   sudo ./scripts/setup.sh --no-user     # skip user addition entirely
#
# After running, users in the 'remora' group can pull images without sudo:
#   remora image pull alpine
#
# Container operations (run, compose) still require root because they use
# Linux namespaces, mounts, and network configuration.

set -euo pipefail

# ── Argument parsing ────────────────────────────────────────────────────────

ADD_USER=""
NO_USER=false

while [[ $# -gt 0 ]]; do
    case "$1" in
        --add-user)
            ADD_USER="${2:?'--add-user requires a username'}"
            shift 2
            ;;
        --no-user)
            NO_USER=true
            shift
            ;;
        *)
            echo "usage: $0 [--add-user USERNAME] [--no-user]" >&2
            exit 1
            ;;
    esac
done

# ── Root check ──────────────────────────────────────────────────────────────

if [[ "$(id -u)" -ne 0 ]]; then
    echo "error: this script must be run as root (use sudo)" >&2
    exit 1
fi

# ── Helpers ─────────────────────────────────────────────────────────────────

ok()   { echo "  [ok]  $*"; }
done_() { echo "  [--]  $* (already done)"; }
info() { echo "==> $*"; }

# ── Create remora system group ───────────────────────────────────────────────

info "Checking remora group..."
if getent group remora > /dev/null 2>&1; then
    done_ "group 'remora' already exists"
else
    groupadd --system remora
    ok "created system group 'remora'"
fi

# ── Create /var/lib/remora/ ──────────────────────────────────────────────────

info "Setting up /var/lib/remora/..."

# Root directory: root:remora 0755 (root owns, group can enter)
mkdir -p /var/lib/remora
chown root:remora /var/lib/remora
chmod 0755 /var/lib/remora
ok "/var/lib/remora (root:remora 0755)"

# Image store subdirs: root:remora 2775 (setgid + group-writable)
# These are written by image pull and build — group members can write.
# Content-addressed (sha256 digest as directory name) so group-write is safe.
# The setgid bit (2xxx) ensures that subdirectories created by root also
# inherit the 'remora' group, so group members can write into them.
# We also recursively chown any existing subdirs that were created by a
# previous root pull before the setgid bit was in place.
for subdir in images layers build-cache; do
    mkdir -p "/var/lib/remora/$subdir"
    chown -R root:remora "/var/lib/remora/$subdir"
    chmod -R g+rwX "/var/lib/remora/$subdir"
    chmod g+s "/var/lib/remora/$subdir"
    ok "/var/lib/remora/$subdir (root:remora 2775, setgid, existing subdirs repaired)"
done

# Runtime subdirs: root:root 0755
# These require root (mounts, network config, container state).
for subdir in volumes networks rootfs; do
    mkdir -p "/var/lib/remora/$subdir"
    chown root:root "/var/lib/remora/$subdir"
    chmod 0755 "/var/lib/remora/$subdir"
    ok "/var/lib/remora/$subdir (root:root 0755)"
done

# ── Add user to remora group ─────────────────────────────────────────────────

if $NO_USER; then
    info "Skipping user addition (--no-user)."
else
    # Determine which user to add.
    if [[ -z "$ADD_USER" ]]; then
        # Default: the user who invoked sudo, if any.
        ADD_USER="${SUDO_USER:-}"
    fi

    if [[ -z "$ADD_USER" ]]; then
        info "No user to add (run as root directly, not via sudo)."
        echo "      To add a user later: sudo usermod -aG remora <username>"
    else
        info "Adding '$ADD_USER' to the remora group..."
        if id -nG "$ADD_USER" | tr ' ' '\n' | grep -q '^remora$'; then
            done_ "'$ADD_USER' is already in the remora group"
        else
            usermod -aG remora "$ADD_USER"
            ok "added '$ADD_USER' to group 'remora'"
            echo ""
            echo "  NOTE: '$ADD_USER' must log out and back in (or run 'newgrp remora')"
            echo "        for group membership to take effect."
        fi
    fi
fi

# ── Done ─────────────────────────────────────────────────────────────────────

echo ""
echo "Setup complete. Users in the 'remora' group can pull images without sudo:"
echo "  remora image pull alpine"
echo ""
echo "Container operations still require root:"
echo "  sudo remora run alpine /bin/sh"
