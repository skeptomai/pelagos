#!/usr/bin/env bash
# tests/e2e/helpers.bash — shared helpers for all remora BATS suites

# ---------------------------------------------------------------------------
# Root guard
# ---------------------------------------------------------------------------

require_root() {
    if [[ $EUID -ne 0 ]]; then
        skip "requires root"
    fi
}

# ---------------------------------------------------------------------------
# Binary resolution
# ---------------------------------------------------------------------------

# Locate the remora binary.  Prefer the debug build so we don't need
# a release build; fall back to whatever is on PATH.
remora_bin() {
    local debug_bin
    debug_bin="$(git -C "$(dirname "${BASH_SOURCE[0]}")" rev-parse --show-toplevel 2>/dev/null)/target/debug/remora"
    if [[ -x "$debug_bin" ]]; then
        echo "$debug_bin"
    else
        echo "remora"
    fi
}

REMORA="$(remora_bin)"

# ---------------------------------------------------------------------------
# Compose helpers
# ---------------------------------------------------------------------------

# compose_up FILE PROJECT
# Start a project in the background; returns when the supervisor has written
# its state file (i.e. all services are started).
compose_up() {
    local file="$1" project="$2"
    "$REMORA" compose up -f "$file" -p "$project" &
    COMPOSE_PID=$!

    # Wait up to 30 s for the project state file to appear
    local deadline=$(( $(date +%s) + 30 ))
    until [[ -f "/run/remora/compose/${project}/state.json" ]] || (( $(date +%s) > deadline )); do
        sleep 0.25
    done
    if [[ ! -f "/run/remora/compose/${project}/state.json" ]]; then
        return 1
    fi
}

# compose_down FILE PROJECT
compose_down() {
    local file="$1" project="$2"
    "$REMORA" compose down -f "$file" -p "$project"
}

# service_pid PROJECT SERVICE
# Print the intermediate PID for a service from the compose state file.
service_pid() {
    local project="$1" service="$2"
    python3 -c "
import json, sys
with open('/run/remora/compose/${project}/state.json') as f:
    d = json.load(f)
print(d['services']['${service}']['pid'])
" 2>/dev/null
}

# inner_pid PARENT_PID
# Return the PID of the first child of PARENT_PID via /proc task children.
inner_pid() {
    local ppid="$1"
    local children
    children=$(cat "/proc/${ppid}/task/${ppid}/children" 2>/dev/null)
    echo "${children%% *}"
}

# proc_status_field PID FIELD
# Read a single field value from /proc/PID/status (e.g. "CapEff", "Seccomp").
proc_status_field() {
    local pid="$1" field="$2"
    grep "^${field}:" "/proc/${pid}/status" 2>/dev/null | awk '{print $2}'
}

# wait_container_up PROJECT SERVICE [TIMEOUT_SECS]
# Poll until the service PID exists in /proc (max TIMEOUT_SECS, default 30).
wait_container_up() {
    local project="$1" service="$2" timeout="${3:-30}"
    local deadline=$(( $(date +%s) + timeout ))
    local pid
    until pid=$(service_pid "$project" "$service") && [[ -n "$pid" ]] && [[ -d "/proc/$pid" ]]; do
        if (( $(date +%s) > deadline )); then
            return 1
        fi
        sleep 0.25
    done
    echo "$pid"
}
