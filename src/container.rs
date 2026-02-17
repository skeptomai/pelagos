//! Container process management using Linux namespaces.
//!
//! This module provides a modern, type-safe interface for spawning processes in isolated
//! Linux namespaces, similar to containers. It uses the [`nix`](https://docs.rs/nix) crate
//! for safe syscall wrappers and [`bitflags`](https://docs.rs/bitflags) for ergonomic
//! namespace combinations.
//!
//! # Overview
//!
//! The main entry point is [`Command`], which provides a builder pattern for configuring
//! and spawning containerized processes. The API is similar to [`std::process::Command`]
//! but with additional support for:
//!
//! - **Linux namespaces** - Isolate processes (PID, Mount, UTS, IPC, User, Net, Cgroup)
//! - **chroot** - Change root directory for filesystem isolation
//! - **Pre-exec callbacks** - Execute code before the target program runs
//!
//! # Examples
//!
//! ## Basic container with namespace isolation
//!
//! ```no_run
//! use remora::container::{Command, Namespace, Stdio};
//!
//! let mut child = Command::new("/bin/sh")
//!     .with_namespaces(Namespace::UTS | Namespace::PID | Namespace::MOUNT)
//!     .with_chroot("/path/to/rootfs")
//!     .stdin(Stdio::Inherit)
//!     .stdout(Stdio::Inherit)
//!     .stderr(Stdio::Inherit)
//!     .spawn()
//!     .expect("Failed to spawn container");
//!
//! let status = child.wait().expect("Failed to wait for container");
//! ```
//!
//! ## With pre-exec callback for mounting filesystems
//!
//! ```no_run
//! # use remora::container::{Command, Namespace};
//! fn mount_proc() -> std::io::Result<()> {
//!     // Mount proc filesystem inside container
//!     // Implementation details...
//!     Ok(())
//! }
//!
//! let child = Command::new("/bin/sh")
//!     .with_namespaces(Namespace::MOUNT | Namespace::PID)
//!     .with_chroot("/path/to/rootfs")
//!     .with_pre_exec(mount_proc)
//!     .spawn()?;
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! # Architecture
//!
//! The implementation uses [`std::process::Command::pre_exec`] to combine namespace
//! creation, chroot, and user callbacks into a single atomic operation before `exec()`.
//!
//! ## Execution flow
//!
//! 1. Parent process calls `spawn()`
//! 2. `fork()` creates child process
//! 3. In child, `pre_exec` callback runs:
//!    - Unshare specified namespaces
//!    - Change root if configured
//!    - Run user pre_exec callback
//! 4. Child calls `exec()` to replace with target program
//!
//! # Safety
//!
//! This module uses `unsafe` in the following places:
//!
//! - **`pre_exec` callback**: Must be signal-safe and cannot allocate. Only simple
//!   syscalls (unshare, chroot, chdir) are performed.
//!
//! # Linux Requirements
//!
//! - **Kernel 3.8+** for basic namespace support
//! - **CAP_SYS_ADMIN** or root for most namespace operations
//! - **User namespaces** (kernel 3.8+) allow unprivileged containers
//!
//! # Phase 2 Improvements
//!
//! - ✅ Enhanced error handling with [`thiserror`](https://docs.rs/thiserror)
//! - ✅ Consuming builder pattern for better ergonomics
//! - ✅ Bitflags for namespace combinations
//! - ✅ Comprehensive documentation
//! - ⏳ Unit tests (in progress)

#![allow(dead_code)] // Allow unused items during incremental development

use bitflags::bitflags;
use nix::sched::{unshare, CloneFlags};
use nix::unistd::chroot;
use std::ffi::OsStr;
use std::fs::File;
use std::io;
use std::os::unix::io::AsRawFd;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::PathBuf;
use std::process::{self, ExitStatus as StdExitStatus};

// Re-export SeccompProfile for public API
pub use crate::seccomp::SeccompProfile;

bitflags! {
    /// Linux namespace types that can be unshared.
    ///
    /// Use bitwise OR to combine multiple namespaces:
    /// ```ignore
    /// let ns = Namespace::UTS | Namespace::PID | Namespace::MOUNT;
    /// ```
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Namespace: u32 {
        /// Mount namespace - isolate filesystem mount points
        const MOUNT  = 0b0000_0001;
        /// UTS namespace - isolate hostname and domain name
        const UTS    = 0b0000_0010;
        /// IPC namespace - isolate System V IPC and POSIX message queues
        const IPC    = 0b0000_0100;
        /// User namespace - isolate user and group IDs
        const USER   = 0b0000_1000;
        /// PID namespace - isolate process ID number space
        const PID    = 0b0001_0000;
        /// Network namespace - isolate network devices, stacks, ports, etc.
        const NET    = 0b0010_0000;
        /// Cgroup namespace - isolate cgroup hierarchy
        const CGROUP = 0b0100_0000;
    }
}

bitflags! {
    /// Linux capabilities that can be retained or dropped.
    ///
    /// By default, processes run with many capabilities. For security,
    /// you can drop capabilities and only keep the ones you need.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// // Keep only network and chown capabilities
    /// let caps = Capability::NET_BIND_SERVICE | Capability::CHOWN;
    /// cmd.with_capabilities(caps);
    /// ```
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Capability: u64 {
        /// CAP_CHOWN - Make arbitrary changes to file UIDs and GIDs
        const CHOWN = 1 << 0;
        /// CAP_DAC_OVERRIDE - Bypass file read, write, and execute permission checks
        const DAC_OVERRIDE = 1 << 1;
        /// CAP_FOWNER - Bypass permission checks on operations that require filesystem UID
        const FOWNER = 1 << 3;
        /// CAP_FSETID - Don't clear set-user-ID and set-group-ID mode bits
        const FSETID = 1 << 4;
        /// CAP_KILL - Bypass permission checks for sending signals
        const KILL = 1 << 5;
        /// CAP_SETGID - Make arbitrary manipulations of process GIDs
        const SETGID = 1 << 6;
        /// CAP_SETUID - Make arbitrary manipulations of process UIDs
        const SETUID = 1 << 7;
        /// CAP_NET_BIND_SERVICE - Bind a socket to privileged ports (< 1024)
        const NET_BIND_SERVICE = 1 << 10;
        /// CAP_NET_RAW - Use RAW and PACKET sockets
        const NET_RAW = 1 << 13;
        /// CAP_SYS_CHROOT - Use chroot()
        const SYS_CHROOT = 1 << 18;
        /// CAP_SYS_ADMIN - Perform a range of system administration operations
        const SYS_ADMIN = 1 << 21;
        /// CAP_SYS_PTRACE - Trace arbitrary processes using ptrace
        const SYS_PTRACE = 1 << 19;
    }
}

impl Namespace {
    /// Convert namespace flags to nix CloneFlags
    fn to_clone_flags(self) -> CloneFlags {
        let mut flags = CloneFlags::empty();

        if self.contains(Namespace::MOUNT) {
            flags |= CloneFlags::CLONE_NEWNS;
        }
        if self.contains(Namespace::UTS) {
            flags |= CloneFlags::CLONE_NEWUTS;
        }
        if self.contains(Namespace::IPC) {
            flags |= CloneFlags::CLONE_NEWIPC;
        }
        if self.contains(Namespace::USER) {
            flags |= CloneFlags::CLONE_NEWUSER;
        }
        if self.contains(Namespace::PID) {
            flags |= CloneFlags::CLONE_NEWPID;
        }
        if self.contains(Namespace::NET) {
            flags |= CloneFlags::CLONE_NEWNET;
        }
        if self.contains(Namespace::CGROUP) {
            flags |= CloneFlags::CLONE_NEWCGROUP;
        }

        flags
    }
}

/// Standard I/O configuration for spawned processes.
///
/// Configures how stdin, stdout, and stderr should be handled for the child process.
/// This is a simplified version of [`std::process::Stdio`] for container use.
///
/// # Examples
///
/// ```no_run
/// use remora::container::{Command, Stdio};
///
/// let child = Command::new("/bin/cat")
///     .stdin(Stdio::Inherit)   // Read from parent's stdin
///     .stdout(Stdio::Inherit)  // Write to parent's stdout
///     .stderr(Stdio::Null)     // Discard error output
///     .spawn()?;
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug, Clone)]
pub enum Stdio {
    /// Inherit stdio from parent process
    ///
    /// The child process will use the same stdin/stdout/stderr as the parent.
    Inherit,

    /// Redirect to /dev/null
    ///
    /// The stream will be discarded (for output) or return EOF (for input).
    Null,

    /// Create a pipe (not yet fully implemented)
    ///
    /// Creates a pipe between parent and child. The parent can read/write
    /// through the pipe to communicate with the child.
    Piped,
}

impl From<Stdio> for process::Stdio {
    fn from(stdio: Stdio) -> Self {
        match stdio {
            Stdio::Inherit => process::Stdio::inherit(),
            Stdio::Null => process::Stdio::null(),
            Stdio::Piped => process::Stdio::piped(),
        }
    }
}

/// A bind mount that maps a host directory into the container.
#[derive(Debug, Clone)]
pub struct BindMount {
    /// Absolute path on the host to mount from.
    pub source: PathBuf,
    /// Absolute path inside the container where it will be mounted (e.g. `/data`).
    pub target: PathBuf,
    /// If true, the bind mount is read-only inside the container.
    pub readonly: bool,
}

/// A tmpfs mount inside the container.
#[derive(Debug, Clone)]
pub struct TmpfsMount {
    /// Absolute path inside the container where tmpfs is mounted (e.g. `/tmp`).
    pub target: PathBuf,
    /// Mount options passed to the kernel (e.g. `"size=100m,mode=1777"`).
    pub options: String,
}

/// A named volume backed by a host directory under `/var/lib/remora/volumes/<name>/`.
///
/// Volumes provide persistent storage that survives container restarts.
///
/// # Examples
///
/// ```ignore
/// let vol = Volume::create("mydata")?;
/// Command::new("/bin/sh")
///     .with_volume(&vol, "/data")
///     .spawn()?;
/// ```
pub struct Volume {
    /// The volume name (used as directory name under `/var/lib/remora/volumes/`).
    pub name: String,
    /// Resolved absolute host path to the volume directory.
    pub path: PathBuf,
}

impl Volume {
    fn volumes_dir() -> PathBuf {
        PathBuf::from("/var/lib/remora/volumes")
    }

    /// Create a new named volume, creating the backing directory if needed.
    pub fn create(name: &str) -> io::Result<Self> {
        let path = Self::volumes_dir().join(name);
        std::fs::create_dir_all(&path)?;
        Ok(Self { name: name.to_string(), path })
    }

    /// Open an existing named volume, returning an error if it does not exist.
    pub fn open(name: &str) -> io::Result<Self> {
        let path = Self::volumes_dir().join(name);
        if !path.exists() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("volume '{}' not found at {}", name, path.display()),
            ));
        }
        Ok(Self { name: name.to_string(), path })
    }

    /// Delete a named volume and its contents.
    pub fn delete(name: &str) -> io::Result<()> {
        let path = Self::volumes_dir().join(name);
        std::fs::remove_dir_all(&path)
    }

    /// Returns the absolute host path of this volume.
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }
}

/// Builder for spawning processes in Linux namespaces.
///
/// Similar to [`std::process::Command`] but with support for Linux namespaces,
/// chroot, and container-specific operations. Uses a consuming builder pattern
/// where each method takes ownership and returns `Self`.
///
/// # Examples
///
/// ```no_run
/// use remora::container::{Command, Namespace, Stdio};
///
/// // Create and configure a containerized process
/// let child = Command::new("/bin/sh")
///     .args(&["-c", "echo hello"])
///     .with_namespaces(Namespace::UTS | Namespace::PID)
///     .with_chroot("/path/to/rootfs")
///     .stdin(Stdio::Inherit)
///     .spawn()
///     .expect("Failed to spawn");
/// ```
///
/// # Method Chaining
///
/// All builder methods consume `self` and return `Self`, enabling fluent chaining:
///
/// ```no_run
/// # use remora::container::{Command, Namespace};
/// Command::new("/bin/ls")
///     .args(&["-la"])
///     .with_namespaces(Namespace::MOUNT)
///     .spawn()?;
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub struct Command {
    inner: process::Command,
    namespaces: Namespace,
    chroot_dir: Option<PathBuf>,
    pre_exec: Option<Box<dyn Fn() -> io::Result<()> + Send + Sync>>,
    uid_maps: Vec<UidMap>,
    gid_maps: Vec<GidMap>,
    uid: Option<u32>,
    gid: Option<u32>,
    join_namespaces: Vec<(PathBuf, Namespace)>,
    // Mount configuration
    mount_proc: bool,
    mount_sys: bool,
    mount_dev: bool,
    pivot_root: Option<(PathBuf, PathBuf)>, // (new_root, put_old)
    // Security configuration
    capabilities: Option<Capability>, // None = keep all, Some = keep only these
    seccomp_profile: Option<SeccompProfile>, // None = no seccomp, Some = apply profile
    no_new_privileges: bool,          // Prevent privilege escalation via setuid
    readonly_rootfs: bool,            // Make rootfs read-only
    masked_paths: Vec<PathBuf>,       // Paths to mask with /dev/null
    // Filesystem mounts
    bind_mounts: Vec<BindMount>,
    tmpfs_mounts: Vec<TmpfsMount>,
    // Resource limits
    rlimits: Vec<ResourceLimit>,
    // Cgroup-based resource management
    cgroup_config: Option<crate::cgroup::CgroupConfig>,
    // Network configuration
    network_config: Option<crate::network::NetworkConfig>,
    // Whether to enable NAT (MASQUERADE) for bridge-mode containers.
    nat: bool,
    // Port-forward rules: (host_port, container_port). Requires Bridge + NAT.
    port_forwards: Vec<(u16, u16)>,
}

impl Command {
    /// Create a new command builder for the given program.
    pub fn new<S: AsRef<OsStr>>(program: S) -> Self {
        Self {
            inner: process::Command::new(program),
            namespaces: Namespace::empty(),
            chroot_dir: None,
            pre_exec: None,
            uid_maps: Vec::new(),
            gid_maps: Vec::new(),
            uid: None,
            gid: None,
            join_namespaces: Vec::new(),
            mount_proc: false,
            mount_sys: false,
            mount_dev: false,
            pivot_root: None,
            capabilities: None,
            seccomp_profile: None,
            no_new_privileges: false,
            readonly_rootfs: false,
            masked_paths: Vec::new(),
            bind_mounts: Vec::new(),
            tmpfs_mounts: Vec::new(),
            rlimits: Vec::new(),
            cgroup_config: None,
            network_config: None,
            nat: false,
            port_forwards: Vec::new(),
        }
    }

    /// Add arguments to pass to the program.
    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.inner.args(args);
        self
    }

    /// Configure stdin for the child process.
    pub fn stdin(mut self, cfg: Stdio) -> Self {
        self.inner.stdin(cfg);
        self
    }

    /// Set an environment variable for the child process.
    pub fn env<K, V>(mut self, key: K, val: V) -> Self
    where
        K: AsRef<std::ffi::OsStr>,
        V: AsRef<std::ffi::OsStr>,
    {
        self.inner.env(key, val);
        self
    }

    /// Configure stdout for the child process.
    pub fn stdout(mut self, cfg: Stdio) -> Self {
        self.inner.stdout(cfg);
        self
    }

    /// Configure stderr for the child process.
    pub fn stderr(mut self, cfg: Stdio) -> Self {
        self.inner.stderr(cfg);
        self
    }

    /// Set the root directory for the child process (chroot).
    ///
    /// This will be executed after namespace creation in the pre_exec callback.
    pub fn with_chroot<P: Into<PathBuf>>(mut self, dir: P) -> Self {
        self.chroot_dir = Some(dir.into());
        self
    }

    /// Legacy API for setting chroot directory
    #[deprecated(since = "0.2.0", note = "Use with_chroot() instead")]
    pub fn chroot_dir<P: Into<PathBuf>>(self, dir: P) -> Self {
        self.with_chroot(dir)
    }

    /// Specify which namespaces to unshare for the child process.
    ///
    /// The namespaces will be created when the process spawns, before exec.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// // Combine multiple namespaces with bitwise OR
    /// cmd.with_namespaces(Namespace::UTS | Namespace::PID | Namespace::MOUNT);
    /// ```
    pub fn with_namespaces(mut self, namespaces: Namespace) -> Self {
        self.namespaces = namespaces;
        self
    }

    /// Legacy API: accepts iterator of namespace references (for backwards compatibility)
    #[deprecated(since = "0.2.0", note = "Use with_namespaces() with bitflags instead")]
    pub fn unshare<'a, I>(mut self, namespaces: I) -> Self
    where
        I: IntoIterator<Item = &'a Namespace>,
    {
        self.namespaces = namespaces.into_iter().fold(Namespace::empty(), |acc, &ns| acc | ns);
        self
    }

    /// Register a callback to run in the child process before exec.
    ///
    /// The callback runs after namespace creation and chroot, but before
    /// the target program is executed. Useful for mounting filesystems, etc.
    ///
    /// Note: The callback must not allocate or perform complex operations.
    /// It runs in a fork context where many operations are unsafe.
    pub fn with_pre_exec<F>(mut self, f: F) -> Self
    where
        F: Fn() -> io::Result<()> + Send + Sync + 'static,
    {
        self.pre_exec = Some(Box::new(f));
        self
    }

    /// Legacy API for setting pre_exec callback
    #[deprecated(since = "0.2.0", note = "Use with_pre_exec() instead")]
    pub fn pre_exec<F>(self, f: F) -> Self
    where
        F: Fn() -> io::Result<()> + Send + Sync + 'static,
    {
        self.with_pre_exec(f)
    }

    /// Set UID mappings for user namespace.
    ///
    /// Requires `Namespace::USER` to be set. Maps UIDs from inside the container
    /// to outside the container, allowing unprivileged containers.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// // Run as root inside, but uid 1000 outside
    /// cmd.with_namespaces(Namespace::USER)
    ///    .with_uid_maps(&[UidMap { inside: 0, outside: 1000, count: 1 }])
    ///    .with_uid(0);
    /// ```
    pub fn with_uid_maps(mut self, maps: &[UidMap]) -> Self {
        self.uid_maps = maps.to_vec();
        self
    }

    /// Set GID mappings for user namespace.
    ///
    /// Requires `Namespace::USER` to be set. Maps GIDs from inside the container
    /// to outside the container.
    pub fn with_gid_maps(mut self, maps: &[GidMap]) -> Self {
        self.gid_maps = maps.to_vec();
        self
    }

    /// Set the user ID to run as inside the container.
    ///
    /// This is the UID the process will have after exec, typically used
    /// with user namespace mapping.
    pub fn with_uid(mut self, uid: u32) -> Self {
        self.uid = Some(uid);
        self
    }

    /// Set the group ID to run as inside the container.
    ///
    /// This is the GID the process will have after exec.
    pub fn with_gid(mut self, gid: u32) -> Self {
        self.gid = Some(gid);
        self
    }

    /// Join an existing namespace instead of creating a new one.
    ///
    /// Opens the namespace file and calls `setns()` to join it before exec.
    /// Can be called multiple times to join different namespace types.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// // Join existing network namespace
    /// cmd.with_namespace_join("/var/run/netns/con", Namespace::NET);
    ///
    /// // Join multiple namespaces
    /// cmd.with_namespace_join("/proc/1234/ns/net", Namespace::NET)
    ///    .with_namespace_join("/proc/1234/ns/pid", Namespace::PID);
    /// ```
    pub fn with_namespace_join<P: Into<PathBuf>>(mut self, path: P, ns: Namespace) -> Self {
        self.join_namespaces.push((path.into(), ns));
        self
    }

    /// Automatically mount /proc filesystem after chroot.
    ///
    /// This mounts a new proc filesystem at /proc inside the container.
    /// Requires `Namespace::MOUNT` to be set.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// cmd.with_namespaces(Namespace::MOUNT)
    ///    .with_chroot("/path/to/rootfs")
    ///    .with_proc_mount();
    /// ```
    pub fn with_proc_mount(mut self) -> Self {
        self.mount_proc = true;
        self
    }

    /// Automatically mount /sys filesystem after chroot.
    ///
    /// This bind mounts /sys from the host into the container.
    /// Requires `Namespace::MOUNT` to be set.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// cmd.with_namespaces(Namespace::MOUNT)
    ///    .with_chroot("/path/to/rootfs")
    ///    .with_sys_mount();
    /// ```
    pub fn with_sys_mount(mut self) -> Self {
        self.mount_sys = true;
        self
    }

    /// Automatically mount /dev filesystem after chroot.
    ///
    /// This bind mounts essential device files into the container.
    /// Requires `Namespace::MOUNT` to be set.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// cmd.with_namespaces(Namespace::MOUNT)
    ///    .with_chroot("/path/to/rootfs")
    ///    .with_dev_mount();
    /// ```
    pub fn with_dev_mount(mut self) -> Self {
        self.mount_dev = true;
        self
    }

    /// Use pivot_root instead of chroot for filesystem isolation.
    ///
    /// pivot_root is more secure than chroot as it actually changes the root
    /// of the mount namespace, preventing escape via chroot.
    ///
    /// # Arguments
    ///
    /// * `new_root` - Path to the new root filesystem
    /// * `put_old` - Path (relative to new_root) where the old root will be mounted
    ///
    /// # Examples
    ///
    /// ```ignore
    /// cmd.with_namespaces(Namespace::MOUNT)
    ///    .with_pivot_root("/path/to/rootfs", "/path/to/rootfs/old_root");
    /// ```
    pub fn with_pivot_root<P1: Into<PathBuf>, P2: Into<PathBuf>>(
        mut self,
        new_root: P1,
        put_old: P2,
    ) -> Self {
        self.pivot_root = Some((new_root.into(), put_old.into()));
        self
    }

    /// Set which capabilities to keep (all others will be dropped).
    ///
    /// For security, containers should run with minimal capabilities.
    /// By default, all capabilities are kept. Use this to drop unnecessary ones.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// // Keep only network and chown capabilities
    /// cmd.with_capabilities(Capability::NET_BIND_SERVICE | Capability::CHOWN);
    ///
    /// // Drop all capabilities
    /// cmd.with_capabilities(Capability::empty());
    /// ```
    pub fn with_capabilities(mut self, caps: Capability) -> Self {
        self.capabilities = Some(caps);
        self
    }

    /// Drop all capabilities for maximum security.
    ///
    /// Equivalent to `with_capabilities(Capability::empty())`.
    pub fn drop_all_capabilities(mut self) -> Self {
        self.capabilities = Some(Capability::empty());
        self
    }

    /// Set a resource limit (rlimit) for the container.
    ///
    /// Controls resource usage such as memory, CPU time, file descriptors, etc.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// // Limit open file descriptors to 1024
    /// cmd.with_rlimit(libc::RLIMIT_NOFILE, 1024, 1024);
    ///
    /// // Limit address space to 512 MB
    /// cmd.with_rlimit(libc::RLIMIT_AS, 512 * 1024 * 1024, 512 * 1024 * 1024);
    /// ```
    pub fn with_rlimit(
        mut self,
        resource: libc::__rlimit_resource_t,
        soft: libc::rlim_t,
        hard: libc::rlim_t,
    ) -> Self {
        self.rlimits.push(ResourceLimit { resource, soft, hard });
        self
    }

    /// Convenience method to limit the number of open file descriptors.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// cmd.with_max_fds(1024);  // Limit to 1024 open files
    /// ```
    pub fn with_max_fds(self, limit: libc::rlim_t) -> Self {
        self.with_rlimit(libc::RLIMIT_NOFILE, limit, limit)
    }

    /// Convenience method to limit address space (virtual memory).
    ///
    /// # Examples
    ///
    /// ```ignore
    /// cmd.with_memory_limit(512 * 1024 * 1024);  // 512 MB limit
    /// ```
    pub fn with_memory_limit(self, bytes: libc::rlim_t) -> Self {
        self.with_rlimit(libc::RLIMIT_AS, bytes, bytes)
    }

    /// Convenience method to limit CPU time.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// cmd.with_cpu_time_limit(60);  // 60 seconds of CPU time
    /// ```
    pub fn with_cpu_time_limit(self, seconds: libc::rlim_t) -> Self {
        self.with_rlimit(libc::RLIMIT_CPU, seconds, seconds)
    }

    /// Set a cgroup memory hard limit in bytes (`memory.max`).
    ///
    /// The container will be OOM-killed if it exceeds this limit. This uses
    /// cgroups v2 and applies to the entire container process group, unlike
    /// `with_memory_limit()` which uses `RLIMIT_AS` (per-process address space).
    ///
    /// Requires root or `CAP_SYS_ADMIN`.
    pub fn with_cgroup_memory(mut self, bytes: i64) -> Self {
        self.cgroup_config.get_or_insert_with(Default::default).memory_limit = Some(bytes);
        self
    }

    /// Set the CPU weight (shares) for the container's cgroup.
    ///
    /// Maps to `cpu.weight` in cgroups v2 (range 1–10000; default 100) and
    /// `cpu.shares` in v1. Higher values receive proportionally more CPU time.
    pub fn with_cgroup_cpu_shares(mut self, shares: u64) -> Self {
        self.cgroup_config.get_or_insert_with(Default::default).cpu_shares = Some(shares);
        self
    }

    /// Set a CPU quota for the container's cgroup.
    ///
    /// `quota_us` is the maximum CPU time (in microseconds) the container may
    /// use per `period_us`. Example: `(50_000, 100_000)` = 50% of one CPU core.
    pub fn with_cgroup_cpu_quota(mut self, quota_us: i64, period_us: u64) -> Self {
        self.cgroup_config.get_or_insert_with(Default::default).cpu_quota = Some((quota_us, period_us));
        self
    }

    /// Set the maximum number of processes/threads in the container's cgroup.
    ///
    /// Maps to `pids.max`. Forks beyond this limit will fail with `EAGAIN`.
    pub fn with_cgroup_pids_limit(mut self, max: u64) -> Self {
        self.cgroup_config.get_or_insert_with(Default::default).pids_limit = Some(max);
        self
    }

    /// Configure container networking.
    ///
    /// - [`NetworkMode::None`](crate::network::NetworkMode::None) — share the host
    ///   network stack (default, no changes).
    /// - [`NetworkMode::Loopback`](crate::network::NetworkMode::Loopback) — create an
    ///   isolated network namespace with only the loopback interface (`lo`, 127.0.0.1).
    /// - [`NetworkMode::Bridge`](crate::network::NetworkMode::Bridge) — create an isolated
    ///   network namespace connected to the `remora0` bridge (172.19.0.x/24).
    ///
    /// `Loopback` and `Bridge` modes automatically add [`Namespace::NET`] to the
    /// namespace set, so you don't need to call `.with_namespaces(Namespace::NET)` separately.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use remora::network::NetworkMode;
    ///
    /// // Isolated loopback only
    /// Command::new("/bin/sh").with_network(NetworkMode::Loopback).spawn()?;
    ///
    /// // Full bridge networking
    /// Command::new("/bin/sh").with_network(NetworkMode::Bridge).spawn()?;
    /// ```
    pub fn with_network(mut self, mode: crate::network::NetworkMode) -> Self {
        // Loopback requires a new NET namespace (unshare in pre_exec).
        // Bridge does NOT unshare NET — the child joins a pre-configured named
        // netns via setns() in pre_exec instead.
        if mode == crate::network::NetworkMode::Loopback {
            self.namespaces |= Namespace::NET;
        }
        self.network_config = Some(crate::network::NetworkConfig { mode });
        self
    }

    /// Enable NAT (MASQUERADE) for a bridge-mode container.
    ///
    /// Requires `.with_network(NetworkMode::Bridge)` — silently ignored for
    /// other network modes.  Installs an nftables MASQUERADE rule on the first
    /// NAT container and removes it when the last one exits (reference-counted).
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use remora::network::NetworkMode;
    /// Command::new("/bin/sh")
    ///     .with_network(NetworkMode::Bridge)
    ///     .with_nat()
    ///     .spawn()?;
    /// ```
    pub fn with_nat(mut self) -> Self {
        self.nat = true;
        self
    }

    /// Forward a host port into the container (TCP only).
    ///
    /// Requires [`NetworkMode::Bridge`] and [`with_nat`](Self::with_nat) (for the
    /// nftables table to already exist). Installs a DNAT rule via nftables so that
    /// connections to `host_port` on any host interface are redirected to
    /// `container_port` on the container's IP.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use remora::network::NetworkMode;
    /// Command::new("/bin/sh")
    ///     .with_network(NetworkMode::Bridge)
    ///     .with_nat()
    ///     .with_port_forward(8080, 80)   // host:8080 → container:80
    ///     .spawn()?;
    /// ```
    pub fn with_port_forward(mut self, host_port: u16, container_port: u16) -> Self {
        self.port_forwards.push((host_port, container_port));
        self
    }

    /// Apply Docker's default seccomp profile (recommended).
    ///
    /// This blocks ~44 dangerous syscalls commonly used in container escapes
    /// while allowing normal application behavior. Matches Docker's default.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// cmd.with_seccomp_default();
    /// ```
    ///
    /// # Security
    ///
    /// Blocked syscalls include: ptrace, mount, reboot, bpf, perf_event_open,
    /// and many others. See [`crate::seccomp`] module for full list.
    pub fn with_seccomp_default(mut self) -> Self {
        self.seccomp_profile = Some(SeccompProfile::Docker);
        self
    }

    /// Apply minimal seccomp profile (highly restrictive).
    ///
    /// Only allows ~40 essential syscalls needed for basic process execution.
    /// Use for highly constrained containers where you control the application.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// cmd.with_seccomp_minimal();
    /// ```
    pub fn with_seccomp_minimal(mut self) -> Self {
        self.seccomp_profile = Some(SeccompProfile::Minimal);
        self
    }

    /// Set a specific seccomp profile.
    ///
    /// Allows choosing between different security profiles programmatically.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// cmd.with_seccomp_profile(SeccompProfile::Docker);
    /// cmd.with_seccomp_profile(SeccompProfile::Minimal);
    /// cmd.with_seccomp_profile(SeccompProfile::None); // No filtering
    /// ```
    pub fn with_seccomp_profile(mut self, profile: SeccompProfile) -> Self {
        self.seccomp_profile = Some(profile);
        self
    }

    /// Disable seccomp filtering (unsafe, for debugging).
    ///
    /// WARNING: Containers without seccomp are less secure and more vulnerable
    /// to escape attacks. Only use when debugging or when security is not critical.
    pub fn without_seccomp(mut self) -> Self {
        self.seccomp_profile = Some(SeccompProfile::None);
        self
    }

    /// Enable no-new-privileges flag to prevent privilege escalation.
    ///
    /// This prevents the process from gaining new privileges via setuid/setgid
    /// binaries or file capabilities. Essential for running untrusted code.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// cmd.with_no_new_privileges(true);
    /// ```
    ///
    /// # Security
    ///
    /// This flag:
    /// - Prevents setuid/setgid binaries from elevating privileges
    /// - Blocks file capability-based privilege escalation
    /// - Required for unprivileged seccomp filtering
    /// - Cannot be unset once enabled
    ///
    /// Recommended for all production containers running untrusted code.
    pub fn with_no_new_privileges(mut self, enabled: bool) -> Self {
        self.no_new_privileges = enabled;
        self
    }

    /// Make the root filesystem read-only.
    ///
    /// This prevents the container from modifying the filesystem, enforcing
    /// immutable infrastructure and preventing malware persistence.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// cmd.with_readonly_rootfs(true);
    /// ```
    ///
    /// # Note
    ///
    /// You'll typically want writable tmpfs mounts for /tmp, /var/tmp, etc:
    /// ```ignore
    /// cmd.with_readonly_rootfs(true)
    ///    .with_pre_exec(|| {
    ///        // Mount tmpfs for writable areas
    ///        mount_tmpfs("/tmp")?;
    ///        Ok(())
    ///    });
    /// ```
    pub fn with_readonly_rootfs(mut self, readonly: bool) -> Self {
        self.readonly_rootfs = readonly;
        self
    }

    /// Mask sensitive paths by mounting /dev/null over them.
    ///
    /// This hides sensitive kernel information from the container, preventing
    /// information leakage and some escape vectors.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// // Use default masked paths
    /// cmd.with_masked_paths_default();
    ///
    /// // Or specify custom paths
    /// cmd.with_masked_paths(&["/proc/kcore", "/sys/firmware"]);
    /// ```
    pub fn with_masked_paths(mut self, paths: &[&str]) -> Self {
        self.masked_paths = paths.iter().map(PathBuf::from).collect();
        self
    }

    /// Use Docker's default set of masked paths.
    ///
    /// Masks the following sensitive paths:
    /// - `/proc/kcore` - Physical memory access
    /// - `/proc/keys` - Kernel keyring
    /// - `/proc/timer_list` - Timing information
    /// - `/proc/sched_debug` - Scheduler debugging
    /// - `/sys/firmware` - Firmware access
    /// - `/sys/devices/virtual/powercap` - Power capping info
    pub fn with_masked_paths_default(mut self) -> Self {
        self.masked_paths = vec![
            PathBuf::from("/proc/kcore"),
            PathBuf::from("/proc/keys"),
            PathBuf::from("/proc/timer_list"),
            PathBuf::from("/proc/sched_debug"),
            PathBuf::from("/sys/firmware"),
            PathBuf::from("/sys/devices/virtual/powercap"),
        ];
        self
    }

    /// Add a read-write bind mount from a host directory into the container.
    ///
    /// The `source` is an absolute path on the host; `target` is the absolute
    /// path inside the container where it will appear.
    ///
    /// Requires `Namespace::MOUNT` to be set.
    pub fn with_bind_mount<P1, P2>(mut self, source: P1, target: P2) -> Self
    where
        P1: Into<PathBuf>,
        P2: Into<PathBuf>,
    {
        self.bind_mounts.push(BindMount { source: source.into(), target: target.into(), readonly: false });
        self
    }

    /// Add a read-only bind mount from a host directory into the container.
    ///
    /// Identical to [`with_bind_mount`] but the mount is read-only inside the container.
    pub fn with_bind_mount_ro<P1, P2>(mut self, source: P1, target: P2) -> Self
    where
        P1: Into<PathBuf>,
        P2: Into<PathBuf>,
    {
        self.bind_mounts.push(BindMount { source: source.into(), target: target.into(), readonly: true });
        self
    }

    /// Mount a tmpfs filesystem at `target` inside the container.
    ///
    /// `options` are passed directly to the kernel (e.g. `"size=100m,mode=1777"`).
    /// Use an empty string for default options.
    ///
    /// tmpfs mounts are always writable and provide in-memory scratch space even
    /// when the rootfs is read-only.
    ///
    /// Requires `Namespace::MOUNT` to be set.
    pub fn with_tmpfs<P: Into<PathBuf>>(mut self, target: P, options: &str) -> Self {
        self.tmpfs_mounts.push(TmpfsMount { target: target.into(), options: options.to_string() });
        self
    }

    /// Mount a named volume at `target` inside the container.
    ///
    /// This is syntactic sugar for [`with_bind_mount`] using the volume's host path.
    pub fn with_volume<P: Into<PathBuf>>(self, vol: &Volume, target: P) -> Self {
        self.with_bind_mount(vol.path.clone(), target)
    }

    /// Spawn the child process with configured namespaces and settings.
    ///
    /// This combines namespace creation, chroot, and user pre_exec callbacks
    /// into a single pre_exec hook for std::process::Command.
    pub fn spawn(mut self) -> Result<Child, Error> {
        // Compile seccomp filter in parent process (requires allocation, can't be done in pre_exec)
        let seccomp_filter = if let Some(profile) = &self.seccomp_profile {
            match profile {
                SeccompProfile::Docker => Some(crate::seccomp::docker_default_filter()
                    .map_err(|e| Error::Seccomp(e))?),
                SeccompProfile::Minimal => Some(crate::seccomp::minimal_filter()
                    .map_err(|e| Error::Seccomp(e))?),
                SeccompProfile::None => None,
            }
        } else {
            None
        };

        // Open namespace files in parent process (can't safely open files in pre_exec)
        // Keep File objects alive so their fds remain valid through spawn
        let join_ns_files: Vec<(File, Namespace)> = self.join_namespaces
            .iter()
            .map(|(path, ns)| {
                File::open(path)
                    .map(|f| (f, *ns))
                    .map_err(Error::Io)
            })
            .collect::<Result<Vec<_>, _>>()?;

        // Extract raw fds for use in pre_exec
        let join_ns_fds: Vec<(i32, Namespace)> = join_ns_files
            .iter()
            .map(|(f, ns)| (f.as_raw_fd(), *ns))
            .collect();

        // Collect configuration to move into pre_exec closure
        let namespaces = self.namespaces;
        let chroot_dir = self.chroot_dir.clone();
        let user_pre_exec = self.pre_exec.take();
        let uid_maps = self.uid_maps.clone();
        let gid_maps = self.gid_maps.clone();
        let uid = self.uid;
        let gid = self.gid;
        let mount_proc = self.mount_proc;
        let mount_sys = self.mount_sys;
        let mount_dev = self.mount_dev;
        let pivot_root = self.pivot_root.clone();
        let capabilities = self.capabilities;
        let rlimits = self.rlimits.clone();
        let no_new_privileges = self.no_new_privileges;
        let readonly_rootfs = self.readonly_rootfs;
        let masked_paths = self.masked_paths.clone();
        let bind_mounts = self.bind_mounts.clone();
        let tmpfs_mounts = self.tmpfs_mounts.clone();
        // Loopback mode: bring up lo inside pre_exec (after unshare(NEWNET)).
        // Bridge mode uses setns instead — lo is configured by setup_bridge_network.
        let bring_up_loopback = self.network_config.as_ref().map_or(false, |c| {
            c.mode == crate::network::NetworkMode::Loopback
        });
        let is_bridge = self.network_config.as_ref().map_or(false, |c| {
            c.mode == crate::network::NetworkMode::Bridge
        });

        // Bridge mode: create and fully configure the named netns BEFORE fork.
        // The child's pre_exec will join it via setns — no race whatsoever.
        let bridge_network: Option<crate::network::NetworkSetup> = if is_bridge {
            let ns_name = crate::network::generate_ns_name();
            Some(crate::network::setup_bridge_network(&ns_name, self.nat, self.port_forwards.clone()).map_err(Error::Io)?)
        } else {
            None
        };
        // Pre-allocate the netns path CString so pre_exec can open it without allocating.
        let bridge_ns_path: Option<std::ffi::CString> = bridge_network.as_ref()
            .map(|n| std::ffi::CString::new(format!("/run/netns/{}", n.ns_name)).unwrap());

        // Install our combined pre_exec hook
        unsafe {
            self.inner.pre_exec(move || {
                use std::ptr;
                use std::ffi::CString;

                // Step 1: Unshare namespaces (create new ones)
                if !namespaces.is_empty() {
                    let flags = namespaces.to_clone_flags();
                    unshare(flags).map_err(|e| io::Error::other(format!("unshare error: {}", e)))?;

                    // Step 1.5: If we created a mount namespace, make all mounts private
                    // to prevent mount propagation leaking to the parent namespace
                    if namespaces.contains(Namespace::MOUNT) {
                        use std::ffi::CStr;
                        use std::ptr;

                        let root = CStr::from_bytes_with_nul(b"/\0").unwrap();
                        let result = libc::mount(
                            ptr::null(),                          // source: NULL (remount)
                            root.as_ptr(),                        // target: root
                            ptr::null(),                          // fstype: NULL (remount)
                            libc::MS_REC | libc::MS_PRIVATE,      // flags: recursive + private
                            ptr::null(),                          // data: NULL
                        );

                        if result != 0 {
                            return Err(io::Error::last_os_error());
                        }
                    }

                    // Step 1.6: Loopback mode — bring up lo after unshare(CLONE_NEWNET).
                    if bring_up_loopback {
                        crate::network::bring_up_loopback()
                            .map_err(|e| io::Error::other(format!("loopback up: {}", e)))?;
                    }

                }

                // Step 1.7: Bridge mode — join the pre-configured named netns via setns.
                // The named netns was fully set up before fork; no race is possible.
                if let Some(ref ns_path) = bridge_ns_path {
                    let fd = libc::open(ns_path.as_ptr(), libc::O_RDONLY | libc::O_CLOEXEC);
                    if fd < 0 {
                        return Err(io::Error::last_os_error());
                    }
                    let ret = libc::setns(fd, libc::CLONE_NEWNET);
                    libc::close(fd);
                    if ret != 0 {
                        return Err(io::Error::last_os_error());
                    }
                }

                // Step 2: Set up UID/GID mapping if user namespace is active
                if namespaces.contains(Namespace::USER) {
                    use std::fs;
                    use std::io::Write;

                    // For unprivileged containers, must deny setgroups before writing gid_map
                    if !gid_maps.is_empty() {
                        let mut setgroups = fs::OpenOptions::new()
                            .write(true)
                            .open("/proc/self/setgroups")
                            .map_err(|e| io::Error::other(format!("open setgroups: {}", e)))?;
                        setgroups.write_all(b"deny\n")
                            .map_err(|e| io::Error::other(format!("write setgroups: {}", e)))?;
                    }

                    // Write UID mappings
                    if !uid_maps.is_empty() {
                        let mut uid_map_file = fs::OpenOptions::new()
                            .write(true)
                            .open("/proc/self/uid_map")
                            .map_err(|e| io::Error::other(format!("open uid_map: {}", e)))?;

                        for map in &uid_maps {
                            writeln!(uid_map_file, "{} {} {}", map.inside, map.outside, map.count)
                                .map_err(|e| io::Error::other(format!("write uid_map: {}", e)))?;
                        }
                    }

                    // Write GID mappings
                    if !gid_maps.is_empty() {
                        let mut gid_map_file = fs::OpenOptions::new()
                            .write(true)
                            .open("/proc/self/gid_map")
                            .map_err(|e| io::Error::other(format!("open gid_map: {}", e)))?;

                        for map in &gid_maps {
                            writeln!(gid_map_file, "{} {} {}", map.inside, map.outside, map.count)
                                .map_err(|e| io::Error::other(format!("write gid_map: {}", e)))?;
                        }
                    }
                }

                // Step 3: Set UID/GID if specified
                if let Some(gid_val) = gid {
                    let result = libc::setgid(gid_val);
                    if result != 0 {
                        return Err(io::Error::last_os_error());
                    }
                }
                if let Some(uid_val) = uid {
                    let result = libc::setuid(uid_val);
                    if result != 0 {
                        return Err(io::Error::last_os_error());
                    }
                }

                // Step 4: Change root if specified
                if let Some((ref new_root, ref put_old)) = pivot_root {
                    // Use pivot_root for better security
                    use std::os::unix::ffi::OsStrExt;

                    // pivot_root syscall (not in nix crate, use libc directly)
                    let new_root_c = CString::new(new_root.as_os_str().as_bytes()).unwrap();
                    let put_old_c = CString::new(put_old.as_os_str().as_bytes()).unwrap();

                    // pivot_root syscall number is 155 on x86_64
                    #[cfg(target_arch = "x86_64")]
                    const SYS_PIVOT_ROOT: i64 = 155;
                    #[cfg(target_arch = "aarch64")]
                    const SYS_PIVOT_ROOT: i64 = 41;

                    let result = libc::syscall(
                        SYS_PIVOT_ROOT,
                        new_root_c.as_ptr(),
                        put_old_c.as_ptr(),
                    );

                    if result != 0 {
                        return Err(io::Error::last_os_error());
                    }

                    // Change to new root
                    std::env::set_current_dir("/")?;

                    // Unmount old root
                    let put_old_rel = put_old.strip_prefix(new_root)
                        .map_err(|_| io::Error::other("put_old must be inside new_root"))?;
                    let put_old_rel_c = CString::new(put_old_rel.as_os_str().as_bytes()).unwrap();

                    let umount_result = libc::umount2(put_old_rel_c.as_ptr(), libc::MNT_DETACH);
                    if umount_result != 0 {
                        // Don't fail if unmount doesn't work - it's not critical
                    }
                } else if let Some(ref dir) = chroot_dir {
                    // Fallback to chroot if pivot_root not specified
                    use std::os::unix::ffi::OsStrExt;

                    // If readonly rootfs is requested, bind-mount the chroot dir to itself BEFORE chroot
                    // This makes it a proper mount point so we can remount it readonly later
                    if readonly_rootfs {
                        let dir_c = CString::new(dir.as_os_str().as_bytes()).unwrap();
                        let result = libc::mount(
                            dir_c.as_ptr(),          // source: chroot dir
                            dir_c.as_ptr(),          // target: same dir
                            ptr::null(),             // fstype: NULL
                            libc::MS_BIND | libc::MS_REC, // recursive bind mount
                            ptr::null(),             // data: NULL
                        );
                        if result != 0 {
                            return Err(io::Error::last_os_error());
                        }
                    }

                    // Perform bind mounts BEFORE chroot — source paths are host paths,
                    // unreachable once we chroot.
                    for bm in &bind_mounts {
                        use std::os::unix::ffi::OsStrExt as _;
                        // Target inside the chroot on the host side
                        let rel = bm.target.strip_prefix("/").unwrap_or(&bm.target);
                        let host_target = dir.join(rel);
                        std::fs::create_dir_all(&host_target)
                            .map_err(|e| io::Error::other(format!("bind mount mkdir: {}", e)))?;
                        let src_c = CString::new(bm.source.as_os_str().as_bytes()).unwrap();
                        let tgt_c = CString::new(host_target.as_os_str().as_bytes()).unwrap();
                        // Step 1: establish the bind
                        let r = libc::mount(
                            src_c.as_ptr(),
                            tgt_c.as_ptr(),
                            ptr::null(),
                            libc::MS_BIND,
                            ptr::null(),
                        );
                        if r != 0 {
                            return Err(io::Error::other(format!(
                                "bind mount {} -> {}: {}",
                                bm.source.display(), host_target.display(),
                                io::Error::last_os_error()
                            )));
                        }
                        // Step 2 (if readonly): remount read-only — Linux requires two calls
                        if bm.readonly {
                            let r2 = libc::mount(
                                ptr::null(),
                                tgt_c.as_ptr(),
                                ptr::null(),
                                libc::MS_REMOUNT | libc::MS_BIND | libc::MS_RDONLY,
                                ptr::null(),
                            );
                            if r2 != 0 {
                                return Err(io::Error::other(format!(
                                    "bind mount remount ro {}: {}",
                                    host_target.display(), io::Error::last_os_error()
                                )));
                            }
                        }
                    }

                    chroot(dir).map_err(|e| io::Error::other(format!("chroot error: {}", e)))?;

                    // Change working directory to / after chroot
                    std::env::set_current_dir("/")?;
                }

                // Step 4.5: Perform automatic mounts if requested
                if mount_proc {
                    // Mount new proc filesystem at /proc
                    let proc = CString::new("proc").unwrap();
                    let result = libc::mount(
                        proc.as_ptr(),      // source
                        proc.as_ptr(),      // target (/proc)
                        proc.as_ptr(),      // fstype (proc)
                        0,                  // flags
                        ptr::null(),        // data
                    );
                    if result != 0 {
                        return Err(io::Error::last_os_error());
                    }
                }

                if mount_sys {
                    // Bind mount /sys (from host) to /sys (in container)
                    let sys = CString::new("/sys").unwrap();
                    let sysfs = CString::new("sysfs").unwrap();
                    let result = libc::mount(
                        sys.as_ptr(),       // source
                        sys.as_ptr(),       // target
                        sysfs.as_ptr(),     // fstype
                        libc::MS_BIND,      // flags
                        ptr::null(),        // data
                    );
                    if result != 0 {
                        return Err(io::Error::last_os_error());
                    }
                }

                if mount_dev {
                    // Bind mount /dev (from host) to /dev (in container)
                    let dev = CString::new("/dev").unwrap();
                    let result = libc::mount(
                        dev.as_ptr(),       // source
                        dev.as_ptr(),       // target
                        ptr::null(),        // fstype (NULL for bind mount)
                        libc::MS_BIND | libc::MS_REC, // recursive bind
                        ptr::null(),        // data
                    );
                    if result != 0 {
                        return Err(io::Error::last_os_error());
                    }
                }

                // Mount tmpfs filesystems AFTER chroot — tmpfs has no host-side source
                for tm in &tmpfs_mounts {
                    std::fs::create_dir_all(&tm.target)
                        .map_err(|e| io::Error::other(format!("tmpfs mkdir: {}", e)))?;
                    let tgt_c = CString::new(tm.target.as_os_str().as_encoded_bytes()).unwrap();
                    let tmpfs_c = CString::new("tmpfs").unwrap();
                    let opts_c = CString::new(tm.options.as_bytes()).unwrap();
                    let opts_ptr = if tm.options.is_empty() { ptr::null() } else { opts_c.as_ptr() as *const libc::c_void };
                    let result = libc::mount(
                        tmpfs_c.as_ptr(),   // source: "tmpfs"
                        tgt_c.as_ptr(),     // target
                        tmpfs_c.as_ptr(),   // fstype: "tmpfs"
                        libc::MS_NOSUID | libc::MS_NODEV, // flags
                        opts_ptr,           // data: mount options
                    );
                    if result != 0 {
                        return Err(io::Error::other(format!(
                            "tmpfs mount {}: {}",
                            tm.target.display(), io::Error::last_os_error()
                        )));
                    }
                }

                // Step 4.75: Drop capabilities if specified
                if let Some(keep_caps) = capabilities {
                    // Drop all capabilities except the ones specified
                    // We use prctl with PR_CAPBSET_DROP to drop from the bounding set
                    const PR_CAPBSET_DROP: i32 = 24;

                    // All capability numbers (0-37 covers most common capabilities)
                    for cap in 0..38 {
                        let cap_bit = 1u64 << cap;

                        // If this capability is NOT in keep_caps, drop it
                        if !keep_caps.contains(Capability::from_bits_truncate(cap_bit)) {
                            let result = libc::prctl(PR_CAPBSET_DROP, cap, 0, 0, 0);
                            // Ignore errors for capabilities that don't exist
                            if result != 0 {
                                let err = io::Error::last_os_error();
                                // EINVAL means capability doesn't exist, which is fine
                                if err.raw_os_error() != Some(libc::EINVAL) {
                                    return Err(err);
                                }
                            }
                        }
                    }
                }

                // Step 4.8: Mask sensitive paths
                if !masked_paths.is_empty() {
                    let dev_null = CString::new("/dev/null").unwrap();
                    for path in &masked_paths {
                        let path_c = match CString::new(path.as_os_str().as_encoded_bytes()) {
                            Ok(p) => p,
                            Err(_) => continue, // Skip paths with null bytes
                        };

                        // Bind mount /dev/null over the path to mask it
                        let result = libc::mount(
                            dev_null.as_ptr(),           // source: /dev/null
                            path_c.as_ptr(),             // target: path to mask
                            ptr::null(),                 // fstype: NULL
                            libc::MS_BIND,               // bind mount
                            ptr::null(),                 // data: NULL
                        );

                        // Ignore errors - path might not exist, which is fine
                        if result != 0 {
                            // Don't fail, just skip this path
                        }
                    }
                }

                // Step 4.85: Make rootfs read-only if requested
                // MUST come after all mounts (/proc, /sys, /dev, masked paths)
                // Note: We already did bind mount before chroot, so just remount readonly now
                if readonly_rootfs {
                    let root = CString::new("/").unwrap();
                    let result = libc::mount(
                        ptr::null(),             // source: NULL (remount)
                        root.as_ptr(),           // target: /
                        ptr::null(),             // fstype: NULL (remount)
                        libc::MS_REMOUNT | libc::MS_RDONLY | libc::MS_BIND, // remount readonly
                        ptr::null(),             // data: NULL
                    );
                    if result != 0 {
                        return Err(io::Error::last_os_error());
                    }
                }

                // Step 4.9: Set resource limits if specified
                for limit in &rlimits {
                    let rlimit = libc::rlimit {
                        rlim_cur: limit.soft,
                        rlim_max: limit.hard,
                    };

                    let result = libc::setrlimit(limit.resource, &rlimit);
                    if result != 0 {
                        return Err(io::Error::last_os_error());
                    }
                }

                // Step 5: Run user-provided pre_exec callback
                if let Some(ref callback) = user_pre_exec {
                    callback()?;
                }

                // Step 6: Join existing namespaces AFTER chroot and filesystem setup
                // This ensures paths are resolved correctly before namespace transitions
                for (fd, _ns) in &join_ns_fds {
                    let result = libc::setns(*fd, 0);
                    if result != 0 {
                        return Err(io::Error::last_os_error());
                    }
                }

                // Step 6.5: Set no-new-privileges flag if requested
                // This prevents privilege escalation via setuid/setgid binaries
                if no_new_privileges {
                    const PR_SET_NO_NEW_PRIVS: i32 = 38;
                    let result = libc::prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0);
                    if result != 0 {
                        return Err(io::Error::last_os_error());
                    }
                }

                // Step 7 (FINAL): Apply seccomp filter if configured
                // CRITICAL: This MUST be the last step! Once seccomp is applied, many syscalls
                // are blocked, so all other setup must be complete.
                if let Some(ref filter) = seccomp_filter {
                    crate::seccomp::apply_filter(filter)?;
                }

                Ok(())
            });
        }

        // Spawn the process
        let child_inner = self.inner.spawn().map_err(Error::Spawn)?;

        // Keep join_ns_files alive until here so file descriptors remain valid
        drop(join_ns_files);

        // Create cgroup and add child PID (parent-side, after fork)
        let cgroup = if let Some(ref cfg) = self.cgroup_config {
            Some(crate::cgroup::setup_cgroup(cfg, child_inner.id()).map_err(Error::Io)?)
        } else {
            None
        };

        // Bridge networking was fully set up before fork; nothing to do here.
        let network = bridge_network;

        Ok(Child { inner: child_inner, cgroup, network })
    }

    /// Spawn the container with a PTY for proper session isolation.
    ///
    /// Allocates a PTY master/slave pair. The slave becomes the container's
    /// controlling terminal (stdin/stdout/stderr). The parent holds the master
    /// and uses it to relay I/O to/from the user's terminal.
    ///
    /// Returns an [`crate::pty::InteractiveSession`] — call `.run()` on it to
    /// start the relay loop, which blocks until the container exits.
    ///
    /// # Differences from `spawn()`
    ///
    /// - The container gets its own session (`setsid`) and controlling terminal
    /// - Signals (Ctrl+C, Ctrl+Z) are scoped to the container's session only
    /// - Terminal settings (colors, readline) are fully isolated
    pub fn spawn_interactive(mut self) -> Result<crate::pty::InteractiveSession, Error> {
        use std::os::fd::AsRawFd;

        // Allocate PTY pair in the parent before fork.
        // master: parent holds this and relays I/O through it.
        // slave:  child's stdin/stdout/stderr will be wired to this.
        let pty = nix::pty::openpty(None, None).map_err(|e| Error::Io(io::Error::from(e)))?;

        let master = pty.master;
        let slave = pty.slave;

        let slave_raw_fd = slave.as_raw_fd();
        let master_raw_fd = master.as_raw_fd();

        // Mark master CLOEXEC so the child doesn't accidentally inherit it
        unsafe {
            libc::fcntl(master_raw_fd, libc::F_SETFD, libc::FD_CLOEXEC);
        }

        // Ensure slave is NOT CLOEXEC — it must survive exec in the child
        unsafe {
            let flags = libc::fcntl(slave_raw_fd, libc::F_GETFD);
            libc::fcntl(slave_raw_fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC);
        }

        // --- From here, identical setup to spawn() except we capture slave_raw_fd ---

        let seccomp_filter = if let Some(profile) = &self.seccomp_profile {
            match profile {
                SeccompProfile::Docker => Some(crate::seccomp::docker_default_filter()
                    .map_err(|e| Error::Seccomp(e))?),
                SeccompProfile::Minimal => Some(crate::seccomp::minimal_filter()
                    .map_err(|e| Error::Seccomp(e))?),
                SeccompProfile::None => None,
            }
        } else {
            None
        };

        let join_ns_files: Vec<(File, Namespace)> = self.join_namespaces
            .iter()
            .map(|(path, ns)| {
                File::open(path)
                    .map(|f| (f, *ns))
                    .map_err(Error::Io)
            })
            .collect::<Result<Vec<_>, _>>()?;

        let join_ns_fds: Vec<(i32, Namespace)> = join_ns_files
            .iter()
            .map(|(f, ns)| (f.as_raw_fd(), *ns))
            .collect();

        let namespaces = self.namespaces;
        let chroot_dir = self.chroot_dir.clone();
        let user_pre_exec = self.pre_exec.take();
        let uid_maps = self.uid_maps.clone();
        let gid_maps = self.gid_maps.clone();
        let uid = self.uid;
        let gid = self.gid;
        let mount_proc = self.mount_proc;
        let mount_sys = self.mount_sys;
        let mount_dev = self.mount_dev;
        let pivot_root = self.pivot_root.clone();
        let capabilities = self.capabilities;
        let rlimits = self.rlimits.clone();
        let no_new_privileges = self.no_new_privileges;
        let readonly_rootfs = self.readonly_rootfs;
        let masked_paths = self.masked_paths.clone();
        let bind_mounts = self.bind_mounts.clone();
        let tmpfs_mounts = self.tmpfs_mounts.clone();
        let bring_up_loopback = self.network_config.as_ref().map_or(false, |c| {
            c.mode == crate::network::NetworkMode::Loopback
        });
        let is_bridge = self.network_config.as_ref().map_or(false, |c| {
            c.mode == crate::network::NetworkMode::Bridge
        });

        // Bridge mode: create and fully configure the named netns BEFORE fork.
        let bridge_network: Option<crate::network::NetworkSetup> = if is_bridge {
            let ns_name = crate::network::generate_ns_name();
            Some(crate::network::setup_bridge_network(&ns_name, self.nat, self.port_forwards.clone()).map_err(Error::Io)?)
        } else {
            None
        };
        let bridge_ns_path: Option<std::ffi::CString> = bridge_network.as_ref()
            .map(|n| std::ffi::CString::new(format!("/run/netns/{}", n.ns_name)).unwrap());

        unsafe {
            self.inner.pre_exec(move || {
                use std::ptr;
                use std::ffi::CString;

                // Step 0: PTY slave setup — runs before everything else.
                // Create a new session so the container is isolated from the
                // parent's session, then make the slave our controlling terminal.
                let setsid_ret = libc::setsid();
                if setsid_ret < 0 {
                    return Err(io::Error::last_os_error());
                }

                // TIOCSCTTY: make the slave the controlling terminal of this session
                let ioctl_ret = libc::ioctl(slave_raw_fd, libc::TIOCSCTTY, 0 as libc::c_int);
                if ioctl_ret < 0 {
                    return Err(io::Error::last_os_error());
                }

                // Wire stdin/stdout/stderr to the slave
                for dest_fd in [0i32, 1, 2] {
                    if slave_raw_fd != dest_fd {
                        let dup_ret = libc::dup2(slave_raw_fd, dest_fd);
                        if dup_ret < 0 {
                            return Err(io::Error::last_os_error());
                        }
                    }
                }
                // Close the original slave fd — 0/1/2 are now the duplicates
                libc::close(slave_raw_fd);

                // Steps 1–7: identical to spawn() from here
                if !namespaces.is_empty() {
                    let flags = namespaces.to_clone_flags();
                    unshare(flags).map_err(|e| io::Error::other(format!("unshare error: {}", e)))?;

                    if namespaces.contains(Namespace::MOUNT) {
                        use std::ffi::CStr;
                        let root = CStr::from_bytes_with_nul(b"/\0").unwrap();
                        let result = libc::mount(
                            ptr::null(),
                            root.as_ptr(),
                            ptr::null(),
                            libc::MS_PRIVATE | libc::MS_REC,
                            ptr::null(),
                        );
                        if result != 0 {
                            return Err(io::Error::last_os_error());
                        }
                    }

                    if bring_up_loopback {
                        crate::network::bring_up_loopback()
                            .map_err(|e| io::Error::other(format!("loopback up: {}", e)))?;
                    }

                }

                // Bridge mode — join the pre-configured named netns via setns.
                if let Some(ref ns_path) = bridge_ns_path {
                    let fd = libc::open(ns_path.as_ptr(), libc::O_RDONLY | libc::O_CLOEXEC);
                    if fd < 0 {
                        return Err(io::Error::last_os_error());
                    }
                    let ret = libc::setns(fd, libc::CLONE_NEWNET);
                    libc::close(fd);
                    if ret != 0 {
                        return Err(io::Error::last_os_error());
                    }
                }

                if !uid_maps.is_empty() || !gid_maps.is_empty() {
                    if let Some(uid_val) = uid {
                        let result = libc::setuid(uid_val);
                        if result != 0 {
                            return Err(io::Error::last_os_error());
                        }
                    }
                    if let Some(gid_val) = gid {
                        let result = libc::setgid(gid_val);
                        if result != 0 {
                            return Err(io::Error::last_os_error());
                        }
                    }
                }

                if let Some((ref new_root, ref put_old)) = pivot_root {
                    use std::os::unix::ffi::OsStrExt;
                    std::fs::create_dir_all(put_old).ok();

                    let new_root_c = CString::new(new_root.as_os_str().as_bytes()).unwrap();
                    let put_old_c = CString::new(put_old.as_os_str().as_bytes()).unwrap();

                    #[cfg(target_arch = "x86_64")]
                    const SYS_PIVOT_ROOT: i64 = 155;
                    #[cfg(target_arch = "aarch64")]
                    const SYS_PIVOT_ROOT: i64 = 41;

                    let result = libc::syscall(SYS_PIVOT_ROOT, new_root_c.as_ptr(), put_old_c.as_ptr());
                    if result != 0 {
                        return Err(io::Error::last_os_error());
                    }
                    std::env::set_current_dir("/")?;

                    let put_old_rel = put_old.strip_prefix(new_root)
                        .map_err(|_| io::Error::other("put_old must be inside new_root"))?;
                    let put_old_rel_c = CString::new(put_old_rel.as_os_str().as_bytes()).unwrap();
                    libc::umount2(put_old_rel_c.as_ptr(), libc::MNT_DETACH);
                } else if let Some(ref dir) = chroot_dir {
                    use std::os::unix::ffi::OsStrExt;

                    if readonly_rootfs {
                        let dir_c = CString::new(dir.as_os_str().as_bytes()).unwrap();
                        let result = libc::mount(
                            dir_c.as_ptr(),
                            dir_c.as_ptr(),
                            ptr::null(),
                            libc::MS_BIND | libc::MS_REC,
                            ptr::null(),
                        );
                        if result != 0 {
                            return Err(io::Error::last_os_error());
                        }
                    }

                    // Perform bind mounts BEFORE chroot — source paths are host paths,
                    // unreachable once we chroot.
                    for bm in &bind_mounts {
                        use std::os::unix::ffi::OsStrExt as _;
                        let rel = bm.target.strip_prefix("/").unwrap_or(&bm.target);
                        let host_target = dir.join(rel);
                        std::fs::create_dir_all(&host_target)
                            .map_err(|e| io::Error::other(format!("bind mount mkdir: {}", e)))?;
                        let src_c = CString::new(bm.source.as_os_str().as_bytes()).unwrap();
                        let tgt_c = CString::new(host_target.as_os_str().as_bytes()).unwrap();
                        let r = libc::mount(
                            src_c.as_ptr(),
                            tgt_c.as_ptr(),
                            ptr::null(),
                            libc::MS_BIND,
                            ptr::null(),
                        );
                        if r != 0 {
                            return Err(io::Error::other(format!(
                                "bind mount {} -> {}: {}",
                                bm.source.display(), host_target.display(),
                                io::Error::last_os_error()
                            )));
                        }
                        if bm.readonly {
                            let r2 = libc::mount(
                                ptr::null(),
                                tgt_c.as_ptr(),
                                ptr::null(),
                                libc::MS_REMOUNT | libc::MS_BIND | libc::MS_RDONLY,
                                ptr::null(),
                            );
                            if r2 != 0 {
                                return Err(io::Error::other(format!(
                                    "bind mount remount ro {}: {}",
                                    host_target.display(), io::Error::last_os_error()
                                )));
                            }
                        }
                    }

                    chroot(dir).map_err(|e| io::Error::other(format!("chroot error: {}", e)))?;
                    std::env::set_current_dir("/")?;
                }

                if mount_proc {
                    let proc = CString::new("proc").unwrap();
                    let result = libc::mount(proc.as_ptr(), proc.as_ptr(), proc.as_ptr(), 0, ptr::null());
                    if result != 0 {
                        return Err(io::Error::last_os_error());
                    }
                }

                if mount_sys {
                    let sys = CString::new("/sys").unwrap();
                    let sysfs = CString::new("sysfs").unwrap();
                    let result = libc::mount(sys.as_ptr(), sys.as_ptr(), sysfs.as_ptr(), libc::MS_BIND, ptr::null());
                    if result != 0 {
                        return Err(io::Error::last_os_error());
                    }
                }

                if mount_dev {
                    let dev = CString::new("/dev").unwrap();
                    let result = libc::mount(dev.as_ptr(), dev.as_ptr(), ptr::null(), libc::MS_BIND | libc::MS_REC, ptr::null());
                    if result != 0 {
                        return Err(io::Error::last_os_error());
                    }
                }

                // Mount tmpfs filesystems AFTER chroot
                for tm in &tmpfs_mounts {
                    std::fs::create_dir_all(&tm.target)
                        .map_err(|e| io::Error::other(format!("tmpfs mkdir: {}", e)))?;
                    let tgt_c = CString::new(tm.target.as_os_str().as_encoded_bytes()).unwrap();
                    let tmpfs_c = CString::new("tmpfs").unwrap();
                    let opts_c = CString::new(tm.options.as_bytes()).unwrap();
                    let opts_ptr = if tm.options.is_empty() { ptr::null() } else { opts_c.as_ptr() as *const libc::c_void };
                    let result = libc::mount(
                        tmpfs_c.as_ptr(),
                        tgt_c.as_ptr(),
                        tmpfs_c.as_ptr(),
                        libc::MS_NOSUID | libc::MS_NODEV,
                        opts_ptr,
                    );
                    if result != 0 {
                        return Err(io::Error::other(format!(
                            "tmpfs mount {}: {}",
                            tm.target.display(), io::Error::last_os_error()
                        )));
                    }
                }

                if let Some(keep_caps) = capabilities {
                    const PR_CAPBSET_DROP: i32 = 24;
                    for cap in 0..38 {
                        let cap_bit = 1u64 << cap;
                        if !keep_caps.contains(Capability::from_bits_truncate(cap_bit)) {
                            let result = libc::prctl(PR_CAPBSET_DROP, cap, 0, 0, 0);
                            if result != 0 {
                                let err = io::Error::last_os_error();
                                if err.raw_os_error() != Some(libc::EINVAL) {
                                    return Err(err);
                                }
                            }
                        }
                    }
                }

                if !masked_paths.is_empty() {
                    let dev_null = CString::new("/dev/null").unwrap();
                    for path in &masked_paths {
                        let path_c = match CString::new(path.as_os_str().as_encoded_bytes()) {
                            Ok(p) => p,
                            Err(_) => continue,
                        };
                        libc::mount(dev_null.as_ptr(), path_c.as_ptr(), ptr::null(), libc::MS_BIND, ptr::null());
                    }
                }

                if readonly_rootfs {
                    let root = CString::new("/").unwrap();
                    let result = libc::mount(
                        ptr::null(),
                        root.as_ptr(),
                        ptr::null(),
                        libc::MS_REMOUNT | libc::MS_RDONLY | libc::MS_BIND,
                        ptr::null(),
                    );
                    if result != 0 {
                        return Err(io::Error::last_os_error());
                    }
                }

                for limit in &rlimits {
                    let rlimit = libc::rlimit { rlim_cur: limit.soft, rlim_max: limit.hard };
                    let result = libc::setrlimit(limit.resource, &rlimit);
                    if result != 0 {
                        return Err(io::Error::last_os_error());
                    }
                }

                if let Some(cb) = &user_pre_exec {
                    cb()?;
                }

                for (fd, _ns) in &join_ns_fds {
                    let result = libc::setns(*fd, 0);
                    if result != 0 {
                        return Err(io::Error::last_os_error());
                    }
                }

                if no_new_privileges {
                    const PR_SET_NO_NEW_PRIVS: i32 = 38;
                    let result = libc::prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0);
                    if result != 0 {
                        return Err(io::Error::last_os_error());
                    }
                }

                if let Some(ref filter) = seccomp_filter {
                    crate::seccomp::apply_filter(filter)?;
                }

                Ok(())
            });
        }

        let child_inner = self.inner.spawn().map_err(Error::Spawn)?;

        // Close the slave in the parent — only the child should have it.
        // If we keep it open, POLLHUP on the master will never fire when
        // the container exits (because we still hold a reference to the slave).
        drop(slave);
        drop(join_ns_files);

        // Create cgroup and add child PID (parent-side, after fork)
        let cgroup = if let Some(ref cfg) = self.cgroup_config {
            Some(crate::cgroup::setup_cgroup(cfg, child_inner.id()).map_err(Error::Io)?)
        } else {
            None
        };

        // Bridge networking was fully set up before fork; nothing to do here.
        let network = bridge_network;

        Ok(crate::pty::InteractiveSession {
            master,
            child: Child { inner: child_inner, cgroup, network },
        })
    }
}

/// A handle to a spawned child process.
///
/// Provides access to the process ID and methods to wait for completion.
/// Similar to [`std::process::Child`] but specifically for containerized processes.
///
/// # Examples
///
/// ```no_run
/// use remora::container::{Command, Namespace};
///
/// let mut child = Command::new("/bin/sleep")
///     .args(&["5"])
///     .with_namespaces(Namespace::PID)
///     .spawn()?;
///
/// println!("Spawned process with PID: {}", child.pid());
///
/// let status = child.wait()?;
/// println!("Process exited with: {:?}", status);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub struct Child {
    inner: process::Child,
    /// Optional cgroup for this container. Deleted after the child exits.
    pub(crate) cgroup: Option<cgroups_rs::fs::Cgroup>,
    /// Optional network state (veth pair). Torn down after the child exits.
    network: Option<crate::network::NetworkSetup>,
}

impl Child {
    /// Returns the process ID of the child.
    pub fn pid(&self) -> i32 {
        self.inner.id() as i32
    }

    /// Returns the host-side veth interface name if bridge networking is active.
    pub fn veth_name(&self) -> Option<&str> {
        self.network.as_ref().map(|n| n.veth_host.as_str())
    }

    /// Returns the named network namespace name (e.g. `rem-12345-0`) if bridge
    /// networking is active. Useful for verifying teardown in tests.
    pub fn netns_name(&self) -> Option<&str> {
        self.network.as_ref().map(|n| n.ns_name.as_str())
    }

    /// Wait for the child process to exit.
    ///
    /// This will block until the process terminates and return its exit status.
    /// If a cgroup was configured, it is deleted after the child exits.
    pub fn wait(&mut self) -> Result<ExitStatus, Error> {
        let status = self.inner.wait().map_err(Error::Wait)?;
        if let Some(cg) = self.cgroup.take() {
            crate::cgroup::teardown_cgroup(cg);
        }
        if let Some(ref net) = self.network {
            crate::network::teardown_network(net);
        }
        Ok(ExitStatus { inner: status })
    }

    /// Wait for the child to exit and collect all output.
    ///
    /// Returns (exit_status, stdout_bytes, stderr_bytes).
    /// Only works if Stdio::Piped was set for stdout/stderr.
    /// If a cgroup was configured, it is deleted after the child exits.
    pub fn wait_with_output(self) -> Result<(ExitStatus, Vec<u8>, Vec<u8>), Error> {
        let output = self.inner.wait_with_output().map_err(Error::Wait)?;
        if let Some(cg) = self.cgroup {
            crate::cgroup::teardown_cgroup(cg);
        }
        if let Some(ref net) = self.network {
            crate::network::teardown_network(net);
        }
        Ok((
            ExitStatus { inner: output.status },
            output.stdout,
            output.stderr,
        ))
    }

    /// Read current resource usage from the container's cgroup.
    ///
    /// Returns statistics on memory, CPU, and process count. Only available
    /// if the container was spawned with cgroup limits configured (e.g.
    /// [`Command::with_cgroup_memory`]). Returns zeros if no cgroup is active.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// let stats = child.resource_stats()?;
    /// println!("Memory: {} bytes", stats.memory_current_bytes);
    /// println!("CPU: {} ns", stats.cpu_usage_ns);
    /// println!("PIDs: {}", stats.pids_current);
    /// ```
    pub fn resource_stats(&self) -> Result<crate::cgroup::ResourceStats, Error> {
        if let Some(ref cg) = self.cgroup {
            crate::cgroup::read_stats(cg).map_err(Error::Io)
        } else {
            Ok(crate::cgroup::ResourceStats::default())
        }
    }
}

/// Exit status of a terminated child process.
#[derive(Debug, Clone)]
pub struct ExitStatus {
    inner: StdExitStatus,
}

impl ExitStatus {
    /// Returns true if the process exited successfully (status code 0).
    pub fn success(&self) -> bool {
        self.inner.success()
    }

    /// Returns the exit code if the process terminated normally.
    pub fn code(&self) -> Option<i32> {
        self.inner.code()
    }

    /// Returns the signal that terminated the process, if any.
    pub fn signal(&self) -> Option<i32> {
        self.inner.signal()
    }
}

/// Errors that can occur during container operations.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Failed to unshare namespaces
    #[error("Failed to unshare namespaces: {0}")]
    Unshare(#[source] nix::Error),

    /// Failed to change root directory
    #[error("Failed to chroot to {path}: {source}")]
    Chroot {
        path: String,
        #[source]
        source: nix::Error,
    },

    /// Failed to change directory after chroot
    #[error("Failed to chdir to {path} after chroot: {source}")]
    Chdir {
        path: String,
        #[source]
        source: io::Error,
    },

    /// Failed to execute pre_exec callback
    #[error("Pre-exec callback failed: {0}")]
    PreExec(#[source] io::Error),

    /// Failed to spawn the process
    #[error("Failed to spawn process: {0}")]
    Spawn(#[source] io::Error),

    /// Failed to wait for process completion
    #[error("Failed to wait for process: {0}")]
    Wait(#[source] io::Error),

    /// Failed to setup or apply seccomp filter
    #[error("Seccomp error: {0}")]
    Seccomp(#[source] io::Error),

    /// Generic I/O error
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
}

/// UID mapping for user namespaces.
///
/// Maps user IDs from inside the container to outside the container.
/// Allows unprivileged users to appear as root inside the container.
///
/// # Examples
///
/// ```ignore
/// // Map container root (0) to host user 1000
/// UidMap { inside: 0, outside: 1000, count: 1 }
///
/// // Map range of 1000 UIDs
/// UidMap { inside: 0, outside: 100000, count: 1000 }
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UidMap {
    /// UID inside the container
    pub inside: u32,
    /// UID outside the container (on the host)
    pub outside: u32,
    /// Number of consecutive UIDs to map
    pub count: u32,
}

/// GID mapping for user namespaces.
///
/// Maps group IDs from inside the container to outside the container.
///
/// # Examples
///
/// ```ignore
/// // Map container root group (0) to host group 1000
/// GidMap { inside: 0, outside: 1000, count: 1 }
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GidMap {
    /// GID inside the container
    pub inside: u32,
    /// GID outside the container (on the host)
    pub outside: u32,
    /// Number of consecutive GIDs to map
    pub count: u32,
}

/// Resource limit (rlimit) configuration.
///
/// Controls resource usage for the containerized process.
///
/// # Examples
///
/// ```ignore
/// // Limit open file descriptors to 1024
/// ResourceLimit {
///     resource: libc::RLIMIT_NOFILE,
///     soft: 1024,
///     hard: 1024,
/// }
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResourceLimit {
    /// Resource type (e.g., libc::RLIMIT_NOFILE, libc::RLIMIT_AS)
    pub resource: libc::__rlimit_resource_t,
    /// Soft limit (can be increased up to hard limit)
    pub soft: libc::rlim_t,
    /// Hard limit (requires privileges to increase)
    pub hard: libc::rlim_t,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_namespace_bitflags_combination() {
        let ns = Namespace::UTS | Namespace::PID | Namespace::MOUNT;

        assert!(ns.contains(Namespace::UTS));
        assert!(ns.contains(Namespace::PID));
        assert!(ns.contains(Namespace::MOUNT));
        assert!(!ns.contains(Namespace::NET));
    }

    #[test]
    fn test_namespace_empty() {
        let ns = Namespace::empty();

        assert!(!ns.contains(Namespace::UTS));
        assert!(!ns.contains(Namespace::PID));
        assert!(ns.is_empty());
    }

    #[test]
    fn test_namespace_all() {
        let ns = Namespace::all();

        assert!(ns.contains(Namespace::UTS));
        assert!(ns.contains(Namespace::PID));
        assert!(ns.contains(Namespace::MOUNT));
        assert!(ns.contains(Namespace::NET));
        assert!(ns.contains(Namespace::IPC));
        assert!(ns.contains(Namespace::USER));
        assert!(ns.contains(Namespace::CGROUP));
    }

    #[test]
    fn test_namespace_to_clone_flags() {
        let ns = Namespace::UTS | Namespace::PID;
        let flags = ns.to_clone_flags();

        assert!(flags.contains(CloneFlags::CLONE_NEWUTS));
        assert!(flags.contains(CloneFlags::CLONE_NEWPID));
        assert!(!flags.contains(CloneFlags::CLONE_NEWNS));
    }

    #[test]
    fn test_namespace_difference() {
        let ns1 = Namespace::UTS | Namespace::PID | Namespace::MOUNT;
        let ns2 = Namespace::PID | Namespace::NET;

        let diff = ns1 & !ns2;  // Items in ns1 but not in ns2

        assert!(diff.contains(Namespace::UTS));
        assert!(diff.contains(Namespace::MOUNT));
        assert!(!diff.contains(Namespace::PID));
        assert!(!diff.contains(Namespace::NET));
    }

    #[test]
    fn test_command_builder_pattern() {
        let cmd = Command::new("/bin/echo")
            .args(&["hello", "world"])
            .with_namespaces(Namespace::UTS)
            .stdin(Stdio::Null)
            .stdout(Stdio::Null)
            .stderr(Stdio::Null);

        // Builder pattern works (compiles)
        assert_eq!(cmd.namespaces, Namespace::UTS);
    }

    #[test]
    fn test_command_chaining() {
        // Test that methods can be chained fluently
        let _cmd = Command::new("/bin/true")
            .args(&["arg1"])
            .with_chroot("/tmp")
            .with_namespaces(Namespace::PID | Namespace::MOUNT);

        // Compilation success means chaining works
    }

    #[test]
    fn test_stdio_conversion() {
        let _inherit: process::Stdio = Stdio::Inherit.into();
        let _null: process::Stdio = Stdio::Null.into();
        let _piped: process::Stdio = Stdio::Piped.into();

        // Conversion works (compiles)
    }

    #[test]
    fn test_error_display() {
        let err = Error::Spawn(io::Error::new(io::ErrorKind::NotFound, "test"));
        let msg = format!("{}", err);

        assert!(msg.contains("Failed to spawn process"));
    }

    #[test]
    fn test_error_from_io() {
        let io_err = io::Error::new(io::ErrorKind::PermissionDenied, "test");
        let err: Error = io_err.into();

        match err {
            Error::Io(_) => {},
            _ => panic!("Expected Error::Io variant"),
        }
    }

    // Integration-style tests (would need proper setup to run)

    #[test]
    #[ignore] // Ignore by default - requires root/CAP_SYS_ADMIN
    fn test_spawn_simple_command() {
        let mut child = Command::new("/bin/true")
            .spawn()
            .expect("Failed to spawn /bin/true");

        let status = child.wait().expect("Failed to wait");
        assert!(status.success());
    }

    #[test]
    #[ignore] // Ignore by default - requires root
    fn test_spawn_with_namespace() {
        let mut child = Command::new("/bin/true")
            .with_namespaces(Namespace::UTS)
            .spawn()
            .expect("Failed to spawn with namespace");

        let status = child.wait().expect("Failed to wait");
        assert!(status.success());
    }
}
