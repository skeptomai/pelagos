# Remora User Guide

**For users familiar with Podman, nerdctl, or containerd.**

---

## What Remora Is (and Isn't)

Remora is a **container runtime library** for Rust. It creates and manages containerised
processes using the same Linux primitives as Docker and Podman underneath — namespaces,
seccomp, capabilities, cgroups — but exposes them as a direct Rust API rather than a
container management daemon.

| | Podman / nerdctl | Remora |
|---|---|---|
| Primary interface | CLI (`podman run ...`) | Rust builder API |
| Image management | Pull, push, layers, registry | **None** — you provide a rootfs directory |
| Daemon | `podman` is daemonless; nerdctl talks to containerd | No daemon |
| OCI lifecycle CLI | `podman create/start/kill/rm` | `remora create/start/state/kill/delete` |
| Rootless | Yes | Yes (auto-detected) |
| Networking | CNI plugins | Native (loopback, bridge/NAT, pasta) |

The short version: Remora is for **embedding container isolation directly into a Rust
program**, not for managing containers from a terminal. If you need `podman run` semantics
from the command line, use Podman. If you need to spawn isolated child processes with
precise control over resources and security — from inside a Rust binary — Remora is the
right fit.

---

## Preparing a Root Filesystem

Remora does not pull images. You provide a rootfs directory — the same unpacked filesystem
that sits inside any container image.

```bash
# Export Alpine from Docker (one-time setup)
mkdir alpine-rootfs
docker export $(docker create alpine) | tar -xC alpine-rootfs/
```

Point every `with_chroot()` call at that directory.

---

## The Command Builder

Everything starts with `remora::container::Command`. It mirrors `std::process::Command`
but adds container-specific methods:

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

---

## Flag Translation

| Podman / nerdctl flag | Remora equivalent |
|---|---|
| `-it` | `spawn_interactive()?.run()` |
| `--rm` | Automatic — `wait()` tears everything down |
| `--network none` | Default (no `.with_network()` call) |
| `--network bridge` | `.with_network(NetworkMode::Bridge)` |
| `--network slirp4netns` | `.with_network(NetworkMode::Pasta)` (rootless) |
| `-p 8080:80` | `.with_port_forward(8080, 80)` |
| `--dns 1.1.1.1` | `.with_dns(&["1.1.1.1"])` |
| `-v /host:/ctr` | `.with_bind_mount("/host", "/ctr")` |
| `-v /host:/ctr:ro` | `.with_bind_mount_ro("/host", "/ctr")` |
| `--tmpfs /run` | `.with_tmpfs("/run")` |
| `--read-only` | `.with_readonly_rootfs(true)` |
| `--memory 256m` | `.with_cgroup_memory(256 * 1024 * 1024)` |
| `--cpus 0.5` | `.with_cgroup_cpu_quota(50_000, 100_000)` |
| `--pids-limit 50` | `.with_cgroup_pids_limit(50)` |
| `--cpu-shares 512` | `.with_cgroup_cpu_shares(512)` |
| `--cap-drop ALL` | `.drop_all_capabilities()` |
| `--cap-add NET_ADMIN` | `.with_capabilities(&[Capability::NetAdmin])` |
| `--security-opt seccomp=default` | `.with_seccomp_default()` |
| `--no-new-privileges` | `.with_no_new_privileges(true)` |
| `--user 1000:1000` | `.with_uid(1000).with_gid(1000)` |
| `-e FOO=bar` | `.env("FOO", "bar")` |
| `-w /app` | `.with_cwd("/app")` |
| `--ulimit nofile=1024` | `.with_rlimit(Resource::NOFILE, 1024, 1024)` |
| `--hostname myhostname` | `.with_hostname("myhostname")` |

---

## Common Patterns

### One-shot command

```rust
// podman run --rm alpine echo hello
let mut child = Command::new("/bin/echo")
    .arg("hello")
    .with_chroot(&rootfs)
    .with_namespaces(Namespace::UTS | Namespace::MOUNT)
    .spawn()?;
child.wait()?;
```

### Capture output

```rust
// podman run --rm alpine cat /etc/os-release
let mut child = Command::new("/bin/cat")
    .arg("/etc/os-release")
    .with_chroot(&rootfs)
    .with_namespaces(Namespace::UTS | Namespace::MOUNT)
    .stdout(Stdio::Piped)
    .spawn()?;
let (status, stdout, _stderr) = child.wait_with_output()?;
println!("{}", String::from_utf8_lossy(&stdout));
```

### Interactive shell

```rust
// podman run -it --rm alpine sh
let session = Command::new("/bin/sh")
    .with_chroot(&rootfs)
    .with_namespaces(Namespace::UTS | Namespace::MOUNT | Namespace::PID)
    .with_proc_mount()
    .spawn_interactive()?;

// Blocks: relays stdin/stdout, forwards SIGWINCH (window resize), restores terminal on exit.
let status = session.run()?;
```

### Read-only rootfs with writable scratch space

```rust
// podman run --rm --read-only --tmpfs /tmp alpine
let mut child = Command::new("/bin/sh")
    .with_chroot(&rootfs)
    .with_namespaces(Namespace::UTS | Namespace::MOUNT)
    .with_readonly_rootfs(true)
    .with_tmpfs("/tmp")
    .spawn()?;
child.wait()?;
```

### Network isolation — loopback only

```rust
// podman run --rm --network=none alpine   (with loopback still up)
use remora::network::NetworkMode;

let mut child = Command::new("/bin/sh")
    .with_chroot(&rootfs)
    .with_namespaces(Namespace::UTS | Namespace::MOUNT)
    .with_network(NetworkMode::Loopback)
    .spawn()?;
child.wait()?;
```

The container gets its own NET namespace with `lo` brought up (127.0.0.1 active), but no
route to the outside world.

### Bridge networking with port forwarding (requires root)

```rust
// podman run --rm --network bridge -p 8080:80 --dns 1.1.1.1 myimage nginx
use remora::network::NetworkMode;

let mut child = Command::new("/usr/sbin/nginx")
    .with_chroot(&rootfs)
    .with_namespaces(Namespace::UTS | Namespace::MOUNT)
    .with_proc_mount()
    .with_network(NetworkMode::Bridge)  // 172.19.0.x/24
    .with_nat()                          // nftables MASQUERADE
    .with_port_forward(8080, 80)         // host:8080 → container:80
    .with_dns(&["1.1.1.1", "8.8.8.8"])
    .spawn()?;
child.wait()?;
```

### Rootless with internet access (pasta)

```rust
// Like podman's --network slirp4netns but using pasta (lower overhead)
// Run this WITHOUT sudo — rootless mode is auto-detected.
use remora::network::NetworkMode;

let mut child = Command::new("/bin/sh")
    .with_chroot(&rootfs)
    .with_namespaces(Namespace::UTS | Namespace::MOUNT)
    .with_proc_mount()
    .with_network(NetworkMode::Pasta)
    // Namespace::USER is added automatically when getuid() != 0.
    // pasta --config-net configures IP + routing inside the container.
    .spawn()?;
child.wait()?;
```

Requires `pasta` from the [passt project](https://passt.top) to be installed.

### Resource limits

```rust
// podman run --rm --memory 256m --cpus 0.5 --pids-limit 50 alpine
let mut child = Command::new("/bin/sh")
    .with_chroot(&rootfs)
    .with_namespaces(Namespace::UTS | Namespace::MOUNT)
    .with_cgroup_memory(256 * 1024 * 1024)
    .with_cgroup_cpu_quota(50_000, 100_000)  // 50ms quota per 100ms period = 0.5 CPU
    .with_cgroup_pids_limit(50)
    .spawn()?;

// Check resource usage while running:
let stats = child.resource_stats()?;
println!("memory used: {} bytes", stats.memory_usage_bytes);

child.wait()?;
```

### Full security hardening

```rust
// podman run --rm --cap-drop ALL --security-opt seccomp=default \
//            --no-new-privileges --read-only alpine
let mut child = Command::new("/bin/sh")
    .with_chroot(&rootfs)
    .with_namespaces(Namespace::UTS | Namespace::MOUNT | Namespace::PID)
    .with_proc_mount()
    .drop_all_capabilities()
    .with_seccomp_default()           // Docker's default seccomp profile
    .with_no_new_privileges(true)
    .with_readonly_rootfs(true)
    .with_masked_paths(&["/proc/kcore", "/proc/sysrq-trigger", "/sys/firmware"])
    .with_tmpfs("/tmp")               // writable scratch despite read-only rootfs
    .spawn()?;
child.wait()?;
```

### Named volumes

```rust
// podman volume create mydata && podman run -v mydata:/data alpine
use remora::container::Volume;

let vol = Volume::open("mydata")
    .unwrap_or_else(|_| Volume::create("mydata").unwrap());

let mut child = Command::new("/bin/sh")
    .with_chroot(&rootfs)
    .with_namespaces(Namespace::UTS | Namespace::MOUNT)
    .with_volume(&vol, "/data")
    .spawn()?;
child.wait()?;
```

Volumes are backed by `/var/lib/remora/volumes/<name>/` and persist across container runs.

### Overlay filesystem (copy-on-write rootfs)

```rust
// Like Docker's overlay2 storage driver — base image untouched, writes go to upper layer.
use std::path::PathBuf;

let upper = PathBuf::from("/tmp/container-upper");
let work  = PathBuf::from("/tmp/container-work");
std::fs::create_dir_all(&upper)?;
std::fs::create_dir_all(&work)?;

let mut child = Command::new("/bin/sh")
    .with_chroot(&rootfs)             // lower (read-only base)
    .with_overlay(&upper, &work)      // upper (per-container writes)
    .with_namespaces(Namespace::UTS | Namespace::MOUNT)
    .spawn()?;
child.wait()?;
// upper/ now contains only the files the container wrote or modified.
```

---

## Root vs Rootless

Remora auto-detects whether it's running as root via `getuid()`. No flag or config needed.

| Behaviour | As root | As non-root (rootless) |
|---|---|---|
| `Namespace::USER` | Not added | Added automatically |
| UID/GID mapping | Not configured | `0 → your_uid` (appears as root inside) |
| Cgroups | Applied | Skipped gracefully (no `CAP_SYS_ADMIN`) |
| `NetworkMode::Bridge` | Works | Rejected — requires host `CAP_NET_ADMIN` |
| `NetworkMode::Pasta` | Rejected by pasta's internals | Works — pasta is designed for rootless |
| `NetworkMode::Loopback` | Works | Works |

For rootless internet access, use `NetworkMode::Pasta` and run without `sudo`.

---

## OCI Lifecycle CLI

For use as an OCI runtime backend (e.g. with containerd or CRI-O), Remora implements the
standard five lifecycle commands. A bundle is a directory containing `config.json` and a
`rootfs/` directory in the OCI Runtime Spec format.

```bash
# Create a container (fork shim, pause before exec)
remora create mycontainer /path/to/bundle

# Start it (signal shim to exec the process)
remora start mycontainer

# Query state
remora state mycontainer
# → {"ociVersion":"1.0.2","id":"mycontainer","status":"running","pid":12345,...}

# Send a signal
remora kill mycontainer SIGTERM

# Clean up
remora delete mycontainer
```

State is stored under `/run/remora/<id>/`. The shim double-forks so `remora create` returns
as soon as the container is in the "created" state.

---

## Full-Featured CLI (`remora run`, `ps`, `stop`, `rm`, `logs`)

Remora provides a full operator CLI similar to `podman`/`nerdctl`. Unlike the OCI lifecycle
commands (machine interface for containerd), these commands are for direct human use.

### Setting Up a Rootfs Image

```bash
# Import Alpine rootfs directory under the name "alpine"
sudo remora rootfs import alpine ./alpine-rootfs

# List imported rootfs images
remora rootfs ls

# Remove a rootfs image (removes symlink only, not the directory)
remora rootfs rm alpine
```

### Running Containers

```bash
# Foreground (blocks until exit)
sudo remora run alpine /bin/echo hello

# Interactive PTY
sudo remora run --interactive alpine /bin/sh

# Detached — prints container name, returns immediately
sudo remora run --detach --name web alpine /bin/sh -c 'while true; do echo tick; sleep 1; done'
```

### Container Lifecycle

```bash
# List running containers
remora ps

# List all containers (including exited)
remora ps --all

# View logs (detached containers only)
remora logs web
remora logs --follow web    # tail -f style

# Stop a container (SIGTERM)
sudo remora stop web

# Remove a container (must be stopped)
remora rm web

# Force remove (SIGKILL + remove even if running)
remora rm --force web
```

### Key `run` Flags

```bash
sudo remora run \
  --name mycontainer          \  # optional name (auto-generated if omitted)
  --detach                    \  # background mode
  --interactive               \  # PTY (incompatible with --detach)
  --network bridge            \  # none|loopback|bridge|pasta
  --publish 8080:80           \  # TCP port forward
  --nat                       \  # enable MASQUERADE NAT
  --dns 1.1.1.1               \  # DNS server
  --volume mydata:/data       \  # named volume (auto-created)
  --bind /host/path:/ctr/path \  # rw bind mount
  --bind-ro /etc:/etc         \  # ro bind mount
  --tmpfs /tmp                \  # in-memory writable dir
  --read-only                 \  # read-only rootfs
  --env KEY=VALUE             \  # environment variable
  --env-file ./env.txt        \  # load env from file
  --workdir /app              \  # working directory inside container
  --user 1000:1000            \  # UID[:GID]
  --hostname mybox            \  # container hostname
  --memory 256m               \  # cgroup v2 memory limit
  --cpus 0.5                  \  # CPU quota (0.5 = 50%)
  --cpu-shares 512            \  # CPU weight
  --pids-limit 50             \  # max processes
  --ulimit nofile=1024:2048   \  # rlimit
  --cap-drop ALL              \  # drop all capabilities
  --cap-add CAP_NET_BIND_SERVICE \  # add back specific caps
  --security-opt seccomp=default \  # seccomp profile
  --security-opt no-new-privileges \
  --sysctl net.ipv4.ip_forward=1 \
  --masked-path /proc/kcore   \
  alpine /bin/sh
```

### Volume Management

```bash
sudo remora volume create mydata
remora volume ls
sudo remora volume rm mydata
```

### Storage Layout

```
/var/lib/remora/
  rootfs/<name>           symlink to rootfs directory
  volumes/<name>/         named volume data
  container_counter       monotonic counter for auto-naming

/run/remora/
  containers/<name>/
    state.json            container metadata and status
    stdout.log            captured stdout (detached mode)
    stderr.log            captured stderr (detached mode)
```

---

## What's Out of Scope

| Podman / nerdctl feature | Remora status |
|---|---|
| `podman pull` / image layers | Out of scope — export a rootfs from Docker/Podman |
| Container registry (push/pull) | Out of scope |
| `podman-compose` / pods | Out of scope |
| Restart policies | Implement in your application around `wait()` |
| Health checks | Implement in your application |
| CNI / network plugins | Out of scope — native implementation |
| AppArmor / SELinux profiles | Planned (deferred) |
| UDP port forwarding | Planned |
| Image build (`podman build`) | Out of scope |
| `remora exec` (join running container) | Planned (Phase 2) |
