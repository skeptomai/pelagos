# Pelagos

[![CI](https://github.com/skeptomai/pelagos/actions/workflows/ci.yml/badge.svg)](https://github.com/skeptomai/pelagos/actions/workflows/ci.yml)

**Pelagos** is a daemonless Linux container runtime written in Rust. It can run a
single container or orchestrate a multi-service stack — and its primary interface
is a Lisp scripting language, not YAML.

The `.reml` scripting layer lets you express things that declarative config cannot:
run a migration before the app starts, wait for a port to be ready, react to
failure with cleanup logic, or build a dependency graph that executes in parallel.
A runtime that is programmable from first principles.

Pelagos is also an embeddable Rust library, making it possible to add container
isolation directly to your own programs without spawning a daemon or shelling out
to Docker.

**[User Guide](docs/USER_GUIDE.md)** — full CLI reference, networking, storage,
security, scripting, and more.

---

## What makes it different

| | Pelagos | Docker | runc |
|--|--------|--------|------|
| Daemon required | ❌ | ✅ | ❌ |
| Library API | ✅ | ❌ | ❌ |
| Config language | Lisp (`.reml`) | YAML | JSON |
| Imperative scripting | ✅ full language | ❌ | ❌ |
| Security-by-default | ✅ all containers | opt-in | opt-in |
| Rootless networking | ✅ pasta | ✅ | limited |
| Linux + Wasm, one runtime | ✅ | ❌ | ❌ |

**Security-by-default** means every container gets seccomp-BPF filtering,
all capabilities dropped, no-new-privileges, masked kernel paths, and PID/UTS/IPC
namespace isolation — without any flags. Services that need specific capabilities
opt back in with `:cap-add`.

**Linux and Wasm, unified.** No other general-purpose container runtime handles
WebAssembly natively — runc, crun, and youki treat `.wasm` files as opaque
executables and fail at `exec()`. Wasm-native runtimes (runwasi, Spin, WasmEdge
shim) go the other direction: Wasm only, no Linux OCI containers. Pelagos is the
only runtime where both workload types share one CLI, one image store, one compose
format, and one node: `pelagos run alpine /bin/sh` and
`pelagos run ghcr.io/example/my-app:latest` work the same way whether the image
contains an ELF binary or a `.wasm` module. See [`docs/WASM_SUPPORT.md`](docs/WASM_SUPPORT.md).

---

## The `.reml` scripting interface

Pelagos compose files are Lisp programs, not config schemas. A minimal stack:

```lisp
(define-service svc-db "db"
  :image "postgres:16"
  :env   ("POSTGRES_PASSWORD" "secret"))

(define-service svc-app "app"
  :image      "myapp:latest"
  :depends-on "db" 5432)

(compose-up
  (compose svc-db svc-app))
```

But when you need more than ordering, the full language is available:

```lisp
; Start the database and wait for it
(define db (container-start svc-db))
(await-port "localhost" 5432 :timeout 60)

; Run migrations — abort the whole deploy if they fail
(define exit-code (container-run svc-migrate))
(unless (zero? exit-code)
  (error "migrations failed — aborting"))

; Start the app, clean up on any exit
(with-cleanup (lambda (result)
                (container-stop db)
                (logf "deploy result: ~a" result))
  (container-wait (container-start svc-app)))
```

The same interpreter supports a **futures graph** for parallel execution:

```lisp
(define-nodes
  (db    svc-db)
  (cache svc-cache))

(define-then db-url db (h)
  (format "postgres://app:secret@~a/appdb" (container-ip h)))

(define-run :parallel
  (app-handle app)
  (db-url     db-url))
```

See [`docs/REML_EXECUTOR_MODEL.md`](docs/REML_EXECUTOR_MODEL.md) for the full
scripting reference.

---

## Features

### Isolation
- **Namespaces:** UTS, Mount, IPC, Network, User, PID, Cgroup
- **Filesystem:** chroot, pivot_root, automatic /proc /sys /dev mounts
- **Security defaults:** seccomp-BPF + all capabilities dropped + no-new-privileges
  + masked paths applied to every container unconditionally

### Security
- **Seccomp-BPF:** Docker's default profile via pure-Rust `seccompiler`
- **Capability management:** all caps dropped by default; `:cap-add` restores specific ones
- **No-new-privileges:** `PR_SET_NO_NEW_PRIVS` blocks setuid/setgid escalation
- **Read-only rootfs:** `MS_RDONLY` remount makes the filesystem immutable
- **Masked paths:** `/proc/kcore`, `/sys/firmware`, and others hidden
- **Landlock LSM:** per-path filesystem rules via Linux 5.13+ kernel interface
- **Structural TOCTOU immunity:** Pelagos uses a single-threaded `pre_exec` hook and
  never re-execs itself — the architecture that drives the November 2025 runc CVE
  cluster (CVE-2025-31133, CVE-2025-52565, CVE-2025-52881) does not exist in Pelagos.
  See [docs/SECURITY.md](docs/SECURITY.md) for details.

### Networking
- **Loopback:** isolated NET namespace, `lo` only
- **Bridge:** veth pair + named bridge, IPAM, DNS service discovery
- **NAT:** nftables MASQUERADE, reference-counted across containers
- **Port mapping:** TCP DNAT via nftables + userspace proxy for localhost
- **Named networks:** user-defined bridge networks with custom subnets
- **Multi-network:** attach containers to multiple networks simultaneously
- **DNS service discovery:** dual-backend (built-in daemon or dnsmasq), automatic
  container name resolution on bridge networks
- **Pasta:** full internet access without root via [pasta](https://passt.top/passt/about/)

### Resource Management
- **rlimits:** memory, CPU time, file descriptors, process count
- **Cgroups v2:** memory hard limit, CPU weight, CPU quota, PID limit
- **Resource stats:** `child.resource_stats()` reads live cgroup counters

### Filesystem
- **Bind mounts:** `with_bind_mount()` (RW) and `with_bind_mount_ro()` (RO)
- **tmpfs:** writable scratch space inside a read-only rootfs
- **Named volumes:** persisted storage, scoped per compose project
- **Overlay filesystem:** copy-on-write layered rootfs via overlayfs

### OCI Images
- **Pull:** `pelagos image pull alpine` — anonymous pulls from any OCI registry
- **Run:** `pelagos run alpine /bin/sh` — multi-layer overlay, image config applied
- **Build:** `pelagos build -t myapp:latest` — Remfile (Dockerfile-compatible syntax)
  with multi-stage builds, ARG, ADD (URLs + archives), `.remignore`, build cache
- **Manage:** `pelagos image ls` / `pelagos image rm`

### WebAssembly / WASI
- **Magic-byte dispatch:** `spawn()` reads the first 4 bytes; `\0asm` triggers the
  Wasm path automatically — the full Linux machinery (namespaces, overlayfs, seccomp,
  pivot_root) is bypassed entirely
- **OCI Wasm images:** pull, run, and build Wasm images from any OCI registry;
  `pelagos image ls` shows a `TYPE` column (`linux` / `wasm`)
- **WASI env + bind mounts:** `--env` passthrough and `--bind host:guest` directory
  mapping with correct host→guest distinction
- **Runtime dispatch:** wasmtime or wasmedge, auto-detected from PATH
- **containerd shim:** `containerd-shim-pelagos-wasm-v1` implements ttrpc shim v2 —
  schedule Wasm pods in Kubernetes via a `RuntimeClass` without a separate node agent
- **`pelagos build` Wasm target:** `FROM scratch` + a `.wasm` output auto-detected
  by magic bytes, stored with `application/wasm` OCI media type

### Multi-Service Orchestration
- **`pelagos compose up/down/ps/logs`:** dependency-ordered service lifecycle
- **TCP readiness:** `:ready-port` polling before dependent services start
- **Scoped resources:** networks, volumes, and container names prefixed per project
- **Lifecycle hooks:** `on-ready` callbacks between startup tiers

### Other
- **Interactive containers:** PTY, SIGWINCH relay, terminal restore
- **`pelagos exec`:** run a command inside a running container (namespace join + PTY)
- **OCI Runtime Spec:** `create` / `start` / `state` / `kill` / `delete` lifecycle
- **Rootless-first:** pull, build, run, overlay, and pasta networking without root — root is an opt-in escape hatch, not the default

---

## Installation

Download a pre-built static binary from the
[Releases](https://github.com/skeptomai/pelagos/releases) page (x86_64 and
aarch64 Linux, statically linked musl), or build from source:

```bash
# Install to /usr/local/bin:
scripts/install.sh

# Or via cargo:
cargo install --path .
```

---

## Quick Start

Pelagos defaults to **rootless** — most operations work without `sudo`. Root is
required only for bridge networking, NAT, port mapping, and OCI lifecycle commands
(`create`/`start`/`kill`/`delete`). For internet access without root, use
`--network pasta` (requires `pasta` from [passt.top](https://passt.top)).

On kernel 5.11+ Pelagos uses native overlayfs with `userxattr` (zero-copy,
kernel-native). On older kernels it falls back to `fuse-overlayfs` automatically.

### Rootless (no sudo)

```bash
pelagos image pull alpine
pelagos run alpine /bin/echo hello

# Interactive shell with internet
pelagos run -i --network pasta alpine /bin/sh
```

### Root (bridge networking, NAT, port mapping)

```bash
sudo pelagos run -i alpine /bin/sh

# Detached container with bridge networking
sudo pelagos run -d --name mybox --network bridge --nat alpine \
  /bin/sh -c 'while true; do echo tick; sleep 1; done'

pelagos ps
pelagos logs -f mybox
sudo pelagos stop mybox && pelagos rm mybox
```

### Multi-service stack

```bash
# A minimal stack — all compose files are Lisp programs (.reml)
sudo -E pelagos compose up -f examples/compose/web-stack/compose.reml -p demo

# With scripting: migrations, conditional startup, parallel execution
sudo -E pelagos compose up -f examples/compose/imperative/compose.reml -p demo
```

---

## Rust Library API

```rust
use pelagos::container::{Command, Namespace};

let mut child = Command::new("/bin/sh")
    .args(&["-c", "echo hello from container"])
    .with_chroot("/path/to/rootfs")
    .with_namespaces(Namespace::UTS | Namespace::MOUNT | Namespace::PID)
    .with_proc_mount()
    .with_seccomp_default()
    .drop_all_capabilities()
    .with_cgroup_memory(256 * 1024 * 1024)
    .spawn()?;

child.wait()?;
```

```rust
// Interactive shell
let session = Command::new("/bin/sh")
    .with_chroot("/path/to/rootfs")
    .with_namespaces(Namespace::UTS | Namespace::MOUNT)
    .with_proc_mount()
    .spawn_interactive()?;

session.run()?;  // relays stdin/stdout, forwards SIGWINCH, restores terminal
```

See the [CLI-to-API translation table](docs/USER_GUIDE.md#cli-to-api-translation)
in the user guide.

---

## Testing

```bash
# Unit tests (no root required):
make test-unit
# or: cargo test --lib

# Integration tests (require root):
sudo -E make test-integration
# or: sudo -E cargo test --test integration_tests

# E2E tests — exercises the full binary via BATS (require root + bats):
sudo -E make test-e2e
# or: sudo -E bats tests/e2e/hardening.bats tests/e2e/lifecycle.bats
```

The E2E suite verifies that `pelagos compose up` applies all four security
hardening defaults to every container it starts, and exercises the full
compose lifecycle (up / ps / down).

See [`docs/INTEGRATION_TESTS.md`](docs/INTEGRATION_TESTS.md) for documentation
of every integration test.

---

## Architecture

### Pre-exec hook order

1. **Parent** — opens namespace files, compiles seccomp BPF, sets up bridge netns
2. **Fork**
3. **Child pre_exec** — unshare → UID/GID maps → setuid/setgid → chroot/pivot_root
   → mounts → **capability drop** → rlimits → setns → seccomp (must be last)
4. **exec** — replace child with target program

Capability drop comes after all mount operations (masked paths, read-only rootfs)
because those mounts require `CAP_SYS_ADMIN`. Seccomp is last because setup
requires syscalls it would otherwise block.

---

## Documentation

| File | Contents |
|------|----------|
| [`docs/USER_GUIDE.md`](docs/USER_GUIDE.md) | CLI and API reference |
| [`docs/REML_EXECUTOR_MODEL.md`](docs/REML_EXECUTOR_MODEL.md) | Lisp scripting: futures graph, `run`, `then`, parallel execution |
| [`docs/INTEGRATION_TESTS.md`](docs/INTEGRATION_TESTS.md) | Every integration test documented |
| [`docs/DESIGN_PRINCIPLES.md`](docs/DESIGN_PRINCIPLES.md) | Non-negotiable design principles |
| [`docs/ROADMAP.md`](docs/ROADMAP.md) | What's done and what's next |
| [`docs/FEATURE_GAPS.md`](docs/FEATURE_GAPS.md) | Gap analysis vs. Docker Desktop / Finch |
| [`docs/RUNTIME_COMPARISON.md`](docs/RUNTIME_COMPARISON.md) | Full feature matrix vs runc/Docker |
| [`docs/SECCOMP_DEEP_DIVE.md`](docs/SECCOMP_DEEP_DIVE.md) | Seccomp-BPF implementation details |
| [`docs/PTY_DEEP_DIVE.md`](docs/PTY_DEEP_DIVE.md) | PTY/interactive session design |
| [`docs/CGROUPS.md`](docs/CGROUPS.md) | Cgroups v1 vs v2 analysis |
| [`docs/BUILD_ROOTFS.md`](docs/BUILD_ROOTFS.md) | How to build the Alpine rootfs |
| [`CHANGELOG.md`](CHANGELOG.md) | Version history and release notes |

---

## Requirements

- Linux kernel 5.11+ recommended (rootless overlay with `userxattr`)
- Kernel 5.0+ works with root, or rootless with `fuse-overlayfs` installed
- `pasta` ([passt](https://passt.top)) for rootless networking
- `nft` (nftables) for NAT and port mapping (root only)
- `ip` (iproute2) for bridge networking (root only)
- `bats` for E2E tests (`sudo pacman -S bats` or `sudo apt install bats`)

---

## License

See LICENSE file for details.
