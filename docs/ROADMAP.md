# Remora Development Roadmap

**Last Updated:** 2026-02-18
**Current Status:** Container exec complete — `remora exec <name> <command>`

---

## Philosophy

1. **Security first** — no feature is worth compromising isolation
2. **Incremental value** — each sub-phase is usable on its own
3. **Native over delegated** — implement directly rather than shelling out to CNI
4. **Test everything** — every feature has integration tests

**Out of scope:** orchestration, GUI

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
- `with_dns(&["1.1.1.1", "8.8.8.8"])`: writes to a per-container temp file at
  `/run/remora/dns-{pid}-{n}/resolv.conf` and bind-mounts it over `/etc/resolv.conf`
  inside the container — the shared rootfs is never modified
- Requires `Namespace::MOUNT` and `with_chroot`; temp file removed in `wait()`

---

### Overlay Filesystem ✅

Layered rootfs using `overlayfs` — lower (read-only base) + upper (writable per-container) layers.

- `with_overlay(upper_dir, work_dir)`: requires `Namespace::MOUNT` + `with_chroot` (lower layer)
- Lower layer (shared Alpine rootfs) is never modified — writes land in `upper_dir`
- Merged mount point auto-created at `/run/remora/overlay-{pid}-{n}/merged/`; removed in `wait()`
- Compatible with `with_readonly_rootfs(true)`, bind mounts, and tmpfs
- Foundation for image-layer-style workflows

---

### OCI Image Layers ✅

Pull OCI images from registries and run containers directly from them.

- `remora image pull <ref>`: native OCI registry pulls via `oci-client` (anonymous auth)
- `remora image ls` / `remora image rm <ref>`: list and remove locally stored images
- `remora run --image <ref> [cmd]`: run a container from a pulled image
- Layers cached content-addressably at `/var/lib/remora/layers/<sha256>/`
- Image metadata at `/var/lib/remora/images/<name>_<tag>/manifest.json`
- Multi-layer overlayfs: `with_image_layers(layer_dirs)` API — multiple lower layers,
  ephemeral upper/work auto-created and cleaned up in `wait()`
- Image config (Env, Cmd, Entrypoint, WorkingDir) applied as defaults; CLI overrides
- OCI whiteout handling: `.wh.*` → overlayfs char device (0,0); `.wh..wh..opq` → xattr
- Dependencies: `oci-client`, `tokio` (current-thread), `flate2`, `tar`, `tempfile`

---

### OCI Compliance (Phase 1) ✅

Parse OCI `config.json` bundles and implement the standard container lifecycle.

**OCI config parsing (first pass — required fields):**
- `ociVersion`, `root.path`, `root.readonly`, `process.args/cwd/env/user/noNewPrivileges`
- `linux.namespaces`, `linux.uidMappings`, `linux.gidMappings`, `mounts`

**OCI lifecycle:**
```bash
remora create <id> <bundle>   # set up container, suspend before exec
remora start <id>             # signal child to exec
remora state <id>             # print JSON state to stdout
remora kill <id> SIGTERM      # send signal to container process
remora delete <id>            # tear down resources, remove state dir
```

**Implementation:** `src/oci.rs` — config/state types, path helpers, `cmd_*` functions.
State stored at `/run/remora/<id>/state.json`. create/start sync via Unix socket
at `/run/remora/<id>/exec.sock`. Double-fork shim ensures parent exits as soon
as "created" state is written.

**OCI Phase 2 (complete):** `process.capabilities` ✅, `linux.maskedPaths` ✅,
`linux.readonlyPaths` ✅, `linux.resources` ✅, `process.rlimits` ✅,
`linux.sysctl` ✅, `linux.devices` ✅, `hooks` (prestart/poststart/poststop) ✅,
`linux.seccomp` ✅.

Deferred to Phase 3: `linux.devices` fine-grained ACLs, `linux.seccomp` argument
conditions (`args` field), `hooks.createRuntime` / `startContainer`, `annotations`.

---

## Completed

### Rootless Mode — Phase 1 (User Namespace + Loopback) ✅

Auto-detection when running as non-root: adds `Namespace::USER`, configures
a default `{inside: 0, outside: host_uid, count: 1}` uid/gid map so the
process appears as UID 0 inside the container, skips cgroups gracefully, and
rejects `NetworkMode::Bridge` with a clear error (pointing to `NetworkMode::Pasta`).

- ✅ Rootless auto-detection (`getuid() != 0`)
- ✅ Auto-add `Namespace::USER` + uid/gid map
- ✅ `NetworkMode::Loopback` works rootless (USER+NET namespace)
- ✅ Graceful cgroup skip (EPERM in rootless)
- ✅ Bridge networking rejected with clear error
- ✅ Fix: uid_map writing was missing from `spawn_interactive()` pre_exec

### Rootless Mode — Phase 2 (pasta Networking) ✅

Full internet access in rootless containers via [pasta](https://passt.top/passt/about/)
(chosen over slirp4netns: lower overhead, no per-container daemon, Podman ≥4.4 default).

- ✅ `NetworkMode::Pasta` variant in `NetworkMode` enum
- ✅ `setup_pasta_network()` — spawns pasta after child exec'd, attaches via `/proc/{pid}/ns/net`
- ✅ `teardown_pasta_network()` — kills pasta relay on container exit
- ✅ `is_pasta_available()` — PATH check for graceful test skip
- ✅ Auto-adds `Namespace::NET`; `bring_up_loopback` applies to pasta mode
- ✅ Works for both root and rootless (USER+NET two-phase unshare)
- ✅ Port forwards passed as `-t HOST:CONTAINER` args to pasta
- ✅ `Child::pasta` field; teardown in `wait()` and `wait_with_output()`
- ✅ Same logic in `spawn_interactive()`

---

### Container Exec ✅

Run commands inside running containers — analogous to `docker exec`.

- `remora exec <name> <command>`: run a process in a running container's namespaces
- `remora exec -i <name> /bin/sh`: interactive mode with PTY
- Options: `-e KEY=VALUE` (env), `-w /path` (workdir), `-u UID[:GID]` (user)
- Namespace discovery: compares `/proc/{pid}/ns/*` inodes against `/proc/1/ns/*`
- Environment inheritance: reads `/proc/{pid}/environ`, CLI `-e` overrides
- Mount namespace joining via `setns()` + `fchdir(root_fd)` + `chroot(".")` in
  pre_exec callback (same technique as `nsenter(1)`)
- No changes to `container.rs` — composes existing primitives
- No resource teardown — exec'd process is ephemeral

---

## In Progress

(nothing currently in progress)

---

## Planned

### AppArmor / SELinux (Moderate Effort)

Apply MAC profiles to containers. Adds defence-in-depth on top of seccomp.

---

## Feature Parity Estimate

| Milestone | Estimated runc parity |
|-----------|----------------------|
| N1–N5 + overlay complete | ~73% |
| OCI compliance (Phase 1) ✅ | ~85% |
| Rootless Phase 1 + Phase 2 (pasta) ✅ | ~90% |
| OCI image layers ✅ | ~93% |
| Container exec ✅ | ~95% |
