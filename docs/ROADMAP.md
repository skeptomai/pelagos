# Remora Development Roadmap

**Last Updated:** 2026-02-17
**Current Status:** Overlay filesystem complete; next: OCI compliance

---

## Philosophy

1. **Security first** — no feature is worth compromising isolation
2. **Incremental value** — each sub-phase is usable on its own
3. **Native over delegated** — implement directly rather than shelling out to CNI
4. **Test everything** — every feature has integration tests

**Out of scope:** image management, registry operations, orchestration, GUI

---

## Completed

### Phase 1 — Security Hardening ✅
- Seccomp-BPF filtering (Docker's default profile + minimal profile)
- No-new-privileges (`PR_SET_NO_NEW_PRIVS`)
- Read-only rootfs (`MS_RDONLY` remount)
- Masked paths (`/proc/kcore`, `/sys/firmware`, etc.)
- Capability management — drop all or keep specific caps
- Resource limits (rlimits: memory, CPU, file descriptors)

### Phase 2 — Interactive Containers ✅
- PTY support via `spawn_interactive()` / `openpty()`
- Session isolation (`setsid` + `TIOCSCTTY`)
- Raw-mode relay with 100ms poll (`InteractiveSession::run()`)
- `SIGWINCH` forwarding → `TIOCSWINSZ`
- `TerminalGuard` RAII — terminal always restored on exit

### Phase 4 — Filesystem Flexibility ✅
- Bind mounts RW and RO (`with_bind_mount`, `with_bind_mount_ro`)
- tmpfs mounts (`with_tmpfs`) — writable scratch space inside read-only rootfs
- Named volumes (`Volume::create/open/delete`, `with_volume`) backed by
  `/var/lib/remora/volumes/<name>/`

### Phase 5 — Cgroups v2 Resource Management ✅
- Memory hard limit (`with_cgroup_memory`)
- CPU weight (`with_cgroup_cpu_shares`)
- CPU quota (`with_cgroup_cpu_quota`)
- PID limit (`with_cgroup_pids_limit`)
- Resource stats (`child.resource_stats()`)
- Automatic cgroup cleanup in `wait()` / `wait_with_output()`

### Phase 6 Networking — N1–N5 ✅

**N1 — Loopback**
- `with_network(NetworkMode::Loopback)`: isolated NET namespace, `lo` brought up
  via `ioctl(SIOCSIFFLAGS)` inside `pre_exec`

**N2 — Bridge**
- `with_network(NetworkMode::Bridge)`: veth pair + `remora0` bridge (172.19.0.x/24)
- Named netns created before fork → no race, no deadlock
- File-locked IPAM at `/run/remora/next_ip`
- Teardown (veth del + netns del) in `wait()` / `wait_with_output()`

**N3 — NAT / MASQUERADE**
- `with_nat()`: enables IP forwarding + installs nftables MASQUERADE rule
- Reference-counted — shared across concurrent NAT containers
- Removed atomically (`nft delete table ip remora`) when last NAT container exits

**N4 — Port Mapping**
- `with_port_forward(host_port, container_port)`: TCP DNAT via nftables prerouting
- Flush-and-rebuild strategy on teardown — no handle tracking required
- Shared `table ip remora` with N3; `disable_port_forwards` checks NAT refcount
  before deleting the table

**N5 — DNS**
- `with_dns(&["1.1.1.1", "8.8.8.8"])`: writes `{rootfs}/etc/resolv.conf` in parent
  before fork using the host-side rootfs path
- No-op if no rootfs is configured

---

### Overlay Filesystem ✅

Layered rootfs using `overlayfs` — lower (read-only base) + upper (writable per-container) layers.

- `with_overlay(upper_dir, work_dir)`: requires `Namespace::MOUNT` + `with_chroot` (lower layer)
- Lower layer (shared Alpine rootfs) is never modified — writes land in `upper_dir`
- Merged mount point auto-created at `/run/remora/overlay-{pid}-{n}/merged/`; removed in `wait()`
- Compatible with `with_readonly_rootfs(true)`, bind mounts, and tmpfs
- Foundation for image-layer-style workflows without full image management

---

## In Progress

Nothing — overlay filesystem is complete.

---

## Planned

### OCI Compliance (Significant Work)

Parse OCI `config.json` bundles and implement the standard container lifecycle.

**OCI config parsing:**
```rust
let config = OciConfig::from_file("config.json")?;
let child = Command::from_oci_config(&config)?.spawn()?;
```

**OCI lifecycle:**
```bash
remora create <id> <bundle>
remora start <id>
remora state <id>
remora kill <id> TERM
remora delete <id>
```

Enables interoperability with Kubernetes, containerd, and other OCI tooling.
Requires `serde_json`; state persistence in `/run/remora/containers/<id>/`.

### Rootless Mode (Significant Work)

Run containers without root using unprivileged user namespaces.
Requires subuid/subgid mapping (`/etc/subuid`, `/etc/subgid`) and rootless
cgroup delegation.

### AppArmor / SELinux (Moderate Effort)

Apply MAC profiles to containers. Adds defence-in-depth on top of seccomp.

---

## Feature Parity Estimate

| Milestone | Estimated runc parity |
|-----------|----------------------|
| Current (N1–N5 + overlay complete) | ~73% |
| After OCI compliance | ~85% |
| After rootless | ~90% |
