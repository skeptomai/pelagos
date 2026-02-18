#!/bin/bash
# Integration test runner for remora
#
# Most tests require root privileges to create namespaces and perform
# privileged operations like chroot, capability management, etc.
#
# Usage:
#   sudo -E ./run-integration-tests.sh           # Run all tests
#   sudo -E ./run-integration-tests.sh <name>    # Run specific test

set -e
cd "$(dirname "$0")/.." || exit 1

# Check if running as root
if [ "$EUID" -ne 0 ]; then
    echo "Error: Integration tests require root privileges"
    echo "Please run with: sudo -E ./run-integration-tests.sh"
    exit 1
fi

echo "==> Running remora integration tests"
echo "==> Note: Some tests may be skipped if not running as root"
echo ""

if [ -z "$1" ]; then
    # Run all tests
    cargo test --test integration_tests -- --nocapture --test-threads=1
else
    # Run specific test
    cargo test --test integration_tests "$1" -- --nocapture --test-threads=1
fi

echo ""
echo "==> Integration tests complete!"
