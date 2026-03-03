#!/usr/bin/env bash
# E2E registry auth tests against multiple OCI registries.
#
# Tests GHCR, Docker Hub, and AWS ECR — each in both private and public
# visibility variants.  The private suite asserts that pull without
# credentials fails after logout; the public suite asserts that it succeeds.
#
# Credentials are loaded from scripts/e2e-creds.env (gitignored) and/or
# environment variables.  See scripts/e2e-creds.env.example for the full
# list of variables.
#
# Usage:
#   # Load from creds file (GHCR token must be pre-expanded — see below):
#   PELAGOS_E2E_GHCR_TOKEN=$(gh auth token) \
#   CREDS_FILE=scripts/.env \
#   sudo -E scripts/test-registry-auth-e2e.sh
#
#   # Run only specific profiles:
#   PELAGOS_E2E_REGISTRIES="ghcr-private dockerhub-public" \
#   PELAGOS_E2E_GHCR_TOKEN=$(gh auth token) \
#   CREDS_FILE=scripts/.env \
#   sudo -E scripts/test-registry-auth-e2e.sh
#
# Note on GHCR tokens: $(gh auth token) must be expanded BEFORE sudo because
# gh stores its token in the user's home directory, which root cannot access.
# Pass it as an environment variable on the command line, not in the creds file.
#
# Must run as root (overlay + port forwarding require CAP_SYS_ADMIN).
set -uo pipefail

BINARY="${BINARY:-./target/debug/pelagos}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CREDS_FILE="${CREDS_FILE:-${SCRIPT_DIR}/e2e-creds.env}"

# Load credentials file if it exists.
if [[ -f "$CREDS_FILE" ]]; then
    echo "Loading credentials from $CREDS_FILE"
    # shellcheck source=/dev/null
    source "$CREDS_FILE"
fi

# ---------------------------------------------------------------------------
# Pre-flight checks
# ---------------------------------------------------------------------------

if [[ ! -x "$BINARY" ]]; then
    echo "ERROR: pelagos binary not found at $BINARY — run 'cargo build' first."
    exit 1
fi

if [[ $(id -u) -ne 0 ]]; then
    echo "ERROR: this script must run as root."
    echo "       sudo -E scripts/test-registry-auth-e2e.sh"
    exit 1
fi

# ---------------------------------------------------------------------------
# Result tracking
# ---------------------------------------------------------------------------

TOTAL_PASS=0
TOTAL_FAIL=0
TOTAL_SKIP=0
declare -a REGISTRY_RESULTS

# Per-suite counters — reset at the start of each run_* call.
_PASS=0
_FAIL=0
_SKIP=0

_pass() { _PASS=$((_PASS+1)); echo "  PASS: $1"; }
_fail() { _FAIL=$((_FAIL+1)); echo "  FAIL: $1"; }
_skip() { _SKIP=$((_SKIP+1)); echo "  SKIP: $1"; }

check_ok() {
    local label="$1"; shift
    if "$@" >/dev/null 2>&1; then
        _pass "$label"
    else
        _fail "$label (expected success, got non-zero from: $*)"
    fi
}

check_fail() {
    local label="$1"; shift
    if "$@" >/dev/null 2>&1; then
        _fail "$label (expected failure, got success from: $*)"
    else
        _pass "$label"
    fi
}

check_output_contains() {
    local label="$1" expected="$2"; shift 2
    local out
    out=$("$@" 2>&1) || true
    if echo "$out" | grep -q "$expected"; then
        _pass "$label"
    else
        _fail "$label (expected '$expected' in output)"
        echo "    output: $out"
    fi
}

# ---------------------------------------------------------------------------
# Private image test suite
#
# All 8 tests.  Test 8: pull after logout must FAIL (auth required).
#
# Arguments: profile  registry  user  token  image
# ---------------------------------------------------------------------------

run_private_suite() {
    local profile="$1" registry="$2" user="$3" token="$4" image="$5"

    _PASS=0; _FAIL=0; _SKIP=0

    echo ""
    echo "================================================================"
    echo "  Profile  : $profile  [private]"
    echo "  Registry : $registry"
    echo "  Image    : $image"
    echo "  User     : $user"
    echo "================================================================"

    local tmphome
    tmphome=$(mktemp -d /tmp/pelagos-e2e-home.XXXXXX)

    echo ""
    echo "--- Setup: ensure alpine available ---"
    if ! "$BINARY" image pull alpine >/dev/null 2>&1; then
        echo "  WARNING: could not pull alpine"
    else
        echo "  alpine ready"
    fi

    echo ""
    echo "--- Test 1: push without credentials must fail ---"
    check_fail "anon push to $image (expect 401/403)" \
        env HOME="$tmphome" "$BINARY" image push alpine --dest "$image"

    echo ""
    echo "--- Test 2: pelagos image login ---"
    check_output_contains "login prints 'Login Succeeded'" "Login Succeeded" \
        env HOME="$tmphome" "$BINARY" image login \
            --username "$user" --password-stdin "$registry" <<<"$token"
    if [[ -f "$tmphome/.docker/config.json" ]]; then
        _pass "~/.docker/config.json written"
    else
        _fail "~/.docker/config.json not found after login"
    fi

    echo ""
    echo "--- Test 3: push via docker config ---"
    local push_out
    push_out=$(HOME="$tmphome" "$BINARY" image push alpine --dest "$image" 2>&1) || true
    if echo "$push_out" | grep -q "Pushed"; then
        _pass "push succeeded"
    else
        _fail "push: 'Pushed' not found in output"
        echo "    output: $push_out"
    fi

    echo ""
    echo "--- Test 4: pull back from registry ---"
    HOME="$tmphome" "$BINARY" image rm "$image" >/dev/null 2>&1 || true
    local pull_out
    pull_out=$(HOME="$tmphome" "$BINARY" image pull "$image" 2>&1) || true
    if echo "$pull_out" | grep -q "Done:"; then
        _pass "pull from registry succeeded"
    else
        _fail "pull: 'Done:' not found in output"
        echo "    output: $pull_out"
    fi
    check_output_contains "image appears in 'pelagos image ls'" "$registry" \
        env HOME="$tmphome" "$BINARY" image ls

    echo ""
    echo "--- Test 5: PELAGOS_REGISTRY_USER / PELAGOS_REGISTRY_PASS env fallback ---"
    HOME="$tmphome" "$BINARY" image rm "$image" >/dev/null 2>&1 || true
    local pull_env
    pull_env=$(HOME="$tmphome" \
        PELAGOS_REGISTRY_USER="$user" PELAGOS_REGISTRY_PASS="$token" \
        "$BINARY" image pull "$image" 2>&1) || true
    if echo "$pull_env" | grep -q "Done:"; then
        _pass "env-var auth pull succeeded"
    else
        _fail "env-var auth pull failed"
        echo "    output: $pull_env"
    fi

    echo ""
    echo "--- Test 6: --username / --password CLI flags ---"
    HOME="$tmphome" "$BINARY" image rm "$image" >/dev/null 2>&1 || true
    local pull_cli
    pull_cli=$(HOME="$tmphome" "$BINARY" image pull "$image" \
        --username "$user" --password "$token" 2>&1) || true
    if echo "$pull_cli" | grep -q "Done:"; then
        _pass "CLI flag auth pull succeeded"
    else
        _fail "CLI flag auth pull failed"
        echo "    output: $pull_cli"
    fi

    echo ""
    echo "--- Test 7: pelagos image logout ---"
    check_ok "logout succeeds" \
        env HOME="$tmphome" "$BINARY" image logout "$registry"
    if [[ -f "$tmphome/.docker/config.json" ]] && \
       grep -q "$registry" "$tmphome/.docker/config.json" 2>/dev/null; then
        _fail "registry entry still present in config.json after logout"
    else
        _pass "registry entry removed from config.json"
    fi

    echo ""
    echo "--- Test 8 [private]: pull after logout must FAIL ---"
    HOME="$tmphome" "$BINARY" image rm "$image" >/dev/null 2>&1 || true
    check_fail "pull $image after logout (expect 401/403)" \
        env HOME="$tmphome" "$BINARY" image pull "$image"

    HOME="$tmphome" "$BINARY" image rm "$image" >/dev/null 2>&1 || true
    rm -rf "$tmphome"

    _record_results "$profile"
}

# ---------------------------------------------------------------------------
# Public image test suite
#
# Same as private except test 8: pull after logout must SUCCEED.
#
# Prerequisites: the repository must already be configured as public on the
# registry before running this suite.  The first push makes the image
# available; visibility is controlled by repository settings, not by pelagos.
#
# Arguments: profile  registry  user  token  image
# ---------------------------------------------------------------------------

run_public_suite() {
    local profile="$1" registry="$2" user="$3" token="$4" image="$5"

    _PASS=0; _FAIL=0; _SKIP=0

    echo ""
    echo "================================================================"
    echo "  Profile  : $profile  [public]"
    echo "  Registry : $registry"
    echo "  Image    : $image"
    echo "  User     : $user"
    echo "================================================================"

    local tmphome
    tmphome=$(mktemp -d /tmp/pelagos-e2e-home.XXXXXX)

    echo ""
    echo "--- Setup: ensure alpine available ---"
    if ! "$BINARY" image pull alpine >/dev/null 2>&1; then
        echo "  WARNING: could not pull alpine"
    else
        echo "  alpine ready"
    fi

    echo ""
    echo "--- Test 1: push without credentials must fail ---"
    check_fail "anon push to $image (expect 401/403 — push always requires auth)" \
        env HOME="$tmphome" "$BINARY" image push alpine --dest "$image"

    echo ""
    echo "--- Test 2: pelagos image login ---"
    check_output_contains "login prints 'Login Succeeded'" "Login Succeeded" \
        env HOME="$tmphome" "$BINARY" image login \
            --username "$user" --password-stdin "$registry" <<<"$token"
    if [[ -f "$tmphome/.docker/config.json" ]]; then
        _pass "~/.docker/config.json written"
    else
        _fail "~/.docker/config.json not found after login"
    fi

    echo ""
    echo "--- Test 3: push via docker config ---"
    local push_out
    push_out=$(HOME="$tmphome" "$BINARY" image push alpine --dest "$image" 2>&1) || true
    if echo "$push_out" | grep -q "Pushed"; then
        _pass "push succeeded"
    else
        _fail "push: 'Pushed' not found in output"
        echo "    output: $push_out"
    fi

    echo ""
    echo "--- Test 4: pull back from registry ---"
    HOME="$tmphome" "$BINARY" image rm "$image" >/dev/null 2>&1 || true
    local pull_out
    pull_out=$(HOME="$tmphome" "$BINARY" image pull "$image" 2>&1) || true
    if echo "$pull_out" | grep -q "Done:"; then
        _pass "pull from registry succeeded"
    else
        _fail "pull: 'Done:' not found in output"
        echo "    output: $pull_out"
    fi
    check_output_contains "image appears in 'pelagos image ls'" "$registry" \
        env HOME="$tmphome" "$BINARY" image ls

    echo ""
    echo "--- Test 5: PELAGOS_REGISTRY_USER / PELAGOS_REGISTRY_PASS env fallback ---"
    HOME="$tmphome" "$BINARY" image rm "$image" >/dev/null 2>&1 || true
    local pull_env
    pull_env=$(HOME="$tmphome" \
        PELAGOS_REGISTRY_USER="$user" PELAGOS_REGISTRY_PASS="$token" \
        "$BINARY" image pull "$image" 2>&1) || true
    if echo "$pull_env" | grep -q "Done:"; then
        _pass "env-var auth pull succeeded"
    else
        _fail "env-var auth pull failed"
        echo "    output: $pull_env"
    fi

    echo ""
    echo "--- Test 6: --username / --password CLI flags ---"
    HOME="$tmphome" "$BINARY" image rm "$image" >/dev/null 2>&1 || true
    local pull_cli
    pull_cli=$(HOME="$tmphome" "$BINARY" image pull "$image" \
        --username "$user" --password "$token" 2>&1) || true
    if echo "$pull_cli" | grep -q "Done:"; then
        _pass "CLI flag auth pull succeeded"
    else
        _fail "CLI flag auth pull failed"
        echo "    output: $pull_cli"
    fi

    echo ""
    echo "--- Test 7: pelagos image logout ---"
    check_ok "logout succeeds" \
        env HOME="$tmphome" "$BINARY" image logout "$registry"
    if [[ -f "$tmphome/.docker/config.json" ]] && \
       grep -q "$registry" "$tmphome/.docker/config.json" 2>/dev/null; then
        _fail "registry entry still present in config.json after logout"
    else
        _pass "registry entry removed from config.json"
    fi

    echo ""
    echo "--- Test 8 [public]: pull after logout must SUCCEED ---"
    HOME="$tmphome" "$BINARY" image rm "$image" >/dev/null 2>&1 || true
    local pull_anon
    pull_anon=$(HOME="$tmphome" "$BINARY" image pull "$image" 2>&1) || true
    if echo "$pull_anon" | grep -q "Done:"; then
        _pass "anonymous pull succeeded (public image accessible without creds)"
    else
        _fail "anonymous pull failed — is the repository set to public?"
        echo "    output: $pull_anon"
    fi

    HOME="$tmphome" "$BINARY" image rm "$image" >/dev/null 2>&1 || true
    rm -rf "$tmphome"

    _record_results "$profile"
}

_record_results() {
    local profile="$1"
    TOTAL_PASS=$((TOTAL_PASS + _PASS))
    TOTAL_FAIL=$((TOTAL_FAIL + _FAIL))
    TOTAL_SKIP=$((TOTAL_SKIP + _SKIP))
    REGISTRY_RESULTS+=("$(printf "%-22s PASS=%-3d FAIL=%-3d SKIP=%d" \
        "$profile" "$_PASS" "$_FAIL" "$_SKIP")")
    echo ""
    echo "  [$profile] PASS=$_PASS  FAIL=$_FAIL  SKIP=$_SKIP"
}

_skip_profile() {
    local profile="$1" reason="$2"
    echo ""
    echo "SKIP: $profile — $reason"
    TOTAL_SKIP=$((TOTAL_SKIP+1))
    REGISTRY_RESULTS+=("$(printf "%-22s SKIPPED (%s)" "$profile" "$reason")")
}

# ---------------------------------------------------------------------------
# Profile runners
# ---------------------------------------------------------------------------

run_ghcr_private() {
    local user="${PELAGOS_E2E_GHCR_USER:-}"
    local token="${PELAGOS_E2E_GHCR_TOKEN:-}"
    local image="${PELAGOS_E2E_GHCR_PRIVATE_IMAGE:-}"
    [[ -z "$user" || -z "$token" || -z "$image" ]] && {
        _skip_profile "ghcr-private" "PELAGOS_E2E_GHCR_USER / _TOKEN / _PRIVATE_IMAGE not set"
        return
    }
    run_private_suite "ghcr-private" "ghcr.io" "$user" "$token" "$image"
}

run_ghcr_public() {
    local user="${PELAGOS_E2E_GHCR_USER:-}"
    local token="${PELAGOS_E2E_GHCR_TOKEN:-}"
    local image="${PELAGOS_E2E_GHCR_PUBLIC_IMAGE:-}"
    [[ -z "$user" || -z "$token" || -z "$image" ]] && {
        _skip_profile "ghcr-public" "PELAGOS_E2E_GHCR_USER / _TOKEN / _PUBLIC_IMAGE not set"
        return
    }
    run_public_suite "ghcr-public" "ghcr.io" "$user" "$token" "$image"
}

run_dockerhub_private() {
    local user="${PELAGOS_E2E_DOCKERHUB_USER:-}"
    local token="${PELAGOS_E2E_DOCKERHUB_TOKEN:-}"
    local image="${PELAGOS_E2E_DOCKERHUB_PRIVATE_IMAGE:-}"
    [[ -z "$user" || -z "$token" || -z "$image" ]] && {
        _skip_profile "dockerhub-private" "PELAGOS_E2E_DOCKERHUB_USER / _TOKEN / _PRIVATE_IMAGE not set"
        return
    }
    run_private_suite "dockerhub-private" "docker.io" "$user" "$token" "$image"
}

run_dockerhub_public() {
    local user="${PELAGOS_E2E_DOCKERHUB_USER:-}"
    local token="${PELAGOS_E2E_DOCKERHUB_TOKEN:-}"
    local image="${PELAGOS_E2E_DOCKERHUB_PUBLIC_IMAGE:-}"
    [[ -z "$user" || -z "$token" || -z "$image" ]] && {
        _skip_profile "dockerhub-public" "PELAGOS_E2E_DOCKERHUB_USER / _TOKEN / _PUBLIC_IMAGE not set"
        return
    }
    run_public_suite "dockerhub-public" "docker.io" "$user" "$token" "$image"
}

run_ecr() {
    local registry="${PELAGOS_E2E_ECR_REGISTRY:-}"
    local region="${PELAGOS_E2E_ECR_REGION:-}"
    local image="${PELAGOS_E2E_ECR_IMAGE:-}"
    [[ -z "$registry" || -z "$region" || -z "$image" ]] && {
        _skip_profile "ecr" "PELAGOS_E2E_ECR_REGISTRY / _REGION / _IMAGE not set"
        return
    }
    if ! command -v aws >/dev/null 2>&1; then
        _skip_profile "ecr" "aws CLI not found in PATH"
        return
    fi
    echo ""
    echo "--- ECR: fetching temporary login token ---"
    local token
    if ! token=$(aws ecr get-login-password --region "$region" 2>&1); then
        _skip_profile "ecr" "aws ecr get-login-password failed: $token"
        return
    fi
    echo "  token obtained (${#token} bytes)"
    run_private_suite "ecr" "$registry" "AWS" "$token" "$image"
}

# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

REGISTRIES="${PELAGOS_E2E_REGISTRIES:-ghcr-private ghcr-public dockerhub-private dockerhub-public ecr}"

echo "=== Pelagos registry auth E2E ==="
echo "  Binary     : $BINARY"
echo "  Creds file : $CREDS_FILE ($([ -f "$CREDS_FILE" ] && echo 'loaded' || echo 'not found'))"
echo "  Profiles   : $REGISTRIES"

for profile in $REGISTRIES; do
    case "$profile" in
        ghcr-private)      run_ghcr_private ;;
        ghcr-public)       run_ghcr_public ;;
        dockerhub-private) run_dockerhub_private ;;
        dockerhub-public)  run_dockerhub_public ;;
        ecr)               run_ecr ;;
        *)
            echo ""
            echo "WARN: unknown profile '$profile'"
            echo "      supported: ghcr-private ghcr-public dockerhub-private dockerhub-public ecr"
            ;;
    esac
done

# ---------------------------------------------------------------------------
# Summary
# ---------------------------------------------------------------------------

echo ""
echo "================================================================"
echo "  Final Results"
echo "================================================================"
for r in "${REGISTRY_RESULTS[@]}"; do
    echo "  $r"
done
echo ""
echo "  TOTAL  PASS=$TOTAL_PASS  FAIL=$TOTAL_FAIL  SKIP=$TOTAL_SKIP"
echo ""

[[ $TOTAL_FAIL -gt 0 ]] && exit 1
exit 0
