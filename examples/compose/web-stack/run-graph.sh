#!/usr/bin/env bash
#
# Web-stack — graph model runner (compose-graph.reml)
# ====================================================
# Builds the web-stack images (shared with run.sh) then runs the stack
# using the declarative future graph instead of the compose-up supervisor.
#
# Contrast with run.sh (supervisor model):
#   run.sh         — compose-up hands a ComposeFile to the CLI supervisor;
#                    manages restart policies, long-term lifecycle.
#   run-graph.sh   — run executes the future graph directly; await-port
#                    replaces depends-on; with-cleanup owns the lifecycle.
#
# Usage:
#   sudo -E ./examples/compose/web-stack/run-graph.sh
#
# Options:
#   BLOG_PORT=N    Override the published host port (default 8080)
#   REMORA=path    Override remora binary

set -euo pipefail
cd "$(dirname "$0")/../../.."

if [ "$EUID" -ne 0 ]; then
    echo "error: run as root: sudo -E $0" >&2
    exit 1
fi

SCRIPT_DIR="examples/compose/web-stack"
WEB_STACK_DIR="examples/web-stack"
BLOG_PORT="${BLOG_PORT:-8080}"
PROJECT="web-graph"
REMORA="${REMORA:-}"

CYAN='\033[0;36m'
BOLD='\033[1m'
NC='\033[0m'
log() { echo -e "\n${CYAN}==>${NC} ${BOLD}$*${NC}"; }

# Auto-detect remora binary.
if [ -z "$REMORA" ]; then
    if [ -f "./target/debug/remora" ]; then
        REMORA="./target/debug/remora"
    elif [ -f "./target/release/remora" ]; then
        REMORA="./target/release/remora"
    else
        echo "==> Building remora..."
        cargo build --bin remora
        REMORA="./target/debug/remora"
    fi
fi

# ── Pull base image ────────────────────────────────────────────────────────

log "Checking base image..."
if "$REMORA" image ls 2>/dev/null | grep -q "alpine:latest"; then
    echo "  alpine:latest already present"
else
    echo "  pulling alpine:latest..."
    "$REMORA" image pull alpine:latest
fi

# ── Build web-stack images ─────────────────────────────────────────────────

log "Building web-stack images..."
for svc in redis app proxy; do
    tag="web-stack-${svc}:latest"
    if "$REMORA" image ls 2>/dev/null | grep -q "$tag"; then
        echo "  $tag already built"
    else
        echo "  building $tag..."
        "$REMORA" build -t "web-stack-${svc}" --network bridge "$WEB_STACK_DIR/${svc}"
    fi
done

# ── Run ────────────────────────────────────────────────────────────────────

log "Running compose-graph.reml (project: $PROJECT, port: $BLOG_PORT)"
echo
BLOG_PORT="$BLOG_PORT" RUST_LOG=info \
    "$REMORA" compose up -f "$SCRIPT_DIR/compose-graph.reml" -p "$PROJECT"
