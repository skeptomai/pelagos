#!/usr/bin/env bash
# Run opencontainers/runtime-tools conformance suite against pelagos.
# Must be run as root (sudo -E ./scripts/run-conformance.sh).
# Requires runtime-tools built at /home/cb/Projects/runtime-tools.

set -euo pipefail

RUNTIME_TOOLS=/home/cb/Projects/runtime-tools
PELAGOS=$(realpath "$(dirname "$0")/../target/debug/pelagos")
RESULTS_FILE=$(realpath "$(dirname "$0")/../rt-results.txt")

if [ ! -f "$PELAGOS" ]; then
    echo "pelagos binary not found at $PELAGOS — run 'sudo -E cargo build' first" >&2
    exit 1
fi

export PATH="$(dirname "$PELAGOS"):$PATH"
export RUNTIME=pelagos

# Run from runtime-tools dir so tests find rootfs-amd64.tar.gz and runtimetest
cd "$RUNTIME_TOOLS"

pass=0
fail=0
skip=0
declare -a failures=()

# Tests known to be permanently unsupported (cgroupv2 stub in runtime-tools)
CGROUP_SKIP_PATTERN="linux_cgroups_"

echo "=== pelagos OCI conformance ($(date)) ===" | tee "$RESULTS_FILE"
echo "" | tee -a "$RESULTS_FILE"

for testbin in validation/*/*.t; do
    name=$(basename "$(dirname "$testbin")")

    # Skip cgroup tests — runtime-tools has cgroupv2 stub that always fails
    if [[ "$name" == ${CGROUP_SKIP_PATTERN}* ]]; then
        echo "SKIP $name  (cgroupv2 not supported by runtime-tools)" | tee -a "$RESULTS_FILE"
        ((skip++)) || true
        continue
    fi

    output=$(sudo PATH="$(dirname "$PELAGOS"):$PATH" RUNTIME=pelagos "$testbin" 2>&1 || true)
    not_ok_count=$(echo "$output" | grep -c "^not ok" || true)
    ok_count=$(echo "$output" | grep -c "^ok" || true)

    if [ "$not_ok_count" -eq 0 ] && [ "$ok_count" -gt 0 ]; then
        echo "PASS $name" | tee -a "$RESULTS_FILE"
        ((pass++)) || true
    elif echo "$output" | grep -q "^1\.\.[0-9]" && [ "$ok_count" -eq 0 ] && [ "$not_ok_count" -eq 0 ]; then
        echo "SKIP $name  (no assertions)" | tee -a "$RESULTS_FILE"
        ((skip++)) || true
    else
        echo "FAIL $name" | tee -a "$RESULTS_FILE"
        echo "$output" | sed 's/^/  /' | tee -a "$RESULTS_FILE"
        failures+=("$name")
        ((fail++)) || true
    fi
done

echo "" | tee -a "$RESULTS_FILE"
echo "=== Summary ===" | tee -a "$RESULTS_FILE"
echo "PASS: $pass  FAIL: $fail  SKIP: $skip" | tee -a "$RESULTS_FILE"

if [ ${#failures[@]} -gt 0 ]; then
    echo "" | tee -a "$RESULTS_FILE"
    echo "Failed tests:" | tee -a "$RESULTS_FILE"
    for f in "${failures[@]}"; do
        echo "  - $f" | tee -a "$RESULTS_FILE"
    done
fi

echo "" | tee -a "$RESULTS_FILE"
echo "Full results saved to: $RESULTS_FILE"

[ "$fail" -eq 0 ]
