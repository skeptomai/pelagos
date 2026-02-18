# Ongoing Tasks

## Current Task: Full-Featured Container CLI — COMPLETE ✅

### What was implemented

**Library changes (`src/container.rs`):**
- Added `hostname: Option<String>` field to `Command` struct
- Added `with_hostname(name)` builder method — calls `sethostname(2)` in pre_exec after UTS unshare
- Added `Child::take_stdout()` and `Child::take_stderr()` — take piped handles before `wait()` for concurrent relay
- Captures `hostname` in both `spawn()` and `spawn_interactive()` pre_exec closures

**New CLI module (`src/cli/`):**
- `mod.rs`: `ContainerState` / `ContainerStatus` (serde), `write_state`, `read_state`, `list_containers`, `check_liveness`, `generate_name`, `parse_memory`, `parse_cpus`, `parse_user`, `parse_ulimit`, `parse_capability`, `format_age`, `now_iso8601`
- `run.rs`: `RunArgs` (clap struct), `cmd_run` — builds `container::Command` from flags, routes to foreground/interactive/detached
- `ps.rs`: walks `/run/remora/containers/*/state.json`, refreshes liveness, prints table
- `stop.rs`: reads state, sends SIGTERM, updates state.json
- `rm.rs`: optional SIGKILL (--force), removes container dir
- `logs.rs`: prints stdout.log + stderr.log; `--follow` polls for new content
- `rootfs.rs`: import (symlink), ls (read_dir + read_link), rm (remove_file)
- `volume.rs`: wraps `remora::container::Volume::{create, delete}`, walks volumes dir for ls

**Updated `src/main.rs`:**
- Replaced legacy `Run` subcommand with full-featured `Run(cli::run::RunArgs)`
- Added `Ps`, `Stop`, `Rm`, `Logs`, `Rootfs { Import | Ls | Rm }`, `Volume { Create | Ls | Rm }`
- OCI lifecycle commands unchanged

**Updated docs:**
- `docs/USER_GUIDE.md`: added full CLI section (rootfs management, lifecycle, all flags, storage layout)
- `CLAUDE.md`: updated file structure listing

### Storage Layout

```
/var/lib/remora/rootfs/<name>    → symlink to rootfs directory
/var/lib/remora/container_counter → monotonic u64 for auto-naming
/run/remora/containers/<name>/state.json
/run/remora/containers/<name>/stdout.log   (detached mode)
/run/remora/containers/<name>/stderr.log   (detached mode)
```

### Architecture: Detached Mode

Parent forks a watcher child. Parent prints name and exits. Watcher:
1. Spawns container with `Stdio::Piped`
2. Updates state.json with real PID
3. Two threads relay stdout/stderr to log files
4. `child.wait()` blocks
5. Updates state.json: status=exited, exit_code=N

---

## Previous Task: Rootless Phase 2 (Pasta Networking) — COMPLETE ✅

### Summary

Added `NetworkMode::Pasta` to provide rootless-compatible full internet access via the
`pasta` user-mode networking tool (from the passt project).

### What was implemented

- `NetworkMode::Pasta` variant in `src/network.rs`
- `PastaSetup` struct holding the pasta background process
- `setup_pasta_network(child_pid, port_forwards)` — spawns pasta after child exec'd
- `teardown_pasta_network(setup)` — kills pasta relay on container exit (best-effort)
- `is_pasta_available()` — PATH check, used for validation and test skipping
- `Child::pasta` field; teardown called in `wait()` and `wait_with_output()`
- Auto-adds `Namespace::NET` and enables `bring_up_loopback` for pasta mode
- Pasta validation in both `spawn()` and `spawn_interactive()`
- Rootless bridge rejection error updated to mention `NetworkMode::Pasta`
- 3 integration tests: `test_pasta_interface_exists`, `test_pasta_rootless`,
  `test_pasta_connectivity` (all skip gracefully when pasta is not installed or no internet)
- `--config-net` flag passed to pasta so IP and routing are auto-configured in the container
- `test_pasta_loopback_up` removed (was testing our code, not pasta; replaced by connectivity test)
- `docs/INTEGRATION_TESTS.md` updated with "Pasta Networking Tests" section
- `docs/ROADMAP.md` updated: Rootless Phase 2 marked complete, parity table updated
- `CLAUDE.md` updated: test count 64→67, pasta in feature list and comparison table

---

## Rootless Phase 1 — COMPLETE ✅

### OCI Phase 2 — COMPLETE ✅

All fields implemented and tested (61 integration tests passing):

- `process.capabilities` → `with_capabilities()`
- `linux.maskedPaths` → `with_masked_paths()`
- `linux.readonlyPaths` → `with_readonly_paths()`
- `linux.resources` → `with_cgroup_memory()` / `with_cgroup_cpu_*()` / `with_cgroup_pids_limit()`
- `process.rlimits` → `with_rlimit()`
- `linux.sysctl` → `with_sysctl()` (new builder; writes to `/proc/sys/` in pre_exec)
- `linux.devices` → `with_device()` (new builder; `mknod` in pre_exec)
- `hooks.prestart` / `poststart` / `poststop` → `run_hooks()` in `cmd_create/start/delete`
- `linux.seccomp` → `filter_from_oci()` in `src/seccomp.rs` → `with_seccomp_program()`

---

### Rootless Mode Phase 1 — COMPLETE ✅

- Auto-detect non-root (`getuid() != 0`)
- Auto-add `Namespace::USER` + default uid/gid map (`{0 → host_uid}`)
- `NetworkMode::Loopback` works rootless (USER+NET namespace)
- Cgroups skipped gracefully (EPERM in rootless)
- `NetworkMode::Bridge` rejected with clear error
- Bug fix: uid_map writing was missing from `spawn_interactive()` pre_exec
- 4 new integration tests (3 skip-if-root, 1 requires root)

---

---

## Current Task: Rootless Phase 2 — pasta Networking 🔄

Full internet access for rootless containers via user-mode networking (`pasta` from the
passt project). `pasta` is not yet installed; tests skip gracefully when absent.

### Architecture

- **No named netns**: `ip netns add` requires root. pasta attaches to the child's netns
  via `/proc/{child_pid}/ns/net` after `spawn()` returns.
- **Pre-exec**: unshare(CLONE_NEWNET) + bring_up_loopback() — same as Loopback mode.
- **Post-spawn**: parent calls `setup_pasta_network(child_pid, port_forwards)`, which
  spawns `pasta /proc/{pid}/ns/net [-t HOST:CONTAINER ...]` in the background.
- **Teardown**: `wait()` kills the pasta process and waits for it (best-effort).

### Changes Required

**`src/network.rs`:**
- Add `NetworkMode::Pasta` variant
- Add `PastaSetup { process: std::process::Child }` struct
- Add `setup_pasta_network(child_pid: u32, port_forwards: &[(u16,u16)]) -> io::Result<PastaSetup>`
- Add `teardown_pasta_network(setup: &mut PastaSetup)` — kill + wait
- Add `is_pasta_available() -> bool` — checks `pasta --version`

**`src/container.rs`:**
- Detect `is_pasta` flag from `network_config`
- Validate pasta binary exists, auto-add `Namespace::NET`
- Update rootless bridge-rejection error: mention `NetworkMode::Pasta`
- Set `bring_up_loopback = true` for pasta mode (loopback always up)
- After `cmd.spawn()` returns: call `setup_pasta_network(child.id(), &port_forwards)`
- Add `pasta: Option<PastaSetup>` to `Child` struct
- Kill pasta in `wait()`, `wait_with_output()`, and `spawn_interactive()`

**`tests/integration_tests.rs`** — 3 new tests (skip if pasta absent):
- `test_pasta_interface_exists` — root, asserts TAP interface visible in `ip addr show`
- `test_pasta_loopback_up` — root, asserts loopback is LOOPBACK,UP
- `test_pasta_rootless` — non-root, asserts TAP interface present

**Docs:** INTEGRATION_TESTS.md (3 new entries), ROADMAP.md (mark complete), CLAUDE.md (64 → 67 tests).

### pasta CLI
```
pasta /proc/{child_pid}/ns/net [-t HOST:CONTAINER ...] [--quiet]
```
Exact flags confirmed once `pasta` is installed. `--quiet` may be `-q` in some versions.

### Port Forwarding
`with_port_forward(host, container)` entries are passed as `-t host:container` flags.
TCP only (matching existing semantics). UDP support is a follow-up.

---

## Planned (Deferred)

### AppArmor / SELinux — MAC Profile Support

Deferred: the seccomp + capabilities + masked paths stack is already solid, and MAC requires
system-side setup (profile loading) that most users won't have. Revisit if there's demand.

#### How it works

Both AppArmor and SELinux are Linux Security Modules (LSMs). From Remora's perspective the
mechanism is identical: write a profile name / label to `/proc/self/attr/exec` in `pre_exec`
before the container execs. The kernel's LSM enforces it at exec time.

- **AppArmor**: profile-based, path-centric rules (`/etc/passwd r,`). Profile identified by
  name (e.g. `docker-default`). Common on Debian/Ubuntu/openSUSE. Arch supports it with the
  `apparmor` package but it is not enabled by default.
- **SELinux**: label-based type enforcement (`system_u:system_r:container_t:s0`). Common on
  RHEL/Fedora/CentOS. Essentially unused on Arch.

Only one LSM is active at a time. Implementing both is trivial (same write, different string
format); testing requires a host with the LSM active and profiles loaded.

#### Proposed API

```rust
// AppArmor — profile must be loaded externally (aa-genprof / aa-complain)
.with_apparmor_profile("docker-default")

// SELinux — label must be valid on the host
.with_selinux_label("system_u:system_r:container_t:s0")
```

#### Implementation sketch

**`src/container.rs` pre_exec** (after capabilities, before seccomp):
```rust
if let Some(ref profile) = self.apparmor_profile {
    let path = "/proc/self/attr/exec";
    // ENOENT / EINVAL means AppArmor is not active — treat as error (don't silently skip)
    std::fs::write(path, format!("exec {}", profile))
        .map_err(|e| io::Error::other(format!("apparmor write failed: {}", e)))?;
}
```

SELinux is identical but the string format is the raw label (no `exec ` prefix).

#### Decisions needed before implementing

1. **Error or warn** if LSM is inactive? (Docker errors; runc errors — we should too.)
2. **Ship a default profile?** A built-in `docker-default`-equivalent AppArmor profile
   requires generating and loading it via `apparmor_parser` — significant extra scope.
   Starting with "bring your own profile" is much simpler.
3. **One builder or two?** Could gate behind a feature flag so non-LSM builds don't pay
   for it. Or just always compile it and no-op the write path if no profile is set.

#### Testing

Needs a VM (or CI runner) with AppArmor active and `apparmor_parser` available. The Arch
dev machine is not a good test environment for this feature.
