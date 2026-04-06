# Ongoing Tasks

## Session completed: 2026-04-06 (SHA e5b09c7, branch feat/rootless-subgid-reliability)

### Issue #195: fix rootless fuse-overlayfs mode=0 mkdir (COMPLETE)

**Root cause identified and fixed:**

The previous "launcher" approach created fuse-overlayfs in a sibling user namespace
to the container's. The kernel's `fuse_allow_current_process()` checks
`current_in_userns(fc->user_ns)` — this returns false for sibling user namespaces,
causing all fuse access to fail with EACCES (OS error 13).

**Fix:** Remove the pre-fork launcher entirely. Fork fuse-overlayfs **inline in
pre_exec** after `CLONE_NEWUSER+CLONE_NEWNS`, so it inherits the container's own
user namespace. FUSE mount lives in container's private mount namespace; cleanup
is automatic on container exit.

**Removed:** `FuseOverlayRootless` struct, `spawn_fuse_overlay_rootless()` function,
`fuse_fd_raw`/`fuse_proc_merged` variables, launcher lifecycle fields in `Child`.

**Test:** `test_rootless_overlay_mode0_mkdir_succeeds` now passes.
**Full suite:** 300/324 pass (15 pre-existing failures confirmed by git stash check).

### Next: investigate 15 integration test failures, then merge

Full suite on this branch: 300/324 pass, 15 fail. Need to determine whether these
failures are pre-existing on the branch or caused by our session's changes.

**Plan (non-destructive):**
1. `git stash` — save working tree changes
2. `git switch main` — move to main (no file modifications)
3. `sudo scripts/reset-test-env.sh`
4. `sudo -E cargo test --test integration_tests 2>&1 | tail -5`
5. `git switch feat/rootless-subgid-reliability`
6. `git stash pop` — restore our changes

If main passes all tests cleanly, the 15 failures are attributable to this branch
(not our session). They likely predate our session and were already present at
`ff3e9cd` (the commit we started from today).

Failing tests observed:
- capabilities: test_capability_dropping, test_selective_capabilities, test_cap_drop_all_zeros_caps
- security: test_combined_phase1_security, test_hardening_combination
- compose: test_compose_cap_add_chown, test_compose_cap_add_chown_denied_without_cap
- core: test_combined_features
- dns: test_dns_multi_network, test_dns_network_isolation, test_dns_upstream_forward
- ipv6: test_ipv6_container_gets_address, test_ipv6_port_forward_localhost
- networking: test_nat_end_to_end_tcp
- build: test_build_apt_install_ca_certificates (passes when run alone — flaky under load?)

Once baseline is confirmed, either fix the failures or note them as branch-level
issues to resolve before the PR.

**Known hang:** `test_tut_p3_seccomp` hangs indefinitely on this host (observed
2026-04-06 on main). Marked `#[ignore]` in integration_tests.rs on this branch;
root cause TBD. Needs to land on main too before merge.

---

## Session completed: 2026-04-02 (SHA 03cff6d)

### Issues resolved this session

| # | Title | Fixed in |
|---|-------|---------|
| — | fix(test): IPv6 NAT test — ping -6 modern invocation + DAD wait | commit 86cda80 |
| — | feat(run): auto-select network default; imply NAT with bridge | commit 03cff6d |
| — | docs: USER_GUIDE, CLAUDE.md, README updated to reflect new defaults | this session |

### Key implementation details

**Smart network auto-default (commit 03cff6d):**
- Root → `NetworkMode::Bridge` + NAT implied (no `--network` or `--nat` needed)
- Rootless + pasta available → `NetworkMode::Pasta`
- Rootless + no pasta → `NetworkMode::Loopback` + warning
- `--no-nat` flag added to suppress implied NAT (routed prefixes, no masquerade)
- `no_nat: bool` added to `SpawnConfig` (serde default+skip) for restart compat
- Scripts and tests updated: removed redundant `--network bridge --nat` flags
- Integration suite: 312/312 passing

**IPv6 NAT test fix (commits 6079234, 86cda80):**
- `host_has_ipv6()` now tries `ping -6` first (modern), `ping6` as legacy fallback
- `test_ipv6_outbound_nat` adds DAD spin-wait before ping (avoids tentative-address silent drops)

---

## Session completed: 2026-04-01 (SHA 088646d)

### Issues resolved this session

| # | Title | Fixed in |
|---|-------|---------|
| #185 | feat(network): IPv6 dual-stack for bridge networks | PR #186 |

### Key implementation details

**#185 — IPv6 dual-stack (PR #186):**
- ULA /64 prefix derived deterministically from FNV-1a hash of network name — no new stored
  fields in `NetworkDef` JSON (fully backward-compatible)
- `network_ipv6_ipam_file(name)` added to `paths.rs`; `allocate_ipv6()` in `network.rs`
  uses flock-serialized counter in `next_ipv6` per-network runtime file
- `ensure_bridge()`: assigns `{gw6}/64` to bridge device after IPv4 setup (idempotent)
- `setup_ipv6_container()`: allocates addr, `ip -6 addr add`, `ip -6 route add default`,
  `accept_ra=2` + `forwarding=1` sysctls (prevents host losing SLAAC default route)
- `setup_ipv6_secondary()`: same but no default route (for secondary interfaces)
- NAT66: `build_nat6_script()` generates nftables `ip6` table + MASQUERADE; added in
  `enable_nat()`; `disable_nat()` tears down `ip6` table and ip6tables FORWARD rules
- `PortForwardEntry` is now a 6-tuple `(ns, ip4, hp, cp, proto, Option<Ipv6Addr>)`;
  parser uses `splitn(6, ':')` with optional 6th field (old 5-field files parse as `None`)
- `build_prerouting6_script()`: IPv6 DNAT in `ip6` table (prerouting hook)
- `tcp_accept_loop_v6()`: tokio task binding `[::1]:host_port`; reuses `tcp_relay()`
- `start_udp_proxy_v6()`: std-thread UDP relay binding `[::1]:host_port`; mirrors IPv4 proxy
- 3 integration tests in `mod ipv6`: address assignment, outbound NAT66 (guarded), localhost proxy
- 308/308 integration tests pass; 321/321 unit tests pass

---

## Session completed: 2026-03-31 (SHA 023ef35)

### Issues resolved this session

| # | Title | Fixed in |
|---|-------|---------|
| #179 | feat(build): auto-pull FROM base images (match docker build behaviour) | PR #181 |
| #183 | fix(network): ip netns del EBUSY — retry loop + lazy-unmount fallback | PR #184 |
| #180 | docs: AVF NAT blocks external port 53 | Closed — was stale; smoltcp relay fixed DNS as side-effect of PR #117 |

### Key implementation details

**#179 — `pelagos build` auto-pull (PRs #181, #182):**
- `execute_build` / `execute_stage` accept `Option<PullFn<'_>>` callback
- `PullFn<'a>` type alias introduced to satisfy `clippy::type_complexity`
- When `load_image` fails for a FROM reference, pull callback is invoked; load retried after pull
- CLI `build.rs` passes a closure that calls `cmd_image_pull`
- All 24 `execute_build` call sites in integration tests pass `None`
- `test_tut_p2_multistage_go_build` pre-pull boilerplate removed (now relies on auto-pull)

**#183 — `delete_netns()` retry loop (PR #184):**
- Replaces single `ip netns del` + naive `remove_file` fallback
- 10 retries × 100 ms = 1 s budget; kernel veth teardown races resolved in the common case
- If all retries fail: `umount2(MNT_DETACH)` (Linux-gated) + `unlink` as last resort
- Ensures `netns_exists()` always returns false post-teardown → NAT/port-forward refcounts accurate
- Fixes 5 previously-failing tests: `test_nat_cleanup`, `test_nat_refcount`,
  `test_bridge_cleanup_after_sigkill`, `test_port_forward_cleanup`,
  `test_port_forward_independent_teardown`

**#180 — DNS investigation:**
- Issue attributed DNS failure to AVF NAT; investigation showed VM uses smoltcp relay (PR #117), not VZNATNetworkDeviceAttachment
- Root cause of original failure: `VZNATNetworkDeviceAttachment` degrades after ~5 VM boots (PF anchor lost); DNS was collateral damage
- Confirmed 2026-03-31: both UDP/53 and TCP/53 work from inside the VM via the relay
- `test_dns_upstream_forward` confirmed passing (not self-skipping) inside the VM
- Issue closed as resolved by PR #117

### PR #178 — pending merge

`fix(build+tests): inject HOME in RUN containers; fix all 6 ignored tests`
- Injects `HOME=/root` in build RUN containers (matches Docker behaviour; fixes Go/pip/npm)
- Fixes all 6 `#[ignore]`-tagged tests: consistent `alpine:3.21` refs, pre-pull guards
- CI passing (lint fmt fix pushed 2026-03-31); merge after CI green

---

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
