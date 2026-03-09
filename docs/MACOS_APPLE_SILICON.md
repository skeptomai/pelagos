# pelagos on Apple Silicon — Design Options

*Researched 2026-03-09. Updated 2026-03-09 with objc2-virtualization findings.*

---

## Background

pelagos uses Linux namespaces, cgroups, and seccomp — Linux-only primitives. On Apple
Silicon a Linux VM is mandatory. The goal is a polished developer tool comparable to
AWS Finch: single installer, transparent CLI, good I/O performance, Rosetta 2 for
x86_64 images.

**VM layer options evaluated:** Lima/VZ, vfkit, raw QEMU, Apple Containerization
(macOS 26), crosvm (no macOS support), cloud-hypervisor/hypervisor-framework
(low-level Rust), Multipass (GPL v3, QEMU-based — eliminated).

**Bottom line on the VM layer:** Lima with the `vmType: vz` backend (Apple
Virtualization Framework) is the correct substrate. It is Apache 2.0, CNCF
Incubating, used by Finch/Colima/Rancher Desktop, gives 3–8s VM boot with virtiofs
file sharing, and is actively maintained. QEMU is slower (15–40s boot, virtio-9p I/O)
and only relevant for cross-arch emulation. The designs below differ on IPC
architecture and distribution model, not on the VM layer.

---

## Option A — Lima/VZ + SSH passthrough (Finch model)

**Architecture:**
```
macOS: pelagos-mac CLI  ──SSH──►  Lima VM (arm64 Alpine)
                                    └─ pelagos binary
```

The macOS CLI (`pelagos-mac`) shells out to `limactl shell` or an SSH command to run
`pelagos` inside the VM. Path and volume arguments are translated (host path → virtiofs
mount path). Lima manages VM lifecycle, virtiofs file sharing, and port forwarding.
Packaged as a signed `.pkg` installer bundling Lima + a custom Lima template + the
pelagos binary for aarch64 Linux.

**Pros:**
- Shortest path to a working product — Finch proved it at scale
- Lima handles virtiofs, port forwarding, VM lifecycle, SSH key management — no reinvention
- Apache 2.0 (Lima) — clean license; distributable in commercial products
- Lima's gRPC-based port forwarder is already robust for `--publish`
- VM stays running; container starts are sub-second after first boot
- Rosetta 2 available via Lima VZ config (`rosetta.enabled: true`)
- Lima is embeddable via Go packages, not just subprocess (Colima proves this)

**Cons:**
- Every CLI invocation forks an SSH process — measurable latency per command (~50–200ms overhead) vs a persistent socket
- Go dependency: Lima is Go; using Lima as a library requires compiling/linking Go, or shipping the `limactl` binary as a subprocess target
- Less control over UX — Lima's abstractions are opinionated (socket path conventions, network config)
- Component version pinning: each Lima update requires a new pelagos-mac release
- SSH passthrough means streaming logs (`pelagos logs --follow`) requires SSH multiplexing or a separate channel

**Performance:** virtiofs I/O reaches 60–80% of native macOS. VM boot 3–8s, container starts sub-second.

**Distribution:** signed `.pkg` installer; follows Finch's model exactly.

**Effort:** Moderate. Most complexity is in macOS CLI path translation and installer packaging, not VM management.

---

## Option B — vfkit + Rust orchestrator + vsock daemon

**Architecture:**
```
macOS: pelagos-mac CLI  ──vsock──►  vfkit VM (arm64 Alpine)
            │                          └─ pelagos-daemon (Rust gRPC/JSON over vsock)
            └──► vfkit subprocess
```

The macOS host binary spawns `vfkit` as a child process with a constructed argument
list to start a minimal Linux VM. Inside the VM, `pelagos-daemon` listens on a
virtio-vsock socket. The macOS CLI connects directly over vsock — no SSH involved. VM
lifecycle (start/stop/clean shutdown) is managed by the Rust host binary.

**Pros:**
- No Go runtime dependency — vfkit binary (~15 MB) is the only foreign component; everything else is Rust
- vsock is a direct host-guest channel: faster than SSH, lower latency per command (< 5ms typical)
- Full control over the protocol — can use gRPC, JSON-RPC, or a custom framing
- Streaming (logs, exec I/O) is clean via vsock multiplexed streams, no SSH channel gymnastics
- vfkit is Apache 2.0, Red Hat maintained, used by CRC + Podman Machine in production
- Tighter control over VM config (disk size, memory, vCPUs) from the Rust side

**Cons:**
- File sharing must be built: vfkit provides the virtiofs device but not the host-side path translation, port forwarding automation, or socket management that Lima includes — all must be implemented
- Port forwarding: must implement vsock-to-TCP forwarding on the host side (or use virtio-net + host routing)
- VM lifecycle management (first boot, kernel extraction, disk image management) must be built from scratch
- More total engineering work than Option A before reaching feature parity with Lima
- vfkit subprocess model means monitoring its health, handling crashes, restart policy
- The `vfkit` binary itself is a distribution dependency (must be bundled in the installer)

**Performance:** Same AVF/VZ baseline as Lima/VZ. vsock IPC is faster than SSH for command latency.

**Distribution:** signed `.pkg` or Homebrew cask. Rust binary + vfkit binary + Linux VM image (kernel + initrd + root disk) are bundled.

**Effort:** Significant. VM lifecycle management and file sharing from scratch is a substantial project.

---

## Option C — Lima/VZ + persistent gRPC daemon (hybrid)

**Architecture:**
```
macOS: pelagos-mac CLI ──Unix socket──► pelagos-mac daemon ──vsock──► pelagos-daemon (gRPC)
                                                │
                                            Lima VM (VZ)
```

Lima manages the VM. Instead of SSH-per-command passthrough, a persistent
`pelagos-daemon` gRPC server runs inside the VM and is reachable from the host via a
Unix domain socket forwarded through Lima's vsock channel. The macOS daemon handles VM
startup and socket lifecycle. The macOS CLI connects to the local Unix socket.

**Pros:**
- Combines Lima's VM management (virtiofs, port forwarding, Rosetta, lifecycle) with low-latency persistent IPC
- CLI command latency drops to < 5ms (no SSH fork per invocation)
- Streaming (logs, exec) is naturally supported via gRPC server streaming
- The gRPC interface is a clean API boundary — the macOS CLI can be thin; the daemon is the contract
- Docker socket compatibility: the daemon can expose a Docker-compatible Unix socket as a future path to drop-in compatibility

**Cons:**
- More moving parts than Option A: the macOS daemon, gRPC server inside the VM, socket forwarding layer — all must be built and maintained
- gRPC protocol design is non-trivial: defining the protobuf interface for all pelagos operations (run, build, compose, exec, logs, etc.)
- Lima's vsock channel for Unix socket forwarding must be confirmed to work reliably for this use case
- Still has the Lima component version pinning problem
- **See security analysis — this option has the weakest security posture**

**Performance:** Lima's VM substrate + persistent socket IPC. CLI roundtrip < 5ms after initial connect.

**Distribution:** signed `.pkg` with Lima + pelagos-mac-daemon + pelagos Linux binary.

**Effort:** Significant upfront (protocol design, daemon, socket forwarding).

---

## Option D — Homebrew formula + Lima template (minimal/community path)

**Architecture:**
```
brew install pelagos           # installs: lima + pelagos Lima template
lima create --template pelagos # starts VM
pelagos run ...                # thin wrapper: limactl shell pelagos -- pelagos run ...
```

A Homebrew formula that declares Lima as a dependency and installs a Lima instance
template YAML (configuring VZ, virtiofs mounts, Rosetta, and the pelagos binary path).
No custom installer, no bundled components. Users manage Lima separately.

**Pros:**
- Minimal engineering: no installer packaging, no custom VM lifecycle code
- Leverages Homebrew's update mechanism — Lima and pelagos update independently
- Low maintenance burden
- Good for early adoption/community experimentation
- Users who already have Lima installed can use the template directly

**Cons:**
- Not "complete" — user experience is fragmented (separate Lima and pelagos updates, manual `lima create`)
- No control over Lima version compatibility — Lima breaking changes affect pelagos without a release
- No macOS daemon socket — tools expecting a Docker socket cannot connect
- Least polished: no native macOS CLI, no signed installer, not enterprise-ready
- Path translation for bind mounts requires user awareness of virtiofs mount points

**Performance:** Same Lima/VZ baseline.

**Distribution:** `brew tap pelagos/tap && brew install pelagos`. No installer.

**Effort:** Small — primary work is the Lima template YAML and wrapper script.

---

## Option E — Apple Containerization (VM-per-container, future)

**Architecture:**
```
macOS: pelagos-mac CLI  ──►  Swift Containerization framework
                                └─ VM per container (AVF, ~50ms start)
                                └─ pelagos replaces vminitd as init system
```

Apple's `Containerization` Swift framework (Apache 2.0, WWDC 2025) provides a
VM-per-container model with sub-second starts. Each pelagos container runs in its own
dedicated micro-VM — native macOS, no shared Linux environment, true isolation.

**Pros:**
- VM-per-container is architecturally cleaner than a shared Linux VM for isolation guarantees
- Apple-native: no third-party dependencies, likely to be well-optimized over time
- Sub-50ms container start time (Apple's claim)
- OCI image support built in
- Apache 2.0 license

**Cons:**
- **Requires macOS 26 Tahoe** — in beta as of early 2026; full container networking requires macOS 26
- Swift-only API — pelagos is Rust; calling Swift from Rust requires FFI bridging or a subprocess model; no C interface
- Too new: framework is 0.x, undocumented in places, no production use
- The VM-per-container model bypasses pelagos's namespace/cgroup machinery — pelagos becomes the Linux init inside each VM, a significant redesign
- Apple can and does break 0.x APIs

**Performance:** Best theoretical; real-world benchmarks not yet available.

**Distribution:** macOS 26+ only — limits addressable audience until 2027+.

**Effort:** Very high, and blocked on macOS 26 availability.

---

## Security Analysis

Five dimensions per option:

1. **Authentication** — who is allowed to send commands to the runtime?
2. **Privilege granted on compromise** — what can an attacker do if the IPC channel is reached?
3. **Host attack surface** — what processes/sockets are listening on the macOS host?
4. **Network exposure** — reachable beyond localhost / the local user?
5. **Container escape / VM isolation** — can a container break out to the host?

### Option A — Lima/VZ + SSH passthrough

**Authentication:** OpenSSH with an ephemeral keypair generated per VM instance by
Lima. Stored at `~/.lima/<instance>/ssh.key` (mode 0600). Authentication is handled
entirely by the OS-level SSH infrastructure — not custom code.

**Privilege on compromise:** An attacker who obtains the Lima SSH private key can SSH
into the VM and run pelagos with the same privileges as the host user mapped into the
VM. They cannot directly reach the macOS host filesystem except via the virtiofs share
(which exposes only explicitly declared directories).

**Host attack surface:** Lima's hostagent Unix socket at `~/.lima/<instance>/ha.sock`
(mode 0600). The SSH daemon listens inside the VM on vsock — not a host TCP port. Port
forwards are localhost-only and opt-in.

**Container escape:** Namespace-based inside the Alpine VM. A container escape reaches
the VM but not the macOS host directly. AVF is the second boundary. The virtiofs share
scope determines the blast radius.

**Summary:** Strong. SSH is the most audited remote access protocol in existence.
Attack surface is minimal and well-understood. Risk scales with the breadth of the
virtiofs share — mounting `/Users/you` is much worse than mounting a specific project
directory.

---

### Option B — vfkit + Rust orchestrator + vsock daemon

**Authentication:** No credential exchanged. The host-side Rust orchestrator forwards
the vsock connection to a Unix domain socket; that socket's filesystem permissions
(0600, owner only) are the sole authentication boundary. This is the Docker socket
model — ownership of the socket file is the key. On macOS with AVF, vsock ports are not
system-wide sockets; they are mediated by the VMM process (vfkit), so external
processes cannot reach the guest directly. The risk is on the host-side forwarded
socket.

**Privilege on compromise:** Whoever reaches the Unix socket can issue any pelagos
command — run with arbitrary bind mounts, exec into containers, read any log. This is
Docker socket equivalent: functionally root-equivalent for the VM and any host paths in
the virtiofs share.

**Host attack surface:** The Unix domain socket (persistent daemon). The vfkit child
process. Any bug in a handler that accepts path arguments (bind mount source, working
directory) is a potential injection vector for someone who can reach the socket.

**Container escape:** Same namespace isolation inside the VM, AVF as the outer
boundary.

**Summary:** Moderate. The socket-as-authentication model is a known footgun. The
required mitigation — strict Unix socket permissions — is well understood but must be
implemented correctly and audited. Input validation in every handler that accepts paths
or commands is mandatory.

---

### Option C — Lima/VZ + persistent gRPC daemon

**Authentication:** This is the critical weakness. **gRPC has no built-in
authentication.** Options are:
- Unix socket ownership — necessary but not sufficient
- mTLS client certificates — correct solution; significant implementation and UX complexity
- Bearer tokens in gRPC metadata — weak; tokens can be stolen from process memory or swap

Without mTLS, the daemon is authenticated only by socket file ownership — same posture
as Option B, but with a larger and more complex attack surface.

**Privilege on compromise:** Highest of all options. The gRPC interface is a
**general-purpose privileged execution API** built to accept arbitrary container
operations. Compromise = create containers with any bind mount, exec arbitrary commands
in any container, read any log, manipulate any volume. This is the Docker daemon
problem, rebuilt from scratch with custom code that lacks Docker's decade of hardening.

The Docker daemon's history is directly instructive: the socket was root-equivalent
from day one, leading to years of CVEs, `--userns-remap`, rootless Docker, and
eventually VM isolation as the primary mitigation. A custom gRPC daemon reproduces this
architecture without that history.

**Host attack surface:** The macOS-side daemon (persistent process + Unix socket) + the
in-VM gRPC server + the vsock forwarding layer — three components vs. one (Lima's
hostagent) in Option A. Each is an attack surface; each has bugs. If the gRPC server
inside the VM accidentally binds to `0.0.0.0` instead of the vsock interface — a common
misconfiguration — it becomes network-reachable.

**Input injection:** Every gRPC handler that accepts a path (bind mount source, COPY
source in build, working directory, log path) is a potential path traversal or injection
vector for any caller who reaches the socket. This class of bug is pervasive in
container runtime implementations.

**Container escape:** Same namespace isolation. But the gRPC daemon runs with elevated
privileges inside the VM (it must — to create namespaces, mount filesystems, manage
cgroups), making it a higher-value target than the SSH server in Option A.

**Summary:** Weakest security posture of the five. The combination of
unauthenticated-by-default gRPC + a privileged custom execution API + multiple new
attack surfaces makes this the highest-risk design. If pursued, mTLS mandatory from
day one + strict handler input validation + a dedicated security audit are
non-negotiable prerequisites — substantially increasing implementation cost beyond what
the latency improvement justifies. **The 150ms SSH overhead in Option A is the correct
price of not building a Docker daemon.**

---

### Option D — Homebrew formula + Lima template

**Runtime security:** Identical to Option A — SSH keypair, Lima hostagent socket, no
network exposure.

**Supply chain** — the one dimension where D is strictly worse than A. A Homebrew tap
formula is a Ruby file fetched from a GitHub repository. Its integrity depends on HTTPS
+ SHA256 checksums for downloaded artifacts (present) but there is no GPG signing of
Homebrew formulas and no Gatekeeper validation of installed binaries. A compromised tap
repository can ship a formula installing a backdoored pelagos binary. The signed,
notarized `.pkg` in Option A is validated by macOS Gatekeeper before installation — a
meaningful defence-in-depth layer absent in D.

**Summary:** Acceptable for a developer/community tool where the user understands
Homebrew's trust model. Insufficient for enterprise distribution where MDM policies
rely on signed installers. Gatekeeper's absence is the meaningful delta from Option A.

---

### Option E — Apple Containerization (VM-per-container)

**Authentication:** Controlled by the macOS session ownership model. No socket exposed
to other users by default. The `com.apple.security.hypervisor` entitlement required to
call the framework is gating at the distribution level.

**Privilege on compromise:** VM-per-container means a successful container escape
reaches only that container's VM — not a shared Linux environment, not other
containers. An AVF hypervisor vulnerability is required to reach the macOS host. This
is qualitatively stronger isolation than any namespace-based option.

**Container escape:** AVF hypervisor boundary per container. A namespace escape inside
the VM does not yield access to other containers or the host. Historically rare; Apple
has strong incentive to fix hypervisor bugs quickly.

**Framework immaturity:** The flip side of OpenSSH's 25 years of audits — the Apple
`Containerization` framework is 0.x with no published security audit and no production
track record. Early framework versions frequently have significant vulnerabilities.

**Summary:** Best isolation model by architecture (hypervisor boundary per container).
Highest unknown risk from framework immaturity. The entitlement model provides
meaningful distribution gating. Not evaluable until macOS 26 ships and accumulates a
security track record.

---

## Summary Tables

### Performance and Engineering

| | A (Lima SSH) | B (vfkit+Rust) | C (Lima+gRPC) | D (Homebrew) | E (Apple) |
|---|---|---|---|---|---|
| Effort | Moderate | Significant | Significant | Small | Very high |
| IPC latency | ~150ms/cmd | ~5ms/cmd | ~5ms/cmd | ~150ms/cmd | ~5ms/cmd |
| Go dependency | Lima binary | vfkit binary only | Lima binary | Lima binary | No |
| File sharing | Auto (virtiofs) | Must build | Auto (virtiofs) | Auto (virtiofs) | Auto (AVF) |
| Docker socket compat | Add later | Buildable | Natural | No | No |
| macOS version req | 13.5+ | 13.5+ | 13.5+ | 13.5+ | **26+** |
| License | Apache 2.0 | Apache 2.0 | Apache 2.0 | Apache 2.0 | Apache 2.0 |
| Polish/completeness | High | High | Highest | Low | Blocked |

### Security

| | Authn model | Compromise impact | Custom attack surface | Container escape barrier | Supply chain |
|---|---|---|---|---|---|
| A (Lima SSH) | OpenSSH keypair | VM + bounded virtiofs scope | None (OpenSSH) | Namespace + AVF | Signed + notarized |
| B (vfkit+vsock) | Unix socket ownership | Docker-socket equivalent | Handler input bugs | Namespace + AVF | Signed + vfkit binary |
| C (Lima+gRPC) | **None by default** | **Docker-socket + handler injection** | **gRPC handlers (custom code)** | Namespace + AVF | Signed |
| D (Homebrew) | OpenSSH keypair | VM + bounded virtiofs scope | None (OpenSSH) | Namespace + AVF | **No Gatekeeper validation** |
| E (Apple) | macOS session | Bounded by AVF hypervisor | Apple framework (unaudited 0.x) | **AVF per container (strongest)** | Entitlement-gated |

---

## Revised Architecture: Pure-Rust AVF Bindings

*Added 2026-03-09 after deeper research into the AVF ecosystem.*

### The subsystem dependency problem

There is a principled distinction between **library dependencies** (subordinate — they
do what you tell them under the contract you define) and **subsystem dependencies**
(Lima, Docker daemon — they have their own lifecycle, conventions, and release cadence;
you build *around* them). Pelagos should have no subsystem-sized external dependencies.
This is what separates a product from an integration.

This principle eliminates Lima as a long-term substrate — not because Lima is bad, but
because it would make pelagos a Lima plugin rather than a product.

### objc2-virtualization changes the calculus

Research reveals that `objc2-virtualization` — a maintained, auto-generated Rust crate
binding the entire `Virtualization.framework` — already exists. It is part of the
`objc2` ecosystem (4,400+ commits, 59k dependents, updated weekly from Xcode SDK
headers). This is not a weekend project; it is production infrastructure.

This means a pure-Rust analog to vfkit is buildable today, without Go, without Lima,
and without writing raw Objective-C FFI by hand:

```
pelagos-mac (Rust)
  └── objc2-virtualization    ← replaces vfkit; library dep, not subsystem
  └── virtiofsd               ← already Rust (Red Hat)
  └── vsock via UnixStream    ← standard library
```

No Go binary at any layer. No subsystem you don't control.

### AVF documentation quality

Apple's `Virtualization.framework` documentation is solid for a proprietary framework:
- Comprehensive Apple developer docs with a dedicated updates page
- Two substantial WWDC sessions: [2022](https://developer.apple.com/videos/play/wwdc2022/10002/)
  and [2023](https://developer.apple.com/videos/play/wwdc2023/10007/)
- The Go `Code-Hex/vz` bindings serve as an annotated reference — they split
  implementation across `virtualization_11.m`, `virtualization_13.m`,
  `virtualization_15.m`, making per-macOS-version API changes explicit and readable

Known gaps: no public C API (Objective-C/Swift only), some GPU and USB functionality
undocumented, private entitlement required for extended features. None of these affect
the Linux VM use case.

**Feature availability by macOS version:**

| macOS | Key additions relevant to pelagos |
|---|---|
| 11 (Big Sur) | Framework introduced; Linux VM support |
| 12 (Monterey) | VirtIO networking, storage, vsock, entropy |
| 13 (Ventura) | EFI boot, virtiofs (folder sharing), Rosetta for x86_64 Linux, VirtioGPU |
| 14 (Sonoma) | NVMe controller, remote storage |
| 15 (Sequoia) | USB device support (limited) |

macOS 13.5+ covers everything pelagos needs. macOS 11/12 would require dropping virtiofs
(fall back to SFTP or NFS) and Rosetta — a reasonable trade-off to defer.

### The pilot project

The pilot validates the architecture concretely before committing to a full
implementation. It is not a throwaway — the pilot *is* the production component.

**`pelagos-vz`** — a thin ergonomic Rust crate over `objc2-virtualization`:
- Boot a Linux VM from a kernel + initrd + disk image
- Configure vsock (exposed as Unix socket on host), virtiofs, NAT networking, Rosetta
- Manage VM lifecycle: start, stop, clean shutdown
- ~500–800 lines; use `Code-Hex/vz` Go bindings as design reference for the API shape

**`pelagos-guest`** — a minimal Rust daemon that runs inside the VM as a startup service:
- Listens on `AF_VSOCK` port N
- Receives JSON command envelopes: `{"cmd": "run", "image": "alpine", "args": [...]}`
- Forks `pelagos` with the given arguments
- Streams stdout/stderr back over vsock, returns exit code
- ~200 lines

**What the pilot proves:**
1. `objc2-virtualization` is usable for booting real Linux VMs
2. vsock IPC works end-to-end from a Rust host to a Rust guest
3. virtiofs file sharing (host directory → container bind mount) is functional
4. The entire stack compiles and runs without any Go binary

**What it does not need:** port forwarding, Rosetta, installer packaging, multi-VM
management. Those come after the architecture is validated.

### Caveats

- `objc2-virtualization` is auto-generated: complete but not ergonomic. `pelagos-vz`
  wraps it with a curated API rather than exposing raw ObjC bindings.
- Code signing is required: the `com.apple.security.virtualization` entitlement must
  be present. This is true of all AVF consumers including vfkit.
- macOS 13.5+ minimum for full feature set. A macOS 12 fallback (drop virtiofs +
  Rosetta) is possible but not a priority.

---

## Recommendation

Security analysis reverses any suggestion to evolve toward Option C.

**The target architecture is now: pure-Rust AVF bindings (`pelagos-vz`) + virtiofsd +
vsock IPC.** The discovery of `objc2-virtualization` eliminates the Go binary dependency
and the Lima subsystem dependency simultaneously. Options A–D were evaluated under the
assumption that a pure-Rust AVF path required writing raw ObjC FFI from scratch. That
assumption is false.

**Option A (Lima/SSH)** remains a valid fast-path if the pilot reveals `objc2-virtualization`
to be immature in practice. SSH authentication is architecturally correct; the Lima
subsystem dependency is the only reason not to use it permanently.

**Option B (vfkit + Rust orchestrator)** is superseded by the pure-Rust path.
vfkit's only advantage over Lima was thinner subsystem coupling; `pelagos-vz` eliminates
the dependency entirely rather than reducing it.

**Option C** should not be the target architecture. The gRPC daemon is the Docker
daemon problem rebuilt from scratch. If a persistent socket is ever needed (Docker
socket compatibility, VS Code Dev Containers), the correct scope is a minimal
read-mostly status socket — not a full execution API — with mTLS mandatory from day
one.

**Option D** is appropriate for community/early-access distribution during development.
Insufficient for enterprise.

**Option E** is a 2027 watch item. Track the macOS 26 release and accumulation of
security track record for the `Containerization` framework.

**Immediate next step:** build the `pelagos-vz` pilot as described above. If it boots a
Linux VM and round-trips a vsock command in under 500 lines of Rust, the architecture
is validated and Options A–B become fallbacks rather than the plan.
