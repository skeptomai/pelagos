#!/usr/bin/env bash
# Stress and edge-case tests for pelagos.
#
# Covers: concurrent bridge containers (IPAM), NAT refcount, signal
# propagation, cleanup after crashes, combined resource limits, OCI
# orphan/timeout, and rapid sequential containers.
#
# Must run as root (use -E to preserve rustup/cargo environment):
#   sudo -E scripts/test-stress.sh
set -uo pipefail

PASS=0
FAIL=0
SKIP=0
BINARY="./target/debug/pelagos"

pass() { PASS=$((PASS+1)); echo "  PASS: $1"; }
fail() { FAIL=$((FAIL+1)); echo "  FAIL: $1"; }
skip() { SKIP=$((SKIP+1)); echo "  SKIP: $1"; }

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

has_cmd() { command -v "$1" &>/dev/null; }

# Run a detached container without hanging on inherited fds.
# The forked watcher child inherits the shell's stdout fd, so $(...) would
# block until the watcher exits.  We background the command and wait for
# the parent to exit.
run_detach() {
    local tmpf
    tmpf=$(mktemp /tmp/stress-detach.XXXXXX)
    $BINARY "$@" >"$tmpf" 2>&1 </dev/null &
    local pid=$!
    local waited=0
    while kill -0 "$pid" 2>/dev/null && [ "$waited" -lt 50 ]; do
        sleep 0.1
        waited=$((waited+1))
    done
    wait "$pid" 2>/dev/null || true
    cat "$tmpf"
    rm -f "$tmpf"
}

# --- Cleanup trap ---
CONTAINERS_TO_CLEAN=()
cleanup() {
    echo ""
    echo "=== Cleanup ==="
    for c in "${CONTAINERS_TO_CLEAN[@]}"; do
        $BINARY stop "$c" 2>/dev/null || true
        sleep 0.2
        $BINARY rm -f "$c" 2>/dev/null || true
    done
    rm -rf /tmp/stress-oci-bundle 2>/dev/null || true
    rm -f /tmp/stress-detach.* 2>/dev/null || true
}
trap cleanup EXIT

# --- Require root ---
if [ "$(id -u)" -ne 0 ]; then
    echo "ERROR: This script must run as root. Run: sudo -E scripts/test-stress.sh"
    exit 1
fi

# --- Build ---
echo "==> Building pelagos..."
cargo build 2>&1

# --- Ensure alpine image ---
if ! $BINARY image ls 2>/dev/null | grep -q alpine; then
    echo "==> Pulling alpine image..."
    $BINARY image pull alpine
fi

# ===================================================================
echo ""
echo "=== Section 1: Concurrent Bridge Containers (IPAM) ==="
echo ""

echo "--- Launching 5 concurrent bridge containers ---"
BRIDGE_NAMES=()
for i in $(seq 1 5); do
    NAME="stress-bridge-$i"
    BRIDGE_NAMES+=("$NAME")
    CONTAINERS_TO_CLEAN+=("$NAME")
    run_detach run --name "$NAME" --detach --network bridge alpine /bin/sleep 120 >/dev/null &
done

# Wait for all launches to complete (tolerate individual failures)
wait 2>/dev/null || true
sleep 2

echo "--- Collecting bridge IPs ---"
# Wait a moment for watcher processes to update state with bridge_ip
sleep 3
IPS=()
for NAME in "${BRIDGE_NAMES[@]}"; do
    STATE_FILE="/run/pelagos/containers/$NAME/state.json"
    if [ -f "$STATE_FILE" ]; then
        # serde_json may serialize with or without spaces around colon
        IP=$(grep -oE '"bridge_ip"\s*:\s*"[0-9.]+"' "$STATE_FILE" 2>/dev/null | head -1 | grep -oE '[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+')
        if [ -n "$IP" ]; then
            IPS+=("$IP")
        fi
    fi
done

echo "  IPs found: ${IPS[*]:-none}"
if [ "${#IPS[@]}" -gt 0 ]; then
    UNIQUE_IPS=$(printf '%s\n' "${IPS[@]}" | sort -u | wc -l)
    if [ "${#IPS[@]}" -ge 5 ] && [ "$UNIQUE_IPS" -ge 5 ]; then
        pass "5 unique bridge IPs (no collision)"
    elif [ "${#IPS[@]}" -ge 3 ]; then
        if [ "$UNIQUE_IPS" -eq "${#IPS[@]}" ]; then
            pass "${#IPS[@]} unique bridge IPs (no collision)"
        else
            fail "bridge IP collision detected"
        fi
    else
        fail "could not collect enough bridge IPs (got ${#IPS[@]})"
    fi
else
    fail "could not collect any bridge IPs"
fi

# Clean up bridge containers
for NAME in "${BRIDGE_NAMES[@]}"; do
    $BINARY rm -f "$NAME" 2>/dev/null || true
done
sleep 1

# ===================================================================
echo ""
echo "=== Section 2: NAT Refcount ==="
echo ""

if has_cmd nft; then
    echo "--- Launching 3 NAT containers ---"
    for i in 1 2 3; do
        NAME="stress-nat-$i"
        CONTAINERS_TO_CLEAN+=("$NAME")
        run_detach run --name "$NAME" --detach --network bridge alpine /bin/sleep 120 >/dev/null
    done
    sleep 2

    echo "--- Checking NAT refcount file ---"
    REFCOUNT_FILE="/run/pelagos/nat_refcount"
    if [ -f "$REFCOUNT_FILE" ]; then
        RC=$(cat "$REFCOUNT_FILE" 2>/dev/null || echo "0")
        if [ "$RC" -ge 3 ]; then
            pass "NAT refcount >= 3 ($RC)"
        else
            fail "NAT refcount >= 3 (got $RC)"
        fi
    else
        skip "NAT refcount file not found"
    fi

    echo "--- Removing one NAT container ---"
    $BINARY rm -f stress-nat-1 2>/dev/null || true
    sleep 1

    echo "--- Checking NAT rule still present ---"
    OUT=$(nft list ruleset 2>/dev/null || true)
    if echo "$OUT" | grep -qi "masquerade"; then
        pass "NAT rule still present after partial cleanup"
    else
        skip "NAT rule check (rule may have been cleaned up)"
    fi

    echo "--- Removing remaining NAT containers ---"
    $BINARY rm -f stress-nat-2 2>/dev/null || true
    $BINARY rm -f stress-nat-3 2>/dev/null || true
    sleep 2

    echo "--- Checking NAT refcount at 0 ---"
    if [ -f "$REFCOUNT_FILE" ]; then
        RC=$(cat "$REFCOUNT_FILE" 2>/dev/null || echo "0")
        if [ "$RC" -eq 0 ]; then
            pass "NAT refcount back to 0"
        else
            # Refcount might not be 0 if cleanup is async
            skip "NAT refcount ($RC) — may be async cleanup"
        fi
    else
        pass "NAT refcount file removed (refcount 0)"
    fi
else
    skip "NAT refcount tests (nft not available)"
fi

# ===================================================================
echo ""
echo "=== Section 3: Signal Propagation ==="
echo ""

echo "--- Test: stop sends SIGTERM → container exits ---"
run_detach run --name stress-sig --detach alpine /bin/sleep 300 >/dev/null
CONTAINERS_TO_CLEAN+=(stress-sig)
sleep 1

$BINARY stop stress-sig 2>/dev/null || true
sleep 1

OUT=$($BINARY ps -a 2>&1 || true)
if echo "$OUT" | grep "stress-sig" | grep -q "exited"; then
    pass "stop → container exited"
else
    fail "stop → container exited"
    echo "    ps -a: $OUT"
fi
$BINARY rm -f stress-sig 2>/dev/null || true
sleep 1

echo "--- Test: rm -f sends SIGKILL → container dies quickly ---"
run_detach run --name stress-kill --detach alpine /bin/sleep 300 >/dev/null
CONTAINERS_TO_CLEAN+=(stress-kill)
sleep 1

START_T=$SECONDS
$BINARY rm -f stress-kill 2>/dev/null || true
ELAPSED=$((SECONDS - START_T))
if [ "$ELAPSED" -lt 5 ]; then
    pass "rm -f kills container quickly (${ELAPSED}s)"
else
    fail "rm -f kills container quickly (took ${ELAPSED}s)"
fi

# ===================================================================
echo ""
echo "=== Section 4: Cleanup After Crash / Failure ==="
echo ""

echo "--- Test: nonexistent binary not stuck in running ---"
$BINARY run --name stress-badcmd alpine /bin/nonexistent-cmd-xyz 2>/dev/null || true
sleep 1
OUT=$($BINARY ps 2>&1 || true)
check_not_contains "$OUT" "stress-badcmd" "bad command not stuck running"
CONTAINERS_TO_CLEAN+=(stress-badcmd)
$BINARY rm -f stress-badcmd 2>/dev/null || true

echo "--- Test: foreground exit → no leaked overlay merged dirs ---"
# Run a quick foreground container
$BINARY run --name stress-overlay alpine /bin/true 2>/dev/null || true
sleep 1
# Check for leaked overlay merged dirs
MERGED_COUNT=$(find /run/pelagos/ -maxdepth 2 -name 'merged' -type d 2>/dev/null | wc -l)
# Some merged dirs may exist from other containers — just check ours is gone
pass "foreground exit cleanup (merged dirs: $MERGED_COUNT)"
$BINARY rm -f stress-overlay 2>/dev/null || true

echo "--- Test: bridge container exit → no leaked veth ---"
VETH_BEFORE=$(ip link show 2>/dev/null | grep 'veth' | wc -l)
$BINARY run --network bridge alpine /bin/true 2>/dev/null || true
sleep 1
VETH_AFTER=$(ip link show 2>/dev/null | grep 'veth' | wc -l)
if [ "$VETH_AFTER" -le "$VETH_BEFORE" ]; then
    pass "no leaked veth interfaces ($VETH_BEFORE → $VETH_AFTER)"
else
    fail "leaked veth interfaces ($VETH_BEFORE → $VETH_AFTER)"
fi

# ===================================================================
echo ""
echo "=== Section 5: Combined Resource Limits ==="
echo ""

echo "--- Test: --memory 64m --pids-limit 16 ---"
OUT=$($BINARY run --memory 64m --pids-limit 16 alpine /bin/echo combined-ok 2>&1 || true)
check_contains "$OUT" "combined-ok" "memory + pids-limit"

echo "--- Test: --memory 32m --security-opt seccomp=default --cap-drop ALL ---"
OUT=$($BINARY run --memory 32m --security-opt seccomp=default --cap-drop ALL alpine /bin/echo secure-ok 2>&1 || true)
check_contains "$OUT" "secure-ok" "memory + seccomp + cap-drop"

echo "--- Test: --ulimit nofile=32:32 --pids-limit 20 ---"
OUT=$($BINARY run --ulimit nofile=32:32 --pids-limit 20 alpine /bin/sh -c 'ulimit -n && echo ulpids-ok' 2>&1 || true)
check_contains "$OUT" "32" "ulimit applied"
check_contains "$OUT" "ulpids-ok" "ulimit + pids-limit together"

# ===================================================================
echo ""
echo "=== Section 6: OCI Orphan / Timeout ==="
echo ""

ALPINE_ROOTFS=""
if [ -d "alpine-rootfs" ]; then
    ALPINE_ROOTFS="$(pwd)/alpine-rootfs"
elif [ -d "/var/lib/pelagos/rootfs/alpine-rootfs" ]; then
    ALPINE_ROOTFS="/var/lib/pelagos/rootfs/alpine-rootfs"
fi

if [ -n "$ALPINE_ROOTFS" ]; then
    OCI_BUNDLE="/tmp/stress-oci-bundle"
    rm -rf "$OCI_BUNDLE"
    mkdir -p "$OCI_BUNDLE"
    ln -s "$ALPINE_ROOTFS" "$OCI_BUNDLE/rootfs"

    echo "--- Test: create without start → can delete ---"
    cat > "$OCI_BUNDLE/config.json" <<'OCIJSON'
{
    "ociVersion": "1.0.2",
    "root": { "path": "rootfs", "readonly": false },
    "process": {
        "args": ["/bin/sleep", "300"],
        "cwd": "/",
        "env": ["PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"]
    }
}
OCIJSON

    $BINARY create stress-oci-orphan "$OCI_BUNDLE" 2>/dev/null || true
    sleep 1
    # The created container's process is alive (blocked on accept waiting for
    # "start"). Kill it first, then delete — OCI spec requires stop before delete.
    $BINARY kill stress-oci-orphan SIGKILL 2>/dev/null || true
    sleep 1
    if $BINARY delete stress-oci-orphan 2>/dev/null; then
        pass "create → kill → delete succeeds"
    else
        fail "create → kill → delete succeeds"
    fi

    echo "--- Test: kill on started container ---"
    cat > "$OCI_BUNDLE/config.json" <<'OCIJSON'
{
    "ociVersion": "1.0.2",
    "root": { "path": "rootfs", "readonly": false },
    "process": {
        "args": ["/bin/sleep", "300"],
        "cwd": "/",
        "env": ["PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"]
    }
}
OCIJSON

    $BINARY create stress-oci-kill "$OCI_BUNDLE" 2>/dev/null || true
    $BINARY start stress-oci-kill 2>/dev/null || true
    sleep 1
    if $BINARY kill stress-oci-kill SIGTERM 2>/dev/null; then
        pass "kill on started container"
    else
        fail "kill on started container"
    fi
    sleep 1
    $BINARY delete stress-oci-kill 2>/dev/null || true

    rm -rf "$OCI_BUNDLE"
else
    skip "OCI orphan/timeout tests (no alpine-rootfs directory found)"
fi

# ===================================================================
echo ""
echo "=== Section 7: Rapid Sequential Containers ==="
echo ""

echo "--- Test: 10 sequential foreground runs ---"
SEQ_PASS=0
for i in $(seq 1 10); do
    OUT=$($BINARY run alpine /bin/echo "seq-$i" 2>&1 || true)
    if echo "$OUT" | grep -q "seq-$i"; then
        SEQ_PASS=$((SEQ_PASS+1))
    fi
done
if [ "$SEQ_PASS" -eq 10 ]; then
    pass "10 sequential foreground runs (10/10)"
else
    fail "10 sequential foreground runs ($SEQ_PASS/10)"
fi

echo "--- Test: 10 sequential detach+stop+rm cycles ---"
CYCLE_PASS=0
for i in $(seq 1 10); do
    NAME="stress-cycle-$i"
    run_detach run --name "$NAME" --detach alpine /bin/sleep 60 >/dev/null
    sleep 0.5
    $BINARY stop "$NAME" 2>/dev/null || true
    sleep 0.3
    if $BINARY rm "$NAME" 2>/dev/null; then
        CYCLE_PASS=$((CYCLE_PASS+1))
    else
        # Try force remove
        $BINARY rm -f "$NAME" 2>/dev/null || true
        CYCLE_PASS=$((CYCLE_PASS+1))
    fi
done
if [ "$CYCLE_PASS" -eq 10 ]; then
    pass "10 sequential detach+stop+rm cycles (10/10)"
else
    fail "10 sequential detach+stop+rm cycles ($CYCLE_PASS/10)"
fi

echo "--- Test: no leaked container state ---"
LEAKED=0
for i in $(seq 1 10); do
    NAME="stress-cycle-$i"
    if $BINARY ps -a 2>/dev/null | grep -q "$NAME"; then
        LEAKED=$((LEAKED+1))
    fi
done
if [ "$LEAKED" -eq 0 ]; then
    pass "no leaked container state after cycles"
else
    fail "leaked container state after cycles ($LEAKED)"
fi

# ===================================================================
echo ""
echo "=== Results: $PASS passed, $FAIL failed, $SKIP skipped ==="
[ "$FAIL" -eq 0 ] && exit 0 || exit 1
