# Remora Development Roadmap

**Last Updated:** 2026-02-17
**Current Status:** Phase 6 networking — N1/N2/N3 complete, N4/N5 next

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

### Phase 6 Networking — N1/N2/N3 ✅

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

---

## In Progress

### Phase 6 Networking — N4/N5

#### N4 — Port Mapping
Allow host ports to forward into bridge-mode containers.

```rust
Command::new("/bin/sh")
    .with_network(NetworkMode::Bridge)
    .with_nat()
    .with_port_forward(8080, 80)   // host:8080 → container:80
    .spawn()?;
```

**Implementation:** nftables DNAT rule — same `nft -f -` pipe pattern as N3.
Reference-counted per port binding; torn down in `teardown_network`.

#### N5 — DNS
Write `/etc/resolv.conf` into the container's rootfs so hostnames resolve.

```rust
Command::new("/bin/sh")
    .with_network(NetworkMode::Bridge)
    .with_nat()
    .with_dns(&["1.1.1.1", "8.8.8.8"])   // default if omitted
    .spawn()?;
```

**Implementation:** write to `{rootfs}/etc/resolv.conf` in parent before fork.
Requires rootfs to be set. No-op for loopback-only containers.

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

### Overlay Filesystem (Moderate Effort)

Layered rootfs using `overlayfs` — lower (read-only) + upper (writable) layers.
Foundation for image-layer-style workflows without full image management.

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
| Current (N1/N2/N3 complete) | ~65% |
| After N4/N5 | ~70% |
| After OCI compliance | ~85% |
| After rootless | ~90% |
