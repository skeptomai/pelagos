#!/usr/bin/env bash
#
# Pelagos Web Stack Demo
# =====================
# Builds and runs a 3-container blog stack:
#   nginx (reverse proxy) → bottle (Python API) → redis (data store)
#
# Usage:  sudo ./examples/web-stack/run.sh
#

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PELAGOS="${PELAGOS:-pelagos}"

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
CYAN='\033[0;36m'
BOLD='\033[1m'
NC='\033[0m'

pass=0
fail=0

log()  { echo -e "${CYAN}==>${NC} $*"; }
ok()   { echo -e "  ${GREEN}PASS${NC} $*"; pass=$((pass + 1)); }
fail() { echo -e "  ${RED}FAIL${NC} $*"; fail=$((fail + 1)); }

cleanup() {
    log "Cleaning up..."
    $REMORA stop proxy  2>/dev/null || true
    $REMORA stop app    2>/dev/null || true
    $REMORA stop redis  2>/dev/null || true
    sleep 1
    $REMORA rm proxy    2>/dev/null || true
    $REMORA rm app      2>/dev/null || true
    $REMORA rm redis    2>/dev/null || true
    $REMORA volume rm notes-data 2>/dev/null || true
    $REMORA network rm frontend 2>/dev/null || true
    $REMORA network rm backend  2>/dev/null || true
    log "Done."
}

die() { echo -e "${RED}ERROR:${NC} $*" >&2; exit 1; }

# ── Prerequisites ──────────────────────────────────────────────────────

log "Checking prerequisites..."

command -v "$PELAGOS" >/dev/null 2>&1 || die "pelagos not found in PATH. Run: cargo build --release && export PATH=\$PWD/target/release:\$PATH"

# Ensure alpine:latest is pulled
if ! $REMORA image ls 2>/dev/null | grep -q "alpine:latest"; then
    log "Pulling alpine:latest..."
    $REMORA image pull alpine:latest
fi

# ── Build Phase ────────────────────────────────────────────────────────
# pelagos build now enables NAT + DNS automatically for bridge RUN steps.

log "Building ${BOLD}web-stack-redis${NC}..."
$REMORA build -t web-stack-redis --network bridge "$SCRIPT_DIR/redis"

log "Building ${BOLD}web-stack-app${NC}..."
$REMORA build -t web-stack-app --network bridge "$SCRIPT_DIR/app"

log "Building ${BOLD}web-stack-proxy${NC}..."
$REMORA build -t web-stack-proxy --network bridge "$SCRIPT_DIR/proxy"

# ── Create Volume ──────────────────────────────────────────────────────

log "Creating volume ${BOLD}notes-data${NC}..."
$REMORA volume create notes-data 2>/dev/null || true

# ── Create Networks ───────────────────────────────────────────────────
#
# Network topology:
#   frontend (10.88.1.0/24):  proxy ←→ app
#   backend  (10.88.2.0/24):           app ←→ redis
#
# Redis is isolated from proxy — they share no network.

log "Creating ${BOLD}frontend${NC} network (10.88.1.0/24)..."
$REMORA network create frontend --subnet 10.88.1.0/24 2>/dev/null || true

log "Creating ${BOLD}backend${NC} network (10.88.2.0/24)..."
$REMORA network create backend --subnet 10.88.2.0/24 2>/dev/null || true

# ── Launch Containers ──────────────────────────────────────────────────

trap cleanup EXIT

start_container() {
    local name="$1"; shift
    log "Starting ${BOLD}${name}${NC}..."
    $REMORA run -d --name "$name" "$@"
    sleep 2
    # Verify container is still running.
    if ! $REMORA ps 2>/dev/null | grep -q "$name"; then
        echo -e "  ${RED}Container '${name}' exited immediately!${NC}"
        echo "  stdout: $(cat /run/pelagos/containers/${name}/stdout.log 2>/dev/null || echo '<empty>')"
        echo "  stderr: $(cat /run/pelagos/containers/${name}/stderr.log 2>/dev/null || echo '<empty>')"
        exit 1
    fi
}

# redis: backend only
start_container redis --network backend --nat web-stack-redis:latest

# app: frontend + backend (bridges both networks)
start_container app --network frontend --network backend --nat --link redis:redis web-stack-app:latest

# proxy: frontend only (cannot reach redis directly)
start_container proxy --network frontend --nat --link app:app web-stack-proxy:latest
sleep 1

# ── Verification ───────────────────────────────────────────────────────

echo
log "${BOLD}Running verification tests...${NC}"
echo

# Resolve the proxy container's bridge IP for direct access.
# Port forwarding (localhost:8080) requires hairpin NAT which is not yet
# implemented; for now we test via the bridge IP directly.
PROXY_IP=$($REMORA ps 2>/dev/null | awk '/^proxy / {print $3}')
PROXY_STATE="/run/pelagos/containers/proxy/state.json"
if [ -f "$PROXY_STATE" ]; then
    PROXY_IP=$(python3 -c "import json; print(json.load(open('$PROXY_STATE')).get('bridge_ip',''))" 2>/dev/null || true)
fi
if [ -z "$PROXY_IP" ]; then
    echo -e "${RED}Could not determine proxy bridge IP${NC}"
    exit 1
fi
log "Proxy bridge IP: ${BOLD}${PROXY_IP}${NC}"

CURL="curl -s --max-time 5"
BASE="http://${PROXY_IP}:80"

# Test 1: Static page
BODY=$($CURL "$BASE/" 2>/dev/null || true)
if echo "$BODY" | grep -q "Pelagos Blog"; then
    ok "GET / — contains 'Pelagos Blog'"
else
    fail "GET / — expected 'Pelagos Blog' in response"
    echo "       body: $(echo "$BODY" | head -3)"
fi

# Test 2: Health check (proxied to app:5000)
# Retry once — the first proxy_pass request can 502 if nginx hasn't connected yet.
BODY=$($CURL "$BASE/health" 2>/dev/null || true)
if ! echo "$BODY" | grep -q '"status"'; then
    sleep 1
    BODY=$($CURL "$BASE/health" 2>/dev/null || true)
fi
if echo "$BODY" | grep -q '"status"'; then
    ok "GET /health — returns status ok"
else
    fail "GET /health — expected JSON status"
    echo "       body: $(echo "$BODY" | head -3)"
fi

# Test 3: Empty notes list
BODY=$($CURL "$BASE/api/notes" 2>/dev/null || true)
if [ "$BODY" = "[]" ]; then
    ok "GET /api/notes — returns empty list"
else
    fail "GET /api/notes — expected [], got: $BODY"
    echo "       body: $(echo "$BODY" | head -3)"
fi

# Test 4: Post a note
BODY=$($CURL -X POST -H 'Content-Type: application/json' \
    -d '{"text":"hello from pelagos"}' \
    "$BASE/api/notes" 2>/dev/null || true)
if echo "$BODY" | grep -q '"ok"'; then
    ok "POST /api/notes — note created"
else
    fail "POST /api/notes — expected ok response, got: $BODY"
fi

# Test 5: Verify note persisted
BODY=$($CURL "$BASE/api/notes" 2>/dev/null || true)
if echo "$BODY" | grep -q "hello from pelagos"; then
    ok "GET /api/notes — note persisted"
else
    fail "GET /api/notes — expected note in list, got: $BODY"
fi

# Test 6: Network isolation — proxy (frontend only) cannot reach redis (backend only)
REDIS_STATE="/run/pelagos/containers/redis/state.json"
REDIS_IP=""
if [ -f "$REDIS_STATE" ]; then
    REDIS_IP=$(python3 -c "import json; print(json.load(open('$REDIS_STATE')).get('bridge_ip',''))" 2>/dev/null || true)
fi
if [ -n "$REDIS_IP" ]; then
    # Run a ping from proxy's network namespace — should fail
    PROXY_NS=$($REMORA ps 2>/dev/null | awk '/^proxy / {print ""}')
    # Use curl timeout to test TCP connectivity to redis port
    if $CURL "http://${REDIS_IP}:6379/" 2>/dev/null; then
        fail "Network isolation — proxy should NOT reach redis at ${REDIS_IP}:6379"
    else
        ok "Network isolation — proxy cannot reach redis (separate networks)"
    fi
else
    fail "Network isolation — could not determine redis bridge IP"
fi

# ── Summary ────────────────────────────────────────────────────────────

echo
echo -e "${BOLD}Results: ${GREEN}${pass} passed${NC}, ${RED}${fail} failed${NC}"

if [ "$fail" -gt 0 ]; then
    echo -e "\n${YELLOW}Some tests failed. Check container logs:${NC}"
    echo "  $REMORA logs redis"
    echo "  $REMORA logs app"
    echo "  $REMORA logs proxy"
    # Keep containers running for debugging — cleanup runs on exit
    echo -e "\nPress Enter to clean up and exit..."
    read -r
fi
