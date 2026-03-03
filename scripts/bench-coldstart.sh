#!/usr/bin/env bash
# bench-coldstart.sh — measure pelagos container cold-start latency
#
# Usage:
#   sudo ./scripts/bench-coldstart.sh [--runs N] [--warmup N] [--compare]
#
# Requires: root (pelagos run needs namespaces), hyperfine, alpine image pulled.
#
# Outputs:
#   - Median / mean / stddev cold-start time for pelagos
#   - With --compare: also benchmarks crun and runc if available
#   - Results written to scripts/bench-results.md (appended with timestamp)
#
# The measured command is the minimal useful workload:
#   pelagos run --rm alpine /bin/true
# This exercises: image layer mount, namespace creation, cgroup setup,
# seccomp compile+load, exec, and teardown.

set -euo pipefail

PELAGOS="${PELAGOS:-$(dirname "$0")/../target/release/pelagos}"
RUNS="${RUNS:-20}"
WARMUP="${WARMUP:-3}"
COMPARE=0
OUTPUT="$(dirname "$0")/bench-results.md"

while [[ $# -gt 0 ]]; do
    case "$1" in
        --runs)    RUNS="$2";    shift 2 ;;
        --warmup)  WARMUP="$2";  shift 2 ;;
        --compare) COMPARE=1;    shift   ;;
        *) echo "Unknown arg: $1"; exit 1 ;;
    esac
done

if [[ $EUID -ne 0 ]]; then
    echo "error: must run as root (container creation requires CAP_SYS_ADMIN)"
    exit 1
fi

if ! command -v hyperfine &>/dev/null; then
    echo "error: hyperfine not found — install with: cargo install hyperfine"
    exit 1
fi

if [[ ! -x "$PELAGOS" ]]; then
    echo "error: pelagos binary not found at $PELAGOS"
    echo "       build with: cargo build --release"
    exit 1
fi

PELAGOS_VERSION="$("$PELAGOS" --version 2>/dev/null || echo unknown)"
KERNEL="$(uname -r)"
DATE="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

echo "=== pelagos cold-start benchmark ==="
echo "Binary:  $PELAGOS ($PELAGOS_VERSION)"
echo "Kernel:  $KERNEL"
echo "Runs:    $RUNS (warmup: $WARMUP)"
echo ""

CMDS=("$PELAGOS run --rm alpine /bin/true")
LABELS=("pelagos")

if [[ $COMPARE -eq 1 ]]; then
    for rt in crun runc; do
        if command -v "$rt" &>/dev/null; then
            echo "Found $rt — will include in comparison"
            # These need a pre-created OCI bundle; skip if no bundle present.
            # For now just note availability.
            echo "  (note: crun/runc require an OCI bundle — comparison not implemented yet)"
        fi
    done
fi

# Run hyperfine
hyperfine \
    --runs "$RUNS" \
    --warmup "$WARMUP" \
    --shell none \
    --export-markdown /tmp/bench-hyperfine.md \
    --export-json /tmp/bench-hyperfine.json \
    "${CMDS[@]}"

echo ""

# Extract median from JSON
if command -v python3 &>/dev/null; then
    MEDIAN_MS=$(python3 -c "
import json, sys
data = json.load(open('/tmp/bench-hyperfine.json'))
r = data['results'][0]
print(f\"{r['median']*1000:.1f} ms  (mean {r['mean']*1000:.1f} ms, stddev {r['stddev']*1000:.1f} ms, min {r['min']*1000:.1f} ms, max {r['max']*1000:.1f} ms)\")
")
    echo "Result: $MEDIAN_MS"
else
    MEDIAN_MS="(python3 not available for JSON parsing)"
fi

# Append to results file
{
    echo ""
    echo "## $DATE"
    echo ""
    echo "- **Kernel:** $KERNEL"
    echo "- **Binary:** $PELAGOS_VERSION"
    echo "- **Runs:** $RUNS (warmup: $WARMUP)"
    echo "- **Command:** \`pelagos run --rm alpine /bin/true\`"
    echo "- **Result:** $MEDIAN_MS"
    echo ""
    cat /tmp/bench-hyperfine.md
} >> "$OUTPUT"

echo ""
echo "Results appended to $OUTPUT"
