#!/usr/bin/env bash
# Healthcheck end-to-end smoke test.
#
# Builds an image with a HEALTHCHECK instruction, runs it detached, and
# polls until the container reaches 'healthy' state (or fails after 15s).
#
# Run with: sudo -E bash scripts/test-healthcheck.sh

set -euo pipefail

cd "$(dirname "$0")/.."

REMORA="./target/debug/remora"
NAME="hc-smoke-test"
IMAGE="hc-test:latest"
CONTEXT="scripts/hc-test-context"
STATE_PATH="/run/remora/containers/${NAME}/state.json"

cleanup() {
    echo ""
    echo "--- Cleanup ---"
    $REMORA stop "$NAME" 2>/dev/null || true
    sleep 0.5
    $REMORA rm -f "$NAME" 2>/dev/null || true
    $REMORA image rm "$IMAGE" 2>/dev/null || true
    echo "Done."
}
trap cleanup EXIT

echo "=== Step 1: Pull base image ==="
$REMORA image pull alpine
echo ""

echo "=== Step 2: Build image with HEALTHCHECK ==="
$REMORA build -t "$IMAGE" "$CONTEXT"
echo ""

echo "=== Step 3: Run detached ==="
$REMORA run -d --name "$NAME" "$IMAGE"
echo ""

echo "=== Step 4: Wait for container pid > 0 ==="
for i in $(seq 1 50); do
    pid=$(python3 -c "import json; d=json.load(open('${STATE_PATH}')); print(d.get('pid',0))" 2>/dev/null || echo 0)
    if [ "$pid" -gt 0 ]; then
        echo "Container started (pid=$pid)"
        break
    fi
    sleep 0.1
done
echo ""

echo "=== Step 5: Poll for 'healthy' (up to 15s) ==="
echo "(expecting: starting -> healthy)"
echo ""
START=$SECONDS
while [ $((SECONDS - START)) -lt 15 ]; do
    health=$(python3 -c "import json; d=json.load(open('${STATE_PATH}')); print(d.get('health','none'))" 2>/dev/null || echo "?")
    $REMORA ps
    printf "  health in state.json: %s\n\n" "$health"
    if [ "$health" = "healthy" ]; then
        echo "SUCCESS: container reached 'healthy'."
        exit 0
    fi
    sleep 1
done

echo "FAILURE: did not reach 'healthy' within 15s. Final state.json:"
cat "$STATE_PATH"
exit 1
