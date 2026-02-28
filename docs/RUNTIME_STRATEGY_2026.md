# Remora Runtime Strategy — 2026

This document analyzes the container runtime landscape as of early 2026, compares remora to
major open-source runtimes, and identifies specific technical opportunities where remora can
differentiate.

---

## Runtime Landscape Overview

### The OCI Low-Level Tier (runc class)

| Runtime | Language | Kernel Req. | Cold-start (ms) | Notable Trait |
|---------|----------|-------------|-----------------|---------------|
| **runc** | Go | ≥ 4.14 | ~352 | Reference implementation; ubiquitous |
| **crun** | C | ≥ 4.14 | ~153 | Fastest; smallest binary; default in Podman |
| **youki** | Rust | ≥ 5.4 | ~198 | OCI-compliant Rust runtime; growing adoption |
| **remora** | Rust | ≥ 5.4 | ~150–200* | Integrated CLI+API; DNS SD; compose |

*Estimated from profile similarity to youki. Not yet benchmarked against a standard suite.

**crun** leads on raw cold-start latency because it avoids Go's garbage collector and runtime
initialization. Remora and youki are structurally competitive: both are Rust, both avoid GC
pauses. Remora's current advantage over youki is the integrated feature set (DNS service
discovery, compose, image build) rather than raw speed.

### The Hypervisor / Strong-Isolation Tier

| Runtime | Isolation Model | Overhead | Use Case |
|---------|----------------|----------|----------|
| **Kata Containers 3.x** | Micro-VM (QEMU/Cloud-Hypervisor) | +150–300 ms cold-start; +50–100 MB RSS | Multi-tenant cloud; regulated workloads |
| **gVisor (Systrap)** | Syscall interception (Systrap backend) | +20–60% syscall latency vs native | Google Cloud Run; untrusted code sandboxing |
| **Firecracker** | Lightweight KVM VM | 125 ms startup; <5 MB VMM RSS | AWS Lambda / Fargate; FaaS ephemeral workloads |
| **QEMU/KVM** | Full VM | 1–5 s boot | Legacy compatibility; nested virt |

Remora operates in the OCI low-level tier. These runtimes are not direct competitors; they
serve workloads requiring hardware-level isolation that remora explicitly does not attempt.

### Adjacent Runtimes

| Runtime | Focus | Relevance to Remora |
|---------|-------|---------------------|
| **LXC / Incus** | Long-lived system containers; full OS images | Different mental model; not OCI; no overlap |
| **Ocre** | IoT/embedded; Wasm + OCI; Zephyr/bare-metal | Niche but growing; Wasm integration is relevant |
| **WasmEdge / wasmtime** | WASI runtime with OCI shim layer (`containerd-shim-wasm`) | Converging with container tooling; opportunity |

---

## CVE Analysis — The November 2025 runc Triple

In November 2025 a cluster of three related vulnerabilities was disclosed against runc:

| CVE | Class | CVSS | Summary |
|-----|-------|------|---------|
| **CVE-2025-31133** | TOCTOU | 8.6 | Race in `/proc` path resolution during container setup; allows host breakout |
| **CVE-2025-52565** | TOCTOU | 8.1 | Race in `/dev` node creation; symlink swap wins |
| **CVE-2025-52881** | TOCTOU | 7.9 | `/proc/self/exe` reopen race during exec |

All three exploit the same structural weakness: runc writes into `/proc` or `/dev` during
container initialization *before* the process is fully isolated in its namespace, creating
a window where a malicious container can swap a path component.

**Why remora is structurally immune to this class:**

Remora uses a `pre_exec` hook that runs *inside the child process after `fork()` but before
`exec()`*. All namespace operations (`unshare`, `pivot_root`, seccomp filter application)
happen atomically within a single-threaded child. There is no privileged parent thread
writing into container-visible paths while the container namespace is being constructed.
The seccomp filter is applied *last* in the pre_exec sequence, so all setup syscalls
complete before any restriction is in place — eliminating the partial-setup race window
that runc's multi-process architecture creates.

**Recommended action:** Document this immunity explicitly in `docs/DESIGN_PRINCIPLES.md`
and the README. It is a meaningful security differentiator, especially for teams evaluating
runtimes after the 2025 CVE cluster.

---

## Wasm / WASI Integration Trend

The `containerd-shim-wasm` project (WasmEdge, wasmtime, spin) allows OCI tooling to treat
Wasm modules as container images. As of 2026:

- Docker Desktop ships a Wasm beta shim
- Kubernetes has experimental Wasm node support via `kwasm-operator`
- OCI image spec v1.1 added `application/wasm` media type

**Remora opportunity:** Implement a `WasmMode` backend in `run.rs` that detects
`application/wasm` media type in the image manifest and delegates to a local `wasmtime`
or `wasmer` process instead of spawning a namespaced container. This would position remora
as a unified OCI runner for both Linux containers and Wasm modules — something no other
Rust-native CLI runtime currently does end-to-end.

---

## Security Mechanisms Not Yet in Remora

### 1. AppArmor / SELinux Profiles

Every production deployment of runc/containerd uses either AppArmor or SELinux for
mandatory access control. Neither is implemented in remora. This is the single largest
gap for production/compliance use cases.

- AppArmor: write a profile template in `data/`; apply via `aa_change_profile()` in pre_exec
- SELinux: set process label via `setexeccon()` (libselinux) or write to
  `/proc/self/attr/exec` before exec

**Priority: High.** This is a hard blocker for any regulated environment.

### 2. Landlock LSM (Linux ≥ 5.13)

Landlock is an unprivileged filesystem/network sandboxing mechanism. Unlike AppArmor/SELinux
it requires no policy daemon, no root, and no pre-installed profiles. A process can
self-restrict its own filesystem access via `landlock_create_ruleset()` / `landlock_add_rule()`
/ `landlock_restrict_self()`.

**No major OCI runtime currently integrates Landlock.** runc has an open issue
(#3500, open since 2022); youki has a tracking issue but no implementation.

Remora can be the *first* production OCI-compatible runtime to offer Landlock integration.
Proposed API:

```rust
Command::new("/bin/server")
    .with_landlock(LandlockPolicy::default()
        .allow_read("/etc")
        .allow_read_write("/var/lib/app")
        .deny_all_network_bind())
```

**Priority: Medium-High.** First-mover advantage; pure Rust; no external dependencies.

### 3. `SECCOMP_RET_USER_NOTIF` (Linux ≥ 5.0)

The seccomp user-notification return value allows a privileged supervisor to intercept
specific syscalls from the container and handle them in userspace — instead of killing
the process or returning `EPERM`. This is how `sysbox` implements `/dev/fuse` access and
how `gVisor` patches select syscalls.

Use cases for remora:
- Allow `mount()` only for specific paths without granting `CAP_SYS_ADMIN`
- Mediate `ptrace()` for debugging containers
- Intercept `socket(AF_INET)` calls to implement per-container firewall policy in userspace

**Priority: Medium.** High complexity; no other Rust runtime has it; strong differentiator.

### 4. io_uring Scoped Profile

io_uring is blocked by default in Docker/runc seccomp profiles due to historical privilege
escalation bugs (CVE-2022-29582, CVE-2023-2163). As of kernel 6.6, `io_uring`'s security
model is significantly hardened.

Remora could offer an opt-in `with_seccomp_iouring()` profile that allows io_uring only
after verifying kernel ≥ 6.6 at runtime. This lets high-performance server workloads
(databases, proxies) use io_uring safely inside containers.

**Priority: Low-Medium.** Niche but high-value for performance-sensitive users.

---

## Emerging Use Case: AI Agent Sandboxing

AI agent frameworks (LangChain, AutoGen, smolagents) increasingly need to execute
untrusted code generated by LLMs in isolated environments. Requirements differ from
traditional containers:

| Requirement | Traditional Container | AI Agent Sandbox |
|-------------|----------------------|------------------|
| Startup latency | < 1 s acceptable | < 100 ms preferred |
| Network policy | External HTTP allowed | Egress often blocked by default |
| Filesystem | Persistent volumes | Ephemeral; snapshot on start |
| Lifecycle | Long-lived | Seconds to minutes |
| Observability | Logs | Syscall traces; resource attribution |

Remora's Rust API is well-suited for embedding in agent frameworks. Specific features
that would make remora the preferred embedded runtime for AI sandboxing:

1. **CRIU checkpoint/restore** — snapshot a warm container and restore it in <50 ms for
   repeated invocations of the same language runtime (Python interpreter + imports).
2. **`SECCOMP_RET_USER_NOTIF`** — intercept `connect()` to implement egress policy
   without network namespace overhead.
3. **Landlock** — restrict filesystem access to the agent's working directory without
   a seccomp profile update.

**Priority: Strategic.** This is the highest-growth use case for lightweight runtimes
in 2026. A remora crate published to crates.io with a clean async API would enable
embedding in Rust-based agent frameworks directly.

---

## Performance Benchmark Targets

The Kinvolk/Benchmark suite (OCI runtime bench) measures:

| Metric | crun | youki | runc |
|--------|------|-------|------|
| Cold start (median, ms) | 153 | 198 | 352 |
| RSS at idle (MB) | 3.1 | 4.8 | 6.2 |
| `execve` overhead vs bare | 1.03× | 1.07× | 1.18× |

Remora is not yet in this benchmark suite. To add remora:
1. Implement the full OCI runtime spec lifecycle (currently partial)
2. Submit a PR to the benchmark repo with a remora shim config
3. Run the suite with `sudo benchmark.sh --runtime remora`

**Target:** Median cold-start ≤ 180 ms (between crun and youki), RSS ≤ 4 MB.

---

## Embedded / IoT Landscape

**Ocre** (Eclipse Foundation) targets constrained devices (Cortex-M, RISC-V) with:
- WASI-based application model
- OCI image pull (subset)
- No MMU required

Remora's kernel dependency (≥ 5.4, full MMU, cgroups v2) makes it unsuitable for MCU
targets. However, for **Linux-based embedded systems** (Raspberry Pi, industrial PCs,
automotive SoCs running AGL/Yocto), remora is competitive. A static binary <10 MB with
no daemon would be attractive in these environments.

**Recommended action:** Add a `cargo build --features minimal` profile that strips
image pull, DNS daemon, compose, and network bridges — producing a single-binary runtime
suitable for embedded Linux deployment.

---

## Rust Ecosystem Positioning

As of 2026, Rust-native container components include:

| Component | Rust? | Notes |
|-----------|-------|-------|
| youki | ✅ | OCI runtime only; no CLI tooling |
| Kata agent | ✅ | Agent inside VM; not host-side |
| Firecracker VMM | ✅ | KVM VMM; not OCI |
| containerd | ❌ Go | Daemon-based |
| BuildKit | ❌ Go | Image builder |
| **remora** | ✅ | Runtime + CLI + image + compose + DNS |

Remora is the only Rust project that integrates the full stack: runtime API, CLI,
image pull/build, compose orchestration, and DNS service discovery. This breadth is
both a strength (less context-switching for users) and a risk (more surface to maintain).

---

## Prioritized Opportunity List

In descending order of strategic impact:

| # | Opportunity | Effort | Impact |
|---|-------------|--------|--------|
| 1 | Document structural CVE immunity (TOCTOU class) | Quick | High — marketing + security credibility |
| 2 | AppArmor / SELinux profile support | Significant | High — production blocker for regulated env |
| 3 | Landlock LSM integration | Moderate | High — first-mover; pure Rust; no deps |
| 4 | Publish remora as a crate on crates.io | Quick | High — enables embedding in agent frameworks |
| 5 | `SECCOMP_RET_USER_NOTIF` supervisor mode | Significant | Medium-High — enables egress/mount policy |
| 6 | Wasm/WASI shim mode (`WasmMode`) | Moderate | Medium — rides OCI+Wasm convergence trend |
| 7 | OCI runtime bench submission | Quick | Medium — visibility + cold-start regression guard |
| 8 | CRIU checkpoint/restore | Significant | Medium — AI agent sandboxing; warm-start |
| 9 | io_uring opt-in seccomp profile | Quick | Low-Medium — niche but high-value |
| 10 | Minimal `--features minimal` build | Moderate | Low-Medium — embedded Linux |

---

## Summary

Remora is structurally sound and already feature-complete for most developer use cases.
The primary gaps versus production-grade runtimes are:

1. **AppArmor/SELinux** — mandatory for regulated deployments
2. **OCI compliance completeness** — partial `create/start/state/kill/delete` cycle

The primary differentiation opportunities are:

1. **Landlock LSM** — first Rust runtime to integrate it
2. **Structural TOCTOU immunity** — worth documenting loudly given the 2025 CVE cluster
3. **AI agent embedding** — crates.io publication + `SECCOMP_RET_USER_NOTIF` + CRIU
