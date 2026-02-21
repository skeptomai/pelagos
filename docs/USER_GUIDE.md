# Remora User Guide

Remora is a lightweight Linux container runtime with a CLI similar to Podman or nerdctl,
plus a Rust library API for embedding container isolation into your own programs.

---

## Installation & Setup

### Install

```bash
git clone https://github.com/skeptomai/remora.git
cd remora

# Option A: Install to /usr/local/bin (recommended)
scripts/install.sh

# Option B: Install to ~/.cargo/bin
cargo install --path .

# Option C: Install to /usr/local/bin via cargo
sudo cargo install --path . --root /usr/local
```

You can also download a pre-built binary from the
[Releases](https://github.com/skeptomai/remora/releases) page and copy it
to a directory on your PATH.

Verify the installation:

```bash
remora --help
```

### Pull an Image

```bash
# Rootless (images stored in ~/.local/share/remora/)
remora image pull alpine

# Or as root (images stored in /var/lib/remora/)
sudo remora image pull alpine

remora image ls
```

---

## Quick Start

### Rootless (no sudo)

```bash
# Pull and run — no root required
remora image pull alpine
remora run alpine /bin/echo hello

# Interactive shell with internet (Ctrl-D to exit)
remora run -i --network pasta alpine /bin/sh

# Check running containers
remora ps
```

### Root (full feature set)

```bash
# Run a one-shot command
sudo remora run alpine /bin/echo hello

# Interactive shell (Ctrl-D to exit)
sudo remora run -i alpine /bin/sh

# Detached (background) container
sudo remora run -d --name mybox alpine \
  /bin/sh -c 'while true; do echo tick; sleep 1; done'

# Check running containers
remora ps

# View logs
remora logs mybox
remora logs -f mybox         # follow (like tail -f)

# Stop and remove
sudo remora stop mybox
remora rm mybox
```

---

## OCI Images

Remora can pull images directly from OCI registries (Docker Hub, etc.) using anonymous
authentication. Image pull works both as root and rootless.

```bash
# Pull an image (rootless or root)
remora image pull alpine
remora image pull alpine:3.19
remora image pull library/ubuntu:latest

# List local images
remora image ls

# Run a container from an image
remora run alpine /bin/sh           # rootless
sudo remora run -i alpine /bin/sh   # root

# Remove a local image
remora image rm alpine
```

Remora automatically sets up a multi-layer overlayfs mount with an ephemeral upper/work
directory (writes are discarded when the container exits). Image config (Env, Cmd,
Entrypoint, WorkingDir) is applied as defaults that CLI flags override.

**Storage locations (root):**
- `/var/lib/remora/images/` -- image manifests and config
- `/var/lib/remora/layers/<sha256>/` -- content-addressable layer cache

**Storage locations (rootless):**
- `~/.local/share/remora/images/` -- image manifests and config
- `~/.local/share/remora/layers/<sha256>/` -- content-addressable layer cache

Root and rootless image stores are separate (matching Podman behavior). An image pulled
as root is not visible rootless, and vice versa.

---

## Building Images

Remora can build custom images from a **Remfile** (simplified Dockerfile). The build
process is daemonless (Buildah-style) — each `RUN` instruction spawns a container,
snapshots the overlay upper directory as a new layer, and stores it in the layer cache.

### Remfile Reference

```
FROM alpine:latest
RUN apk add --no-cache curl nginx
COPY index.html /var/www/index.html
ENV APP_PORT=8080
WORKDIR /var/www
CMD ["nginx", "-g", "daemon off;"]
EXPOSE 8080
```

| Instruction | Syntax | Effect |
|-------------|--------|--------|
| `FROM` | `FROM <image>[:<tag>]` | Load base image (must be pulled locally first) |
| `RUN` | `RUN <command>` | Execute in a container; filesystem changes become a new layer |
| `COPY` | `COPY <src> <dest>` | Copy file/directory from build context into image as a new layer |
| `CMD` | `CMD ["arg1", "arg2"]` or `CMD command args` | Set default command (JSON array or shell form) |
| `ENV` | `ENV KEY=VALUE` or `ENV KEY VALUE` | Set an environment variable |
| `WORKDIR` | `WORKDIR /path` | Set working directory for subsequent RUN and the final image |
| `EXPOSE` | `EXPOSE <port>[/protocol]` | Documentation only (no runtime effect) |

Comments (`#`) and blank lines are ignored. Continuation lines (trailing `\`) are supported.

### Building

```bash
# Build from Remfile in current directory
sudo remora build -t myapp:latest .

# Build from a specific Remfile
sudo remora build -t myapp:latest -f path/to/Remfile .

# Specify network mode for RUN steps (default: bridge for root, pasta for rootless)
sudo remora build -t myapp:latest --network bridge .

# Rootless build (uses pasta networking for RUN steps)
remora build -t myapp:latest .
```

The base image must be pulled locally first:

```bash
remora image pull alpine
remora build -t myapp:latest .
```

### Running Built Images

Built images are stored in the same image store as pulled images and can be run
with `remora run`:

```bash
sudo remora build -t myapp:latest .
sudo remora run myapp:latest
sudo remora run -i myapp:latest /bin/sh   # override CMD with interactive shell
```

### Build Example

```bash
# Create a build context
mkdir myapp && cd myapp
echo '<h1>Hello from Remora</h1>' > index.html

cat > Remfile <<'EOF'
FROM alpine:latest
RUN apk add --no-cache curl
COPY index.html /srv/index.html
ENV GREETING=hello
WORKDIR /srv
CMD ["/bin/sh", "-c", "cat index.html && echo $GREETING"]
EOF

# Pull base image, build, and run
sudo remora image pull alpine
sudo remora build -t myapp:latest .
sudo remora run myapp:latest
```

### `build` Reference

```
remora build [OPTIONS] [CONTEXT]
```

| Flag | Short | Description |
|------|-------|-------------|
| `--tag <TAG>` | `-t` | Image tag (required), e.g. `myapp:latest` |
| `--file <PATH>` | `-f` | Path to Remfile (default: `<context>/Remfile`) |
| `--network <MODE>` | | Network for RUN steps: `bridge`, `pasta`, `none`, `auto` (default) |
| `CONTEXT` | | Build context directory (default: `.`) |

`--network auto` selects `bridge` when running as root, `pasta` when rootless.

---

## Container Lifecycle

```bash
# List running containers
remora ps

# List all containers (including exited)
remora ps --all

# View logs (detached containers)
remora logs <name>
remora logs --follow <name>

# Stop a container (sends SIGTERM)
sudo remora stop <name>

# Remove a stopped container
remora rm <name>

# Force remove (SIGKILL + remove even if running)
remora rm --force <name>
```

---

## Exec Into a Running Container

Run a command inside an already-running container by joining its namespaces:

```bash
# Run a command
remora exec mybox /bin/ls /

# Interactive shell
remora exec -i mybox /bin/sh

# With environment variables
remora exec -e FOO=bar mybox /bin/env

# With a specific working directory
remora exec -w /tmp mybox /bin/pwd

# As a specific user
remora exec -u 1000:1000 mybox /bin/id
```

`exec` discovers the container's namespaces from `/proc/<pid>/ns/*` and joins them with
`setns()`. The container's environment is inherited from `/proc/<pid>/environ`, with
`-e` flags as overrides.

---

## Networking

By default, containers have no network (`--network none`).

### Network Modes

```bash
# No network (default)
sudo remora run alpine /bin/sh

# Loopback only (127.0.0.1, no external access)
sudo remora run --network loopback alpine /bin/sh

# Bridge networking (veth pair + remora0 bridge, 172.19.0.x/24)
sudo remora run --network bridge --nat alpine /bin/sh

# Pasta (rootless, full internet access via user-mode networking)
remora run --network pasta alpine /bin/sh    # no sudo needed
```

### NAT, Port Forwarding, and DNS

```bash
# Enable outbound internet (MASQUERADE via nftables)
sudo remora run --network bridge --nat alpine /bin/sh

# Publish ports (host:container TCP forwarding)
sudo remora run --network bridge --nat -p 8080:80 alpine /bin/sh

# Custom DNS servers
sudo remora run --network bridge --nat --dns 1.1.1.1 --dns 8.8.8.8 alpine /bin/sh
```

### Container Linking

```bash
# Link containers by name (injects /etc/hosts entry)
sudo remora run -d --name db --network bridge --nat alpine /bin/sh -c 'sleep 3600'
sudo remora run --network bridge --nat --link db alpine /bin/sh -c 'ping -c1 db'
```

### Rootless Networking

For rootless containers (no sudo), use `--network pasta`. This requires
[pasta](https://passt.top) (from the passt project) to be installed.

```bash
# Full internet access without root
remora run --network pasta -i alpine /bin/sh
```

Bridge networking requires root and is rejected in rootless mode.

---

## Storage

### Named Volumes

Volumes persist data across container runs. They are stored in the data directory
(root: `/var/lib/remora/volumes/`, rootless: `~/.local/share/remora/volumes/`).

```bash
# Create a volume (works rootless)
remora volume create mydata

# List volumes
remora volume ls

# Use a volume (auto-created if it doesn't exist)
remora run -v mydata:/data alpine /bin/sh

# Remove a volume
remora volume rm mydata
```

### Bind Mounts

Map host directories into the container:

```bash
# Read-write bind mount
sudo remora run --bind /host/path:/container/path alpine /bin/sh

# Read-only bind mount
sudo remora run --bind-ro /etc/hosts:/etc/hosts alpine /bin/sh
```

### tmpfs

In-memory writable directories (useful with `--read-only`):

```bash
sudo remora run --read-only --tmpfs /tmp --tmpfs /run alpine /bin/sh
```

### Overlay (with OCI images)

When using OCI images, Remora automatically creates a multi-layer overlayfs mount. The
base image layers are read-only; writes go to an ephemeral upper directory that is
discarded when the container exits.

In rootless mode, Remora uses the `userxattr` mount option (kernel 5.11+) or falls back
to `fuse-overlayfs` automatically. See [Rootless Overlay](#rootless-overlay) for details.

---

## Security & Isolation

### Read-Only Rootfs

```bash
sudo remora run --read-only alpine /bin/sh
# Combine with --tmpfs for writable scratch space
sudo remora run --read-only --tmpfs /tmp alpine /bin/sh
```

### Capabilities

```bash
# Drop all capabilities (most restrictive)
sudo remora run --cap-drop ALL alpine /bin/sh

# Drop all, then add back specific ones
sudo remora run --cap-drop ALL --cap-add NET_BIND_SERVICE alpine /bin/sh
```

Supported capabilities: CHOWN, DAC_OVERRIDE, FOWNER, SETGID, SETUID,
NET_BIND_SERVICE, NET_RAW, SYS_CHROOT, SYS_ADMIN, SYS_PTRACE.

### Seccomp Profiles

```bash
# Docker's default seccomp profile (recommended)
sudo remora run --security-opt seccomp=default alpine /bin/sh

# Minimal profile (tighter restrictions)
sudo remora run --security-opt seccomp=minimal alpine /bin/sh

# Disable seccomp entirely
sudo remora run --security-opt seccomp=none alpine /bin/sh
```

### Other Security Options

```bash
# Prevent privilege escalation via setuid/setgid binaries
sudo remora run --security-opt no-new-privileges alpine /bin/sh

# Mask sensitive kernel paths
sudo remora run --masked-path /proc/kcore --masked-path /proc/sysrq-trigger alpine /bin/sh

# Set kernel parameters inside container
sudo remora run --sysctl net.ipv4.ip_forward=1 alpine /bin/sh

# Run as non-root user inside container
sudo remora run --user 1000:1000 alpine /bin/id
```

---

## Resource Limits

### Cgroups v2

```bash
# Memory limit (supports k, m, g suffixes)
sudo remora run --memory 256m alpine /bin/sh

# CPU quota (fractional CPUs: 0.5 = 50% of one core)
sudo remora run --cpus 0.5 alpine /bin/sh

# CPU shares/weight (relative to other containers)
sudo remora run --cpu-shares 512 alpine /bin/sh

# Max number of processes
sudo remora run --pids-limit 50 alpine /bin/sh
```

### rlimits

```bash
# Set file descriptor limit
sudo remora run --ulimit nofile=1024:2048 alpine /bin/sh

# Set max processes
sudo remora run --ulimit nproc=100:200 alpine /bin/sh
```

Supported ulimit resources: nofile (openfiles), nproc (maxproc), as (vmem), cpu,
fsize, memlock, stack, core, rss, msgqueue, nice, rtprio.

---

## Rootless Mode

Remora auto-detects rootless mode when run without sudo (`getuid() != 0`). No flag
needed — just omit `sudo`.

```bash
# Pull and run — fully rootless
remora image pull alpine
remora run alpine /bin/echo hello

# Interactive shell with internet
remora run -i --network pasta alpine /bin/sh
```

**What works rootless:**
- Image pull (`remora image pull`)
- Image run with overlay filesystem (kernel 5.11+ native, or `fuse-overlayfs` fallback)
- Named volumes (`-v mydata:/data`)
- Loopback networking (`--network loopback`)
- Pasta networking (`--network pasta`) -- full internet access
- User namespace isolation (auto-configured: container UID 0 maps to your host UID)

**What requires root:**
- Bridge networking (`--network bridge`)
- NAT and port mapping (`--nat`, `-p`)
- Cgroups (skipped gracefully in rootless mode)

### Rootless Overlay

Remora uses overlayfs with the `userxattr` mount option on kernel 5.11+. This stores
whiteout metadata in `user.*` extended attributes instead of `trusted.*`, which doesn't
require `CAP_SYS_ADMIN`.

On older kernels, Remora automatically falls back to
[fuse-overlayfs](https://github.com/containers/fuse-overlayfs). If neither works, you'll
see a clear error with instructions.

```bash
# Install fuse-overlayfs (only needed for kernels < 5.11)
# Arch Linux
pacman -S fuse-overlayfs
# Debian/Ubuntu
apt install fuse-overlayfs
# Fedora
dnf install fuse-overlayfs
```

### Rootless Storage

Rootless mode uses XDG Base Directory paths:

| Purpose | Root path | Rootless path |
|---------|-----------|---------------|
| Images & layers | `/var/lib/remora/` | `~/.local/share/remora/` |
| Volumes | `/var/lib/remora/volumes/` | `~/.local/share/remora/volumes/` |
| Runtime state | `/run/remora/` | `$XDG_RUNTIME_DIR/remora/` |

Root and rootless stores are completely separate (same as Podman). An image pulled as
root is not available in rootless mode, and vice versa.

---

## Complete `run` Reference

```
remora run [OPTIONS] <IMAGE> [COMMAND [ARGS...]]
remora run [OPTIONS] --rootfs <ROOTFS> [COMMAND [ARGS...]]
```

| Flag | Short | Description |
|------|-------|-------------|
| `--name <NAME>` | | Container name (auto-generated if omitted) |
| `--detach` | `-d` | Run in background |
| `--interactive` | `-i` | Allocate a PTY (incompatible with `--detach`) |
| `--rootfs <ROOTFS>` | | Use a local rootfs instead of an OCI image (advanced) |
| `--network <MODE>` | | `none` (default), `loopback`, `bridge`, `pasta` |
| `--publish <H:C>` | `-p` | TCP port forward host:container (repeatable) |
| `--nat` | | Enable MASQUERADE NAT (requires bridge) |
| `--dns <IP>` | | DNS server (repeatable) |
| `--link <NAME[:ALIAS]>` | | Link to another container |
| `--volume <V:/PATH>` | `-v` | Named volume (auto-created) |
| `--bind <H:C>` | | RW bind mount host:container (repeatable) |
| `--bind-ro <H:C>` | | RO bind mount (repeatable) |
| `--tmpfs <PATH>` | | tmpfs mount (repeatable) |
| `--read-only` | | Make rootfs read-only |
| `--env <K=V>` | `-e` | Environment variable (repeatable) |
| `--env-file <PATH>` | | Load environment from file |
| `--workdir <PATH>` | `-w` | Working directory inside container |
| `--user <UID[:GID]>` | `-u` | User/group to run as |
| `--hostname <NAME>` | | Container hostname |
| `--memory <LIMIT>` | | Memory limit (e.g. `256m`, `1g`) |
| `--cpus <FRAC>` | | CPU quota as fraction (e.g. `0.5`) |
| `--cpu-shares <N>` | | CPU weight (relative) |
| `--pids-limit <N>` | | Max number of processes |
| `--ulimit <R=S:H>` | | rlimit (e.g. `nofile=1024:2048`) (repeatable) |
| `--cap-drop <CAP>` | | Drop capability (repeatable, or `ALL`) |
| `--cap-add <CAP>` | | Add capability (repeatable) |
| `--security-opt <OPT>` | | `seccomp=default\|minimal\|none`, `no-new-privileges` |
| `--sysctl <K=V>` | | Kernel parameter (repeatable) |
| `--masked-path <PATH>` | | Path to mask inside container (repeatable) |

---

## OCI Runtime Interface

For integration with container managers like containerd or CRI-O, Remora implements the
OCI Runtime Spec lifecycle commands. These operate on OCI bundles (a directory with
`config.json` and `rootfs/`).

```bash
# Create a container (fork shim, pause before exec)
remora create mycontainer /path/to/bundle

# Start it (signal shim to exec the process)
remora start mycontainer

# Query state (JSON output)
remora state mycontainer
# {"ociVersion":"1.0.2","id":"mycontainer","status":"running","pid":12345,...}

# Send a signal
remora kill mycontainer SIGTERM

# Clean up state directory
remora delete mycontainer
```

State is stored under `/run/remora/<id>/`. The shim double-forks so `remora create`
returns as soon as the container is in the "created" state.

---

## Rust Library API

For developers embedding Remora as a library in Rust programs.

### The Command Builder

```rust
use remora::container::{Command, Namespace, Stdio};

let mut child = Command::new("/bin/sh")
    .with_chroot("/path/to/rootfs")
    .with_namespaces(Namespace::UTS | Namespace::MOUNT | Namespace::PID)
    .with_proc_mount()
    .stdout(Stdio::Piped)
    .spawn()?;

let status = child.wait()?;
```

`spawn()` forks, runs `pre_exec` setup in the child (namespaces, mounts, capabilities,
seccomp), then execs the target binary. It returns a `Child` handle whose `wait()` and
`wait_with_output()` methods block until the container exits and clean up all resources.

### Interactive Shell

```rust
let session = Command::new("/bin/sh")
    .with_chroot("/path/to/rootfs")
    .with_namespaces(Namespace::UTS | Namespace::MOUNT)
    .with_proc_mount()
    .spawn_interactive()?;

// Blocks: relays stdin/stdout, forwards SIGWINCH, restores terminal on exit
let status = session.run()?;
```

### CLI-to-API Translation

| CLI flag | Rust API equivalent |
|---|---|
| `-i` | `spawn_interactive()?.run()` |
| `--rm` | Automatic -- `wait()` tears everything down |
| `--network none` | Default (no `.with_network()` call) |
| `--network bridge` | `.with_network(NetworkMode::Bridge)` |
| `--network pasta` | `.with_network(NetworkMode::Pasta)` |
| `-p 8080:80` | `.with_port_forward(8080, 80)` |
| `--dns 1.1.1.1` | `.with_dns(&["1.1.1.1"])` |
| `--bind /host:/ctr` | `.with_bind_mount("/host", "/ctr")` |
| `--bind-ro /host:/ctr` | `.with_bind_mount_ro("/host", "/ctr")` |
| `--tmpfs /run` | `.with_tmpfs("/run")` |
| `-v mydata:/data` | `.with_volume(&vol, "/data")` |
| `--read-only` | `.with_readonly_rootfs(true)` |
| `--memory 256m` | `.with_cgroup_memory(256 * 1024 * 1024)` |
| `--cpus 0.5` | `.with_cgroup_cpu_quota(50_000, 100_000)` |
| `--pids-limit 50` | `.with_cgroup_pids_limit(50)` |
| `--cpu-shares 512` | `.with_cgroup_cpu_shares(512)` |
| `--cap-drop ALL` | `.drop_all_capabilities()` |
| `--cap-add NET_ADMIN` | `.with_capabilities(&[Capability::NetAdmin])` |
| `--security-opt seccomp=default` | `.with_seccomp_default()` |
| `--security-opt no-new-privileges` | `.with_no_new_privileges(true)` |
| `--user 1000:1000` | `.with_uid(1000).with_gid(1000)` |
| `-e FOO=bar` | `.env("FOO", "bar")` |
| `-w /app` | `.with_cwd("/app")` |
| `--ulimit nofile=1024` | `.with_rlimit(Resource::NOFILE, 1024, 1024)` |
| `--hostname myhostname` | `.with_hostname("myhostname")` |
| `--masked-path /proc/kcore` | `.with_masked_paths(&["/proc/kcore"])` |
| `--sysctl net.ipv4.ip_forward=1` | `.with_sysctl("net.ipv4.ip_forward", "1")` |
| `alpine` (positional) | `.with_image_layers(layer_dirs)` |

---

## Advanced: Local Rootfs

For development or testing, you can bypass OCI images and use a local rootfs directory
directly. This is mainly useful for Remora contributors and custom rootfs builds.

```bash
# Build a rootfs from Docker:
scripts/build-rootfs-docker.sh       # requires Docker + sudo
# or from an Alpine tarball:
scripts/build-rootfs-tarball.sh      # requires sudo

# Register it with Remora:
sudo remora rootfs import alpine ./alpine-rootfs

# List registered rootfs entries:
remora rootfs ls

# Run with a local rootfs (advanced):
sudo remora run --rootfs alpine /bin/echo hello

# Remove a rootfs entry:
sudo remora rootfs rm alpine
```

See `docs/BUILD_ROOTFS.md` for detailed rootfs build instructions.

---

## Storage Layout

### Root (`sudo remora ...`)

```
/var/lib/remora/
  rootfs/<name>              symlink to imported rootfs directory
  volumes/<name>/            named volume data
  images/<ref>/              OCI image manifests and config
  layers/<sha256>/           content-addressable layer cache
  container_counter          monotonic counter for auto-naming

/run/remora/
  containers/<name>/
    state.json               container metadata and status
    stdout.log               captured stdout (detached mode)
    stderr.log               captured stderr (detached mode)
  <oci-id>/
    state.json               OCI lifecycle state
    exec.sock                sync socket (create/start handshake)
  next_ip                    IPAM counter for bridge networking
  nat_refcount               reference count for nftables NAT rules
  dns-<pid>-<n>/             per-container resolv.conf
```

### Rootless (`remora ...`)

```
~/.local/share/remora/       ($XDG_DATA_HOME/remora/)
  rootfs/<name>              symlink to imported rootfs directory
  volumes/<name>/            named volume data
  images/<ref>/              OCI image manifests and config
  layers/<sha256>/           content-addressable layer cache
  container_counter          monotonic counter for auto-naming

$XDG_RUNTIME_DIR/remora/     (fallback: /tmp/remora-$UID/)
  containers/<name>/
    state.json               container metadata and status
    stdout.log               captured stdout (detached mode)
    stderr.log               captured stderr (detached mode)
  dns-<pid>-<n>/             per-container resolv.conf
```

---

## Testing

Remora has several layers of tests. Unit tests and lint run without root; everything
else requires `sudo -E` to preserve the rustup/cargo environment.

### 1. Unit Tests + Lint (no root)

```bash
cargo test --lib
cargo clippy -- -D warnings
cargo fmt -- --check
```

53 unit tests covering parsers, builders, seccomp filter compilation, cgroup path
parsing, image manifest handling, and namespace flags.

### 2. Integration Tests (root)

```bash
sudo -E cargo test --test integration_tests
```

72 integration tests that exercise the full container lifecycle: spawn, namespaces,
mounts, seccomp, capabilities, cgroups, networking, overlay, exec, OCI lifecycle,
and rootless mode. Requires an Alpine rootfs (pulled automatically or via
`scripts/build-rootfs-docker.sh`).

### 3. E2E Test Suite (root)

```bash
sudo -E ./scripts/test-e2e.sh
```

End-to-end CLI tests covering `remora run`, `ps`, `stop`, `rm`, `logs`, `exec`,
detached mode, environment variables, bind mounts, volumes, bridge networking,
port forwarding, DNS, container linking, and security options. Builds the binary
and runs real containers.

### 4. Build E2E (root)

```bash
sudo -E ./scripts/test-build.sh
```

Tests `remora build` end-to-end: Remfile parsing, RUN steps with networking,
COPY, ENV, WORKDIR, CMD (JSON and shell form), multi-step builds, and image
tagging. Verifies that built images can be run.

### 5. Stress Tests (root)

```bash
sudo -E ./scripts/test-stress.sh
```

18 stress tests across 7 categories: rapid lifecycle, parallel containers,
resource-constrained containers, overlay filesystem stress, network stress,
OCI lifecycle stress, and edge cases.

### 6. Web Stack Example (root)

```bash
cargo build --release
sudo PATH=$PWD/target/release:$PATH ./examples/web-stack/run.sh
```

Builds and runs a 3-container blog stack (nginx → Bottle API → Redis) on bridge
networking with container linking. Runs 5 HTTP verification tests. Unlike the
other scripts, this one doesn't call `cargo build` internally — it expects
`remora` in your PATH, so pass the release binary path via `PATH` instead of `-E`.

### Full Pre-Release Checklist

```bash
# 1. Lint + unit tests (no root)
cargo test --lib && cargo clippy -- -D warnings && cargo fmt -- --check

# 2. Integration tests (root)
sudo -E cargo test --test integration_tests

# 3. E2E suites (root)
sudo -E ./scripts/test-e2e.sh
sudo -E ./scripts/test-build.sh
sudo -E ./scripts/test-stress.sh

# 4. Example app (root, release build)
cargo build --release
sudo PATH=$PWD/target/release:$PATH ./examples/web-stack/run.sh
```

---

## Troubleshooting

### "rootfs not found"

Pull an OCI image first:

```bash
remora image pull alpine
remora run alpine /bin/sh
```

Or if using a local rootfs, import it:

```bash
scripts/build-rootfs-docker.sh
sudo remora rootfs import alpine ./alpine-rootfs
sudo remora run --rootfs alpine /bin/sh
```

### Permission denied / EPERM

Try rootless mode first (no sudo needed for image pull, run, volumes). If a specific
feature requires root (bridge networking, cgroups), run with `sudo`.

### Rootless overlay fails

If you see an error about overlay mount failing:
- **Kernel 5.11+:** native overlay with `userxattr` should work automatically
- **Older kernels:** install `fuse-overlayfs` (see [Rootless Overlay](#rootless-overlay))
- Check that your filesystem supports user xattrs (`tmpfs`, `ext4`, `btrfs` all do)

### Integration tests fail

Tests require root and an Alpine rootfs:

```bash
sudo -E cargo test --test integration_tests
```

### Bridge networking: "No such device" or "Operation not permitted"

Bridge mode requires root, `ip` (iproute2), and `nft` (nftables). Ensure both are
installed and you're running as root.

### Pasta not found

Install [passt](https://passt.top) for rootless networking:

```bash
# Arch Linux
pacman -S passt
# Debian/Ubuntu
apt install passt
# Fedora
dnf install passt
```

### Container exec fails with "no container namespaces found"

The target container must be running. Check with `remora ps`.
