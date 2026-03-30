# Ongoing Tasks

## Completed: system prune / system df (#126, #127) — 2026-03-29, HEAD: 80a92e6

- `pelagos system prune [--all] [--volumes]` and `pelagos system df` implemented
- `ensure_blob()` fix for image save/push when blob not retained after pull (#127)
- 5 integration tests in `mod system_prune` (pure-synthetic, no network)
- Mass `serial(nat)` audit: 20+ bridge/NAT/DNS/compose tests promoted from unnamed serial
- `ensure_alpine()` check-before-pull guard added across all tutorial test modules
- 306/306 integration tests pass (stable across consecutive runs)

---

## In-progress: feat/compose-fixes-160-161-169

### Issues targeted

| # | Title | Kind |
|---|-------|------|
| #160 | feat: compose should auto-pull missing images on `compose up` | feat |
| #161 | feat: compose should deregister ports on down/failure (Linux scope) | fix |
| #169 | fix: compose down should wait for volumes to flush before SIGKILL | fix |

Branch: `feat/compose-fixes-160-161-169`

---

### #169 — Process group kill + `stop_grace_period`

**Root cause (primary):** `cmd_compose_down` sends `SIGTERM`/`SIGKILL` to `svc_state.pid`,
which is the PID of the container's init process — typically a shell script entrypoint
(`/run.sh`). That shell does not forward `SIGTERM` to the real process (prometheus, grafana,
etc). When `SIGKILL` is sent, it kills the shell, but the actual prometheus binary is
orphaned — re-parented to the compose subreaper — and continues running, holding the TSDB
`flock()` on `/prometheus/lock`. The next `compose up` starts a new prometheus instance that
races with the still-running orphan for the lock → `resource temporarily unavailable`.

The 500ms post-SIGKILL delay does not help because prometheus is still alive. This is not a
timing problem; it is a kill scope problem.

**Root cause (secondary):** No per-service configurable `stop_grace_period`. Hardcoded 10s is
too short for some services, but the real benefit requires the pgid fix first — `stop_grace_period`
only helps processes that actually respond to SIGTERM.

---

**Changes (primary — pgid fix):**

1. `src/container.rs` — early in the `pre_exec` closure, before Step 0 (cgroup self-assign)
   ```rust
   // Make every container process a process group leader so that
   // compose/stop can kill the entire subtree with kill(-pid, sig).
   libc::setpgid(0, 0);
   ```
   This is safe for all spawn paths (non-PTY and PTY). In PTY mode `setsid()` runs
   immediately after and supersedes it; in all other cases it guarantees pgid == pid.

2. `src/cli/compose.rs` — `cmd_compose_down`
   - Change `kill(svc_state.pid, SIGTERM)` → `kill(-svc_state.pid, SIGTERM)`
   - Change `kill(svc_state.pid, SIGKILL)` → `kill(-svc_state.pid, SIGKILL)`
   - Also fix the fallback SIGKILL for services not in topo order (line ~787)
   - Increase post-SIGKILL delay from 500ms to 2000ms (belt-and-suspenders)

**Changes (secondary — `stop_signal` from image config):**

`STOPSIGNAL` is a Dockerfile instruction (OCI `Config.StopSignal` field, e.g. `"SIGQUIT"` for
nginx graceful drain) that specifies which signal to send instead of SIGTERM. Currently pelagos
does not read it.

3. `src/image.rs` — `ImageConfig`
   ```rust
   #[serde(default)]
   pub stop_signal: String,   // "" means SIGTERM (absent in most images)
   ```

4. `src/cli/image.rs` — `parse_image_config`
   ```rust
   let stop_signal = container_config
       .and_then(|c| c.get("StopSignal"))
       .and_then(|v| v.as_str())
       .unwrap_or("")
       .to_string();
   ```
   Add `stop_signal` to the `ImageConfig { ... }` return.

5. `src/cli/compose.rs` — `spawn_service` / `cmd_compose_down`
   - `spawn_service` should record the resolved stop signal per service. Since
     `ComposeServiceState` only stores container_name/status/pid, and `cmd_compose_down`
     re-reads the compose file for topo order anyway, the cleanest approach is: in
     `cmd_compose_down`, after resolving the grace period map, also resolve a
     `stop_signal_map` by loading each service's image manifest and reading
     `manifest.config.stop_signal`. Parse the signal name/number to a `libc::c_int`.
   - Fall back to `SIGTERM` if absent, unparseable, or image manifest unavailable.
   - Helper: `fn parse_signal(s: &str) -> libc::c_int` — handles `"SIGTERM"`, `"15"`,
     `"SIGQUIT"`, `"3"`, etc.

**Changes (tertiary — `stop_grace_period`):**

6. `src/compose.rs`
   - Add `stop_grace_period: Option<u64>` to `ServiceSpec` (seconds; `None` = default 10s)
   - Add `"stop-grace-period"` arm in `parse_service_spec` match

7. `src/lisp/pelagos.rs` — `apply_service_opt`
   - Add `"stop-grace-period"` arm accepting an integer `Value::Int`

8. `src/cli/compose.rs` — `cmd_compose_down`
   - Build `grace_map` from re-read compose file
   - Use `grace_map.get(svc_name).copied().unwrap_or(10)` instead of hardcoded `10`

**Tests:**
- Integration: `test_compose_down_kills_shell_entrypoint_descendants` — container whose
  entrypoint is a shell script that forks a long-running child (`sh -c "sleep 999 & wait"`);
  compose down; assert the sleep child is also dead. Direct regression test for the pgid fix.
- Unit `test_parse_image_config_stop_signal` — JSON with `"StopSignal": "SIGQUIT"`, assert
  `config.stop_signal == "SIGQUIT"`; JSON without the field, assert `config.stop_signal == ""`
- Unit `test_parse_signal` — `"SIGTERM"` → 15, `"SIGQUIT"` → 3, `"9"` → 9, `""` → 15 (default)
- Unit `test_compose_parse_stop_grace_period` — parse `(stop-grace-period 30)`, assert field
- Unit `test_service_builtin_stop_grace_period` — Lisp builtin sets the field

**Docs:** `docs/USER_GUIDE.md` — add `(stop-grace-period N)` to the service options table.

---

### #161 — Compose up failure cleanup (Linux scope)

**Scope note:** The macOS PortDispatcher changes described in the issue comment are
out of scope here (separate repo, separate daemon). Linux scope:
- On `compose up` failure mid-run, partially-created state (containers, networks, DNS)
  is not cleaned up, blocking re-runs.
- On `compose up` with a stale state file (supervisor dead), the code silently proceeds
  without cleaning up dangling container state or networks from the previous failed run.

**Root cause analysis:**

- `run_compose_with_hooks` creates networks before forking into the supervisor, then spawns
  services one by one. If service N fails, services 1..N-1 are running and networks exist.
  On error return, nothing is torn down.
- `cmd_compose_up_reml` detects dead supervisor (via `check_liveness`) and proceeds, but
  does not clean up stale container dirs, network state, or DNS entries.

**Changes:**

1. `src/cli/compose.rs` — `run_compose_with_hooks`
   - After the service startup loop, if an error occurs, clean up before returning:
     - SIGKILL all `container_pids` that were started
     - Remove their container state dirs via `container_dir(&cn)`
     - Remove DNS entries via `dns_remove_entry`
     - Remove created networks via `cmd_network_rm` for each in `created_networks`
     - This makes compose-up atomic from the user's perspective
   - Implement as a cleanup closure/function called on the `Err` path

2. `src/cli/compose.rs` — `cmd_compose_up_reml`
   - After detecting dead supervisor (state file exists, supervisor not alive):
     ```rust
     // Stale state from previous failed/crashed run — clean up.
     cleanup_stale_project(&existing);
     ```
   - `cleanup_stale_project` kills living containers in stale state, removes their dirs,
     removes networks, removes DNS entries, removes the project state file

**Tests:**
- Integration: `test_compose_up_failure_cleans_up` — compose with service 1 = valid alpine image,
  service 2 = deliberately nonexistent image (with `--no-pull`). Assert: compose up returns error,
  networks are removed, no container state dirs remain.
- Integration: `test_compose_up_restart_after_failure` — same setup; first run fails; second run
  (with corrected config: both valid images) succeeds without manual `compose down`.

---

### #160 — Auto-pull missing images

**Root cause:** `resolve_image()` returns an error immediately if the image is not in the local
store. Docker Compose pulls missing images automatically (`--pull missing` default).

**Changes:**

1. `src/cli/compose.rs`
   - Add `--no-pull` flag to `ComposeCmd::Up` (clap boolean, default false):
     ```rust
     #[clap(long)]
     no_pull: bool,
     ```
   - Thread `no_pull` through `cmd_compose_up` → `cmd_compose_up_reml`
   - Add `pull_missing_images(spec: &ComposeFile, no_pull: bool)` called upfront in
     `cmd_compose_up_reml`, before network/volume creation and before forking. This ensures:
     - All images are available before any containers start (fail-fast)
     - Pull errors are printed to the terminal (parent process, before fork)
     - Per-service pull progress is shown: `Pulling <image>...`
   - Modify `resolve_image` to accept `no_pull: bool`; when image not found and `!no_pull`,
     call `super::image::cmd_image_pull(image_ref, None, None, false, false)` then retry
   - `pull_missing_images` iterates `spec.services`, deduplicates image refs, calls
     `resolve_or_pull_image` for each — stops early and returns the first pull error

**Test for upfront deduplication:** Multiple services using the same image should only trigger
one pull attempt.

**Tests:**
- Unit: `test_compose_pull_deduplicates_images` — `pull_missing_images` called with two services
  sharing the same image ref; only one pull happens (use counter or verify via mock)
  - Actually this is hard to unit-test without mocking; instead test via the integration path
- Integration: `test_compose_no_pull_fails_immediately` — compose with an image not in local cache
  and `--no-pull` flag; assert error message contains "not found locally" and no pull was attempted
- Integration: `test_compose_auto_pull_on_up` — compose with a real pullable image not in cache;
  assert it's pulled and visible in `pelagos image ls` afterwards. Tag as `#[serial]` to avoid
  concurrent ECR pulls.

**Note on `--no-pull`:** With `--no-pull`, the behaviour is exactly what it is today — fail
immediately if the image is not local. This is the right default for air-gapped environments.

---

### Implementation order

1. #169 (standalone struct + parser changes, no moving parts)
2. #160 (builds on working compose up, tests require pulls)
3. #161 (cleanup logic; last because it changes error paths touched by #160 tests)

Tests for each issue ship in the same commit as the code.

---

## Session completed: 2026-03-29 (SHA 80a92e6)

### Issues resolved this session

| # | Title | Fixed in |
|---|-------|---------|
| parallel test flakiness | fix: eliminate integration test flakiness under parallel execution | 80a92e6 |
| #126 | feat: `pelagos system prune / system df` | b1517ae |
| #127 | fix(storage): do not retain blobs after layer unpack | b6529fb |

### Key implementation details

**Parallel integration test flakiness (80a92e6):**
Three root causes identified and fixed:
1. **ECR/Docker Hub rate limits**: Multiple `ensure_alpine()` helpers unconditionally called
   `pelagos image pull` even when image was cached. 20+ concurrent calls hit rate limits.
   Fix: check `pelagos image ls` first, return early if image already present.
2. **Serial-key group mismatch**: Basic bridge tests had no `#[serial]` attribute at all —
   ran in parallel with NAT tests. Compose tests used unnamed `#[serial]` — could run
   concurrently with `#[serial(nat)]` DNS tests. Fix: all networking-touching tests moved
   to `#[serial(nat)]` key.
3. **Watcher cleanup race in `pelagos rm --force`**: `cmd_rm` removed state dir before
   watcher process finished nftables/veth cleanup. Fix: poll `state.watcher_pid` exit for
   up to 5s before removing state dir (`src/cli/rm.rs`).
Integration test baseline is now **306/306** passing in 3 consecutive parallel suite runs (~41s each).

---

## Session completed: 2026-03-29 (SHA ce8c503, v0.59.0 + #159)

### Issues closed this session

| # | Title | Fixed in |
|---|-------|---------|
| #159 | fix(lisp): inline `let` in `define-service` dotted-pair cdr | ce8c503 |
| #162 | dup of #163 — closed | — |

### Key implementation details

**#159 (inline let in define-service dotted-pair):**
- Root cause: `("K" . (let ...))` with a list-valued cdr produces identical `Value::Pair` chain
  to `("K" let ...)`. `(list? sub)` can't distinguish them; `(length sub)` can.
- Fix: `(and (list? sub) (= (length sub) 2))` in `expand-opt` in `stdlib.lisp`
- Four unit tests in `src/lisp/mod.rs`; new fixture `examples/compose/monitoring-inline-let/compose.reml`
- `home-monitoring/remora/compose.reml` updated to use inline let (removed top-level define workarounds)

---

## Session completed: 2026-03-19 (SHA 1e488ba, v0.59.0)

### Issues closed this session

| # | Title | Fixed in |
|---|-------|---------|
| #124 | fix(run): write state with real PID before relaying stdout | v0.59.0 |

### Key implementation details

**#124 (run state ordering race):**
- Two distinct races: (A) `run_foreground` — stdout Inherit let container output flow before `write_state(real_pid)`; (B) `run_detached` non-attach — parent exited before watcher wrote real PID
- Fix A: change stdout/stderr to `Stdio::Piped`, call `write_state(real_pid)` immediately after spawn, then start relay threads — data only flows after state is written
- Fix B: sync pipe (O_CLOEXEC) created before fork; watcher writes 1 byte after `write_state(real_pid)`; parent blocks on read before printing name / starting relay
- `run_detached` with `-a STDOUT/-a STDERR` was already correctly ordered (relay starts after write_state); sync pipe added there too for explicit guarantee and to avoid SIGPIPE
- Two integration tests: `test_run_foreground_state_written_before_output_issue_124`, `test_run_detached_state_ready_on_return_issue_124`

---

## Session completed: 2026-03-17 (SHA 21b80d7, v0.58.0)

### Issues closed this session

| # | Title | Fixed in |
|---|-------|---------|
| #121 | fix(exec): join container PID namespace — /proc/self no longer dangling | v0.58.0 |

### Key implementation details

**#121 (exec-into doesn't join PID namespace — /proc/self dangling):**
- Root cause: `cmd_exec` always skipped `setns(CLONE_NEWPID)` due to a misunderstanding — the "double-fork" limitation applies to *creating* a new PID namespace, not *joining* an existing one
- Fix: for root exec, save the PID ns path in the ns_entries loop, then call `setns(CLONE_NEWPID)` **in the parent process** right before `spawn()`. The fork inside `spawn()` then creates the child in the container's PID namespace
- Rootless exec still skips (PID ns owned by container's user ns; joining it in parent would change parent's credentials — known limitation)
- Updated `test_exec_joins_pid_namespace` to assert `readlink /proc/self/ns/mnt` exits 0 and returns `mnt:[...]`

---

## Session completed: 2026-03-17 (SHA fbefebc, v0.57.0)

### Issues closed this session

| # | Title | Fixed in |
|---|-------|---------|
| #120 | fix: always create /etc/hosts with localhost entries in containers | v0.57.0 |

### Key implementation details

**#120 (/etc/hosts missing — localhost unresolvable):**
- Both spawn paths (`spawn()` and `spawn_interactive()`/OCI) updated
- Condition changed from `!self.links.is_empty()` → `MOUNT namespace + chroot is active`
- Always writes Docker-compatible localhost block: `127.0.0.1 localhost`, `::1 localhost ip6-localhost ip6-loopback`, `fe00::0 ip6-localnet`
- If hostname is set, also adds `127.0.1.1 <hostname>` (mirrors Docker)
- Links still appended after the localhost block (no behaviour change for existing users of `with_link`)
- Two integration tests: `test_etc_hosts_localhost_present`, `test_etc_hosts_hostname_alias`

---

## Session completed: 2026-03-17 (SHA 5eee65c, v0.56.0)

### Issues closed this session

| # | Title | Fixed in |
|---|-------|---------|
| #118 | fix(run): redirect watcher stdio to /dev/null; pelagos start returns promptly | v0.56.0 |

### Key implementation details

**#118 (watcher stdio / pelagos start hangs):**
- Root cause: watcher child inherited pipe write-end; caller (SSH/vsock/Stdio::piped) blocked waiting for EOF
- Fix: after `setsid()` in watcher child, `open("/dev/null") + dup2` on stdin/stdout/stderr
- Releases all inherited pipe FDs; parent exits → caller sees EOF immediately
- Integration test `test_start_returns_promptly` reproduces via `Stdio::piped()`, asserts exit ≤ 2s

---

## Session completed: 2026-03-17 (SHA afb1b7f, v0.55.0)

### Issues closed this session

| # | Title | Fixed in |
|---|-------|---------|
| #117 | feat: -a/--attach + --sig-proxy for detached output streaming | v0.55.0 |
| #116 | feat: SpawnConfig.tmpfs; multi-name `pelagos start n1 n2` | v0.54.0 |
| #115 | fix(exec): load image-config ENV for exec'd processes | v0.53.0 |
| #114 | fix(run): preserve image-config PATH (apply_cli_options) | v0.52.0 |
| #112 | fix: CA cert EBUSY in build containers (overlay upper dir pre-seed) | v0.51.0 |
| #109 | fix: pelagos run finds locally-built images (FROM local-tag) | v0.34.0 (stale) |

### Key implementation details

**#117 (-a/--attach):**
- `RunArgs`: `--attach`/`-a STREAM` + `--sig-proxy` (Docker compat, ignored)
- `DetachedArgs` struct wraps run_detached args (clippy ≤7 limit)
- `pipe2(O_CLOEXEC)` before fork; watcher tees via `start_tee_relay`; parent relays to stdout/stderr
- Container name → stderr in attach mode; stdout clean for caller

**#116 (SpawnConfig.tmpfs + multi-name start):**
- `SpawnConfig.tmpfs: Vec<String>` — now persisted through restart
- `pelagos start n1 n2 ...` — starts sequentially; OCI fallback for single unknown ID

**#115 (exec image-config ENV):**
- `cmd_exec` loads `manifest.config.env` from `state.spawn_config.image`
- Merge: image_env base → proc/environ gaps filled → CLI `-e` wins

**#114 (image-config PATH):**
- Removed unconditional PATH from `apply_cli_options`
- `build_image_run`: inject fallback only if manifest.config.env omits it, then apply image env

**#112 (CA cert EBUSY):**
- Copy host CA cert to overlay upper dir in PARENT before fork (not bind-mount in pre_exec)

---

## Open Issues (GitHub)

| # | Title | Kind |
|---|-------|------|
| #160 | feat: compose auto-pull missing images | IN PROGRESS |
| #161 | feat: compose up failure cleanup (Linux scope) | IN PROGRESS |
| #169 | fix: stop_grace_period + post-SIGKILL delay | IN PROGRESS |
| #73 | feat(wasm): persistent Wasm VM pool (epic #67 P4) | feat/low-pri |
| #71 | feat(wasm): WASI preview 2 socket passthrough (epic #67 P2) | feat/low-pri |
| #70 | feat(wasm): mixed Linux+Wasm compose validation (epic #67 P1) | feat/low-pri |
| #67 | epic: Wasm/WASI deeper support | epic/low-pri |
| #62 | feat: minimal --features build for embedded/IoT | feat/low-pri |
| #61 | feat: CRIU checkpoint/restore support | feat/low-pri |
| #49-47 | track: upstream runtime-tools test bugs | tracking |
| #60 | feat: io_uring opt-in seccomp profile | feat/low-pri |
