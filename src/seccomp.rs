//! Seccomp (Secure Computing Mode) syscall filtering for containers.
//!
//! This module provides seccomp-BPF filtering to restrict which system calls
//! a containerized process can execute. This is a critical security feature
//! that prevents container escape and privilege escalation.
//!
//! # Overview
//!
//! Seccomp works by installing a Berkeley Packet Filter (BPF) program into the
//! kernel that filters system calls. When a process attempts to make a syscall,
//! the BPF program decides whether to:
//! - Allow it (SCMP_ACT_ALLOW)
//! - Kill the process (SCMP_ACT_KILL)
//! - Return an error (SCMP_ACT_ERRNO)
//!
//! # Profiles
//!
//! We provide several pre-configured profiles:
//!
//! - **Docker**: Matches Docker's default seccomp profile (~44 blocked syscalls)
//! - **Minimal**: Extremely restrictive, allows only ~40 essential syscalls
//! - **None**: No filtering (unsafe, for debugging only)
//!
//! # Examples
//!
//! ```no_run
//! use remora::container::{Command, SeccompProfile};
//!
//! // Use Docker's default profile (recommended)
//! let child = Command::new("/bin/sh")
//!     .with_seccomp_default()
//!     .spawn()?;
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! # Security
//!
//! Docker's default profile blocks dangerous syscalls including:
//! - Container escapes: `ptrace`, `personality`, `bpf`, `perf_event_open`
//! - System modification: `reboot`, `swapon`, `swapoff`, `kexec_load`
//! - Namespace manipulation: `unshare`, `mount`, `umount2`, `pivot_root`, `chroot`
//! - Clock manipulation: `clock_adjtime`, `settimeofday`, `adjtimex`
//! - Kernel modules: `init_module`, `delete_module`, `finit_module`
//! - Keyring: `add_key`, `request_key`, `keyctl`
//! - And many more...
//!
//! # Performance
//!
//! Seccomp filtering adds ~20-50ns overhead per syscall, which is negligible
//! for most workloads (< 0.01% for typical applications).

use seccompiler::{BpfProgram, SeccompAction, SeccompFilter, SeccompRule};
use std::collections::BTreeMap;
use std::io;

/// Seccomp profile configurations.
///
/// Determines which syscalls are allowed or blocked for the containerized process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SeccompProfile {
    /// Docker's default seccomp profile.
    ///
    /// Blocks ~44 dangerous syscalls while allowing normal application behavior.
    /// This is the recommended profile for production containers.
    Docker,

    /// Minimal seccomp profile.
    ///
    /// Extremely restrictive - allows only ~40 essential syscalls needed for
    /// basic process execution. Use for highly constrained environments.
    Minimal,

    /// No seccomp filtering.
    ///
    /// WARNING: Unsafe! Only use for debugging or when seccomp is not needed.
    /// Containers will have full syscall access.
    None,
}

/// Generate Docker's default seccomp filter.
///
/// This matches the profile used by Docker and most container runtimes.
/// It blocks dangerous syscalls that could lead to container escape or
/// privilege escalation while allowing normal application operation.
///
/// # Returns
///
/// A compiled BPF program ready to be loaded into the kernel.
pub fn docker_default_filter() -> Result<BpfProgram, io::Error> {
    use std::convert::TryInto;

    // Syscalls blocked by Docker's default profile.
    // These are dangerous and commonly used in container escapes.
    let blocked_syscalls = vec![
        // Namespace and isolation manipulation (container escape vectors)
        "unshare",      // Create new namespaces
        "setns",        // Join existing namespaces
        "mount",        // Mount filesystems
        "umount",       // Unmount filesystems
        "umount2",      // Unmount with flags
        "pivot_root",   // Change root mount point
        "chroot",       // Change root directory

        // Process tracing and debugging (escape via ptrace injection)
        "ptrace",       // Trace processes
        "process_vm_readv",  // Read process memory
        "process_vm_writev", // Write process memory

        // Kernel module manipulation (kernel-level access)
        "init_module",    // Load kernel module
        "finit_module",   // Load kernel module from fd
        "delete_module",  // Unload kernel module

        // System control (DOS or system takeover)
        "reboot",         // Reboot system
        "kexec_load",     // Load new kernel
        "kexec_file_load", // Load new kernel from file

        // Time manipulation (affects host clock)
        "clock_settime",  // Set system clock
        "settimeofday",   // Set time of day
        "clock_adjtime",  // Adjust system clock
        "adjtimex",       // Tune kernel clock

        // Swap manipulation
        "swapon",         // Enable swap
        "swapoff",        // Disable swap

        // Kernel keyring (credential access)
        "add_key",        // Add key to keyring
        "request_key",    // Request key from keyring
        "keyctl",         // Manipulate keyring

        // BPF and performance monitoring (kernel inspection/manipulation)
        "bpf",            // Extended BPF syscall
        "perf_event_open", // Performance monitoring

        // CPU affinity and NUMA (resource isolation bypass)
        "mbind",          // Set NUMA memory policy
        "set_mempolicy",  // Set NUMA memory policy
        "migrate_pages",  // Move process pages between NUMA nodes
        "move_pages",     // Move process pages

        // Quota manipulation
        "quotactl",       // Filesystem quotas

        // User namespace credential manipulation (when not using user namespaces)
        "setuid",         // Set user ID (blocked by default, allowed with conditions in real Docker)
        "setgid",         // Set group ID

        // Architecture-specific personality (can bypass security)
        "personality",    // Set execution domain

        // ACCT and system accounting
        "acct",           // Process accounting

        // Lookup dcookie (kernel debugging)
        "lookup_dcookie", // Get file name from dcookie

        // Name to handle (expose kernel pointers)
        "name_to_handle_at", // Get handle for pathname
        "open_by_handle_at", // Open file via handle

        // User namespace (when not using USER namespace)
        "userfaultfd",    // User fault handling
    ];

    // Build map of blocked syscalls with empty rule vectors (match all arguments)
    let rules: BTreeMap<i64, Vec<SeccompRule>> = blocked_syscalls
        .iter()
        .filter_map(|name| syscall_number(name).ok().map(|num| (num, vec![])))
        .collect();

    // Create filter with Docker's semantics:
    // - Default action: Allow (permit all syscalls not in the map)
    // - Match action: Errno(EPERM) (block syscalls in the map)
    // - Target: current architecture
    let target_arch = std::env::consts::ARCH
        .try_into()
        .map_err(|e| io::Error::other(format!("Unsupported architecture: {:?}", e)))?;

    let filter = SeccompFilter::new(
        rules,
        SeccompAction::Allow,            // Default: allow everything else
        SeccompAction::Errno(libc::EPERM as u32),  // Matched syscalls get EPERM
        target_arch,
    )
    .map_err(|e| io::Error::other(format!("Failed to create seccomp filter: {}", e)))?;

    // Convert to BPF program
    let program: BpfProgram = filter
        .try_into()
        .map_err(|e| io::Error::other(format!("Failed to compile seccomp filter: {}", e)))?;

    Ok(program)
}

/// Generate a minimal seccomp filter.
///
/// This is an extremely restrictive profile that only allows essential syscalls
/// needed for basic process execution. Useful for highly constrained containers.
pub fn minimal_filter() -> Result<BpfProgram, io::Error> {
    use std::convert::TryInto;

    // Only allow essential syscalls for basic process execution
    let allowed_syscalls = vec![
        // Process control
        "exit", "exit_group", "wait4", "waitid",
        // Memory
        "brk", "mmap", "munmap", "mprotect", "mremap",
        // I/O
        "read", "write", "readv", "writev", "pread64", "pwrite64",
        "open", "openat", "close", "lseek",
        // File metadata
        "fstat", "stat", "lstat", "newfstatat", "access", "faccessat",
        // Directories
        "getcwd", "chdir", "fchdir",
        // Signals
        "rt_sigaction", "rt_sigprocmask", "rt_sigreturn",
        // Time
        "clock_gettime", "gettimeofday", "time", "nanosleep",
        // Process info
        "getpid", "getuid", "getgid", "geteuid", "getegid",
        // Arch-specific
        "arch_prctl",
        // Futex for threading
        "futex", "set_robust_list", "get_robust_list",
    ];

    // Build map of allowed syscalls with empty rule vectors (match all arguments)
    let rules: BTreeMap<i64, Vec<SeccompRule>> = allowed_syscalls
        .iter()
        .filter_map(|name| syscall_number(name).ok().map(|num| (num, vec![])))
        .collect();

    // Create filter with minimal semantics:
    // - Default action: Errno(EPERM) (deny all syscalls not in the map)
    // - Match action: Allow (permit syscalls in the map)
    // - Target: current architecture
    let target_arch = std::env::consts::ARCH
        .try_into()
        .map_err(|e| io::Error::other(format!("Unsupported architecture: {:?}", e)))?;

    let filter = SeccompFilter::new(
        rules,
        SeccompAction::Errno(libc::EPERM as u32),  // Default: deny everything
        SeccompAction::Allow,                       // Matched syscalls allowed
        target_arch,
    )
    .map_err(|e| io::Error::other(format!("Failed to create minimal filter: {}", e)))?;

    // Convert to BPF program
    let program: BpfProgram = filter
        .try_into()
        .map_err(|e| io::Error::other(format!("Failed to compile minimal filter: {}", e)))?;

    Ok(program)
}

/// Get syscall number for a given syscall name on the current architecture.
///
/// This uses a simple mapping for common syscalls. For production use,
/// you'd want a complete mapping or use libseccomp's name resolution.
fn syscall_number(name: &str) -> Result<i64, io::Error> {
    // Architecture-specific syscall numbers
    #[cfg(target_arch = "x86_64")]
    match name {
        // Namespace/mount operations
        "unshare" => Ok(272),
        "setns" => Ok(308),
        "mount" => Ok(165),
        "umount" => Ok(166),
        "umount2" => Ok(166),
        "pivot_root" => Ok(155),
        "chroot" => Ok(161),

        // Process tracing
        "ptrace" => Ok(101),
        "process_vm_readv" => Ok(310),
        "process_vm_writev" => Ok(311),

        // Kernel modules
        "init_module" => Ok(175),
        "finit_module" => Ok(313),
        "delete_module" => Ok(176),

        // System control
        "reboot" => Ok(169),
        "kexec_load" => Ok(246),
        "kexec_file_load" => Ok(320),

        // Time
        "clock_settime" => Ok(227),
        "settimeofday" => Ok(164),
        "clock_adjtime" => Ok(305),
        "adjtimex" => Ok(159),

        // Swap
        "swapon" => Ok(167),
        "swapoff" => Ok(168),

        // Keyring
        "add_key" => Ok(248),
        "request_key" => Ok(249),
        "keyctl" => Ok(250),

        // BPF/perf
        "bpf" => Ok(321),
        "perf_event_open" => Ok(298),

        // NUMA
        "mbind" => Ok(237),
        "set_mempolicy" => Ok(238),
        "migrate_pages" => Ok(256),
        "move_pages" => Ok(279),

        // Quota
        "quotactl" => Ok(179),

        // User/group
        "setuid" => Ok(105),
        "setgid" => Ok(106),

        // Personality
        "personality" => Ok(135),

        // ACCT
        "acct" => Ok(163),

        // Dcookie
        "lookup_dcookie" => Ok(212),

        // File handles
        "name_to_handle_at" => Ok(303),
        "open_by_handle_at" => Ok(304),

        // Userfaultfd
        "userfaultfd" => Ok(323),

        // Minimal profile allowed syscalls
        "exit" => Ok(60),
        "exit_group" => Ok(231),
        "wait4" => Ok(61),
        "waitid" => Ok(247),
        "brk" => Ok(12),
        "mmap" => Ok(9),
        "munmap" => Ok(11),
        "mprotect" => Ok(10),
        "mremap" => Ok(25),
        "read" => Ok(0),
        "write" => Ok(1),
        "readv" => Ok(19),
        "writev" => Ok(20),
        "pread64" => Ok(17),
        "pwrite64" => Ok(18),
        "open" => Ok(2),
        "openat" => Ok(257),
        "close" => Ok(3),
        "lseek" => Ok(8),
        "fstat" => Ok(5),
        "stat" => Ok(4),
        "lstat" => Ok(6),
        "newfstatat" => Ok(262),
        "access" => Ok(21),
        "faccessat" => Ok(269),
        "getcwd" => Ok(79),
        "chdir" => Ok(80),
        "fchdir" => Ok(81),
        "rt_sigaction" => Ok(13),
        "rt_sigprocmask" => Ok(14),
        "rt_sigreturn" => Ok(15),
        "clock_gettime" => Ok(228),
        "gettimeofday" => Ok(96),
        "time" => Ok(201),
        "nanosleep" => Ok(35),
        "getpid" => Ok(39),
        "getuid" => Ok(102),
        "getgid" => Ok(104),
        "geteuid" => Ok(107),
        "getegid" => Ok(108),
        "arch_prctl" => Ok(158),
        "futex" => Ok(202),
        "set_robust_list" => Ok(273),
        "get_robust_list" => Ok(274),

        _ => Err(io::Error::other(format!("Unknown syscall: {}", name))),
    }

    #[cfg(target_arch = "aarch64")]
    match name {
        // Namespace/mount operations
        "unshare" => Ok(97),
        "setns" => Ok(268),
        "mount" => Ok(40),
        "umount" => Ok(39),
        "umount2" => Ok(39),
        "pivot_root" => Ok(41),
        "chroot" => Ok(51),

        // Process tracing
        "ptrace" => Ok(117),
        "process_vm_readv" => Ok(270),
        "process_vm_writev" => Ok(271),

        // Kernel modules
        "init_module" => Ok(105),
        "finit_module" => Ok(273),
        "delete_module" => Ok(106),

        // System control
        "reboot" => Ok(142),
        "kexec_load" => Ok(104),
        "kexec_file_load" => Ok(294),

        // Time
        "clock_settime" => Ok(112),
        "settimeofday" => Ok(170),
        "clock_adjtime" => Ok(266),
        "adjtimex" => Ok(171),

        // Swap
        "swapon" => Ok(224),
        "swapoff" => Ok(225),

        // Keyring
        "add_key" => Ok(217),
        "request_key" => Ok(218),
        "keyctl" => Ok(219),

        // BPF/perf
        "bpf" => Ok(280),
        "perf_event_open" => Ok(241),

        // NUMA
        "mbind" => Ok(235),
        "set_mempolicy" => Ok(237),
        "migrate_pages" => Ok(238),
        "move_pages" => Ok(239),

        // Quota
        "quotactl" => Ok(60),

        // User/group
        "setuid" => Ok(146),
        "setgid" => Ok(144),

        // Personality
        "personality" => Ok(92),

        // ACCT
        "acct" => Ok(89),

        // Dcookie
        "lookup_dcookie" => Ok(18),

        // File handles
        "name_to_handle_at" => Ok(264),
        "open_by_handle_at" => Ok(265),

        // Userfaultfd
        "userfaultfd" => Ok(282),

        // Minimal profile syscalls
        "exit" => Ok(93),
        "exit_group" => Ok(94),
        "wait4" => Ok(260),
        "waitid" => Ok(95),
        "brk" => Ok(214),
        "mmap" => Ok(222),
        "munmap" => Ok(215),
        "mprotect" => Ok(226),
        "mremap" => Ok(216),
        "read" => Ok(63),
        "write" => Ok(64),
        "readv" => Ok(65),
        "writev" => Ok(66),
        "pread64" => Ok(67),
        "pwrite64" => Ok(68),
        "openat" => Ok(56),
        "close" => Ok(57),
        "lseek" => Ok(62),
        "fstat" => Ok(80),
        "newfstatat" => Ok(79),
        "faccessat" => Ok(48),
        "getcwd" => Ok(17),
        "chdir" => Ok(49),
        "fchdir" => Ok(50),
        "rt_sigaction" => Ok(134),
        "rt_sigprocmask" => Ok(135),
        "rt_sigreturn" => Ok(139),
        "clock_gettime" => Ok(113),
        "gettimeofday" => Ok(169),
        "nanosleep" => Ok(101),
        "getpid" => Ok(172),
        "getuid" => Ok(174),
        "getgid" => Ok(176),
        "geteuid" => Ok(175),
        "getegid" => Ok(177),
        "futex" => Ok(98),
        "set_robust_list" => Ok(99),
        "get_robust_list" => Ok(100),

        _ => Err(io::Error::other(format!("Unknown syscall for aarch64: {}", name))),
    }

    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        Err(io::Error::other(format!(
            "Unsupported architecture for syscall {}",
            name
        )))
    }
}

/// Apply a seccomp filter to the current process.
///
/// This should be called in the pre_exec hook, after other setup is complete
/// but before exec. Once applied, the filter cannot be removed.
///
/// # Safety
///
/// This must be called in a signal-safe context (e.g., pre_exec hook).
/// The BPF program is loaded into the kernel and will filter all subsequent
/// syscalls made by this process.
pub fn apply_filter(program: &BpfProgram) -> Result<(), io::Error> {
    // Apply the BPF program using prctl
    // SAFETY: We're in pre_exec context, and seccompiler provides the raw BPF bytecode
    seccompiler::apply_filter(program)
        .map_err(|e| io::Error::other(format!("Failed to apply seccomp filter: {}", e)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_docker_filter_compiles() {
        let result = docker_default_filter();
        assert!(result.is_ok(), "Docker filter should compile successfully");
    }

    #[test]
    fn test_minimal_filter_compiles() {
        let result = minimal_filter();
        assert!(result.is_ok(), "Minimal filter should compile successfully");
    }

    #[test]
    fn test_syscall_numbers_x86_64() {
        #[cfg(target_arch = "x86_64")]
        {
            assert_eq!(syscall_number("read").unwrap(), 0);
            assert_eq!(syscall_number("write").unwrap(), 1);
            assert_eq!(syscall_number("open").unwrap(), 2);
            assert_eq!(syscall_number("close").unwrap(), 3);
            assert_eq!(syscall_number("ptrace").unwrap(), 101);
            assert_eq!(syscall_number("reboot").unwrap(), 169);
        }
    }

    #[test]
    fn test_unknown_syscall() {
        let result = syscall_number("nonexistent_syscall_12345");
        assert!(result.is_err());
    }

    #[test]
    fn test_seccomp_profile_equality() {
        assert_eq!(SeccompProfile::Docker, SeccompProfile::Docker);
        assert_eq!(SeccompProfile::Minimal, SeccompProfile::Minimal);
        assert_eq!(SeccompProfile::None, SeccompProfile::None);
        assert_ne!(SeccompProfile::Docker, SeccompProfile::Minimal);
    }
}
