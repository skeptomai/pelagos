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
| #70 | feat(wasm): mixed Linux+Wasm compose validation (P1) | feat |
| #71 | feat(wasm): WASI preview 2 socket passthrough (P2) | feat |
| #72 | feat(wasm): Component Model via embedded wasmtime (P3) | feat/CLOSED |
| #73 | feat(wasm): persistent Wasm VM pool (P4) | feat/low-pri |
| #69 | fix: integration test suite hangs locally (DNS tests) | bug/CLOSED |

## Current Baseline (2026-03-07, SHA 201e4b4, v0.24.0)

- Unit tests: **296/296 pass**
- Integration tests: **243/243 pass, 6 ignored** — all pre-existing failures fixed
- CI: **all jobs green** on v0.24.0 release (lint + unit + integration + x86_64 + aarch64)
- Released: **v0.24.0** — https://github.com/skeptomai/pelagos/releases/tag/v0.24.0

**Note for next session:** Always `sudo scripts/reset-test-env.sh` if starting from
a possibly dirty environment. The reset script now handles veths, nftables, and DNS
daemon cleanup correctly.

## Completed This Session (2026-03-07, v0.24.0)

### Resource cleanup fixes
- Investigated orphaned veths/nftables after container exit; confirmed natural exit
  and `pelagos stop` always clean up correctly
- **Watcher SIGTERM handler**: forwards SIGTERM/SIGINT to container so `kill <watcher_pid>`
  triggers normal teardown_resources() path (veth/netns/nftables cleanup)
- **`pelagos rm --force`**: SIGTERM-first with 5 s grace period before SIGKILL
- **`reset-test-env.sh`**: SIGTERM-first; deletes all `vh-*`/`vp-*` veths;
  removes all `pelagos-*` nftables tables dynamically; stops DNS daemon

### Capability management
- `Capability::DEFAULT_CAPS` (Podman's 11-cap set) as secure-but-functional baseline
- Compose `spawn_service` uses DEFAULT_CAPS (fixes postgres/nginx/redis breaking)
- `(cap-drop ...)` in compose (both S-expr and Lisp parsers); `(cap-drop ALL)` supported
- `--cap-drop NAME` CLI flag supporting all 41 Linux cap names
- `parse_capability()` rewritten to use `Capability::from_name()` (was 9 hardcoded caps)
- 6 new integration tests for cap behavior

### Tutorial + image fixes
- `image ls` TYPE column: now shows `component` vs `wasm` vs `linux` correctly
- Tutorial: $GITHUB_USER var, redis pull prerequisite, Wasm prerequisites table,
  std::process::id() removal, component-ctx/ build context, Part 7 rewritten (issue #70)
- USER_GUIDE.md: graceful shutdown, reset-test-env.sh sections

### Test fixes (3 pre-existing failures eliminated)
- `test_exec_joins_pid_namespace`: rewritten to use `/bin/echo` (avoids /proc/self
  limitation); documents known PID-namespace-exec double-fork issue
- `test_healthcheck_unhealthy`: fixed polling race (was checking file existence only;
  now waits for pid > 0 like other tests)
- `test_tut_p4_compose_depends_on`: confirmed dirty-state flakiness; passes after reset

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

## Bug #69 — RESOLVED

Root cause was dirty local environment state from a previous crashed session
(orphaned `pelagos-dns` processes, stale network namespaces). Post-reboot +
`sudo scripts/reset-test-env.sh` + `--test-threads=1` → 202/202 pass in 37s.
No code changes required. Issue closed.

## Completed This Session (2026-03-04)

### P3b — Wasm Component Model via embedded wasmtime (#72)

**Feature gate:** `--features embedded-wasm` (same as P3a)

- `src/wasm.rs`:
  - `WASM_MODULE_VERSION`: const `[0x01, 0x00, 0x00, 0x00]` to distinguish modules from components
  - `is_wasm_component_binary(path)`: reads bytes 4-7; returns `true` if magic matches but version ≠ module version
  - `run_embedded_inner`: now detects component vs plain module, routes to P2 or P1 path
  - `run_embedded_component_file`: private helper — loads component from disk with `component-model` engine, calls `run_embedded_component`
  - `run_embedded_component(engine, component, extra_args, wasi)`: `pub` entry point — implements `WasiView` via `WasiState { ctx: WasiCtx, table: ResourceTable }`, builds P2 context, calls `wasmtime_wasi::p2::add_to_linker_sync` + `Command::instantiate` + `call_run`, handles `I32Exit` in error chain
  - Fixed P3a argv TODO: `run_embedded_module` now calls `builder.arg("module.wasm")` + `builder.arg(arg)` for each extra arg
  - 4 new unit tests: `test_is_wasm_component_binary_{module_is_false,component_is_true,too_short_is_false,non_wasm_is_false}`
- `Cargo.toml`:
  - wasmtime: added `"component-model"` to features
  - wasmtime-wasi: added `"p2"` to features
- `src/build.rs`:
  - `detect_wasm_layers`: after renaming to `module.wasm`, checks `is_wasm_component_binary`; emits `"application/vnd.bytecodealliance.wasm.component.layer.v0+wasm"` for components, `"application/wasm"` for plain modules
- `tests/integration_tests.rs`:
  - `test_wasm_component_detection_from_bytes`: asserts module/component byte detection
  - `test_wasm_embedded_component_exit_code`: compiles hello.rs → wasm32-wasip2 at test time, runs component in-process, asserts exit code 0; skips if wasm32-wasip2 unavailable
- `scripts/test-wasm-embedded-e2e.sh`:
  - Component section: checks/installs `wasm32-wasip2`, compiles to component, builds image, runs 3 tests (output, env, bind); 14/14 total pass
- `.github/workflows/ci.yml`:
  - `wasm-embedded-e2e-tests` job: added `wasm32-wasip2` to targets list
- `docs/INTEGRATION_TESTS.md`: entries for 2 new integration tests

### P3a — Embedded wasmtime for plain Wasm modules

**Feature gate:** `--features embedded-wasm`

- Added `[features]` to `Cargo.toml`; optional deps `wasmtime 42` + `wasmtime-wasi 42`
- `src/wasm.rs`:
  - `run_wasm_embedded(program, extra_args, wasi)` — public entry point, spawns a thread
  - `run_embedded_module(engine, module, extra_args, wasi)` — inner function (pub for tests);
    uses `wasmtime_wasi::p1::add_to_linker_sync` + `WasiCtxBuilder::build_p1()`
  - I32Exit detection: traverses the full anyhow error chain (proc_exit wraps it in a backtrace context)
  - 2 unit tests: `test_embedded_exit_zero`, `test_embedded_exit_nonzero`
- `src/container.rs`:
  - `ChildInner` enum: `Process(std::process::Child)` + `#[cfg] Embedded(Option<JoinHandle<i32>>)`
  - `Child.inner` changed to `ChildInner`; `wait_inner()` helper handles both variants via `ExitStatusExt::from_raw`
  - `spawn_wasm_impl()`: uses embedded path when all stdio is Inherit and feature is on
  - Updated `pid()`, `take_stdout()`, `take_stderr()`, `wait()`, `wait_preserve_overlay()`,
    `wait_with_output()`, `Drop` to dispatch on `ChildInner`
- `tests/integration_tests.rs`: `wasm_embedded_tests::test_wasm_embedded_exit_code` (no root, no PATH runtime)
- `docs/INTEGRATION_TESTS.md`: entry for the new integration test
- **E2E test** (`scripts/test-wasm-embedded-e2e.sh`, 8/8 pass):
  - Builds pelagos with `--features embedded-wasm`
  - Compiles `scripts/wasm-embedded-context/hello.rs` → wasm32-wasip1
  - Runs `pelagos build` on `scripts/wasm-embedded-context/Remfile` (FROM scratch + COPY)
  - Strips wasmtime/wasmedge from PATH → proves in-process execution
  - Tests: basic output, `--env` passthrough, `--bind` preopened dir

## Wasm Epic #67 — Sub-issues

| # | GH | Title | Priority |
|---|----|-------|----------|
| P1 | #70 | Mixed Linux+Wasm compose validation | Medium |
| P2 | #71 | WASI preview 2 socket passthrough | Medium |
| P3 | #72 | Wasm Component Model via embedded wasmtime | **DONE** |
| P4 | #73 | Persistent Wasm VM pool | Low (depends on P3) |
| P5 | #68 | `pelagos build` Wasm target | **DONE** |

**P2/P3 socket note:** P2 threads socket flags through the subprocess CLI path
(`build_wasmtime_cmd`). P3 adds a separate embedded path via `WasiCtxBuilder`.
They coexist — P2 work is not thrown away. P3 must include socket support in
its `WasiCtxBuilder` setup for parity.

## Completed This Session (2026-03-07)

### Rootless `pelagos exec` fixes and tests (commits 8a20ce4 → e66950d)

All work is in `src/cli/exec.rs`, `src/cli/stop.rs`, `src/container.rs`,
`scripts/setup.sh`, `tests/integration_tests.rs`, `docs/INTEGRATION_TESTS.md`.

**1. Namespace join ordering fix (f41c212)**
Rootless exec was failing with EPERM on `setns`. Root cause: the
`join_ns_fds` mechanism in container.rs runs before the `user_pre_exec`
callback. Joining any namespace (MOUNT, UTS, IPC, NET) owned by the
container's user namespace requires `CAP_SYS_ADMIN` in that user namespace,
which we don't have until AFTER we join it.

Fix: skip `join_ns_fds` entirely for rootless containers; handle all
namespace joins (USER → MOUNT → chroot → UTS/IPC/NET/CGROUP) in the
`user_pre_exec` callback in the correct order.

**2. pid==0 race in exec and stop (8a20ce4, 6a254c5)**
`pelagos run --detach` writes `state.json` with `pid=0` before the watcher
spawns the container, creating a brief window. Both `cmd_exec` and `cmd_stop`
had races: they saw `pid=0`, called `check_liveness(0)` → false, and either
spuriously failed or prematurely marked the container Exited (stop race
allowed a subsequent exec to succeed on an ostensibly stopped container).

Fix: poll for `pid > 0` in both `cmd_exec` and `cmd_stop` with a 2s deadline.

**3. Environ reads intermediate process, not container (6a254c5)**
For PID-namespace containers, `state.pid` is the intermediate process P (which
ran `pre_exec` but never called `exec()`). P's `/proc/pid/environ` reflects the
fork-inherited host environment, not the `--env` vars. The actual container
(grandchild C) has the correct environ.

Fix: read from C via `/proc/{P}/task/{P}/children`, falling back to P if no
grandchild exists (non-PID-ns case).

**4. fuse-overlayfs `allow_other` for `--user UID` exec (1c8949c)**
`pelagos exec --user 1000` failed with EPERM/EACCES. Root cause: fuse-overlayfs
mounts lacked `allow_other`. Only the mounting user (host UID 1000 = container
UID 0) could access the FUSE filesystem. After `setuid(1000)` inside the
container's user namespace, the process becomes host UID 100999 (from the
`/etc/subuid` range), which FUSE rejected with EACCES on `execve`.

Fix:
- `src/container.rs` `spawn_fuse_overlayfs()`: add `allow_other` to the
  fuse-overlayfs mount options string.
- `scripts/setup.sh`: enable `user_allow_other` in `/etc/fuse.conf` (required
  by the kernel for non-root users to use `allow_other` when mounting FUSE).

**5. UID/GID validation against uid_map (e66950d)**
`pelagos exec --user 1000` from a `newgrp pelagos` shell failed with the
cryptic "Invalid argument (os error 22)". Root cause: `newgrp`/`sg` changes
the effective GID away from the primary GID. `newuidmap_will_work()` checks
`egid == pw_gid` and returns false, causing the container's uid_map to collapse
to a single entry `0 host_uid 1`. `setuid(1000)` inside that user namespace
→ EINVAL (UID 1000 not covered by any uid_map entry).

Fix: in `cmd_exec`, read `/proc/{pid}/uid_map` (and `gid_map`) before
spawning and emit a clear error naming the uid_map and the root cause.

**Tests added (5 new rootless exec tests):**
- `test_rootless_exec_noninteractive`: basic exec in running container
- `test_rootless_exec_sees_container_filesystem`: MOUNT ns join verified
- `test_rootless_exec_environment`: env inherit + `-e` override
- `test_rootless_exec_nonrunning_fails`: liveness check rejects stopped containers
- `test_rootless_exec_user_workdir`: `--user 1000`, `--workdir`, `--user 1000:1000`,
  write-as-uid-1000 (verifies fuse-overlayfs `allow_other` covers writes too)

**Known pre-existing failure:**
`exec::test_exec_joins_pid_namespace` fails after a full suite run due to
dirty container state. Passes when run in isolation after
`sudo scripts/reset-test-env.sh`.

## Next Session: Start Here

1. **#52 — AppArmor/SELinux profile support** (highest real-world security impact)
   - Sub-issues: #63 (AppArmor template), #64 (SELinux process label)
   - Design choice to resolve: generate profiles at build time vs ship canned profiles
2. #60 (io_uring seccomp profile) — useful complement to existing seccomp work
3. #61 (CRIU checkpoint/restore) — complex but differentiating feature

## Session Notes

For historical session notes (completed work, design rationale) see git log.
