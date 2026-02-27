#!/usr/bin/env bats
# tests/e2e/lifecycle.bats
#
# End-to-end tests for the compose up / ps / down lifecycle.
#
# Tests that compose up starts services, ps reports them as running,
# and down stops them cleanly.
#
# Prerequisites:
#   - Run as root (sudo -E bats tests/e2e/lifecycle.bats)
#   - alpine:latest pulled (remora image pull alpine:latest)
#   - remora binary built (cargo build)

load helpers.bash

FIXTURE="$(dirname "$BATS_TEST_FILENAME")/fixtures/sleep-probe.reml"
PROJECT="bats-e2e-lifecycle"

setup_file() {
    require_root
    compose_up "$FIXTURE" "$PROJECT"
}

teardown_file() {
    compose_down "$FIXTURE" "$PROJECT" 2>/dev/null || true
    [[ -n "${COMPOSE_PID:-}" ]] && kill "$COMPOSE_PID" 2>/dev/null || true
}

# ---------------------------------------------------------------------------
# Tests
# ---------------------------------------------------------------------------

@test "compose up writes project state file" {
    [[ -f "/run/remora/compose/${PROJECT}/state.json" ]]
}

@test "compose ps shows probe as running" {
    run "$REMORA" compose ps -f "$FIXTURE" -p "$PROJECT"
    [ "$status" -eq 0 ]
    [[ "$output" =~ "probe" ]]
    [[ "$output" =~ "running" ]]
}

@test "compose ps header contains SERVICE CONTAINER STATUS PID columns" {
    run "$REMORA" compose ps -f "$FIXTURE" -p "$PROJECT"
    [ "$status" -eq 0 ]
    [[ "$output" =~ "SERVICE" ]]
    [[ "$output" =~ "CONTAINER" ]]
    [[ "$output" =~ "STATUS" ]]
    [[ "$output" =~ "PID" ]]
}

@test "probe container name is scoped as PROJECT-SERVICE" {
    run "$REMORA" compose ps -f "$FIXTURE" -p "$PROJECT"
    [ "$status" -eq 0 ]
    [[ "$output" =~ "${PROJECT}-probe" ]]
}

@test "probe PID is alive in /proc" {
    local pid
    pid=$(service_pid "$PROJECT" "probe")
    [[ -n "$pid" ]]
    [[ -d "/proc/$pid" ]]
}

@test "compose down stops the probe container" {
    # Down the project and verify the state file is removed
    run compose_down "$FIXTURE" "$PROJECT"
    [ "$status" -eq 0 ]

    # Wait a moment for cleanup
    sleep 1

    # State file should be gone
    [[ ! -f "/run/remora/compose/${PROJECT}/state.json" ]]
}
