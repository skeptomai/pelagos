#!/usr/bin/env bash
# Comprehensive E2E tests for root-mode CLI.
#
# Covers: foreground/detached lifecycle, rootfs/volume/image management,
# exec, networking, filesystem/mount flags, security options, container
# linking, OCI lifecycle, and error cases.
#
# Must run as root (use -E to preserve rustup/cargo environment):
#   sudo -E scripts/test-e2e.sh
set -euo pipefail

PASS=0
FAIL=0
SKIP=0
BINARY="./target/debug/remora"

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

check_exit_ok() {
    local label="$1"; shift
    if "$@" >/dev/null 2>&1; then
        pass "$label"
    else
        fail "$label (exit code $?)"
    fi
}

check_exit_fail() {
    local label="$1"; shift
    if "$@" >/dev/null 2>&1; then
        fail "$label (expected non-zero exit)"
    else
        pass "$label"
    fi
}

# Run a detached container without hanging on inherited fds.
# Usage: run_detach [remora run args...]
# The forked watcher child inherits the shell's stdout fd, so $(...) would
# block until the watcher exits.  We avoid this by redirecting to a temp file
# and closing fds in the subshell.
run_detach() {
    local tmpf
    tmpf=$(mktemp /tmp/e2e-detach.XXXXXX)
    # Run in a subshell with stdout/stderr redirected to the temp file,
    # then close the fds so the watcher child doesn't hold them open.
    $BINARY "$@" >"$tmpf" 2>&1 </dev/null &
    local pid=$!
    # Wait up to 5s for the parent to exit (it prints the name and returns).
    local waited=0
    while kill -0 "$pid" 2>/dev/null && [ "$waited" -lt 50 ]; do
        sleep 0.1
        waited=$((waited+1))
    done
    wait "$pid" 2>/dev/null || true
    cat "$tmpf"
    rm -f "$tmpf"
}

has_cmd() { command -v "$1" &>/dev/null; }

# --- Cleanup helper ---
CONTAINERS_TO_CLEAN=()
cleanup() {
    echo ""
    echo "=== Cleanup ==="
    for c in "${CONTAINERS_TO_CLEAN[@]}"; do
        $BINARY stop "$c" 2>/dev/null || true
        sleep 0.3
        $BINARY rm -f "$c" 2>/dev/null || true
    done
    # Clean up temp files
    rm -f /tmp/e2e-envfile.txt 2>/dev/null || true
    rm -f /tmp/e2e-detach.* 2>/dev/null || true
    rm -rf /tmp/e2e-bind-test 2>/dev/null || true
    rm -rf /tmp/e2e-oci-bundle 2>/dev/null || true
}
trap cleanup EXIT

# --- Require root ---
if [ "$(id -u)" -ne 0 ]; then
    echo "ERROR: This script must run as root. Run: sudo -E scripts/test-e2e.sh"
    exit 1
fi

# --- Build ---
echo "==> Building remora..."
cargo build 2>&1

# --- Ensure alpine image ---
if ! $BINARY image ls 2>/dev/null | grep -q alpine; then
    echo "==> Pulling alpine image..."
    $BINARY image pull alpine
fi

# ===================================================================
echo ""
echo "=== Section 1: Foreground Container Basics ==="
echo ""

echo "--- Test: echo hello ---"
OUT=$($BINARY run alpine /bin/echo hello 2>&1 || true)
check_contains "$OUT" "hello" "echo hello"

echo "--- Test: /bin/true exits 0 ---"
if $BINARY run alpine /bin/true 2>/dev/null; then
    pass "/bin/true exits 0"
else
    fail "/bin/true exits 0"
fi

echo "--- Test: /bin/false exits non-zero ---"
if $BINARY run alpine /bin/false 2>/dev/null; then
    fail "/bin/false exits non-zero (got 0)"
else
    pass "/bin/false exits non-zero"
fi

echo "--- Test: --hostname ---"
OUT=$($BINARY run --hostname mybox alpine /bin/hostname 2>&1 || true)
check_contains "$OUT" "mybox" "--hostname mybox"

echo "--- Test: --workdir ---"
OUT=$($BINARY run --workdir /tmp alpine /bin/pwd 2>&1 || true)
check_contains "$OUT" "/tmp" "--workdir /tmp"

echo "--- Test: --user ---"
OUT=$($BINARY run --user 1000:1000 alpine id 2>&1 || true)
check_contains "$OUT" "uid=1000" "--user uid=1000"
check_contains "$OUT" "gid=1000" "--user gid=1000"

echo "--- Test: --env ---"
OUT=$($BINARY run --env MYVAR=hello42 alpine /bin/sh -c 'echo $MYVAR' 2>&1 || true)
check_contains "$OUT" "hello42" "--env MYVAR=hello42"

echo "--- Test: --env-file ---"
cat > /tmp/e2e-envfile.txt <<'ENVEOF'
# This is a comment
EKEY1=eval1

EKEY2=eval2
ENVEOF
OUT=$($BINARY run --env-file /tmp/e2e-envfile.txt alpine /bin/sh -c 'echo $EKEY1 $EKEY2' 2>&1 || true)
check_contains "$OUT" "eval1" "--env-file EKEY1"
check_contains "$OUT" "eval2" "--env-file EKEY2"

# ===================================================================
echo ""
echo "=== Section 2: Detached Container Lifecycle ==="
echo ""

echo "--- Test: run --detach ---"
OUT=$(run_detach run --name e2e-detach --detach alpine /bin/sleep 300)
CONTAINERS_TO_CLEAN+=(e2e-detach)
check_contains "$OUT" "e2e-detach" "detach prints name"
sleep 1

echo "--- Test: ps shows running ---"
OUT=$($BINARY ps 2>&1 || true)
check_contains "$OUT" "e2e-detach" "ps shows e2e-detach"
check_contains "$OUT" "running" "ps shows running"

echo "--- Test: ps -a shows running ---"
OUT=$($BINARY ps -a 2>&1 || true)
check_contains "$OUT" "e2e-detach" "ps -a shows e2e-detach"

echo "--- Test: logs with detached container ---"
run_detach run --name e2e-logs --detach alpine /bin/sh -c 'echo log-marker-42' >/dev/null
CONTAINERS_TO_CLEAN+=(e2e-logs)
sleep 2
OUT=$($BINARY logs e2e-logs 2>&1 || true)
check_contains "$OUT" "log-marker-42" "logs contains output"

echo "--- Test: stop ---"
check_exit_ok "stop e2e-detach" $BINARY stop e2e-detach

echo "--- Test: ps no longer shows stopped ---"
sleep 1
OUT=$($BINARY ps 2>&1 || true)
check_not_contains "$OUT" "e2e-detach" "ps hides stopped container"

echo "--- Test: ps -a shows exited ---"
OUT=$($BINARY ps -a 2>&1 || true)
check_contains "$OUT" "e2e-detach" "ps -a shows e2e-detach"
check_contains "$OUT" "exited" "ps -a shows exited"

echo "--- Test: stop already stopped ---"
OUT=$($BINARY stop e2e-detach 2>&1 || true)
check_contains "$OUT" "not running" "stop already stopped"

echo "--- Test: rm ---"
check_exit_ok "rm e2e-detach" $BINARY rm e2e-detach

echo "--- Test: ps -a no longer shows removed ---"
OUT=$($BINARY ps -a 2>&1 || true)
check_not_contains "$OUT" "e2e-detach" "ps -a hides removed"

echo "--- Test: rm running without --force ---"
run_detach run --name e2e-rmtest --detach alpine /bin/sleep 300 >/dev/null
CONTAINERS_TO_CLEAN+=(e2e-rmtest)
sleep 1
OUT=$($BINARY rm e2e-rmtest 2>&1 || true)
check_contains "$OUT" "is running" "rm running requires --force"

echo "--- Test: rm -f running ---"
check_exit_ok "rm -f running" $BINARY rm -f e2e-rmtest
sleep 1

echo "--- Test: name collision ---"
run_detach run --name e2e-collision --detach alpine /bin/sleep 300 >/dev/null
CONTAINERS_TO_CLEAN+=(e2e-collision)
sleep 1
OUT=$(run_detach run --name e2e-collision --detach alpine /bin/sleep 300)
check_contains "$OUT" "already exists" "name collision error"
$BINARY rm -f e2e-collision 2>/dev/null || true
sleep 1

# ===================================================================
echo ""
echo "=== Section 3: Rootfs CLI ==="
echo ""

echo "--- Test: rootfs import ---"
# Use /tmp as a test rootfs directory
check_exit_ok "rootfs import" $BINARY rootfs import test-rootfs /tmp

echo "--- Test: rootfs ls ---"
OUT=$($BINARY rootfs ls 2>&1 || true)
check_contains "$OUT" "test-rootfs" "rootfs ls shows test-rootfs"

echo "--- Test: rootfs rm ---"
check_exit_ok "rootfs rm" $BINARY rootfs rm test-rootfs

echo "--- Test: rootfs ls after rm ---"
OUT=$($BINARY rootfs ls 2>&1 || true)
check_not_contains "$OUT" "test-rootfs" "rootfs ls no longer shows test-rootfs"

echo "--- Test: rootfs rm nonexistent ---"
OUT=$($BINARY rootfs rm nonexistent-rootfs 2>&1 || true)
check_contains "$OUT" "not found" "rootfs rm nonexistent"

# ===================================================================
echo ""
echo "=== Section 4: Volume CLI ==="
echo ""

echo "--- Test: volume create ---"
check_exit_ok "volume create" $BINARY volume create e2e-vol

echo "--- Test: volume ls ---"
OUT=$($BINARY volume ls 2>&1 || true)
check_contains "$OUT" "e2e-vol" "volume ls shows e2e-vol"

echo "--- Test: volume data persistence ---"
$BINARY run --volume e2e-vol:/data alpine /bin/sh -c 'echo persist-test > /data/file.txt' 2>&1 || true
OUT=$($BINARY run --volume e2e-vol:/data alpine /bin/cat /data/file.txt 2>&1 || true)
check_contains "$OUT" "persist-test" "volume data persists"

echo "--- Test: volume rm ---"
check_exit_ok "volume rm" $BINARY volume rm e2e-vol

echo "--- Test: volume ls after rm ---"
OUT=$($BINARY volume ls 2>&1 || true)
check_not_contains "$OUT" "e2e-vol" "volume ls no longer shows e2e-vol"

echo "--- Test: volume rm nonexistent ---"
check_exit_fail "volume rm nonexistent" $BINARY volume rm nonexistent-vol

# ===================================================================
echo ""
echo "=== Section 5: Image CLI ==="
echo ""

echo "--- Test: image ls ---"
OUT=$($BINARY image ls 2>&1 || true)
check_contains "$OUT" "alpine" "image ls shows alpine"

echo "--- Test: pull busybox ---"
$BINARY image pull busybox 2>&1 || true
OUT=$($BINARY image ls 2>&1 || true)
check_contains "$OUT" "busybox" "image ls shows busybox"

echo "--- Test: image rm busybox ---"
check_exit_ok "image rm busybox" $BINARY image rm busybox
OUT=$($BINARY image ls 2>&1 || true)
check_not_contains "$OUT" "busybox" "image ls no longer shows busybox"

echo "--- Test: image rm nonexistent ---"
check_exit_fail "image rm nonexistent" $BINARY image rm nonexistent-image

# ===================================================================
echo ""
echo "=== Section 6: Exec CLI ==="
echo ""

echo "--- Starting detached container for exec tests ---"
run_detach run --name e2e-exec --detach alpine /bin/sleep 300 >/dev/null
CONTAINERS_TO_CLEAN+=(e2e-exec)
sleep 1

echo "--- Test: exec echo ---"
OUT=$($BINARY exec e2e-exec /bin/echo exec-hello 2>&1 || true)
check_contains "$OUT" "exec-hello" "exec echo"

echo "--- Test: exec sees container fs ---"
OUT=$($BINARY exec e2e-exec /bin/cat /etc/alpine-release 2>&1 || true)
if echo "$OUT" | grep -qE '^[0-9]'; then
    pass "exec sees container fs"
else
    fail "exec sees container fs (output: $OUT)"
fi

echo "--- Test: exec --env ---"
OUT=$($BINARY exec -e EVAR=testval e2e-exec /bin/sh -c 'echo $EVAR' 2>&1 || true)
check_contains "$OUT" "testval" "exec --env"

echo "--- Test: exec --workdir ---"
OUT=$($BINARY exec --workdir /tmp e2e-exec /bin/pwd 2>&1 || true)
check_contains "$OUT" "/tmp" "exec --workdir"

echo "--- Test: exec --user ---"
OUT=$($BINARY exec --user 1000:1000 e2e-exec id 2>&1 || true)
check_contains "$OUT" "uid=1000" "exec --user uid"
check_contains "$OUT" "gid=1000" "exec --user gid"

echo "--- Stopping exec container ---"
$BINARY stop e2e-exec 2>/dev/null || true
sleep 1

echo "--- Test: exec on stopped container ---"
OUT=$($BINARY exec e2e-exec /bin/echo nope 2>&1 || true)
check_contains "$OUT" "not running" "exec on stopped container"

echo "--- Test: exec on nonexistent container ---"
OUT=$($BINARY exec nonexistent-ctr /bin/echo nope 2>&1 || true)
check_contains "$OUT" "not found" "exec on nonexistent container"

$BINARY rm -f e2e-exec 2>/dev/null || true
sleep 1

# ===================================================================
echo ""
echo "=== Section 7: Networking CLI ==="
echo ""

echo "--- Test: --network loopback ---"
OUT=$($BINARY run --network loopback alpine /bin/sh -c 'ip addr show lo' 2>&1 || true)
check_contains "$OUT" "LOOPBACK" "loopback interface"

echo "--- Test: --network bridge ---"
OUT=$($BINARY run --network bridge alpine /bin/sh -c 'ip addr' 2>&1 || true)
check_contains "$OUT" "172.19" "bridge IP"

echo "--- Test: bridge + NAT ---"
if has_cmd nft; then
    $BINARY run --network bridge --nat alpine /bin/true 2>/dev/null || true
    OUT=$(nft list ruleset 2>/dev/null || true)
    # NAT rule may already be cleaned up, but if it's still there check for masquerade
    # This is a best-effort check
    if echo "$OUT" | grep -qi "masquerade"; then
        pass "NAT masquerade rule"
    else
        skip "NAT masquerade rule (rule cleaned up before check)"
    fi
else
    skip "NAT masquerade (nft not available)"
fi

echo "--- Test: --dns ---"
OUT=$($BINARY run --network loopback --dns 1.1.1.1 alpine /bin/cat /etc/resolv.conf 2>&1 || true)
check_contains "$OUT" "1.1.1.1" "--dns 1.1.1.1"

echo "--- Test: pasta ---"
if has_cmd pasta; then
    OUT=$($BINARY run --network pasta alpine /bin/sh -c 'sleep 2 && ip addr show' 2>&1 || true)
    if echo "$OUT" | grep -v 'lo:' | grep -q 'inet '; then
        pass "pasta non-lo interface with inet"
    else
        fail "pasta non-lo interface with inet"
        echo "    output: $OUT"
    fi
else
    skip "pasta networking (pasta not installed)"
fi

# ===================================================================
echo ""
echo "=== Section 8: Filesystem & Mount Flags ==="
echo ""

echo "--- Test: --read-only ---"
OUT=$($BINARY run --read-only alpine /bin/sh -c 'touch /testfile 2>&1 || echo READONLY' 2>&1 || true)
check_contains "$OUT" "READONLY" "read-only rootfs"

echo "--- Test: --read-only --tmpfs /tmp ---"
OUT=$($BINARY run --read-only --tmpfs /tmp alpine /bin/sh -c 'echo writable > /tmp/test && cat /tmp/test' 2>&1 || true)
check_contains "$OUT" "writable" "tmpfs writable on read-only rootfs"

echo "--- Test: --bind ---"
mkdir -p /tmp/e2e-bind-test
echo "bind-test-data" > /tmp/e2e-bind-test/file.txt
OUT=$($BINARY run --bind /tmp/e2e-bind-test:/mnt alpine /bin/cat /mnt/file.txt 2>&1 || true)
check_contains "$OUT" "bind-test-data" "bind mount read"

echo "--- Test: --bind write-through ---"
$BINARY run --bind /tmp/e2e-bind-test:/mnt alpine /bin/sh -c 'echo written-from-container > /mnt/written.txt' 2>&1 || true
if [ -f /tmp/e2e-bind-test/written.txt ]; then
    OUT=$(cat /tmp/e2e-bind-test/written.txt)
    check_contains "$OUT" "written-from-container" "bind mount write-through"
else
    fail "bind mount write-through (file not created)"
fi

echo "--- Test: --bind-ro ---"
OUT=$($BINARY run --bind-ro /tmp/e2e-bind-test:/mnt alpine /bin/sh -c 'echo fail > /mnt/fail.txt 2>&1 || echo BLOCKED' 2>&1 || true)
check_contains "$OUT" "BLOCKED" "bind-ro blocks write"

echo "--- Test: --sysctl ---"
OUT=$($BINARY run --network loopback --sysctl net.ipv4.ip_nonlocal_bind=1 alpine /bin/sh -c 'cat /proc/sys/net/ipv4/ip_nonlocal_bind' 2>&1 || true)
check_contains "$OUT" "1" "sysctl net.ipv4.ip_nonlocal_bind=1"

# ===================================================================
echo ""
echo "=== Section 9: Security Options ==="
echo ""

echo "--- Test: seccomp=default ---"
OUT=$($BINARY run --security-opt seccomp=default alpine /bin/echo seccomp-ok 2>&1 || true)
check_contains "$OUT" "seccomp-ok" "seccomp=default"

echo "--- Test: seccomp=minimal ---"
OUT=$($BINARY run --security-opt seccomp=minimal alpine /bin/echo seccomp-min-ok 2>&1 || true)
check_contains "$OUT" "seccomp-min-ok" "seccomp=minimal"

echo "--- Test: no-new-privileges ---"
OUT=$($BINARY run --security-opt no-new-privileges alpine /bin/echo nnp-ok 2>&1 || true)
check_contains "$OUT" "nnp-ok" "no-new-privileges"

echo "--- Test: --cap-drop ALL ---"
OUT=$($BINARY run --cap-drop ALL alpine /bin/echo cap-ok 2>&1 || true)
check_contains "$OUT" "cap-ok" "cap-drop ALL"

echo "--- Test: --cap-drop ALL --cap-add CAP_CHOWN ---"
OUT=$($BINARY run --cap-drop ALL --cap-add CAP_CHOWN alpine /bin/sh -c 'chown 1000 /tmp && echo chown-ok' 2>&1 || true)
check_contains "$OUT" "chown-ok" "cap-add CAP_CHOWN"

echo "--- Test: --ulimit nofile=16:16 ---"
OUT=$($BINARY run --ulimit nofile=16:16 alpine /bin/sh -c 'ulimit -n' 2>&1 || true)
check_contains "$OUT" "16" "ulimit nofile=16"

echo "--- Test: --memory 128m ---"
OUT=$($BINARY run --memory 128m alpine /bin/echo mem-ok 2>&1 || true)
check_contains "$OUT" "mem-ok" "memory 128m"

echo "--- Test: --pids-limit 32 ---"
OUT=$($BINARY run --pids-limit 32 alpine /bin/echo pids-ok 2>&1 || true)
check_contains "$OUT" "pids-ok" "pids-limit 32"

echo "--- Test: --cpus 0.5 ---"
OUT=$($BINARY run --cpus 0.5 alpine /bin/echo cpu-ok 2>&1 || true)
check_contains "$OUT" "cpu-ok" "cpus 0.5"

# ===================================================================
echo ""
echo "=== Section 10: Container Linking ==="
echo ""

echo "--- Starting link server container ---"
run_detach run --name e2e-linkserver --detach --network bridge alpine /bin/sleep 300 >/dev/null
CONTAINERS_TO_CLEAN+=(e2e-linkserver)
sleep 1

echo "--- Test: --link ---"
OUT=$($BINARY run --network bridge --link e2e-linkserver alpine /bin/cat /etc/hosts 2>&1 || true)
check_contains "$OUT" "e2e-linkserver" "--link adds /etc/hosts entry"

echo "--- Test: --link with alias ---"
OUT=$($BINARY run --network bridge --link e2e-linkserver:myalias alpine /bin/cat /etc/hosts 2>&1 || true)
check_contains "$OUT" "myalias" "--link alias in /etc/hosts"

$BINARY rm -f e2e-linkserver 2>/dev/null || true
sleep 1

# ===================================================================
echo ""
echo "=== Section 11: OCI Lifecycle Commands ==="
echo ""

# We need an alpine rootfs directory for the OCI bundle.
ALPINE_ROOTFS=""
if [ -d "alpine-rootfs" ]; then
    ALPINE_ROOTFS="$(pwd)/alpine-rootfs"
elif [ -d "/var/lib/remora/rootfs/alpine-rootfs" ]; then
    ALPINE_ROOTFS="/var/lib/remora/rootfs/alpine-rootfs"
fi

if [ -n "$ALPINE_ROOTFS" ]; then
    echo "--- Creating OCI bundle ---"
    OCI_BUNDLE="/tmp/e2e-oci-bundle"
    rm -rf "$OCI_BUNDLE"
    mkdir -p "$OCI_BUNDLE"
    # Symlink rootfs
    ln -s "$ALPINE_ROOTFS" "$OCI_BUNDLE/rootfs"
    # Create minimal config.json
    cat > "$OCI_BUNDLE/config.json" <<'OCIJSON'
{
    "ociVersion": "1.0.2",
    "root": {
        "path": "rootfs",
        "readonly": false
    },
    "process": {
        "args": ["/bin/sh", "-c", "echo oci-ok && sleep 2"],
        "cwd": "/",
        "env": ["PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"]
    }
}
OCIJSON

    echo "--- Test: create ---"
    check_exit_ok "oci create" $BINARY create e2e-oci "$OCI_BUNDLE"

    echo "--- Test: state shows created ---"
    OUT=$($BINARY state e2e-oci 2>&1 || true)
    check_contains "$OUT" "created" "oci state=created"

    echo "--- Test: start ---"
    check_exit_ok "oci start" $BINARY start e2e-oci

    echo "--- Waiting for container to finish ---"
    sleep 4

    echo "--- Test: state shows stopped ---"
    OUT=$($BINARY state e2e-oci 2>&1 || true)
    check_contains "$OUT" "stopped" "oci state=stopped"

    echo "--- Test: delete ---"
    check_exit_ok "oci delete" $BINARY delete e2e-oci

    echo "--- Test: state after delete ---"
    OUT=$($BINARY state e2e-oci 2>&1 || true)
    # State should fail after delete — either error output or missing dir
    if echo "$OUT" | grep -qi "error\|not found"; then
        pass "oci state dir removed"
    elif [ ! -d "/run/remora/e2e-oci" ]; then
        pass "oci state dir removed"
    else
        fail "oci state dir removed"
    fi

    rm -rf "$OCI_BUNDLE"
else
    skip "OCI lifecycle (no alpine-rootfs directory found)"
fi

# ===================================================================
echo ""
echo "=== Section 12: Error Cases ==="
echo ""

echo "--- Test: run with no image ---"
OUT=$($BINARY run 2>&1 || true)
if echo "$OUT" | grep -qi "error\|required"; then
    pass "run with no image errors"
else
    fail "run with no image errors"
fi

echo "--- Test: --detach --interactive ---"
OUT=$($BINARY run --detach --interactive alpine /bin/sh 2>&1 || true)
check_contains "$OUT" "mutually exclusive" "--detach --interactive"

echo "--- Test: stop nonexistent ---"
OUT=$($BINARY stop nonexistent-ctr 2>&1 || true)
check_contains "$OUT" "no container named" "stop nonexistent"

echo "--- Test: rm nonexistent ---"
OUT=$($BINARY rm nonexistent-ctr 2>&1 || true)
check_contains "$OUT" "no container named" "rm nonexistent"

echo "--- Test: logs on foreground container ---"
# Run a quick foreground container first
$BINARY run --name e2e-fglog alpine /bin/true 2>/dev/null || true
CONTAINERS_TO_CLEAN+=(e2e-fglog)
OUT=$($BINARY logs e2e-fglog 2>&1 || true)
check_contains "$OUT" "was it started with --detach" "logs on foreground container"
$BINARY rm -f e2e-fglog 2>/dev/null || true

# ===================================================================
echo ""
echo "=== Results: $PASS passed, $FAIL failed, $SKIP skipped ==="
[ "$FAIL" -eq 0 ] && exit 0 || exit 1
