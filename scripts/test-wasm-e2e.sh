#!/usr/bin/env bash
# E2E tests for Wasm/WASI runtime integration.
#
# Tests the full CLI path for Wasm images: synthetic image store setup,
# `pelagos image ls` TYPE column, `pelagos run` via wasmtime, env passthrough,
# and preopened-dir (bind-mount) file access.
#
# Requirements:
#   - Must run as root (namespaces + layer store access)
#   - wasmtime must be in PATH
#   - rustc with wasm32-wasip1 target must be available (to build test module)
#
# Usage:
#   sudo -E env PATH="$HOME/.wasmtime/bin:$PATH" scripts/test-wasm-e2e.sh
set -uo pipefail

PASS=0
FAIL=0
SKIP=0
BINARY="${BINARY:-./target/debug/pelagos}"
LAYERS_DIR="/var/lib/pelagos/layers"
IMAGES_DIR="/var/lib/pelagos/images"

# ── Helpers ──────────────────────────────────────────────────────────────────

pass() { PASS=$((PASS+1)); echo "  PASS: $1"; }
fail() { FAIL=$((FAIL+1)); echo "  FAIL: $1"; }
skip() { SKIP=$((SKIP+1)); echo "  SKIP: $1"; }

has_cmd() { command -v "$1" &>/dev/null; }

check_contains() {
    local output="$1" expected="$2" label="$3"
    if echo "$output" | grep -qF "$expected"; then
        pass "$label"
    else
        fail "$label — expected '$expected' in output"
        echo "    actual output: $output"
    fi
}

check_not_contains() {
    local output="$1" unwanted="$2" label="$3"
    if echo "$output" | grep -qF "$unwanted"; then
        fail "$label — found unwanted '$unwanted' in output"
        echo "    actual output: $output"
    else
        pass "$label"
    fi
}

# ── Pre-flight checks ─────────────────────────────────────────────────────────

echo "=== Wasm E2E Tests ==="
echo ""

if [ "$(id -u)" -ne 0 ]; then
    echo "FATAL: must run as root (sudo -E scripts/test-wasm-e2e.sh)"
    exit 1
fi

if ! has_cmd wasmtime; then
    echo "SKIP: wasmtime not found in PATH — install from https://wasmtime.dev"
    exit 0
fi

WASMTIME_VER=$(wasmtime --version 2>&1)
echo "  wasmtime: $WASMTIME_VER"

if ! [ -x "$BINARY" ]; then
    echo "FATAL: pelagos binary not found at $BINARY — run 'cargo build' first"
    exit 1
fi

# ── Build a test Wasm module ──────────────────────────────────────────────────

WORK_DIR=$(mktemp -d /tmp/pelagos-wasm-e2e.XXXXXX)
trap 'cleanup_all' EXIT

TEST_IMAGE_REF="pelagos-wasm-e2e-hello"
# sanitised dirname: slashes, colons and @ become _
TEST_IMAGE_DIR="${IMAGES_DIR}/pelagos-wasm-e2e-hello"
FAKE_DIGEST="sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
FAKE_HEX="aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
TEST_LAYER_DIR="${LAYERS_DIR}/${FAKE_HEX}"

cleanup_all() {
    # Remove synthetic image + layer from the store.
    rm -rf "$TEST_IMAGE_DIR" "$TEST_LAYER_DIR" "$WORK_DIR"
}

WASM_SRC="${WORK_DIR}/hello.rs"
WASM_BIN="${WORK_DIR}/hello.wasm"

cat > "$WASM_SRC" << 'RUST'
use std::env;
use std::fs;

fn main() {
    println!("hello wasm");
    if let Ok(val) = env::var("WASM_TEST_VAR") {
        println!("env:WASM_TEST_VAR={}", val);
    }
    if let Ok(content) = fs::read_to_string("/data/test.txt") {
        print!("file:{}", content);
    }
}
RUST

echo ""
echo "--- Building test Wasm module ---"

if ! has_cmd rustc; then
    skip "rustc not found — cannot build Wasm test module"
    echo ""
    echo "Results: $PASS passed, $FAIL failed, $SKIP skipped"
    exit $([ "$FAIL" -eq 0 ] && echo 0 || echo 1)
fi

# Check that wasm32-wasip1 target is installed.
if ! rustc --print target-list 2>/dev/null | grep -q "wasm32-wasip1"; then
    # Try rustup.
    if has_cmd rustup; then
        rustup target add wasm32-wasip1 2>/dev/null || true
    fi
fi

if ! rustc --target wasm32-wasip1 --edition 2021 \
        -o "$WASM_BIN" "$WASM_SRC" 2>"${WORK_DIR}/rustc.err"; then
    echo "  SKIP: failed to compile wasm32-wasip1 module:"
    cat "${WORK_DIR}/rustc.err"
    skip "wasm32-wasip1 compilation failed"
    echo ""
    echo "Results: $PASS passed, $FAIL failed, $SKIP skipped"
    exit $([ "$FAIL" -eq 0 ] && echo 0 || echo 1)
fi

echo "  module: $WASM_BIN ($(wc -c < "$WASM_BIN") bytes)"

# Quick sanity: can wasmtime run it directly?
if ! out=$(wasmtime run "$WASM_BIN" 2>&1); then
    echo "FATAL: wasmtime cannot run the test module: $out"
    exit 1
fi
echo "  wasmtime direct: OK"

# ── Seed the pelagos image store ─────────────────────────────────────────────

echo ""
echo "--- Seeding synthetic Wasm image in pelagos store ---"

mkdir -p "$TEST_LAYER_DIR"
cp "$WASM_BIN" "${TEST_LAYER_DIR}/module.wasm"

mkdir -p "$TEST_IMAGE_DIR"
cat > "${TEST_IMAGE_DIR}/manifest.json" << JSON
{
  "reference": "${TEST_IMAGE_REF}",
  "digest": "${FAKE_DIGEST}",
  "layers": ["${FAKE_DIGEST}"],
  "layer_types": ["application/wasm"],
  "config": {
    "env": [],
    "cmd": [],
    "entrypoint": [],
    "working_dir": "",
    "healthcheck": null
  }
}
JSON

echo "  image dir: $TEST_IMAGE_DIR"
echo "  layer dir: $TEST_LAYER_DIR"

# ── Tests ─────────────────────────────────────────────────────────────────────

echo ""
echo "--- 1. pelagos image ls — TYPE column ---"

LS_OUT=$("$BINARY" image ls 2>&1)
check_contains "$LS_OUT" "wasm" "image ls shows TYPE=wasm for Wasm image"
check_contains "$LS_OUT" "$TEST_IMAGE_REF" "image ls lists the test Wasm image"

echo ""
echo "--- 2. pelagos run — basic output ---"

RUN_OUT=$(sudo -E env PATH="$PATH" "$BINARY" run "$TEST_IMAGE_REF" 2>&1)
check_contains "$RUN_OUT" "hello wasm" "run: Wasm module prints 'hello wasm'"
check_not_contains "$RUN_OUT" "error" "run: no error message"

echo ""
echo "--- 3. pelagos run — env passthrough ---"

ENV_OUT=$(sudo -E env PATH="$PATH" "$BINARY" run \
    --env WASM_TEST_VAR=testvalue42 \
    "$TEST_IMAGE_REF" 2>&1)
check_contains "$ENV_OUT" "env:WASM_TEST_VAR=testvalue42" "run: --env value reaches the Wasm module"

echo ""
echo "--- 4. pelagos run — preopened dir (--bind) ---"

BIND_DIR="${WORK_DIR}/binddata"
mkdir -p "$BIND_DIR"
echo "bind mount works" > "${BIND_DIR}/test.txt"

BIND_OUT=$(sudo -E env PATH="$PATH" "$BINARY" run \
    --bind "${BIND_DIR}:/data" \
    "$TEST_IMAGE_REF" 2>&1)
check_contains "$BIND_OUT" "file:bind mount works" "run: --bind dir visible as /data inside Wasm"

echo ""
echo "--- 5. Wasm magic-byte detection via is_wasm_binary ---"

# Verify the compiled binary has the correct magic bytes.
MAGIC=$(xxd -l4 "$WASM_BIN" 2>/dev/null | head -1 || od -An -tx1 -N4 "$WASM_BIN" 2>/dev/null | tr -d ' ')
if echo "$MAGIC" | grep -qi "00 61 73 6d\|006173 6d\|0061736d"; then
    pass "compiled hello.wasm has Wasm magic bytes (\\0asm)"
else
    # Try a simpler check: just read the raw bytes.
    FIRST4=$(dd if="$WASM_BIN" bs=1 count=4 2>/dev/null | od -An -tx1 | tr -d ' \n')
    if [ "$FIRST4" = "0061736d" ]; then
        pass "compiled hello.wasm has Wasm magic bytes (\\0asm)"
    else
        fail "Wasm magic bytes not found — got: $FIRST4"
    fi
fi

# ── Summary ───────────────────────────────────────────────────────────────────

echo ""
echo "Results: $PASS passed, $FAIL failed, $SKIP skipped"
echo ""

[ "$FAIL" -eq 0 ]
