# Ongoing Tasks

## Current Task: None

No active task. Session closed cleanly at v0.3.2.

---

## Potential Next Moves

### 1. Port Forwarding from Localhost (Blocked)

Port forwarding uses nftables DNAT in the PREROUTING chain. This works for
external hosts but NOT for traffic from localhost (which uses the OUTPUT chain).
Six different nftables approaches were tried and all failed due to hairpin NAT
interactions with `br_netfilter`.

**Root cause:** Docker solves this with `docker-proxy` — a userspace TCP proxy.
nftables DNAT alone cannot handle localhost→container reliably.

**Current state:** PREROUTING-only rules; works for external hosts, not localhost.

**Solution:** Implement a lightweight userspace TCP proxy, or document the
limitation.

### 2. Inter-Container nginx proxy_pass (Unresolved)

nginx returns 502 when using `proxy_pass` to another container, even though
the target is reachable via `wget` from the same container. Suspected cause:
`br_netfilter` causing the NAT masquerade rule to match bridged traffic.

**Not yet verified:**
- Whether `br_netfilter` is loaded
- Whether disabling it fixes the issue
- Whether the issue exists without any nftables rules

### 3. Example Applications

**Multi-Container Web Stack** — partially working (see above blockers)

**Build Sandbox** — rootless container for compiling user code with
read-only rootfs, tmpfs /tmp, resource limits, seccomp + cap-drop ALL

**CI Test Runner** — pull image, run tests, collect exit code with
`--env`, `--bind`, `--workdir`, detached mode + `logs --follow`

### 4. `remora build` Enhancements

- **ENTRYPOINT instruction** — parser + config only (no layer)
- **ADD instruction** — URL downloads + auto tar extraction
- **LABEL instruction** — image metadata, parser + config only
- **USER instruction** — default UID/GID, parser + config only
- **ARG instruction** — build-time variables with `${NAME}` substitution
- **Multi-stage builds** — `FROM ... AS builder` / `COPY --from=builder` (significant work)
- **Build cache** — hash (instruction + parent layer) to skip unchanged RUN steps
- **`.remignore` file** — exclude files from build context

### 5. Remaining runc Parity Gaps (~20%)

**Security / MAC (Significant Work):**
- AppArmor profiles (`linux.apparmorProfile`)
- SELinux labels (`linux.selinuxProcessLabel`)

**Seccomp (Moderate Work):**
- Argument-level conditions (`linux.seccomp.syscalls[].args[]`)

**Cgroups (Moderate Work):**
- I/O bandwidth limits (`linux.resources.blockIO`)

**OCI Hooks (Quick):**
- `createRuntime` and `startContainer` hook points (OCI Runtime Spec 1.1+)

**OCI Config (Quick-to-Moderate):**
- `linux.devices` fine-grained ACLs
- `annotations` key-value metadata

**Other:**
- Checkpoint/Restore via CRIU (significant work, low priority)
- Intel RDT (very niche, low priority)
- PID namespace in CLI foreground mode (needs shim or double-fork)

### 6. Multi-Network Support

User-defined bridge networks with per-network subnets, IPAM, NAT, and isolation.
`remora network create/ls/rm` CLI, `--network <name>` on run, parameterized
nftables rules. Full design plan: **[docs/MULTI_NETWORK.md](docs/MULTI_NETWORK.md)**.

### 7. Other Improvements

- Authenticated registry pulls (Docker Hub private repos)
- `remora build` rootless mode
- Error message audit for clarity

---

## Completed Features

### JSON Output + Container Inspect (v0.3.2)
**COMPLETE** — `--format json` on all list commands; `container inspect` command.
- `remora ps/container ls --format json`
- `remora volume/image/rootfs ls --format json`
- `remora container inspect <name>` (always JSON)
- 4 integration tests (volume, rootfs, container, image JSON cycles)
- Apache 2.0 LICENSE file added

### `remora build` (v0.3.0)
**COMPLETE** — Build images from Remfiles (simplified Dockerfiles).
- Remfile parser: FROM, RUN, COPY, CMD, ENV, WORKDIR, EXPOSE
- Buildah-style daemonless build: overlay snapshot per RUN step
- Path traversal protection on COPY
- 14 unit tests + 22 E2E assertions

### Stress Tests (v0.2.1)
**COMPLETE** — 18 pass, 0 fail, 0 skip.

### E2E Bug Fixes (v0.2.1)
**COMPLETE** — pre_exec ordering, proc mount, seccomp, exec workdir.

### Rootless Mode (v0.2.0)
**COMPLETE** — storage path abstraction, rootless overlay, multi-UID mapping,
cgroup v2 delegation.

### v0.1.0 — Initial Release
Full feature set: namespaces, seccomp, capabilities, cgroups v2, overlay,
networking (loopback/bridge/NAT/ports/DNS/pasta), OCI image pull, container
exec, OCI runtime compliance, interactive PTY.

---

## Current Capabilities

| Category | Features |
|----------|----------|
| Lifecycle | foreground, detached, ps, stop, rm, logs, name collision |
| Images | pull (anonymous, Docker Hub), multi-layer overlay, ls, rm, build |
| Exec | command in running container, PTY (-i), env/workdir/user |
| Networking | loopback, bridge+IPAM, NAT+MASQUERADE, port forwarding, DNS, pasta |
| Filesystem | overlay CoW, bind RW/RO, tmpfs, named volumes, read-only rootfs |
| Security | seccomp (default+minimal), capabilities, no-new-privs, masked paths |
| Resources | cgroups v2 (memory, CPU quota/shares, PIDs), rlimits |
| OCI | create/start/state/kill/delete lifecycle, config.json parsing |
| Rootless | images, overlay (native userxattr + fuse-overlayfs fallback), pasta, cgroups v2 |
| JSON output | `--format json` on all list commands, `container inspect` |

## Known Limitations

- PID namespace: works in library API, architectural limitation in CLI foreground mode
- No daemon mode: CLI tool and library only
- No AppArmor/SELinux: MAC profile support deferred
- No authenticated registry pulls: anonymous only
- No I/O bandwidth cgroups
- Rootless overlay: requires kernel 5.11+ or fuse-overlayfs
- Port forwarding: works for external hosts, not from localhost
