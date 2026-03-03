#!/usr/bin/env bash
#
# Test script for `pelagos exec`.  Run with:  sudo -E ./test-exec.sh
#
# Cleans up containers on exit regardless of success or failure.

cd "$(dirname "$0")/.." || exit 1

CARGO="cargo"
PELAGOS="$CARGO run --"
CONTAINER="test-exec"
RC=0

cleanup() {
    echo ""
    echo "=== Cleanup ==="
    $PELAGOS stop "$CONTAINER" 2>/dev/null || true
    sleep 1
    $PELAGOS rm -f "$CONTAINER" 2>/dev/null || true
}
trap cleanup EXIT

set -euo pipefail

echo "=== Building ==="
$CARGO build

echo ""
echo "=== Starting detached container ==="
$PELAGOS run --name "$CONTAINER" --detach alpine-rootfs /bin/sleep 300

sleep 1

echo ""
echo "=== Exec: echo hello ==="
$PELAGOS exec "$CONTAINER" /bin/sh -c "echo hello from exec"

echo ""
echo "=== Exec: read /etc/hostname ==="
$PELAGOS exec "$CONTAINER" /bin/cat /etc/hostname

echo ""
echo "=== Exec: env override ==="
$PELAGOS exec -e FOO=bar "$CONTAINER" /bin/sh -c 'echo FOO=$FOO'

echo ""
echo "=== Exec: ps inside container ==="
$PELAGOS exec "$CONTAINER" /bin/ps aux

# Cleanup the manual-test container before running integration tests
# so they don't interfere with each other.
cleanup
trap - EXIT

echo ""
echo "=== Running integration tests ==="
$CARGO test --test integration_tests exec -- --test-threads=1 || RC=$?

if [ $RC -eq 0 ]; then
    echo ""
    echo "=== All exec tests passed ==="
else
    echo ""
    echo "=== Tests failed (exit $RC) ==="
    exit $RC
fi
