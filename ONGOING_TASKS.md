# Ongoing Tasks

All work is tracked in GitHub Issues. This file is a brief index.

## Open Issues

| # | Title | Kind |
|---|-------|------|
| #86 | bug: postgres:alpine CAP_CHOWN/CAP_FOWNER denied in compose | bug/CLOSED |
| #80 | bug: test_dns_upstream_forward fails (EAGAIN on 8.8.8.8 fwd socket) | bug/CLOSED |
| #74 | epic: cgroup enforcement test coverage | epic/CLOSED |
| #79 | feat: test_cgroup_pids_limit_pid_namespace (sub of #74) | test/CLOSED |
| #78 | feat: test_cgroup_resource_stats_pid_namespace (sub of #74) | test/CLOSED |
| #77 | feat: test_cgroup_cpuset_pid_namespace (sub of #74) | test/CLOSED |
| #76 | feat: test_cgroup_cpu_quota_pid_namespace (sub of #74) | test/CLOSED |
| #52 | epic: AppArmor / SELinux profile support | epic/CLOSED |
| #63 | feat(mac): AppArmor profile template (sub of #52) | feat/CLOSED |
| #64 | feat(mac): SELinux process label support (sub of #52) | feat/CLOSED |
| #60 | feat: io_uring opt-in seccomp profile | feat/low-pri |
| #61 | feat: CRIU checkpoint/restore support | feat/low-pri |
| #62 | feat: minimal --features build for embedded/IoT | feat/low-pri |
| #67 | epic: deeper Wasm/WASI support | epic |
| #70 | feat(wasm): mixed Linux+Wasm compose validation (P1) | feat |
| #71 | feat(wasm): WASI preview 2 socket passthrough (P2) | feat |
| #73 | feat(wasm): persistent Wasm VM pool (P4) | feat/low-pri |
| #47 | track: runtime-tools pidfile.t kill-on-stopped bug (upstream) | upstream |
| #48 | track: runtime-tools process_rlimits broken by Go 1.19+ (upstream) | upstream |
| #49 | track: runtime-tools delete tests hardcoded for cgroupv1 (upstream) | upstream |

## Current Baseline (2026-03-09, post-v0.24.0)

- Unit tests: **299/299 pass**
- Integration tests: **249/249 pass, 6 ignored**
- Tree: clean, up to date with origin/main

**Note for next session:** Always `sudo scripts/reset-test-env.sh` if starting from
a possibly dirty environment.

## Completed This Session (2026-03-08)

### Issue triage + GH reconciliation
- Discovered #80 (DNS upstream forward) and #86 (compose caps) already fixed in v0.24.0 but not closed — closed both
- Discovered #74 epic + sub-issues #76–#79 (cgroup enforcement tests) already implemented and passing — closed all
- Updated ONGOING_TASKS.md issue index to match GH reality (21 open → all correctly tracked)

### AppArmor / SELinux support (#52, #63, #64) — commit 34527bc
- `src/mac.rs`: `is_apparmor_enabled()`, `is_selinux_enabled()`, async-signal-safe fd helpers
- **Two-step fd technique** (matches runc): open attr fd before chroot (step 3.9), write after NNP/Landlock but before seccomp (step 6.56); both spawn paths (root + rootless) updated
- LSM availability checked in parent process so allocation-safe; graceful skip when LSM absent
- `Command::with_apparmor_profile()`, `Command::with_selinux_label()` builder methods
- `--apparmor-profile`, `--selinux-label` CLI flags on `pelagos run`
- `(apparmor-profile ...)`, `(selinux-label ...)` in compose specs
- `scripts/apparmor-profiles/pelagos-container` — production profile template
- `scripts/apparmor-profiles/pelagos-test` — permissive profile for tests
- 3 new integration tests; 246/246 pass
- Issues #52, #63, #64 closed

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

## Apple Silicon Package — Design Options (researched 2026-03-09)

See detailed notes below. Decision pending.

---

## Next Session: Start Here

1. #61 (CRIU checkpoint/restore) — complex but differentiating feature
2. Wasm: #70 (mixed compose validation), #71 (WASI P2 sockets)

## Completed This Session (2026-03-09)

### io_uring opt-in seccomp profile (#60) — commit TBD

- **Gap closed**: added `io_uring_setup`, `io_uring_enter`, `io_uring_register` to
  `docker_default_filter()`'s blocked list, matching Docker's actual behaviour
  (syscall numbers 425/426/427, same on x86_64 and aarch64)
- **Opt-in**: `SeccompProfile::DockerWithIoUring` variant + `docker_iouring_filter()`
  function (identical to Docker minus the three io_uring syscalls)
- **Builder**: `Command::with_seccomp_allow_io_uring()`
- **CLI**: `--security-opt seccomp=iouring` / `seccomp=io-uring`
- **Test binary**: `scripts/iouring-test-context/iouring_probe.c` — static C probe,
  exits 1 on EPERM (seccomp blocked), 0 on EINVAL/EFAULT (kernel reached)
- **Integration tests**: `test_seccomp_docker_blocks_io_uring`,
  `test_seccomp_iouring_profile_allows_io_uring` — both pass
- Unit tests: 299/299 pass; integration tests: 248/248 pass
- Issue #60 closed

---

## Plan: io_uring opt-in seccomp profile (#60)

### Background

Docker's real default seccomp profile blocks `io_uring_setup`, `io_uring_enter`,
`io_uring_register` — these syscalls were blocked by Docker post-5.12 due to
historical kernel exploits via io_uring. Pelagos's Docker-derived profile currently
omits them from the blocked list (gap vs Docker reality). The feature:

1. Closes the gap: add the three syscalls to `docker_default_filter()`'s blocked list.
2. Provides opt-in: add `SeccompProfile::DockerWithIoUring` + `with_seccomp_allow_io_uring()`
   builder + `--security-opt seccomp=iouring` CLI flag.

### Syscall numbers (same on x86_64 and aarch64)

| Syscall | x86_64 / aarch64 |
|---------|-------------------|
| `io_uring_setup` | 425 |
| `io_uring_enter` | 426 |
| `io_uring_register` | 427 |

### Files changed

| File | Change |
|------|--------|
| `src/seccomp.rs` | Add numbers to `syscall_number()`; add to `docker_default_filter()` blocked list; add `SeccompProfile::DockerWithIoUring`; add `docker_iouring_filter()` |
| `src/container.rs` | Match arm for new variant; `with_seccomp_allow_io_uring()` builder |
| `src/cli/run.rs` | `"iouring"\|"io-uring"` in seccomp CLI parser |
| `scripts/iouring-test-context/iouring_probe.c` | C probe: calls `io_uring_setup(0,NULL)`; exits 1 on EPERM, 0 on anything else |
| `tests/integration_tests.rs` | 2 new tests (see below) |
| `docs/INTEGRATION_TESTS.md` | Document new tests |

### `docker_iouring_filter()`

Identical to `docker_default_filter()` but excludes the three io_uring names from
`blocked_syscalls`. No other changes.

### Tests

| Test | Root | Assertion |
|------|------|-----------|
| `test_seccomp_docker_blocks_io_uring` | yes | Container with default Docker seccomp; runs `iouring_probe`; expects exit 1 (EPERM from seccomp) |
| `test_seccomp_iouring_profile_allows_io_uring` | yes | Container with `DockerWithIoUring`; runs `iouring_probe`; expects exit 0 (EINVAL from kernel — syscall reached) |

Both tests compile `iouring_probe.c` at test time with `cc`/`gcc`. Skip if compiler absent.

---

## Plan: AppArmor / SELinux Profile Support (#52, #63, #64)

### Context

- **AppArmor**: AVAILABLE on this host (`/proc/self/attr/apparmor/exec` writable).
- **SELinux**: NOT available (`/sys/fs/selinux/` absent). Code is implemented; tests verify graceful skip.
- **No existing MAC code** in the codebase. Clean slate.

### Pre-exec hook placement

The AppArmor exec-attr write must happen:
- AFTER the PID-namespace double-fork (line ~3212) — so the grandchild (real container) writes its own attr, not the intermediate waiter
- BEFORE chroot/pivot_root (line ~3282) — so `/proc/self/attr/apparmor/exec` is accessible via the host's /proc
- WRITTEN late — after all security setup (caps, ambient, OOM adj, user callback, NNP) but BEFORE seccomp

Technique: open the attr fd early (before chroot), write it late (before seccomp). Follows runc's pattern.

### New steps in pre_exec

**Step 3.9 (new)** — after double-fork, before overlay mount (~line 3240):
```
Open /proc/self/attr/apparmor/exec → apparmor_attr_fd (−1 if AppArmor not running)
Open /proc/self/attr/exec          → selinux_attr_fd  (−1 if SELinux not running)
```

**Step 6.56 (new)** — after Landlock (step 6.55), before seccomp (step 7):
```
if apparmor_attr_fd >= 0: write(apparmor_attr_fd, profile); close(fd)
if selinux_attr_fd  >= 0: write(selinux_attr_fd,  label);   close(fd)
```

### Files to create / modify

| File | Change |
|------|--------|
| `src/mac.rs` (new) | `is_apparmor_enabled()`, `is_selinux_enabled()` (used from tests + CLI) |
| `src/lib.rs` | `pub mod mac;` |
| `src/container.rs` | New fields `apparmor_profile`, `selinux_label`; builder methods; two new pre_exec steps |
| `src/cli/run.rs` | `--apparmor-profile` and `--selinux-label` CLI flags |
| `src/compose.rs` | `apparmor_profile`, `selinux_label` fields on `ServiceSpec` + parser |
| `src/cli/compose.rs` | Wire fields in `spawn_service()` |
| `scripts/apparmor-profiles/pelagos-container` (new) | Minimal AppArmor profile template (permissive-ish baseline, commented) |
| `scripts/apparmor-profiles/pelagos-test` (new) | Fully permissive profile for integration tests only |
| `tests/integration_tests.rs` | 3 new tests (see below) |
| `docs/INTEGRATION_TESTS.md` | Document the 3 new tests |

### Tests

| Test | Requires root | When run | Assertion |
|------|--------------|----------|-----------|
| `test_apparmor_profile_unconfined` | yes | AppArmor enabled OR not | `.with_apparmor_profile("unconfined")` — container exits 0 |
| `test_apparmor_profile_applied` | yes | Skip if AppArmor disabled or `apparmor_parser` missing | Load `pelagos-test` profile; run container that prints `/proc/self/attr/current`; assert output contains `pelagos-test`; unload profile |
| `test_selinux_label_no_selinux` | yes | Always | `.with_selinux_label("system_u:system_r:container_t:s0")` — container exits 0 (label silently ignored when SELinux not running) |

### Graceful degradation rules

- `open()` of attr path returns ENOENT / EACCES → LSM not running → fd = −1 → skip silently, container runs unconfined
- `write()` fails (e.g., EINVAL — profile not found in kernel) → propagate as spawn error
- "unconfined" is always a safe profile name on AppArmor-enabled systems

### What #63 and #64 deliver

- **#63**: `with_apparmor_profile()` API, `--apparmor-profile` CLI flag, `(apparmor-profile ...)` in compose, `scripts/apparmor-profiles/pelagos-container` template
- **#64**: `with_selinux_label()` API, `--selinux-label` CLI flag, `(selinux-label ...)` in compose, graceful skip when SELinux absent

## Session Notes

For historical session notes (completed work, design rationale) see git log.

---

## Design Options: pelagos for Apple Silicon

### Background

pelagos uses Linux namespaces, cgroups, and seccomp — Linux-only primitives. On Apple Silicon a Linux VM is mandatory. The goal is a polished developer tool comparable to AWS Finch: single installer, transparent CLI, good I/O performance, Rosetta 2 for x86_64 images.

**VM layer options evaluated:** Lima/VZ, vfkit, raw QEMU, Apple Containerization (macOS 26), crosvm (no macOS), cloud-hypervisor/hypervisor-framework (low-level Rust), Multipass (GPL v3, QEMU-based — eliminated).

**Bottom line on the VM layer:** Lima with the `vmType: vz` backend (Apple Virtualization Framework) is the correct substrate. It is Apache 2.0, CNCF Incubating, used by Finch/Colima/Rancher Desktop, gives 3–8s VM boot with virtiofs file sharing, and is actively maintained. QEMU is slower (15–40s boot, virtio-9p I/O) and only relevant for cross-arch emulation. The four designs below differ on IPC architecture and distribution model, not on the VM layer.

---

### Option A — Lima/VZ + SSH passthrough (Finch model)

**Architecture:**
```
macOS: pelagos-mac CLI  ──SSH──►  Lima VM (arm64 Alpine)
                                    └─ pelagos binary
```
The macOS CLI (`pelagos-mac`) shells out to `limactl shell` or an SSH command to run `pelagos` inside the VM. Path and volume arguments are translated (host path → virtiofs mount path). Lima manages VM lifecycle, virtiofs file sharing, and port forwarding. Packaged as a signed `.pkg` installer bundling Lima + a custom Lima template + the pelagos binary for aarch64 Linux.

**Pros:**
- Shortest path to a working product — Finch proved it at scale
- Lima handles virtiofs, port forwarding, VM lifecycle, SSH key management — no reinvention
- Apache 2.0 (Lima) — clean license; distributable in commercial products
- Lima's gRPC-based port forwarder is already robust for `--publish`
- VM stays running; container starts are sub-second after first boot
- Rosetta 2 available via Lima VZ config (`rosetta.enabled: true`)
- Lima is embeddable via Go packages, not just subprocess (Colima proves this)

**Cons:**
- Every CLI invocation forks an SSH process or gRPC call — measurable latency per command (~50–200ms overhead) vs a persistent socket
- Go dependency: Lima is Go; if using Lima as a library requires compiling/linking Go, or shipping the `limactl` binary as a subprocess target
- Less control over UX — Lima's abstractions are opinionated (e.g., socket path conventions, network config)
- Component version pinning: each Lima update requires a new pelagos-mac release
- SSH passthrough means streaming logs (`pelagos logs --follow`) requires SSH multiplexing or a separate channel

**Performance:** virtiofs I/O reaches 60–80% of native macOS. VM boot 3–8s, container starts sub-second.

**Distribution:** signed `.pkg` installer; follows Finch's model exactly.

**Effort:** Moderate. Most complexity is in the macOS CLI path translation and installer packaging, not VM management.

---

### Option B — vfkit + Rust orchestrator + vsock daemon

**Architecture:**
```
macOS: pelagos-mac CLI  ──vsock──►  vfkit VM (arm64 Alpine)
            │                          └─ pelagos-daemon (Rust gRPC/JSON over vsock)
            └──► vfkit subprocess
```
The macOS host binary spawns `vfkit` as a child process with a constructed argument list to start a minimal Linux VM. Inside the VM, `pelagos-daemon` listens on a virtio-vsock socket. The macOS CLI connects directly over vsock — no SSH involved. VM lifecycle (start/stop/clean shutdown) is managed by the Rust host binary.

**Pros:**
- No Go runtime dependency — vfkit binary (~15 MB) is the only foreign component; everything else is Rust
- vsock is a direct host-guest channel: faster than SSH, lower latency per command (< 5ms typical)
- Full control over the protocol — can use gRPC, JSON-RPC, or a custom framing
- Streaming (logs, exec I/O) is clean via vsock multiplexed streams, no SSH channel gymnastics
- vfkit is Apache 2.0, Red Hat maintained, used by CRC + Podman Machine in production
- Tighter control over VM config (disk size, memory, vCPUs) from the Rust side

**Cons:**
- File sharing must be built: vfkit provides the virtiofs device but not the host-side path translation, port forwarding automation, or socket management that Lima includes. These must be implemented.
- Port forwarding: must implement vsock-to-TCP forwarding on the host side (or use virtio-net + host routing)
- VM lifecycle management (first boot, kernel extraction, disk image management) must be built from scratch
- More total engineering work than Option A — weeks more — before reaching feature parity with Lima
- vfkit subprocess model means monitoring its health, handling crashes, restart policy
- The `vfkit` binary itself is a distribution dependency (must be bundled in the installer)

**Performance:** Same AVF/VZ baseline as Lima/VZ. vsock IPC is faster than SSH for command latency.

**Distribution:** signed `.pkg` or Homebrew cask. The Rust binary + vfkit binary + Linux VM image (kernel + initrd + root disk) are bundled.

**Effort:** Significant. VM lifecycle management and file sharing from scratch is a substantial project.

---

### Option C — Lima/VZ + persistent gRPC daemon (hybrid)

**Architecture:**
```
macOS: pelagos-mac CLI ──Unix socket──► pelagos-mac daemon ──vsock/SSH──► pelagos-daemon (gRPC)
                                                │
                                            Lima VM (VZ)
```
Lima manages the VM. Instead of SSH-per-command passthrough, a persistent `pelagos-daemon` gRPC server runs inside the VM and is reachable from the host via a Unix domain socket forwarded through Lima's vsock channel. The macOS daemon handles VM startup and socket lifecycle. The macOS CLI connects to the macOS daemon socket.

**Pros:**
- Combines Lima's VM management (virtiofs, port forwarding, Rosetta, lifecycle) with low-latency persistent IPC
- CLI command latency drops to < 5ms (no SSH fork per invocation)
- Streaming (logs, exec) is naturally supported via gRPC server streaming
- The gRPC interface is a clean API boundary — the macOS CLI can be thin; the daemon is the contract
- Docker socket compatibility: the daemon can expose a Docker-compatible Unix socket as a future path to drop-in compatibility
- Lima's VM management is still doing all the heavy lifting

**Cons:**
- More moving parts than Option A: the macOS daemon, gRPC server inside the VM, socket forwarding layer — all must be built and maintained
- gRPC protocol design is non-trivial: defining the protobuf interface for all pelagos operations (run, build, compose, exec, logs, etc.)
- Lima's vsock channel for Unix socket forwarding must be confirmed to work reliably for this use case (Lima uses vsock for its own guestagent communication; piggybacking a second socket is possible but needs validation)
- Still has the Lima component version pinning problem

**Performance:** Best of both worlds — Lima's VM substrate + persistent socket IPC. CLI roundtrip < 5ms after initial connect.

**Distribution:** signed `.pkg` with Lima + pelagos-mac-daemon + pelagos Linux binary.

**Effort:** Significant upfront (protocol design, daemon, socket forwarding) but pays off at scale.

---

### Option D — Homebrew formula + Lima template (minimal/community path)

**Architecture:**
```
brew install pelagos
# installs: lima + pelagos Lima template
lima create --template pelagos  →  starts VM
pelagos run ...  →  thin wrapper around: limactl shell pelagos -- pelagos run ...
```
Ship a Homebrew formula that declares Lima as a dependency and installs a Lima instance template YAML (configuring VZ, virtiofs mounts, Rosetta, and the pelagos binary path). No custom installer, no bundled components. Users manage Lima separately.

**Pros:**
- Minimal engineering: no installer packaging, no custom VM lifecycle code
- Leverages Homebrew's update mechanism — Lima and pelagos update independently
- Low maintenance burden
- Good for early adoption/community experimentation
- Users who already have Lima installed can use the template directly

**Cons:**
- Not "complete" — user experience is fragmented (separate Lima and pelagos updates, manual `lima create`)
- No control over Lima version compatibility — Lima breaking changes affect pelagos without a release
- No macOS daemon socket — tools expecting a Docker socket cannot connect
- Least polished: no native macOS CLI, no signed installer, not enterprise-ready
- Path translation for bind mounts requires user awareness of virtiofs mount points

**Performance:** Same Lima/VZ baseline. No additional overhead.

**Distribution:** `brew tap pelagos/tap && brew install pelagos`. No installer.

**Effort:** Small — primary work is the Lima template YAML and wrapper script.

---

### Option E — Apple Containerization (VM-per-container, future)

**Architecture:**
```
macOS: pelagos-mac CLI  ──►  Swift Containerization framework
                                └─ VM per container (AVF, ~50ms start)
                                └─ pelagos replaces vminitd as init system
```
Apple's new `Containerization` Swift framework (Apache 2.0, WWDC 2025) provides a VM-per-container model with sub-second starts. Each pelagos container would run in its own dedicated micro-VM — native macOS, no shared Linux environment, true isolation.

**Pros:**
- VM-per-container is architecturally cleaner than a shared Linux VM for isolation guarantees
- Apple-native: no third-party dependencies, likely to be well-optimized over time
- Sub-50ms container start time (Apple's claim)
- OCI image support built in
- Apache 2.0 license

**Cons:**
- **Requires macOS 26 Tahoe** — in beta as of early 2026; full networking between containers requires macOS 26
- Swift-only API — pelagos is Rust; calling Swift from Rust requires FFI bridging or a subprocess model; the Swift framework has no C interface
- Too new: the framework is at 0.x, undocumented in places, no production use
- The VM-per-container model bypasses pelagos entirely (pelagos's namespace/cgroup machinery is Linux-side; if each container is a VM, pelagos becomes the Linux init inside the VM, a significant redesign)
- Apple can and does break APIs; betting on a first-version Apple framework is risky

**Performance:** Best theoretical; real-world benchmarks not yet available.

**Distribution:** macOS 26+ only — limits addressable audience until 2027+.

**Effort:** Very high, and blocked on macOS 26 availability.

---

### Security Analysis

Five dimensions per option:
1. **Authentication** — who is allowed to send commands to the runtime?
2. **Privilege granted on compromise** — what can an attacker do if the IPC channel is reached?
3. **Host attack surface** — what processes/sockets are listening on the macOS host?
4. **Network exposure** — reachable beyond localhost / the local user?
5. **Container escape / VM isolation** — can a container break out to the host?

#### Option A — Lima/VZ + SSH passthrough

**Authentication:** OpenSSH with an ephemeral keypair generated per VM instance by Lima. Stored at `~/.lima/<instance>/ssh.key` (mode 0600). Authentication is handled entirely by the OS-level SSH infrastructure — not custom code.

**Privilege on compromise:** An attacker who obtains the Lima SSH private key can SSH into the VM and run pelagos with the same privileges as the host user mapped into the VM. They cannot directly reach the macOS host filesystem except via the virtiofs share (which exposes only explicitly declared directories).

**Host attack surface:** Lima's hostagent Unix socket at `~/.lima/<instance>/ha.sock` (mode 0600). The SSH daemon listens inside the VM on vsock — not a host TCP port. Port forwards are localhost-only and opt-in.

**Container escape:** Namespace-based inside the Alpine VM. A container escape reaches the VM but not the macOS host directly. AVF is the second boundary. The virtiofs share scope determines the blast radius.

**Summary:** Strong. SSH is the most audited remote access protocol in existence. Attack surface is minimal and well-understood. Risk scales with the breadth of the virtiofs share (mounting `/Users/you` is much worse than mounting a specific project directory).

---

#### Option B — vfkit + Rust orchestrator + vsock daemon

**Authentication:** No credential exchanged. The host-side Rust orchestrator forwards the vsock connection to a Unix domain socket; that socket's filesystem permissions (0600, owner only) are the sole authentication boundary. This is the Docker socket model — ownership of the socket file is the key. On macOS with AVF, vsock ports are not system-wide sockets; they are mediated by the VMM process (vfkit), so external processes cannot reach the guest directly. The risk is on the host-side forwarded socket.

**Privilege on compromise:** Whoever reaches the Unix socket can issue any pelagos command — run with arbitrary bind mounts, exec into containers, read any log. This is Docker socket equivalent: functionally root-equivalent for the VM and any host paths in the virtiofs share.

**Host attack surface:** The Unix domain socket (persistent daemon). The vfkit child process. Any bug in a gRPC handler that accepts path arguments (bind mount source, working directory) is a potential injection vector for someone who can reach the socket.

**Container escape:** Same namespace isolation inside the VM, AVF as the outer boundary.

**Summary:** Moderate. The socket-as-authentication model is a known footgun. The required mitigation — strict Unix socket permissions — is well understood but must be implemented correctly and audited. Input validation in every handler that accepts paths or commands is mandatory.

---

#### Option C — Lima/VZ + persistent gRPC daemon

**Authentication:** This is the source of the user's correct unease. **gRPC has no built-in authentication.** Options are: Unix socket ownership (necessary but not sufficient), mTLS client certificates (correct; significant implementation and UX complexity), or bearer tokens in gRPC metadata (weak — tokens can be stolen from process memory or swap). Without mTLS, the daemon is authenticated only by socket file ownership — same posture as Option B, but with a larger and more complex attack surface.

**Privilege on compromise:** Highest of all options. The gRPC interface is a **general-purpose privileged execution API** built to accept arbitrary container operations. Compromise = create containers with any bind mount, exec arbitrary commands in any container, read any log, manipulate any volume. This is the Docker daemon problem, rebuilt from scratch with custom code that lacks Docker's decade of hardening.

The Docker daemon's history is directly instructive: the socket was root-equivalent from day one, leading to years of CVEs, `--userns-remap`, rootless Docker, and eventually VM isolation as the primary mitigation. A custom gRPC daemon reproduces this architecture without that history.

**Host attack surface:** The macOS-side daemon (persistent process + Unix socket) + the in-VM gRPC server + the vsock forwarding layer — three components vs. one (Lima's hostagent) in Option A. Each is an attack surface; each has bugs. Additionally, if the gRPC server inside the VM accidentally binds to `0.0.0.0` instead of the vsock interface — a common misconfiguration — it becomes network-reachable.

**Input injection:** Every gRPC handler that accepts a path (bind mount source, COPY source in build, working directory, log path) is a potential path traversal or injection vector for any caller who reaches the socket. This class of bug is pervasive in container runtime implementations.

**Container escape:** Same namespace isolation. But the gRPC daemon runs with elevated privileges inside the VM (it must — to create namespaces, mount filesystems, manage cgroups), making it a higher-value target than the SSH server in Option A.

**Summary:** Weakest security posture of the five. The combination of unauthenticated-by-default gRPC + a privileged custom execution API + multiple new attack surfaces makes this the highest-risk design. If pursued, mTLS mandatory from day one + strict handler input validation + a dedicated security audit are non-negotiable prerequisites — substantially increasing implementation cost beyond what the latency improvement justifies. **The 150ms SSH overhead in Option A is the correct price of not building a Docker daemon.**

---

#### Option D — Homebrew formula + Lima template

**Runtime security:** Identical to Option A — SSH keypair, Lima hostagent socket, no network exposure.

**Supply chain** — the one dimension where D is strictly worse than A. A Homebrew tap formula is a Ruby file fetched from a GitHub repository. Its integrity depends on HTTPS + SHA256 checksums for downloaded artifacts (present) but there is no GPG signing of Homebrew formulas and no Gatekeeper validation of installed binaries. A compromised tap repository can ship a formula installing a backdoored pelagos binary. The signed, notarized `.pkg` in Option A is validated by macOS Gatekeeper before installation — a meaningful defense-in-depth layer absent in D.

**Summary:** Acceptable for a developer/community tool where the user understands Homebrew's trust model. Insufficient for enterprise distribution where MDM policies rely on signed installers. Gatekeeper's absence is the meaningful delta from Option A.

---

#### Option E — Apple Containerization (VM-per-container)

**Authentication:** Controlled by the macOS session ownership model. No socket exposed to other users by default. The `com.apple.security.hypervisor` entitlement required to call the framework is gating at the distribution level.

**Privilege on compromise:** VM-per-container means a successful container escape reaches only that container's VM — not a shared Linux environment, not other containers. An AVF hypervisor vulnerability is required to reach the macOS host. This is qualitatively stronger isolation than any namespace-based option.

**Container escape:** AVF hypervisor boundary per container. A namespace escape inside the VM does not yield access to other containers or the host. Historically rare; Apple has strong incentive to fix hypervisor bugs quickly.

**Framework immaturity:** The flip side of OpenSSH's 25 years of audits — the Apple `Containerization` framework is 0.x with no published security audit and no production track record. Early framework versions frequently have significant vulnerabilities. Betting a security story on an unaudited v0 framework is its own risk.

**Summary:** Best isolation model by architecture (hypervisor boundary per container). Highest unknown risk from framework immaturity. The entitlement model provides meaningful distribution gating. Not evaluable until macOS 26 ships and accumulates a security track record.

---

### Summary Tables

**Performance and engineering:**

| | Option A (Lima SSH) | Option B (vfkit+Rust) | Option C (Lima+gRPC) | Option D (Homebrew) | Option E (Apple) |
|---|---|---|---|---|---|
| Effort | Moderate | Significant | Significant | Small | Very high |
| IPC latency | ~150ms/cmd | ~5ms/cmd | ~5ms/cmd | ~150ms/cmd | ~5ms/cmd |
| Go dependency | Lima binary | vfkit binary only | Lima binary | Lima binary | No |
| File sharing | Auto (virtiofs) | Must build | Auto (virtiofs) | Auto (virtiofs) | Auto (AVF) |
| Docker socket compat | Add later | Buildable | Natural | No | No |
| macOS version req | 13.5+ | 13.5+ | 13.5+ | 13.5+ | 26+ |
| License | Apache 2.0 | Apache 2.0 | Apache 2.0 | Apache 2.0 | Apache 2.0 |
| Polish/completeness | High | High | Highest | Low | Blocked |

**Security:**

| | Authn model | Compromise impact | Custom attack surface | Container escape barrier | Supply chain |
|---|---|---|---|---|---|
| A (Lima SSH) | OpenSSH keypair | VM + bounded virtiofs scope | None (OpenSSH) | Namespace + AVF | Signed + notarized |
| B (vfkit+vsock) | Unix socket ownership | Docker-socket equivalent | gRPC handler bugs | Namespace + AVF | Signed + vfkit binary |
| C (Lima+gRPC) | **None by default** | **Docker-socket + handler injection** | **gRPC handlers (custom code)** | Namespace + AVF | Signed |
| D (Homebrew) | OpenSSH keypair | VM + bounded virtiofs scope | None (OpenSSH) | Namespace + AVF | **No Gatekeeper validation** |
| E (Apple) | macOS session | Bounded by AVF hypervisor | Apple framework (unaudited 0.x) | **AVF per container (strongest)** | Entitlement-gated |

### Revised Recommendation

Security analysis reverses the earlier suggestion to evolve toward Option C.

- **Option A** is the correct v1 and likely permanent architecture. SSH passthrough is not just "simpler" — it is architecturally correct: authentication is handled by a protocol with 25 years of security hardening, the attack surface is a single well-understood component, and the 150ms per-command overhead is the correct price for not building a Docker daemon.
- **Option B** is worth pursuing if latency becomes a measured user pain point. The vsock+Unix-socket model is sound if socket permissions are strict and every handler validates its inputs. Consider it v2 only after Option A is shipping and users are actually complaining about command latency.
- **Option C** should not be the target architecture. The gRPC daemon is the Docker daemon problem rebuilt from scratch. If a persistent socket is needed (e.g., for Docker socket compatibility), the correct approach is to expose a minimal, read-mostly status socket (not a full execution API) with mTLS mandatory from day one.
- **Option D** is fine for community distribution; insufficient for enterprise.
- **Option E** is a 2027 watch item.

