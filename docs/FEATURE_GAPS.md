# Feature Gaps vs. Finch / Docker Desktop

This document tracks the delta between Remora and a full Docker Desktop / Finch
equivalent.  The goal is to whittle this list down over time.  Items are roughly
ordered by impact.

---

## Context

**Finch** is a Mac packaging layer that bundles Lima (VM) + nerdctl (CLI) +
containerd (daemon) + runc (runtime) + BuildKit (image builder) into a single
`brew install finch`.  It is integration and packaging work, not runtime work.

**Remora** is the opposite end of the stack: it implements container isolation,
networking, image management, and orchestration from scratch on Linux kernel
interfaces, with no daemon and no CNI plugins underneath.

The natural comparison is **Remora vs. runc** (same layer), but Remora extends
upward into networking, image management, and programmable orchestration as
first-class concerns — territory runc explicitly does not cover.

---

## What Remora already has

| Capability | Notes |
|------------|-------|
| Container runtime | Namespaces (UTS/Mount/IPC/Net/PID/Cgroup), seccomp-BPF, capabilities, no-new-privileges, masked paths |
| Security-by-default | All four hardening defaults applied unconditionally to every container |
| OCI image pull | Authenticated pulls; `~/.docker/config.json`; env-var creds; content-addressable layer store |
| OCI image push | `remora image push` — distributes built/pulled images; blob cache for roundtrip |
| Registry login | `remora image login/logout` — writes `~/.docker/config.json` |
| Image build | `remora build` — Remfile (Dockerfile-compatible), multi-stage, ARG, ADD (URLs + archives), build cache, `.remignore` |
| Networking | Loopback, bridge, NAT, TCP port mapping, DNS service discovery, named networks, multi-network containers, pasta (rootless) |
| Multi-service orchestration | `remora compose up/down/ps/logs` — Lisp DSL + futures graph executor |
| Imperative scripting | `.reml` Lisp interpreter: `container-start`, `await-port`, `with-cleanup`, `guard`, parallel graph |
| Interactive containers | PTY, SIGWINCH relay, terminal restore |
| `remora exec` | Namespace join + PTY |
| Volumes / bind mounts / tmpfs / overlay | Full filesystem flexibility |
| Rootless mode | Pull, build, run, overlay (native or fuse), pasta networking |
| OCI Runtime Spec | `create` / `start` / `state` / `kill` / `delete` lifecycle |
| Library API | Embeddable Rust crate — no daemon or subprocess needed |

---

## Gaps

### Critical (blocks real-world use)

#### ~~Registry authentication~~ ✅ COMPLETE
`remora image login <registry>` writes credentials to `~/.docker/config.json`.
`remora image pull --username <u> --password <p>` authenticates pulls.
Auth resolution order: CLI flags → `REMORA_REGISTRY_USER`/`REMORA_REGISTRY_PASS`
env vars → `~/.docker/config.json` (`credHelpers` / `credsStore` / `auths`) → Anonymous.
Credential helpers fully supported: `docker-credential-ecr-login`, OS keychain, etc.

---

#### ~~Image push~~ ✅ COMPLETE
`remora image push <ref> [--dest <registry/repo:tag>]` distributes locally-stored
images via the OCI Distribution Spec.  Blobs are cached during pull/build in
`/var/lib/remora/blobs/`; the OCI config JSON is stored alongside `manifest.json`.
Auth uses the same resolution order as pull.

---

### Significant (limits daily usefulness)

#### ~~`remora image save` / `remora image load`~~ ✅ COMPLETE
OCI Image Layout tar format.  Interoperable with Docker, Podman, skopeo, crane.
`remora image save <ref> [-o file.tar]` and `remora image load [-i file.tar] [--tag ref]`.

---

#### ~~Proper image tagging~~ ✅ COMPLETE
`remora image tag <source> <target>` — assigns a new local reference, shares layers.

---

#### ~~UDP port mapping~~ ✅ COMPLETE
`-p HOST:CONTAINER/udp` and `-p HOST:CONTAINER/both` map UDP (or both TCP+UDP)
via nftables DNAT + a userspace UDP relay for localhost traffic.
The userspace relay uses per-client session sockets with 30-second idle eviction.

> **⚠️ Scaling note (future work):** The current userspace proxy uses one thread
> per port mapping plus one thread per active UDP session. This is fine for
> development workloads but scales poorly under high connection counts. A future
> `remora network proxy` refactor should replace this with a `tokio` async task
> pool (already a dependency). Do not address this now.

---

#### Container healthchecks
Dockerfile `HEALTHCHECK` instruction and runtime health monitoring.  Currently
compose readiness is TCP-port-only; no support for exec-based health probes or
HTTP checks.

---

### Nice to have

#### BuildKit feature parity
BuildKit (used by Finch/Docker) is a separate high-performance build daemon with:
- **Parallel stage execution** — independent FROM stages run concurrently
- **Remote build cache** — push/pull layer cache to/from a registry
- **SSH agent forwarding** in RUN steps (`--mount=type=ssh`)
- **Secret mounts** (`--mount=type=secret`) — secrets available during build, not baked into layers
- **`--platform` cross-compilation** — build amd64 images on arm64 and vice versa

Our builder is solid and covers the common case, but is sequential and
local-cache-only.

---

#### `docker save` / `docker load` tar format compatibility
Our `image save`/`load` (once implemented) should ideally speak the Docker legacy
tar format so images can be exchanged with Docker hosts without a registry.

---

#### Mac / Windows host support
Finch's primary value proposition is the Lima VM wrapper that makes Linux
containers work transparently on Mac.  Remora is Linux-only by nature — all the
interesting code is Linux kernel interfaces.  A Lima-based wrapper (`remora-mac`
or similar) could provide the same experience, but this is a separate packaging
project, not a runtime gap.

---

## Dropped / out of scope

| Item | Reason |
|------|--------|
| containerd / daemon | Explicitly out of scope — daemonless is a design principle |
| CNI plugins | We implement networking natively; CNI adds complexity without benefit |
| Kubernetes CRI | Possible future direction, not current scope |
| Windows containers | Linux-only runtime; not planned |
