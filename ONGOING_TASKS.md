# Ongoing Tasks

All work is tracked in GitHub Issues. This file is a brief index.

## Open Issues

| # | Title | Kind |
|---|-------|------|
| #47 | track: runtime-tools pidfile.t kill-on-stopped bug (upstream) | upstream |
| #48 | track: runtime-tools process_rlimits broken by Go 1.19+ (upstream) | upstream |
| #49 | track: runtime-tools delete tests hardcoded for cgroupv1 (upstream) | upstream |
| #52 | epic: AppArmor / SELinux profile support | epic |
| #60 | feat: io_uring opt-in seccomp profile | feat/low-pri |
| #61 | feat: CRIU checkpoint/restore support | feat/low-pri |
| #62 | feat: minimal --features build for embedded/IoT | feat/low-pri |
| #63 | feat(mac): AppArmor profile template (sub of #51) | feat |
| #64 | feat(mac): SELinux process label support (sub of #51) | feat |
| #67 | epic: deeper Wasm/WASI support | epic |
| #69 | fix: integration test suite hangs locally (DNS tests) | bug/ACTIVE |

## Current Baseline (2026-03-03, SHA df18ca1)

- Unit tests: **290/290 pass** (4 new wasm regression tests added this session)
- Integration tests: should be ~202 but **suite hangs locally** in `dns::` module — see #69
- CI (GitHub Actions): **all 5 jobs pass** (lint, unit-tests, integration-tests, e2e-tests, wasm-e2e-tests)
- E2E tests: **7 Wasm e2e tests pass** (`scripts/test-wasm-e2e.sh`)

## Completed This Session (2026-03-03)

**Wasm/WASI follow-on work (issues #65, #66, #68)**

### #65 — Wasm e2e test script + host→guest dir mapping bug fix

- Fixed `WasiConfig.preopened_dirs`: changed from `Vec<PathBuf>` (identity-only)
  to `Vec<(PathBuf, PathBuf)>` (host, guest pairs)
- Updated `build_wasmtime_cmd`: `--dir host::guest` (double colon)
- Updated `build_wasmedge_cmd`: `--dir host:guest` (single colon)
- Added `with_wasi_preopened_dir_mapped(host, guest)` builder on `Command`
- Fixed `src/cli/run.rs` fast-path to use mapped version for `--bind`
- Fixed `src/bin/pelagos-shim-wasm.rs` rootfs identity tuple
- Created `scripts/test-wasm-e2e.sh` — 7 tests covering: image ls TYPE column,
  run basic output, env passthrough, --bind dir mapping, magic bytes
- Added 4 unit regression tests in `src/wasm.rs`:
  `test_wasmtime_cmd_identity_dir_mapping`, `test_wasmtime_cmd_mapped_dir`
  (regression guard — asserts identity form NOT produced), `test_wasmedge_cmd_mapped_dir`,
  `test_wasmtime_cmd_env_vars`

### #66 — CI integration for e2e tests

- Added `e2e-tests` job to `.github/workflows/ci.yml` (nftables, iproute2, passt,
  rootfs build, reset, then `scripts/test-e2e.sh`)
- Added `wasm-e2e-tests` job (wasm32-wasip1 target, wasmtime install, `scripts/test-wasm-e2e.sh`)
- All 5 CI jobs green on first push

### #68 — `pelagos build` Wasm target (P5-option-B)

- `FROM scratch` support in `execute_stage()`: starts with empty layers + default
  `ImageConfig` instead of pulling a base image
- `detect_wasm_layers(layers)`: post-build scan of each layer dir; if a layer
  contains exactly one `.wasm` file with valid magic bytes, renames it to
  `module.wasm` and records `"application/wasm"` as the layer media type
- Helper functions: `find_sole_wasm_file()`, `collect_layer_files()`
- Used in `execute_build()`: `layer_types: detect_wasm_layers(&layers)`
- 4 new integration tests in `wasm_build_tests` module:
  `test_build_wasm_from_scratch_detects_mediatype`,
  `test_build_wasm_second_layer_only`,
  `test_build_non_wasm_layer_not_detected`,
  `test_build_elf_with_wasm_extension_not_detected`
- CI: all 5 jobs pass (SHA df18ca1)

Also: `docs/WASM_SUPPORT.md` created — three-layer architecture, CLI examples,
comparison table vs runc/runwasi/Spin, limitations, roadmap pointing to epic #67.

## Active Bug: Integration Test Suite Hangs (#69)

**Symptom:** Running the full integration test suite locally hangs indefinitely
in the `dns::` module. The CI integration-tests job runs in 90s fine (different
environment — GitHub Actions Ubuntu, no leftover network state).

**Diagnosis so far (2026-03-03):**
- Confirmed via binary search: `dns::` module is the culprit
- `dns::test_dns_daemon_lifecycle` — **passes individually** (~0s)
- `dns::test_dns_dnsmasq_lifecycle` — **skipped** (dnsmasq not installed locally)
- Remaining 6 DNS tests not yet individually confirmed before reboot

**Root cause hypothesis:** An orphaned `pelagos-dns` process or a network
namespace left over from a previous test run is blocking a subsequent test.
The `reset-test-env.sh` script was also failing with exit 144 during this
session, suggesting the local environment was in a bad state.

**Plan after reboot:**
1. Run `sudo scripts/reset-test-env.sh` (should work cleanly post-reboot)
2. Run each DNS test individually with `timeout 30` to find the one that hangs:
   ```bash
   CARGO="$(rustup which cargo)"
   for t in dns::test_dns_multi_network dns::test_dns_network_isolation \
             dns::test_dns_resolves_container_name dns::test_dns_upstream_forward; do
     echo -n "$t ... "
     sudo env RUSTUP_HOME=$HOME/.rustup CARGO_HOME=$HOME/.cargo PATH=$HOME/.cargo/bin:$PATH \
       timeout 30 "$CARGO" test --test integration_tests "$t" -- --test-threads=1 2>&1 | tail -1
     sudo killall pelagos-dns 2>/dev/null || true
   done
   ```
3. Read the hanging test's implementation and fix: likely missing timeout on
   DNS daemon wait, or blocking `wait()` with no cleanup path
4. After fix, run full suite to confirm it completes

## Wasm Epic #67 — Sub-issues

| # | Title | Priority |
|---|-------|----------|
| P2 | WASI preview 2 socket passthrough | Medium |
| P3 | Wasm Component Model execution | Low (needs embedded crate) |
| P4 | Persistent Wasm VM pool | Low |
| P5 | `pelagos build` Wasm target | **DONE** (#68) |

P1 (Mixed Linux+Wasm compose validation) dropped — needs P2 (sockets) first to
be meaningful.

## Suggested Next Steps (after DNS fix)

- Fix #69 (DNS hang) — must do first; full integration suite must pass locally
- #52 (AppArmor/SELinux) — highest real-world security impact
- #60 (io_uring seccomp profile) — useful complement to existing seccomp work
- #61 (CRIU) — complex but differentiating checkpoint/restore feature

## Session Notes

For historical session notes (completed work, design rationale) see git log.
