#!/bin/bash
# Start the home monitoring stack via remora compose.
#
# Pulls required images (skipping any already cached), substitutes
# the Plex token from the environment or .env file, and brings up
# prometheus + grafana + snmp-exporter + plex-exporter.
#
# Usage:
#   sudo -E ./scripts/start-monitoring.sh
#   sudo -E PLEX_TOKEN=xxx ./scripts/start-monitoring.sh
#
# Options:
#   --foreground    Run compose in foreground (default: background)
#   --down          Stop and remove the stack instead of starting it
#   --down-volumes  Stop and remove the stack, including grafana data volume

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
MONITORING_ROOT="$HOME/Projects/home-monitoring"
COMPOSE_FILE="$MONITORING_ROOT/remora/compose.rem"
ENV_FILE="$MONITORING_ROOT/monitoring-setup/.env"

# Stable project name — keeps network/volume names consistent across runs.
PROJECT="home-monitoring"

# Fixed (non-random) resolved compose file — lives alongside the real one so
# relative bind-mount paths (./config/...) resolve correctly.
RESOLVED_COMPOSE="$MONITORING_ROOT/remora/.compose-resolved.rem"

FOREGROUND=false
DOWN=false
DOWN_VOLUMES=false

for arg in "$@"; do
    case "$arg" in
        --foreground)   FOREGROUND=true ;;
        --down)         DOWN=true ;;
        --down-volumes) DOWN=true; DOWN_VOLUMES=true ;;
        *) echo "Unknown option: $arg"; exit 1 ;;
    esac
done

# ── Helpers ────────────────────────────────────────────────────────────────────

info()  { echo "==> $*"; }
warn()  { echo "    [warn] $*" >&2; }
die()   { echo "error: $*" >&2; exit 1; }

# ── Root check ─────────────────────────────────────────────────────────────────

if [ "$EUID" -ne 0 ]; then
    die "This script must be run as root. Use: sudo -E $0 $*"
fi

# ── Build remora from source ───────────────────────────────────────────────────
# Always build so the binary matches the source tree the script lives in.
# `cargo build --release` is a no-op when nothing has changed.

info "Building remora..."
cd "$REPO_ROOT"
cargo build --release 2>&1
REMORA="$REPO_ROOT/target/release/remora"
info "Using remora: $REMORA"

# ── Down path ──────────────────────────────────────────────────────────────────

if $DOWN; then
    info "Stopping monitoring stack..."
    if $DOWN_VOLUMES; then
        "$REMORA" compose down -f "$COMPOSE_FILE" -p "$PROJECT" --volumes
    else
        "$REMORA" compose down -f "$COMPOSE_FILE" -p "$PROJECT"
    fi
    rm -f "$RESOLVED_COMPOSE"
    info "Done."
    exit 0
fi

# ── Compose file check ─────────────────────────────────────────────────────────

[ -f "$COMPOSE_FILE" ] || die "compose file not found: $COMPOSE_FILE"

# ── Plex token ─────────────────────────────────────────────────────────────────

# Resolve in order: env var → .env file → warn and leave placeholder.
if [ -z "${PLEX_TOKEN:-}" ] && [ -f "$ENV_FILE" ]; then
    PLEX_TOKEN="$(grep '^PLEX_TOKEN=' "$ENV_FILE" 2>/dev/null | cut -d= -f2- | tr -d '"' || true)"
fi

if [ -z "${PLEX_TOKEN:-}" ] || [ "$PLEX_TOKEN" = "YOUR_PLEX_TOKEN_HERE" ]; then
    warn "PLEX_TOKEN not set — plex-exporter will start but metrics will fail."
    warn "Set it with: sudo -E PLEX_TOKEN=yourtoken $0"
    PLEX_TOKEN="YOUR_PLEX_TOKEN_HERE"
fi

# Write the resolved compose file next to the original so ./config/... paths work.
trap 'rm -f "$RESOLVED_COMPOSE"' EXIT
sed "s/YOUR_PLEX_TOKEN_HERE/$PLEX_TOKEN/" "$COMPOSE_FILE" > "$RESOLVED_COMPOSE"

# ── Clean up any leftover state from previous runs ─────────────────────────────

info "Cleaning up any previous stack state..."
"$REMORA" compose down -f "$RESOLVED_COMPOSE" -p "$PROJECT" 2>/dev/null || true

# Also remove any orphaned networks that use the same subnet — these are left
# behind when a previous run crashed before compose down could clean up.
SUBNET_ADDR="172.20.0.0"
SUBNET_PREFIX="24"
for cfg in /var/lib/remora/networks/*/config.json; do
    [ -f "$cfg" ] || continue
    net_name="$(basename "$(dirname "$cfg")")"
    if grep -q "\"addr\": \"${SUBNET_ADDR}\"" "$cfg" 2>/dev/null && \
       grep -q "\"prefix_len\": ${SUBNET_PREFIX}" "$cfg" 2>/dev/null; then
        info "  removing stale network '$net_name' (${SUBNET_ADDR}/${SUBNET_PREFIX})..."
        "$REMORA" network rm "$net_name" 2>/dev/null || true
    fi
done

# ── Image pull ─────────────────────────────────────────────────────────────────

IMAGES=(
    "prom/snmp-exporter:v0.21.0"
    "ghcr.io/axsuul/plex-media-server-exporter:latest"
    "ghcr.io/akpw/mktxp:latest"
    "prom/graphite-exporter:latest"
    "prom/prometheus:latest"
    "grafana/grafana:latest"
)

info "Pulling images (already-cached images will be skipped)..."
for img in "${IMAGES[@]}"; do
    info "  pull $img"
    "$REMORA" image pull "$img" || warn "pull failed for $img — will try to use cached version"
done

# ── Pre-create volumes with correct ownership ──────────────────────────────────
# Grafana runs as UID 472 — the named volume must be owned by that user.

GRAFANA_VOL_DIR="/var/lib/remora/volumes/${PROJECT}-grafana-data"
if [ ! -d "$GRAFANA_VOL_DIR" ]; then
    info "Creating grafana data volume..."
    "$REMORA" volume create "${PROJECT}-grafana-data" 2>/dev/null || true
fi
if [ -d "$GRAFANA_VOL_DIR" ]; then
    chown -R 472:472 "$GRAFANA_VOL_DIR"
fi

# ── Start compose ──────────────────────────────────────────────────────────────

info "Starting monitoring stack..."
if $FOREGROUND; then
    "$REMORA" compose up -f "$RESOLVED_COMPOSE" -p "$PROJECT" --foreground
else
    "$REMORA" compose up -f "$RESOLVED_COMPOSE" -p "$PROJECT"
    echo ""
    echo "Stack is running. Useful commands:"
    echo "  sudo -E $REMORA compose ps   -f $COMPOSE_FILE -p $PROJECT"
    echo "  sudo -E $REMORA compose logs -f $COMPOSE_FILE -p $PROJECT --follow"
    echo "  sudo -E $0 --down"
    echo ""
    echo "  Grafana:    http://localhost:3000  (admin / prom-operator)"
    echo "  Prometheus: http://localhost:9090"
fi
