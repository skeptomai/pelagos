#!/usr/bin/env bats
# tests/e2e/hardening.bats
#
# End-to-end verification that spawn_service applies all four security
# hardening defaults (seccomp, capability drop, no-new-privs, PID namespace)
# to every container started via `remora compose up`.
#
# These tests close the gap that unit + integration tests cannot: they exercise
# the full binary path through cmd_compose_up_reml → spawn_service.
#
# Prerequisites:
#   - Run as root (sudo -E bats tests/e2e/hardening.bats)
#   - alpine:latest pulled (remora image pull alpine:latest)
#   - remora binary built (cargo build)

load helpers.bash

FIXTURE="$(dirname "$BATS_TEST_FILENAME")/fixtures/sleep-probe.reml"
PROJECT="bats-e2e-harden"

setup_file() {
    require_root
    compose_up "$FIXTURE" "$PROJECT"
}

teardown_file() {
    compose_down "$FIXTURE" "$PROJECT" 2>/dev/null || true
    # Kill background supervisor if still running
    [[ -n "${COMPOSE_PID:-}" ]] && kill "$COMPOSE_PID" 2>/dev/null || true
}

# ---------------------------------------------------------------------------
# Helper: locate the inner container process
# ---------------------------------------------------------------------------

inner_container_pid() {
    local ppid
    ppid=$(service_pid "$PROJECT" "probe")
    [[ -n "$ppid" ]] || { echo ""; return 1; }
    inner_pid "$ppid"
}

# ---------------------------------------------------------------------------
# Tests
# ---------------------------------------------------------------------------

@test "compose up starts probe service" {
    run service_pid "$PROJECT" "probe"
    [ "$status" -eq 0 ]
    [[ -n "$output" ]]
    [[ -d "/proc/$output" ]]
}

@test "probe container has seccomp-BPF filter (Seccomp: 2)" {
    local pid
    pid=$(inner_container_pid)
    [[ -n "$pid" ]] || skip "could not locate inner container pid"

    run proc_status_field "$pid" "Seccomp"
    [ "$status" -eq 0 ]
    [ "$output" = "2" ]
}

@test "probe container has all capabilities dropped (CapEff: 0)" {
    local pid
    pid=$(inner_container_pid)
    [[ -n "$pid" ]] || skip "could not locate inner container pid"

    run proc_status_field "$pid" "CapEff"
    [ "$status" -eq 0 ]
    # All 64 bits zero
    [ "$output" = "0000000000000000" ]
}

@test "probe container has no-new-privileges set (NoNewPrivs: 1)" {
    local pid
    pid=$(inner_container_pid)
    [[ -n "$pid" ]] || skip "could not locate inner container pid"

    run proc_status_field "$pid" "NoNewPrivs"
    [ "$status" -eq 0 ]
    [ "$output" = "1" ]
}

@test "probe container runs in isolated PID namespace (NSpid has 2 entries)" {
    local pid
    pid=$(inner_container_pid)
    [[ -n "$pid" ]] || skip "could not locate inner container pid"

    local nspid_line
    nspid_line=$(grep "^NSpid:" "/proc/${pid}/status" 2>/dev/null)
    # NSpid shows PID in each namespace, outermost first.
    # Two entries (host PID + inner PID) confirm PID namespace isolation.
    local count
    count=$(echo "$nspid_line" | awk '{print NF - 1}')
    [[ "$count" -ge 2 ]]
}

@test "probe container runs in isolated UTS namespace" {
    local pid
    pid=$(inner_container_pid)
    [[ -n "$pid" ]] || skip "could not locate inner container pid"

    local host_uts container_uts
    host_uts=$(readlink /proc/1/ns/uts)
    container_uts=$(readlink "/proc/${pid}/ns/uts")
    [[ "$host_uts" != "$container_uts" ]]
}
