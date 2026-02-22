# Remora - Linux Container Runtime

## ⚠️ CRITICAL RULES FOR CLAUDE ⚠️

### Write Plans to ONGOING_TASKS.md
**Before presenting any implementation plan to the user, write the full plan to
`ONGOING_TASKS.md` in the project root.** This ensures plans survive context resets.

- `ONGOING_TASKS.md` always reflects the current task and its full implementation detail
- After completing a task, update it with the next planned task
- The file includes: context, API design, pre_exec sequence changes, exact file changes,
  test descriptions, verification steps, and notes/risks

### ❌ NEVER RUN SUDO COMMANDS
**YOU CANNOT RUN SUDO** - The user MUST run sudo commands themselves.

**What NOT to do:**
- ❌ `sudo cargo test`
- ❌ `sudo -E cargo run`
- ❌ `sudo ./script.sh`
- ❌ ANY command starting with `sudo`

**What TO do instead:**
- ✅ Tell the user: "Please run: sudo -E cargo test --test integration_tests"
- ✅ Explain what the command will do
- ✅ Wait for user to run it and report results

### Integration Tests Are Part of the Feature
**Every feature MUST include integration tests in the same commit. A feature is not done until it is tested.**

- Parser/serialization features: add tests in `tests/integration_tests.rs` that exercise the public API
- Runtime features (networking, containers, cgroups): add root-requiring tests that spawn real containers
- Do NOT defer test writing to a follow-up — tests ship with the code

### Document Every Integration Test
**When writing a new integration test, you MUST also add its entry to `docs/INTEGRATION_TESTS.md` in the same change.**

The entry must include:
- The function name as a heading
- Whether it requires root and/or rootfs
- What it actually asserts and why — not just what the code does, but what failure would indicate

This is a hard requirement, not optional cleanup.

### Use `log` Crate for All Diagnostic Output
**NEVER use `eprintln!` for debugging or diagnostic messages.**

- ✅ `log::debug!("probe result: {}", ok)` — developer diagnostics
- ✅ `log::info!("using native overlay+userxattr")` — noteworthy runtime events
- ✅ `log::warn!("fuse unmount failed: {}", e)` — non-fatal problems
- ❌ `eprintln!("[debug] ...")` — never, even temporarily

`eprintln!` is reserved for **user-facing error messages** in the CLI binary (e.g. `eprintln!("remora: error: {}", e)`). Everything else goes through `log::*` so it respects `RUST_LOG` filtering and doesn't pollute stderr when users don't want it.

### User Macros

**"Make it so!"** — Clean up, comment, commit, and push:
1. Remove any temporary debug code or dead comments
2. Ensure `cargo fmt`, `cargo clippy -- -D warnings`, and `cargo test --lib` pass
3. Commit with a descriptive message
4. Push to remote

**"Engage!"** — Tag, release, and monitor:
1. Create a git tag (ask user for version if unclear)
2. Push the tag to trigger GitHub Actions release workflow
3. Monitor the release with a background agent and report result

### Ask Before Major Decisions
- API design choices
- Adding new features not explicitly requested
- Architectural changes
- When uncertain about the right approach

### No Time Estimates
**NEVER include time estimates** in any documentation or planning:
- ❌ "~3 weeks", "1-2 weeks", "3 days"
- ✅ Use: "Quick", "Moderate Effort", "Significant Work"

---

## Project Overview

Remora is a modern, lightweight Linux container runtime written in Rust. It provides a safe, ergonomic API for creating containerized processes using Linux namespaces, seccomp filtering, capabilities, and resource limits.

## Current State (Updated Feb 17, 2026)

### ✅ Completed Features

**Core Isolation:**
- Linux namespaces: UTS, Mount, IPC, User, Net, Cgroup (6/7)
- PID namespace (works in library, architectural limitation in CLI)
- Filesystem isolation: chroot and pivot_root
- Automatic mounts: /proc, /sys, /dev

**Security (Phase 1 COMPLETE ✅):**
- **Seccomp filtering**: Docker's default profile + minimal profile
- **No-new-privileges**: Prevent setuid/setgid escalation
- **Read-only rootfs**: Immutable filesystem
- **Masked paths**: Hide sensitive kernel info
- **Capability management**: Drop/keep specific capabilities
- **Resource limits**: rlimits for memory, CPU, file descriptors

**Interactive Containers (Phase 2 COMPLETE ✅):**
- **PTY support**: `spawn_interactive()` allocates a PTY pair via `openpty()`
- **Session isolation**: `setsid()` + `TIOCSCTTY` gives container its own session
- **Raw-mode relay**: `InteractiveSession::run()` polls stdin↔master, 100ms timeout
- **Window resize**: `SIGWINCH` handler syncs terminal size to PTY via `TIOCSWINSZ`
- **Terminal restore**: `TerminalGuard` RAII ensures raw mode is always cleaned up
- **`src/pty.rs`**: relay loop, `TerminalGuard`, `InteractiveSession`

**Advanced Resource Management (Phase 5 COMPLETE ✅):**
- **Cgroups v2**: `with_cgroup_memory()`, `with_cgroup_cpu_shares()`, `with_cgroup_cpu_quota()`, `with_cgroup_pids_limit()`
- **Auto-detection**: `cgroups-rs` auto-detects v1 vs v2 via `hierarchies::auto()`
- **Resource stats**: `child.resource_stats()` returns memory, CPU, and PID stats
- **Automatic cleanup**: cgroup deleted in `wait()` / `wait_with_output()`
- **Coexists with rlimits**: both mechanisms work independently

**Filesystem Flexibility (Phase 4 COMPLETE ✅):**
- **Bind mounts**: `with_bind_mount()` (RW) and `with_bind_mount_ro()` (RO) — map host dirs into container
- **tmpfs mounts**: `with_tmpfs()` — in-memory writable scratch space (works with read-only rootfs)
- **Named volumes**: `Volume::create/open/delete` backed by `/var/lib/remora/volumes/<name>/`; `with_volume()` builder method
- **Overlay filesystem**: `with_overlay(upper_dir, work_dir)` — copy-on-write layered rootfs; requires `Namespace::MOUNT` + `with_chroot`; merged dir auto-managed at `/run/remora/overlay-{pid}-{n}/merged/`

**OCI Image Layers (COMPLETE ✅):**
- **Image pull**: `remora image pull alpine` — native OCI registry pulls via `oci-client`; anonymous auth; layers cached content-addressably at `/var/lib/remora/layers/<sha256>/`
- **Image run**: `remora run alpine /bin/sh` — multi-layer overlayfs mount with ephemeral upper/work; image config (Env, Cmd, Entrypoint, WorkingDir) applied as defaults
- **Image management**: `remora image ls`, `remora image rm <ref>` — list/remove locally stored images
- **Multi-layer overlay**: `with_image_layers(layer_dirs)` — API for mounting multiple overlay lower layers; auto-creates ephemeral upper/work dirs
- **OCI whiteouts**: `.wh.*` files converted to overlayfs char device (0,0) whiteouts; `.wh..wh..opq` sets `trusted.overlay.opaque` xattr
- **`src/image.rs`**: `ImageConfig`, `ImageManifest`, `extract_layer()`, `save_image()`, `load_image()`, `layer_dirs()`
- **`src/cli/image.rs`**: `cmd_image_pull()`, `cmd_image_ls()`, `cmd_image_rm()`

**Networking (Phase 6 COMPLETE ✅):**
- **N1 Loopback**: `with_network(NetworkMode::Loopback)` — isolated NET namespace, lo brought up via ioctl (127.0.0.1 active)
- **N2 Bridge**: `with_network(NetworkMode::Bridge)` — veth pair + `remora0` bridge (172.19.0.x/24), IPAM via per-network state files
- **N2b Named Networks**: `with_network(NetworkMode::BridgeNamed("frontend"))` — user-defined bridge networks with custom subnets
- **N3 NAT**: `with_nat()` — nftables MASQUERADE per-network, reference-counted via per-network state files
- **N4 Port mapping**: `with_port_forward(host_port, container_port)` — TCP DNAT via nftables prerouting + userspace TCP proxy for localhost access
- **N5 DNS**: `with_dns(&[...])` — writes to `/run/remora/dns-{pid}-{n}/resolv.conf` and bind-mounts it into the container; shared rootfs is never modified; requires `Namespace::MOUNT` + `with_chroot`
- **N6 Pasta**: `with_network(NetworkMode::Pasta)` — user-mode networking via `pasta`; rootless-compatible full internet access; attaches to container netns via `/proc/{pid}/ns/net` after exec
- **Multi-network**: `remora network create/ls/rm/inspect` — per-network `Ipv4Net` subnets, `NetworkDef` config, IPAM, NAT, nftables tables (`remora-<name>`); `--network <name>` on run/build
- **Multi-network containers**: `with_additional_network("backend")` — attach secondary bridge interfaces (eth1, eth2, ...) with subnet routes; `attach_network_to_netns()` / `teardown_secondary_network()` in network.rs; `--network frontend --network backend` CLI; smart link resolution via `network_ips` in state.json
- **N7 DNS service discovery**: dual-backend DNS — `builtin` (`remora-dns` daemon, default) or `dnsmasq` (production-grade); automatic container name resolution on bridge networks; per-network config files at `/run/remora/dns/<network>.conf`; SIGHUP reload; upstream forwarding; `--dns-backend` CLI flag or `REMORA_DNS_BACKEND` env var; auto-start/stop lifecycle managed by `ensure_dns_daemon()` / container teardown
- **Automatic cleanup**: veth pair, netns, nftables rules, pasta relay, secondary networks, DNS entries cleaned up in `wait()` / `wait_with_output()`
- **`src/network.rs`**: `NetworkMode`, `Ipv4Net`, `NetworkDef`, `bring_up_loopback()`, `setup_bridge_network()`, `teardown_network()`, `attach_network_to_netns()`, `teardown_secondary_network()`, `setup_pasta_network()`, `teardown_pasta_network()`, `is_pasta_available()`, `bootstrap_default_network()`, `load_network_def()`
- **`src/dns.rs`**: DNS daemon management: `DnsBackend` enum, `active_backend()`, `ensure_dns_daemon()`, `dns_add_entry()`, `dns_remove_entry()`; dual-backend dispatch (builtin/dnsmasq)
- **`src/bin/remora-dns.rs`**: DNS daemon binary: UDP server, A-record resolution, upstream forwarding, SIGHUP reload
- **`src/cli/network.rs`**: `cmd_network_create()`, `cmd_network_ls()`, `cmd_network_rm()`, `cmd_network_inspect()`

**Image Build (COMPLETE ✅):**
- **`remora build -t <tag> [--file <path>] [--network bridge|pasta] [--build-arg KEY=VALUE] [context]`**: build images from Remfiles
- **Remfile parser**: FROM (+ `AS alias`), RUN, COPY (+ `--from=stage`), ADD, CMD, ENTRYPOINT (JSON + shell form), ENV, WORKDIR, EXPOSE, LABEL, USER, ARG
- **Build engine**: overlay snapshot per RUN step, context COPY as layers, config-only instructions
- **Multi-stage builds**: `FROM ... AS builder` / `COPY --from=builder`; stages split at FROM boundaries; only final stage produces output manifest
- **ARG instruction**: `ARG NAME=default` with `$VAR`/`${VAR}` substitution; `--build-arg` CLI flag; ARG allowed before FROM (Docker compat)
- **ADD instruction**: URL download (http/https via ureq), local archive auto-extraction (.tar, .tar.gz, .tar.bz2, .tar.xz), plain copy fallback
- **`.remignore`**: gitignore-style patterns to exclude files from COPY/ADD context (via `ignore` crate)
- **Build cache**: sha256(parent_layer + instruction) keyed layer cache; `--no-cache` flag to bypass
- **Layer creation**: tar+gzip for sha256 digest, extracted dir stored in layer store (dedup)
- **Path traversal protection**: COPY/ADD rejects sources outside the build context
- **`wait_preserve_overlay()`**: Child method that skips overlay cleanup for build engine
- **`src/build.rs`**: `Instruction`, `parse_remfile()`, `execute_build()`, `execute_stage()`, `split_into_stages()`, `substitute_vars()`, `create_layer_from_dir()`, `BuildError`
- **`src/cli/build.rs`**: `BuildArgs`, `cmd_build()`

**Container Exec (COMPLETE ✅):**
- **`remora exec <name> <command>`**: run a command inside a running container
- **Namespace discovery**: compares `/proc/{pid}/ns/*` inodes against `/proc/1/ns/*` to find container namespaces
- **Environment inheritance**: reads `/proc/{pid}/environ` as base, CLI `-e` overrides
- **Interactive mode**: `remora exec -i <name> /bin/sh` allocates a PTY
- **User/workdir**: `--user UID[:GID]`, `--workdir /path` options
- **`src/cli/exec.rs`**: `ExecArgs`, `cmd_exec()`, `discover_namespaces()`, `read_proc_environ()`

**Compose (COMPLETE ✅):**
- **`remora compose up [-f compose.rem] [-p project] [--foreground]`**: parse S-expression compose file, create scoped networks/volumes, start services in dependency order with TCP readiness polling, supervisor process with log relay
- **`remora compose down [-f compose.rem] [-p project] [-v]`**: stop services in reverse topo order (SIGTERM → SIGKILL), remove networks/volumes/state
- **`remora compose ps [-f compose.rem] [-p project]`**: list services with status
- **`remora compose logs [-f compose.rem] [-p project] [--follow] [service]`**: view prefixed service logs
- **S-expression format**: `(compose (network ...) (volume ...) (service ...))` — `;` comments, bare words, quoted strings, keyword args (`:ready-port`), nested lists
- **Dependency management**: `(depends-on (db :ready-port 5432))` — topological sort (Kahn's), cycle detection, TCP readiness polling (250ms interval, 60s timeout)
- **Scoped naming**: containers `{project}-{service}`, networks `{project}-{net}`, volumes `{project}-{vol}`; DNS uses bare service names for intra-project discovery
- **`src/sexpr.rs`**: `SExpr`, `parse()`, `ParseError` — zero-dependency recursive descent parser
- **`src/compose.rs`**: `ComposeFile`, `ServiceSpec`, `parse_compose()`, `validate()`, `topo_sort()`
- **`src/cli/compose.rs`**: `ComposeCmd`, `cmd_compose()`, supervisor, TCP readiness, scoped naming

**OCI Compliance (Phase 1 COMPLETE ✅):**
- **`remora create <id> <bundle>`**: parse `config.json`, fork shim, block on `exec.sock` until `start`
- **`remora start <id>`**: connect to `exec.sock`, send byte → container execs
- **`remora state <id>`**: read `state.json`, check liveness via `kill(pid, 0)`, print JSON
- **`remora kill <id> <sig>`**: send signal to container PID
- **`remora delete <id>`**: remove `/run/remora/<id>/` after container is stopped
- **`src/oci.rs`**: `OciConfig`, `OciState`, `build_command()`, all `cmd_*` functions
- **Sync mechanism**: double-fork; grandchild pre_exec writes PID + blocks on `accept(exec.sock)`
- **State persistence**: `/run/remora/<id>/state.json` (serde_json)

**Advanced:**
- UID/GID mapping for user namespaces
- Namespace joining (attach to existing namespaces)
- Ergonomic builder API

### 📁 File Structure

```
src/
  lib.rs                  # Library entry point
  main.rs                 # CLI binary (run/exec/ps/stop/rm/logs + OCI lifecycle)
  build.rs                # Image build engine: Remfile parser + executor
  compose.rs              # Compose model: ComposeFile, ServiceSpec, parse, validate, topo-sort
  container.rs            # Main API (~2270 lines)
  oci.rs                  # OCI Runtime Spec implementation
  cgroup.rs               # Cgroups v2 resource management
  network.rs              # Native networking (N1-N7 + multi-network)
  dns.rs                  # DNS daemon management: ensure_dns_daemon, dns_add/remove_entry
  seccomp.rs              # Seccomp-BPF filtering (~400 lines)
  sexpr.rs                # S-expression parser: SExpr, parse(), zero-dependency recursive descent
  pty.rs                  # PTY relay, TerminalGuard, InteractiveSession
  image.rs                # OCI image store: layer extraction, manifest persistence
  bin/
    remora-dns.rs         # DNS daemon binary: UDP server, A-record resolution, upstream forwarding
  cli/
    mod.rs                # Shared types: ContainerState, helpers, parsers
    build.rs              # remora build — build images from Remfiles
    compose.rs            # remora compose up/down/ps/logs — multi-service orchestration
    exec.rs               # remora exec — run command in running container
    run.rs                # remora run — build + launch containers
    ps.rs                 # remora ps — list containers
    stop.rs               # remora stop — SIGTERM a container
    rm.rs                 # remora rm — remove a container
    logs.rs               # remora logs [--follow] — view container output
    network.rs            # remora network create/ls/rm/inspect
    rootfs.rs             # remora rootfs import/ls/rm
    volume.rs             # remora volume create/ls/rm
    image.rs              # remora image pull/ls/rm — OCI registry pulls

tests/
  integration_tests.rs    # 84 integration tests (require root)

examples/
  seccomp_demo.rs         # Seccomp demonstration

Documentation:
  README.md                             # Project overview
  CLAUDE.md                             # This file
  docs/ROADMAP.md                       # Development plan (NO time estimates!)
  docs/INTEGRATION_TESTS.md            # Every integration test documented
  docs/DESIGN_PRINCIPLES.md             # Non-negotiable design principles
  docs/USER_GUIDE.md                    # Full CLI and API user guide
  docs/RUNTIME_COMPARISON.md            # vs Docker/runc/Podman
  docs/SECCOMP_DEEP_DIVE.md            # Seccomp implementation details
  docs/CGROUPS.md                       # Cgroups v1 vs v2 analysis
  docs/PTY_DEEP_DIVE.md                # PTY/interactive session design
  docs/BUILD_ROOTFS.md                  # How to build the Alpine rootfs
```

## Dependencies

### Current Dependencies (Cargo.toml)

```toml
log = "*"
env_logger = "*"
nix = { version = "0.31.1", features = ["process", "sched", "mount", "fs", "term", "poll", "signal", "ioctl"] }
libc = "*"
clap = { version = "3.1.6", features = ["derive"] }
thiserror = "2.0"
bitflags = "2.6"
cgroups-rs = "0.5.0"      # For future cgroup management
seccompiler = "0.5.0"     # Pure Rust seccomp-BPF (Firecracker)
serde = { version = "1", features = ["derive"] }  # OCI config.json / state.json
serde_json = "1"          # JSON for OCI bundle config and state files
oci-client = "0.16"       # OCI registry client for image pulls
tokio = { version = "1", features = ["rt", "net", "time", "io-util"] }  # Async runtime (image pulls)
flate2 = "1"              # Gzip decompression for OCI layer tarballs
tar = "0.4"               # Tar extraction for OCI layers
tempfile = "3"            # Temp files for layer downloads
```

**Note:** The DNS service discovery feature (`remora-dns` daemon) requires no new dependencies — it uses only `std::net::UdpSocket` for the DNS server and existing `nix`/`libc` for signal handling.

**Removed dependencies:**
- ~~unshare~~ - Replaced with custom implementation using nix
- ~~subprocess~~ - Never used
- ~~cgroups-fs~~ - Replaced with cgroups-rs
- ~~palaver~~ - Never used

## Root Filesystem

Remora requires an Alpine Linux rootfs to run containers.

**Two build options:**

1. **With Docker** (recommended):
   ```bash
   scripts/build-rootfs-docker.sh
   ```

2. **Without Docker** (tarball):
   ```bash
   scripts/build-rootfs-tarball.sh
   ```

See `BUILD_ROOTFS.md` for detailed instructions.

## Usage Examples

### Basic Container
```rust
use remora::container::{Command, Namespace, Stdio};

let mut child = Command::new("/bin/sh")
    .with_chroot("/path/to/rootfs")
    .with_namespaces(Namespace::UTS | Namespace::MOUNT | Namespace::PID)
    .with_proc_mount()
    .with_seccomp_default()      // Docker's seccomp profile
    .drop_all_capabilities()     // Least privilege
    .spawn()?;

child.wait()?;
```

### Interactive Container (PTY)
```rust
use remora::container::{Command, Namespace};

let session = Command::new("/bin/sh")
    .with_chroot("/path/to/rootfs")
    .with_namespaces(Namespace::UTS | Namespace::MOUNT)
    .with_proc_mount()
    .spawn_interactive()?;

// Blocks: relays stdin/stdout, forwards SIGWINCH, restores terminal on exit
let status = session.run()?;
```

### Running Examples (User Must Run)
```bash
# User runs:
sudo -E cargo run --example seccomp_demo
# Interactive shell:
sudo -E cargo run -- --rootfs alpine-rootfs --exe /bin/sh --uid 0 --gid 0
```

## Testing

### Unit Tests (No Root Required)
```bash
cargo test --lib
```

### Integration Tests (Require Root)
Tell user to run:
```bash
sudo -E cargo test --test integration_tests
```

## Architecture

### Pre-exec Hook Order (Critical!)
The spawn process has a carefully orchestrated setup:

1. **Parent process** (before fork):
   - Open namespace files (can't do in pre_exec)
   - Compile seccomp BPF filter (requires allocation)

2. **Fork**: Create child process

3. **Pre-exec hook** (in child, before exec):
   1. Unshare namespaces
   2. Make mounts private (if MOUNT namespace)
   3. Set up UID/GID mappings (if USER namespace)
   4. Set UID/GID
   5. Change root (chroot or pivot_root)
   6. Mount filesystems (/proc, /sys, /dev)
   7. Drop capabilities
   8. Set resource limits
   9. Run user pre_exec callback
   10. Join existing namespaces (setns)
   11. **Apply seccomp filter (MUST BE LAST!)**

4. **Exec**: Replace with target program

**Why seccomp is last:** Many syscalls needed for setup (mount, setuid) would be blocked if applied earlier.

## Development Workflow

### Making Changes
1. Write code
2. Run unit tests: `cargo test --lib`
3. Build: `cargo build`
4. Tell user to run integration tests if relevant

### Adding Features
1. Ask user if uncertain about approach
2. Implement in src/
3. Add tests
4. Update README.md
5. Add example if appropriate

### Documentation
- Keep concise and practical
- Focus on "how to use" over theory
- Provide working examples
- Update README when adding major features

## Next Steps (from ROADMAP.md)

**Phase 1 - Security Hardening: COMPLETE ✅**
- ✅ Seccomp filtering
- ✅ Read-only rootfs (MS_RDONLY via bind-mount + remount)
- ✅ Masked paths (/proc/kcore, /sys/firmware, etc.)
- ✅ No new privileges (PR_SET_NO_NEW_PRIVS)
- ✅ Capability management
- ✅ Resource limits (rlimits)

**Phase 2 - Interactive Containers: COMPLETE ✅**
- ✅ PTY support (`spawn_interactive()`, `InteractiveSession::run()`)
- ✅ SIGWINCH forwarding (window resize)
- ✅ Session isolation (setsid + TIOCSCTTY)

**Phase 5 - Advanced Resource Management: COMPLETE ✅**
- ✅ Cgroups v2 memory limit — `with_cgroup_memory(bytes)`
- ✅ Cgroups v2 CPU shares/weight — `with_cgroup_cpu_shares(weight)`
- ✅ Cgroups v2 CPU quota — `with_cgroup_cpu_quota(quota_us, period_us)`
- ✅ Cgroups v2 PID limit — `with_cgroup_pids_limit(max)`
- ✅ Resource stats — `child.resource_stats()`
- ✅ Automatic cgroup cleanup on `wait()`

**Phase 4 - Filesystem Flexibility: COMPLETE ✅**
- ✅ Bind mounts (RW and RO) — `with_bind_mount()`, `with_bind_mount_ro()`
- ✅ tmpfs mounts — `with_tmpfs()`
- ✅ Named volumes — `Volume::create/open/delete`, `with_volume()`

**Phase 6 - Networking: COMPLETE ✅**
- ✅ N1 Loopback — `with_network(NetworkMode::Loopback)`
- ✅ N2 Bridge — `with_network(NetworkMode::Bridge)`
- ✅ N3 NAT — `with_nat()`
- ✅ N4 Port mapping — `with_port_forward(host_port, container_port)`
- ✅ N5 DNS — `with_dns(&[...])`
- ✅ N7 DNS service discovery — dual-backend (builtin `remora-dns` + dnsmasq), `--dns-backend` flag

**Rootless Mode - Phase 2 (Pasta): COMPLETE ✅**
- ✅ N6 Pasta — `with_network(NetworkMode::Pasta)` — rootless-compatible full internet via `pasta`

See docs/ROADMAP.md for full plan (no time estimates!)

## Common Issues

### "alpine-rootfs not found"
Run: `scripts/fix-rootfs.sh` (requires Docker + sudo)

### Integration tests fail
User must run with: `sudo -E cargo test --test integration_tests`

### Permission denied
Many features require root or CAP_SYS_ADMIN

### Alpine binary paths
Alpine uses `/usr/bin/` for many utilities, NOT `/bin/`. Busybox core applets
(sh, ash, cat, cp, echo, ls, etc.) are symlinked in `/bin/`, but utilities like
`id`, `env`, `wc`, `sort`, `tr` live in `/usr/bin/`. When writing tests or
examples that run inside Alpine containers, use bare command names (e.g. `id`)
to let PATH resolve them, or use the correct `/usr/bin/id` path. **Never assume
`/bin/id` exists.**

## Comparison to Docker/runc

| Feature | Remora | Docker |
|---------|--------|--------|
| Namespaces | ✅ 6/7 | ✅ All |
| Seccomp | ✅ Docker profile | ✅ |
| Capabilities | ✅ | ✅ |
| Resource limits | ✅ rlimits + cgroups v2 | ✅ cgroups |
| TTY/PTY | ✅ PTY relay | ✅ |
| Bind mounts | ✅ RW + RO | ✅ |
| tmpfs mounts | ✅ | ✅ |
| Named volumes | ✅ | ✅ |
| Overlay filesystem | ✅ CoW layered rootfs | ✅ |
| Networking | ✅ N1–N7 + multi-network containers (Loopback/Bridge/NAT/Ports/DNS/Pasta/Named/Multi-attach/DNS-SD) | ✅ Native libnetwork |
| DNS service discovery | ✅ Dual-backend (builtin + dnsmasq) container name resolution | ✅ Embedded DNS server |
| Rootless networking | ✅ pasta (full internet, no root) | ✅ |
| OCI image pull | ✅ `remora image pull` (anonymous) | ✅ |
| Image build | ✅ `remora build` (Remfile) | ✅ Dockerfile |
| Container exec | ✅ `remora exec` (ns join + PTY) | ✅ |
| Compose | ✅ `remora compose` (S-expression) | ✅ docker compose (YAML) |
| OCI Compatible | 🔄 Partial | ✅ |

**Current parity: ~80% of runc features**
