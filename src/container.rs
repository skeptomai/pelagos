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

/// Portable type for rlimit resource constants.
/// glibc defines `__rlimit_resource_t` (c_uint), musl uses plain `c_int`.
#[cfg(target_env = "gnu")]
pub type RlimitResource = libc::__rlimit_resource_t;
#[cfg(not(target_env = "gnu"))]
pub type RlimitResource = libc::c_int;
pub use seccompiler::BpfProgram;
use std::ffi::OsStr;
use std::fs::File;
use std::io;
use std::os::unix::io::AsRawFd;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::PathBuf;
use std::process::{self, ExitStatus as StdExitStatus};
use std::sync::atomic::{AtomicU32, Ordering};

/// Counter for unique overlay merged-dir names.
static OVERLAY_COUNTER: AtomicU32 = AtomicU32::new(0);

/// Counter for unique per-container DNS temp-dir names.
static DNS_COUNTER: AtomicU32 = AtomicU32::new(0);

/// Counter for unique per-container hosts temp-dir names.
static HOSTS_COUNTER: AtomicU32 = AtomicU32::new(0);

// Re-export SeccompProfile for public API
pub use crate::seccomp::SeccompProfile;

// ── Rootless overlay helpers ────────────────────────────────────────────────

/// Probe whether native overlayfs with `userxattr` is supported (kernel 5.11+).
///
/// Forks a child that enters a new user + mount namespace, attempts a tiny
/// overlay mount with `,userxattr`, and exits 0 on success. The parent reads
/// the exit code. Result is cached in a `OnceLock` so the probe runs at most
/// once per process.
fn native_rootless_overlay_supported() -> bool {
    use std::sync::OnceLock;
    static RESULT: OnceLock<bool> = OnceLock::new();
    *RESULT.get_or_init(|| {
        let Ok(tmp) = tempfile::TempDir::new() else {
            return false;
        };
        let base = tmp.path();
        let lower = base.join("lower");
        let upper = base.join("upper");
        let work = base.join("work");
        let merged = base.join("merged");
        for d in [&lower, &upper, &work, &merged] {
            if std::fs::create_dir(d).is_err() {
                return false;
            }
        }

        // Fork a child that unshares user+mount namespaces and tries the mount.
        let pid = unsafe { libc::fork() };
        if pid < 0 {
            return false;
        }
        if pid == 0 {
            // Child: unshare user + mount namespaces, try overlay mount.
            let ret = unsafe { libc::unshare(libc::CLONE_NEWUSER | libc::CLONE_NEWNS) };
            if ret != 0 {
                unsafe { libc::_exit(1) };
            }
            // Write uid/gid mappings — mount() requires them after unshare(NEWUSER).
            let uid = unsafe { libc::getuid() };
            let gid = unsafe { libc::getgid() };
            let _ = std::fs::write("/proc/self/setgroups", "deny\n");
            let _ = std::fs::write("/proc/self/uid_map", format!("0 {} 1\n", uid));
            let _ = std::fs::write("/proc/self/gid_map", format!("0 {} 1\n", gid));
            let opts = format!(
                "lowerdir={},upperdir={},workdir={},userxattr",
                lower.display(),
                upper.display(),
                work.display()
            );
            let opts_c = match std::ffi::CString::new(opts) {
                Ok(c) => c,
                Err(_) => unsafe { libc::_exit(1) },
            };
            let merged_c = match std::ffi::CString::new(merged.as_os_str().as_encoded_bytes()) {
                Ok(c) => c,
                Err(_) => unsafe { libc::_exit(1) },
            };
            let ov_type = c"overlay";
            let ret = unsafe {
                libc::mount(
                    ov_type.as_ptr(),
                    merged_c.as_ptr(),
                    ov_type.as_ptr(),
                    0,
                    opts_c.as_ptr() as *const libc::c_void,
                )
            };
            unsafe { libc::_exit(if ret == 0 { 0 } else { 1 }) };
        }
        // Parent: wait for child and check exit code.
        let mut status: libc::c_int = 0;
        let ret = unsafe { libc::waitpid(pid, &mut status, 0) };
        if ret < 0 {
            return false;
        }
        // WIFEXITED && WEXITSTATUS == 0
        libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0
    })
}

/// Check whether `fuse-overlayfs` is available on PATH.
fn is_fuse_overlayfs_available() -> bool {
    std::process::Command::new("fuse-overlayfs")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok()
}

/// Spawn a `fuse-overlayfs` subprocess to mount an overlay filesystem.
///
/// Returns the child process handle. The caller must unmount (via `fusermount3 -u`)
/// and reap this child after the container exits.
fn spawn_fuse_overlayfs(
    lower: &str,
    upper: &std::path::Path,
    work: &std::path::Path,
    merged: &std::path::Path,
) -> io::Result<std::process::Child> {
    let opts = format!(
        "lowerdir={},upperdir={},workdir={}",
        lower,
        upper.display(),
        work.display()
    );
    std::process::Command::new("fuse-overlayfs")
        .args(["-o", &opts])
        .arg(merged)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
}

/// Resolve a container's bridge IP by name.
///
/// Searches CLI state (`/run/remora/containers/{name}/state.json`) and OCI state
/// (`/run/remora/{name}/state.json`). Returns the bridge IP string if the container
/// is running and has bridge networking, or an error otherwise.
pub fn resolve_container_ip(name: &str) -> io::Result<String> {
    // Try CLI state first.
    let cli_path = crate::paths::containers_dir().join(name).join("state.json");
    if let Ok(data) = std::fs::read_to_string(&cli_path) {
        // Parse just the fields we need with serde_json::Value to avoid
        // coupling to the CLI crate's ContainerState type.
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&data) {
            if let Some(ip) = v.get("bridge_ip").and_then(|v| v.as_str()) {
                if !ip.is_empty() {
                    // Check liveness
                    if let Some(pid) = v.get("pid").and_then(|v| v.as_i64()) {
                        if pid > 0 && unsafe { libc::kill(pid as i32, 0) } == 0 {
                            return Ok(ip.to_string());
                        }
                    }
                    return Err(io::Error::other(format!(
                        "linked container '{}' is not running",
                        name
                    )));
                }
            }
            return Err(io::Error::other(format!(
                "linked container '{}' has no bridge IP (is it using bridge networking?)",
                name
            )));
        }
    }

    // Try OCI state.
    let oci_path = crate::paths::oci_state_dir(name).join("state.json");
    if let Ok(data) = std::fs::read_to_string(&oci_path) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&data) {
            if let Some(ip) = v.get("bridge_ip").and_then(|v| v.as_str()) {
                if !ip.is_empty() {
                    if let Some(pid) = v.get("pid").and_then(|v| v.as_i64()) {
                        if pid > 0 && unsafe { libc::kill(pid as i32, 0) } == 0 {
                            return Ok(ip.to_string());
                        }
                    }
                    return Err(io::Error::other(format!(
                        "linked container '{}' is not running",
                        name
                    )));
                }
            }
            return Err(io::Error::other(format!(
                "linked container '{}' has no bridge IP (is it using bridge networking?)",
                name
            )));
        }
    }

    Err(io::Error::other(format!(
        "container '{}' not found (searched CLI and OCI state)",
        name
    )))
}

/// Resolve a container's IP on a network shared with this container.
///
/// Reads the target container's `state.json`, checks the `network_ips` map
/// for any network in `my_networks`. Returns the first match.
pub fn resolve_container_ip_on_shared_network(
    name: &str,
    my_networks: &[String],
) -> io::Result<String> {
    let cli_path = crate::paths::containers_dir().join(name).join("state.json");
    if let Ok(data) = std::fs::read_to_string(&cli_path) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&data) {
            // Check liveness first.
            if let Some(pid) = v.get("pid").and_then(|v| v.as_i64()) {
                if pid <= 0 || unsafe { libc::kill(pid as i32, 0) } != 0 {
                    return Err(io::Error::other(format!(
                        "linked container '{}' is not running",
                        name
                    )));
                }
            }
            // Check network_ips map for a shared network.
            if let Some(ips) = v.get("network_ips").and_then(|v| v.as_object()) {
                for net_name in my_networks {
                    if let Some(ip) = ips.get(net_name).and_then(|v| v.as_str()) {
                        return Ok(ip.to_string());
                    }
                }
            }
        }
    }
    Err(io::Error::other(format!(
        "container '{}' has no IP on a shared network",
        name
    )))
}

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
        // Bit positions match the Linux capability numbers exactly.
        // See linux/capability.h; the u64 bitmask is split lo/hi for capset(2).
        const CHOWN              = 1 <<  0; // CAP_CHOWN
        const DAC_OVERRIDE       = 1 <<  1; // CAP_DAC_OVERRIDE
        const DAC_READ_SEARCH    = 1 <<  2; // CAP_DAC_READ_SEARCH
        const FOWNER             = 1 <<  3; // CAP_FOWNER
        const FSETID             = 1 <<  4; // CAP_FSETID
        const KILL               = 1 <<  5; // CAP_KILL
        const SETGID             = 1 <<  6; // CAP_SETGID
        const SETUID             = 1 <<  7; // CAP_SETUID
        const SETPCAP            = 1 <<  8; // CAP_SETPCAP
        const LINUX_IMMUTABLE    = 1 <<  9; // CAP_LINUX_IMMUTABLE
        const NET_BIND_SERVICE   = 1 << 10; // CAP_NET_BIND_SERVICE
        const NET_BROADCAST      = 1 << 11; // CAP_NET_BROADCAST
        const NET_ADMIN          = 1 << 12; // CAP_NET_ADMIN
        const NET_RAW            = 1 << 13; // CAP_NET_RAW
        const IPC_LOCK           = 1 << 14; // CAP_IPC_LOCK
        const IPC_OWNER          = 1 << 15; // CAP_IPC_OWNER
        const SYS_MODULE         = 1 << 16; // CAP_SYS_MODULE
        const SYS_RAWIO          = 1 << 17; // CAP_SYS_RAWIO
        const SYS_CHROOT         = 1 << 18; // CAP_SYS_CHROOT
        const SYS_PTRACE         = 1 << 19; // CAP_SYS_PTRACE
        const SYS_PACCT          = 1 << 20; // CAP_SYS_PACCT
        const SYS_ADMIN          = 1 << 21; // CAP_SYS_ADMIN
        const SYS_BOOT           = 1 << 22; // CAP_SYS_BOOT
        const SYS_NICE           = 1 << 23; // CAP_SYS_NICE
        const SYS_RESOURCE       = 1 << 24; // CAP_SYS_RESOURCE
        const SYS_TIME           = 1 << 25; // CAP_SYS_TIME
        const SYS_TTY_CONFIG     = 1 << 26; // CAP_SYS_TTY_CONFIG
        const MKNOD              = 1 << 27; // CAP_MKNOD
        const LEASE              = 1 << 28; // CAP_LEASE
        const AUDIT_WRITE        = 1 << 29; // CAP_AUDIT_WRITE
        const AUDIT_CONTROL      = 1 << 30; // CAP_AUDIT_CONTROL
        const SETFCAP            = 1 << 31; // CAP_SETFCAP
        const MAC_OVERRIDE       = 1 << 32; // CAP_MAC_OVERRIDE
        const MAC_ADMIN          = 1 << 33; // CAP_MAC_ADMIN
        const SYSLOG             = 1 << 34; // CAP_SYSLOG
        const WAKE_ALARM         = 1 << 35; // CAP_WAKE_ALARM
        const BLOCK_SUSPEND      = 1 << 36; // CAP_BLOCK_SUSPEND
        const AUDIT_READ         = 1 << 37; // CAP_AUDIT_READ
        const PERFMON            = 1 << 38; // CAP_PERFMON
        const BPF                = 1 << 39; // CAP_BPF
        const CHECKPOINT_RESTORE = 1 << 40; // CAP_CHECKPOINT_RESTORE
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

/// A kernel filesystem mount (proc, sysfs, devpts, mqueue, cgroup, etc.).
///
/// Used by the OCI bundle handler to mount special filesystems that are specified
/// in `config.json` rather than being auto-detected by remora.
#[derive(Debug, Clone)]
pub struct KernelMount {
    /// Filesystem type passed to `mount(2)` (e.g. `"proc"`, `"sysfs"`, `"devpts"`).
    pub fs_type: String,
    /// Source argument passed to `mount(2)` (often same as `fs_type` or `"none"`).
    pub source: String,
    /// Absolute path inside the container where the fs is mounted.
    pub target: PathBuf,
    /// `MS_*` mount flags (e.g. `MS_NOSUID | MS_NOEXEC`).
    pub flags: libc::c_ulong,
    /// Optional data string (e.g. `"newinstance,ptmxmode=0666"` for devpts).
    pub data: String,
}

/// Overlay filesystem configuration — lower layer is `chroot_dir`; upper and work
/// are user-supplied. The merged mount point is managed by Remora.
#[derive(Debug, Clone)]
pub struct OverlayConfig {
    /// Writable layer — container writes land here; persists after container exit.
    pub upper_dir: PathBuf,
    /// Required by overlayfs; must be on the same filesystem as `upper_dir`.
    pub work_dir: PathBuf,
    /// Additional lower layers (top-first). When non-empty, these are used as the
    /// overlayfs `lowerdir=` stack instead of the single `chroot_dir`.
    pub lower_dirs: Vec<PathBuf>,
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
        crate::paths::volumes_dir()
    }

    /// Create a new named volume, creating the backing directory if needed.
    pub fn create(name: &str) -> io::Result<Self> {
        let path = Self::volumes_dir().join(name);
        std::fs::create_dir_all(&path)?;
        Ok(Self {
            name: name.to_string(),
            path,
        })
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
        Ok(Self {
            name: name.to_string(),
            path,
        })
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
///     .args(["-c", "echo hello"])
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
///     .args(["-la"])
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
    readonly_paths: Vec<PathBuf>,     // Paths to remount read-only
    // Filesystem mounts
    bind_mounts: Vec<BindMount>,
    tmpfs_mounts: Vec<TmpfsMount>,
    kernel_mounts: Vec<KernelMount>,
    // Resource limits
    rlimits: Vec<ResourceLimit>,
    // Cgroup-based resource management
    cgroup_config: Option<crate::cgroup::CgroupConfig>,
    // Network configuration
    network_config: Option<crate::network::NetworkConfig>,
    // Whether to enable NAT (MASQUERADE) for bridge-mode containers.
    nat: bool,
    // Port-forward rules: (host_port, container_port, proto). Requires Bridge + NAT.
    port_forwards: Vec<(u16, u16, crate::network::PortProto)>,
    // DNS servers to write into the container's /etc/resolv.conf.
    dns_servers: Vec<String>,
    // Overlay filesystem (upper + work dirs; lower = chroot_dir).
    overlay: Option<OverlayConfig>,
    // OCI sync: (ready_write_fd, listen_fd). Used by cmd_create to block the container
    // in pre_exec until "remora start" connects to exec.sock.
    oci_sync: Option<(i32, i32)>,
    // PTY slave fd for OCI terminal mode (process.terminal = true).
    // When set, pre_exec calls setsid()+dup2(slave,0/1/2)+TIOCSCTTY before exec.
    pty_slave: Option<i32>,
    // Container working directory (set after chroot; relative to new root).
    container_cwd: Option<PathBuf>,
    // Sysctl key=value pairs to write to /proc/sys/ in pre_exec.
    sysctl: Vec<(String, String)>,
    // Device nodes to create inside the container in pre_exec.
    devices: Vec<DeviceNode>,
    // Pre-compiled seccomp BPF program (takes priority over seccomp_profile).
    seccomp_program: Option<seccompiler::BpfProgram>,
    // Mount propagation flags applied to the rootfs mountpoint after pivot_root/chroot.
    // None → default (MS_PRIVATE|MS_REC). Some(flags) → apply those flags instead.
    rootfs_propagation: Option<libc::c_ulong>,
    // Hostname to set inside the container's UTS namespace.
    hostname: Option<String>,
    // Whether to use newuidmap/newgidmap helpers for multi-range UID/GID mapping.
    use_id_helpers: bool,
    // Container links: (container_name, alias) → resolved to /etc/hosts entries at spawn time.
    links: Vec<(String, String)>,
    // Additional bridge networks to attach (secondary interfaces: eth1, eth2, ...).
    additional_networks: Vec<String>,
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
            readonly_paths: Vec::new(),
            bind_mounts: Vec::new(),
            tmpfs_mounts: Vec::new(),
            kernel_mounts: Vec::new(),
            rlimits: Vec::new(),
            cgroup_config: None,
            network_config: None,
            nat: false,
            port_forwards: Vec::new(),
            dns_servers: Vec::new(),
            overlay: None,
            oci_sync: None,
            pty_slave: None,
            container_cwd: None,
            sysctl: Vec::new(),
            devices: Vec::new(),
            seccomp_program: None,
            rootfs_propagation: None,
            hostname: None,
            links: Vec::new(),
            use_id_helpers: false,
            additional_networks: Vec::new(),
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

    /// OR additional namespace flags into the current set without clearing existing flags.
    pub fn add_namespaces(mut self, namespaces: Namespace) -> Self {
        self.namespaces |= namespaces;
        self
    }

    /// Return the current namespace flags.
    pub fn namespaces(&self) -> Namespace {
        self.namespaces
    }

    /// Legacy API: accepts iterator of namespace references (for backwards compatibility)
    #[deprecated(since = "0.2.0", note = "Use with_namespaces() with bitflags instead")]
    pub fn unshare<'a, I>(mut self, namespaces: I) -> Self
    where
        I: IntoIterator<Item = &'a Namespace>,
    {
        self.namespaces = namespaces
            .into_iter()
            .fold(Namespace::empty(), |acc, &ns| acc | ns);
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
        resource: RlimitResource,
        soft: libc::rlim_t,
        hard: libc::rlim_t,
    ) -> Self {
        self.rlimits.push(ResourceLimit {
            resource,
            soft,
            hard,
        });
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
        self.cgroup_config
            .get_or_insert_with(Default::default)
            .memory_limit = Some(bytes);
        self
    }

    /// Set the CPU weight (shares) for the container's cgroup.
    ///
    /// Maps to `cpu.weight` in cgroups v2 (range 1–10000; default 100) and
    /// `cpu.shares` in v1. Higher values receive proportionally more CPU time.
    pub fn with_cgroup_cpu_shares(mut self, shares: u64) -> Self {
        self.cgroup_config
            .get_or_insert_with(Default::default)
            .cpu_shares = Some(shares);
        self
    }

    /// Set a CPU quota for the container's cgroup.
    ///
    /// `quota_us` is the maximum CPU time (in microseconds) the container may
    /// use per `period_us`. Example: `(50_000, 100_000)` = 50% of one CPU core.
    pub fn with_cgroup_cpu_quota(mut self, quota_us: i64, period_us: u64) -> Self {
        self.cgroup_config
            .get_or_insert_with(Default::default)
            .cpu_quota = Some((quota_us, period_us));
        self
    }

    /// Set the maximum number of processes/threads in the container's cgroup.
    ///
    /// Maps to `pids.max`. Forks beyond this limit will fail with `EAGAIN`.
    pub fn with_cgroup_pids_limit(mut self, max: u64) -> Self {
        self.cgroup_config
            .get_or_insert_with(Default::default)
            .pids_limit = Some(max);
        self
    }

    /// Override the cgroup path/name for this container (OCI `linux.cgroupsPath`).
    ///
    /// By default remora names the cgroup `remora-{child_pid}`. When the OCI config
    /// specifies `linux.cgroupsPath`, pass it here to use that name instead.
    pub fn with_cgroup_path(mut self, path: impl Into<String>) -> Self {
        self.cgroup_config.get_or_insert_with(Default::default).path = Some(path.into());
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
        // Normalize Bridge → BridgeNamed("remora0") so internal code only
        // needs to match BridgeNamed(_).
        let mode = match mode {
            crate::network::NetworkMode::Bridge => {
                crate::network::NetworkMode::BridgeNamed("remora0".into())
            }
            other => other,
        };
        // Loopback requires a new NET namespace (unshare in pre_exec).
        // Bridge does NOT unshare NET — the child joins a pre-configured named
        // netns via setns() in pre_exec instead.
        if mode == crate::network::NetworkMode::Loopback {
            self.namespaces |= Namespace::NET;
        }
        self.network_config = Some(crate::network::NetworkConfig { mode });
        self
    }

    /// Attach an additional bridge network to this container.
    ///
    /// The container must already have a primary bridge network set via
    /// [`with_network`]. Each additional network gets a secondary interface
    /// (`eth1`, `eth2`, ...) with a subnet route only (no default route).
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use remora::container::Command;
    /// # use remora::network::NetworkMode;
    /// let cmd = Command::new("/bin/sh")
    ///     .with_network(NetworkMode::BridgeNamed("frontend".into()))
    ///     .with_additional_network("backend");
    /// ```
    pub fn with_additional_network(mut self, network_name: &str) -> Self {
        self.additional_networks.push(network_name.to_string());
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
        self.port_forwards
            .push((host_port, container_port, crate::network::PortProto::Tcp));
        self
    }

    /// Map `host_port` → `container_port` for UDP traffic.
    pub fn with_port_forward_udp(mut self, host_port: u16, container_port: u16) -> Self {
        self.port_forwards
            .push((host_port, container_port, crate::network::PortProto::Udp));
        self
    }

    /// Map `host_port` → `container_port` for both TCP and UDP traffic.
    pub fn with_port_forward_both(mut self, host_port: u16, container_port: u16) -> Self {
        self.port_forwards
            .push((host_port, container_port, crate::network::PortProto::Both));
        self
    }

    /// Write DNS nameservers into the container's `/etc/resolv.conf`.
    ///
    /// Writes nameserver lines to a per-container temp file under
    /// `/run/remora/dns-{pid}-{n}/resolv.conf` (never touches the shared rootfs)
    /// and bind-mounts it over `/etc/resolv.conf` inside the container.
    /// The temp file is removed in `wait()` / `wait_with_output()`.
    ///
    /// Requires [`Namespace::MOUNT`] (so the bind mount stays inside the
    /// container's private mount namespace) and [`with_chroot`](Self::with_chroot).
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use remora::network::NetworkMode;
    /// Command::new("/bin/sh")
    ///     .with_network(NetworkMode::Bridge)
    ///     .with_nat()
    ///     .with_dns(&["1.1.1.1", "8.8.8.8"])
    ///     .spawn()?;
    /// ```
    pub fn with_dns<S: AsRef<str>>(mut self, servers: &[S]) -> Self {
        self.dns_servers = servers.iter().map(|s| s.as_ref().to_owned()).collect();
        self
    }

    /// Link to another running container by name.
    ///
    /// At spawn time, the target container's bridge IP is looked up and an
    /// `/etc/hosts` entry is injected via bind-mount. Requires both containers
    /// to use bridge networking, and requires [`Namespace::MOUNT`] + [`with_chroot`](Self::with_chroot).
    ///
    /// The container name is used as the hostname alias.
    pub fn with_link(mut self, container_name: &str) -> Self {
        self.links
            .push((container_name.to_string(), container_name.to_string()));
        self
    }

    /// Link to another running container with a custom alias.
    ///
    /// Like [`with_link`](Self::with_link), but the target is reachable by `alias`
    /// in addition to its original name.
    pub fn with_link_alias(mut self, container_name: &str, alias: &str) -> Self {
        self.links
            .push((container_name.to_string(), alias.to_string()));
        self
    }

    /// Mount an overlay filesystem on top of the chroot rootfs.
    ///
    /// Requires [`Namespace::MOUNT`] and [`with_chroot`](Self::with_chroot).
    /// Container writes land in `upper_dir` (visible on the host after exit);
    /// the lower layer (`chroot_dir`) is never modified.
    ///
    /// `upper_dir` and `work_dir` must be on the same filesystem and must not
    /// themselves reside on an overlayfs mount.
    ///
    /// The merged mount point is created by Remora at
    /// `/run/remora/overlay-{pid}-{n}/merged/` and removed after `wait()`.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// Command::new("/bin/sh")
    ///     .with_chroot("/shared/alpine-rootfs")
    ///     .with_namespaces(Namespace::MOUNT | Namespace::UTS)
    ///     .with_overlay("/scratch/upper", "/scratch/work")
    ///     .spawn()?;
    /// ```
    pub fn with_overlay<P1: Into<PathBuf>, P2: Into<PathBuf>>(
        mut self,
        upper_dir: P1,
        work_dir: P2,
    ) -> Self {
        self.overlay = Some(OverlayConfig {
            upper_dir: upper_dir.into(),
            work_dir: work_dir.into(),
            lower_dirs: Vec::new(),
        });
        self
    }

    /// Set up a multi-layer overlay from pre-extracted OCI image layers.
    ///
    /// `layer_dirs` must be ordered **top-first** (as overlayfs expects for `lowerdir=`).
    /// The bottom (last) layer is used as the chroot directory.  An ephemeral upper and
    /// work directory are auto-created under `/run/remora/overlay-{pid}-{n}/` and removed
    /// after `wait()`.
    ///
    /// Automatically enables `Namespace::MOUNT` and `/proc` mount.
    ///
    /// Do not combine with `with_chroot()` or `with_overlay()` — this method sets both.
    pub fn with_image_layers(mut self, layer_dirs: Vec<PathBuf>) -> Self {
        assert!(
            !layer_dirs.is_empty(),
            "with_image_layers requires at least one layer"
        );
        // Bottom layer (last element) serves as the chroot anchor.
        self.chroot_dir = Some(layer_dirs.last().unwrap().clone());
        self.overlay = Some(OverlayConfig {
            upper_dir: PathBuf::new(), // placeholder — auto-created by spawn
            work_dir: PathBuf::new(),  // placeholder — auto-created by spawn
            lower_dirs: layer_dirs,
        });
        self.namespaces |= Namespace::MOUNT;
        self.mount_proc = true;
        self.mount_dev = true;
        self
    }

    /// Clear the environment for the child process (inherit nothing from parent).
    ///
    /// After calling this, only environment variables set via [`env`](Self::env)
    /// will be present in the container. Used by OCI `build_command` to apply
    /// exactly the env specified in `config.json`.
    pub fn env_clear(mut self) -> Self {
        self.inner.env_clear();
        self
    }

    /// Set the working directory inside the container (applied after chroot).
    ///
    /// Must be an absolute path relative to the new root. Defaults to `/`.
    /// Used by OCI to apply `process.cwd` from `config.json`.
    pub fn with_cwd<P: Into<PathBuf>>(mut self, path: P) -> Self {
        self.container_cwd = Some(path.into());
        self
    }

    /// Set the hostname inside the container.
    ///
    /// Requires `Namespace::UTS` to be active; the hostname is set via
    /// `sethostname(2)` in the container's UTS namespace after unshare.
    pub fn with_hostname(mut self, name: impl Into<String>) -> Self {
        self.hostname = Some(name.into());
        self
    }

    /// Configure OCI create/start synchronization.
    ///
    /// Internal — used by `remora create`. The child's pre_exec writes its PID
    /// to `ready_write_fd`, then blocks on `accept(listen_fd)` waiting for
    /// `remora start` to connect and send a byte.
    pub fn with_oci_sync(mut self, ready_write_fd: i32, listen_fd: i32) -> Self {
        self.oci_sync = Some((ready_write_fd, listen_fd));
        self
    }

    /// Wire a PTY slave fd as the container's stdin/stdout/stderr.
    ///
    /// Used by `remora create` when `process.terminal: true`. The pre_exec
    /// closure calls `setsid()`, `dup2(slave, 0/1/2)`, and `TIOCSCTTY` so the
    /// container process gets a controlling terminal backed by the PTY.
    ///
    /// The slave fd must NOT be `O_CLOEXEC` — it must survive the fork chain
    /// to reach pre_exec.  The caller closes the slave in the parent after fork
    /// and sends the master fd to `--console-socket` via `SCM_RIGHTS`.
    pub fn with_pty_slave(mut self, slave_fd: i32) -> Self {
        self.pty_slave = Some(slave_fd);
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

    /// Make specific paths inside the container read-only.
    ///
    /// Each path is bind-mounted to itself, then remounted with `MS_RDONLY`.
    /// This is equivalent to `linux.readonlyPaths` in an OCI config.
    pub fn with_readonly_paths(mut self, paths: &[&str]) -> Self {
        self.readonly_paths = paths.iter().map(PathBuf::from).collect();
        self
    }

    /// Set a kernel parameter inside the container's UTS/network namespace.
    ///
    /// Equivalent to `linux.sysctl` in OCI config. The key uses dot notation
    /// (e.g. `"net.ipv4.ip_forward"`); it is translated to `/proc/sys/net/ipv4/ip_forward`
    /// and written in pre_exec.
    pub fn with_sysctl(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.sysctl.push((key.into(), value.into()));
        self
    }

    /// Create a device node inside the container.
    ///
    /// Equivalent to `linux.devices` in OCI config. The node is created with
    /// `mknod` in pre_exec after chroot, so `path` is relative to the container root.
    pub fn with_device(mut self, device: DeviceNode) -> Self {
        self.devices.push(device);
        self
    }

    /// Apply a pre-compiled seccomp BPF program instead of a named profile.
    ///
    /// Takes priority over `with_seccomp_default()` / `with_seccomp_profile()`.
    /// Used by the OCI `linux.seccomp` path.
    pub fn with_seccomp_program(mut self, program: seccompiler::BpfProgram) -> Self {
        self.seccomp_program = Some(program);
        self
    }

    /// Override the rootfs mount propagation applied after chroot/pivot_root.
    ///
    /// Pass `MS_SHARED`, `MS_SLAVE`, `MS_PRIVATE`, or `MS_UNBINDABLE` (optionally OR'd
    /// with `MS_REC`). By default remora applies `MS_PRIVATE | MS_REC`.
    pub fn with_rootfs_propagation(mut self, flags: libc::c_ulong) -> Self {
        self.rootfs_propagation = Some(flags);
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
        self.bind_mounts.push(BindMount {
            source: source.into(),
            target: target.into(),
            readonly: false,
        });
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
        self.bind_mounts.push(BindMount {
            source: source.into(),
            target: target.into(),
            readonly: true,
        });
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
        self.tmpfs_mounts.push(TmpfsMount {
            target: target.into(),
            options: options.to_string(),
        });
        self
    }

    /// Mount a kernel filesystem (proc, sysfs, devpts, mqueue, cgroup2, …) at `target`.
    ///
    /// Used by the OCI bundle handler to honour arbitrary `mounts` entries from
    /// `config.json`. `flags` should be `MS_*` constants from `libc`; `data` is
    /// passed verbatim to the kernel (e.g. `"newinstance,ptmxmode=0666"` for devpts).
    ///
    /// The mount is performed inside the container's mount namespace, after chroot/pivot_root.
    pub fn with_kernel_mount<P: Into<PathBuf>>(
        mut self,
        fs_type: impl Into<String>,
        source: impl Into<String>,
        target: P,
        flags: libc::c_ulong,
        data: impl Into<String>,
    ) -> Self {
        self.kernel_mounts.push(KernelMount {
            fs_type: fs_type.into(),
            source: source.into(),
            target: target.into(),
            flags,
            data: data.into(),
        });
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
        let seccomp_filter: Option<seccompiler::BpfProgram> =
            if let Some(prog) = self.seccomp_program.take() {
                Some(prog)
            } else if let Some(profile) = &self.seccomp_profile {
                match profile {
                    SeccompProfile::Docker => {
                        Some(crate::seccomp::docker_default_filter().map_err(Error::Seccomp)?)
                    }
                    SeccompProfile::Minimal => {
                        Some(crate::seccomp::minimal_filter().map_err(Error::Seccomp)?)
                    }
                    SeccompProfile::None => None,
                }
            } else {
                None
            };

        // Open namespace files in parent process (can't safely open files in pre_exec)
        // Keep File objects alive so their fds remain valid through spawn
        let join_ns_files: Vec<(File, Namespace)> = self
            .join_namespaces
            .iter()
            .map(|(path, ns)| File::open(path).map(|f| (f, *ns)).map_err(Error::Io))
            .collect::<Result<Vec<_>, _>>()?;

        // Extract raw fds for use in pre_exec
        let join_ns_fds: Vec<(i32, Namespace)> = join_ns_files
            .iter()
            .map(|(f, ns)| (f.as_raw_fd(), *ns))
            .collect();

        // Detect rootless mode (running as non-root) and auto-configure.
        let is_rootless = unsafe { libc::getuid() } != 0;
        if is_rootless {
            // Unprivileged containers require a user namespace.
            self.namespaces |= Namespace::USER;
            let host_uid = unsafe { libc::getuid() };
            let host_gid = unsafe { libc::getgid() };

            // Try multi-range subordinate UID/GID mapping via newuidmap/newgidmap.
            if self.uid_maps.is_empty() {
                if crate::idmap::has_newuidmap() && crate::idmap::has_newgidmap() {
                    if let Ok(username) = crate::idmap::current_username() {
                        let uid_ranges = crate::idmap::parse_subid_file(
                            std::path::Path::new("/etc/subuid"),
                            &username,
                            host_uid,
                        )
                        .unwrap_or_default();
                        let gid_ranges = crate::idmap::parse_subid_file(
                            std::path::Path::new("/etc/subgid"),
                            &username,
                            host_gid,
                        )
                        .unwrap_or_default();

                        if !uid_ranges.is_empty() && !gid_ranges.is_empty() {
                            self.uid_maps.push(UidMap {
                                inside: 0,
                                outside: host_uid,
                                count: 1,
                            });
                            self.uid_maps.push(UidMap {
                                inside: 1,
                                outside: uid_ranges[0].start,
                                count: uid_ranges[0].count,
                            });
                            self.gid_maps.push(GidMap {
                                inside: 0,
                                outside: host_gid,
                                count: 1,
                            });
                            self.gid_maps.push(GidMap {
                                inside: 1,
                                outside: gid_ranges[0].start,
                                count: gid_ranges[0].count,
                            });
                            self.use_id_helpers = true;
                            log::info!(
                                "rootless multi-UID: {} subordinate UIDs, {} subordinate GIDs",
                                uid_ranges[0].count,
                                gid_ranges[0].count
                            );
                        }
                    }
                }
                // Fallback: single-UID map (current behavior).
                if self.uid_maps.is_empty() {
                    self.uid_maps.push(UidMap {
                        inside: 0,
                        outside: host_uid,
                        count: 1,
                    });
                }
                if self.gid_maps.is_empty() {
                    self.gid_maps.push(GidMap {
                        inside: 0,
                        outside: host_gid,
                        count: 1,
                    });
                }
            }
            // Bridge networking requires root-level capabilities on the host network.
            if self
                .network_config
                .as_ref()
                .is_some_and(|c| c.mode.is_bridge())
            {
                return Err(Error::Io(io::Error::other(
                    "NetworkMode::Bridge requires root; use NetworkMode::Pasta for rootless internet access",
                )));
            }
        }

        // Pasta mode: validate pasta is available and auto-add NET namespace.
        let is_pasta = self
            .network_config
            .as_ref()
            .is_some_and(|c| c.mode == crate::network::NetworkMode::Pasta);
        if is_pasta {
            if !crate::network::is_pasta_available() {
                return Err(Error::Io(io::Error::other(
                    "NetworkMode::Pasta requires pasta — install from https://passt.top",
                )));
            }
            self.namespaces |= Namespace::NET;
        }

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
        let readonly_paths = self.readonly_paths.clone();
        let sysctl = self.sysctl.clone();
        let devices = self.devices.clone();
        let bind_mounts = self.bind_mounts.clone();
        let tmpfs_mounts = self.tmpfs_mounts.clone();
        let kernel_mounts = self.kernel_mounts.clone();
        let rootfs_propagation = self.rootfs_propagation;
        let hostname = self.hostname.clone();
        let use_id_helpers = self.use_id_helpers;
        // Loopback/Pasta mode: bring up lo inside pre_exec (after unshare(NEWNET)).
        // Bridge mode uses setns instead — lo is configured by setup_bridge_network.
        let bring_up_loopback = self.network_config.as_ref().is_some_and(|c| {
            c.mode == crate::network::NetworkMode::Loopback
                || c.mode == crate::network::NetworkMode::Pasta
        });
        let bridge_network_name: Option<String> = self
            .network_config
            .as_ref()
            .and_then(|c| c.mode.bridge_network_name().map(|s| s.to_owned()));
        // Bridge mode: create and fully configure the named netns BEFORE fork.
        // The child's pre_exec will join it via setns — no race whatsoever.
        let bridge_network: Option<crate::network::NetworkSetup> =
            if let Some(ref net_name) = bridge_network_name {
                let ns_name = crate::network::generate_ns_name();
                Some(
                    crate::network::setup_bridge_network(
                        &ns_name,
                        net_name,
                        self.nat,
                        self.port_forwards.clone(),
                    )
                    .map_err(Error::Io)?,
                )
            } else {
                None
            };
        // Pre-allocate the netns path CString so pre_exec can open it without allocating.
        let bridge_ns_path: Option<std::ffi::CString> = bridge_network
            .as_ref()
            .map(|n| std::ffi::CString::new(format!("/run/netns/{}", n.ns_name)).unwrap());

        // Attach additional bridge networks to the same netns (secondary interfaces).
        let mut secondary_networks: Vec<crate::network::NetworkSetup> = Vec::new();
        if let Some(ref primary) = bridge_network {
            for (i, net_name) in self.additional_networks.iter().enumerate() {
                let iface = format!("eth{}", i + 1);
                secondary_networks.push(
                    crate::network::attach_network_to_netns(&primary.ns_name, net_name, &iface)
                        .map_err(Error::Io)?,
                );
            }
        }

        // Validate overlay prerequisites before fork.
        if self.overlay.is_some() && !self.namespaces.contains(Namespace::MOUNT) {
            return Err(Error::Io(io::Error::other(
                "with_overlay requires Namespace::MOUNT",
            )));
        }
        if self.overlay.is_some() && self.chroot_dir.is_none() {
            return Err(Error::Io(io::Error::other(
                "with_overlay requires with_chroot",
            )));
        }

        // Create the overlay merged dir before fork. The actual mount happens in
        // pre_exec (after unshare(NEWNS)), but the directory must exist first.
        // When upper/work are empty (image-layer mode), auto-create them as siblings of merged.
        let overlay_merged_dir: Option<PathBuf> = if let Some(ref mut ov) = self.overlay {
            let pid = unsafe { libc::getpid() };
            let n = OVERLAY_COUNTER.fetch_add(1, Ordering::Relaxed);
            let base = crate::paths::overlay_base(pid, n);
            let merged = base.join("merged");
            std::fs::create_dir_all(&merged).map_err(Error::Io)?;
            // Auto-create ephemeral upper/work for image-layer mode.
            // Directories are 0755 so that after setuid() to a non-root UID
            // the overlay merged view remains accessible (the kernel checks
            // upper/work dir permissions against the caller's fsuid).
            if ov.upper_dir.as_os_str().is_empty() {
                let upper = base.join("upper");
                let work = base.join("work");
                std::fs::create_dir_all(&upper).map_err(Error::Io)?;
                std::fs::create_dir_all(&work).map_err(Error::Io)?;
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o755));
                let _ = std::fs::set_permissions(&upper, std::fs::Permissions::from_mode(0o755));
                let _ = std::fs::set_permissions(&work, std::fs::Permissions::from_mode(0o755));
                let _ = std::fs::set_permissions(&merged, std::fs::Permissions::from_mode(0o755));
                ov.upper_dir = upper;
                ov.work_dir = work;
            }
            Some(merged)
        } else {
            None
        };

        // Pre-allocate CStrings for the overlay mount (lower, upper, work, merged).
        // Must be done in the parent — no allocation allowed in pre_exec.
        let overlay_cstrings: Option<(
            std::ffi::CString,
            std::ffi::CString,
            std::ffi::CString,
            std::ffi::CString,
        )> = match (&self.overlay, &overlay_merged_dir) {
            (Some(ov), Some(merged)) => {
                use std::os::unix::ffi::OsStrExt as _;
                // Build lowerdir: use lower_dirs if present, else chroot_dir.
                let lower_str = if !ov.lower_dirs.is_empty() {
                    ov.lower_dirs
                        .iter()
                        .map(|p| p.to_string_lossy().into_owned())
                        .collect::<Vec<_>>()
                        .join(":")
                } else {
                    self.chroot_dir
                        .as_ref()
                        .unwrap()
                        .to_string_lossy()
                        .into_owned()
                };
                let cstrings = (
                    std::ffi::CString::new(lower_str.as_bytes()).unwrap(),
                    std::ffi::CString::new(ov.upper_dir.as_os_str().as_bytes()).unwrap(),
                    std::ffi::CString::new(ov.work_dir.as_os_str().as_bytes()).unwrap(),
                    std::ffi::CString::new(merged.as_os_str().as_bytes()).unwrap(),
                );
                log::debug!(
                    "overlay config: lowerdir={} upperdir={} workdir={} merged={}",
                    cstrings.0.to_string_lossy(),
                    cstrings.1.to_string_lossy(),
                    cstrings.2.to_string_lossy(),
                    cstrings.3.to_string_lossy(),
                );
                // Verify each lower dir exists before fork so the error is clear.
                for lower_path in &ov.lower_dirs {
                    if !lower_path.is_dir() {
                        return Err(Error::Io(io::Error::other(format!(
                            "overlay lowerdir does not exist: {}",
                            lower_path.display()
                        ))));
                    }
                }
                Some(cstrings)
            }
            _ => None,
        };

        // Rootless overlay: decide between native overlay+userxattr vs fuse-overlayfs.
        let mut fuse_overlay_child: Option<std::process::Child> = None;
        let mut fuse_overlay_merged: Option<PathBuf> = None;
        let use_fuse_overlay: bool;
        if is_rootless && self.overlay.is_some() {
            if native_rootless_overlay_supported() {
                log::debug!("rootless overlay: using native overlay+userxattr");
                use_fuse_overlay = false;
            } else if is_fuse_overlayfs_available() {
                log::info!("rootless overlay: falling back to fuse-overlayfs");
                // Spawn fuse-overlayfs before fork.
                if let (Some(ov), Some(merged)) = (&self.overlay, &overlay_merged_dir) {
                    let lower_str = if !ov.lower_dirs.is_empty() {
                        ov.lower_dirs
                            .iter()
                            .map(|p| p.to_string_lossy().into_owned())
                            .collect::<Vec<_>>()
                            .join(":")
                    } else {
                        self.chroot_dir
                            .as_ref()
                            .unwrap()
                            .to_string_lossy()
                            .into_owned()
                    };
                    let child =
                        spawn_fuse_overlayfs(&lower_str, &ov.upper_dir, &ov.work_dir, merged)
                            .map_err(Error::Io)?;
                    fuse_overlay_merged = Some(merged.clone());
                    fuse_overlay_child = Some(child);
                    // Give fuse-overlayfs a moment to mount.
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
                use_fuse_overlay = true;
            } else {
                return Err(Error::Io(io::Error::other(
                    "rootless overlay requires kernel 5.11+ or fuse-overlayfs; \
                     install fuse-overlayfs or run as root",
                )));
            }
        } else {
            use_fuse_overlay = false;
        }

        // Collect OCI sync fds (captured by value — i32 is Copy).
        let oci_sync = self.oci_sync;
        let pty_slave = self.pty_slave;
        let container_cwd = self.container_cwd.clone();

        // DNS: auto-inject bridge gateway IP(s) as primary nameservers for the
        // embedded DNS daemon, then append user-specified --dns servers as fallback.
        let mut auto_dns: Vec<String> = Vec::new();
        if let Some(ref net) = bridge_network {
            if let Ok(net_def) = crate::network::load_network_def(&net.network_name) {
                auto_dns.push(net_def.gateway.to_string());
            }
        }
        for sec in &secondary_networks {
            if let Ok(net_def) = crate::network::load_network_def(&sec.network_name) {
                let gw = net_def.gateway.to_string();
                if !auto_dns.contains(&gw) {
                    auto_dns.push(gw);
                }
            }
        }
        // Append user-specified DNS servers as fallback.
        auto_dns.extend(self.dns_servers.iter().cloned());

        // DNS: write nameservers to a per-container temp file; bind-mount into container.
        // Requires Namespace::MOUNT so the bind mount stays in the container's private namespace.
        if !auto_dns.is_empty() {
            if !self.namespaces.contains(Namespace::MOUNT) {
                return Err(Error::Io(io::Error::other(
                    "with_dns requires Namespace::MOUNT",
                )));
            }
            if self.chroot_dir.is_none() {
                return Err(Error::Io(io::Error::other("with_dns requires with_chroot")));
            }
        }
        let dns_temp_dir: Option<PathBuf> = if !auto_dns.is_empty() {
            let pid = unsafe { libc::getpid() };
            let n = DNS_COUNTER.fetch_add(1, Ordering::Relaxed);
            let dir = crate::paths::dns_dir(pid, n);
            std::fs::create_dir_all(&dir).map_err(Error::Io)?;
            let mut content = String::new();
            for s in &auto_dns {
                content.push_str("nameserver ");
                content.push_str(s);
                content.push('\n');
            }
            std::fs::write(dir.join("resolv.conf"), content).map_err(Error::Io)?;
            Some(dir)
        } else {
            None
        };
        // Pre-allocate the CString for the temp resolv.conf path (used in pre_exec).
        let dns_temp_file_cstring: Option<std::ffi::CString> = dns_temp_dir.as_ref().map(|dir| {
            use std::os::unix::ffi::OsStrExt as _;
            std::ffi::CString::new(dir.join("resolv.conf").as_os_str().as_bytes()).unwrap()
        });

        // Links: resolve container names → IPs and write /etc/hosts temp file.
        if !self.links.is_empty() {
            if !self.namespaces.contains(Namespace::MOUNT) {
                return Err(Error::Io(io::Error::other(
                    "with_link requires Namespace::MOUNT",
                )));
            }
            if self.chroot_dir.is_none() {
                return Err(Error::Io(io::Error::other(
                    "with_link requires with_chroot",
                )));
            }
        }
        // Collect this container's network names for smart link resolution.
        let my_networks: Vec<String> = {
            let mut nets = Vec::new();
            if let Some(ref name) = bridge_network_name {
                nets.push(name.clone());
            }
            for name in &self.additional_networks {
                nets.push(name.clone());
            }
            nets
        };
        let hosts_temp_dir: Option<PathBuf> = if !self.links.is_empty() {
            let pid = unsafe { libc::getpid() };
            let n = HOSTS_COUNTER.fetch_add(1, Ordering::Relaxed);
            let dir = crate::paths::hosts_dir(pid, n);
            std::fs::create_dir_all(&dir).map_err(Error::Io)?;
            let mut content = String::from("127.0.0.1\tlocalhost\n");
            for (container_name, alias) in &self.links {
                // Try to resolve on a shared network first, fall back to any IP.
                let ip = resolve_container_ip_on_shared_network(container_name, &my_networks)
                    .or_else(|_| resolve_container_ip(container_name))
                    .map_err(Error::Io)?;
                if alias == container_name {
                    content.push_str(&format!("{}\t{}\n", ip, alias));
                } else {
                    content.push_str(&format!("{}\t{}\t{}\n", ip, alias, container_name));
                }
            }
            std::fs::write(dir.join("hosts"), content).map_err(Error::Io)?;
            Some(dir)
        } else {
            None
        };
        let hosts_temp_file_cstring: Option<std::ffi::CString> =
            hosts_temp_dir.as_ref().map(|dir| {
                use std::os::unix::ffi::OsStrExt as _;
                std::ffi::CString::new(dir.join("hosts").as_os_str().as_bytes()).unwrap()
            });

        // Create idmap sync pipes before the pre_exec closure so it can capture the FDs.
        // (ready_w, done_r) go into the child closure; (ready_r, done_w) stay for the parent thread.
        let (idmap_ready_w, idmap_done_r, idmap_ready_r, idmap_done_w) = if use_id_helpers {
            let mut ready_fds = [0i32; 2];
            let mut done_fds = [0i32; 2];
            if unsafe { libc::pipe(ready_fds.as_mut_ptr()) } != 0
                || unsafe { libc::pipe(done_fds.as_mut_ptr()) } != 0
            {
                return Err(Error::Io(io::Error::last_os_error()));
            }
            (ready_fds[1], done_fds[0], ready_fds[0], done_fds[1])
        } else {
            (-1, -1, -1, -1)
        };

        // Install our combined pre_exec hook
        unsafe {
            self.inner.pre_exec(move || {
                use std::ffi::CString;
                use std::ptr;

                // Step 1: Unshare namespaces.
                if !namespaces.is_empty() {
                    if is_rootless && namespaces.contains(Namespace::USER) {
                        // Rootless two-phase unshare:
                        // 1a. Unshare user namespace alone first.
                        unshare(CloneFlags::CLONE_NEWUSER)
                            .map_err(|e| io::Error::other(format!("unshare USER: {}", e)))?;
                        // 1b. Write uid/gid maps.
                        if use_id_helpers {
                            // Multi-range maps: signal parent thread to run newuidmap/newgidmap.
                            let pid: u32 = libc::getpid() as u32;
                            let pid_bytes = pid.to_ne_bytes();
                            libc::write(
                                idmap_ready_w,
                                pid_bytes.as_ptr() as *const libc::c_void,
                                4,
                            );
                            libc::close(idmap_ready_w);
                            // Block until parent has written the maps.
                            let mut buf = [0u8; 1];
                            libc::read(idmap_done_r, buf.as_mut_ptr() as *mut libc::c_void, 1);
                            libc::close(idmap_done_r);
                        } else {
                            // Single-UID map: write directly to /proc/self/{uid,gid}_map.
                            use std::io::Write;
                            if !gid_maps.is_empty() {
                                let mut sg = std::fs::OpenOptions::new()
                                    .write(true)
                                    .open("/proc/self/setgroups")
                                    .map_err(|e| io::Error::other(format!("setgroups: {}", e)))?;
                                sg.write_all(b"deny\n").map_err(|e| {
                                    io::Error::other(format!("setgroups write: {}", e))
                                })?;
                            }
                            if !uid_maps.is_empty() {
                                let mut content = String::new();
                                for map in &uid_maps {
                                    content.push_str(&format!(
                                        "{} {} {}\n",
                                        map.inside, map.outside, map.count
                                    ));
                                }
                                let mut f = std::fs::OpenOptions::new()
                                    .write(true)
                                    .open("/proc/self/uid_map")
                                    .map_err(|e| io::Error::other(format!("uid_map: {}", e)))?;
                                f.write_all(content.as_bytes()).map_err(|e| {
                                    io::Error::other(format!("uid_map write: {}", e))
                                })?;
                            }
                            if !gid_maps.is_empty() {
                                let mut content = String::new();
                                for map in &gid_maps {
                                    content.push_str(&format!(
                                        "{} {} {}\n",
                                        map.inside, map.outside, map.count
                                    ));
                                }
                                let mut f = std::fs::OpenOptions::new()
                                    .write(true)
                                    .open("/proc/self/gid_map")
                                    .map_err(|e| io::Error::other(format!("gid_map: {}", e)))?;
                                f.write_all(content.as_bytes()).map_err(|e| {
                                    io::Error::other(format!("gid_map write: {}", e))
                                })?;
                            }
                        }
                        // 1c. Unshare remaining namespaces — now with proper uid/gid mapping
                        //     and full capabilities in the user namespace.
                        let remaining = namespaces & !Namespace::USER;
                        if !remaining.is_empty() {
                            unshare(remaining.to_clone_flags())
                                .map_err(|e| io::Error::other(format!("unshare error: {}", e)))?;
                        }
                    } else {
                        // Privileged (root) mode: unshare all namespaces at once.
                        unshare(namespaces.to_clone_flags())
                            .map_err(|e| io::Error::other(format!("unshare error: {}", e)))?;
                    }

                    // Step 1.5: If we created a mount namespace, make all mounts private
                    // to prevent mount propagation leaking to the parent namespace.
                    // linux.rootfsPropagation overrides the default MS_PRIVATE|MS_REC.
                    if namespaces.contains(Namespace::MOUNT) {
                        use std::ptr;

                        let prop_flags =
                            rootfs_propagation.unwrap_or(libc::MS_REC | libc::MS_PRIVATE);
                        let root = c"/";
                        let result = libc::mount(
                            ptr::null(),   // source: NULL (remount)
                            root.as_ptr(), // target: root
                            ptr::null(),   // fstype: NULL (remount)
                            prop_flags,
                            ptr::null(), // data: NULL
                        );

                        if result != 0 {
                            let err = io::Error::last_os_error();
                            // Any USER namespace (rootless or root-created) causes inherited mounts
                            // to be marked MNT_LOCKED by the kernel — their propagation cannot be
                            // changed, returning EINVAL. Safe to skip: the new mount namespace
                            // already provides isolation even without re-labelling propagation.
                            let has_user_ns = is_rootless || namespaces.contains(Namespace::USER);
                            if !has_user_ns || err.raw_os_error() != Some(libc::EINVAL) {
                                return Err(io::Error::other(format!("MS_PRIVATE: {}", err)));
                            }
                        }
                    }

                    // Step 1.6: Loopback mode — bring up lo after unshare(CLONE_NEWNET).
                    if bring_up_loopback {
                        crate::network::bring_up_loopback()
                            .map_err(|e| io::Error::other(format!("loopback up: {}", e)))?;
                    }

                    // Step 1.61: Set container hostname in the UTS namespace.
                    if let Some(ref name) = hostname {
                        let r = libc::sethostname(name.as_ptr() as *const libc::c_char, name.len());
                        if r != 0 {
                            return Err(io::Error::last_os_error());
                        }
                    }
                }

                // Step 1.65: PID namespace double-fork.
                //
                // Two cases handled here:
                //
                // A. Creating a new PID namespace (namespaces contains Namespace::PID):
                //    unshare(CLONE_NEWPID) puts our CHILDREN into a new PID namespace —
                //    we ourselves stay in the parent namespace.  This means:
                //      (a) we are NOT PID 1 in the new namespace
                //      (b) the first child we fork becomes PID 1
                //      (c) when that PID 1 exits, the kernel marks the namespace defunct
                //      (d) every subsequent fork() fails with ENOMEM
                //    Fix: fork once more so the child IS PID 1 in the new namespace.
                //
                // B. Joining an existing PID namespace (join_ns_fds contains PID):
                //    setns(CLONE_NEWPID) only updates pid_for_children for the calling
                //    process — it does NOT move the calling process into the namespace.
                //    exec() alone does not trigger the transition; only a subsequent
                //    fork() puts children into the new namespace.  So we must double-fork:
                //    setns → fork → grandchild is in the target namespace → grandchild execs.
                //
                // In both cases the intermediate (us, inner_pid > 0) waits for the child
                // and propagates the exit status.  PR_SET_PDEATHSIG on the child ensures
                // it dies if the intermediate is killed.
                if namespaces.contains(Namespace::PID) {
                    let inner_pid = libc::fork();
                    if inner_pid < 0 {
                        return Err(io::Error::last_os_error());
                    }
                    if inner_pid > 0 {
                        // Intermediate (P): wait for the real container (PID 1) and
                        // exit with its status.  Never returns from pre_exec.
                        //
                        // Die if our parent (the watcher) is killed unexpectedly.
                        // Without this, killing the watcher would orphan P → C would
                        // survive indefinitely.  The watcher sets PR_SET_CHILD_SUBREAPER
                        // so P is re-parented to the watcher (not init) if watcher dies,
                        // ensuring this pdeathsig fires in one hop.
                        libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
                        //
                        // Close all fds > 2 first.  std::process::Command uses an
                        // internal CLOEXEC pipe to report pre_exec/exec errors back
                        // to the parent.  Both we and the child hold the write end
                        // after fork.  If we keep ours open, the parent's read()
                        // blocks forever because the pipe never reaches EOF.
                        // The intermediate only needs waitpid — no fds required.
                        for fd in 3..1024 {
                            libc::close(fd);
                        }
                        let mut status: libc::c_int = 0;
                        loop {
                            let r = libc::waitpid(inner_pid, &mut status, 0);
                            if r == inner_pid {
                                break;
                            }
                            if r < 0 {
                                // std::io::Error::last_os_error() reads errno
                                // without allocating — portable across glibc and musl.
                                let e =
                                    std::io::Error::last_os_error().raw_os_error().unwrap_or(-1);
                                if e != libc::EINTR {
                                    libc::_exit(1);
                                }
                            }
                        }
                        if libc::WIFEXITED(status) {
                            libc::_exit(libc::WEXITSTATUS(status));
                        } else {
                            libc::_exit(128 + libc::WTERMSIG(status));
                        }
                    }
                    // Child: we are now PID 1 in the new PID namespace.
                    // Ensure we die if the intermediate (our parent) is killed.
                    libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
                } else if let Some(&(pid_join_fd, _)) =
                    join_ns_fds.iter().find(|(_, ns)| *ns == Namespace::PID)
                {
                    // Case B: joining an existing PID namespace via setns.
                    // setns changes pid_for_children; the grandchild (born after the fork
                    // below) is the first process created under the new pid_for_children
                    // and therefore enters the target PID namespace.
                    if libc::setns(pid_join_fd, 0) != 0 {
                        return Err(io::Error::last_os_error());
                    }
                    let inner_pid = libc::fork();
                    if inner_pid < 0 {
                        return Err(io::Error::last_os_error());
                    }
                    if inner_pid > 0 {
                        // Intermediate (P): die if watcher is killed.
                        libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
                        for fd in 3..1024 {
                            libc::close(fd);
                        }
                        let mut status: libc::c_int = 0;
                        loop {
                            let r = libc::waitpid(inner_pid, &mut status, 0);
                            if r == inner_pid {
                                break;
                            }
                            let e = std::io::Error::last_os_error().raw_os_error().unwrap_or(-1);
                            if e != libc::EINTR {
                                libc::_exit(1);
                            }
                        }
                        if libc::WIFEXITED(status) {
                            libc::_exit(libc::WEXITSTATUS(status));
                        } else {
                            libc::_exit(128 + libc::WTERMSIG(status));
                        }
                    }
                    // Grandchild: now in the target PID namespace.
                    // Die if our parent (the intermediate) dies unexpectedly.
                    libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
                }

                // Step 1.7: Bridge mode — join the pre-configured named netns via setns.
                // The named netns was fully set up before fork; no race is possible.
                if let Some(ref ns_path) = bridge_ns_path {
                    let fd = libc::open(ns_path.as_ptr(), libc::O_RDONLY | libc::O_CLOEXEC);
                    if fd < 0 {
                        return Err(io::Error::other(format!(
                            "open netns '{}': {}",
                            ns_path.to_string_lossy(),
                            io::Error::last_os_error()
                        )));
                    }
                    let ret = libc::setns(fd, libc::CLONE_NEWNET);
                    libc::close(fd);
                    if ret != 0 {
                        return Err(io::Error::other(format!(
                            "setns netns '{}': {}",
                            ns_path.to_string_lossy(),
                            io::Error::last_os_error()
                        )));
                    }
                }

                // Step 2: Set up UID/GID mapping if user namespace is active.
                // Skip for rootless — maps were written early in Step 1 so that
                // subsequent namespace unshares could use the resulting capabilities.
                if namespaces.contains(Namespace::USER) && !is_rootless {
                    use std::fs;
                    use std::io::Write;

                    // For unprivileged containers, must deny setgroups before writing gid_map
                    if !gid_maps.is_empty() {
                        let mut setgroups = fs::OpenOptions::new()
                            .write(true)
                            .open("/proc/self/setgroups")
                            .map_err(|e| io::Error::other(format!("open setgroups: {}", e)))?;
                        setgroups
                            .write_all(b"deny\n")
                            .map_err(|e| io::Error::other(format!("write setgroups: {}", e)))?;
                    }

                    // Write UID mappings — must be a single write() call (kernel requirement).
                    if !uid_maps.is_empty() {
                        use std::io::Write as _;
                        let mut content = String::new();
                        for map in &uid_maps {
                            content.push_str(&format!(
                                "{} {} {}\n",
                                map.inside, map.outside, map.count
                            ));
                        }
                        let mut uid_map_file = fs::OpenOptions::new()
                            .write(true)
                            .open("/proc/self/uid_map")
                            .map_err(|e| io::Error::other(format!("open uid_map: {}", e)))?;
                        uid_map_file
                            .write_all(content.as_bytes())
                            .map_err(|e| io::Error::other(format!("write uid_map: {}", e)))?;
                    }

                    // Write GID mappings — must be a single write() call (kernel requirement).
                    if !gid_maps.is_empty() {
                        use std::io::Write as _;
                        let mut content = String::new();
                        for map in &gid_maps {
                            content.push_str(&format!(
                                "{} {} {}\n",
                                map.inside, map.outside, map.count
                            ));
                        }
                        let mut gid_map_file = fs::OpenOptions::new()
                            .write(true)
                            .open("/proc/self/gid_map")
                            .map_err(|e| io::Error::other(format!("open gid_map: {}", e)))?;
                        gid_map_file
                            .write_all(content.as_bytes())
                            .map_err(|e| io::Error::other(format!("write gid_map: {}", e)))?;
                    }
                }

                // Step 3.5: Mount overlayfs (if configured).
                // The merged dir becomes the effective root for chroot and bind mounts.
                let overlay_merged: Option<&std::ffi::CString> =
                    if let Some((lower, upper, work, merged)) = &overlay_cstrings {
                        if use_fuse_overlay {
                            // fuse-overlayfs already mounted by parent — skip kernel mount.
                            Some(merged)
                        } else {
                            let mut opts_str = format!(
                                "lowerdir={},upperdir={},workdir={}",
                                lower.to_string_lossy(),
                                upper.to_string_lossy(),
                                work.to_string_lossy()
                            );
                            if is_rootless {
                                opts_str.push_str(",userxattr");
                            }
                            let opts = std::ffi::CString::new(opts_str).unwrap();
                            let ov_type = c"overlay";
                            let ret = libc::mount(
                                ov_type.as_ptr(),
                                merged.as_ptr(),
                                ov_type.as_ptr(),
                                0,
                                opts.as_ptr() as *const libc::c_void,
                            );
                            if ret != 0 {
                                return Err(io::Error::other(format!(
                                    "overlay mount (lowerdir={}): {}",
                                    lower.to_string_lossy(),
                                    io::Error::last_os_error()
                                )));
                            }
                            Some(merged)
                        }
                    } else {
                        None
                    };

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

                    let result =
                        libc::syscall(SYS_PIVOT_ROOT, new_root_c.as_ptr(), put_old_c.as_ptr());

                    if result != 0 {
                        return Err(io::Error::other(format!(
                            "pivot_root({}, {}): {}",
                            new_root.display(),
                            put_old.display(),
                            io::Error::last_os_error()
                        )));
                    }

                    // Change to new root
                    std::env::set_current_dir("/")?;

                    // Unmount old root
                    let put_old_rel = put_old
                        .strip_prefix(new_root)
                        .map_err(|_| io::Error::other("put_old must be inside new_root"))?;
                    let put_old_rel_c = CString::new(put_old_rel.as_os_str().as_bytes()).unwrap();

                    let umount_result = libc::umount2(put_old_rel_c.as_ptr(), libc::MNT_DETACH);
                    if umount_result != 0 {
                        // Don't fail if unmount doesn't work - it's not critical
                    }
                } else if let Some(ref dir) = chroot_dir {
                    // Fallback to chroot if pivot_root not specified
                    use std::os::unix::ffi::OsStrExt;

                    // When overlay is active, the merged dir is the effective root.
                    // Otherwise the chroot dir itself is the effective root.
                    let effective_root: &std::path::Path = overlay_merged
                        .as_ref()
                        .map(|m| std::path::Path::new(m.to_str().unwrap()))
                        .unwrap_or(dir.as_path());

                    // DNS: bind-mount the per-container resolv.conf over /etc/resolv.conf.
                    // Done here (before chroot) using the host-side effective_root path.
                    // Because Namespace::MOUNT is required, the bind mount is scoped to this
                    // container's private mount namespace — the host's rootfs is never touched.
                    if let Some(ref dns_src) = dns_temp_file_cstring {
                        let etc_host = effective_root.join("etc");
                        std::fs::create_dir_all(&etc_host)
                            .map_err(|e| io::Error::other(format!("dns mkdir /etc: {}", e)))?;
                        let resolv_host = etc_host.join("resolv.conf");
                        let tgt_c =
                            std::ffi::CString::new(resolv_host.as_os_str().as_bytes()).unwrap();
                        // Ensure target file exists — bind mount requires the target to exist.
                        let fd = libc::open(
                            tgt_c.as_ptr(),
                            libc::O_CREAT | libc::O_WRONLY | libc::O_CLOEXEC,
                            0o644u32,
                        );
                        if fd >= 0 {
                            libc::close(fd);
                        }
                        let r = libc::mount(
                            dns_src.as_ptr(),
                            tgt_c.as_ptr(),
                            ptr::null(),
                            libc::MS_BIND,
                            ptr::null(),
                        );
                        if r != 0 {
                            return Err(io::Error::other(format!(
                                "dns bind mount: {}",
                                io::Error::last_os_error()
                            )));
                        }
                    }

                    // Hosts: bind-mount the per-container hosts file over /etc/hosts.
                    // Same mechanism as DNS — scoped to this container's mount namespace.
                    if let Some(ref hosts_src) = hosts_temp_file_cstring {
                        let etc_host = effective_root.join("etc");
                        std::fs::create_dir_all(&etc_host)
                            .map_err(|e| io::Error::other(format!("hosts mkdir /etc: {}", e)))?;
                        let hosts_host = etc_host.join("hosts");
                        let tgt_c =
                            std::ffi::CString::new(hosts_host.as_os_str().as_bytes()).unwrap();
                        let fd = libc::open(
                            tgt_c.as_ptr(),
                            libc::O_CREAT | libc::O_WRONLY | libc::O_CLOEXEC,
                            0o644u32,
                        );
                        if fd >= 0 {
                            libc::close(fd);
                        }
                        let r = libc::mount(
                            hosts_src.as_ptr(),
                            tgt_c.as_ptr(),
                            ptr::null(),
                            libc::MS_BIND,
                            ptr::null(),
                        );
                        if r != 0 {
                            return Err(io::Error::other(format!(
                                "hosts bind mount: {}",
                                io::Error::last_os_error()
                            )));
                        }
                    }

                    // If readonly rootfs is requested, bind-mount the effective root to itself
                    // BEFORE chroot — this makes it a proper mount point so we can remount it
                    // readonly later. When overlay is active, the overlay IS already a proper
                    // mount point — skip the self-bind in that case.
                    if readonly_rootfs && overlay_merged.is_none() {
                        let dir_c = CString::new(dir.as_os_str().as_bytes()).unwrap();
                        let result = libc::mount(
                            dir_c.as_ptr(),               // source: chroot dir
                            dir_c.as_ptr(),               // target: same dir
                            ptr::null(),                  // fstype: NULL
                            libc::MS_BIND | libc::MS_REC, // recursive bind mount
                            ptr::null(),                  // data: NULL
                        );
                        if result != 0 {
                            return Err(io::Error::last_os_error());
                        }
                    }

                    // Perform bind mounts BEFORE chroot — source paths are host paths,
                    // unreachable once we chroot.
                    for bm in &bind_mounts {
                        use std::os::unix::ffi::OsStrExt as _;
                        // Target inside the effective root on the host side
                        let rel = bm.target.strip_prefix("/").unwrap_or(&bm.target);
                        let host_target = effective_root.join(rel);
                        // Linux requires the mount target to exist and be the same type
                        // (file or directory) as the source.
                        if bm.source.is_dir() {
                            std::fs::create_dir_all(&host_target).map_err(|e| {
                                io::Error::other(format!("bind mount mkdir: {}", e))
                            })?;
                        } else {
                            if let Some(parent) = host_target.parent() {
                                std::fs::create_dir_all(parent).map_err(|e| {
                                    io::Error::other(format!("bind mount mkdir: {}", e))
                                })?;
                            }
                            if !host_target.exists() {
                                std::fs::File::create(&host_target).map_err(|e| {
                                    io::Error::other(format!("bind mount mkfile: {}", e))
                                })?;
                            }
                        }
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
                                bm.source.display(),
                                host_target.display(),
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
                                    host_target.display(),
                                    io::Error::last_os_error()
                                )));
                            }
                        }
                    }

                    // Minimal /dev setup BEFORE chroot — host /dev paths still accessible.
                    if mount_dev {
                        use std::os::unix::ffi::OsStrExt as _;
                        let dev_host = effective_root.join("dev");
                        std::fs::create_dir_all(&dev_host)
                            .map_err(|e| io::Error::other(format!("mkdir /dev: {}", e)))?;
                        let dev_host_c = CString::new(dev_host.as_os_str().as_bytes()).unwrap();
                        let tmpfs_type = CString::new("tmpfs").unwrap();
                        let dev_opts = CString::new("mode=755,size=65536k").unwrap();
                        let r = libc::mount(
                            tmpfs_type.as_ptr(),
                            dev_host_c.as_ptr(),
                            tmpfs_type.as_ptr(),
                            libc::MS_NOSUID | libc::MS_STRICTATIME,
                            dev_opts.as_ptr() as *const libc::c_void,
                        );
                        if r != 0 {
                            let e = io::Error::last_os_error();
                            if !is_rootless {
                                return Err(io::Error::other(format!("mount tmpfs /dev: {}", e)));
                            }
                        } else {
                            // Create subdirectories.
                            let _ = std::fs::create_dir_all(dev_host.join("pts"));
                            let _ = std::fs::create_dir_all(dev_host.join("shm"));
                            let _ = std::fs::create_dir_all(dev_host.join("mqueue"));

                            // Mount devpts at /dev/pts (tolerate failure).
                            let devpts_path =
                                CString::new(dev_host.join("pts").as_os_str().as_bytes()).unwrap();
                            let devpts_type = CString::new("devpts").unwrap();
                            let devpts_opts =
                                CString::new("newinstance,ptmxmode=0666,mode=0620,gid=5").unwrap();
                            let _ = libc::mount(
                                devpts_type.as_ptr(),
                                devpts_path.as_ptr(),
                                devpts_type.as_ptr(),
                                libc::MS_NOSUID | libc::MS_NOEXEC,
                                devpts_opts.as_ptr() as *const libc::c_void,
                            );

                            // Mount tmpfs at /dev/shm.
                            let shm_path =
                                CString::new(dev_host.join("shm").as_os_str().as_bytes()).unwrap();
                            let shm_opts = CString::new("mode=1777,size=65536k").unwrap();
                            let _ = libc::mount(
                                tmpfs_type.as_ptr(),
                                shm_path.as_ptr(),
                                tmpfs_type.as_ptr(),
                                libc::MS_NOSUID | libc::MS_NODEV | libc::MS_NOEXEC,
                                shm_opts.as_ptr() as *const libc::c_void,
                            );

                            // Mount mqueue at /dev/mqueue (tolerate failure).
                            let mqueue_path =
                                CString::new(dev_host.join("mqueue").as_os_str().as_bytes())
                                    .unwrap();
                            let mqueue_type = CString::new("mqueue").unwrap();
                            let _ = libc::mount(
                                mqueue_type.as_ptr(),
                                mqueue_path.as_ptr(),
                                mqueue_type.as_ptr(),
                                libc::MS_NOSUID | libc::MS_NODEV | libc::MS_NOEXEC,
                                ptr::null(),
                            );

                            // Bind-mount safe devices from host /dev/<name>.
                            for dev_name in &["null", "zero", "full", "random", "urandom", "tty"] {
                                let host_dev = CString::new(format!("/dev/{}", dev_name)).unwrap();
                                let target = dev_host.join(dev_name);
                                let target_c = CString::new(target.as_os_str().as_bytes()).unwrap();
                                // Create empty target file for bind mount.
                                let tfd = libc::open(
                                    target_c.as_ptr(),
                                    libc::O_CREAT | libc::O_WRONLY | libc::O_CLOEXEC,
                                    0o666u32,
                                );
                                if tfd >= 0 {
                                    libc::close(tfd);
                                }
                                let r = libc::mount(
                                    host_dev.as_ptr(),
                                    target_c.as_ptr(),
                                    ptr::null(),
                                    libc::MS_BIND,
                                    ptr::null(),
                                );
                                if r != 0 {
                                    log::debug!(
                                        "bind-mount /dev/{} failed: {}",
                                        dev_name,
                                        io::Error::last_os_error()
                                    );
                                }
                            }

                            // Symlinks (using host-side paths).
                            let _ =
                                std::os::unix::fs::symlink("/proc/self/fd", dev_host.join("fd"));
                            let _ = std::os::unix::fs::symlink(
                                "/proc/self/fd/0",
                                dev_host.join("stdin"),
                            );
                            let _ = std::os::unix::fs::symlink(
                                "/proc/self/fd/1",
                                dev_host.join("stdout"),
                            );
                            let _ = std::os::unix::fs::symlink(
                                "/proc/self/fd/2",
                                dev_host.join("stderr"),
                            );
                            let _ = std::os::unix::fs::symlink("pts/ptmx", dev_host.join("ptmx"));
                        }
                    }

                    chroot(effective_root)
                        .map_err(|e| io::Error::other(format!("chroot error: {}", e)))?;

                    // Change working directory after chroot (defaults to /).
                    let cwd = container_cwd
                        .as_deref()
                        .unwrap_or(std::path::Path::new("/"));
                    std::env::set_current_dir(cwd)
                        .map_err(|e| io::Error::other(format!("set_current_dir: {}", e)))?;
                }

                // Step 4.5: Perform automatic mounts if requested.
                // IMPORTANT: Use absolute paths for mount targets — cwd may not
                // be "/" if the caller used with_cwd().
                if mount_proc {
                    // Ensure /proc exists — some minimal images omit it.
                    let _ = std::fs::create_dir_all("/proc");
                    let proc_src = CString::new("proc").unwrap();
                    let proc_tgt = CString::new("/proc").unwrap();
                    let result = libc::mount(
                        proc_src.as_ptr(), // source
                        proc_tgt.as_ptr(), // target
                        proc_src.as_ptr(), // fstype (proc)
                        0,                 // flags
                        ptr::null(),       // data
                    );
                    // In rootless mode OR with a USER namespace, proc mount fails (EPERM or
                    // EINVAL) because the PID namespace is not owned by our user namespace.
                    // In rootless mode, proc mount fails because the PID namespace is not
                    // owned by our user namespace. With USER+PID (auto-added by spawn()),
                    // proc succeeds. Only skip errors in rootless mode.
                    if result != 0 && !is_rootless {
                        return Err(io::Error::other(format!(
                            "mount proc: {}",
                            io::Error::last_os_error()
                        )));
                    }
                }

                if mount_sys {
                    // Ensure /sys exists — some minimal images omit it.
                    let _ = std::fs::create_dir_all("/sys");
                    // Bind mount /sys (from host) to /sys (in container)
                    let sys = CString::new("/sys").unwrap();
                    let sysfs = CString::new("sysfs").unwrap();
                    let result = libc::mount(
                        sys.as_ptr(),   // source
                        sys.as_ptr(),   // target
                        sysfs.as_ptr(), // fstype
                        libc::MS_BIND,  // flags
                        ptr::null(),    // data
                    );
                    // Rootless: /sys bind may fail on locked mounts; inherited /sys is still usable.
                    if result != 0 && !is_rootless {
                        return Err(io::Error::other(format!(
                            "mount sys: {}",
                            io::Error::last_os_error()
                        )));
                    }
                }

                // Mount tmpfs filesystems AFTER chroot — tmpfs has no host-side source
                for tm in &tmpfs_mounts {
                    std::fs::create_dir_all(&tm.target)
                        .map_err(|e| io::Error::other(format!("tmpfs mkdir: {}", e)))?;
                    let tgt_c = CString::new(tm.target.as_os_str().as_encoded_bytes()).unwrap();
                    let tmpfs_c = CString::new("tmpfs").unwrap();
                    let opts_c = CString::new(tm.options.as_bytes()).unwrap();
                    let opts_ptr = if tm.options.is_empty() {
                        ptr::null()
                    } else {
                        opts_c.as_ptr() as *const libc::c_void
                    };
                    let result = libc::mount(
                        tmpfs_c.as_ptr(),                 // source: "tmpfs"
                        tgt_c.as_ptr(),                   // target
                        tmpfs_c.as_ptr(),                 // fstype: "tmpfs"
                        libc::MS_NOSUID | libc::MS_NODEV, // flags
                        opts_ptr,                         // data: mount options
                    );
                    if result != 0 {
                        return Err(io::Error::other(format!(
                            "tmpfs mount {}: {}",
                            tm.target.display(),
                            io::Error::last_os_error()
                        )));
                    }
                }

                // Mount kernel filesystems (proc, sysfs, devpts, mqueue, cgroup2, …)
                // specified by OCI config.json `mounts` entries.
                for km in &kernel_mounts {
                    std::fs::create_dir_all(&km.target).map_err(|e| {
                        io::Error::other(format!(
                            "kernel mount mkdir {}: {}",
                            km.target.display(),
                            e
                        ))
                    })?;
                    let tgt_c = CString::new(km.target.as_os_str().as_encoded_bytes()).unwrap();
                    let src_c = CString::new(km.source.as_bytes()).unwrap();
                    let fst_c = CString::new(km.fs_type.as_bytes()).unwrap();
                    let dat_c = CString::new(km.data.as_bytes()).unwrap();
                    let dat_ptr: *const libc::c_void = if km.data.is_empty() {
                        ptr::null()
                    } else {
                        dat_c.as_ptr() as *const libc::c_void
                    };
                    let result = libc::mount(
                        src_c.as_ptr(),
                        tgt_c.as_ptr(),
                        fst_c.as_ptr(),
                        km.flags,
                        dat_ptr,
                    );
                    if result != 0 {
                        return Err(io::Error::other(format!(
                            "mount {} ({}) at {}: {}",
                            km.fs_type,
                            km.source,
                            km.target.display(),
                            io::Error::last_os_error()
                        )));
                    }
                }

                // Step 4.7: Apply sysctl settings (write to /proc/sys/)
                for (key, value) in &sysctl {
                    // Convert "net.ipv4.ip_forward" -> "/proc/sys/net/ipv4/ip_forward"
                    let proc_path = format!("/proc/sys/{}", key.replace('.', "/"));
                    let path_c = match std::ffi::CString::new(proc_path.as_bytes()) {
                        Ok(p) => p,
                        Err(_) => continue,
                    };
                    let flags = libc::O_WRONLY | libc::O_TRUNC;
                    let fd = libc::open(path_c.as_ptr(), flags, 0);
                    if fd >= 0 {
                        let bytes = value.as_bytes();
                        libc::write(fd, bytes.as_ptr() as *const libc::c_void, bytes.len());
                        libc::close(fd);
                    }
                    // Ignore errors — sysctl may not exist in this namespace
                }

                // Step 4.72: Create device nodes
                if !devices.is_empty() {
                    for dev in &devices {
                        let path_c =
                            match std::ffi::CString::new(dev.path.as_os_str().as_encoded_bytes()) {
                                Ok(p) => p,
                                Err(_) => continue,
                            };
                        let type_bits: libc::mode_t = match dev.kind {
                            'b' => libc::S_IFBLK,
                            'p' => libc::S_IFIFO,
                            _ => libc::S_IFCHR, // 'c' and default
                        };
                        let devnum =
                            libc::makedev(dev.major as libc::c_uint, dev.minor as libc::c_uint);
                        let r = libc::mknod(
                            path_c.as_ptr(),
                            type_bits | (dev.mode as libc::mode_t),
                            devnum,
                        );
                        if r == 0 && (dev.uid != 0 || dev.gid != 0) {
                            libc::chown(path_c.as_ptr(), dev.uid, dev.gid);
                        }
                        // Ignore mknod errors — device may already exist
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
                            dev_null.as_ptr(), // source: /dev/null
                            path_c.as_ptr(),   // target: path to mask
                            ptr::null(),       // fstype: NULL
                            libc::MS_BIND,     // bind mount
                            ptr::null(),       // data: NULL
                        );

                        // Ignore errors - path might not exist, which is fine
                        if result != 0 {
                            // Don't fail, just skip this path
                        }
                    }
                }

                // Step 4.82: Make specific paths read-only (linux.readonlyPaths)
                if !readonly_paths.is_empty() {
                    for path in &readonly_paths {
                        let path_c = match CString::new(path.as_os_str().as_encoded_bytes()) {
                            Ok(p) => p,
                            Err(_) => continue,
                        };
                        // First bind-mount the path to itself to create a separate mount point,
                        // then remount it read-only.
                        let r = libc::mount(
                            path_c.as_ptr(),
                            path_c.as_ptr(),
                            ptr::null(),
                            libc::MS_BIND,
                            ptr::null(),
                        );
                        if r != 0 {
                            continue;
                        } // path may not exist; skip
                        libc::mount(
                            ptr::null(),
                            path_c.as_ptr(),
                            ptr::null(),
                            libc::MS_REMOUNT | libc::MS_BIND | libc::MS_RDONLY,
                            ptr::null(),
                        );
                        // Ignore remount errors (e.g. already read-only)
                    }
                }

                // Step 4.85: Make rootfs read-only if requested
                // MUST come after all mounts (/proc, /sys, /dev, masked paths)
                // Note: We already did bind mount before chroot, so just remount readonly now
                if readonly_rootfs {
                    let root = CString::new("/").unwrap();
                    let result = libc::mount(
                        ptr::null(),                                        // source: NULL (remount)
                        root.as_ptr(),                                      // target: /
                        ptr::null(), // fstype: NULL (remount)
                        libc::MS_REMOUNT | libc::MS_RDONLY | libc::MS_BIND, // remount readonly
                        ptr::null(), // data: NULL
                    );
                    if result != 0 {
                        return Err(io::Error::last_os_error());
                    }
                }

                // Step 4.86: Drop capabilities.
                //
                // MUST come after all mount operations (masked paths, readonly
                // paths, readonly rootfs) because those require CAP_SYS_ADMIN.
                // Two-step drop (mirrors Docker / runc):
                //
                // 1. PR_CAPBSET_DROP — remove unwanted caps from the bounding
                //    set so exec() cannot re-grant them.
                // 2. capset() — explicitly set the effective, permitted, and
                //    inheritable kernel sets to the desired mask.  Without this
                //    step, CapEff/CapPrm remain at their current values (full
                //    root caps) regardless of what the bounding set says.
                if let Some(keep_caps) = capabilities {
                    const PR_CAPBSET_DROP: i32 = 24;
                    for cap in 0..41u64 {
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

                    let bits = keep_caps.bits();
                    let lo = bits as u32;
                    let hi = (bits >> 32) as u32;

                    #[repr(C)]
                    struct CapHeader {
                        version: u32,
                        pid: i32,
                    }
                    #[repr(C)]
                    struct CapData {
                        effective: u32,
                        permitted: u32,
                        inheritable: u32,
                    }

                    let header = CapHeader {
                        version: 0x2008_0522,
                        pid: 0,
                    };
                    let data = [
                        CapData {
                            effective: lo,
                            permitted: lo,
                            inheritable: lo,
                        },
                        CapData {
                            effective: hi,
                            permitted: hi,
                            inheritable: hi,
                        },
                    ];

                    let ret =
                        libc::syscall(libc::SYS_capset, &header as *const CapHeader, data.as_ptr());
                    if ret != 0 {
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
                // MUST run before setuid — exec's callback does setns(CLONE_NEWNS)
                // which requires CAP_SYS_ADMIN.
                if let Some(ref callback) = user_pre_exec {
                    callback()?;
                }

                // Step 6: Join existing namespaces AFTER chroot and filesystem setup
                // This ensures paths are resolved correctly before namespace transitions.
                // PID namespace entries are skipped here — they are handled at step 1.65
                // via double-fork (see below).
                for (fd, ns) in &join_ns_fds {
                    if *ns == Namespace::PID {
                        // Handled at step 1.65 via double-fork.
                        continue;
                    }
                    let result = libc::setns(*fd, 0);
                    if result != 0 {
                        return Err(io::Error::last_os_error());
                    }
                }

                // Step 6.1: Set UID/GID if specified.
                // MUST come after all privileged operations (overlay mount, chroot,
                // /proc, /dev, bind mounts, capabilities, user callback, ns joins)
                // because those need root.
                if let Some(gid_val) = gid {
                    let result = libc::setgid(gid_val);
                    if result != 0 {
                        return Err(io::Error::other(format!(
                            "setgid: {}",
                            io::Error::last_os_error()
                        )));
                    }
                }
                if let Some(uid_val) = uid {
                    let result = libc::setuid(uid_val);
                    if result != 0 {
                        return Err(io::Error::other(format!(
                            "setuid: {}",
                            io::Error::last_os_error()
                        )));
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

                // Step 6.6: PTY slave setup for OCI terminal mode.
                // When the caller allocated a PTY (process.terminal = true), wire the
                // slave fd as stdin/stdout/stderr and make it the controlling terminal.
                if let Some(slave_fd) = pty_slave {
                    libc::setsid();
                    libc::dup2(slave_fd, 0);
                    libc::dup2(slave_fd, 1);
                    libc::dup2(slave_fd, 2);
                    libc::ioctl(slave_fd, libc::TIOCSCTTY as libc::c_ulong, 0i32);
                    if slave_fd > 2 {
                        libc::close(slave_fd);
                    }
                }

                // Step 7: Apply seccomp filter if configured.
                // CRITICAL: This MUST be before the OCI sync! Once seccomp is applied, many syscalls
                // are blocked. All setup must be complete before signalling "created".
                if let Some(ref filter) = seccomp_filter {
                    crate::seccomp::apply_filter(filter)?;
                }

                // Step 8: OCI create/start synchronization.
                // Signals the parent that setup is complete (writes PID to ready_write_fd),
                // then blocks on accept(listen_fd) until "remora start" connects.
                // After receiving the start byte, pre_exec returns → exec happens.
                if let Some((ready_w, listen_fd)) = oci_sync {
                    // Write our PID (4 bytes, native-endian) to signal "created".
                    let pid: i32 = libc::getpid();
                    let pid_bytes = pid.to_ne_bytes();
                    libc::write(ready_w, pid_bytes.as_ptr() as *const libc::c_void, 4);
                    libc::close(ready_w);

                    // Block until "remora start" connects and sends one byte.
                    let conn = libc::accept4(listen_fd, ptr::null_mut(), ptr::null_mut(), 0);
                    if conn >= 0 {
                        let mut buf = [0u8; 1];
                        libc::read(conn, buf.as_mut_ptr() as *mut libc::c_void, 1);
                        libc::close(conn);
                    }
                    libc::close(listen_fd);
                }

                Ok(())
            });
        }

        // If using ID helpers, spawn the helper thread now (before fork).
        // It reads the child PID from the pipe, runs newuidmap/newgidmap, signals done.
        if use_id_helpers {
            let uid_maps_h = self.uid_maps.clone();
            let gid_maps_h = self.gid_maps.clone();
            let ready_r = idmap_ready_r;
            let done_w = idmap_done_w;

            std::thread::spawn(move || {
                let mut pid_bytes = [0u8; 4];
                let n =
                    unsafe { libc::read(ready_r, pid_bytes.as_mut_ptr() as *mut libc::c_void, 4) };
                unsafe { libc::close(ready_r) };
                if n != 4 {
                    unsafe { libc::close(done_w) };
                    return;
                }
                let child_pid = u32::from_ne_bytes(pid_bytes);

                if let Err(e) = crate::idmap::apply_uid_map(child_pid, &uid_maps_h) {
                    log::warn!("newuidmap failed: {}", e);
                }
                if let Err(e) = crate::idmap::apply_gid_map(child_pid, &gid_maps_h) {
                    log::warn!("newgidmap failed: {}", e);
                }

                unsafe { libc::write(done_w, [0u8].as_ptr() as *const libc::c_void, 1) };
                unsafe { libc::close(done_w) };
            });
        }

        // Spawn the process
        let child_inner = match self.inner.spawn() {
            Ok(c) => c,
            Err(e) => {
                if use_id_helpers {
                    // Close child-side pipe ends to unblock the helper thread.
                    unsafe { libc::close(idmap_ready_w) };
                    unsafe { libc::close(idmap_done_r) };
                }
                return Err(Error::Spawn(e));
            }
        };

        // Close child-side pipe ends in the parent (child inherited them via fork).
        if use_id_helpers {
            unsafe { libc::close(idmap_ready_w) };
            unsafe { libc::close(idmap_done_r) };
        }

        // Keep join_ns_files alive until here so file descriptors remain valid
        drop(join_ns_files);

        // Create cgroup and add child PID (parent-side, after fork)
        let cgroup = if let Some(ref cfg) = self.cgroup_config {
            if is_rootless {
                match crate::cgroup_rootless::setup_rootless_cgroup(cfg, child_inner.id()) {
                    Ok(cg) => Some(CgroupHandle::Rootless(cg)),
                    Err(e) => {
                        log::warn!("rootless cgroup setup failed, skipping: {}", e);
                        None
                    }
                }
            } else {
                match crate::cgroup::setup_cgroup(cfg, child_inner.id()) {
                    Ok(cg) => Some(CgroupHandle::Root(cg)),
                    Err(e) => return Err(Error::Io(e)),
                }
            }
        } else {
            None
        };

        // Bridge networking was fully set up before fork; nothing to do here.
        let network = bridge_network;

        // Pasta: spawn the relay after the child has exec'd (/proc/{pid}/ns/net is live).
        let pasta: Option<crate::network::PastaSetup> = if is_pasta {
            Some(
                crate::network::setup_pasta_network(child_inner.id(), &self.port_forwards)
                    .map_err(Error::Io)?,
            )
        } else {
            None
        };

        Ok(Child {
            inner: child_inner,
            cgroup,
            network,
            secondary_networks,
            pasta,
            overlay_merged_dir,
            dns_temp_dir,
            hosts_temp_dir,
            fuse_overlay_child,
            fuse_overlay_merged,
        })
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

        let seccomp_filter: Option<seccompiler::BpfProgram> =
            if let Some(prog) = self.seccomp_program.take() {
                Some(prog)
            } else if let Some(profile) = &self.seccomp_profile {
                match profile {
                    SeccompProfile::Docker => {
                        Some(crate::seccomp::docker_default_filter().map_err(Error::Seccomp)?)
                    }
                    SeccompProfile::Minimal => {
                        Some(crate::seccomp::minimal_filter().map_err(Error::Seccomp)?)
                    }
                    SeccompProfile::None => None,
                }
            } else {
                None
            };

        let join_ns_files: Vec<(File, Namespace)> = self
            .join_namespaces
            .iter()
            .map(|(path, ns)| File::open(path).map(|f| (f, *ns)).map_err(Error::Io))
            .collect::<Result<Vec<_>, _>>()?;

        let join_ns_fds: Vec<(i32, Namespace)> = join_ns_files
            .iter()
            .map(|(f, ns)| (f.as_raw_fd(), *ns))
            .collect();

        // Detect rootless mode and auto-configure (same logic as spawn()).
        let is_rootless = unsafe { libc::getuid() } != 0;
        if is_rootless {
            self.namespaces |= Namespace::USER;
            let host_uid = unsafe { libc::getuid() };
            let host_gid = unsafe { libc::getgid() };

            // Try multi-range subordinate UID/GID mapping via newuidmap/newgidmap.
            if self.uid_maps.is_empty() {
                if crate::idmap::has_newuidmap() && crate::idmap::has_newgidmap() {
                    if let Ok(username) = crate::idmap::current_username() {
                        let uid_ranges = crate::idmap::parse_subid_file(
                            std::path::Path::new("/etc/subuid"),
                            &username,
                            host_uid,
                        )
                        .unwrap_or_default();
                        let gid_ranges = crate::idmap::parse_subid_file(
                            std::path::Path::new("/etc/subgid"),
                            &username,
                            host_gid,
                        )
                        .unwrap_or_default();

                        if !uid_ranges.is_empty() && !gid_ranges.is_empty() {
                            self.uid_maps.push(UidMap {
                                inside: 0,
                                outside: host_uid,
                                count: 1,
                            });
                            self.uid_maps.push(UidMap {
                                inside: 1,
                                outside: uid_ranges[0].start,
                                count: uid_ranges[0].count,
                            });
                            self.gid_maps.push(GidMap {
                                inside: 0,
                                outside: host_gid,
                                count: 1,
                            });
                            self.gid_maps.push(GidMap {
                                inside: 1,
                                outside: gid_ranges[0].start,
                                count: gid_ranges[0].count,
                            });
                            self.use_id_helpers = true;
                            log::info!(
                                "rootless multi-UID: {} subordinate UIDs, {} subordinate GIDs",
                                uid_ranges[0].count,
                                gid_ranges[0].count
                            );
                        }
                    }
                }
                // Fallback: single-UID map (current behavior).
                if self.uid_maps.is_empty() {
                    self.uid_maps.push(UidMap {
                        inside: 0,
                        outside: host_uid,
                        count: 1,
                    });
                }
                if self.gid_maps.is_empty() {
                    self.gid_maps.push(GidMap {
                        inside: 0,
                        outside: host_gid,
                        count: 1,
                    });
                }
            }
            // Bridge networking requires root-level capabilities on the host network.
            if self
                .network_config
                .as_ref()
                .is_some_and(|c| c.mode.is_bridge())
            {
                return Err(Error::Io(io::Error::other(
                    "NetworkMode::Bridge requires root; use NetworkMode::Pasta for rootless internet access",
                )));
            }
        }

        // Pasta mode: validate pasta is available and auto-add NET namespace.
        let is_pasta = self
            .network_config
            .as_ref()
            .is_some_and(|c| c.mode == crate::network::NetworkMode::Pasta);
        if is_pasta {
            if !crate::network::is_pasta_available() {
                return Err(Error::Io(io::Error::other(
                    "NetworkMode::Pasta requires pasta — install from https://passt.top",
                )));
            }
            self.namespaces |= Namespace::NET;
        }

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
        let readonly_paths = self.readonly_paths.clone();
        let sysctl = self.sysctl.clone();
        let devices = self.devices.clone();
        let bind_mounts = self.bind_mounts.clone();
        let tmpfs_mounts = self.tmpfs_mounts.clone();
        let kernel_mounts = self.kernel_mounts.clone();
        let rootfs_propagation = self.rootfs_propagation;
        let hostname = self.hostname.clone();
        let use_id_helpers = self.use_id_helpers;
        let bring_up_loopback = self.network_config.as_ref().is_some_and(|c| {
            c.mode == crate::network::NetworkMode::Loopback
                || c.mode == crate::network::NetworkMode::Pasta
        });
        let bridge_network_name: Option<String> = self
            .network_config
            .as_ref()
            .and_then(|c| c.mode.bridge_network_name().map(|s| s.to_owned()));
        let _is_bridge = bridge_network_name.is_some();

        // Bridge mode: create and fully configure the named netns BEFORE fork.
        let bridge_network: Option<crate::network::NetworkSetup> =
            if let Some(ref net_name) = bridge_network_name {
                let ns_name = crate::network::generate_ns_name();
                Some(
                    crate::network::setup_bridge_network(
                        &ns_name,
                        net_name,
                        self.nat,
                        self.port_forwards.clone(),
                    )
                    .map_err(Error::Io)?,
                )
            } else {
                None
            };
        let bridge_ns_path: Option<std::ffi::CString> = bridge_network
            .as_ref()
            .map(|n| std::ffi::CString::new(format!("/run/netns/{}", n.ns_name)).unwrap());

        // Attach additional bridge networks to the same netns (secondary interfaces).
        let mut secondary_networks: Vec<crate::network::NetworkSetup> = Vec::new();
        if let Some(ref primary) = bridge_network {
            for (i, net_name) in self.additional_networks.iter().enumerate() {
                let iface = format!("eth{}", i + 1);
                secondary_networks.push(
                    crate::network::attach_network_to_netns(&primary.ns_name, net_name, &iface)
                        .map_err(Error::Io)?,
                );
            }
        }

        // Validate overlay prerequisites before fork.
        if self.overlay.is_some() && !self.namespaces.contains(Namespace::MOUNT) {
            return Err(Error::Io(io::Error::other(
                "with_overlay requires Namespace::MOUNT",
            )));
        }
        if self.overlay.is_some() && self.chroot_dir.is_none() {
            return Err(Error::Io(io::Error::other(
                "with_overlay requires with_chroot",
            )));
        }

        // Create the overlay merged dir before fork.
        // When upper/work are empty (image-layer mode), auto-create them as siblings of merged.
        let overlay_merged_dir: Option<PathBuf> = if let Some(ref mut ov) = self.overlay {
            let pid = unsafe { libc::getpid() };
            let n = OVERLAY_COUNTER.fetch_add(1, Ordering::Relaxed);
            let base = crate::paths::overlay_base(pid, n);
            let merged = base.join("merged");
            std::fs::create_dir_all(&merged).map_err(Error::Io)?;
            if ov.upper_dir.as_os_str().is_empty() {
                let upper = base.join("upper");
                let work = base.join("work");
                std::fs::create_dir_all(&upper).map_err(Error::Io)?;
                std::fs::create_dir_all(&work).map_err(Error::Io)?;
                ov.upper_dir = upper;
                ov.work_dir = work;
            }
            Some(merged)
        } else {
            None
        };

        // Pre-allocate CStrings for the overlay mount (lower, upper, work, merged).
        let overlay_cstrings: Option<(
            std::ffi::CString,
            std::ffi::CString,
            std::ffi::CString,
            std::ffi::CString,
        )> = match (&self.overlay, &overlay_merged_dir) {
            (Some(ov), Some(merged)) => {
                use std::os::unix::ffi::OsStrExt as _;
                let lower_str = if !ov.lower_dirs.is_empty() {
                    ov.lower_dirs
                        .iter()
                        .map(|p| p.to_string_lossy().into_owned())
                        .collect::<Vec<_>>()
                        .join(":")
                } else {
                    self.chroot_dir
                        .as_ref()
                        .unwrap()
                        .to_string_lossy()
                        .into_owned()
                };
                Some((
                    std::ffi::CString::new(lower_str.as_bytes()).unwrap(),
                    std::ffi::CString::new(ov.upper_dir.as_os_str().as_bytes()).unwrap(),
                    std::ffi::CString::new(ov.work_dir.as_os_str().as_bytes()).unwrap(),
                    std::ffi::CString::new(merged.as_os_str().as_bytes()).unwrap(),
                ))
            }
            _ => None,
        };

        // Rootless overlay: decide between native overlay+userxattr vs fuse-overlayfs.
        // Temporarily set the PTY slave to CLOEXEC so the overlay probe fork and
        // any fuse-overlayfs daemon don't inherit it (which would prevent POLLHUP
        // on the master when the container exits).
        let mut fuse_overlay_child: Option<std::process::Child> = None;
        let mut fuse_overlay_merged: Option<PathBuf> = None;
        let use_fuse_overlay: bool;
        if is_rootless && self.overlay.is_some() {
            unsafe {
                let flags = libc::fcntl(slave_raw_fd, libc::F_GETFD);
                libc::fcntl(slave_raw_fd, libc::F_SETFD, flags | libc::FD_CLOEXEC);
            }
            if native_rootless_overlay_supported() {
                log::debug!("rootless overlay: using native overlay+userxattr");
                use_fuse_overlay = false;
            } else if is_fuse_overlayfs_available() {
                log::info!("rootless overlay: falling back to fuse-overlayfs");
                if let (Some(ov), Some(merged)) = (&self.overlay, &overlay_merged_dir) {
                    let lower_str = if !ov.lower_dirs.is_empty() {
                        ov.lower_dirs
                            .iter()
                            .map(|p| p.to_string_lossy().into_owned())
                            .collect::<Vec<_>>()
                            .join(":")
                    } else {
                        self.chroot_dir
                            .as_ref()
                            .unwrap()
                            .to_string_lossy()
                            .into_owned()
                    };
                    let child =
                        spawn_fuse_overlayfs(&lower_str, &ov.upper_dir, &ov.work_dir, merged)
                            .map_err(Error::Io)?;
                    fuse_overlay_merged = Some(merged.clone());
                    fuse_overlay_child = Some(child);
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
                use_fuse_overlay = true;
            } else {
                return Err(Error::Io(io::Error::other(
                    "rootless overlay requires kernel 5.11+ or fuse-overlayfs; \
                     install fuse-overlayfs or run as root",
                )));
            }
            // Restore slave to non-CLOEXEC so the container child inherits it.
            unsafe {
                let flags = libc::fcntl(slave_raw_fd, libc::F_GETFD);
                libc::fcntl(slave_raw_fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC);
            }
        } else {
            use_fuse_overlay = false;
        }

        // Collect OCI sync fds.
        let oci_sync = self.oci_sync;
        let pty_slave = self.pty_slave;
        let container_cwd = self.container_cwd.clone();

        // DNS: auto-inject bridge gateway IP(s) as primary nameservers for the
        // embedded DNS daemon, then append user-specified --dns servers as fallback.
        let mut auto_dns: Vec<String> = Vec::new();
        if let Some(ref net) = bridge_network {
            if let Ok(net_def) = crate::network::load_network_def(&net.network_name) {
                auto_dns.push(net_def.gateway.to_string());
            }
        }
        for sec in &secondary_networks {
            if let Ok(net_def) = crate::network::load_network_def(&sec.network_name) {
                let gw = net_def.gateway.to_string();
                if !auto_dns.contains(&gw) {
                    auto_dns.push(gw);
                }
            }
        }
        auto_dns.extend(self.dns_servers.iter().cloned());

        // DNS: write nameservers to a per-container temp file; bind-mount into container.
        if !auto_dns.is_empty() {
            if !self.namespaces.contains(Namespace::MOUNT) {
                return Err(Error::Io(io::Error::other(
                    "with_dns requires Namespace::MOUNT",
                )));
            }
            if self.chroot_dir.is_none() {
                return Err(Error::Io(io::Error::other("with_dns requires with_chroot")));
            }
        }
        let dns_temp_dir: Option<PathBuf> = if !auto_dns.is_empty() {
            let pid = unsafe { libc::getpid() };
            let n = DNS_COUNTER.fetch_add(1, Ordering::Relaxed);
            let dir = crate::paths::dns_dir(pid, n);
            std::fs::create_dir_all(&dir).map_err(Error::Io)?;
            let mut content = String::new();
            for s in &auto_dns {
                content.push_str("nameserver ");
                content.push_str(s);
                content.push('\n');
            }
            std::fs::write(dir.join("resolv.conf"), content).map_err(Error::Io)?;
            Some(dir)
        } else {
            None
        };
        let dns_temp_file_cstring: Option<std::ffi::CString> = dns_temp_dir.as_ref().map(|dir| {
            use std::os::unix::ffi::OsStrExt as _;
            std::ffi::CString::new(dir.join("resolv.conf").as_os_str().as_bytes()).unwrap()
        });

        // Links: resolve container names → IPs and write /etc/hosts temp file.
        if !self.links.is_empty() {
            if !self.namespaces.contains(Namespace::MOUNT) {
                return Err(Error::Io(io::Error::other(
                    "with_link requires Namespace::MOUNT",
                )));
            }
            if self.chroot_dir.is_none() {
                return Err(Error::Io(io::Error::other(
                    "with_link requires with_chroot",
                )));
            }
        }
        // Collect this container's network names for smart link resolution.
        let my_networks: Vec<String> = {
            let mut nets = Vec::new();
            if let Some(ref name) = bridge_network_name {
                nets.push(name.clone());
            }
            for name in &self.additional_networks {
                nets.push(name.clone());
            }
            nets
        };
        let hosts_temp_dir: Option<PathBuf> = if !self.links.is_empty() {
            let pid = unsafe { libc::getpid() };
            let n = HOSTS_COUNTER.fetch_add(1, Ordering::Relaxed);
            let dir = crate::paths::hosts_dir(pid, n);
            std::fs::create_dir_all(&dir).map_err(Error::Io)?;
            let mut content = String::from("127.0.0.1\tlocalhost\n");
            for (container_name, alias) in &self.links {
                // Try to resolve on a shared network first, fall back to any IP.
                let ip = resolve_container_ip_on_shared_network(container_name, &my_networks)
                    .or_else(|_| resolve_container_ip(container_name))
                    .map_err(Error::Io)?;
                if alias == container_name {
                    content.push_str(&format!("{}\t{}\n", ip, alias));
                } else {
                    content.push_str(&format!("{}\t{}\t{}\n", ip, alias, container_name));
                }
            }
            std::fs::write(dir.join("hosts"), content).map_err(Error::Io)?;
            Some(dir)
        } else {
            None
        };
        let hosts_temp_file_cstring: Option<std::ffi::CString> =
            hosts_temp_dir.as_ref().map(|dir| {
                use std::os::unix::ffi::OsStrExt as _;
                std::ffi::CString::new(dir.join("hosts").as_os_str().as_bytes()).unwrap()
            });

        // Create idmap sync pipes before the pre_exec closure so it can capture the FDs.
        let (idmap_ready_w_i, idmap_done_r_i, idmap_ready_r_i, idmap_done_w_i) = if use_id_helpers {
            let mut ready_fds = [0i32; 2];
            let mut done_fds = [0i32; 2];
            if unsafe { libc::pipe(ready_fds.as_mut_ptr()) } != 0
                || unsafe { libc::pipe(done_fds.as_mut_ptr()) } != 0
            {
                return Err(Error::Io(io::Error::last_os_error()));
            }
            (ready_fds[1], done_fds[0], ready_fds[0], done_fds[1])
        } else {
            (-1, -1, -1, -1)
        };

        unsafe {
            self.inner.pre_exec(move || {
                use std::ffi::CString;
                use std::ptr;

                // Step 0: PTY slave setup — runs before everything else.
                let setsid_ret = libc::setsid();
                if setsid_ret < 0 {
                    return Err(io::Error::last_os_error());
                }

                let ioctl_ret = libc::ioctl(slave_raw_fd, libc::TIOCSCTTY as _, 0 as libc::c_int);
                if ioctl_ret < 0 {
                    return Err(io::Error::last_os_error());
                }

                for dest_fd in [0i32, 1, 2] {
                    if slave_raw_fd != dest_fd {
                        let dup_ret = libc::dup2(slave_raw_fd, dest_fd);
                        if dup_ret < 0 {
                            return Err(io::Error::last_os_error());
                        }
                    }
                }
                libc::close(slave_raw_fd);

                // Steps 1–7: identical to spawn() from here
                if !namespaces.is_empty() {
                    if is_rootless && namespaces.contains(Namespace::USER) {
                        unshare(CloneFlags::CLONE_NEWUSER)
                            .map_err(|e| io::Error::other(format!("unshare USER: {}", e)))?;
                        // 1b. Write uid/gid maps.
                        if use_id_helpers {
                            let pid: u32 = libc::getpid() as u32;
                            let pid_bytes = pid.to_ne_bytes();
                            libc::write(
                                idmap_ready_w_i,
                                pid_bytes.as_ptr() as *const libc::c_void,
                                4,
                            );
                            libc::close(idmap_ready_w_i);
                            let mut buf = [0u8; 1];
                            libc::read(idmap_done_r_i, buf.as_mut_ptr() as *mut libc::c_void, 1);
                            libc::close(idmap_done_r_i);
                        } else {
                            use std::io::Write;
                            if !gid_maps.is_empty() {
                                let mut sg = std::fs::OpenOptions::new()
                                    .write(true)
                                    .open("/proc/self/setgroups")
                                    .map_err(|e| io::Error::other(format!("setgroups: {}", e)))?;
                                sg.write_all(b"deny\n").map_err(|e| {
                                    io::Error::other(format!("setgroups write: {}", e))
                                })?;
                            }
                            if !uid_maps.is_empty() {
                                let mut content = String::new();
                                for map in &uid_maps {
                                    content.push_str(&format!(
                                        "{} {} {}\n",
                                        map.inside, map.outside, map.count
                                    ));
                                }
                                let mut f = std::fs::OpenOptions::new()
                                    .write(true)
                                    .open("/proc/self/uid_map")
                                    .map_err(|e| io::Error::other(format!("uid_map: {}", e)))?;
                                f.write_all(content.as_bytes()).map_err(|e| {
                                    io::Error::other(format!("uid_map write: {}", e))
                                })?;
                            }
                            if !gid_maps.is_empty() {
                                let mut content = String::new();
                                for map in &gid_maps {
                                    content.push_str(&format!(
                                        "{} {} {}\n",
                                        map.inside, map.outside, map.count
                                    ));
                                }
                                let mut f = std::fs::OpenOptions::new()
                                    .write(true)
                                    .open("/proc/self/gid_map")
                                    .map_err(|e| io::Error::other(format!("gid_map: {}", e)))?;
                                f.write_all(content.as_bytes()).map_err(|e| {
                                    io::Error::other(format!("gid_map write: {}", e))
                                })?;
                            }
                        }
                        let remaining = namespaces & !Namespace::USER;
                        if !remaining.is_empty() {
                            unshare(remaining.to_clone_flags())
                                .map_err(|e| io::Error::other(format!("unshare error: {}", e)))?;
                        }
                    } else {
                        unshare(namespaces.to_clone_flags())
                            .map_err(|e| io::Error::other(format!("unshare error: {}", e)))?;
                    }

                    // linux.rootfsPropagation overrides the default MS_PRIVATE|MS_REC.
                    if namespaces.contains(Namespace::MOUNT) {
                        let prop_flags =
                            rootfs_propagation.unwrap_or(libc::MS_REC | libc::MS_PRIVATE);
                        let root = c"/";
                        let result = libc::mount(
                            ptr::null(),
                            root.as_ptr(),
                            ptr::null(),
                            prop_flags,
                            ptr::null(),
                        );
                        if result != 0 {
                            let err = io::Error::last_os_error();
                            // Any USER namespace causes MNT_LOCKED on inherited mounts (EINVAL).
                            let has_user_ns = is_rootless || namespaces.contains(Namespace::USER);
                            if !has_user_ns || err.raw_os_error() != Some(libc::EINVAL) {
                                return Err(err);
                            }
                        }
                    }

                    if bring_up_loopback {
                        crate::network::bring_up_loopback()
                            .map_err(|e| io::Error::other(format!("loopback up: {}", e)))?;
                    }

                    // Set container hostname in the UTS namespace.
                    if let Some(ref name) = hostname {
                        let r = libc::sethostname(name.as_ptr() as *const libc::c_char, name.len());
                        if r != 0 {
                            return Err(io::Error::last_os_error());
                        }
                    }
                }

                // PID namespace double-fork (same as spawn() Step 1.65).
                // See spawn() for detailed explanation of both cases.
                if namespaces.contains(Namespace::PID) {
                    let inner_pid = libc::fork();
                    if inner_pid < 0 {
                        return Err(io::Error::last_os_error());
                    }
                    if inner_pid > 0 {
                        // Intermediate (P): die if watcher is killed.
                        libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
                        // Close all fds > 2 — see spawn() Step 1.65 for rationale.
                        for fd in 3..1024 {
                            libc::close(fd);
                        }
                        let mut status: libc::c_int = 0;
                        loop {
                            let r = libc::waitpid(inner_pid, &mut status, 0);
                            if r == inner_pid {
                                break;
                            }
                            if r < 0 {
                                // std::io::Error::last_os_error() reads errno
                                // without allocating — portable across glibc and musl.
                                let e =
                                    std::io::Error::last_os_error().raw_os_error().unwrap_or(-1);
                                if e != libc::EINTR {
                                    libc::_exit(1);
                                }
                            }
                        }
                        if libc::WIFEXITED(status) {
                            libc::_exit(libc::WEXITSTATUS(status));
                        } else {
                            libc::_exit(128 + libc::WTERMSIG(status));
                        }
                    }
                    libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
                } else if let Some(&(pid_join_fd, _)) =
                    join_ns_fds.iter().find(|(_, ns)| *ns == Namespace::PID)
                {
                    // Joining an existing PID namespace — setns then double-fork.
                    if libc::setns(pid_join_fd, 0) != 0 {
                        return Err(io::Error::last_os_error());
                    }
                    let inner_pid = libc::fork();
                    if inner_pid < 0 {
                        return Err(io::Error::last_os_error());
                    }
                    if inner_pid > 0 {
                        // Intermediate (P): die if watcher is killed.
                        libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
                        for fd in 3..1024 {
                            libc::close(fd);
                        }
                        let mut status: libc::c_int = 0;
                        loop {
                            let r = libc::waitpid(inner_pid, &mut status, 0);
                            if r == inner_pid {
                                break;
                            }
                            let e = std::io::Error::last_os_error().raw_os_error().unwrap_or(-1);
                            if e != libc::EINTR {
                                libc::_exit(1);
                            }
                        }
                        if libc::WIFEXITED(status) {
                            libc::_exit(libc::WEXITSTATUS(status));
                        } else {
                            libc::_exit(128 + libc::WTERMSIG(status));
                        }
                    }
                    libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
                }

                // Bridge mode — join the pre-configured named netns via setns.
                if let Some(ref ns_path) = bridge_ns_path {
                    let fd = libc::open(ns_path.as_ptr(), libc::O_RDONLY | libc::O_CLOEXEC);
                    if fd < 0 {
                        return Err(io::Error::other(format!(
                            "open netns '{}': {}",
                            ns_path.to_string_lossy(),
                            io::Error::last_os_error()
                        )));
                    }
                    let ret = libc::setns(fd, libc::CLONE_NEWNET);
                    libc::close(fd);
                    if ret != 0 {
                        return Err(io::Error::other(format!(
                            "setns netns '{}': {}",
                            ns_path.to_string_lossy(),
                            io::Error::last_os_error()
                        )));
                    }
                }

                // Step 2: Set up UID/GID mapping if user namespace is active.
                // Skip for rootless — maps were written early in Step 1.
                if namespaces.contains(Namespace::USER) && !is_rootless {
                    use std::io::Write;

                    if !gid_maps.is_empty() {
                        let mut setgroups = std::fs::OpenOptions::new()
                            .write(true)
                            .open("/proc/self/setgroups")
                            .map_err(|e| io::Error::other(format!("open setgroups: {}", e)))?;
                        setgroups
                            .write_all(b"deny\n")
                            .map_err(|e| io::Error::other(format!("write setgroups: {}", e)))?;
                    }
                    // uid_map and gid_map MUST be written in a single write() call.
                    if !uid_maps.is_empty() {
                        let mut content = String::new();
                        for map in &uid_maps {
                            content.push_str(&format!(
                                "{} {} {}\n",
                                map.inside, map.outside, map.count
                            ));
                        }
                        let mut uid_map_file = std::fs::OpenOptions::new()
                            .write(true)
                            .open("/proc/self/uid_map")
                            .map_err(|e| io::Error::other(format!("open uid_map: {}", e)))?;
                        uid_map_file
                            .write_all(content.as_bytes())
                            .map_err(|e| io::Error::other(format!("write uid_map: {}", e)))?;
                    }
                    if !gid_maps.is_empty() {
                        let mut content = String::new();
                        for map in &gid_maps {
                            content.push_str(&format!(
                                "{} {} {}\n",
                                map.inside, map.outside, map.count
                            ));
                        }
                        let mut gid_map_file = std::fs::OpenOptions::new()
                            .write(true)
                            .open("/proc/self/gid_map")
                            .map_err(|e| io::Error::other(format!("open gid_map: {}", e)))?;
                        gid_map_file
                            .write_all(content.as_bytes())
                            .map_err(|e| io::Error::other(format!("write gid_map: {}", e)))?;
                    }
                }

                // Step 3.5: Mount overlayfs (if configured).
                let overlay_merged: Option<&std::ffi::CString> =
                    if let Some((lower, upper, work, merged)) = &overlay_cstrings {
                        if use_fuse_overlay {
                            // fuse-overlayfs already mounted by parent — skip kernel mount.
                            Some(merged)
                        } else {
                            let mut opts_str = format!(
                                "lowerdir={},upperdir={},workdir={}",
                                lower.to_string_lossy(),
                                upper.to_string_lossy(),
                                work.to_string_lossy()
                            );
                            if is_rootless {
                                opts_str.push_str(",userxattr");
                            }
                            let opts = std::ffi::CString::new(opts_str).unwrap();
                            let ov_type = c"overlay";
                            let ret = libc::mount(
                                ov_type.as_ptr(),
                                merged.as_ptr(),
                                ov_type.as_ptr(),
                                0,
                                opts.as_ptr() as *const libc::c_void,
                            );
                            if ret != 0 {
                                return Err(io::Error::last_os_error());
                            }
                            Some(merged)
                        }
                    } else {
                        None
                    };

                if let Some((ref new_root, ref put_old)) = pivot_root {
                    use std::os::unix::ffi::OsStrExt;
                    std::fs::create_dir_all(put_old).ok();

                    let new_root_c = CString::new(new_root.as_os_str().as_bytes()).unwrap();
                    let put_old_c = CString::new(put_old.as_os_str().as_bytes()).unwrap();

                    #[cfg(target_arch = "x86_64")]
                    const SYS_PIVOT_ROOT: i64 = 155;
                    #[cfg(target_arch = "aarch64")]
                    const SYS_PIVOT_ROOT: i64 = 41;

                    let result =
                        libc::syscall(SYS_PIVOT_ROOT, new_root_c.as_ptr(), put_old_c.as_ptr());
                    if result != 0 {
                        return Err(io::Error::last_os_error());
                    }
                    std::env::set_current_dir("/")?;

                    let put_old_rel = put_old
                        .strip_prefix(new_root)
                        .map_err(|_| io::Error::other("put_old must be inside new_root"))?;
                    let put_old_rel_c = CString::new(put_old_rel.as_os_str().as_bytes()).unwrap();
                    libc::umount2(put_old_rel_c.as_ptr(), libc::MNT_DETACH);
                } else if let Some(ref dir) = chroot_dir {
                    use std::os::unix::ffi::OsStrExt;

                    // When overlay is active, use the merged dir as the effective root.
                    let effective_root: &std::path::Path = overlay_merged
                        .as_ref()
                        .map(|m| std::path::Path::new(m.to_str().unwrap()))
                        .unwrap_or(dir.as_path());

                    // DNS: bind-mount the per-container resolv.conf over /etc/resolv.conf.
                    if let Some(ref dns_src) = dns_temp_file_cstring {
                        let etc_host = effective_root.join("etc");
                        std::fs::create_dir_all(&etc_host)
                            .map_err(|e| io::Error::other(format!("dns mkdir /etc: {}", e)))?;
                        let resolv_host = etc_host.join("resolv.conf");
                        let tgt_c =
                            std::ffi::CString::new(resolv_host.as_os_str().as_bytes()).unwrap();
                        let fd = libc::open(
                            tgt_c.as_ptr(),
                            libc::O_CREAT | libc::O_WRONLY | libc::O_CLOEXEC,
                            0o644u32,
                        );
                        if fd >= 0 {
                            libc::close(fd);
                        }
                        let r = libc::mount(
                            dns_src.as_ptr(),
                            tgt_c.as_ptr(),
                            ptr::null(),
                            libc::MS_BIND,
                            ptr::null(),
                        );
                        if r != 0 {
                            return Err(io::Error::other(format!(
                                "dns bind mount: {}",
                                io::Error::last_os_error()
                            )));
                        }
                    }

                    // Hosts: bind-mount the per-container hosts file over /etc/hosts.
                    if let Some(ref hosts_src) = hosts_temp_file_cstring {
                        let etc_host = effective_root.join("etc");
                        std::fs::create_dir_all(&etc_host)
                            .map_err(|e| io::Error::other(format!("hosts mkdir /etc: {}", e)))?;
                        let hosts_host = etc_host.join("hosts");
                        let tgt_c =
                            std::ffi::CString::new(hosts_host.as_os_str().as_bytes()).unwrap();
                        let fd = libc::open(
                            tgt_c.as_ptr(),
                            libc::O_CREAT | libc::O_WRONLY | libc::O_CLOEXEC,
                            0o644u32,
                        );
                        if fd >= 0 {
                            libc::close(fd);
                        }
                        let r = libc::mount(
                            hosts_src.as_ptr(),
                            tgt_c.as_ptr(),
                            ptr::null(),
                            libc::MS_BIND,
                            ptr::null(),
                        );
                        if r != 0 {
                            return Err(io::Error::other(format!(
                                "hosts bind mount: {}",
                                io::Error::last_os_error()
                            )));
                        }
                    }

                    // Skip readonly self-bind when overlay active — overlayfs IS a proper mount point.
                    if readonly_rootfs && overlay_merged.is_none() {
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
                        let host_target = effective_root.join(rel);
                        if bm.source.is_dir() {
                            std::fs::create_dir_all(&host_target).map_err(|e| {
                                io::Error::other(format!("bind mount mkdir: {}", e))
                            })?;
                        } else {
                            if let Some(parent) = host_target.parent() {
                                std::fs::create_dir_all(parent).map_err(|e| {
                                    io::Error::other(format!("bind mount mkdir: {}", e))
                                })?;
                            }
                            if !host_target.exists() {
                                std::fs::File::create(&host_target).map_err(|e| {
                                    io::Error::other(format!("bind mount mkfile: {}", e))
                                })?;
                            }
                        }
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
                                bm.source.display(),
                                host_target.display(),
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
                                    host_target.display(),
                                    io::Error::last_os_error()
                                )));
                            }
                        }
                    }

                    // Minimal /dev setup BEFORE chroot — host /dev paths still accessible.
                    if mount_dev {
                        use std::os::unix::ffi::OsStrExt as _;
                        let dev_host = effective_root.join("dev");
                        std::fs::create_dir_all(&dev_host)
                            .map_err(|e| io::Error::other(format!("mkdir /dev: {}", e)))?;
                        let dev_host_c = CString::new(dev_host.as_os_str().as_bytes()).unwrap();
                        let tmpfs_type = CString::new("tmpfs").unwrap();
                        let dev_opts = CString::new("mode=755,size=65536k").unwrap();
                        let r = libc::mount(
                            tmpfs_type.as_ptr(),
                            dev_host_c.as_ptr(),
                            tmpfs_type.as_ptr(),
                            libc::MS_NOSUID | libc::MS_STRICTATIME,
                            dev_opts.as_ptr() as *const libc::c_void,
                        );
                        if r != 0 {
                            let e = io::Error::last_os_error();
                            if !is_rootless {
                                return Err(io::Error::other(format!("mount tmpfs /dev: {}", e)));
                            }
                        } else {
                            let _ = std::fs::create_dir_all(dev_host.join("pts"));
                            let _ = std::fs::create_dir_all(dev_host.join("shm"));
                            let _ = std::fs::create_dir_all(dev_host.join("mqueue"));

                            let devpts_path =
                                CString::new(dev_host.join("pts").as_os_str().as_bytes()).unwrap();
                            let devpts_type = CString::new("devpts").unwrap();
                            let devpts_opts =
                                CString::new("newinstance,ptmxmode=0666,mode=0620,gid=5").unwrap();
                            let _ = libc::mount(
                                devpts_type.as_ptr(),
                                devpts_path.as_ptr(),
                                devpts_type.as_ptr(),
                                libc::MS_NOSUID | libc::MS_NOEXEC,
                                devpts_opts.as_ptr() as *const libc::c_void,
                            );

                            let shm_path =
                                CString::new(dev_host.join("shm").as_os_str().as_bytes()).unwrap();
                            let shm_opts = CString::new("mode=1777,size=65536k").unwrap();
                            let _ = libc::mount(
                                tmpfs_type.as_ptr(),
                                shm_path.as_ptr(),
                                tmpfs_type.as_ptr(),
                                libc::MS_NOSUID | libc::MS_NODEV | libc::MS_NOEXEC,
                                shm_opts.as_ptr() as *const libc::c_void,
                            );

                            let mqueue_path =
                                CString::new(dev_host.join("mqueue").as_os_str().as_bytes())
                                    .unwrap();
                            let mqueue_type = CString::new("mqueue").unwrap();
                            let _ = libc::mount(
                                mqueue_type.as_ptr(),
                                mqueue_path.as_ptr(),
                                mqueue_type.as_ptr(),
                                libc::MS_NOSUID | libc::MS_NODEV | libc::MS_NOEXEC,
                                ptr::null(),
                            );

                            for dev_name in &["null", "zero", "full", "random", "urandom", "tty"] {
                                let host_dev = CString::new(format!("/dev/{}", dev_name)).unwrap();
                                let target = dev_host.join(dev_name);
                                let target_c = CString::new(target.as_os_str().as_bytes()).unwrap();
                                let tfd = libc::open(
                                    target_c.as_ptr(),
                                    libc::O_CREAT | libc::O_WRONLY | libc::O_CLOEXEC,
                                    0o666u32,
                                );
                                if tfd >= 0 {
                                    libc::close(tfd);
                                }
                                let r = libc::mount(
                                    host_dev.as_ptr(),
                                    target_c.as_ptr(),
                                    ptr::null(),
                                    libc::MS_BIND,
                                    ptr::null(),
                                );
                                if r != 0 {
                                    log::debug!(
                                        "bind-mount /dev/{} failed: {}",
                                        dev_name,
                                        io::Error::last_os_error()
                                    );
                                }
                            }

                            let _ =
                                std::os::unix::fs::symlink("/proc/self/fd", dev_host.join("fd"));
                            let _ = std::os::unix::fs::symlink(
                                "/proc/self/fd/0",
                                dev_host.join("stdin"),
                            );
                            let _ = std::os::unix::fs::symlink(
                                "/proc/self/fd/1",
                                dev_host.join("stdout"),
                            );
                            let _ = std::os::unix::fs::symlink(
                                "/proc/self/fd/2",
                                dev_host.join("stderr"),
                            );
                            let _ = std::os::unix::fs::symlink("pts/ptmx", dev_host.join("ptmx"));
                        }
                    }

                    chroot(effective_root)
                        .map_err(|e| io::Error::other(format!("chroot error: {}", e)))?;
                    let cwd = container_cwd
                        .as_deref()
                        .unwrap_or(std::path::Path::new("/"));
                    std::env::set_current_dir(cwd)?;
                }

                if mount_proc {
                    // Ensure /proc exists — some minimal images omit it.
                    let _ = std::fs::create_dir_all("/proc");
                    let proc_src = CString::new("proc").unwrap();
                    let proc_tgt = CString::new("/proc").unwrap();
                    let result = libc::mount(
                        proc_src.as_ptr(),
                        proc_tgt.as_ptr(),
                        proc_src.as_ptr(),
                        0,
                        ptr::null(),
                    );
                    // In rootless mode, proc mount fails without an owned PID namespace.
                    // With USER+PID (auto-added by spawn()), proc succeeds. Only skip in rootless.
                    if result != 0 && !is_rootless {
                        return Err(io::Error::other(format!(
                            "mount proc: {}",
                            io::Error::last_os_error()
                        )));
                    }
                }

                if mount_sys {
                    // Ensure /sys exists — some minimal images omit it.
                    let _ = std::fs::create_dir_all("/sys");
                    let sys = CString::new("/sys").unwrap();
                    let sysfs = CString::new("sysfs").unwrap();
                    let result = libc::mount(
                        sys.as_ptr(),
                        sys.as_ptr(),
                        sysfs.as_ptr(),
                        libc::MS_BIND,
                        ptr::null(),
                    );
                    // Rootless: /sys bind may fail on locked mounts; inherited /sys is still usable.
                    if result != 0 && !is_rootless {
                        return Err(io::Error::other(format!(
                            "mount sys: {}",
                            io::Error::last_os_error()
                        )));
                    }
                }

                // Mount tmpfs filesystems AFTER chroot
                for tm in &tmpfs_mounts {
                    std::fs::create_dir_all(&tm.target)
                        .map_err(|e| io::Error::other(format!("tmpfs mkdir: {}", e)))?;
                    let tgt_c = CString::new(tm.target.as_os_str().as_encoded_bytes()).unwrap();
                    let tmpfs_c = CString::new("tmpfs").unwrap();
                    let opts_c = CString::new(tm.options.as_bytes()).unwrap();
                    let opts_ptr = if tm.options.is_empty() {
                        ptr::null()
                    } else {
                        opts_c.as_ptr() as *const libc::c_void
                    };
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
                            tm.target.display(),
                            io::Error::last_os_error()
                        )));
                    }
                }

                // Mount kernel filesystems (proc, sysfs, devpts, mqueue, cgroup2, …)
                // specified by OCI config.json `mounts` entries.
                for km in &kernel_mounts {
                    std::fs::create_dir_all(&km.target).map_err(|e| {
                        io::Error::other(format!(
                            "kernel mount mkdir {}: {}",
                            km.target.display(),
                            e
                        ))
                    })?;
                    let tgt_c = CString::new(km.target.as_os_str().as_encoded_bytes()).unwrap();
                    let src_c = CString::new(km.source.as_bytes()).unwrap();
                    let fst_c = CString::new(km.fs_type.as_bytes()).unwrap();
                    let dat_c = CString::new(km.data.as_bytes()).unwrap();
                    let dat_ptr: *const libc::c_void = if km.data.is_empty() {
                        ptr::null()
                    } else {
                        dat_c.as_ptr() as *const libc::c_void
                    };
                    let result = libc::mount(
                        src_c.as_ptr(),
                        tgt_c.as_ptr(),
                        fst_c.as_ptr(),
                        km.flags,
                        dat_ptr,
                    );
                    if result != 0 {
                        return Err(io::Error::other(format!(
                            "mount {} ({}) at {}: {}",
                            km.fs_type,
                            km.source,
                            km.target.display(),
                            io::Error::last_os_error()
                        )));
                    }
                }

                for (key, value) in &sysctl {
                    let proc_path = format!("/proc/sys/{}", key.replace('.', "/"));
                    let path_c = match std::ffi::CString::new(proc_path.as_bytes()) {
                        Ok(p) => p,
                        Err(_) => continue,
                    };
                    let fd = libc::open(path_c.as_ptr(), libc::O_WRONLY | libc::O_TRUNC, 0);
                    if fd >= 0 {
                        let bytes = value.as_bytes();
                        libc::write(fd, bytes.as_ptr() as *const libc::c_void, bytes.len());
                        libc::close(fd);
                    }
                }

                if !devices.is_empty() {
                    for dev in &devices {
                        let path_c =
                            match std::ffi::CString::new(dev.path.as_os_str().as_encoded_bytes()) {
                                Ok(p) => p,
                                Err(_) => continue,
                            };
                        let type_bits: libc::mode_t = match dev.kind {
                            'b' => libc::S_IFBLK,
                            'p' => libc::S_IFIFO,
                            _ => libc::S_IFCHR,
                        };
                        let devnum =
                            libc::makedev(dev.major as libc::c_uint, dev.minor as libc::c_uint);
                        let r = libc::mknod(
                            path_c.as_ptr(),
                            type_bits | (dev.mode as libc::mode_t),
                            devnum,
                        );
                        if r == 0 && (dev.uid != 0 || dev.gid != 0) {
                            libc::chown(path_c.as_ptr(), dev.uid, dev.gid);
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
                        libc::mount(
                            dev_null.as_ptr(),
                            path_c.as_ptr(),
                            ptr::null(),
                            libc::MS_BIND,
                            ptr::null(),
                        );
                    }
                }

                if !readonly_paths.is_empty() {
                    for path in &readonly_paths {
                        let path_c = match CString::new(path.as_os_str().as_encoded_bytes()) {
                            Ok(p) => p,
                            Err(_) => continue,
                        };
                        let r = libc::mount(
                            path_c.as_ptr(),
                            path_c.as_ptr(),
                            ptr::null(),
                            libc::MS_BIND,
                            ptr::null(),
                        );
                        if r != 0 {
                            continue;
                        }
                        libc::mount(
                            ptr::null(),
                            path_c.as_ptr(),
                            ptr::null(),
                            libc::MS_REMOUNT | libc::MS_BIND | libc::MS_RDONLY,
                            ptr::null(),
                        );
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

                // Drop capabilities after all mount operations.
                // Same logic as step 4.86 in the chroot path.
                if let Some(keep_caps) = capabilities {
                    const PR_CAPBSET_DROP: i32 = 24;
                    for cap in 0..41u64 {
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

                    let bits = keep_caps.bits();
                    let lo = bits as u32;
                    let hi = (bits >> 32) as u32;

                    #[repr(C)]
                    struct CapHeader {
                        version: u32,
                        pid: i32,
                    }
                    #[repr(C)]
                    struct CapData {
                        effective: u32,
                        permitted: u32,
                        inheritable: u32,
                    }

                    let header = CapHeader {
                        version: 0x2008_0522,
                        pid: 0,
                    };
                    let data = [
                        CapData {
                            effective: lo,
                            permitted: lo,
                            inheritable: lo,
                        },
                        CapData {
                            effective: hi,
                            permitted: hi,
                            inheritable: hi,
                        },
                    ];

                    let ret =
                        libc::syscall(libc::SYS_capset, &header as *const CapHeader, data.as_ptr());
                    if ret != 0 {
                        return Err(io::Error::last_os_error());
                    }
                }

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

                // User callback BEFORE setuid — exec's callback does setns
                // which requires CAP_SYS_ADMIN.
                if let Some(cb) = &user_pre_exec {
                    cb()?;
                }

                for (fd, ns) in &join_ns_fds {
                    if *ns == Namespace::PID {
                        // Handled at step 1.65 (double-fork) — skip here.
                        continue;
                    }
                    let result = libc::setns(*fd, 0);
                    if result != 0 {
                        return Err(io::Error::last_os_error());
                    }
                }

                // Set UID/GID after all privileged operations (user callback, ns joins).
                if let Some(gid_val) = gid {
                    let result = libc::setgid(gid_val);
                    if result != 0 {
                        return Err(io::Error::other(format!(
                            "setgid: {}",
                            io::Error::last_os_error()
                        )));
                    }
                }
                if let Some(uid_val) = uid {
                    let result = libc::setuid(uid_val);
                    if result != 0 {
                        return Err(io::Error::other(format!(
                            "setuid: {}",
                            io::Error::last_os_error()
                        )));
                    }
                }

                if no_new_privileges {
                    const PR_SET_NO_NEW_PRIVS: i32 = 38;
                    let result = libc::prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0);
                    if result != 0 {
                        return Err(io::Error::last_os_error());
                    }
                }

                // PTY slave setup for OCI terminal mode (same logic as spawn()).
                if let Some(slave_fd) = pty_slave {
                    libc::setsid();
                    libc::dup2(slave_fd, 0);
                    libc::dup2(slave_fd, 1);
                    libc::dup2(slave_fd, 2);
                    libc::ioctl(slave_fd, libc::TIOCSCTTY as libc::c_ulong, 0i32);
                    if slave_fd > 2 {
                        libc::close(slave_fd);
                    }
                }

                if let Some(ref filter) = seccomp_filter {
                    crate::seccomp::apply_filter(filter)?;
                }

                // Step 8: OCI sync (same as spawn()).
                if let Some((ready_w, listen_fd)) = oci_sync {
                    let pid: i32 = libc::getpid();
                    let pid_bytes = pid.to_ne_bytes();
                    libc::write(ready_w, pid_bytes.as_ptr() as *const libc::c_void, 4);
                    libc::close(ready_w);
                    let conn = libc::accept4(listen_fd, ptr::null_mut(), ptr::null_mut(), 0);
                    if conn >= 0 {
                        let mut buf = [0u8; 1];
                        libc::read(conn, buf.as_mut_ptr() as *mut libc::c_void, 1);
                        libc::close(conn);
                    }
                    libc::close(listen_fd);
                }

                Ok(())
            });
        }

        // If using ID helpers, spawn the helper thread now (before fork).
        // It reads the child PID from the pipe, runs newuidmap/newgidmap, signals done.
        if use_id_helpers {
            let uid_maps_h = self.uid_maps.clone();
            let gid_maps_h = self.gid_maps.clone();
            let ready_r = idmap_ready_r_i;
            let done_w = idmap_done_w_i;

            std::thread::spawn(move || {
                let mut pid_bytes = [0u8; 4];
                let n =
                    unsafe { libc::read(ready_r, pid_bytes.as_mut_ptr() as *mut libc::c_void, 4) };
                unsafe { libc::close(ready_r) };
                if n != 4 {
                    unsafe { libc::close(done_w) };
                    return;
                }
                let child_pid = u32::from_ne_bytes(pid_bytes);

                if let Err(e) = crate::idmap::apply_uid_map(child_pid, &uid_maps_h) {
                    log::warn!("newuidmap failed: {}", e);
                }
                if let Err(e) = crate::idmap::apply_gid_map(child_pid, &gid_maps_h) {
                    log::warn!("newgidmap failed: {}", e);
                }

                unsafe { libc::write(done_w, [0u8].as_ptr() as *const libc::c_void, 1) };
                unsafe { libc::close(done_w) };
            });
        }

        // Spawn the process
        let child_inner = match self.inner.spawn() {
            Ok(c) => c,
            Err(e) => {
                if use_id_helpers {
                    // Close child-side pipe ends to unblock the helper thread.
                    unsafe { libc::close(idmap_ready_w_i) };
                    unsafe { libc::close(idmap_done_r_i) };
                }
                return Err(Error::Spawn(e));
            }
        };

        // Close child-side pipe ends in the parent (child inherited them via fork).
        if use_id_helpers {
            unsafe { libc::close(idmap_ready_w_i) };
            unsafe { libc::close(idmap_done_r_i) };
        }

        // Close the slave in the parent — only the child should have it.
        // If we keep it open, POLLHUP on the master will never fire when
        // the container exits (because we still hold a reference to the slave).
        drop(slave);
        drop(join_ns_files);

        // Create cgroup and add child PID (parent-side, after fork)
        let cgroup = if let Some(ref cfg) = self.cgroup_config {
            if is_rootless {
                match crate::cgroup_rootless::setup_rootless_cgroup(cfg, child_inner.id()) {
                    Ok(cg) => Some(CgroupHandle::Rootless(cg)),
                    Err(e) => {
                        log::warn!("rootless cgroup setup failed, skipping: {}", e);
                        None
                    }
                }
            } else {
                match crate::cgroup::setup_cgroup(cfg, child_inner.id()) {
                    Ok(cg) => Some(CgroupHandle::Root(cg)),
                    Err(e) => return Err(Error::Io(e)),
                }
            }
        } else {
            None
        };

        // Bridge networking was fully set up before fork; nothing to do here.
        let network = bridge_network;

        // Pasta: spawn the relay after the child has exec'd (/proc/{pid}/ns/net is live).
        let pasta: Option<crate::network::PastaSetup> = if is_pasta {
            Some(
                crate::network::setup_pasta_network(child_inner.id(), &self.port_forwards)
                    .map_err(Error::Io)?,
            )
        } else {
            None
        };

        Ok(crate::pty::InteractiveSession {
            master,
            child: Child {
                inner: child_inner,
                cgroup,
                network,
                secondary_networks,
                pasta,
                overlay_merged_dir,
                dns_temp_dir,
                hosts_temp_dir,
                fuse_overlay_child,
                fuse_overlay_merged,
            },
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
///     .args(["5"])
///     .with_namespaces(Namespace::PID)
///     .spawn()?;
///
/// println!("Spawned process with PID: {}", child.pid());
///
/// let status = child.wait()?;
/// println!("Process exited with: {:?}", status);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
/// Handle for either a root (cgroups-rs) or rootless (direct fs) cgroup.
pub(crate) enum CgroupHandle {
    Root(cgroups_rs::fs::Cgroup),
    Rootless(crate::cgroup_rootless::RootlessCgroup),
}

pub struct Child {
    inner: process::Child,
    /// Optional cgroup for this container. Deleted after the child exits.
    pub(crate) cgroup: Option<CgroupHandle>,
    /// Optional network state (veth pair). Torn down after the child exits.
    network: Option<crate::network::NetworkSetup>,
    /// Secondary network attachments (eth1, eth2, ...). Torn down before primary.
    secondary_networks: Vec<crate::network::NetworkSetup>,
    /// Optional pasta relay process. Killed after the child exits.
    pasta: Option<crate::network::PastaSetup>,
    /// Overlay merged-dir created before fork; removed after the child exits.
    overlay_merged_dir: Option<PathBuf>,
    /// Per-container DNS temp dir (`/run/remora/dns-{pid}-{n}/`); removed after child exits.
    dns_temp_dir: Option<PathBuf>,
    /// Per-container hosts temp dir; removed after child exits.
    hosts_temp_dir: Option<PathBuf>,
    /// fuse-overlayfs subprocess (rootless fallback). Unmounted + reaped after child exits.
    fuse_overlay_child: Option<std::process::Child>,
    /// Merged dir path for fuse-overlayfs unmount (needed because overlay_merged_dir is the
    /// parent's "merged" subdir, and we need the exact path for fusermount3).
    fuse_overlay_merged: Option<PathBuf>,
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

    /// Returns the container's bridge IP (e.g. `172.19.0.5`) if bridge networking is active.
    pub fn container_ip(&self) -> Option<String> {
        self.network.as_ref().map(|n| n.container_ip.to_string())
    }

    /// Returns all network IPs as `(network_name, ip_string)` pairs.
    ///
    /// Includes the primary network (if any) and all secondary networks.
    pub fn container_ips(&self) -> Vec<(&str, String)> {
        let mut ips = Vec::new();
        if let Some(ref net) = self.network {
            ips.push((net.network_name.as_str(), net.container_ip.to_string()));
        }
        for net in &self.secondary_networks {
            ips.push((net.network_name.as_str(), net.container_ip.to_string()));
        }
        ips
    }

    /// Returns the container's IP on a specific network, or `None` if not attached.
    pub fn container_ip_on(&self, network_name: &str) -> Option<String> {
        if let Some(ref net) = self.network {
            if net.network_name == network_name {
                return Some(net.container_ip.to_string());
            }
        }
        for net in &self.secondary_networks {
            if net.network_name == network_name {
                return Some(net.container_ip.to_string());
            }
        }
        None
    }

    /// Returns the secondary network setups (for test assertions).
    pub fn secondary_networks(&self) -> &[crate::network::NetworkSetup] {
        &self.secondary_networks
    }

    /// Returns the overlay merged-dir path if an overlay filesystem was configured.
    ///
    /// The path is removed by `wait()` / `wait_with_output()`. Useful in tests to
    /// verify cleanup without relying on global directory state.
    pub fn overlay_merged_dir(&self) -> Option<&std::path::Path> {
        self.overlay_merged_dir.as_deref()
    }

    /// Take ownership of the child's piped stdout handle.
    ///
    /// Returns `None` if stdout was not set to `Stdio::Piped`, or if already taken.
    /// Call this once before `wait()` to stream output concurrently.
    pub fn take_stdout(&mut self) -> Option<std::process::ChildStdout> {
        self.inner.stdout.take()
    }

    /// Take ownership of the child's piped stderr handle.
    ///
    /// Returns `None` if stderr was not set to `Stdio::Piped`, or if already taken.
    /// Call this once before `wait()` to stream output concurrently.
    pub fn take_stderr(&mut self) -> Option<std::process::ChildStderr> {
        self.inner.stderr.take()
    }

    /// Wait for the child process to exit.
    ///
    /// This will block until the process terminates and return its exit status.
    /// If a cgroup was configured, it is deleted after the child exits.
    pub fn wait(&mut self) -> Result<ExitStatus, Error> {
        let status = self.inner.wait().map_err(Error::Wait)?;
        self.teardown_resources(false);
        Ok(ExitStatus { inner: status })
    }

    /// Wait for the child process to exit, preserving the overlay base directory.
    ///
    /// Performs all normal teardown (cgroup, network, pasta, fuse-overlayfs, dns/hosts)
    /// but **does not remove** the overlay base directory. Instead, it returns the
    /// path to the overlay base dir (parent of `merged/`) so the caller can inspect
    /// the upper layer before cleaning up.
    ///
    /// Used by the build engine to extract modified files from each RUN step.
    pub fn wait_preserve_overlay(&mut self) -> Result<(ExitStatus, Option<PathBuf>), Error> {
        let status = self.inner.wait().map_err(Error::Wait)?;
        // Capture the overlay base dir path before teardown consumes it.
        let overlay_base = self
            .overlay_merged_dir
            .as_ref()
            .and_then(|merged| merged.parent().map(|p| p.to_path_buf()));
        self.teardown_resources(true);
        Ok((ExitStatus { inner: status }, overlay_base))
    }

    /// Wait for the child to exit and collect all output.
    ///
    /// Returns (exit_status, stdout_bytes, stderr_bytes).
    /// Only works if Stdio::Piped was set for stdout/stderr.
    /// If a cgroup was configured, it is deleted after the child exits.
    pub fn wait_with_output(&mut self) -> Result<(ExitStatus, Vec<u8>, Vec<u8>), Error> {
        use std::io::Read;
        // Drain stdout/stderr before waiting (avoids pipe deadlock on large output).
        let mut stdout_buf = Vec::new();
        let mut stderr_buf = Vec::new();
        if let Some(mut out) = self.inner.stdout.take() {
            let _ = out.read_to_end(&mut stdout_buf);
        }
        if let Some(mut err) = self.inner.stderr.take() {
            let _ = err.read_to_end(&mut stderr_buf);
        }
        let status = self.inner.wait().map_err(Error::Wait)?;
        self.teardown_resources(false);
        Ok((ExitStatus { inner: status }, stdout_buf, stderr_buf))
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
            match cg {
                CgroupHandle::Root(cg) => crate::cgroup::read_stats(cg).map_err(Error::Io),
                CgroupHandle::Rootless(cg) => {
                    crate::cgroup_rootless::read_rootless_stats(cg).map_err(Error::Io)
                }
            }
        } else {
            Ok(crate::cgroup::ResourceStats::default())
        }
    }

    /// Tear down all resources owned by this `Child`.
    ///
    /// Uses `take()` / `drain()` so the method is idempotent — calling it
    /// twice (e.g. from `wait()` then `Drop`) is harmless.
    ///
    /// When `preserve_overlay` is true the overlay base directory is kept
    /// intact (used by the build engine to extract upper-layer diffs).
    fn teardown_resources(&mut self, preserve_overlay: bool) {
        if let Some(cg) = self.cgroup.take() {
            match cg {
                CgroupHandle::Root(cg) => crate::cgroup::teardown_cgroup(cg),
                CgroupHandle::Rootless(ref cg) => {
                    crate::cgroup_rootless::teardown_rootless_cgroup(cg)
                }
            }
        }
        // Tear down secondary networks before primary (veths before netns).
        for net in self.secondary_networks.drain(..) {
            crate::network::teardown_secondary_network(&net);
        }
        if let Some(net) = self.network.take() {
            crate::network::teardown_network(net);
        }
        if let Some(ref mut p) = self.pasta.take() {
            crate::network::teardown_pasta_network(p);
        }
        // Unmount fuse-overlayfs before removing the overlay base dir.
        if let Some(ref fuse_merged) = self.fuse_overlay_merged.take() {
            let merged_str = fuse_merged.to_string_lossy();
            let unmounted = std::process::Command::new("fusermount3")
                .args(["-u", &*merged_str])
                .status()
                .is_ok_and(|s| s.success())
                || std::process::Command::new("fusermount")
                    .args(["-u", &*merged_str])
                    .status()
                    .is_ok_and(|s| s.success());
            if !unmounted {
                log::warn!(
                    "failed to unmount fuse-overlayfs at {}; is fusermount3 installed?",
                    merged_str
                );
            }
        }
        if let Some(ref mut fuse_child) = self.fuse_overlay_child.take() {
            match fuse_child.try_wait() {
                Ok(Some(_)) => {}
                _ => {
                    log::warn!("fuse-overlayfs did not exit after unmount; killing");
                    let _ = fuse_child.kill();
                }
            }
            let _ = fuse_child.wait();
        }
        if !preserve_overlay {
            if let Some(ref merged) = self.overlay_merged_dir.take() {
                if let Some(parent) = merged.parent() {
                    let _ = std::fs::remove_dir_all(parent);
                }
            }
        }
        if let Some(ref dir) = self.dns_temp_dir.take() {
            let _ = std::fs::remove_dir_all(dir);
        }
        if let Some(ref dir) = self.hosts_temp_dir.take() {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

impl Drop for Child {
    fn drop(&mut self) {
        // Kill the child process if still alive, then reap to avoid zombies.
        let _ = self.inner.kill();
        let _ = self.inner.wait();
        // Teardown resources that wait() would normally clean up.
        // All fields use take()/drain() so this is safe even if wait() already ran.
        self.teardown_resources(false);
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
    pub resource: RlimitResource,
    /// Soft limit (can be increased up to hard limit)
    pub soft: libc::rlim_t,
    /// Hard limit (requires privileges to increase)
    pub hard: libc::rlim_t,
}

/// A device node to create inside the container.
///
/// Used with `with_device()` to create character (`'c'`), block (`'b'`), or
/// FIFO (`'p'`) devices in the container's `/dev`.
#[derive(Debug, Clone)]
pub struct DeviceNode {
    /// Absolute path inside the container (e.g. `/dev/fuse`)
    pub path: PathBuf,
    /// Device type: `'c'` character, `'b'` block, `'p'` FIFO
    pub kind: char,
    /// Major device number
    pub major: u64,
    /// Minor device number
    pub minor: u64,
    /// File mode (permissions), e.g. `0o666`
    pub mode: u32,
    /// Owner UID
    pub uid: u32,
    /// Owner GID
    pub gid: u32,
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

        let diff = ns1 & !ns2; // Items in ns1 but not in ns2

        assert!(diff.contains(Namespace::UTS));
        assert!(diff.contains(Namespace::MOUNT));
        assert!(!diff.contains(Namespace::PID));
        assert!(!diff.contains(Namespace::NET));
    }

    #[test]
    fn test_command_builder_pattern() {
        let cmd = Command::new("/bin/echo")
            .args(["hello", "world"])
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
            .args(["arg1"])
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
            Error::Io(_) => {}
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
