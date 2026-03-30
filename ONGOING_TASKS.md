# Ongoing Tasks

## Completed: system prune / system df (#126, #127) — 2026-03-29, HEAD: 80a92e6

- `pelagos system prune [--all] [--volumes]` and `pelagos system df` implemented
- `ensure_blob()` fix for image save/push when blob not retained after pull (#127)
- 5 integration tests in `mod system_prune` (pure-synthetic, no network)
- Mass `serial(nat)` audit: 20+ bridge/NAT/DNS/compose tests promoted from unnamed serial
- `ensure_alpine()` check-before-pull guard added across all tutorial test modules
- 306/306 integration tests pass (stable across consecutive runs)

---

## Completed: compose hardening (#160, #161, #169) — commit bfc24eb

- **#160**: `compose up` auto-pulls missing images; `--no-pull` flag to opt out
- **#161**: rollback on `compose up` failure (kill started containers, remove networks/DNS);
  stale supervisor state cleaned up before re-run
- **#169**: process-group kill (`kill(-pid, sig)`) so shell-entrypoint descendants die;
  `stop_grace_period` per-service; `STOPSIGNAL` from image config honoured
- All three landed in a single commit on `feat/compose-fixes-160-161-169`, merged to `main`
- Branch deleted (remote and local)

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
| #73 | feat(wasm): persistent Wasm VM pool (epic #67 P4) | feat/low-pri |
| #71 | feat(wasm): WASI preview 2 socket passthrough (epic #67 P2) | feat/low-pri |
| #70 | feat(wasm): mixed Linux+Wasm compose validation (epic #67 P1) | feat/low-pri |
| #67 | epic: Wasm/WASI deeper support | epic/low-pri |
| #62 | feat: minimal --features build for embedded/IoT | feat/low-pri |
| #61 | feat: CRIU checkpoint/restore support | feat/low-pri |
| #49-47 | track: upstream runtime-tools test bugs | tracking |
| #60 | feat: io_uring opt-in seccomp profile | feat/low-pri |
