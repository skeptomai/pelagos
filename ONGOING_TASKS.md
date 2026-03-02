# Ongoing Tasks

## Next: Epic #23 — OCI full compliance / runtime-tools conformance (2026-03-02)

### Immediate next step (after reboot)

The machine needs a reboot before resuming work. Reason: kernel was updated from
`6.18.7-arch1-1` to `6.18.13-arch1-1` but has not been rebooted. The `veth` kernel
module is compiled as `=m` (loadable) but `/lib/modules/6.18.7-arch1-1/` has no module
tree — only `6.18.13` does. This causes all 39 networking integration tests to fail with
`"Unknown device type"` from `ip link add ... type veth`.

After reboot, verify networking tests pass:
```
sudo -E cargo test --test integration_tests networking
```

Then prosecute **issue #25**: run the `opencontainers/runtime-tools` conformance suite
end-to-end and fix all failures. This closes issue #25, which closes epic #23.

### Open issues (as of 2026-03-02)

| Issue | Description | Action |
|-------|-------------|--------|
| #23 | Epic: OCI full compliance — console-socket + runtime-tools | Open; closes when #25 done |
| #25 | Run runtime-tools conformance suite and fix all failures | **Next task** |
| #37 | Zombie-keeper / PID stability — documented, non-blocking | Open marker; see #44 |
| #44 | pidfd-based process identity hardening | Future work (needs shim mgmt socket) |

### What was completed this session (2026-03-02)

**Epic #29 fully closed.** All PRs merged to main, branch cleaned up.

| PR | Content | Status |
|----|---------|--------|
| #38 | OCI linux.resources → cgroup wiring (memory swap/reservation/swappiness, cpuset, blkio, devices, net_cls) | ✅ merged |
| #43 | OCI lifecycle correctness: cmd_state persists "stopped", cmd_kill gates on state.json, ESRCH as success | ✅ merged |
| #46 | pid_start_time PID reuse detection: stores jiffies in state.json at create, compares at state/kill time | ✅ merged |
| #28 | fix/oci-runtimetools (killsig, seccomp, /dev nodes) | ✅ closed as superseded — all content already in main via #38/#43 |
| #29 | Epic: OCI linux.resources + lifecycle correctness | ✅ closed |

**Integration test counts on main (oci_lifecycle + cgroups):**
- `oci_lifecycle`: 23 pass
- `cgroups`: 15 pass
- Total integration suite: 141 pass / 39 fail (networking only — veth module, fixed by reboot)

### How to run runtime-tools (#25)

The `opencontainers/runtime-tools` binary must be built from source. The approach used
in the previous PR #28 cycle:

```bash
# Build runtime-tools (requires Go)
git clone https://github.com/opencontainers/runtime-tools /tmp/runtime-tools
cd /tmp/runtime-tools && make all

# Run against remora
sudo -E ./validation/run_ocitest.sh remora /path/to/bundle
```

Or use the individual test binaries under `validation/`. The test runner expects the
runtime to accept `--bundle`, `--console-socket`, `--pid-file` flags (all implemented).

**Known constraint:** runtime-tools `runtimetest` has stub cgroupv2 implementations —
every `linux_cgroups_*` test returns `"cgroupv2 is not supported yet"`. This system is
cgroupv2-only. Those tests will never pass via runtime-tools regardless of remora's
behavior. Verify cgroup compliance through remora's own integration tests instead.

### PID reuse / zombie-keeper context (issue #37)

The OCI spec requires `state.pid` to be stable from container exit until `remora delete`.
Without holding a zombie, the PID can be reused. Three mitigations are in place:

1. **cmd_kill** gates on `state.json` status only (not `kill(pid,0)`), so it isn't fooled.
2. **cmd_state** detects zombies and writes "stopped" to disk when found.
3. **pid_start_time** (jiffies from `/proc/<pid>/stat` field 22) stored in `state.json`
   at create time; compared at state/kill time — mismatch = PID reused, treat as stopped.

True zombie-keeper doesn't work with PID namespace containers (double-fork means
`state.pid` is adopted by host init, not the shim). Issue #37 is documented as
non-blocking. Issue #44 tracks the pidfd-based approach (needs shim management socket).

---

## Completed: Epic #29 — OCI linux.resources + lifecycle correctness (2026-03-01/02)

All sub-issues #31–#36, #39–#42, #45 closed. Epic #29 closed. See PRs #38, #43, #46.

---

## Completed: remora exec PID namespace join (2026-02-28)

### Context

`remora exec` did not join the container's PID namespace. When a PID namespace is
active, `state.pid` is the intermediate process P whose `/proc/P/ns/pid` is the host
PID namespace. The fix uses `/proc/P/ns/pid_for_children` as a fallback in
`discover_namespaces`, and implements a double-fork in `container.rs` step 1.65 to
actually enter the target namespace (since `setns(CLONE_NEWPID)` alone only updates
`pid_for_children` — the calling process is not moved; only a subsequent fork enters
the new namespace, followed by exec).

GitHub issue: #1 (closed by this work).

### Files changed

- `src/cli/exec.rs`: `discover_namespaces` — `pid_for_children` fallback
- `src/container.rs`: step 1.65 Case B — PID namespace join double-fork (both
  `spawn()` and `spawn_interactive()` pre-exec hooks)
- `tests/integration_tests.rs`:
  - `build_exec_command` helper updated with `pid_for_children` fallback
  - new test `exec::test_exec_joins_pid_namespace`
- `docs/WATCHER_PROCESS_MODEL.md`: updated caveat section, marked limitation fixed
- `docs/INTEGRATION_TESTS.md`: added `test_exec_joins_pid_namespace` entry

---

## Completed: watcher subreaper (2026-02-28)

### Context

When a container uses a PID namespace, the watcher forks an intermediate process P
which then forks the container C.  If the watcher was killed unexpectedly (OOM, etc.),
P was re-parented to host PID 1 rather than the watcher.  P's `PR_SET_PDEATHSIG`
(SIGKILL to C) depends on P's parent dying — but after re-parenting to init, that
signal never fires and C becomes an orphan.

The fix calls `prctl(PR_SET_CHILD_SUBREAPER, 1)` in the watcher (and compose
supervisor) immediately after `setsid()`.  This makes the watcher the reaper for all
orphaned descendants; if the watcher is killed, P is re-parented to the watcher not to
init, and P's pdeathsig fires in one hop when the watcher exits.

GitHub issue: #5 (closed by this work).

### Files changed

- `src/cli/run.rs`: added `prctl(PR_SET_CHILD_SUBREAPER, 1)` after `setsid()` in
  the watcher child branch
- `src/cli/compose.rs`: added `prctl(PR_SET_CHILD_SUBREAPER, 1)` after `setsid()` in
  both the daemonize path (line ~220) and the foreground-with-hooks path (line ~347)
- `tests/integration_tests.rs`: new module `watcher`, new test
  `test_watcher_kill_propagates_to_container`
- `docs/WATCHER_PROCESS_MODEL.md`: marked limitation fixed, updated signal propagation
  prose and known-limitations table
- `docs/INTEGRATION_TESTS.md`: added `test_watcher_kill_propagates_to_container` entry

---

## Completed: health probe timeout SIGKILL (2026-02-28)

### Context

When a health probe timed out, the probe child process was abandoned — the probe
thread was left running until the OS cleaned it up. This left a stray process and
consumed a thread slot in the watcher indefinitely.

The fix introduces `exec_in_container_with_pid_sink` in `src/cli/exec.rs`, which
stores the spawned child's host PID into an `Arc<AtomicI32>` immediately after
`spawn()` (before blocking on `wait()`). `run_probe` in `health.rs` passes this
sink to the probe thread. On `recv_timeout`, the monitor reads the PID from the
shared atomic and sends `SIGKILL`, ensuring the child is cleaned up immediately.

GitHub issue: #2 (closed by this work).

### Files changed

- `src/cli/exec.rs`: new `exec_in_container_with_pid_sink` + `Arc<AtomicI32>` import
- `src/cli/health.rs`: `run_probe` updated to pass pid sink + SIGKILL on timeout
- `tests/integration_tests.rs`: new test
  `healthcheck_tests::test_probe_child_pid_is_killable`
- `docs/WATCHER_PROCESS_MODEL.md`: marked probe-timeout limitation as fixed
- `docs/INTEGRATION_TESTS.md`: added `test_probe_child_pid_is_killable` entry

---

## Completed: epoll log relay (2026-02-28)

### Context

Each watcher previously spawned two dedicated relay threads (one for stdout, one
for stderr). This cost 2 threads per container at steady state. The fix replaces
both with a single `epoll`-based relay thread in `src/cli/relay.rs` that
multiplexes both pipe fds via `epoll_wait`, reducing the static thread count per
container from 3 to 2 (main + relay, down from main + stdout relay + stderr relay).

GitHub issue: #3 (closed by this work).

### Files changed

- `src/cli/relay.rs`: new module — `start_log_relay`, `relay_loop` (epoll), 3 unit
  tests
- `src/cli/mod.rs`: added `pub mod relay;`
- `src/cli/run.rs`: replaced two relay `thread::spawn` + two `join` calls with
  `super::relay::start_log_relay`; removed unused `Read` import
- `src/cli/compose.rs`: replaced two relay `thread::spawn` calls with
  `super::relay::start_log_relay`; removed unused `Read` import
- `docs/INTEGRATION_TESTS.md`: added entries for all three relay unit tests

---

## Completed: UDP proxy thread joining (2026-02-28)

### Context

UDP proxy threads (one per mapped port, plus one per active client session) were
never explicitly joined. `teardown_network` set the stop flag but returned
immediately; threads exited within 100ms on their own. This meant the inbound
socket was still held briefly after teardown returned, and reply threads had no
explicit synchronisation point.

The fix stores per-port `JoinHandle`s in `NetworkSetup.proxy_udp_threads`.
`teardown_network` now drains and joins them after setting the stop flag, ensuring
the inbound socket is released before the function returns.  `start_udp_proxy`
accumulates reply-thread handles and joins them all after its main loop exits (once
the stop flag causes the loop to terminate), completing the cleanup chain.

GitHub issue: #4 (closed by this work).

### Files changed

- `src/network.rs`:
  - `NetworkSetup`: added `proxy_udp_threads: Vec<JoinHandle<()>>`
  - `start_port_proxies`: changed return type to 3-tuple; collects per-port handles
  - callsite: destructured 3-tuple, stored `proxy_udp_threads` in `NetworkSetup`
  - `teardown_network`: joins per-port threads after setting stop flag
  - `start_udp_proxy`: collects reply handles, prunes finished ones, joins remainder
  - secondary `NetworkSetup` literal: added `proxy_udp_threads: Vec::new()`
- `tests/integration_tests.rs`: new test
  `networking::test_udp_proxy_threads_joined_on_teardown`
- `docs/INTEGRATION_TESTS.md`: added entry
- `docs/WATCHER_PROCESS_MODEL.md`: marked limitation as fixed

---

## All issues resolved

All four open issues are now closed.

---

## Runtime Strategy Analysis (2026-02-28)

A strategic analysis of the container runtime landscape, remora's position, and prioritized
technical opportunities has been written to:

**`docs/RUNTIME_STRATEGY_2026.md`**

Key findings:

- Remora is structurally immune to the November 2025 runc TOCTOU CVE cluster
  (CVE-2025-31133, CVE-2025-52565, CVE-2025-52881) — worth documenting loudly.
- Top gaps vs production runtimes: AppArmor/SELinux support; OCI lifecycle completeness.
- Top differentiation opportunities: Landlock LSM (first Rust runtime), crates.io
  publication for AI agent embedding, `SECCOMP_RET_USER_NOTIF` supervisor mode.
- Performance target: ≤ 180 ms median cold-start (between crun ~153 ms and youki ~198 ms).

See the doc for the full runtime comparison matrix, CVE analysis, Wasm/WASI trends,
embedded/IoT landscape, and the ranked opportunity list.

---

## Completed: OCI Runtime Spec compliance Phases 1–6 (2026-02-28)

Epic issue: #11 (closed).

### Summary

Full OCI lifecycle compliance implemented across 6 phases, merged to main as PRs #18–22.

| Phase | Content | PR |
|-------|---------|-----|
| 1 | `--bundle`, `--console-socket`, `--pid-file` CLI flags | #18 |
| 2 | Kernel mount type dispatch (proc, sysfs, devpts, mqueue, cgroup2) | #19 |
| 3+4 | Complete cap/signal tables, annotations, double-proc-mount fix, tmpfs flag fix | #20 |
| 5 | `linux.rootfsPropagation` + `linux.cgroupsPath` | #22 (rebased) |
| 6 | `createContainer`/`startContainer` hooks in container namespace | #22 |

### Key bugs fixed

- **`OciHooks` serde rename**: `OciHooks` was missing `#[serde(rename_all = "camelCase")]`,
  causing `createContainer` / `startContainer` / `createRuntime` hook arrays to be silently
  ignored on deserialization (JSON key `"createContainer"` never matched field `create_container`).
  Fixed by adding the attribute — the root cause of hook test failures.

- **Double proc mount**: `build_command` auto-added `with_proc_mount()` when a mount namespace
  was requested, but OCI bundles that already list a `proc`-type mount caused a double-mount
  failure. Fixed with an `has_explicit_proc` guard.

- **tmpfs flag vs data**: OCI mount options like `nosuid`, `strictatime` were passed as the
  `data` string argument to `mount(2)` instead of the flags argument, causing `EINVAL`.
  Fixed by parsing known MS_* flag names out of options before calling `with_kernel_mount`.

### 18 OCI lifecycle integration tests all pass.
