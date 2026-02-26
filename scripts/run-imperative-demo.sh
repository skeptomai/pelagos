#!/usr/bin/env bash
#
# run-imperative-demo.sh — pull images and run the graph model compose example
#
# Demonstrates the declarative future graph with topology-aware cascade teardown:
#   - db and cache start in parallel (tier 1)
#   - db-url and cache-url are computed (tier 2)
#   - migrate runs and exits (tier 3)
#   - app starts with both URLs injected (tier 4)
#   - container-wait cascades SIGTERM through migrate, cache, db automatically
#
# Usage:
#   sudo -E ./scripts/run-imperative-demo.sh

set -euo pipefail
cd "$(dirname "$0")/.."

if [ "$EUID" -ne 0 ]; then
    echo "error: this script must be run as root (sudo -E ./scripts/run-imperative-demo.sh)" >&2
    exit 1
fi

REMORA="${REMORA:-./target/debug/remora}"
COMPOSE="examples/compose/imperative/compose.reml"
PROJECT="imperative-demo"

if [ ! -f "$REMORA" ]; then
    echo "==> Building remora..."
    cargo build --bin remora
fi

echo "==> Pulling images..."
for image in postgres:16 redis:7-alpine alpine:latest; do
    if "$REMORA" image ls 2>/dev/null | grep -q "^${image}"; then
        echo "    $image already present"
    else
        echo "    pulling $image..."
        "$REMORA" image pull "$image"
    fi
done

echo
echo "==> Running: $COMPOSE"
echo
RUST_LOG=info "$REMORA" compose up -f "$COMPOSE" -p "$PROJECT"
