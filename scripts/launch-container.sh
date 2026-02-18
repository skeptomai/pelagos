#!/bin/bash
# Launch remora container with proper environment setup

set -e
cd "$(dirname "$0")/.." || exit 1

echo "==> Preparing container environment..."

# Create init script in rootfs if it doesn't exist
if [ ! -f alpine-rootfs/init.sh ]; then
    echo "==> Creating init.sh in rootfs..."
    sudo tee alpine-rootfs/init.sh > /dev/null <<'EOF'
#!/bin/ash
# Container init script - sets up environment and launches shell

# Source profile to set PATH and other environment variables
if [ -f /etc/profile ]; then
    . /etc/profile
fi

# Set minimal PATH if profile didn't set it
export PATH="${PATH:-/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin}"

echo "Container initialized. PATH=$PATH"
echo "Available commands: ls, ps, cat, grep, etc."
echo "Type 'exit' to leave the container."
echo ""

# Launch interactive shell
exec /bin/ash
EOF
    sudo chmod +x alpine-rootfs/init.sh
    echo "✓ init.sh created"
fi

# Set up environment for remora
export RUST_LOG=info
export RUST_BACKTRACE=full

echo "==> Launching container..."
echo ""

# Launch remora with init script
sudo -E ./target/debug/remora \
    --exe /init.sh \
    --rootfs ./alpine-rootfs \
    --uid 1000 \
    --gid 1000

echo ""
echo "==> Container exited"
