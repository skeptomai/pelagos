#!/usr/bin/env bash
#
# test-reml.sh — manual test driver for the .reml Lisp compose format
#
# Phase 1 (no root):  cargo test — evaluator correctness, fixture parsing,
#                     env fallback, depends-on port syntax
# Phase 2 (root):     full 3-service web stack via compose.reml — builds
#                     images, starts stack, runs HTTP smoke tests, shows
#                     on-ready hook log output, tears down
#
# Usage:
#   # Phase 1 only (no root needed):
#   ./scripts/test-reml.sh --no-stack
#
#   # Full run (requires root for compose up):
#   sudo -E ./scripts/test-reml.sh
#
#   # Custom host port (tests env-driven config):
#   BLOG_PORT=9090 sudo -E ./scripts/test-reml.sh

set -euo pipefail
cd "$(dirname "$0")/.."

# ── Colours ───────────────────────────────────────────────────────
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
CYAN='\033[0;36m'
BOLD='\033[1m'
NC='\033[0m'

log()     { echo -e "\n${CYAN}══>${NC} ${BOLD}$*${NC}"; }
step()    { echo -e "    ${CYAN}·${NC} $*"; }
ok()      { echo -e "    ${GREEN}✔${NC}  $*"; }
warn()    { echo -e "    ${YELLOW}⚠${NC}  $*"; }
fail()    { echo -e "    ${RED}✘${NC}  $*"; FAILURES=$((FAILURES + 1)); }
die()     { echo -e "\n${RED}ERROR:${NC} $*" >&2; exit 1; }
FAILURES=0

NO_STACK=false
for arg in "$@"; do
    [ "$arg" = "--no-stack" ] && NO_STACK=true
done

BLOG_PORT="${BLOG_PORT:-8080}"
REMORA="${REMORA:-./target/release/remora}"
COMPOSE_REML="examples/compose/web-stack/compose.reml"
COMPOSE_REM="examples/compose/web-stack/compose.rem"
WEB_STACK_DIR="examples/web-stack"
PROJECT="reml-test"

# ── Phase 0: Prerequisites ─────────────────────────────────────────
log "Phase 0: prerequisites"

if ! command -v cargo >/dev/null 2>&1; then
    die "cargo not found"
fi
step "cargo found: $(cargo --version)"

if [ ! -f "$COMPOSE_REML" ]; then
    die "compose.reml not found at $COMPOSE_REML"
fi
step "compose.reml found"

if [ ! -f "$COMPOSE_REM" ]; then
    die "compose.rem not found at $COMPOSE_REM"
fi
step "compose.rem found"

# ── Phase 1: No-root evaluator tests ──────────────────────────────
log "Phase 1: Lisp evaluator tests (no root required)"
echo
echo "  These tests run without containers or root.  They cover:"
echo "  · eval_str  — inline string evaluation (unit tests in src/lisp/mod.rs)"
echo "  · eval_file — reads compose.reml from disk, asserts ComposeFile structure"
echo "  · depends-on with port → HealthCheck::Port"
echo "  · env/fallback pattern (BLOG_PORT default + override)"
echo

step "Running: cargo test --lib  (evaluator unit tests)"
cargo test --lib --quiet 2>&1 | tail -3

step "Running: cargo test --test integration_tests test_lisp  (fixture tests)"
cargo test --test integration_tests test_lisp -- --nocapture 2>&1

echo
ok "All no-root Lisp tests passed"

# ── Phase 1b: Show the fixture file ──────────────────────────────
log "Phase 1b: compose.reml contents"
echo
echo "  This is the Lisp program that replaces compose.rem."
echo "  The features demonstrated are called out in the comments."
echo
cat -n "$COMPOSE_REML"

# ── Phase 2: Full stack (requires root) ───────────────────────────
if $NO_STACK; then
    echo
    echo -e "  ${YELLOW}Skipping Phase 2 (--no-stack).${NC}"
    echo "  To run the full stack:  sudo -E $0"
    echo
    exit 0
fi

if [ "$EUID" -ne 0 ]; then
    echo
    warn "Phase 2 requires root.  Re-run with:"
    echo
    echo "  sudo -E $0"
    echo
    echo "  Or skip the stack entirely:"
    echo "  $0 --no-stack"
    echo
    exit 1
fi

log "Phase 2: full web-stack via compose.reml  (root)"
echo
echo "  Stack architecture:"
echo "    frontend (10.88.1.0/24):  proxy ←→ app"
echo "    backend  (10.88.2.0/24):           app ←→ redis"
echo
echo "  What to watch for:"
echo "    1. Two on-ready hook messages in the log:"
echo "       [lisp] redis: datastore layer ready — application tier starting"
echo "       [lisp] app: application tier healthy — proxy starting"
echo "    2. Dependency ordering enforced: redis first, then app, then proxy"
if [ "$BLOG_PORT" != "8080" ]; then
    echo "    3. Stack published on BLOG_PORT=$BLOG_PORT (env-driven config)"
fi
echo

# ── Build ──────────────────────────────────────────────────────────
log "Phase 2a: build remora (release)"
step "Running: cargo build --release"
cargo build --release --quiet
step "Binary: $REMORA"

log "Phase 2b: build web-stack images"

if ! "$REMORA" image ls 2>/dev/null | grep -q "alpine:latest"; then
    step "Pulling alpine:latest..."
    "$REMORA" image pull alpine:latest
else
    step "alpine:latest already present"
fi

for svc in redis app proxy; do
    tag="web-stack-${svc}:latest"
    if "$REMORA" image ls 2>/dev/null | grep -q "$tag"; then
        step "Image $tag already built"
    else
        step "Building $tag from $WEB_STACK_DIR/$svc/Remfile..."
        "$REMORA" build -t "web-stack-$svc" --network bridge "$WEB_STACK_DIR/$svc"
        ok "$tag built"
    fi
done

# ── Compose up ────────────────────────────────────────────────────
log "Phase 2c: compose up -f compose.reml"
echo
echo "  Starting supervisor in background.  Log output follows."
echo "  Look for the on-ready hook lines tagged [lisp]."
echo

# Capture supervisor output to a temp file so we can grep it after teardown.
LOGFILE="$(mktemp /tmp/remora-reml-test-XXXXXX.log)"
step "Log file: $LOGFILE"

BLOG_PORT="$BLOG_PORT" RUST_LOG=info \
    "$REMORA" compose up \
        -f "$COMPOSE_REML" \
        -p "$PROJECT" \
        --foreground \
    2>&1 | tee "$LOGFILE" &
COMPOSE_PID=$!

cleanup() {
    log "Teardown"
    "$REMORA" compose down -f "$COMPOSE_REML" -p "$PROJECT" -v 2>/dev/null || true
    wait "$COMPOSE_PID" 2>/dev/null || true
    step "Log saved to $LOGFILE"
}
trap cleanup EXIT

# Wait for proxy to answer on BLOG_PORT.
log "Phase 2d: waiting for stack to accept connections on port $BLOG_PORT"
READY=false
for i in $(seq 1 40); do
    if curl -s --max-time 1 "http://127.0.0.1:$BLOG_PORT/" >/dev/null 2>&1; then
        READY=true
        break
    fi
    printf "."
    sleep 1
done
echo

if ! $READY; then
    fail "Stack did not become ready within 40 seconds"
    echo
    echo "  Last 30 log lines:"
    tail -30 "$LOGFILE"
    exit 1
fi
ok "Stack is up"

# ── Verify on-ready hooks fired ────────────────────────────────────
log "Phase 2e: verify on-ready hooks fired"
echo
step "Checking log for hook messages..."

if grep -q "redis: datastore layer ready" "$LOGFILE"; then
    ok "on-ready 'redis' hook fired: $(grep 'redis: datastore layer ready' "$LOGFILE" | head -1 | sed 's/.*\[lisp\]/[lisp]/')"
else
    fail "on-ready 'redis' hook message not found in log"
fi

if grep -q "app: application tier healthy" "$LOGFILE"; then
    ok "on-ready 'app' hook fired: $(grep 'app: application tier healthy' "$LOGFILE" | head -1 | sed 's/.*\[lisp\]/[lisp]/')"
else
    fail "on-ready 'app' hook message not found in log"
fi

# ── HTTP smoke tests ───────────────────────────────────────────────
log "Phase 2f: HTTP smoke tests"
echo
CURL="curl -s --max-time 5"
BASE="http://127.0.0.1:$BLOG_PORT"

BODY=$($CURL "$BASE/" 2>/dev/null || true)
if echo "$BODY" | grep -q "Remora Blog"; then
    ok "GET /  →  contains 'Remora Blog'"
else
    fail "GET /  →  expected 'Remora Blog'"
fi

BODY=$($CURL "$BASE/health" 2>/dev/null || true)
if echo "$BODY" | grep -q '"status"'; then
    ok "GET /health  →  JSON status"
else
    fail "GET /health  →  expected JSON status, got: $BODY"
fi

BODY=$($CURL "$BASE/api/notes" 2>/dev/null || true)
if [ "$BODY" = "[]" ]; then
    ok "GET /api/notes  →  empty list"
else
    fail "GET /api/notes  →  expected [], got: $BODY"
fi

BODY=$($CURL -X POST -H 'Content-Type: application/json' \
    -d '{"text":"hello from compose.reml"}' \
    "$BASE/api/notes" 2>/dev/null || true)
if echo "$BODY" | grep -q '"ok"'; then
    ok "POST /api/notes  →  note created"
else
    fail "POST /api/notes  →  expected ok, got: $BODY"
fi

BODY=$($CURL "$BASE/api/notes" 2>/dev/null || true)
if echo "$BODY" | grep -q "hello from compose.reml"; then
    ok "GET /api/notes  →  note persisted through redis"
else
    fail "GET /api/notes  →  note not found, got: $BODY"
fi

# ── Env-driven port test ───────────────────────────────────────────
if [ "$BLOG_PORT" != "8080" ]; then
    log "Phase 2g: env-driven port check"
    ok "Stack responded on BLOG_PORT=$BLOG_PORT (not default 8080) — env() fallback working"
fi

# ── Service list ──────────────────────────────────────────────────
log "Phase 2g: service list"
echo
"$REMORA" compose ps -f "$COMPOSE_REML" -p "$PROJECT"

# ── Summary ───────────────────────────────────────────────────────
echo
echo -e "──────────────────────────────────────────────────────"
if [ "$FAILURES" -eq 0 ]; then
    echo -e "${GREEN}${BOLD}All checks passed.${NC}"
else
    echo -e "${RED}${BOLD}$FAILURES check(s) failed.${NC}"
    echo
    echo "  Full log: $LOGFILE"
    echo "  Service logs:"
    echo "    RUST_LOG=info $REMORA compose logs -f $COMPOSE_REML -p $PROJECT redis"
    echo "    RUST_LOG=info $REMORA compose logs -f $COMPOSE_REML -p $PROJECT app"
    echo "    RUST_LOG=info $REMORA compose logs -f $COMPOSE_REML -p $PROJECT proxy"
    exit 1
fi
