#!/usr/bin/env bash
#
# Clean up all remora runtime state.  Run with: sudo scripts/reset-test-env.sh
#
set -euo pipefail

echo "=== Killing any remaining remora containers ==="
if [ -d /run/remora/containers ]; then
    for state in /run/remora/containers/*/state.json; do
        [ -f "$state" ] || continue
        pid=$(grep -o '"pid":[0-9]*' "$state" | head -1 | cut -d: -f2)
        watcher=$(grep -o '"watcher_pid":[0-9]*' "$state" | head -1 | cut -d: -f2)
        [ -n "$pid" ] && [ "$pid" != "0" ] && kill -9 "$pid" 2>/dev/null && echo "  killed container pid $pid" || true
        [ -n "$watcher" ] && [ "$watcher" != "0" ] && kill -9 "$watcher" 2>/dev/null && echo "  killed watcher pid $watcher" || true
    done
fi

echo "=== Removing remora bridge ==="
ip link del remora0 2>/dev/null && echo "  deleted remora0" || echo "  (no remora0)"

echo "=== Cleaning up network namespaces ==="
for ns in $(ip netns list 2>/dev/null | awk '{print $1}' | grep '^rem-'); do
    ip netns del "$ns" 2>/dev/null && echo "  deleted netns $ns" || true
done

echo "=== Removing nftables rules ==="
nft delete table inet remora 2>/dev/null && echo "  deleted nft table" || echo "  (no nft table)"
nft delete table ip remora 2>/dev/null && echo "  deleted nft ip table" || true

echo "=== Flushing iptables remora rules ==="
iptables -D FORWARD -s 172.19.0.0/24 -j ACCEPT 2>/dev/null && echo "  deleted FORWARD src rule" || true
iptables -D FORWARD -d 172.19.0.0/24 -j ACCEPT 2>/dev/null && echo "  deleted FORWARD dst rule" || true
iptables -t nat -D POSTROUTING -s 172.19.0.0/24 ! -o remora0 -j MASQUERADE 2>/dev/null && echo "  deleted MASQUERADE rule" || true

echo "=== Removing /run/remora ==="
rm -rf /run/remora && echo "  done" || true

echo "=== Unmounting any stale overlays ==="
grep -o '/run/remora/overlay-[^ ]*' /proc/mounts 2>/dev/null | while read -r mnt; do
    umount -l "$mnt" 2>/dev/null && echo "  unmounted $mnt" || true
done

echo "=== Clean ==="
