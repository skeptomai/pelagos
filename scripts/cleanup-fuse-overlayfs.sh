#!/bin/bash
# Clean up orphaned fuse-overlayfs mounts and processes from failed rootless containers.

set -euo pipefail

echo "=== fuse-overlayfs mounts ==="
mounts=$(grep -c 'fuse-overlayfs' /proc/self/mountinfo 2>/dev/null || true)
if [ "$mounts" -gt 0 ]; then
    grep 'fuse-overlayfs' /proc/self/mountinfo | while read -r line; do
        mountpoint=$(echo "$line" | awk '{print $5}')
        echo "  unmounting: $mountpoint"
        fusermount3 -u "$mountpoint" 2>/dev/null \
            || fusermount -u "$mountpoint" 2>/dev/null \
            || echo "    FAILED — try: fusermount3 -u $mountpoint"
    done
else
    echo "  none found"
fi

echo ""
echo "=== fuse-overlayfs processes ==="
procs=$(pgrep -u "$(id -u)" fuse-overlayfs 2>/dev/null || true)
if [ -n "$procs" ]; then
    ps -p "$procs" -o pid,args 2>/dev/null || true
    echo ""
    read -rp "Kill these processes? [y/N] " confirm
    if [[ "$confirm" =~ ^[Yy]$ ]]; then
        echo "$procs" | xargs kill
        echo "  killed"
    fi
else
    echo "  none found"
fi

echo ""
echo "=== orphaned overlay dirs ==="
runtime_dir="${XDG_RUNTIME_DIR:-/tmp/remora-$(id -u)}/remora"
for base in "$runtime_dir" /run/remora; do
    if [ -d "$base" ]; then
        found=0
        for d in "$base"/overlay-*; do
            [ -d "$d" ] || continue
            echo "  $d"
            found=1
        done
        if [ "$found" -eq 1 ]; then
            read -rp "Remove these directories? [y/N] " confirm
            if [[ "$confirm" =~ ^[Yy]$ ]]; then
                rm -rf "$base"/overlay-*
                echo "  removed"
            fi
        fi
    fi
done

echo ""
echo "done"
