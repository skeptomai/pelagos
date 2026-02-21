# Ongoing Tasks

## Current Task: None

All three v0.3.3 quick wins completed:

- **Task A: ENTRYPOINT + LABEL + USER** — COMPLETE
- **Task B: Build cache** — COMPLETE
- **Task C: Localhost port forwarding (userspace TCP proxy)** — COMPLETE

### What was done (v0.3.3):

### Task A: ENTRYPOINT + LABEL + USER instructions for `remora build`

**ENTRYPOINT:** Parser recognizes `ENTRYPOINT ["cmd", "arg"]` (JSON) and
`ENTRYPOINT cmd arg` (shell form). Stored in `ImageConfig.entrypoint`.
At runtime, if the user provides a command it replaces CMD but ENTRYPOINT
remains as the prefix. Config-only instruction — no layer created.

**LABEL:** Parser recognizes `LABEL key=value key2="value 2"`. Stored in
`ImageConfig.labels` (HashMap). Config-only — no layer.

**USER:** Parser recognizes `USER uid[:gid]`. Stored in `ImageConfig.user`.
Applied as default UID/GID at container run time. Config-only — no layer.

**Files to change:**
- `src/build.rs` — add `Entrypoint`, `Label`, `User` variants to `Instruction` enum; parse them; handle in `execute_build()`
- `src/image.rs` — add `entrypoint`, `labels`, `user` fields to `ImageConfig`
- `src/cli/run.rs` — apply entrypoint (prefix to cmd), user (default uid/gid) from image config
- Unit tests for each new instruction parser

### Task B: Build Cache

Hash each (instruction text + parent layer digest) to produce a cache key.
Before executing a RUN step, check if a layer with that cache key already
exists. If so, skip execution and reuse the layer.

**Cache key:** `sha256(parent_layer_digest + "\n" + instruction_text)`

**Storage:** `/var/lib/remora/build-cache/<cache_key>` → symlink or file
containing the layer digest.

**Invalidation:** Any cache miss invalidates all subsequent steps (same as
Docker). `--no-cache` flag bypasses entirely.

**Files to change:**
- `src/build.rs` — cache lookup before RUN, cache store after RUN, `--no-cache` support
- `src/cli/build.rs` — add `--no-cache` flag to `BuildArgs`
- `src/paths.rs` — add `build_cache_dir()` helper

### Task C: Localhost Port Forwarding (Userspace TCP Proxy)

Spawn a background `TcpListener` on `0.0.0.0:{host_port}` for each port
mapping. On accept, connect to `{container_ip}:{container_port}` and relay
bytes bidirectionally. This handles localhost traffic that nftables DNAT
misses.

**Approach:** `std::thread::spawn` a listener thread per port mapping. Each
accepted connection spawns a relay thread (or two: read/write halves).
The listener threads are killed on container teardown.

**Files to change:**
- `src/network.rs` — add `start_port_proxy()` / `stop_port_proxy()`, thread handles stored in `NetworkSetup`
- `src/container.rs` — wire proxy start/stop into spawn/wait lifecycle

---

## Priority Evaluation (Feb 2026)

### High Impact / Low Effort (DO NEXT)
- **ENTRYPOINT + LABEL + USER** — quick parser additions, high value since real images use ENTRYPOINT constantly
- **Build cache** — transforms `remora build` from demo to practical tool
- **Localhost port forwarding** — userspace TCP proxy, fixes the most visible networking gap

### Moderate Impact / Moderate Effort (LATER)
- **`.remignore`** — glob filtering on build context, nice quality-of-life
- **OCI `annotations`** — key-value metadata passthrough
- **ARG instruction** — variable substitution across instructions
- **OCI hooks** — `createRuntime` / `startContainer` hook points
- **Multi-network** — user-defined bridges; design doc at [docs/MULTI_NETWORK.md](docs/MULTI_NETWORK.md)

### Low Impact / High Effort (DEFER)
- **AppArmor/SELinux** — significant work, most users don't use MAC profiles
- **Seccomp argument conditions** — niche, default profile is sufficient
- **I/O bandwidth cgroups** — rarely used in practice
- **Multi-stage builds** — premature without build cache; revisit after cache lands
- **CRIU checkpoint/restore** — huge effort, very niche
- **Intel RDT** — very niche, low priority

### Investigation Needed
- **Inter-container nginx 502** — debugging task, needs `br_netfilter` investigation before sizing

---

## Potential Next Moves

### ~~1. Port Forwarding from Localhost~~ — DONE (v0.3.3)

Solved with userspace TCP proxy (`start_port_proxies()` in `src/network.rs`).
Each port mapping gets a `TcpListener` on `0.0.0.0:{host_port}` that relays
to `{container_ip}:{container_port}`. Works alongside nftables DNAT rules.

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

- ~~**ENTRYPOINT instruction**~~ — DONE (v0.3.3)
- **ADD instruction** — URL downloads + auto tar extraction
- ~~**LABEL instruction**~~ — DONE (v0.3.3)
- ~~**USER instruction**~~ — DONE (v0.3.3)
- **ARG instruction** — build-time variables with `${NAME}` substitution
- **Multi-stage builds** — `FROM ... AS builder` / `COPY --from=builder` (significant work)
- ~~**Build cache**~~ — DONE (v0.3.3)
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

### High-Impact Quick Wins (v0.3.3)
**COMPLETE** — Three features for highest impact-per-effort.
- **ENTRYPOINT/LABEL/USER** build instructions: parser, ImageConfig fields, runtime application
- **Build cache**: sha256(parent_layer + instruction) keyed, `--no-cache` flag, stale entry cleanup
- **Localhost port forwarding**: userspace TCP proxy per port mapping, solves nftables DNAT limitation
- 8 new unit tests (parser + cache key)

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
