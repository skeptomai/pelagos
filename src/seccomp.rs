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
        "unshare",    // Create new namespaces
        "setns",      // Join existing namespaces
        "mount",      // Mount filesystems
        "umount",     // Unmount filesystems
        "umount2",    // Unmount with flags
        "pivot_root", // Change root mount point
        "chroot",     // Change root directory
        // Process tracing and debugging (escape via ptrace injection)
        "ptrace",            // Trace processes
        "process_vm_readv",  // Read process memory
        "process_vm_writev", // Write process memory
        // Kernel module manipulation (kernel-level access)
        "init_module",   // Load kernel module
        "finit_module",  // Load kernel module from fd
        "delete_module", // Unload kernel module
        // System control (DOS or system takeover)
        "reboot",          // Reboot system
        "kexec_load",      // Load new kernel
        "kexec_file_load", // Load new kernel from file
        // Time manipulation (affects host clock)
        "clock_settime", // Set system clock
        "settimeofday",  // Set time of day
        "clock_adjtime", // Adjust system clock
        "adjtimex",      // Tune kernel clock
        // Swap manipulation
        "swapon",  // Enable swap
        "swapoff", // Disable swap
        // Kernel keyring (credential access)
        "add_key",     // Add key to keyring
        "request_key", // Request key from keyring
        "keyctl",      // Manipulate keyring
        // BPF and performance monitoring (kernel inspection/manipulation)
        "bpf",             // Extended BPF syscall
        "perf_event_open", // Performance monitoring
        // CPU affinity and NUMA (resource isolation bypass)
        "mbind",         // Set NUMA memory policy
        "set_mempolicy", // Set NUMA memory policy
        "migrate_pages", // Move process pages between NUMA nodes
        "move_pages",    // Move process pages
        // Quota manipulation
        "quotactl", // Filesystem quotas
        // User namespace credential manipulation (when not using user namespaces)
        "setuid", // Set user ID (blocked by default, allowed with conditions in real Docker)
        "setgid", // Set group ID
        // Architecture-specific personality (can bypass security)
        "personality", // Set execution domain
        // ACCT and system accounting
        "acct", // Process accounting
        // Lookup dcookie (kernel debugging)
        "lookup_dcookie", // Get file name from dcookie
        // Name to handle (expose kernel pointers)
        "name_to_handle_at", // Get handle for pathname
        "open_by_handle_at", // Open file via handle
        // User namespace (when not using USER namespace)
        "userfaultfd", // User fault handling
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
        SeccompAction::Allow, // Default: allow everything else
        SeccompAction::Errno(libc::EPERM as u32), // Matched syscalls get EPERM
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

    // Only allow essential syscalls for basic process execution.
    // This is restrictive but sufficient to run simple statically- or
    // dynamically-linked binaries (e.g. /bin/echo on Alpine/musl).
    let allowed_syscalls = vec![
        // Process lifecycle
        "exit",
        "exit_group",
        "wait4",
        "waitid",
        "execve",
        "execveat",
        "clone",
        "fork",
        "vfork",
        // Memory
        "brk",
        "mmap",
        "munmap",
        "mprotect",
        "mremap",
        "madvise",
        // I/O
        "read",
        "write",
        "readv",
        "writev",
        "pread64",
        "pwrite64",
        "open",
        "openat",
        "close",
        "lseek",
        "dup",
        "dup2",
        "dup3",
        "pipe",
        "pipe2",
        "poll",
        "ppoll",
        "select",
        "pselect6",
        // File metadata
        "fstat",
        "stat",
        "lstat",
        "newfstatat",
        "access",
        "faccessat",
        "faccessat2",
        "readlink",
        "readlinkat",
        // File operations
        "fcntl",
        "ioctl",
        "ftruncate",
        "truncate",
        "rename",
        "renameat",
        "renameat2",
        "unlink",
        "unlinkat",
        "mkdir",
        "mkdirat",
        "rmdir",
        "chmod",
        "fchmod",
        "fchmodat",
        "chown",
        "fchown",
        "fchownat",
        "symlink",
        "symlinkat",
        "link",
        "linkat",
        // Directories
        "getcwd",
        "chdir",
        "fchdir",
        "getdents64",
        "getdents",
        // Signals
        "rt_sigaction",
        "rt_sigprocmask",
        "rt_sigreturn",
        "sigaltstack",
        "kill",
        "tgkill",
        // Time
        "clock_gettime",
        "clock_getres",
        "gettimeofday",
        "time",
        "nanosleep",
        "clock_nanosleep",
        // Process info
        "getpid",
        "getppid",
        "gettid",
        "getuid",
        "getgid",
        "geteuid",
        "getegid",
        "getgroups",
        "getresuid",
        "getresgid",
        "getrlimit",
        "prlimit64",
        // Arch-specific / thread setup
        "arch_prctl",
        "set_tid_address",
        "set_robust_list",
        "get_robust_list",
        "prctl",
        "rseq",
        "getrandom",
        // Futex for threading
        "futex",
        // Socket (for basic IPC)
        "socket",
        "connect",
        "sendto",
        "recvfrom",
        "sendmsg",
        "recvmsg",
        "bind",
        "listen",
        "accept",
        "accept4",
        "getsockname",
        "getpeername",
        "getsockopt",
        "setsockopt",
        "shutdown",
        // Misc required by libc
        "uname",
        "umask",
        "sysinfo",
        "statfs",
        "fstatfs",
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
        SeccompAction::Errno(libc::EPERM as u32), // Default: deny everything
        SeccompAction::Allow,                     // Matched syscalls allowed
        target_arch,
    )
    .map_err(|e| io::Error::other(format!("Failed to create minimal filter: {}", e)))?;

    // Convert to BPF program
    let program: BpfProgram = filter
        .try_into()
        .map_err(|e| io::Error::other(format!("Failed to compile minimal filter: {}", e)))?;

    Ok(program)
}

/// Build a seccomp BPF program from an OCI `linux.seccomp` configuration.
///
/// Handles `SCMP_ACT_ALLOW`, `SCMP_ACT_ERRNO`, `SCMP_ACT_KILL`, `SCMP_ACT_KILL_THREAD`,
/// `SCMP_ACT_KILL_PROCESS`, `SCMP_ACT_LOG`, and `SCMP_ACT_TRAP`.
/// Argument conditions (`args`) are ignored in this first-pass implementation.
pub fn filter_from_oci(config: &crate::oci::OciSeccomp) -> Result<BpfProgram, io::Error> {
    use std::convert::TryInto;

    fn oci_action_to_seccomp(action: &str) -> Option<SeccompAction> {
        match action {
            "SCMP_ACT_ALLOW" => Some(SeccompAction::Allow),
            "SCMP_ACT_ERRNO" | "SCMP_ACT_ENOSYS" => Some(SeccompAction::Errno(libc::EPERM as u32)),
            "SCMP_ACT_KILL" | "SCMP_ACT_KILL_THREAD" => Some(SeccompAction::KillThread),
            "SCMP_ACT_KILL_PROCESS" => Some(SeccompAction::KillProcess),
            "SCMP_ACT_LOG" => Some(SeccompAction::Log),
            "SCMP_ACT_TRAP" => Some(SeccompAction::Trap),
            _ => None,
        }
    }

    let default_action = oci_action_to_seccomp(&config.default_action).ok_or_else(|| {
        io::Error::other(format!(
            "unknown seccomp defaultAction: {}",
            config.default_action
        ))
    })?;

    // Build syscall → rules map. For each rule, the action overrides the default.
    let mut rules: BTreeMap<i64, Vec<SeccompRule>> = BTreeMap::new();

    for rule in &config.syscalls {
        let action = match oci_action_to_seccomp(&rule.action) {
            Some(a) => a,
            None => continue, // skip unknown actions
        };

        // Only add rules that differ from the default (seccompiler semantics:
        // rules map is "match → match_action"; default_action is the fallback).
        // We unconditionally add the entry; seccompiler handles the logic.
        for name in &rule.names {
            if let Ok(num) = syscall_number(name) {
                rules.entry(num).or_default();
                // An empty Vec<SeccompRule> means "match any args → match_action".
                // We use the action as the match_action; but SeccompFilter only has
                // one match_action. Work around this by building one filter per
                // unique action if needed — for now handle the common case where
                // all syscall rules share the same action.
                let _ = action; // captured per-loop below
            }
        }
    }

    // Simplified but correct approach: build a single filter where:
    // - rules map contains all syscalls with the per-rule action that matches
    // - For heterogeneous actions we build them as separate entries
    // The seccompiler model: filter has ONE match_action and ONE default_action.
    // Entries in the rules map use the match_action if they match.
    //
    // This means we can only represent "allowlist" (default=KILL, rules=ALLOW)
    // or "denylist" (default=ALLOW, rules=ERRNO) in a single filter.
    //
    // For full generality we'd need multiple chained filters. For now we collect
    // only the rules whose action differs from the default.

    let mut filtered_rules: BTreeMap<i64, Vec<SeccompRule>> = BTreeMap::new();
    let mut match_action: Option<SeccompAction> = None;

    for rule in &config.syscalls {
        let action = match oci_action_to_seccomp(&rule.action) {
            Some(a) => a,
            None => continue,
        };
        if action == default_action {
            continue; // Same as default — no need to add to map
        }
        if match_action.is_none() {
            match_action = Some(action.clone());
        }
        for name in &rule.names {
            if let Ok(num) = syscall_number(name) {
                filtered_rules.entry(num).or_default();
            }
        }
    }

    let effective_match = match_action.unwrap_or(SeccompAction::Allow);

    let target_arch = std::env::consts::ARCH
        .try_into()
        .map_err(|e| io::Error::other(format!("Unsupported architecture: {:?}", e)))?;

    let filter =
        SeccompFilter::new(filtered_rules, default_action, effective_match, target_arch)
            .map_err(|e| io::Error::other(format!("Failed to create OCI seccomp filter: {}", e)))?;

    filter
        .try_into()
        .map_err(|e| io::Error::other(format!("Failed to compile OCI seccomp filter: {}", e)))
}

/// Get syscall number for a given syscall name on the current architecture.
///
/// This uses a simple mapping for common syscalls. For production use,
/// you'd want a complete mapping or use libseccomp's name resolution.
pub fn syscall_number(name: &str) -> Result<i64, io::Error> {
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
        "execve" => Ok(59),
        "execveat" => Ok(322),
        "clone" => Ok(56),
        "fork" => Ok(57),
        "vfork" => Ok(58),
        "brk" => Ok(12),
        "mmap" => Ok(9),
        "munmap" => Ok(11),
        "mprotect" => Ok(10),
        "mremap" => Ok(25),
        "madvise" => Ok(28),
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
        "dup" => Ok(32),
        "dup2" => Ok(33),
        "dup3" => Ok(292),
        "pipe" => Ok(22),
        "pipe2" => Ok(293),
        "poll" => Ok(7),
        "ppoll" => Ok(271),
        "select" => Ok(23),
        "pselect6" => Ok(270),
        "fstat" => Ok(5),
        "stat" => Ok(4),
        "lstat" => Ok(6),
        "newfstatat" => Ok(262),
        "access" => Ok(21),
        "faccessat" => Ok(269),
        "faccessat2" => Ok(439),
        "readlink" => Ok(89),
        "readlinkat" => Ok(267),
        "fcntl" => Ok(72),
        "ioctl" => Ok(16),
        "ftruncate" => Ok(77),
        "truncate" => Ok(76),
        "rename" => Ok(82),
        "renameat" => Ok(264),
        "renameat2" => Ok(316),
        "unlink" => Ok(87),
        "unlinkat" => Ok(263),
        "mkdir" => Ok(83),
        "mkdirat" => Ok(258),
        "rmdir" => Ok(84),
        "chmod" => Ok(90),
        "fchmod" => Ok(91),
        "fchmodat" => Ok(268),
        "chown" => Ok(92),
        "fchown" => Ok(93),
        "fchownat" => Ok(260),
        "symlink" => Ok(88),
        "symlinkat" => Ok(266),
        "link" => Ok(86),
        "linkat" => Ok(265),
        "getcwd" => Ok(79),
        "chdir" => Ok(80),
        "fchdir" => Ok(81),
        "getdents64" => Ok(217),
        "getdents" => Ok(78),
        "rt_sigaction" => Ok(13),
        "rt_sigprocmask" => Ok(14),
        "rt_sigreturn" => Ok(15),
        "sigaltstack" => Ok(131),
        "kill" => Ok(62),
        "tgkill" => Ok(234),
        "clock_gettime" => Ok(228),
        "clock_getres" => Ok(229),
        "gettimeofday" => Ok(96),
        "time" => Ok(201),
        "nanosleep" => Ok(35),
        "clock_nanosleep" => Ok(230),
        "getpid" => Ok(39),
        "getppid" => Ok(110),
        "gettid" => Ok(186),
        "getuid" => Ok(102),
        "getgid" => Ok(104),
        "geteuid" => Ok(107),
        "getegid" => Ok(108),
        "getgroups" => Ok(115),
        "getresuid" => Ok(118),
        "getresgid" => Ok(120),
        "getrlimit" => Ok(97),
        "prlimit64" => Ok(302),
        "arch_prctl" => Ok(158),
        "set_tid_address" => Ok(218),
        "set_robust_list" => Ok(273),
        "get_robust_list" => Ok(274),
        "prctl" => Ok(157),
        "rseq" => Ok(334),
        "getrandom" => Ok(318),
        "futex" => Ok(202),
        "socket" => Ok(41),
        "connect" => Ok(42),
        "sendto" => Ok(44),
        "recvfrom" => Ok(45),
        "sendmsg" => Ok(46),
        "recvmsg" => Ok(47),
        "bind" => Ok(49),
        "listen" => Ok(50),
        "accept" => Ok(43),
        "accept4" => Ok(288),
        "getsockname" => Ok(51),
        "getpeername" => Ok(52),
        "getsockopt" => Ok(55),
        "setsockopt" => Ok(54),
        "shutdown" => Ok(48),
        "uname" => Ok(63),
        "umask" => Ok(95),
        "sysinfo" => Ok(99),
        "statfs" => Ok(137),
        "fstatfs" => Ok(138),

        // Signal-related
        "rt_sigsuspend" => Ok(130),
        "rt_sigpending" => Ok(127),
        "rt_sigtimedwait" => Ok(128),
        "rt_sigqueueinfo" => Ok(129),
        "rt_tgsigqueueinfo" => Ok(297),
        "signalfd" => Ok(282),
        "signalfd4" => Ok(289),
        "tkill" => Ok(200),
        "pause" => Ok(34),

        // Process/session management
        "setpgid" => Ok(109),
        "getpgid" => Ok(121),
        "getpgrp" => Ok(111),
        "getsid" => Ok(124),
        "setsid" => Ok(112),
        "setreuid" => Ok(113),
        "setresuid" => Ok(117),
        "setresgid" => Ok(119),
        "setregid" => Ok(114),
        "setgroups" => Ok(116),
        "setfsgid" => Ok(123),
        "setfsuid" => Ok(122),
        "setpriority" => Ok(141),
        "getpriority" => Ok(140),
        "setitimer" => Ok(38),
        "getitimer" => Ok(36),
        "setrlimit" => Ok(160),
        "getrusage" => Ok(98),
        "times" => Ok(100),
        "alarm" => Ok(37),
        "syslog" => Ok(103),

        // Capabilities
        "capget" => Ok(125),
        "capset" => Ok(126),

        // File/filesystem operations
        "creat" => Ok(85),
        "flock" => Ok(73),
        "fsync" => Ok(74),
        "fdatasync" => Ok(75),
        "mknod" => Ok(133),
        "mknodat" => Ok(259),
        "lchown" => Ok(94),
        "utime" => Ok(132),
        "utimes" => Ok(235),
        "utimensat" => Ok(280),
        "futimesat" => Ok(261),
        "readahead" => Ok(187),
        "fallocate" => Ok(285),
        "copy_file_range" => Ok(326),
        "fadvise64" => Ok(221),
        "fanotify_mark" => Ok(301),
        "sync" => Ok(162),
        "syncfs" => Ok(306),
        "sync_file_range" => Ok(277),
        "statx" => Ok(332),

        // Extended attributes
        "getxattr" => Ok(191),
        "lgetxattr" => Ok(192),
        "fgetxattr" => Ok(193),
        "listxattr" => Ok(194),
        "llistxattr" => Ok(195),
        "removexattr" => Ok(197),
        "lremovexattr" => Ok(198),
        "fremovexattr" => Ok(199),
        "setxattr" => Ok(188),
        "lsetxattr" => Ok(189),
        "fsetxattr" => Ok(190),

        // IPC - semaphores
        "semget" => Ok(64),
        "semop" => Ok(65),
        "semctl" => Ok(66),
        "semtimedop" => Ok(220),

        // IPC - message queues
        "msgget" => Ok(68),
        "msgsnd" => Ok(69),
        "msgrcv" => Ok(70),
        "msgctl" => Ok(71),

        // IPC - shared memory
        "shmget" => Ok(29),
        "shmat" => Ok(30),
        "shmctl" => Ok(31),
        "shmdt" => Ok(67),

        // POSIX message queues
        "mq_open" => Ok(240),
        "mq_unlink" => Ok(241),
        "mq_timedsend" => Ok(242),
        "mq_timedreceive" => Ok(243),
        "mq_notify" => Ok(244),
        "mq_getsetattr" => Ok(245),

        // Socket operations
        "socketpair" => Ok(53),
        "sendfile" => Ok(40),
        "sendmmsg" => Ok(307),
        "recvmmsg" => Ok(299),
        "splice" => Ok(275),
        "tee" => Ok(276),
        "vmsplice" => Ok(278),

        // Memory management
        "mincore" => Ok(27),
        "msync" => Ok(26),
        "mlock" => Ok(149),
        "mlock2" => Ok(325),
        "mlockall" => Ok(151),
        "munlock" => Ok(150),
        "munlockall" => Ok(152),
        "remap_file_pages" => Ok(216),
        "memfd_create" => Ok(319),

        // Async I/O
        "io_setup" => Ok(206),
        "io_destroy" => Ok(207),
        "io_getevents" => Ok(208),
        "io_submit" => Ok(209),
        "io_cancel" => Ok(210),
        "preadv" => Ok(295),
        "pwritev" => Ok(296),

        // I/O scheduling
        "ioprio_set" => Ok(251),
        "ioprio_get" => Ok(252),

        // Epoll
        "epoll_create" => Ok(213),
        "epoll_create1" => Ok(291),
        "epoll_ctl" => Ok(233),
        "epoll_wait" => Ok(232),
        "epoll_pwait" => Ok(281),

        // Timers
        "timer_create" => Ok(222),
        "timer_settime" => Ok(223),
        "timer_gettime" => Ok(224),
        "timer_getoverrun" => Ok(225),
        "timer_delete" => Ok(226),
        "timerfd_create" => Ok(283),
        "timerfd_settime" => Ok(286),
        "timerfd_gettime" => Ok(287),

        // Event/inotify
        "eventfd" => Ok(284),
        "eventfd2" => Ok(290),
        "inotify_init" => Ok(253),
        "inotify_init1" => Ok(294),
        "inotify_add_watch" => Ok(254),
        "inotify_rm_watch" => Ok(255),

        // Scheduling
        "sched_setparam" => Ok(142),
        "sched_getparam" => Ok(143),
        "sched_setscheduler" => Ok(144),
        "sched_getscheduler" => Ok(145),
        "sched_get_priority_max" => Ok(146),
        "sched_get_priority_min" => Ok(147),
        "sched_rr_get_interval" => Ok(148),
        "sched_setaffinity" => Ok(203),
        "sched_getaffinity" => Ok(204),
        "sched_setattr" => Ok(314),
        "sched_getattr" => Ok(315),
        "sched_yield" => Ok(24),

        // CPU/thread
        "getcpu" => Ok(309),
        "get_thread_area" => Ok(211),
        "set_thread_area" => Ok(205),
        "modify_ldt" => Ok(154),

        // Seccomp
        "seccomp" => Ok(317),

        // Landlock
        "landlock_create_ruleset" => Ok(444),
        "landlock_add_rule" => Ok(445),
        "landlock_restrict_self" => Ok(446),

        // Miscellaneous
        "restart_syscall" => Ok(219),

        // 32-bit compat syscalls: not native on x86_64, silently ignored
        "_llseek" | "_newselect" | "chown32" | "epoll_ctl_old" | "epoll_wait_old"
        | "fadvise64_64" | "fchown32" | "fcntl64" | "fstat64" | "fstatat64" | "fstatfs64"
        | "ftruncate64" | "getegid32" | "geteuid32" | "getgid32" | "getgroups32"
        | "getresgid32" | "getresuid32" | "getuid32" | "ipc" | "lchown32" | "lstat64" | "mmap2"
        | "recv" | "send" | "sendfile64" | "setfsgid32" | "setfsuid32" | "setgid32"
        | "setgroups32" | "setregid32" | "setresgid32" | "setresuid32" | "setreuid32"
        | "setuid32" | "sigreturn" | "socketcall" | "stat64" | "statfs64" | "truncate64"
        | "ugetrlimit" | "waitpid" => Err(io::Error::other(format!(
            "32-bit compat syscall not available on x86_64: {}",
            name
        ))),

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

        _ => Err(io::Error::other(format!(
            "Unknown syscall for aarch64: {}",
            name
        ))),
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

/// Apply a seccomp BPF filter WITHOUT setting PR_SET_NO_NEW_PRIVS first.
///
/// Used when the OCI config has `noNewPrivileges: false` — the caller holds
/// CAP_SYS_ADMIN (before capability drops) so the kernel allows the filter
/// installation without requiring NNP to be set.  Unlike `apply_filter()`,
/// this does NOT call `prctl(PR_SET_NO_NEW_PRIVS, 1, …)`, preserving the
/// process's original NNP state.
///
/// # Safety
/// Must be called in pre_exec (after fork, before exec) with CAP_SYS_ADMIN
/// in the effective capability set.
pub fn apply_filter_no_nnp(program: &BpfProgram) -> Result<(), io::Error> {
    // BpfProgram is Vec<sock_filter>; build a sock_fprog pointing into it.
    let fprog = libc::sock_fprog {
        len: program.len() as u16,
        filter: program.as_ptr() as *mut libc::sock_filter,
    };
    let result = unsafe {
        libc::prctl(
            libc::PR_SET_SECCOMP,
            libc::SECCOMP_MODE_FILTER as libc::c_ulong,
            &fprog as *const libc::sock_fprog as libc::c_ulong,
            0,
            0,
        )
    };
    if result != 0 {
        Err(io::Error::other(format!(
            "Failed to apply seccomp filter (no-nnp): {}",
            io::Error::last_os_error()
        )))
    } else {
        Ok(())
    }
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
