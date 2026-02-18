#!/bin/bash
# Test namespace joining with setns()

set -e
cd "$(dirname "$0")/.." || exit 1

echo "==> Testing namespace joining with setns()"
echo ""

# Check if network namespace 'con' exists
if ! sudo ip netns list | grep -q "^con"; then
    echo "Network namespace 'con' not found. Creating it..."
    echo "Running setup.sh..."
    sudo ./setup.sh
    echo ""
fi

echo "==> Network namespace 'con' exists"
echo ""

# Build latest version
echo "==> Building latest code..."
cargo build --quiet
echo ""

echo "==> Testing namespace joining..."
echo ""
echo "This test will:"
echo "  1. Join the 'con' network namespace"
echo "  2. Run 'ip addr' inside the container"
echo "  3. Verify we see the veth2 interface from the 'con' namespace"
echo ""

echo "==> Current interfaces in 'con' namespace (for reference):"
sudo ip netns exec con ip addr show | grep -E "^[0-9]+:|inet " | head -10
echo ""

echo "==> Launching container with network namespace joining..."
echo ""

# Test: join network namespace and check interface
sudo -E ./target/debug/remora \
    --exe /init.sh \
    --rootfs ./alpine-rootfs \
    --uid 1000 \
    --gid 1000 \
    --join-netns con << 'CONTAINER_EOF'
echo ""
echo "==> Inside container, checking network interfaces:"
ip addr show
echo ""
echo "==> Checking for veth2 interface:"
if ip addr show | grep -q "veth2"; then
    echo "✅ SUCCESS! veth2 interface found in container"
    ip addr show veth2
else
    echo "❌ FAILURE! veth2 interface NOT found"
    echo "This means namespace joining did not work correctly."
fi
echo ""
echo "==> Checking for expected IP address (172.16.0.1):"
if ip addr show | grep -q "172.16.0.1"; then
    echo "✅ SUCCESS! IP address 172.16.0.1 found"
else
    echo "❌ FAILURE! Expected IP 172.16.0.1 not found"
fi
exit
CONTAINER_EOF

echo ""
echo "==> Test complete!"
echo ""
echo "Expected results:"
echo "  ✅ veth2 interface should be visible in container"
echo "  ✅ IP 172.16.0.1 should be assigned to veth2"
echo "  ✅ lo (loopback) interface should be present"
