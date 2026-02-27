#!/usr/bin/env bash
#
# Imperative compose examples runner
# ====================================
# Pulls required images and runs one of the four imperative .reml examples.
#
# Usage:
#   sudo -E ./examples/compose/imperative/run.sh [example]
#
# Examples:
#   graph       compose.reml              — graph model: define-run, cascade teardown (default)
#   chain       compose-chain.reml        — monadic chain: resolve executor, manual teardown
#   sequential  compose-eager-sequential.reml  — eager sequential: container-start
#   parallel    compose-eager-parallel.reml    — eager parallel: container-start-bg/join
#
# Options:
#   REMORA=path   Override remora binary (default: auto-detect from cargo build)

set -euo pipefail
cd "$(dirname "$0")/../../.."

if [ "$EUID" -ne 0 ]; then
    echo "error: run as root: sudo -E $0 [example]" >&2
    exit 1
fi

EXAMPLE="${1:-graph}"
REMORA="${REMORA:-}"

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

CYAN='\033[0;36m'
BOLD='\033[1m'
NC='\033[0m'
log() { echo -e "\n${CYAN}==>${NC} ${BOLD}$*${NC}"; }

case "$EXAMPLE" in
    graph)
        COMPOSE_FILE="examples/compose/imperative/compose.reml"
        PROJECT="imperative-graph"
        IMAGES=("postgres:16" "redis:7-alpine" "alpine:latest")
        ;;
    chain)
        COMPOSE_FILE="examples/compose/imperative/compose-chain.reml"
        PROJECT="imperative-chain"
        IMAGES=("postgres:16" "alpine:latest")
        ;;
    sequential)
        COMPOSE_FILE="examples/compose/imperative/compose-eager-sequential.reml"
        PROJECT="imperative-seq"
        IMAGES=("postgres:16" "alpine:latest")
        ;;
    parallel)
        COMPOSE_FILE="examples/compose/imperative/compose-eager-parallel.reml"
        PROJECT="imperative-par"
        IMAGES=("postgres:16" "redis:7-alpine" "alpine:latest")
        ;;
    *)
        echo "error: unknown example '$EXAMPLE'" >&2
        echo "usage: $0 [graph|chain|sequential|parallel]" >&2
        exit 1
        ;;
esac

log "Example: $EXAMPLE  →  $COMPOSE_FILE"

# ── Pull images ────────────────────────────────────────────────────────────

log "Checking images..."
for image in "${IMAGES[@]}"; do
    if "$REMORA" image ls 2>/dev/null | grep -qF "$image"; then
        echo "  $image already present"
    else
        echo "  pulling $image..."
        "$REMORA" image pull "$image"
    fi
done

# ── Run ────────────────────────────────────────────────────────────────────

log "Running $COMPOSE_FILE (project: $PROJECT)"
echo
RUST_LOG=info "$REMORA" compose up -f "$COMPOSE_FILE" -p "$PROJECT"
