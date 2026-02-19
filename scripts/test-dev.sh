#!/usr/bin/env bash
# E2E tests for Phase D: minimal /dev setup.
#
# Section A — Root tests (run with sudo):
#   sudo scripts/test-dev.sh
#
# Section B — Rootless tests (no sudo):
#   scripts/test-dev.sh --rootless
set -euo pipefail

PASS=0
FAIL=0
BINARY="./target/debug/remora"

pass() { PASS=$((PASS+1)); echo "  PASS: $1"; }
fail() { FAIL=$((FAIL+1)); echo "  FAIL: $1"; }

check_contains() {
    local output="$1" expected="$2" label="$3"
    if echo "$output" | grep -q "$expected"; then
        pass "$label"
    else
        fail "$label (expected '$expected' in output)"
        echo "    output: $output"
    fi
}

check_not_contains() {
    local output="$1" unwanted="$2" label="$3"
    if echo "$output" | grep -q "$unwanted"; then
        fail "$label (found unwanted '$unwanted' in output)"
        echo "    output: $output"
    else
        pass "$label"
    fi
}

# Build first.
echo "==> Building remora..."
cargo build 2>&1

if [ "${1:-}" = "--rootless" ]; then
    echo ""
    echo "=== Section B: Rootless /dev tests ==="
    echo ""

    # Ensure alpine image is available.
    if ! $BINARY image ls 2>/dev/null | grep -q alpine; then
        echo "Pulling alpine image (rootless)..."
        $BINARY image pull alpine
    fi

    echo "--- Test: ls /dev/ shows minimal set ---"
    OUT=$($BINARY run alpine /bin/ls /dev/ 2>/dev/null || true)
    check_contains "$OUT" "null" "rootless /dev/null present"
    check_contains "$OUT" "zero" "rootless /dev/zero present"
    check_contains "$OUT" "random" "rootless /dev/random present"
    check_contains "$OUT" "urandom" "rootless /dev/urandom present"
    check_not_contains "$OUT" "sda" "rootless no /dev/sda"

    echo "--- Test: write to /dev/null ---"
    OUT=$($BINARY run alpine /bin/sh -c 'echo ok > /dev/null && echo pass' 2>/dev/null || true)
    check_contains "$OUT" "pass" "rootless /dev/null write"

    echo "--- Test: read /dev/zero ---"
    OUT=$($BINARY run alpine /bin/sh -c 'head -c 4 /dev/zero | wc -c' 2>/dev/null || true)
    check_contains "$OUT" "4" "rootless /dev/zero read"

    echo "--- Test: /dev symlinks ---"
    OUT=$($BINARY run alpine /bin/sh -c 'test -L /dev/fd && test -L /dev/stdin && echo ok' 2>/dev/null || true)
    check_contains "$OUT" "ok" "rootless /dev symlinks"

else
    if [ "$(id -u)" -ne 0 ]; then
        echo "Section A requires root. Run: sudo scripts/test-dev.sh"
        echo "For rootless tests: scripts/test-dev.sh --rootless"
        exit 1
    fi

    echo ""
    echo "=== Section A: Root /dev tests ==="
    echo ""

    # Ensure alpine image is available.
    if ! $BINARY image ls 2>/dev/null | grep -q alpine; then
        echo "Pulling alpine image..."
        $BINARY image pull alpine
    fi

    echo "--- Test: ls /dev/ shows minimal set ---"
    OUT=$($BINARY run alpine /bin/ls /dev/ 2>/dev/null)
    check_contains "$OUT" "null" "/dev/null present"
    check_contains "$OUT" "zero" "/dev/zero present"
    check_contains "$OUT" "full" "/dev/full present"
    check_contains "$OUT" "random" "/dev/random present"
    check_contains "$OUT" "urandom" "/dev/urandom present"
    check_contains "$OUT" "tty" "/dev/tty present"
    check_contains "$OUT" "pts" "/dev/pts present"
    check_contains "$OUT" "shm" "/dev/shm present"
    check_not_contains "$OUT" "sda" "no /dev/sda"
    check_not_contains "$OUT" "nvme" "no /dev/nvme"

    echo "--- Test: write to /dev/null ---"
    OUT=$($BINARY run alpine /bin/sh -c 'echo ok > /dev/null && echo pass' 2>/dev/null)
    check_contains "$OUT" "pass" "/dev/null write works"

    echo "--- Test: read /dev/zero ---"
    OUT=$($BINARY run alpine /bin/sh -c 'head -c 4 /dev/zero | wc -c' 2>/dev/null)
    check_contains "$OUT" "4" "/dev/zero read works"

    echo "--- Test: /dev symlinks ---"
    OUT=$($BINARY run alpine /bin/sh -c 'test -L /dev/fd && test -L /dev/stdin && test -L /dev/stdout && test -L /dev/stderr && echo ok' 2>/dev/null)
    check_contains "$OUT" "ok" "/dev symlinks present"

    echo ""
    echo "--- Running integration tests (dev module) ---"
    cargo test --test integration_tests dev -- --test-threads=1 2>&1 || FAIL=$((FAIL+1))
fi

echo ""
echo "=== Results: $PASS passed, $FAIL failed ==="
[ "$FAIL" -eq 0 ] && exit 0 || exit 1
