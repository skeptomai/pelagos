# Rusternetes on Pelagos — Integration Plan

*2026-04-29.*

---

## The Model: Everything Runs in Linux

The correct architecture — confirmed by how Docker Desktop, Rancher Desktop, and Colima all solve the same problem on macOS — is that the entire Kubernetes stack runs **inside a Linux VM**. The Mac is a client. kubectl on macOS reaches the API server via a port-forwarded endpoint. There is no networking topology problem because the kubelet and the containers it manages are on the same network.

```
macOS host
  kubectl ──────────────────────────────────────────────── port-forward :6443
  pelagos --profile build vm ssh ──────────────────────── SSH / vsock shell

  pelagos build VM (Ubuntu 24.04, 192.168.106.2)
    rusternetes (api-server, scheduler, controller-manager, kubelet)
    pelagos-dockerd                ← new binary, pelagos repo
    pelagos (runtime)
    containers                     ← same network as kubelet, no gap
```

This is not a compromise or workaround — it is the correct architecture. Running the control plane inside Linux and exposing only the API port to the Mac is exactly what every production-quality macOS Kubernetes implementation does.

---

## Development Environment: The Build VM

All implementation and testing happens inside the **pelagos build VM** (Ubuntu 24.04, glibc, aarch64, at `192.168.106.2`). It already has:
- Rust toolchain installed
- pelagos source virtiofs-mounted at `/mnt/Projects/pelagos`
- Standard `cargo build --release` works (glibc, no zigbuild needed)
- SSH access via `pelagos --profile build vm ssh`

Rusternetes is compiled for Linux glibc inside this VM. Containers run in this VM via pelagos. The kubelet talks to pelagos-dockerd on a local Unix socket. Everything is local to one Linux environment.

---

## The Integration Point in Rusternetes

The kubelet isolates all container operations in one struct: `ContainerRuntime` in `crates/kubelet/src/runtime.rs`, wrapping a `bollard::Docker` client. It connects via:

```rust
let docker = Docker::connect_with_local_defaults()?;
```

`connect_with_local_defaults()` respects `DOCKER_HOST`. Pointing it at the pelagos Docker socket requires no code change in rusternetes — only an environment variable.

The kubelet uses approximately 12 Docker Engine API calls:

| bollard call | Purpose |
|---|---|
| `docker.inspect_image()` | Check if image is cached locally |
| `docker.create_image()` | Pull image from registry |
| `docker.list_containers()` | Find containers by label |
| `docker.create_container()` | Create (not yet start) a container |
| `docker.start_container()` | Start a created container |
| `docker.stop_container()` | Graceful stop |
| `docker.kill_container()` | SIGKILL |
| `docker.remove_container()` | Remove stopped container |
| `docker.inspect_container()` | Get state, IP address, status |
| `docker.create_exec()` / `start_exec()` | kubectl exec |
| `docker.logs()` | Container log streaming |
| `docker.stats()` | CPU/memory metrics |

---

## The Work: pelagos-dockerd

A new binary in the **pelagos** repo. A Unix socket HTTP server implementing the subset of the Docker Engine REST API that rusternetes's kubelet actually calls. It translates each Docker API call into the equivalent pelagos library or CLI operation.

This lives in pelagos, not pelagos-mac. It is a Linux binary. It has no knowledge of VMs, vsock, or macOS.

**What it implements:**
- HTTP server on a configurable Unix socket path
- The ~12 Docker API endpoints listed above
- Docker JSON request/response schemas mapped to pelagos operations
- Docker exec HTTP upgrade + stdio framing (the most complex part)
- Log streaming via chunked HTTP

**What it does not implement:**
- The full Docker Engine API (swarm, plugins, system events, etc.)
- Anything rusternetes doesn't call

**rusternetes changes:** Zero for the integration. `DOCKER_HOST=unix:///run/pelagos/dockerd.sock` in the environment is all that changes.

---

## Key Differences to Bridge (in pelagos-dockerd)

| Docker/bollard behaviour | pelagos equivalent |
|---|---|
| Pause container for pod network namespace sharing | pelagos named bridge networks (N2b) handle multi-container net-ns sharing natively; shim synthesises fake pause container responses without starting one |
| Container labels for pod tracking | pelagos has no label system — encode pod/container identity into container names |
| Docker bridge assigns pod IPs, returned via inspect | pelagos N2 bridge assigns IPs — same concept, different query path |
| `hostPath` / `emptyDir` as bind mounts | pelagos `with_bind_mount()` / `with_volume()` — direct equivalents |
| Exec via HTTP upgrade + raw stdio | pelagos exec via framed binary protocol (type + u32 len + data) — translate in shim |
| Log streaming via chunked HTTP | pelagos captures stdout/stderr — bridge to chunked HTTP in shim |
| Stats from daemon (CPU, memory) | pelagos `child.resource_stats()` via cgroups-rs — map to Docker stats JSON |

---

## Phases

**Phase 1 — pelagos-dockerd (pelagos repo)**
New binary implementing the Docker Engine API subset. Test independently with `docker` CLI and bollard before involving rusternetes. Validate: `docker ps`, `docker run`, `docker exec`, `docker logs`.

**Phase 2 — wire up rusternetes**
Set `DOCKER_HOST=unix:///run/pelagos/dockerd.sock` in the kubelet environment. No code changes. Validate with a simple pod (`nginx`), then work up to Deployments, Services, and health probes.

**Phase 3 — optional: expose API server to macOS**
Run rusternetes with `-p 6443:6443` on the build VM profile. Set kubeconfig on macOS to `https://127.0.0.1:6443`. kubectl and the rusternetes web console become accessible from the Mac without SSH.

**Phase 4 — optional: native Pelagos backend in rusternetes**
Trait-ify `ContainerRuntime` in rusternetes's kubelet and add a `PelagosRuntime` that calls pelagos directly, removing the Docker API translation layer. Separable cleanup once the integration is proven.

---

## What pelagos-mac Does Not Need to Change

Nothing, for this integration. pelagos-mac already supports port-forwarding (`-p host:guest`) for API server access. The Docker socket shim is a Linux binary in the pelagos repo. The networking topology problem that exists when running rusternetes on the macOS host does not exist when rusternetes runs inside the VM — which is the correct architecture.

---

## Files to Create / Modify

| Location | What |
|---|---|
| `pelagos/src/bin/pelagos-dockerd.rs` (or `pelagos/crates/dockerd/`) | New Docker Engine API server |
| `rusternetes/` — environment config | `DOCKER_HOST` pointing at pelagos socket |
| `rusternetes/crates/kubelet/src/runtime.rs` | (Phase 4 only) Extract `ContainerRuntimeTrait` |
| `rusternetes/crates/kubelet/src/kubelet.rs` | (Phase 4 only) `Arc<ContainerRuntime>` → `Arc<dyn ContainerRuntimeTrait>` |
