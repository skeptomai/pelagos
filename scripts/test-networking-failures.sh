#!/usr/bin/env bash
# Run the networking cleanup integration tests that failed after the PID namespace change.
# Usage: sudo -E bash scripts/test-networking-failures.sh

set -euo pipefail
cd "$(dirname "$0")/.."

tests=(
    networking::test_bridge_cleanup_after_sigkill
    networking::test_nat_cleanup
    networking::test_nat_refcount
    networking::test_port_forward_cleanup
    networking::test_port_forward_independent_teardown
)

overall=0
for t in "${tests[@]}"; do
    echo "=== $t ==="
    cargo test --test integration_tests "$t" -- --nocapture || overall=1
    echo ""
done

exit $overall
