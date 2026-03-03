# Pelagos test targets
#
# Usage:
#   make test-unit          # No root required
#   make test-integration   # Requires root — run as: sudo -E make test-integration
#   make test-e2e           # Requires root + BATS — run as: sudo -E make test-e2e
#   make test               # Unit + integration (requires root)

.PHONY: test test-unit test-integration test-e2e build

build:
	cargo build

test-unit:
	cargo test --lib

# Integration tests require root; user must invoke with sudo -E.
test-integration:
	cargo test --test integration_tests

# E2E tests require BATS and a built binary.
# Install BATS: sudo pacman -S bash-bats  (Arch)  or  sudo apt install bats  (Debian/Ubuntu)
test-e2e: build
	bats tests/e2e/hardening.bats tests/e2e/lifecycle.bats

# Run unit + integration (skips e2e so it can be run without BATS installed).
test: test-unit test-integration
