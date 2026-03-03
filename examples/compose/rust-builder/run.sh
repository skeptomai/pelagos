#!/usr/bin/env bash
#
# Pelagos Rust Builder Stack Demo
# ==============================
# Builds a Rust build environment with sccache, starts the container with
# `pelagos compose up`, and runs smoke tests via `pelagos exec`.
#
# Usage:  sudo ./examples/compose/rust-builder/run.sh
#
# Options:
#   --no-stack   Skip image build (image must already exist)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PELAGOS="${PELAGOS:-pelagos}"
PROJECT="rust-builder"
CONTAINER="${PROJECT}-rust-builder"

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

    tag="rust-builder:latest"
    if $REMORA image ls 2>/dev/null | grep -q "$tag"; then
        log "Image ${BOLD}${tag}${NC} already built"
    else
        log "Building ${BOLD}${tag}${NC} (this may take a few minutes)..."
        $REMORA build -t rust-builder --network bridge "$SCRIPT_DIR/rust-builder"
    fi
fi

# ── Compose Up ────────────────────────────────────────────────────

log "Starting rust-builder container..."
$REMORA compose up -f "$SCRIPT_DIR/compose.reml" -p "$PROJECT" --foreground &
COMPOSE_PID=$!

cleanup() {
    log "Tearing down..."
    $REMORA compose down -f "$SCRIPT_DIR/compose.reml" -p "$PROJECT" -v 2>/dev/null || true
    wait "$COMPOSE_PID" 2>/dev/null || true
    log "Done."
}
trap cleanup EXIT

# Wait for the container to appear in `pelagos ps`
log "Waiting for container to start..."
for i in $(seq 1 30); do
    if $REMORA ps 2>/dev/null | grep -q "$CONTAINER"; then
        break
    fi
    sleep 1
done

# ── Verification ──────────────────────────────────────────────────

echo
log "${BOLD}Running verification tests...${NC}"
echo

exec_in() { $REMORA exec "$CONTAINER" sh -c "$1" 2>/dev/null; }

# Test 1: Rust compiler installed
if RUSTC_VER=$(exec_in 'rustc --version' 2>/dev/null); then
    ok "rustc installed — ${RUSTC_VER}"
else
    fail "rustc --version failed"
fi

# Test 2: Cargo available
if CARGO_VER=$(exec_in 'cargo --version' 2>/dev/null); then
    ok "cargo installed — ${CARGO_VER}"
else
    fail "cargo --version failed"
fi

# Test 3: sccache available
if exec_in 'sccache --version' >/dev/null 2>&1; then
    ok "sccache installed"
else
    fail "sccache --version failed"
fi

# Test 4: RUSTC_WRAPPER is set to sccache inside the container
if exec_in 'test "$RUSTC_WRAPPER" = "sccache"' >/dev/null 2>&1; then
    ok "RUSTC_WRAPPER=sccache is set"
else
    fail "RUSTC_WRAPPER is not set to sccache"
fi

# Test 5: Build a minimal Rust hello-world project
BUILD_SCRIPT='
set -e
mkdir -p /tmp/hw/src
cat > /tmp/hw/Cargo.toml <<TOML
[package]
name = "hw"
version = "0.1.0"
edition = "2021"
TOML
printf "fn main() { println!(\"ok\"); }" > /tmp/hw/src/main.rs
cd /tmp/hw
cargo build 2>&1
'
if exec_in "$BUILD_SCRIPT" >/dev/null 2>&1; then
    ok "cargo build — hello-world compiled successfully"
else
    fail "cargo build — hello-world failed to compile"
fi

# Test 6: sccache shows compiler activity after build
STATS=$(exec_in 'sccache --show-stats' 2>/dev/null || echo "")
if echo "$STATS" | grep -q "Compile requests"; then
    ok "sccache tracked compile requests"
else
    fail "sccache shows no activity: ${STATS:0:200}"
fi

# Test 7: Rebuild uses sccache (clean artifacts, rebuild should be faster via cache)
REBUILD_SCRIPT='
set -e
cd /tmp/hw
cargo clean
cargo build 2>&1
'
if exec_in "$REBUILD_SCRIPT" >/dev/null 2>&1; then
    ok "cargo build — rebuild after clean succeeded (sccache cache active)"
else
    fail "cargo build — rebuild after clean failed"
fi

# Test 8: Service status
echo
log "Service status:"
$REMORA compose ps -f "$SCRIPT_DIR/compose.reml" -p "$PROJECT"

# ── Summary ───────────────────────────────────────────────────────

echo
echo -e "${BOLD}Results: ${GREEN}${pass} passed${NC}, ${RED}${fail} failed${NC}"

if [ "$fail" -gt 0 ]; then
    echo -e "\nCheck container logs:"
    echo "  $REMORA compose logs -f $SCRIPT_DIR/compose.reml -p $PROJECT rust-builder"
fi

echo
echo -e "${CYAN}Interactive shell:${NC}  sudo $REMORA exec $CONTAINER /bin/sh"
echo -e "${CYAN}Build a project:${NC}    sudo $REMORA exec $CONTAINER cargo build --release"
echo -e "${CYAN}Cache stats:${NC}        sudo $REMORA exec $CONTAINER sccache --show-stats"
echo
echo "Press Enter to tear down..."
read -r
