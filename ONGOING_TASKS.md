# Ongoing Tasks

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

## Open GitHub issues (remaining work)

| # | Title |
|---|-------|
| #3 | Log relay: thread-per-fd model wastes 2 threads per container |
| #4 | UDP proxy: reply threads never explicitly joined |
