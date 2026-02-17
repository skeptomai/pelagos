# Remora Development Roadmap

**Date:** 2026-02-16
**Current Status:** Phase 0 Complete (Basic containerization)
**Goal:** Production-grade container runtime library

---

## Philosophy & Approach

**Design Principles:**
1. **Security First** - No feature is worth compromising security
2. **Incremental Value** - Each phase delivers usable functionality
3. **Learn from Others** - Study runc, Docker, Podman implementations
4. **Keep it Rust** - Leverage type safety and zero-cost abstractions
5. **Test Everything** - Every feature needs integration tests

**Not in Scope:**
- ❌ Image management (pull/push/build) - Use existing tools
- ❌ Container orchestration - Focus on single-container runtime
- ❌ GUI/Dashboard - CLI and library only
- ❌ Registry operations - Out of scope

---

## Phase 0: Foundation ✅ COMPLETE

**Status:** Done (current state)

**What We Have:**
- ✅ Basic namespace isolation (UTS, MOUNT, IPC, NET, USER, CGROUP)
- ✅ chroot/pivot_root
- ✅ Capability management (12 capabilities)
- ✅ Resource limits via rlimits
- ✅ Automatic mount helpers (/proc, /sys, /dev)
- ✅ Namespace joining (setns)
- ✅ 12 integration tests
- ✅ Comprehensive documentation

**Assessment:**
- Good foundation for learning and experimentation
- Not production-ready due to security gaps
- About 35% feature parity with runc

---

## Phase 1: Security Hardening 🔴 CRITICAL

**Priority:** ⭐⭐⭐ HIGHEST
**Complexity:** 🟡 Medium
**Impact:** 🔥 CRITICAL - Blocks production use

### Overview

**Goal:** Make containers actually secure for real-world use

**Why First:**
- Security is non-negotiable for any production runtime
- Everything else is pointless if containers aren't secure
- Establishes secure-by-default mindset
- Required before anyone should use this seriously

**Current Risk:**
```bash
# Right now, a container can:
- Call any syscall (no seccomp filtering)
- Write to rootfs (no read-only option)
- Access /proc/kcore, /sys/firmware (unmasked paths)
- Gain new privileges (no no-new-privileges)
- Escape via various kernel exploits
```

### Features to Implement

#### 1.1 Seccomp Filtering ⭐⭐⭐

**What:** Syscall filtering to block dangerous kernel operations

**Why Critical:**
- Blocks 90% of container escape techniques
- Prevents kernel exploits
- Industry standard (Docker, Podman, all production runtimes use this)

**Complexity:** 🟡 Medium
- Need to parse seccomp BPF profiles
- Integrate with libseccomp or implement manually
- Default profile with ~300 blocked syscalls

**Scope:**
```rust
// API Design
let child = Command::new("/bin/sh")
    .with_seccomp_default()  // Use default profile
    .with_seccomp_file("custom.json")  // Custom profile
    .with_seccomp_allow(&["read", "write", "exit"])  // Whitelist
    .spawn()?;
```

**Implementation Notes:**
- Use `libseccomp-rs` crate or `seccompiler-rs` (from Firecracker)
- Ship with Docker's default seccomp profile
- Allow custom profiles (JSON format)
- Option to disable (for debugging)

**Testing:**
- Verify blocked syscalls actually fail
- Test default profile doesn't break common apps
- Test custom profiles work

**Reference Implementations:**
- [Docker default seccomp profile](https://github.com/moby/moby/blob/master/profiles/seccomp/default.json)
- [OCI runtime spec - seccomp](https://github.com/opencontainers/runtime-spec/blob/main/config-linux.md#seccomp)

**Useful For:**
- ✅ Production deployments
- ✅ Running untrusted code
- ✅ Security-conscious environments
- ✅ Compliance requirements

**Usefulness Score:** 10/10 - Absolutely essential

---

#### 1.2 Read-Only Rootfs ⭐⭐⭐

**What:** Make container filesystem immutable

**Why Critical:**
- Prevents malware persistence
- Enforces immutable infrastructure
- Common security best practice

**Complexity:** 🟢 Easy
- Just remount rootfs as read-only
- Simple flag in mount options

**Scope:**
```rust
let child = Command::new("/bin/sh")
    .with_chroot("/path/to/rootfs")
    .with_readonly_rootfs(true)  // New flag
    .spawn()?;
```

**Implementation:**
```rust
// In pre_exec after chroot:
if readonly_rootfs {
    unsafe {
        libc::mount(
            ptr::null(),
            c"/".as_ptr(),
            ptr::null(),
            libc::MS_REMOUNT | libc::MS_RDONLY | libc::MS_BIND,
            ptr::null()
        );
    }
}
```

**Testing:**
- Verify writes to rootfs fail
- Verify reads still work
- Test with tmpfs for writable areas

**Useful For:**
- ✅ Immutable containers
- ✅ Security hardening
- ✅ Preventing rootkit installation
- ✅ Compliance (PCI, SOC2)

**Usefulness Score:** 9/10 - Very common pattern

---

#### 1.3 Masked Paths ⭐⭐

**What:** Hide sensitive kernel paths from containers

**Why Important:**
- Prevents information leakage
- Blocks some escape vectors
- Standard practice in all runtimes

**Complexity:** 🟢 Easy
- Bind mount /dev/null over sensitive paths

**Paths to Mask:**
```
/proc/kcore          # Physical memory access
/proc/keys           # Kernel keyring
/proc/timer_list     # Timing attacks
/proc/sched_debug    # Scheduling info
/sys/firmware        # Firmware access
/sys/devices/virtual/powercap  # Power info
```

**Scope:**
```rust
let child = Command::new("/bin/sh")
    .with_masked_paths_default()  // Use default list
    .with_masked_paths(&["/proc/kcore", "/sys/firmware"])  // Custom
    .spawn()?;
```

**Implementation:**
```rust
// In pre_exec after chroot:
for path in masked_paths {
    mount_dev_null_over(path)?;
}
```

**Testing:**
- Verify masked paths return ENOENT
- Verify container still functions

**Useful For:**
- ✅ Information hiding
- ✅ Security hardening
- ✅ Preventing kernel info leaks

**Usefulness Score:** 7/10 - Defense in depth

---

#### 1.4 No-New-Privileges Flag ⭐⭐

**What:** Prevent privilege escalation via setuid binaries

**Why Important:**
- Blocks setuid/setgid exploits
- Prevents privilege escalation
- Simple but effective

**Complexity:** 🟢 Trivial
- Single prctl call

**Scope:**
```rust
let child = Command::new("/bin/sh")
    .with_no_new_privileges(true)
    .spawn()?;
```

**Implementation:**
```rust
// In pre_exec:
unsafe {
    libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0);
}
```

**Testing:**
- Verify setuid binaries don't gain privileges
- Verify normal execution still works

**Useful For:**
- ✅ Preventing privilege escalation
- ✅ Running untrusted code
- ✅ Required for unprivileged seccomp

**Usefulness Score:** 8/10 - Simple and effective

---

### Phase 1 Deliverables

**New API Methods:**
```rust
.with_seccomp_default()
.with_seccomp_file(path)
.with_seccomp_allow(&[syscalls])
.with_readonly_rootfs(bool)
.with_masked_paths_default()
.with_masked_paths(&[paths])
.with_no_new_privileges(bool)
```

**Dependencies:**
```toml
libseccomp = "0.3"  # or seccompiler
```

**Tests:**
- Seccomp profile enforcement
- Read-only rootfs verification
- Masked paths inaccessibility
- No-new-privileges enforcement
- Integration tests for all features combined

**Documentation:**
- SECURITY.md - Security features guide
- Update RUNTIME_COMPARISON.md
- Add examples to README

**Success Criteria:**
- ✅ Default seccomp profile blocks dangerous syscalls
- ✅ Containers can run read-only
- ✅ Sensitive paths are hidden
- ✅ Privilege escalation is prevented
- ✅ All tests pass

**After Phase 1:**
- Containers are secure enough for production use
- Can run untrusted code safely
- Meets basic security compliance requirements

---

## Phase 2: Interactive Containers 🟡 HIGH VALUE

**Priority:** ⭐⭐⭐ High
**Complexity:** 🟡 Medium
**Impact:** 🔥 HIGH - Enables development workflows
**Why Second:** Interactive shells are extremely common use case

### Overview

**Goal:** Enable interactive development workflows

**Current Problem:**
```bash
# This doesn't work:
$ remora --exe /bin/sh --rootfs ./alpine-rootfs
# Can't type anything, no interactive shell!

# Docker can do:
$ docker run -it alpine sh
/ # ls
bin  dev  etc  home
/ # whoami
root
```

**Why High Priority:**
- Interactive shells are fundamental to containers
- Developers expect `docker run -it` behavior
- Debugging without interactive shell is painful
- Enables REPL workflows, shell access, debugging

### Features to Implement

#### 2.1 TTY/PTY Support ⭐⭐⭐

**What:** Pseudo-terminal support for interactive shells

**Why Critical:**
- Required for interactive shells
- Needed for text editors (vim, nano)
- Terminal control codes (colors, cursor movement)
- Job control (Ctrl+C, Ctrl+Z)

**Complexity:** 🔴 High
- PTY allocation is complex
- Terminal settings management
- Window size handling
- Signal forwarding

**Current State:**
```rust
// We only have:
.stdin(Stdio::Inherit)   // Not a real TTY
.stdout(Stdio::Inherit)  // No terminal control
```

**Target API:**
```rust
let child = Command::new("/bin/sh")
    .with_chroot(rootfs)
    .with_tty(true)  // Allocate PTY
    .with_raw_terminal(true)  // Raw mode
    .spawn()?;

// Or more explicit:
let pty = Pty::new()?;
let child = Command::new("/bin/sh")
    .with_pty(&pty)
    .spawn()?;

// Interactive session
pty.interact()?;  // Forward stdin/stdout/signals
```

**Implementation Challenges:**

1. **PTY Allocation:**
```rust
// Need to:
- Open /dev/ptmx (master)
- Get slave PTY path (ptsname)
- Set slave as stdin/stdout/stderr in child
- Handle master in parent for I/O
```

2. **Terminal Settings:**
```rust
// Need to manage:
- Raw mode vs cooked mode
- Echo settings
- Line buffering
- Special character handling
```

3. **Window Size:**
```rust
// Need to:
- Detect terminal size (TIOCGWINSZ)
- Forward size to PTY (TIOCSWINSZ)
- Handle SIGWINCH (window resize)
```

4. **Signal Handling:**
```rust
// Need to forward:
- Ctrl+C (SIGINT)
- Ctrl+Z (SIGTSTP)
- Ctrl+\ (SIGQUIT)
```

**Crates to Use:**
- `nix::pty` - PTY operations
- `termion` - Terminal control
- `crossterm` - Cross-platform terminal (alternative)

**Reference Implementations:**
- runc: https://github.com/opencontainers/runc/blob/main/libcontainer/utils/utils_unix.go#L89
- Docker: Uses containerd's console package
- youki (Rust runtime): https://github.com/containers/youki

**Testing:**
- Verify interactive shell works
- Test Ctrl+C signal forwarding
- Test window resize
- Test cursor movement, colors
- Test vim/nano editors work

**Useful For:**
- ✅ Interactive debugging
- ✅ Development containers
- ✅ Running shells
- ✅ Text editors
- ✅ Any interactive program

**Usefulness Score:** 10/10 - Fundamental feature

---

#### 2.2 Better Signal Handling ⭐⭐

**What:** Forward signals to container process

**Why Important:**
- Graceful shutdown (SIGTERM before SIGKILL)
- Interactive control (Ctrl+C)
- Process management

**Complexity:** 🟡 Medium
- Signal handling in Rust is tricky
- Need signal forwarding to child
- Handle signal masks

**Current State:**
```rust
// We can wait for exit, but can't send signals
child.wait()?;
```

**Target API:**
```rust
let mut child = Command::new("/bin/sh")
    .spawn()?;

// Send signals
child.kill(Signal::SIGTERM)?;
child.kill(Signal::SIGKILL)?;

// Wait with timeout
child.wait_timeout(Duration::from_secs(10))?;
```

**Implementation:**
```rust
impl Child {
    pub fn kill(&mut self, signal: Signal) -> Result<()> {
        unsafe {
            libc::kill(self.pid, signal as i32);
        }
        Ok(())
    }

    pub fn wait_timeout(&mut self, timeout: Duration) -> Result<Option<ExitStatus>> {
        // Use WNOHANG with a loop
    }
}
```

**Testing:**
- Verify SIGTERM is delivered
- Verify SIGKILL works
- Test timeout behavior

**Useful For:**
- ✅ Graceful shutdown
- ✅ Container lifecycle management
- ✅ Cleanup on exit

**Usefulness Score:** 8/10 - Common need

---

#### 2.3 Exec into Running Container ⭐⭐⭐

**What:** Run commands in an already-running container

**Why Important:**
- Debugging running containers
- Inspecting state
- Running one-off commands

**Complexity:** 🔴 Very High
- Need to join all namespaces of running container
- Need to setns() into PID, mount, net, etc.
- Need to match container environment
- Most complex feature in this phase

**Current State:**
```bash
# Can't do this:
docker exec -it mycontainer /bin/sh
```

**Target API:**
```rust
// Start container
let container = Command::new("/my/app")
    .spawn_detached()?;  // Background

// Later, exec into it
let exec = Command::new("/bin/sh")
    .exec_into(&container)  // Join its namespaces
    .with_tty(true)
    .spawn()?;
```

**Implementation:**
```rust
// Need to:
1. Get all namespace FDs from target container:
   - /proc/<pid>/ns/pid
   - /proc/<pid>/ns/mnt
   - /proc/<pid>/ns/net
   - etc.

2. setns() into each namespace:
   for ns in namespaces {
       setns(ns_fd, ns_type)?;
   }

3. Match container's:
   - Working directory
   - Environment variables
   - UID/GID
   - Capabilities
   - cgroup

4. Execute command
```

**Challenges:**
- PID namespace joining is tricky (might not be possible without fork)
- Need container state tracking
- Need to match exact container environment

**Alternative:**
- Skip this for now, implement in Phase 6 with full lifecycle management
- Or implement simple version (join namespaces only, not full exec)

**Testing:**
- Exec into container works
- Sees same filesystem
- Shares network
- Can see processes (if PID namespace works)

**Useful For:**
- ✅ Debugging running containers
- ✅ Inspecting container state
- ✅ Running diagnostic commands

**Usefulness Score:** 9/10 - Very common workflow

**Recommendation:** Maybe defer to Phase 6, focus on TTY/signals first

---

### Phase 2 Deliverables

**New API Methods:**
```rust
.with_tty(bool)
.with_pty(pty)
.with_raw_terminal(bool)
.kill(signal)
.wait_timeout(duration)
.exec_into(container)  // Maybe Phase 6
```

**New Types:**
```rust
pub struct Pty { /* master/slave FDs */ }
pub enum Signal { SIGTERM, SIGKILL, SIGINT, ... }
```

**Dependencies:**
```toml
nix = { version = "0.31", features = ["pty", "signal"] }
termion = "2.0"  # or crossterm
```

**Tests:**
- Interactive shell works
- Signal forwarding works
- Window resize works
- Terminal control codes work
- Ctrl+C interrupts properly

**Success Criteria:**
- ✅ Can run interactive shells (like `docker run -it`)
- ✅ Ctrl+C, Ctrl+Z work properly
- ✅ Terminal editors work (vim, nano)
- ✅ Can send signals to containers
- ✅ Colors and cursor movement work

**After Phase 2:**
- Development workflows enabled
- Interactive debugging possible
- Much more usable for developers

---

## Phase 3: Networking 🔴 ESSENTIAL

**Priority:** ⭐⭐⭐ High
**Complexity:** 🔴 Very High
**Impact:** 🔥 CRITICAL - Most containers need networking
**Why Third:** Containers need isolation AND communication

### Overview

**Goal:** Create isolated networks for containers

**Current Problem:**
```bash
# Container shares host network:
$ remora --exe /bin/sh
/ # ip addr
1: lo: <LOOPBACK,UP>
2: eth0: <BROADCAST,UP>  # HOST'S NETWORK!
3: wlan0: <BROADCAST,UP>  # ALL HOST INTERFACES!

# Docker can do:
$ docker run alpine ip addr
1: lo: <LOOPBACK,UP>
2: eth0@if15: <BROADCAST,UP>  # ISOLATED veth interface
```

**Why Critical:**
- Network isolation is fundamental to containers
- Most production containers need networking
- Security: prevent access to host network
- Flexibility: multiple containers on same host

**Complexity Warning:**
This is the hardest phase. Networking is complex and involves:
- Network namespaces (we can create, but not setup)
- veth pairs (virtual ethernet)
- Bridge configuration
- Routing tables
- iptables rules
- DNS resolution

### Features to Implement

#### 3.1 Create Network Namespace ⭐⭐⭐

**What:** Isolated network stack for container

**Current State:**
```rust
// We can join existing network namespaces:
.with_namespace_join("/var/run/netns/test", Namespace::NET)

// But we can't create isolated ones
```

**Complexity:** 🟡 Medium
- Creating namespace is easy (unshare)
- Setting up network IN that namespace is hard

**Scope:**
```rust
let child = Command::new("/bin/sh")
    .with_network_isolation(true)  // Create NET namespace
    .spawn()?;

// Inside container:
// Only sees lo (loopback), no other interfaces
```

**Implementation:**
```rust
// In pre_exec:
if network_isolation {
    // Unshare NET namespace (we already do this)
    unshare(CloneFlags::CLONE_NEWNET)?;

    // Setup loopback interface
    setup_loopback()?;
}

fn setup_loopback() -> Result<()> {
    // Bring up lo interface
    // This requires netlink or ip command
}
```

**Testing:**
- Container only sees lo interface
- lo interface is UP
- Can ping 127.0.0.1

**Useful For:**
- ✅ Network isolation
- ✅ Multiple containers on same host
- ✅ Security

**Usefulness Score:** 10/10 - Fundamental

---

#### 3.2 veth Pair Creation ⭐⭐⭐

**What:** Virtual ethernet pair for container connectivity

**Why Critical:**
- Connect container to host
- Containers need outbound connectivity
- Foundation for all container networking

**Complexity:** 🔴 Very High
- Need to use netlink (rtnetlink)
- Create veth pair: veth0 (host) <-> veth1 (container)
- Move veth1 into container's network namespace
- Configure IP addresses
- Setup routing

**Scope:**
```rust
let child = Command::new("/bin/sh")
    .with_network_bridge("docker0")  // Or default bridge
    .with_network_ip("172.17.0.2/16")
    .spawn()?;

// Inside container:
// eth0: 172.17.0.2
// Can reach 172.17.0.1 (bridge)
// Can reach internet
```

**Implementation Steps:**

1. **Create veth pair** (in host network namespace):
```rust
// Use rtnetlink crate
let veth_host = "vethXXXXX";  // Random name
let veth_container = "eth0";

create_veth_pair(veth_host, veth_container)?;
```

2. **Move veth into container namespace**:
```rust
// After container starts:
move_interface_to_namespace(veth_container, container_pid)?;
```

3. **Configure IP address**:
```rust
// In container:
set_ip_address(veth_container, "172.17.0.2/16")?;
set_interface_up(veth_container)?;
```

4. **Setup routing**:
```rust
// In container:
add_default_route("172.17.0.1")?;  // Bridge IP
```

5. **Connect to bridge** (on host):
```rust
add_interface_to_bridge(veth_host, "docker0")?;
```

**Crates to Use:**
- `rtnetlink` - Netlink library for network configuration
- `nix::net` - Network utilities
- Or shell out to `ip` command (simpler but less portable)

**Reference:**
- Docker's libnetwork: https://github.com/moby/libnetwork
- CNI (Container Network Interface): https://github.com/containernetworking/cni
- runc delegates to CNI plugins

**Testing:**
- Container has eth0 interface
- Can ping bridge IP
- Can ping internet (with NAT)
- Multiple containers can communicate

**Useful For:**
- ✅ Container connectivity
- ✅ Container-to-container networking
- ✅ Outbound internet access

**Usefulness Score:** 10/10 - Essential for real containers

---

#### 3.3 Bridge Networking ⭐⭐

**What:** Network bridge for connecting containers

**Why Important:**
- Connect multiple containers together
- Shared network for container communication
- Default network for Docker

**Complexity:** 🔴 High
- Create bridge interface
- Configure bridge IP
- Setup NAT for outbound traffic
- iptables rules

**Scope:**
```rust
// Create bridge (one-time setup):
let bridge = NetworkBridge::new("remora0")?;
bridge.set_ip("172.18.0.1/16")?;
bridge.enable_nat()?;

// Use bridge for containers:
let child = Command::new("/bin/sh")
    .with_network_bridge(&bridge)
    .spawn()?;
```

**Implementation:**
```rust
// 1. Create bridge:
create_bridge("remora0")?;
set_bridge_ip("remora0", "172.18.0.1/16")?;

// 2. Enable NAT (iptables):
iptables("-t nat -A POSTROUTING -s 172.18.0.0/16 -j MASQUERADE")?;

// 3. Enable forwarding:
echo("1", "/proc/sys/net/ipv4/ip_forward")?;
```

**Testing:**
- Bridge exists and is UP
- Containers get IPs from bridge subnet
- Containers can ping each other
- Containers can reach internet

**Useful For:**
- ✅ Multi-container applications
- ✅ Container-to-container communication
- ✅ Shared network

**Usefulness Score:** 8/10 - Common pattern

---

#### 3.4 DNS Configuration ⭐⭐

**What:** DNS resolution for containers

**Why Important:**
- Containers need to resolve hostnames
- Access to internet services

**Complexity:** 🟢 Easy
- Copy /etc/resolv.conf into container
- Or generate custom resolv.conf

**Scope:**
```rust
let child = Command::new("/bin/sh")
    .with_dns(&["8.8.8.8", "8.8.4.4"])
    .spawn()?;
```

**Implementation:**
```rust
// Write /etc/resolv.conf in container rootfs:
write_resolv_conf(rootfs, &dns_servers)?;
```

**Testing:**
- Container can resolve hostnames
- nslookup/dig work

**Useful For:**
- ✅ Accessing external services
- ✅ Package managers (apt, apk)

**Usefulness Score:** 9/10 - Very common need

---

### Phase 3 Deliverables

**New API Methods:**
```rust
.with_network_isolation(bool)
.with_network_bridge(name)
.with_network_ip(ip)
.with_dns(&[servers])
```

**New Types:**
```rust
pub struct NetworkBridge { /* ... */ }
```

**Dependencies:**
```toml
rtnetlink = "0.14"  # Network configuration
ipnetwork = "0.20"  # IP address handling
```

**Helper Tools:**
```rust
// Network utility module
pub mod net {
    pub fn create_veth_pair(...) -> Result<()>;
    pub fn create_bridge(...) -> Result<()>;
    pub fn setup_nat(...) -> Result<()>;
}
```

**Tests:**
- Container has isolated network
- Can ping loopback
- Can ping bridge gateway
- Can ping internet
- Can resolve DNS
- Multiple containers can communicate

**Success Criteria:**
- ✅ Containers have isolated network namespaces
- ✅ veth pairs connect containers to host
- ✅ Containers can reach internet
- ✅ DNS resolution works
- ✅ Multiple containers can communicate

**After Phase 3:**
- Containers have proper network isolation
- Can run networked applications
- Multi-container setups possible

**Reality Check:**
This is the hardest phase. Consider:
1. Start with basic loopback-only isolation
2. Add veth pairs second
3. Bridge networking third
4. Or use CNI plugins (delegate networking to external tools)

**Alternative Approach - CNI Plugins:**
Instead of implementing networking ourselves, use CNI:
```rust
// Delegate to CNI plugin:
let network = CniNetwork::new("bridge")?;
network.setup_container(container_id)?;
```

This is what runc does - it delegates all networking to CNI plugins.

**Recommendation:** Start with CNI delegation, implement native later if needed.

---

## Phase 4: Filesystem Flexibility 🟡 COMMON

**Priority:** ⭐⭐ Medium-High
**Complexity:** 🟡 Medium
**Impact:** 🔥 HIGH - Very common use case
**Why Fourth:** Needed for persistent data, configuration

### Overview

**Goal:** Flexible filesystem management

**Current Problem:**
```bash
# Can only chroot into a directory
remora --rootfs /path/to/rootfs

# Can't:
- Mount host directories into container
- Create tmpfs for /tmp
- Share files between host and container
- Persist data
```

**Why Important:**
- Containers need persistent storage
- Need to share config files
- Need temporary writable space (even with read-only rootfs)
- Very common pattern in production

### Features to Implement

#### 4.1 Bind Mounts ⭐⭐⭐

**What:** Mount host directory into container

**Why Critical:**
- Share files between host and container
- Persist data
- Mount configuration files
- Development workflows (mount source code)

**Complexity:** 🟡 Medium
- Mount operations are straightforward
- Path resolution can be tricky
- Need to handle permissions

**Scope:**
```rust
let child = Command::new("/bin/sh")
    .with_chroot(rootfs)
    .with_bind_mount("/host/data", "/container/data")
    .with_bind_mount_ro("/host/config", "/etc/config")  // Read-only
    .spawn()?;
```

**Implementation:**
```rust
pub struct BindMount {
    source: PathBuf,
    target: PathBuf,
    readonly: bool,
}

// In pre_exec after chroot:
for mount in bind_mounts {
    let mut flags = libc::MS_BIND;
    if mount.readonly {
        flags |= libc::MS_RDONLY;
    }

    unsafe {
        libc::mount(
            mount.source.as_ptr(),
            mount.target.as_ptr(),
            ptr::null(),
            flags,
            ptr::null()
        );
    }
}
```

**Testing:**
- Files visible in container
- Modifications sync to host (read-write)
- Read-only mounts prevent writes
- Permissions preserved

**Useful For:**
- ✅ Persistent storage
- ✅ Configuration files
- ✅ Log collection
- ✅ Development workflows

**Usefulness Score:** 10/10 - Extremely common

---

#### 4.2 tmpfs Mounts ⭐⭐

**What:** In-memory temporary filesystems

**Why Important:**
- Fast temporary storage
- Writable /tmp in read-only containers
- No disk I/O for temp files
- Automatic cleanup

**Complexity:** 🟢 Easy
- Simple mount operation

**Scope:**
```rust
let child = Command::new("/bin/sh")
    .with_readonly_rootfs(true)
    .with_tmpfs("/tmp", "size=100m")  // 100MB limit
    .with_tmpfs("/run", "size=50m")
    .spawn()?;
```

**Implementation:**
```rust
// In pre_exec:
unsafe {
    libc::mount(
        c"tmpfs".as_ptr(),
        c"/tmp".as_ptr(),
        c"tmpfs".as_ptr(),
        libc::MS_NOSUID | libc::MS_NODEV,
        c"size=100m".as_ptr()
    );
}
```

**Testing:**
- tmpfs mounted at specified path
- Size limit enforced
- Files disappear after container stops
- Fast (in-memory)

**Useful For:**
- ✅ Read-only containers with writable /tmp
- ✅ Fast temporary storage
- ✅ Build artifacts
- ✅ Caching

**Usefulness Score:** 8/10 - Common pattern

---

#### 4.3 Volume Management ⭐⭐

**What:** Named, managed storage volumes

**Why Important:**
- Persistent data between container runs
- Share data between containers
- Lifecycle independent of containers

**Complexity:** 🟡 Medium
- Need volume creation/deletion
- Need volume listing
- Need cleanup

**Scope:**
```rust
// Create volume:
let volume = Volume::create("mydata")?;

// Use volume:
let child = Command::new("/bin/sh")
    .with_volume(&volume, "/data")
    .spawn()?;

// Volume persists after container stops
// Can be reused by other containers
```

**Implementation:**
```rust
// Volumes are just directories:
pub struct Volume {
    name: String,
    path: PathBuf,  // e.g., /var/lib/remora/volumes/mydata
}

impl Volume {
    pub fn create(name: &str) -> Result<Self> {
        let path = PathBuf::from("/var/lib/remora/volumes").join(name);
        fs::create_dir_all(&path)?;
        Ok(Volume { name: name.to_string(), path })
    }
}

// Usage is just a bind mount:
.with_bind_mount(volume.path, "/data")
```

**Testing:**
- Volume persists data
- Multiple containers can use same volume
- Volume can be deleted

**Useful For:**
- ✅ Databases
- ✅ Persistent application data
- ✅ Shared data between containers

**Usefulness Score:** 9/10 - Essential for stateful apps

---

#### 4.4 Overlay Filesystem ⭐

**What:** Layered filesystem for image layers

**Why Important:**
- Enables image layers (base + modifications)
- Copy-on-write efficiency
- Foundation for image management

**Complexity:** 🔴 High
- Overlay mounting is complex
- Need lower, upper, work directories
- Cleanup can be tricky

**Scope:**
```rust
let child = Command::new("/bin/sh")
    .with_overlay_rootfs(
        lower: "/layers/base",
        upper: "/layers/container",
        work: "/layers/work"
    )
    .spawn()?;
```

**Implementation:**
```rust
// Mount overlayfs:
unsafe {
    libc::mount(
        c"overlay".as_ptr(),
        target.as_ptr(),
        c"overlay".as_ptr(),
        0,
        format!("lowerdir={},upperdir={},workdir={}",
            lower, upper, work).as_ptr()
    );
}
```

**Testing:**
- Lower layer is read-only
- Modifications go to upper layer
- Lower layer unchanged after container exits

**Useful For:**
- ✅ Image layers (if we add image support later)
- ✅ Efficient storage
- ✅ Fast container startup

**Usefulness Score:** 7/10 - Important for images, not critical now

**Recommendation:** Defer to Phase 6 when adding OCI support

---

### Phase 4 Deliverables

**New API Methods:**
```rust
.with_bind_mount(source, target)
.with_bind_mount_ro(source, target)
.with_tmpfs(path, options)
.with_volume(volume, target)
.with_overlay_rootfs(lower, upper, work)  // Maybe Phase 6
```

**New Types:**
```rust
pub struct BindMount { source, target, readonly }
pub struct Volume { name, path }
```

**Tests:**
- Bind mounts work
- Read-only mounts enforced
- tmpfs size limits work
- Volumes persist data
- Multiple containers can share volumes

**Success Criteria:**
- ✅ Can mount host directories
- ✅ tmpfs works for temporary storage
- ✅ Volumes provide persistent storage
- ✅ Read-only mounts work

**After Phase 4:**
- Containers can have persistent data
- Flexible filesystem configurations
- Development workflows improved

---

## Phase 5: Advanced Resource Management 🟡 BETTER CONTROL

**Priority:** ⭐⭐ Medium
**Complexity:** 🟡 Medium
**Impact:** 🔥 Medium - Better than rlimits
**Why Fifth:** Nice upgrade, but rlimits work for now

### Overview

**Goal:** Implement cgroups for better resource control

**Current State:**
```rust
// We have rlimits:
.with_max_fds(1024)
.with_memory_limit(512_000_000)
.with_cpu_time_limit(300)

// We have cgroups-rs dependency, but not implemented
```

**Why Not Earlier:**
- rlimits work well for basic limits
- cgroups add complexity
- Most containers don't need advanced cgroups

**When You Need Cgroups:**
- I/O bandwidth limits
- CPU shares (proportional)
- Better memory accounting
- Device access control
- Hierarchical limits

### Features to Implement

#### 5.1 cgroups v2 Integration ⭐⭐

**What:** Use cgroups-rs to manage container resources

**Complexity:** 🟡 Medium
- API integration straightforward
- Cleanup important
- Testing needs root

**Scope:**
```rust
use cgroups_rs::Cgroup;

let child = Command::new("/bin/sh")
    .with_cgroup_memory(512_000_000)  // 512 MB
    .with_cgroup_cpu_quota(50, 100)   // 50% of one core
    .with_cgroup_cpu_weight(500)      // Relative weight
    .with_cgroup_io_weight(400)       // I/O priority
    .spawn()?;
```

**Implementation:**
```rust
// In spawn():
let cgroup = Cgroup::new("remora", &format!("container_{}", id))?;

if let Some(mem) = self.cgroup_memory {
    let mem_controller: &MemController = cgroup.controller_of().unwrap();
    mem_controller.set_limit(mem)?;
}

// Add child PID to cgroup:
cgroup.add_task_by_tgid(child.pid())?;

// Store for cleanup:
self.cgroup = Some(cgroup);

// In drop:
if let Some(cg) = self.cgroup {
    cg.delete()?;
}
```

**Testing:**
- Memory limits enforced
- CPU quotas work
- I/O limits effective
- cgroup cleanup happens

**Useful For:**
- ✅ Better resource isolation
- ✅ I/O bandwidth limits
- ✅ CPU shares
- ✅ Memory accounting

**Usefulness Score:** 7/10 - Nice to have, not critical

---

#### 5.2 Resource Accounting ⭐

**What:** Track actual resource usage

**Why Useful:**
- Monitor container resource consumption
- Billing/metering
- Optimization

**Complexity:** 🟢 Easy
- cgroups provides stats for free

**Scope:**
```rust
let stats = child.resource_stats()?;
println!("Memory used: {} bytes", stats.memory_used);
println!("CPU time: {} ns", stats.cpu_time);
println!("I/O read: {} bytes", stats.io_read);
```

**Implementation:**
```rust
// Read from cgroup files:
pub struct ResourceStats {
    memory_used: u64,
    memory_max: u64,
    cpu_time: u64,
    io_read: u64,
    io_write: u64,
}

impl Child {
    pub fn resource_stats(&self) -> Result<ResourceStats> {
        if let Some(cg) = &self.cgroup {
            let mem: &MemController = cg.controller_of().unwrap();
            // Read stats from cgroup
        }
    }
}
```

**Useful For:**
- ✅ Monitoring
- ✅ Resource optimization
- ✅ Cost allocation

**Usefulness Score:** 6/10 - Nice for monitoring

---

### Phase 5 Deliverables

**New API Methods:**
```rust
.with_cgroup_memory(bytes)
.with_cgroup_cpu_quota(quota, period)
.with_cgroup_cpu_weight(weight)
.with_cgroup_io_weight(weight)
.resource_stats()
```

**Dependencies:**
Already have: `cgroups-rs = "0.5.0"`

**Tests:**
- cgroup limits enforced
- Resource stats accurate
- Cleanup works

**Success Criteria:**
- ✅ cgroup limits work
- ✅ Better than rlimits for most use cases
- ✅ Resource monitoring available

**After Phase 5:**
- Professional resource management
- Better visibility into resource usage
- I/O and CPU control beyond rlimits

---

## Phase 6: OCI Compliance & Lifecycle 🟡 INTEROPERABILITY

**Priority:** ⭐⭐ Medium
**Complexity:** 🔴 High
**Impact:** 🔥 Medium - Enables ecosystem integration
**Why Sixth:** Nice to have, not critical for basic use

### Overview

**Goal:** OCI Runtime Specification compliance

**Why Important:**
- Interoperability with Docker, Podman, Kubernetes
- Standard config format
- Ecosystem compatibility

**Why Not Earlier:**
- OCI spec is large and complex
- Can work without it (custom API)
- Most value comes from earlier phases

**OCI Runtime Spec Includes:**
- config.json format
- Bundle format (rootfs + config)
- Lifecycle operations (create, start, kill, delete)
- Hooks system
- State management

### Features to Implement

#### 6.1 OCI config.json Parsing ⭐⭐

**What:** Read OCI configuration files

**Scope:**
```rust
let config = OciConfig::from_file("config.json")?;
let child = Command::from_oci_config(&config)?.spawn()?;
```

**Complexity:** 🟡 Medium
- JSON parsing (serde)
- Map OCI spec to our API
- Handle all config options

**Useful For:**
- ✅ OCI bundle compatibility
- ✅ Standard config format
- ✅ Tool interoperability

**Usefulness Score:** 7/10 - Good for interop

---

#### 6.2 Container Lifecycle ⭐⭐

**What:** Full create/start/kill/delete lifecycle

**Current:**
```rust
// We only have spawn (create+start combined)
let child = Command::new("/bin/sh").spawn()?;
child.wait()?;
```

**OCI Spec:**
```bash
runc create <id> <bundle>  # Create but don't start
runc start <id>             # Start the container
runc state <id>             # Get container state
runc kill <id> <signal>     # Send signal
runc delete <id>            # Remove container
```

**Scope:**
```rust
let container = Container::create(id, config)?;  // Created state
container.start()?;                               // Running state
container.kill(Signal::TERM)?;                    // Send signal
container.wait()?;                                // Wait for exit
container.delete()?;                              // Remove
```

**Complexity:** 🔴 High
- Need state management
- Need persistent state storage
- Need two-phase startup

**Useful For:**
- ✅ OCI compliance
- ✅ Container management tools
- ✅ Orchestration

**Usefulness Score:** 6/10 - Nice for tools, not end users

---

#### 6.3 Hooks System ⭐

**What:** Execute hooks at lifecycle events

**OCI Hooks:**
- prestart - After container created, before started
- createRuntime - After namespaces created
- createContainer - In container, before pivot_root
- startContainer - In container, before exec
- poststart - After container started
- poststop - After container deleted

**Scope:**
```rust
let child = Command::new("/bin/sh")
    .with_hook_prestart("/path/to/hook.sh")
    .spawn()?;
```

**Complexity:** 🟡 Medium
- Execute programs at specific points
- Pass container state to hooks

**Useful For:**
- ✅ Custom setup logic
- ✅ Integration with external tools
- ✅ Monitoring/logging

**Usefulness Score:** 5/10 - Specialized use cases

---

### Phase 6 Deliverables

**New API:**
```rust
OciConfig::from_file(path)
Command::from_oci_config(config)
Container::create(id, config)
Container::start()
Container::state()
Container::kill(signal)
Container::delete()
```

**Dependencies:**
```toml
serde_json = "1.0"
```

**Tests:**
- Parse OCI config.json
- Lifecycle operations work
- Hooks execute at right times
- State management accurate

**Success Criteria:**
- ✅ Can run OCI bundles
- ✅ Compatible with OCI tools
- ✅ Lifecycle management works

**After Phase 6:**
- OCI-compliant runtime
- Can integrate with Kubernetes, Docker, etc.
- Standard config format

---

## Phase 7: Advanced Features 🟢 PRODUCTION POLISH

**Priority:** ⭐ Low
**Complexity:** 🔴 Very High
**Impact:** Variable
**Why Last:** Nice to have, not essential

### Features

#### 7.1 Rootless Containers ⭐⭐

**What:** Run containers as non-root user

**Complexity:** 🔴 Very High
- USER namespace complexities
- Subuid/subgid mapping
- Unprivileged cgroups
- File ownership mapping

**Usefulness:** 8/10 for security-conscious environments

---

#### 7.2 Checkpoint/Restore (CRIU) ⭐

**What:** Save and restore container state

**Complexity:** 🔴 Very High
- CRIU integration
- File descriptor handling
- Network state preservation

**Usefulness:** 5/10 - Very specialized

---

#### 7.3 AppArmor/SELinux ⭐⭐

**What:** Mandatory Access Control integration

**Complexity:** 🟡 Medium
- Load security profiles
- Apply to container

**Usefulness:** 7/10 for high-security environments

---

## Summary & Recommendations

### Recommended Order

**Immediate (Weeks 1-4):**
1. Phase 1: Security Hardening
   - Blocks production use without this
   - Relatively quick wins

**Short-term (Weeks 4-12):**
2. Phase 2: Interactive Containers
   - High value for developers
   - Moderate complexity

3. Phase 3: Networking
   - Essential but hard
   - Consider CNI delegation to simplify

**Medium-term (Months 3-6):**
4. Phase 4: Filesystem Flexibility
   - Common use cases
   - Straightforward implementation

5. Phase 5: Advanced Resources
   - Nice upgrade from rlimits
   - Leverage existing dependency

**Long-term (Months 6-12):**
6. Phase 6: OCI Compliance
   - Interoperability
   - Complex but valuable

7. Phase 7: Advanced Features
   - As needed
   - Specialized use cases

### Complexity Assessment

**Easy (Quick Implementation):**
- Read-only rootfs
- Masked paths
- No-new-privileges
- tmpfs mounts
- DNS configuration

**Medium (Moderate Effort):**
- Seccomp filtering
- Signal handling
- Bind mounts
- Volume management
- cgroups integration

**Hard (Significant Effort):**
- TTY/PTY support
- Network namespace setup
- veth pairs
- OCI config parsing

**Very Hard (Major Undertaking):**
- Full networking stack
- Exec into container
- Container lifecycle
- Rootless mode
- CRIU integration

### Impact vs Effort Matrix

```
High Impact, Low Effort:
├─ Read-only rootfs
├─ Masked paths
├─ No-new-privileges
├─ tmpfs mounts
└─ Bind mounts

High Impact, Medium Effort:
├─ Seccomp filtering
├─ Signal handling
└─ Volume management

High Impact, High Effort:
├─ TTY/PTY support
├─ Network namespace + veth
└─ DNS configuration

Medium Impact, Low Effort:
├─ DNS configuration
└─ Resource stats

Medium Impact, Medium Effort:
├─ cgroups integration
└─ OCI config parsing

Low Impact, High Effort:
├─ Checkpoint/Restore
└─ Full OCI lifecycle
```

### Reality Check

**After Phase 1-3 (Security + Interactive + Networking):**
- ~70% feature parity with runc
- Production-ready for most use cases
- Usable by developers
- Secure enough for real workloads

**After Phase 1-6:**
- ~90% feature parity with runc
- OCI-compliant
- Full-featured runtime
- Can integrate with orchestration

**What We'll Never Have (Out of Scope):**
- Image building (use buildah, docker build)
- Image registry (use skopeo, docker push/pull)
- Orchestration (use Kubernetes, Swarm)
- Multi-container apps (use compose)

### Final Recommendation

**Minimum Viable Production Runtime:**
- Phase 1: Security ✅ MUST HAVE
- Phase 2: Interactive ✅ MUST HAVE
- Phase 3: Networking ✅ MUST HAVE
- Phase 4: Filesystems ⚠️ HIGHLY RECOMMENDED

**Everything else is optional** depending on use case.

Focus on Phases 1-3 first. That gives you a secure, usable, networked container runtime - which is 80% of the value.

---

**Last Updated:** 2026-02-16
**Status:** Planning document
**Next Step:** Begin Phase 1 - Security Hardening
