#!/usr/bin/env bash
#
# Remora Jupyter Stack Demo
# =========================
# Builds JupyterLab + Redis images, starts the stack with
# `remora compose up`, and runs smoke tests against the live lab.
#
# Usage:  sudo ./examples/compose/jupyter/run.sh
#
# Options:
#   JUPYTER_PORT=N   Override the published JupyterLab port (default 8888)
#   --no-stack       Skip image build (images must already exist)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REMORA="${REMORA:-remora}"
JUPYTER_PORT="${JUPYTER_PORT:-8888}"

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

command -v "$REMORA" >/dev/null 2>&1 || \
    die "remora not found.  Run: cargo build --release && sudo REMORA=\$PWD/target/release/remora $0"

# ── Build Phase ───────────────────────────────────────────────────

if [ "$BUILD_STACK" -eq 1 ]; then
    if ! $REMORA image ls 2>/dev/null | grep -q "alpine:latest"; then
        log "Pulling alpine:latest..."
        $REMORA image pull alpine:latest
    fi

    for svc in redis jupyterlab; do
        tag="jupyter-${svc}:latest"
        if $REMORA image ls 2>/dev/null | grep -q "$tag"; then
            log "Image ${BOLD}${tag}${NC} already built"
        else
            log "Building ${BOLD}${tag}${NC}..."
            $REMORA build -t "jupyter-${svc}" --network bridge "$SCRIPT_DIR/${svc}"
        fi
    done
fi

# ── Compose Up ────────────────────────────────────────────────────

log "Starting Jupyter stack (port ${BOLD}${JUPYTER_PORT}${NC})..."
JUPYTER_PORT="$JUPYTER_PORT" \
    $REMORA compose up -f "$SCRIPT_DIR/compose.reml" -p jupyter --foreground &
COMPOSE_PID=$!

cleanup() {
    log "Tearing down..."
    $REMORA compose down -f "$SCRIPT_DIR/compose.reml" -p jupyter -v 2>/dev/null || true
    wait "$COMPOSE_PID" 2>/dev/null || true
    log "Done."
}
trap cleanup EXIT

# Wait for JupyterLab to be ready
log "Waiting for JupyterLab on port ${JUPYTER_PORT}..."
for i in $(seq 1 60); do
    if curl -s --max-time 2 "http://127.0.0.1:${JUPYTER_PORT}/api" >/dev/null 2>&1; then
        break
    fi
    sleep 2
done

# ── Verification ──────────────────────────────────────────────────

echo
log "${BOLD}Running verification tests...${NC}"
echo

CURL="curl -s --max-time 10"
BASE="http://127.0.0.1:${JUPYTER_PORT}"

# Test 1: API endpoint
BODY=$($CURL "$BASE/api" 2>/dev/null || true)
if echo "$BODY" | grep -q '"version"'; then
    ok "GET /api — JupyterLab API responds with version info"
else
    fail "GET /api — expected version JSON, got: $BODY"
fi

# Test 2: Lab UI
BODY=$($CURL "$BASE/lab" 2>/dev/null || true)
if echo "$BODY" | grep -qi "jupyterlab"; then
    ok "GET /lab — JupyterLab UI served"
else
    fail "GET /lab — expected JupyterLab HTML"
fi

# Test 3: Kernel specs available
BODY=$($CURL "$BASE/api/kernelspecs" 2>/dev/null || true)
if echo "$BODY" | grep -q '"kernelspecs"'; then
    ok "GET /api/kernelspecs — Python kernel registered"
else
    fail "GET /api/kernelspecs — expected kernelspecs JSON"
fi

# Test 4: Sessions endpoint (list of running kernels)
BODY=$($CURL "$BASE/api/sessions" 2>/dev/null || true)
if [ "$BODY" = "[]" ] || echo "$BODY" | grep -q '\['; then
    ok "GET /api/sessions — sessions endpoint reachable"
else
    fail "GET /api/sessions — unexpected response: $BODY"
fi

# Test 5: Service status
echo
log "Service status:"
$REMORA compose ps -f "$SCRIPT_DIR/compose.reml" -p jupyter

# ── Summary ───────────────────────────────────────────────────────

echo
echo -e "${BOLD}Results: ${GREEN}${pass} passed${NC}, ${RED}${fail} failed${NC}"

if [ "$fail" -gt 0 ]; then
    echo -e "\nCheck service logs:"
    echo "  $REMORA compose logs -f $SCRIPT_DIR/compose.reml -p jupyter redis"
    echo "  $REMORA compose logs -f $SCRIPT_DIR/compose.reml -p jupyter jupyterlab"
fi

echo -e "\n${CYAN}JupyterLab:${NC} http://localhost:${JUPYTER_PORT}/lab  (no token required)"
echo -e "${CYAN}Logs:${NC}       $REMORA compose logs -f $SCRIPT_DIR/compose.reml -p jupyter jupyterlab"
echo -e "\nPress Enter to tear down..."
read -r
