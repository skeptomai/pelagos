#!/usr/bin/env bash
#
# Pelagos Monitoring Stack Demo
# ============================
# Builds Prometheus + Loki + Grafana images, starts the stack with
# `pelagos compose up`, and runs smoke tests against the live services.
#
# Usage:  sudo ./examples/compose/monitoring/run.sh
#
# Options:
#   GRAFANA_PASSWORD=N   Override the Grafana admin password (default: admin)
#   --no-stack           Skip image build (images must already exist)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PELAGOS="${PELAGOS:-pelagos}"
GRAFANA_PASSWORD="${GRAFANA_PASSWORD:-admin}"

RED='\033[0;31m'
GREEN='\033[0;32m'
CYAN='\033[0;36m'
BOLD='\033[1m'
NC='\033[0m'

pass=0
fail=0

log()  { echo -e "${CYAN}==>${NC} $*"; }
ok()   { echo -e "  ${GREEN}PASS${NC} $*"; pass=$((pass + 1)); }
fail() { echo -e "  ${RED}FAIL${NC} $*"; fail=$((fail + 1)); }
die()  { echo -e "${RED}ERROR:${NC} $*" >&2; exit 1; }

BUILD_STACK=1
for arg in "$@"; do
    case "$arg" in
        --no-stack) BUILD_STACK=0 ;;
    esac
done

command -v "$PELAGOS" >/dev/null 2>&1 || \
    die "pelagos not found.  Run: cargo build --release && sudo PELAGOS=\$PWD/target/release/pelagos $0"

# ── Build Phase ───────────────────────────────────────────────────

if [ "$BUILD_STACK" -eq 1 ]; then
    if ! $REMORA image ls 2>/dev/null | grep -q "alpine:latest"; then
        log "Pulling alpine:latest..."
        $REMORA image pull alpine:latest
    fi

    for svc in prometheus loki grafana; do
        tag="monitoring-${svc}:latest"
        if $REMORA image ls 2>/dev/null | grep -q "$tag"; then
            log "Image ${BOLD}${tag}${NC} already built"
        else
            log "Building ${BOLD}${tag}${NC}..."
            $REMORA build -t "monitoring-${svc}" --network bridge "$SCRIPT_DIR/${svc}"
        fi
    done
fi

# ── Compose Up ────────────────────────────────────────────────────

log "Starting monitoring stack..."
GRAFANA_PASSWORD="$GRAFANA_PASSWORD" \
    $REMORA compose up -f "$SCRIPT_DIR/compose.reml" -p monitoring --foreground &
COMPOSE_PID=$!

cleanup() {
    log "Tearing down..."
    $REMORA compose down -f "$SCRIPT_DIR/compose.reml" -p monitoring -v 2>/dev/null || true
    wait "$COMPOSE_PID" 2>/dev/null || true
    log "Done."
}
trap cleanup EXIT

# Wait for all three backends to be ready
log "Waiting for services to be ready..."
for i in $(seq 1 60); do
    prom_ok=0; loki_ok=0; grafana_ok=0
    curl -sf --max-time 2 "http://127.0.0.1:9090/-/ready"  >/dev/null 2>&1 && prom_ok=1    || true
    curl -sf --max-time 2 "http://127.0.0.1:3100/ready"    >/dev/null 2>&1 && loki_ok=1    || true
    curl -sf --max-time 2 "http://127.0.0.1:3000/api/health" >/dev/null 2>&1 && grafana_ok=1 || true
    if [ "$prom_ok" -eq 1 ] && [ "$loki_ok" -eq 1 ] && [ "$grafana_ok" -eq 1 ]; then
        break
    fi
    sleep 2
done

# ── Verification ──────────────────────────────────────────────────

echo
log "${BOLD}Running verification tests...${NC}"
echo

CURL="curl -s --max-time 10"

# Test 1: Prometheus ready
BODY=$($CURL "http://127.0.0.1:9090/-/ready" 2>/dev/null || true)
if [ -n "$BODY" ]; then
    ok "GET /-/ready — Prometheus is ready"
else
    fail "GET /-/ready — Prometheus did not respond"
fi

# Test 2: Prometheus self-scrape targets
BODY=$($CURL "http://127.0.0.1:9090/api/v1/targets" 2>/dev/null || true)
if echo "$BODY" | grep -q '"status":"success"'; then
    ok "GET /api/v1/targets — Prometheus scrape targets configured"
else
    fail "GET /api/v1/targets — unexpected response: ${BODY:0:100}"
fi

# Test 3: Loki ready
BODY=$($CURL "http://127.0.0.1:3100/ready" 2>/dev/null || true)
if echo "$BODY" | grep -qi "ready"; then
    ok "GET /ready — Loki is ready"
else
    fail "GET /ready — Loki did not respond: ${BODY:0:100}"
fi

# Test 4: Grafana health
BODY=$($CURL "http://127.0.0.1:3000/api/health" 2>/dev/null || true)
if echo "$BODY" | grep -q '"database"'; then
    ok "GET /api/health — Grafana database healthy"
else
    fail "GET /api/health — expected database status, got: ${BODY:0:100}"
fi

# Test 5: Grafana datasources provisioned
BODY=$($CURL -u "admin:${GRAFANA_PASSWORD}" "http://127.0.0.1:3000/api/datasources" 2>/dev/null || true)
if echo "$BODY" | grep -q '"Prometheus"'; then
    ok "GET /api/datasources — Prometheus datasource provisioned"
else
    fail "GET /api/datasources — Prometheus datasource missing: ${BODY:0:200}"
fi

if echo "$BODY" | grep -q '"Loki"'; then
    ok "GET /api/datasources — Loki datasource provisioned"
else
    fail "GET /api/datasources — Loki datasource missing: ${BODY:0:200}"
fi

# Test 6: Service status
echo
log "Service status:"
$REMORA compose ps -f "$SCRIPT_DIR/compose.reml" -p monitoring

# ── Summary ───────────────────────────────────────────────────────

echo
echo -e "${BOLD}Results: ${GREEN}${pass} passed${NC}, ${RED}${fail} failed${NC}"

if [ "$fail" -gt 0 ]; then
    echo -e "\nCheck service logs:"
    for svc in prometheus loki grafana; do
        echo "  $REMORA compose logs -f $SCRIPT_DIR/compose.reml -p monitoring $svc"
    done
fi

echo
echo -e "${CYAN}Prometheus:${NC}  http://localhost:9090"
echo -e "${CYAN}Grafana:${NC}     http://localhost:3000  (admin / ${GRAFANA_PASSWORD})"
echo -e "${CYAN}Loki:${NC}        http://localhost:3100"
echo
echo "Press Enter to tear down..."
read -r
