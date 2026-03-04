# Wasm/WASI Support in Pelagos

## Overview

Pelagos runs both Linux OCI containers and WebAssembly modules from the same
CLI, the same image store, and the same compose format. No separate toolchain,
no separate node configuration. A `pelagos run` invocation works whether the
target is an ELF binary in a layered rootfs or a `.wasm` module stored as a
raw OCI blob.

This document covers the architecture of the current Wasm implementation, what
you can do with it today, where it sits relative to other runtimes, and what
the next layer of capability looks like.

---

## Architecture

Wasm support is built in three layers, each independent and testable.

### Layer 1 — Binary detection and runtime dispatch (`src/wasm.rs`)

`spawn()` reads the first 4 bytes of the target executable before doing
anything else. WebAssembly modules always begin with `\0asm`
(`0x00 0x61 0x73 0x6D`). If those bytes are present — or if a WASI config
was explicitly attached to the `Command` — the entire Linux machinery
(namespaces, cgroups, seccomp, pivot_root, overlay mounts) is bypassed and
the module is handed to an installed Wasm runtime instead.

```
spawn() called
    │
    ├─ read 4 bytes from program path
    │       │
    │       ├─ 0x00 0x61 0x73 0x6D  → spawn_wasm_impl()
    │       │       │
    │       │       ├─ find wasmtime in PATH  → build wasmtime command
    │       │       └─ find wasmedge in PATH  → build wasmedge command
    │       │
    │       └─ anything else → normal Linux fork/exec path
    │
    └─ wasi_config explicitly set → always takes Wasm path
```

Runtime selection is controlled by `WasmRuntime` (`Wasmtime | WasmEdge |
Auto`). `Auto` (the default) tries wasmtime first, then wasmedge.

`WasiConfig` carries three things passed to the runtime subprocess:

- `env: Vec<(String, String)>` — appears as `--env KEY=val` flags
- `preopened_dirs: Vec<(PathBuf, PathBuf)>` — `(host, guest)` pairs;
  appears as `--dir /host::/guest` (wasmtime) or `--dir /host:/guest`
  (wasmedge). The host→guest distinction matters: `--bind /data:/app/data`
  maps the host directory `/data` to the guest path `/app/data`, not to
  `/data` inside the module.
- `runtime: WasmRuntime` — selects the backend

**Why subprocess dispatch rather than embedding?**

Embedding wasmtime as a Rust library is possible (the `wasmtime` crate exists)
but adds ~15MB to the binary, pulls in a large dependency tree, and couples
pelagos's release cadence to the Wasm runtime's. Subprocess dispatch means:

- Users upgrade wasmtime independently and immediately get new WASI features,
  bug fixes, and security patches without a pelagos release.
- The pelagos binary stays small and focused.
- Cold-start cost is one extra process spawn (~3–5ms) — acceptable for all
  but the highest-frequency serverless use cases.

### Layer 2 — OCI Wasm artifact support (`src/image.rs`, `src/cli/image.rs`)

The OCI image spec allows layer blobs with non-tarball media types. Three
media types are recognised as Wasm:

| Media type | Used by |
|---|---|
| `application/wasm` | Generic |
| `application/vnd.wasm.content.layer.v1+wasm` | OCI Wasm Working Group |
| `application/vnd.bytecodealliance.wasm.component.layer.v0+wasm` | Bytecode Alliance (components) |

When `pelagos image pull` encounters a layer with one of these types, it
copies the raw blob as `<layer-dir>/module.wasm` rather than decompressing a
tarball. The `ImageManifest` records the media type for each layer in the
`layer_types` field (backward-compatible via `#[serde(default)]`).

At run time, `build_image_run()` checks `manifest.is_wasm_image()`. If true,
it skips overlay filesystem setup entirely, extracts the module path, and
builds a `Command` routed through `spawn_wasm_impl()`. The full Linux
container machinery — overlayfs, namespaces, seccomp, pivot_root — is never
invoked for a Wasm image.

`pelagos image ls` shows a `TYPE` column (`wasm` or `linux`) so you can see
at a glance what's in the local store.

### Layer 3 — containerd shim (`src/bin/pelagos-shim-wasm.rs`)

`containerd-shim-pelagos-wasm-v1` implements the containerd shim v2 protocol
over ttrpc. Install (or symlink) it into PATH and add to containerd's config:

```toml
[plugins."io.containerd.grpc.v1.cri".containerd.runtimes.wasm]
  runtime_type = "io.containerd.pelagos.wasm.v1"
```

containerd then manages Wasm containers through the standard CRI interface —
the same interface Kubernetes uses. The shim lifecycle maps directly to the
OCI runtime operations:

| containerd call | Shim action |
|---|---|
| `create` | Parse OCI bundle `config.json`, record state |
| `start` | Call `spawn_wasm()`, track PID |
| `state` | Poll child liveness via `try_wait()` |
| `kill` | Forward signal via `nix::sys::signal::kill` |
| `wait` | Block on `child.wait()`, return exit status |
| `delete` | Drop child handle, clean up state |
| `shutdown` | Signal the shim process to exit |

---

## What you can do today

### Run a Wasm binary directly

```bash
# From host filesystem — magic-byte detection triggers automatically
sudo pelagos run /path/to/app.wasm

# With WASI environment variables
sudo pelagos run --env DATABASE_URL=postgres://... /path/to/app.wasm

# With a preopened host directory
sudo pelagos run --bind /host/data:/data /path/to/app.wasm
```

### Pull and run a Wasm OCI image

```bash
# Pull a Wasm image (TYPE column shows "wasm")
pelagos image pull ghcr.io/example/my-wasm-app:latest
pelagos image ls
# REPOSITORY                          TAG     TYPE   SIZE
# ghcr.io/example/my-wasm-app        latest  wasm   1.8 MB

# Run it — no rootfs, no overlayfs, starts in milliseconds
sudo pelagos run ghcr.io/example/my-wasm-app:latest

# Pass env and bind mounts
sudo pelagos run \
    --env CONFIG=/config/app.toml \
    --bind /etc/myapp:/config \
    ghcr.io/example/my-wasm-app:latest
```

### Use with containerd / Kubernetes

```bash
# Install the shim
sudo cp target/release/pelagos-shim-wasm \
    /usr/local/bin/containerd-shim-pelagos-wasm-v1

# Configure containerd (add to /etc/containerd/config.toml)
# [plugins."io.containerd.grpc.v1.cri".containerd.runtimes.wasm]
#   runtime_type = "io.containerd.pelagos.wasm.v1"

# Schedule a Wasm pod in Kubernetes via RuntimeClass
kubectl apply -f - <<EOF
apiVersion: node.k8s.io/v1
kind: RuntimeClass
metadata:
  name: pelagos-wasm
handler: wasm
EOF
```

---

## Comparison with other runtimes

### Wasm-specific runtimes

**runwasi** (CNCF) is the reference implementation for containerd Wasm shims.
It embeds wasmtime (and optionally wasmedge or WasmEdge) as a library and
provides a shim-per-runtime architecture. Our shim implements the same ttrpc
protocol and is structurally identical. The difference: runwasi is Wasm-only;
pelagos handles both.

**Spin** (Fermyon) is an application framework built on wasmtime, optimised
for HTTP trigger workloads. It has a richer programming model (components,
typed interfaces) but is purpose-built for Spin applications — not a
general-purpose container runtime.

**WasmEdge** ships both a runtime and a containerd shim. Its WASI preview 2
socket support is ahead of wasmtime's CLI surface, making it stronger for
network-facing Wasm workloads today.

### General-purpose container runtimes

No other general-purpose Linux container runtime (runc, crun, youki) handles
Wasm natively. They treat `.wasm` files as opaque executables and attempt to
run them through the kernel's ELF loader, which fails immediately.

The meaningful comparison is:

| | pelagos | runc/crun | runwasi | Spin |
|---|---|---|---|---|
| Linux OCI containers | ✅ full | ✅ full | ❌ | ❌ |
| Wasm OCI images | ✅ | ❌ | ✅ | ✅ |
| containerd shim | ✅ | ✅ (via runc) | ✅ | via shim |
| Same CLI for both | ✅ | n/a | n/a | n/a |
| Same image store for both | ✅ | n/a | n/a | n/a |
| Mixed Linux+Wasm compose | ✅ (code path exists) | ❌ | ❌ | ❌ |
| Embedded Wasm runtime | ❌ (subprocess) | n/a | ✅ | ✅ |
| WASI preview 2 | via runtime upgrade | n/a | via runtime upgrade | partial |
| Wasm Component Model | ❌ | ❌ | partial | ✅ |
| Cold-start overhead | ~5ms (process spawn) | ~50–100ms | ~1–2ms | ~1ms |

The unique value proposition is the unified model: one node, one runtime, one
image registry, one CLI, one compose format for Linux and Wasm workloads
running side-by-side.

---

## Current limitations

**Subprocess dispatch overhead.** Each Wasm module invocation spawns a new
wasmtime/wasmedge process. For long-running services this is immaterial. For
high-frequency short-lived functions (>100/s on the same node) an embedded
runtime with a persistent VM pool would be faster.

**WASI surface is env + preopened dirs only.** WASI preview 2 defines
sockets, HTTP, clocks, random, and a typed component interface. wasmtime
exposes these via its Rust API but not all of them through the CLI. We
currently expose only what the CLI supports.

**No Wasm Component Model.** Components are the composable future of Wasm —
modules that export and import typed interfaces, composed at the host level.
The component media type (`application/vnd.bytecodealliance.wasm.component.layer.v0+wasm`)
is recognised and stored correctly, but execution is identical to a plain
module. Component-aware execution requires embedding the wasmtime Rust crate.

**Mixed Linux+Wasm compose is untested.** The code path exists — a `.reml`
compose file can reference a Wasm image service alongside Linux services, and
DNS service discovery runs independently of container type. But it has not
been exercised in tests.

**No persistent Wasm VM pool.** Each `pelagos run` spawns a fresh runtime
process. A pelagos daemon mode that keeps a warm VM pool would reduce
cold-start latency to sub-millisecond for high-frequency invocations.

---

## Roadmap

See GitHub epic #67 for the tracked issues. High-level priorities:

1. **Mixed Linux+Wasm compose test** — exercise and validate the existing
   code path with a `.reml` file containing both service types.

2. **WASI preview 2 socket passthrough** — thread wasmtime's `--wasi` flags
   for TCP/UDP socket capability through `WasiConfig`, enabling network-facing
   Wasm services without a Linux network namespace.

3. **Wasm Component Model execution** — embed `wasmtime` as a library
   (behind a `--features embedded-wasm` Cargo flag) to support component
   composition and eliminate the subprocess overhead for production deployments.

4. **Persistent VM pool** — a pelagos Wasm daemon that pre-warms a pool of
   runtime instances, reducing cold-start to sub-millisecond.

5. **`pelagos build` Wasm target** — extend Remfile with a `FROM wasm32-wasip1`
   syntax that compiles Rust/C/Go source to a Wasm OCI image without requiring
   a separate toolchain setup.
