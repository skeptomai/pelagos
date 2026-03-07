#!/usr/bin/env bash
#
# Clean up all pelagos runtime state.  Run with: sudo scripts/reset-test-env.sh
#
set -euo pipefail

echo "=== Killing any remaining pelagos containers ==="
if [ -d /run/pelagos/containers ]; then
    for state in /run/pelagos/containers/*/state.json; do
        [ -f "$state" ] || continue
        pid=$(grep -o '"pid":[0-9]*' "$state" | head -1 | cut -d: -f2)
        watcher=$(grep -o '"watcher_pid":[0-9]*' "$state" | head -1 | cut -d: -f2)
        # Send SIGTERM first so the watcher can run teardown (veth/netns cleanup).
        [ -n "$watcher" ] && [ "$watcher" != "0" ] && kill -TERM "$watcher" 2>/dev/null && echo "  SIGTERM'd watcher $watcher" || true
        [ -n "$pid" ] && [ "$pid" != "0" ] && kill -TERM "$pid" 2>/dev/null && echo "  SIGTERM'd container $pid" || true
    done
    # Give watchers up to 3 seconds to clean up before resorting to SIGKILL.
    sleep 3
    for state in /run/pelagos/containers/*/state.json; do
        [ -f "$state" ] || continue
        pid=$(grep -o '"pid":[0-9]*' "$state" | head -1 | cut -d: -f2)
        watcher=$(grep -o '"watcher_pid":[0-9]*' "$state" | head -1 | cut -d: -f2)
        [ -n "$pid" ] && [ "$pid" != "0" ] && kill -9 "$pid" 2>/dev/null && echo "  killed container pid $pid" || true
        [ -n "$watcher" ] && [ "$watcher" != "0" ] && kill -9 "$watcher" 2>/dev/null && echo "  killed watcher pid $watcher" || true
    done
fi

echo "=== Cleaning up orphaned veth interfaces ==="
for iface in $(ip link show | awk -F': ' '/^[0-9]+: (vh|vp)-/{print $2}' | cut -d'@' -f1); do
    ip link del "$iface" 2>/dev/null && echo "  deleted $iface" || true
done

echo "=== Removing pelagos bridge ==="
ip link del pelagos0 2>/dev/null && echo "  deleted pelagos0" || echo "  (no pelagos0)"

echo "=== Cleaning up network namespaces ==="
for ns in $(ip netns list 2>/dev/null | awk '{print $1}' | grep '^rem-'); do
    ip netns del "$ns" 2>/dev/null && echo "  deleted netns $ns" || true
done

echo "=== Removing nftables pelagos tables ==="
nft list tables 2>/dev/null | awk '/pelagos/{print $2, $3}' | while read -r family table; do
    nft delete table "$family" "$table" 2>/dev/null && echo "  deleted nft table $family $table" || true
done

echo "=== Flushing iptables pelagos rules ==="
iptables -D FORWARD -s 172.19.0.0/24 -j ACCEPT 2>/dev/null && echo "  deleted FORWARD src rule" || true
iptables -D FORWARD -d 172.19.0.0/24 -j ACCEPT 2>/dev/null && echo "  deleted FORWARD dst rule" || true
iptables -t nat -D POSTROUTING -s 172.19.0.0/24 ! -o pelagos0 -j MASQUERADE 2>/dev/null && echo "  deleted MASQUERADE rule" || true
# Remove iptables DNS INPUT rules for all bridge interfaces.
for iface in $(iptables -L INPUT -n 2>/dev/null | awk '/dpt:53/{print $7}' | sed 's/in=//'); do
    iptables -D INPUT -i "$iface" -p udp --dport 53 -j ACCEPT 2>/dev/null || true
done

echo "=== Stopping pelagos-dns daemon ==="
pkill -x pelagos-dns 2>/dev/null && echo "  stopped pelagos-dns" || echo "  (not running)"

echo "=== Removing /run/pelagos ==="
rm -rf /run/pelagos && echo "  done" || true

echo "=== Unmounting any stale overlays ==="
grep -o '/run/pelagos/overlay-[^ ]*' /proc/mounts 2>/dev/null | while read -r mnt; do
    umount -l "$mnt" 2>/dev/null && echo "  unmounted $mnt" || true
done || true

echo "=== Clean ==="
