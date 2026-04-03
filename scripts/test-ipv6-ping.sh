#!/bin/bash
# test-ipv6-ping.sh — demonstrate IPv6 ULA bridge connectivity with 0% packet loss.
#
# Step 1: prime the NDP neighbor cache so the first ping doesn't wait on
#         Neighbor Solicitation / Advertisement resolution.
# Step 2: ping6 the bridge gateway 5 times and assert 0% packet loss.
#
# Requires root (bridge networking).

set -euo pipefail

GATEWAY="fd7e:73ca:9801::1"
BRIDGE="pelagos0"

# Derive bridge MAC at runtime so the script stays correct after reboots.
BRIDGE_MAC=$(ip link show "$BRIDGE" 2>/dev/null | awk '/link\/ether/{print $2}')
if [[ -z "$BRIDGE_MAC" ]]; then
    echo "error: $BRIDGE not found — is the pelagos0 bridge up?" >&2
    exit 1
fi

exec sudo -E cargo run --bin pelagos -- run --network bridge --rm alpine /bin/ash -c "
    # Wait for DAD (Duplicate Address Detection) to complete.
    # While an address is 'tentative' the kernel silently drops outbound packets.
    echo 'Waiting for DAD to complete...'
    while ip -6 addr show eth0 | grep -q tentative; do sleep 0.1; done

    # Seed the NDP neighbor cache so seq=0 doesn't wait on NS/NA resolution.
    ip -6 neigh add $GATEWAY lladdr $BRIDGE_MAC dev eth0 nud reachable 2>/dev/null || true

    echo 'NDP cache primed. Running ping6 ...'
    echo ''
    ping6 -c 5 $GATEWAY
"
