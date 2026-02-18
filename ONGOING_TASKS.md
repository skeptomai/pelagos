# Ongoing Tasks

## Current Task: Rootless Phase 1 — COMPLETE ✅

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

## Planned

- **Rootless Phase 2** — pasta networking integration (full internet access without root)
- **AppArmor / SELinux** — MAC profile support; defence-in-depth on top of seccomp
