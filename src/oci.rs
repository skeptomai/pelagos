//! OCI Runtime Specification v1.0.2 implementation.
//!
//! Implements the five lifecycle subcommands (create, start, state, kill, delete)
//! and config.json parsing for OCI bundle compatibility.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

// ---------------------------------------------------------------------------
// config.json types (first-pass — required fields only)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OciConfig {
    pub oci_version: String,
    pub root: OciRoot,
    pub process: Option<OciProcess>,
    pub hostname: Option<String>,
    pub linux: Option<OciLinux>,
    #[serde(default)]
    pub mounts: Vec<OciMount>,
    pub hooks: Option<OciHooks>,
    pub annotations: Option<HashMap<String, String>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OciRoot {
    pub path: String,
    #[serde(default)]
    pub readonly: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OciProcess {
    pub args: Vec<String>,
    pub cwd: String,
    #[serde(default)]
    pub env: Vec<String>,
    pub user: Option<OciUser>,
    #[serde(default)]
    pub no_new_privileges: bool,
    #[serde(default)]
    pub terminal: bool,
    pub capabilities: Option<OciCapabilities>,
    #[serde(default)]
    pub rlimits: Vec<OciRlimit>,
    pub oom_score_adj: Option<i32>,
}

/// OCI rlimit entry from `process.rlimits`.
#[derive(Debug, Deserialize)]
pub struct OciRlimit {
    #[serde(rename = "type")]
    pub type_: String,
    pub hard: u64,
    pub soft: u64,
}

/// OCI capability sets — each is a list of capability names like "CAP_CHOWN".
#[derive(Debug, Deserialize, Default)]
pub struct OciCapabilities {
    #[serde(default)]
    pub bounding: Vec<String>,
    #[serde(default)]
    pub effective: Vec<String>,
    #[serde(default)]
    pub inheritable: Vec<String>,
    #[serde(default)]
    pub permitted: Vec<String>,
    #[serde(default)]
    pub ambient: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OciUser {
    #[serde(default)]
    pub uid: u32,
    #[serde(default)]
    pub gid: u32,
    #[serde(default)]
    pub additional_gids: Vec<u32>,
    pub umask: Option<u32>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OciLinux {
    #[serde(default)]
    pub namespaces: Vec<OciNamespace>,
    #[serde(default)]
    pub uid_mappings: Vec<OciIdMapping>,
    #[serde(default)]
    pub gid_mappings: Vec<OciIdMapping>,
    #[serde(default)]
    pub masked_paths: Vec<String>,
    #[serde(default)]
    pub readonly_paths: Vec<String>,
    pub resources: Option<OciResources>,
    #[serde(default)]
    pub sysctl: HashMap<String, String>,
    #[serde(default)]
    pub devices: Vec<OciDevice>,
    pub seccomp: Option<OciSeccomp>,
    /// Mount propagation of the rootfs: "shared" | "slave" | "private" | "unbindable".
    /// Defaults to "rprivate" (remora makes all mounts private by default).
    pub rootfs_propagation: Option<String>,
    /// Absolute path for the container's cgroup (e.g. "/myapp/container1").
    /// If absent, remora auto-generates a path.
    pub cgroups_path: Option<String>,
}

// ---------------------------------------------------------------------------
// linux.resources
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct OciResources {
    pub memory: Option<OciMemoryResources>,
    pub cpu: Option<OciCpuResources>,
    pub pids: Option<OciPidsResources>,
    pub block_io: Option<OciBlockIOResources>,
    pub network: Option<OciNetworkResources>,
    #[serde(default)]
    pub devices: Vec<OciDeviceCgroup>,
    #[serde(default)]
    pub hugepage_limits: Vec<OciHugepageLimit>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct OciMemoryResources {
    /// Hard memory limit in bytes (`memory.max`).
    pub limit: Option<i64>,
    /// Memory + swap limit in bytes (`memory.swap.max` on v2, `memory.memsw.limit_in_bytes` on v1).
    /// -1 means unlimited swap.
    pub swap: Option<i64>,
    /// Soft memory limit / low-water mark (`memory.low` on v2, `memory.soft_limit_in_bytes` on v1).
    pub reservation: Option<i64>,
    /// Kernel memory limit in bytes (v1 only; ignored on v2).
    pub kernel: Option<i64>,
    /// Kernel TCP buffer memory limit in bytes (v1 only; ignored on v2).
    pub kernel_tcp: Option<i64>,
    /// Swappiness hint (0–100) for the memory controller (v1 only; ignored on v2).
    pub swappiness: Option<u64>,
    /// Disable OOM killer for the cgroup.
    #[serde(default)]
    pub disable_oom_killer: bool,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct OciCpuResources {
    /// CPU weight / shares (`cpu.weight` on v2, `cpu.shares` on v1).
    pub shares: Option<u64>,
    /// CPU quota in microseconds per period (`cpu.max` on v2).
    pub quota: Option<i64>,
    /// CPU period in microseconds.
    pub period: Option<u64>,
    /// Realtime CPU runtime in microseconds (v1 only; ignored on v2).
    pub realtime_runtime: Option<i64>,
    /// Realtime CPU period in microseconds (v1 only; ignored on v2).
    pub realtime_period: Option<u64>,
    /// CPUs allowed for this cgroup (cpuset string, e.g. "0-3,6").
    pub cpus: Option<String>,
    /// Memory nodes allowed for this cgroup (cpuset string, e.g. "0-1").
    pub mems: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct OciPidsResources {
    /// Maximum number of pids in the cgroup (`pids.max`).
    pub limit: Option<i64>,
}

/// linux.resources.blockIO
#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct OciBlockIOResources {
    /// Overall block I/O weight (10–1000).
    pub weight: Option<u16>,
    /// Leaf-node weight (v1 only).
    pub leaf_weight: Option<u16>,
    /// Per-device weight overrides.
    #[serde(default)]
    pub weight_device: Vec<OciWeightDevice>,
    /// Per-device read BPS throttle.
    #[serde(default)]
    pub throttle_read_bps_device: Vec<OciThrottleDevice>,
    /// Per-device write BPS throttle.
    #[serde(default)]
    pub throttle_write_bps_device: Vec<OciThrottleDevice>,
    /// Per-device read IOPS throttle.
    #[serde(default)]
    pub throttle_read_iops_device: Vec<OciThrottleDevice>,
    /// Per-device write IOPS throttle.
    #[serde(default)]
    pub throttle_write_iops_device: Vec<OciThrottleDevice>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OciWeightDevice {
    pub major: u64,
    pub minor: u64,
    pub weight: Option<u16>,
    pub leaf_weight: Option<u16>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OciThrottleDevice {
    pub major: u64,
    pub minor: u64,
    pub rate: u64,
}

/// linux.resources.network
#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct OciNetworkResources {
    /// net_cls classid (v1 only; ignored on v2).
    pub class_id: Option<u32>,
    /// net_prio interface priorities (v1 only; ignored on v2).
    #[serde(default)]
    pub priorities: Vec<OciNetPriority>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OciNetPriority {
    pub name: String,
    pub priority: u32,
}

/// A single entry in linux.resources.devices (device cgroup allow/deny rules).
/// Note: distinct from linux.devices (actual device node creation).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OciDeviceCgroup {
    pub allow: bool,
    #[serde(rename = "type", default)]
    pub kind: String,
    pub major: Option<i64>,
    pub minor: Option<i64>,
    #[serde(default)]
    pub access: String,
}

/// linux.resources.hugepageLimits entry.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OciHugepageLimit {
    pub page_size: String,
    pub limit: u64,
}

// ---------------------------------------------------------------------------
// linux.devices
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OciDevice {
    pub path: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub major: Option<u64>,
    pub minor: Option<u64>,
    #[serde(default = "default_file_mode")]
    pub file_mode: u32,
    #[serde(default)]
    pub uid: u32,
    #[serde(default)]
    pub gid: u32,
}

fn default_file_mode() -> u32 {
    0o666
}

// ---------------------------------------------------------------------------
// linux.seccomp
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OciSeccomp {
    pub default_action: String,
    #[serde(default)]
    pub architectures: Vec<String>,
    #[serde(default)]
    pub syscalls: Vec<OciSyscallRule>,
}

#[derive(Debug, Deserialize)]
pub struct OciSyscallRule {
    #[serde(default)]
    pub names: Vec<String>,
    pub action: String,
    #[serde(default)]
    pub args: Vec<OciSyscallArg>,
}

#[derive(Debug, Deserialize)]
pub struct OciSyscallArg {
    pub index: u32,
    pub value: u64,
    pub op: String,
}

// ---------------------------------------------------------------------------
// hooks
// ---------------------------------------------------------------------------

/// OCI lifecycle hooks. Run in the host namespace.
#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct OciHooks {
    #[serde(default)]
    pub prestart: Vec<OciHook>,
    #[serde(default)]
    pub create_runtime: Vec<OciHook>,
    #[serde(default)]
    pub create_container: Vec<OciHook>,
    #[serde(default)]
    pub start_container: Vec<OciHook>,
    #[serde(default)]
    pub poststart: Vec<OciHook>,
    #[serde(default)]
    pub poststop: Vec<OciHook>,
}

#[derive(Debug, Deserialize)]
pub struct OciHook {
    pub path: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: Vec<String>,
    pub timeout: Option<u32>,
}

#[derive(Debug, Deserialize)]
pub struct OciNamespace {
    #[serde(rename = "type")]
    pub ns_type: String,
    pub path: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct OciIdMapping {
    #[serde(rename = "hostID")]
    pub host_id: u32,
    #[serde(rename = "containerID")]
    pub container_id: u32,
    pub size: u32,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OciMount {
    pub destination: String,
    #[serde(rename = "type")]
    pub mount_type: Option<String>,
    pub source: Option<String>,
    #[serde(default)]
    pub options: Vec<String>,
}

// ---------------------------------------------------------------------------
// State types
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OciState {
    pub oci_version: String,
    pub id: String,
    pub status: String,
    pub pid: i32,
    pub bundle: String,
    /// Annotations from config.json, echoed back per the OCI Runtime Spec.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annotations: Option<std::collections::HashMap<String, String>>,
    /// Bridge IP address, populated when using bridge networking.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bridge_ip: Option<String>,
    /// Process start time in jiffies from /proc/<pid>/stat field 22.
    ///
    /// Stored at create time and compared at state/kill time to detect PID reuse.
    /// If the current starttime differs from this value, the original container
    /// process has exited and `state.pid` now belongs to an unrelated process.
    /// See issue #44 for the longer-term pidfd-based approach.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid_start_time: Option<u64>,
}

// ---------------------------------------------------------------------------
// Path helpers
// ---------------------------------------------------------------------------

pub fn state_dir(id: &str) -> PathBuf {
    crate::paths::oci_state_dir(id)
}

pub fn state_path(id: &str) -> PathBuf {
    state_dir(id).join("state.json")
}

pub fn exec_sock_path(id: &str) -> PathBuf {
    state_dir(id).join("exec.sock")
}

// ---------------------------------------------------------------------------
// State I/O
// ---------------------------------------------------------------------------

pub fn read_state(id: &str) -> io::Result<OciState> {
    let content = fs::read(state_path(id))?;
    serde_json::from_slice(&content).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

pub fn write_state(id: &str, state: &OciState) -> io::Result<()> {
    let content = serde_json::to_vec_pretty(state).map_err(io::Error::other)?;
    fs::write(state_path(id), content)
}

// ---------------------------------------------------------------------------
// Config loading
// ---------------------------------------------------------------------------

pub fn config_from_bundle(bundle: &Path) -> io::Result<OciConfig> {
    let config_path = bundle.join("config.json");
    let content = fs::read(&config_path)?;
    serde_json::from_slice(&content).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

// ---------------------------------------------------------------------------
// Capability name → Remora Capability flag
// ---------------------------------------------------------------------------

/// Convert an OCI capability name (e.g. "CAP_CHOWN") to the corresponding
/// Remora `Capability` bitflag. Returns `None` for unknown names.
fn oci_cap_to_flag(name: &str) -> Option<crate::container::Capability> {
    use crate::container::Capability;
    // Strip optional "CAP_" prefix — OCI bundles may include it or omit it.
    let n = name.strip_prefix("CAP_").unwrap_or(name);
    match n {
        "CHOWN" => Some(Capability::CHOWN),
        "DAC_OVERRIDE" => Some(Capability::DAC_OVERRIDE),
        "DAC_READ_SEARCH" => Some(Capability::DAC_READ_SEARCH),
        "FOWNER" => Some(Capability::FOWNER),
        "FSETID" => Some(Capability::FSETID),
        "KILL" => Some(Capability::KILL),
        "SETGID" => Some(Capability::SETGID),
        "SETUID" => Some(Capability::SETUID),
        "SETPCAP" => Some(Capability::SETPCAP),
        "LINUX_IMMUTABLE" => Some(Capability::LINUX_IMMUTABLE),
        "NET_BIND_SERVICE" => Some(Capability::NET_BIND_SERVICE),
        "NET_BROADCAST" => Some(Capability::NET_BROADCAST),
        "NET_ADMIN" => Some(Capability::NET_ADMIN),
        "NET_RAW" => Some(Capability::NET_RAW),
        "IPC_LOCK" => Some(Capability::IPC_LOCK),
        "IPC_OWNER" => Some(Capability::IPC_OWNER),
        "SYS_MODULE" => Some(Capability::SYS_MODULE),
        "SYS_RAWIO" => Some(Capability::SYS_RAWIO),
        "SYS_CHROOT" => Some(Capability::SYS_CHROOT),
        "SYS_PTRACE" => Some(Capability::SYS_PTRACE),
        "SYS_PACCT" => Some(Capability::SYS_PACCT),
        "SYS_ADMIN" => Some(Capability::SYS_ADMIN),
        "SYS_BOOT" => Some(Capability::SYS_BOOT),
        "SYS_NICE" => Some(Capability::SYS_NICE),
        "SYS_RESOURCE" => Some(Capability::SYS_RESOURCE),
        "SYS_TIME" => Some(Capability::SYS_TIME),
        "SYS_TTY_CONFIG" => Some(Capability::SYS_TTY_CONFIG),
        "MKNOD" => Some(Capability::MKNOD),
        "LEASE" => Some(Capability::LEASE),
        "AUDIT_WRITE" => Some(Capability::AUDIT_WRITE),
        "AUDIT_CONTROL" => Some(Capability::AUDIT_CONTROL),
        "SETFCAP" => Some(Capability::SETFCAP),
        "MAC_OVERRIDE" => Some(Capability::MAC_OVERRIDE),
        "MAC_ADMIN" => Some(Capability::MAC_ADMIN),
        "SYSLOG" => Some(Capability::SYSLOG),
        "WAKE_ALARM" => Some(Capability::WAKE_ALARM),
        "BLOCK_SUSPEND" => Some(Capability::BLOCK_SUSPEND),
        "AUDIT_READ" => Some(Capability::AUDIT_READ),
        "PERFMON" => Some(Capability::PERFMON),
        "BPF" => Some(Capability::BPF),
        "CHECKPOINT_RESTORE" => Some(Capability::CHECKPOINT_RESTORE),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Build a container::Command from OCI config
// ---------------------------------------------------------------------------

/// Validate that the ociVersion string is a recognized spec version (1.x.y).
/// Rejects obviously invalid strings like "invalid" or "0.1".
fn is_supported_oci_version(version: &str) -> bool {
    // Accept any 1.x.y version — the OCI spec has been at major version 1 since 1.0.0.
    // Reject anything that doesn't start with "1." to catch typos and test injections.
    let parts: Vec<&str> = version.splitn(3, '.').collect();
    if parts.len() < 2 {
        return false;
    }
    parts[0] == "1" && parts[1].chars().all(|c| c.is_ascii_digit())
}

/// Validate that the symlink at `path` refers to a namespace of type `ns_type`.
/// Returns an error if the path doesn't exist or the type doesn't match.
fn validate_ns_path_type(path: &str, ns_type: &str) -> io::Result<()> {
    let target = std::fs::read_link(path).map_err(|e| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!("namespace path '{}': {}", path, e),
        )
    })?;
    let target_str = target.to_string_lossy();
    // Symlink targets look like "ipc:[4026531839]" or "mnt:[...]".
    // The OCI type name "mount" maps to the kernel name "mnt"; "network" → "net".
    let expected_prefix = match ns_type {
        "mount" => "mnt",
        "network" => "net",
        other => other,
    };
    let prefix = format!("{}:[", expected_prefix);
    if !target_str.starts_with(&prefix) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "namespace path '{}' has type '{}' (expected '{}')",
                path, target_str, ns_type
            ),
        ));
    }
    Ok(())
}

pub fn build_command(config: &OciConfig, bundle: &Path) -> io::Result<crate::container::Command> {
    use crate::container::{Command, Namespace};

    // Validate ociVersion: must look like a supported spec version (1.x.y).
    if !is_supported_oci_version(&config.oci_version) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "unsupported ociVersion '{}' — expected 1.x.y",
                config.oci_version
            ),
        ));
    }

    let root_path = bundle.join(&config.root.path);

    // process is optional in OCI spec; when absent use a no-op placeholder.
    let process = config.process.as_ref();
    let exe: &str = process
        .and_then(|p| p.args.first().map(|s| s.as_str()))
        .unwrap_or("/bin/true");
    let cwd: &str = process.map(|p| p.cwd.as_str()).unwrap_or("/");

    let mut cmd = Command::new(exe)
        .env_clear()
        .with_chroot(&root_path)
        .with_cwd(cwd)
        .stdout(crate::container::Stdio::Inherit)
        .stderr(crate::container::Stdio::Inherit);

    if let Some(p) = process {
        // Remaining args (exe is args[0])
        if p.args.len() > 1 {
            let rest: Vec<&str> = p.args[1..].iter().map(|s| s.as_str()).collect();
            cmd = cmd.args(&rest);
        }

        // Environment
        for entry in &p.env {
            if let Some(eq) = entry.find('=') {
                cmd = cmd.env(&entry[..eq], &entry[eq + 1..]);
            } else {
                cmd = cmd.env(entry, "");
            }
        }

        // User (uid/gid/supplementary groups/umask)
        if let Some(ref user) = p.user {
            cmd = cmd.with_uid(user.uid).with_gid(user.gid);
            if !user.additional_gids.is_empty() {
                cmd = cmd.with_additional_gids(&user.additional_gids);
            }
            if let Some(mask) = user.umask {
                cmd = cmd.with_umask(mask);
            }
        }

        // Security flags
        cmd = cmd.with_no_new_privileges(p.no_new_privileges);

        // OOM score adjustment
        if let Some(score) = p.oom_score_adj {
            cmd = cmd.with_oom_score_adj(score);
        }
    }

    // Hostname
    if let Some(ref h) = config.hostname {
        cmd = cmd.with_hostname(h);
    }

    if config.root.readonly {
        cmd = cmd.with_readonly_rootfs(true);
    }

    // Linux namespaces + UID/GID mappings
    if let Some(ref linux) = config.linux {
        let mut ns_flags = Namespace::empty();
        for ns in &linux.namespaces {
            let flag = match ns.ns_type.as_str() {
                "mount" => Some(Namespace::MOUNT),
                "uts" => Some(Namespace::UTS),
                "ipc" => Some(Namespace::IPC),
                "user" => Some(Namespace::USER),
                "pid" => Some(Namespace::PID),
                "network" => Some(Namespace::NET),
                "cgroup" => Some(Namespace::CGROUP),
                _ => None,
            };
            if let Some(flag) = flag {
                if let Some(ref path) = ns.path {
                    // Validate that the path's namespace type matches the configured type.
                    validate_ns_path_type(path, &ns.ns_type)?;
                    // Join an existing namespace by path
                    cmd = cmd.with_namespace_join(path, flag);
                } else {
                    ns_flags |= flag;
                }
            }
        }
        if !ns_flags.is_empty() {
            cmd = cmd.with_namespaces(ns_flags);
        }

        // Mount proc automatically when a mount namespace is requested,
        // unless the OCI config already supplies an explicit "proc" type mount
        // (adding both would cause a double-mount and a pre_exec failure).
        let has_mount_ns = linux
            .namespaces
            .iter()
            .any(|n| n.ns_type == "mount" && n.path.is_none());
        let has_explicit_proc = config
            .mounts
            .iter()
            .any(|m| m.mount_type.as_deref() == Some("proc"));
        if has_mount_ns && !has_explicit_proc {
            cmd = cmd.with_proc_mount();
        }

        // UID/GID mappings
        if !linux.uid_mappings.is_empty() {
            let maps: Vec<crate::container::UidMap> = linux
                .uid_mappings
                .iter()
                .map(|m| crate::container::UidMap {
                    inside: m.container_id,
                    outside: m.host_id,
                    count: m.size,
                })
                .collect();
            cmd = cmd.with_uid_maps(&maps);
        }
        if !linux.gid_mappings.is_empty() {
            let maps: Vec<crate::container::GidMap> = linux
                .gid_mappings
                .iter()
                .map(|m| crate::container::GidMap {
                    inside: m.container_id,
                    outside: m.host_id,
                    count: m.size,
                })
                .collect();
            cmd = cmd.with_gid_maps(&maps);
        }
    }

    // process.capabilities → with_capabilities()
    if let Some(caps) = process.and_then(|p| p.capabilities.as_ref()) {
        use crate::container::Capability;
        // The OCI bounding set defines which capabilities can be in the effective set.
        // We use it (falling back to effective) as the "keep" set for Remora's
        // with_capabilities() which drops everything not in the set.
        let source = if !caps.bounding.is_empty() {
            &caps.bounding
        } else {
            &caps.effective
        };
        if !source.is_empty() {
            let mut keep = Capability::empty();
            for name in source {
                if let Some(flag) = oci_cap_to_flag(name) {
                    keep |= flag;
                }
            }
            cmd = cmd.with_capabilities(keep);
        }

        // process.capabilities.ambient → raise ambient caps in pre_exec.
        // The cap number is the bit position in the Capability bitflag.
        for name in &caps.ambient {
            if let Some(flag) = oci_cap_to_flag(name) {
                let cap_num = flag.bits().trailing_zeros() as u8;
                cmd = cmd.with_ambient_capability(cap_num);
            }
        }
    }

    // linux.maskedPaths / linux.readonlyPaths / resources / sysctl / devices / seccomp
    if let Some(ref linux) = config.linux {
        if !linux.masked_paths.is_empty() {
            let paths: Vec<&str> = linux.masked_paths.iter().map(|s| s.as_str()).collect();
            cmd = cmd.with_masked_paths(&paths);
        }
        if !linux.readonly_paths.is_empty() {
            let paths: Vec<&str> = linux.readonly_paths.iter().map(|s| s.as_str()).collect();
            cmd = cmd.with_readonly_paths(&paths);
        }

        // linux.resources → cgroup builders
        if let Some(ref res) = linux.resources {
            // Memory
            if let Some(ref mem) = res.memory {
                if let Some(limit) = mem.limit {
                    if limit > 0 {
                        cmd = cmd.with_cgroup_memory(limit);
                    }
                }
                if let Some(swap) = mem.swap {
                    cmd = cmd.with_cgroup_memory_swap(swap);
                }
                if let Some(res) = mem.reservation {
                    if res > 0 {
                        cmd = cmd.with_cgroup_memory_reservation(res);
                    }
                }
                if let Some(swappiness) = mem.swappiness {
                    cmd = cmd.with_cgroup_memory_swappiness(swappiness);
                }
            }
            // CPU + cpuset
            if let Some(ref cpu) = res.cpu {
                if let Some(shares) = cpu.shares {
                    if shares > 0 {
                        cmd = cmd.with_cgroup_cpu_shares(shares);
                    }
                }
                if let (Some(quota), Some(period)) = (cpu.quota, cpu.period) {
                    if quota > 0 && period > 0 {
                        cmd = cmd.with_cgroup_cpu_quota(quota, period);
                    }
                }
                if let Some(ref cpus) = cpu.cpus {
                    if !cpus.is_empty() {
                        cmd = cmd.with_cgroup_cpuset_cpus(cpus.clone());
                    }
                }
                if let Some(ref mems) = cpu.mems {
                    if !mems.is_empty() {
                        cmd = cmd.with_cgroup_cpuset_mems(mems.clone());
                    }
                }
            }
            // PIDs
            if let Some(ref pids) = res.pids {
                if let Some(limit) = pids.limit {
                    if limit > 0 {
                        cmd = cmd.with_cgroup_pids_limit(limit as u64);
                    }
                }
            }
            // Block I/O
            if let Some(ref bio) = res.block_io {
                if let Some(w) = bio.weight {
                    cmd = cmd.with_cgroup_blkio_weight(w);
                }
                for d in &bio.throttle_read_bps_device {
                    cmd = cmd.with_cgroup_blkio_throttle_read_bps(d.major, d.minor, d.rate);
                }
                for d in &bio.throttle_write_bps_device {
                    cmd = cmd.with_cgroup_blkio_throttle_write_bps(d.major, d.minor, d.rate);
                }
                for d in &bio.throttle_read_iops_device {
                    cmd = cmd.with_cgroup_blkio_throttle_read_iops(d.major, d.minor, d.rate);
                }
                for d in &bio.throttle_write_iops_device {
                    cmd = cmd.with_cgroup_blkio_throttle_write_iops(d.major, d.minor, d.rate);
                }
            }
            // Device cgroup allow/deny rules
            for dev in &res.devices {
                let kind = dev.kind.chars().next().unwrap_or('a');
                let major = dev.major.unwrap_or(-1);
                let minor = dev.minor.unwrap_or(-1);
                cmd =
                    cmd.with_cgroup_device_rule(dev.allow, kind, major, minor, dev.access.clone());
            }
            // Network (v1 only; silently skipped on v2)
            if let Some(ref net) = res.network {
                if let Some(class_id) = net.class_id {
                    cmd = cmd.with_cgroup_net_classid(class_id as u64);
                }
                for p in &net.priorities {
                    cmd = cmd.with_cgroup_net_priority(p.name.clone(), p.priority as u64);
                }
            }
        }

        // linux.sysctl
        for (key, value) in &linux.sysctl {
            cmd = cmd.with_sysctl(key, value);
        }

        // linux.devices
        for dev in &linux.devices {
            let major = dev.major.unwrap_or(0);
            let minor = dev.minor.unwrap_or(0);
            let kind = dev.kind.chars().next().unwrap_or('c');
            cmd = cmd.with_device(crate::container::DeviceNode {
                path: PathBuf::from(&dev.path),
                kind,
                major,
                minor,
                mode: dev.file_mode,
                uid: dev.uid,
                gid: dev.gid,
            });
        }

        // linux.seccomp → with_seccomp_program()
        if let Some(ref seccomp) = linux.seccomp {
            let prog = crate::seccomp::filter_from_oci(seccomp)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            cmd = cmd.with_seccomp_program(prog);
        }

        // linux.rootfsPropagation
        if let Some(ref prop) = linux.rootfs_propagation {
            let flags: libc::c_ulong = match prop.as_str() {
                "shared" => libc::MS_SHARED | libc::MS_REC,
                "slave" => libc::MS_SLAVE | libc::MS_REC,
                "private" => libc::MS_PRIVATE | libc::MS_REC,
                "unbindable" => libc::MS_UNBINDABLE | libc::MS_REC,
                other => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("unknown rootfsPropagation: {}", other),
                    ));
                }
            };
            cmd = cmd.with_rootfs_propagation(flags);
        }

        // linux.cgroupsPath
        if let Some(ref cg_path) = linux.cgroups_path {
            cmd = cmd.with_cgroup_path(cg_path.as_str());
        }
    }

    // process.rlimits
    for rl in process.map(|p| p.rlimits.as_slice()).unwrap_or(&[]) {
        let resource = match rl.type_.as_str() {
            "RLIMIT_CORE" => Some(libc::RLIMIT_CORE),
            "RLIMIT_CPU" => Some(libc::RLIMIT_CPU),
            "RLIMIT_DATA" => Some(libc::RLIMIT_DATA),
            "RLIMIT_FSIZE" => Some(libc::RLIMIT_FSIZE),
            "RLIMIT_LOCKS" => Some(libc::RLIMIT_LOCKS),
            "RLIMIT_MEMLOCK" => Some(libc::RLIMIT_MEMLOCK),
            "RLIMIT_MSGQUEUE" => Some(libc::RLIMIT_MSGQUEUE),
            "RLIMIT_NICE" => Some(libc::RLIMIT_NICE),
            "RLIMIT_NOFILE" => Some(libc::RLIMIT_NOFILE),
            "RLIMIT_NPROC" => Some(libc::RLIMIT_NPROC),
            "RLIMIT_RSS" => Some(libc::RLIMIT_RSS),
            "RLIMIT_RTPRIO" => Some(libc::RLIMIT_RTPRIO),
            "RLIMIT_RTTIME" => Some(libc::RLIMIT_RTTIME),
            "RLIMIT_SIGPENDING" => Some(libc::RLIMIT_SIGPENDING),
            "RLIMIT_STACK" => Some(libc::RLIMIT_STACK),
            "RLIMIT_AS" => Some(libc::RLIMIT_AS),
            _ => None,
        };
        if let Some(res) = resource {
            cmd = cmd.with_rlimit(res, rl.soft as libc::rlim_t, rl.hard as libc::rlim_t);
        }
    }

    // Track whether /dev is being freshly mounted as tmpfs (no device nodes yet).
    let mut dev_is_fresh_tmpfs = false;

    // OCI mounts (processed in order)
    for mount in &config.mounts {
        let dest = &mount.destination;
        let mount_type = mount.mount_type.as_deref().unwrap_or("bind");

        if dest == "/dev" && mount_type == "tmpfs" {
            dev_is_fresh_tmpfs = true;
        }
        let is_ro = mount.options.iter().any(|o| o == "ro" || o == "readonly");

        // Parse MS_* flags from option strings.
        // Propagation flags (shared/slave/private/unbindable) must be applied as a
        // SEPARATE mount(2) call after the initial mount — combining them in the
        // initial call returns EINVAL on Linux.
        let mut flags: libc::c_ulong = 0;
        let mut propagation_flags: libc::c_ulong = 0;
        let mut extra_data_parts: Vec<&str> = Vec::new();
        for opt in &mount.options {
            match opt.as_str() {
                "nosuid" => flags |= libc::MS_NOSUID,
                "noexec" => flags |= libc::MS_NOEXEC,
                "nodev" => flags |= libc::MS_NODEV,
                "ro" | "readonly" => flags |= libc::MS_RDONLY,
                "relatime" => flags |= libc::MS_RELATIME,
                "noatime" => flags |= libc::MS_NOATIME,
                "nodiratime" => flags |= libc::MS_NODIRATIME,
                "strictatime" => flags |= libc::MS_STRICTATIME,
                // Propagation flags — collect separately, applied as a remount step.
                "shared" => propagation_flags |= libc::MS_SHARED,
                "rshared" => propagation_flags |= libc::MS_SHARED | libc::MS_REC,
                "slave" => propagation_flags |= libc::MS_SLAVE,
                "rslave" => propagation_flags |= libc::MS_SLAVE | libc::MS_REC,
                "private" => propagation_flags |= libc::MS_PRIVATE,
                "rprivate" => propagation_flags |= libc::MS_PRIVATE | libc::MS_REC,
                "unbindable" => propagation_flags |= libc::MS_UNBINDABLE,
                "runbindable" => propagation_flags |= libc::MS_UNBINDABLE | libc::MS_REC,
                "bind" => flags |= libc::MS_BIND,
                "rbind" => flags |= libc::MS_BIND | libc::MS_REC,
                other => extra_data_parts.push(other),
            }
        }
        let extra_data = extra_data_parts.join(",");

        match mount_type {
            "tmpfs" => {
                // Use with_kernel_mount so the parsed MS_* flags (nosuid, strictatime, etc.)
                // go into the mount(2) flags argument, while only non-flag options
                // (mode=, size=, uid=, gid=) are passed as mount data.
                // with_tmpfs hardcodes MS_NOSUID|MS_NODEV and passes all opts as data,
                // which causes EINVAL when flag-like tokens appear in the data string.
                //
                // Do NOT hardcode MS_NODEV here — /dev is a tmpfs that needs device nodes.
                // Only apply MS_NODEV if the OCI config actually specified "nodev".
                //
                // Use the config-specified source (e.g. "shm" for /dev/shm) so that
                // /proc/mounts shows the correct source string, which runtimetest validates.
                let src = mount.source.as_deref().unwrap_or("tmpfs");
                cmd = cmd.with_kernel_mount("tmpfs", src, dest, flags, &extra_data);
            }
            "proc" => {
                let f = libc::MS_NOSUID | libc::MS_NOEXEC | libc::MS_NODEV | flags;
                let src = mount.source.as_deref().unwrap_or("proc");
                cmd = cmd.with_kernel_mount("proc", src, dest, f, "");
            }
            "sysfs" => {
                let f = libc::MS_NOSUID | libc::MS_NOEXEC | libc::MS_NODEV | flags;
                let src = mount.source.as_deref().unwrap_or("sysfs");
                cmd = cmd.with_kernel_mount("sysfs", src, dest, f, "");
            }
            "devpts" => {
                let f = libc::MS_NOSUID | libc::MS_NOEXEC | flags;
                let src = mount.source.as_deref().unwrap_or("devpts");
                let data = if extra_data.is_empty() {
                    "newinstance,ptmxmode=0666,mode=0620".to_string()
                } else {
                    format!("newinstance,{}", extra_data)
                };
                cmd = cmd.with_kernel_mount("devpts", src, dest, f, data);
            }
            "mqueue" => {
                let f = libc::MS_NOSUID | libc::MS_NOEXEC | libc::MS_NODEV | flags;
                let src = mount.source.as_deref().unwrap_or("mqueue");
                cmd = cmd.with_kernel_mount("mqueue", src, dest, f, "");
            }
            "cgroup" => {
                let f =
                    libc::MS_NOSUID | libc::MS_NOEXEC | libc::MS_NODEV | libc::MS_RELATIME | flags;
                let src = mount.source.as_deref().unwrap_or("cgroup");
                cmd = cmd.with_kernel_mount("cgroup", src, dest, f, &extra_data);
            }
            "cgroup2" => {
                let f =
                    libc::MS_NOSUID | libc::MS_NOEXEC | libc::MS_NODEV | libc::MS_RELATIME | flags;
                let src = mount.source.as_deref().unwrap_or("cgroup2");
                cmd = cmd.with_kernel_mount("cgroup2", src, dest, f, "");
            }
            // "bind" or anything unrecognised → bind mount
            _ => {
                if let Some(ref source) = mount.source {
                    if is_ro {
                        cmd = cmd.with_bind_mount_ro(source, dest);
                    } else {
                        cmd = cmd.with_bind_mount(source, dest);
                    }
                }
            }
        }

        // Apply propagation flags as a separate remount step after the initial mount.
        if propagation_flags != 0 {
            cmd = cmd.with_propagation_remount(dest, propagation_flags);
        }
    }

    // When /dev is a fresh tmpfs, populate the standard OCI devices and symlinks.
    // Per the OCI Runtime Spec §4.5, the runtime MUST create these device nodes
    // and symlinks. mknod/symlink errors are silently ignored (device may already exist).
    if dev_is_fresh_tmpfs {
        use crate::container::DeviceNode;
        let default_devices: &[(&str, char, u64, u64, u32)] = &[
            ("/dev/null", 'c', 1, 3, 0o666),
            ("/dev/zero", 'c', 1, 5, 0o666),
            ("/dev/full", 'c', 1, 7, 0o666),
            ("/dev/random", 'c', 1, 8, 0o666),
            ("/dev/urandom", 'c', 1, 9, 0o666),
            ("/dev/tty", 'c', 5, 0, 0o666),
        ];
        for &(path, kind, major, minor, mode) in default_devices {
            cmd = cmd.with_device(DeviceNode {
                path: PathBuf::from(path),
                kind,
                major,
                minor,
                mode,
                uid: 0,
                gid: 0,
            });
        }

        // OCI spec §4.5: runtime MUST create these default symlinks in /dev.
        let default_symlinks: &[(&str, &str)] = &[
            ("/dev/fd", "/proc/self/fd"),
            ("/dev/stdin", "/proc/self/fd/0"),
            ("/dev/stdout", "/proc/self/fd/1"),
            ("/dev/stderr", "/proc/self/fd/2"),
            ("/dev/ptmx", "pts/ptmx"),
        ];
        for &(link, target) in default_symlinks {
            cmd = cmd.with_dev_symlink(link, target);
        }
    }

    Ok(cmd)
}

// ---------------------------------------------------------------------------
// Socket helpers
// ---------------------------------------------------------------------------

/// Create a Unix domain socket listener at `path`. Returns the listening fd.
fn create_listen_socket(path: &Path) -> io::Result<i32> {
    use std::os::unix::ffi::OsStrExt;
    let path_bytes = path.as_os_str().as_bytes();
    if path_bytes.len() >= 108 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "socket path too long",
        ));
    }

    unsafe {
        let fd = libc::socket(libc::AF_UNIX, libc::SOCK_STREAM | libc::SOCK_CLOEXEC, 0);
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }

        let mut addr: libc::sockaddr_un = std::mem::zeroed();
        addr.sun_family = libc::AF_UNIX as libc::sa_family_t;
        std::ptr::copy_nonoverlapping(
            path_bytes.as_ptr() as *const libc::c_char,
            addr.sun_path.as_mut_ptr(),
            path_bytes.len(),
        );
        let addr_len = std::mem::size_of::<libc::sockaddr_un>() as libc::socklen_t;

        let ret = libc::bind(
            fd,
            &addr as *const libc::sockaddr_un as *const libc::sockaddr,
            addr_len,
        );
        if ret != 0 {
            libc::close(fd);
            return Err(io::Error::last_os_error());
        }

        let ret = libc::listen(fd, 1);
        if ret != 0 {
            libc::close(fd);
            return Err(io::Error::last_os_error());
        }

        Ok(fd)
    }
}

/// Connect to a Unix domain socket at `path`. Returns the connected fd.
fn connect_socket(path: &Path) -> io::Result<i32> {
    use std::os::unix::ffi::OsStrExt;
    let path_bytes = path.as_os_str().as_bytes();

    unsafe {
        let fd = libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0);
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }

        let mut addr: libc::sockaddr_un = std::mem::zeroed();
        addr.sun_family = libc::AF_UNIX as libc::sa_family_t;
        std::ptr::copy_nonoverlapping(
            path_bytes.as_ptr() as *const libc::c_char,
            addr.sun_path.as_mut_ptr(),
            path_bytes.len(),
        );
        let addr_len = std::mem::size_of::<libc::sockaddr_un>() as libc::socklen_t;

        let ret = libc::connect(
            fd,
            &addr as *const libc::sockaddr_un as *const libc::sockaddr,
            addr_len,
        );
        if ret != 0 {
            libc::close(fd);
            return Err(io::Error::last_os_error());
        }

        Ok(fd)
    }
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------
// Hook execution
// ---------------------------------------------------------------------------

/// Run a list of OCI hooks sequentially. Each hook is executed as a child
/// process in the host namespace; the container state JSON is passed on stdin.
/// Returns an error if any hook exits non-zero.
fn run_hooks(hooks: &[OciHook], state: &OciState) -> io::Result<()> {
    use std::io::Write;

    let state_json = serde_json::to_vec(state).map_err(io::Error::other)?;

    for hook in hooks {
        let mut child = std::process::Command::new(&hook.path);

        // args[0] is conventionally the program name; the rest are real args.
        if hook.args.len() > 1 {
            child.args(&hook.args[1..]);
        }

        for env_entry in &hook.env {
            if let Some(eq) = env_entry.find('=') {
                child.env(&env_entry[..eq], &env_entry[eq + 1..]);
            }
        }

        child.stdin(std::process::Stdio::piped());

        let mut proc = child.spawn()?;

        // Write state JSON to the hook's stdin.
        if let Some(mut stdin) = proc.stdin.take() {
            let _ = stdin.write_all(&state_json);
        }

        let timeout = hook.timeout.map(|t| Duration::from_secs(t as u64));

        if let Some(dur) = timeout {
            // Poll for completion with timeout.
            let deadline = std::time::Instant::now() + dur;
            loop {
                match proc.try_wait()? {
                    Some(status) => {
                        if !status.success() {
                            return Err(io::Error::other(format!(
                                "hook {} exited with status {}",
                                hook.path, status
                            )));
                        }
                        break;
                    }
                    None => {
                        if std::time::Instant::now() >= deadline {
                            let _ = proc.kill();
                            return Err(io::Error::new(
                                io::ErrorKind::TimedOut,
                                format!("hook {} timed out after {}s", hook.path, dur.as_secs()),
                            ));
                        }
                        std::thread::sleep(Duration::from_millis(50));
                    }
                }
            }
        } else {
            let status = proc.wait()?;
            if !status.success() {
                return Err(io::Error::other(format!(
                    "hook {} exited with status {}",
                    hook.path, status
                )));
            }
        }
    }

    Ok(())
}

/// Run a list of OCI hooks in the container's namespaces (createContainer /
/// startContainer hooks).
///
/// Opens the container's namespace fds from `/proc/<container_pid>/ns/{net,uts,ipc}`,
/// then forks a helper child that calls `setns(2)` for each namespace and runs every
/// hook sequentially in that joined namespace. The parent waits for the child.
///
/// Mount namespace is intentionally excluded: by the time `createContainer` /
/// `startContainer` hooks are called in remora's lifecycle the container process
/// has already called `pivot_root`, so the mount namespace's filesystem view is
/// the container rootfs — the hook binary (on the host) would not be found.
/// OCI runtimes that run hooks before `pivot_root` (e.g. runc) can join the mount
/// namespace; remora joins the remaining namespaces (net, uts, ipc).
///
/// PID namespace is excluded for the same reason: `setns(CLONE_NEWPID)` only
/// affects `pid_for_children`; the calling process is not moved.
///
/// This satisfies the OCI spec requirement that `createContainer` and
/// `startContainer` hooks execute inside the container's network/uts/ipc
/// namespace context.
fn run_hooks_in_ns(hooks: &[OciHook], state: &OciState, container_pid: i32) -> io::Result<()> {
    if hooks.is_empty() {
        return Ok(());
    }

    // Only join non-filesystem namespaces so the hook binary on the host is
    // still accessible after setns.
    let ns_names = ["net", "uts", "ipc"];
    let mut ns_fds: Vec<i32> = Vec::new();
    for ns in &ns_names {
        let path = format!("/proc/{}/ns/{}", container_pid, ns);
        let path_c = match std::ffi::CString::new(path.as_bytes()) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let fd = unsafe { libc::open(path_c.as_ptr(), libc::O_RDONLY | libc::O_CLOEXEC) };
        if fd >= 0 {
            // Only add if it differs from the host namespace (same inode = already joined)
            let host_path = format!("/proc/1/ns/{}", ns);
            let host_c = match std::ffi::CString::new(host_path.as_bytes()) {
                Ok(c) => c,
                Err(_) => {
                    ns_fds.push(fd);
                    continue;
                }
            };
            let mut cst = unsafe { std::mem::zeroed::<libc::stat>() };
            let mut hst = unsafe { std::mem::zeroed::<libc::stat>() };
            let cst_ok = unsafe { libc::fstat(fd, &mut cst) } == 0;
            let hst_ok = unsafe { libc::stat(host_c.as_ptr(), &mut hst) } == 0;
            if cst_ok && hst_ok && cst.st_ino == hst.st_ino {
                // Same namespace as host — skip (setns would be a no-op / EPERM)
                unsafe { libc::close(fd) };
            } else {
                ns_fds.push(fd);
            }
        }
    }

    let state_json = serde_json::to_vec(state).map_err(io::Error::other)?;

    // Fork a helper process that joins the container namespaces, then runs hooks.
    let pid = unsafe { libc::fork() };
    match pid {
        -1 => {
            for fd in ns_fds {
                unsafe { libc::close(fd) };
            }
            Err(io::Error::last_os_error())
        }
        0 => {
            // CHILD: join each namespace then exec hooks.
            //
            // IMPORTANT: we must NOT call Rust's std::process::Command here —
            // doing so after fork() in a potentially-multithreaded process risks
            // deadlock (Rust's internal I/O and allocator mutexes may be held by
            // threads that no longer exist in the forked child).  Use raw libc
            // fork+exec for each hook instead.
            for &fd in &ns_fds {
                unsafe { libc::setns(fd, 0) };
                unsafe { libc::close(fd) };
            }

            // Create a pipe so we can write state JSON to each hook's stdin.
            for hook in hooks {
                let mut stdin_pipe = [0i32; 2];
                if unsafe { libc::pipe(stdin_pipe.as_mut_ptr()) } != 0 {
                    unsafe { libc::_exit(1) };
                }
                let (pipe_r, pipe_w) = (stdin_pipe[0], stdin_pipe[1]);

                // Build argv and envp as CString arrays.
                let mut argv_cstr: Vec<std::ffi::CString> = Vec::new();
                let path_c = match std::ffi::CString::new(hook.path.as_bytes()) {
                    Ok(c) => c,
                    Err(_) => unsafe { libc::_exit(1) },
                };
                argv_cstr.push(path_c.clone());
                for arg in hook.args.iter().skip(1) {
                    match std::ffi::CString::new(arg.as_bytes()) {
                        Ok(c) => argv_cstr.push(c),
                        Err(_) => unsafe { libc::_exit(1) },
                    }
                }
                let mut argv_ptrs: Vec<*const libc::c_char> =
                    argv_cstr.iter().map(|c| c.as_ptr()).collect();
                argv_ptrs.push(std::ptr::null());

                let mut envp_cstr: Vec<std::ffi::CString> = Vec::new();
                for entry in &hook.env {
                    if let Ok(c) = std::ffi::CString::new(entry.as_bytes()) {
                        envp_cstr.push(c);
                    }
                }
                let mut envp_ptrs: Vec<*const libc::c_char> =
                    envp_cstr.iter().map(|c| c.as_ptr()).collect();
                envp_ptrs.push(std::ptr::null());

                let hook_pid = unsafe { libc::fork() };
                match hook_pid {
                    -1 => unsafe { libc::_exit(1) },
                    0 => {
                        // Hook grandchild: redirect stdin from pipe, then exec.
                        unsafe { libc::close(pipe_w) };
                        unsafe { libc::dup2(pipe_r, 0) };
                        unsafe { libc::close(pipe_r) };
                        unsafe {
                            libc::execve(path_c.as_ptr(), argv_ptrs.as_ptr(), envp_ptrs.as_ptr())
                        };
                        unsafe { libc::_exit(127) };
                    }
                    _ => {
                        // Helper child: write state JSON to hook's stdin, then wait.
                        unsafe { libc::close(pipe_r) };
                        let mut written = 0usize;
                        while written < state_json.len() {
                            let n = unsafe {
                                libc::write(
                                    pipe_w,
                                    state_json[written..].as_ptr() as *const libc::c_void,
                                    state_json.len() - written,
                                )
                            };
                            if n <= 0 {
                                break;
                            }
                            written += n as usize;
                        }
                        unsafe { libc::close(pipe_w) };

                        // Wait for hook, respecting optional timeout.
                        let deadline = hook
                            .timeout
                            .map(|t| std::time::Instant::now() + Duration::from_secs(t as u64));
                        loop {
                            let mut wstatus = 0i32;
                            let ret =
                                unsafe { libc::waitpid(hook_pid, &mut wstatus, libc::WNOHANG) };
                            if ret == hook_pid {
                                let ok =
                                    libc::WIFEXITED(wstatus) && libc::WEXITSTATUS(wstatus) == 0;
                                if !ok {
                                    unsafe { libc::_exit(1) };
                                }
                                break;
                            }
                            if let Some(dl) = deadline {
                                if std::time::Instant::now() >= dl {
                                    unsafe { libc::kill(hook_pid, libc::SIGKILL) };
                                    unsafe { libc::_exit(1) };
                                }
                            }
                            // Brief sleep to avoid busy-poll.
                            unsafe {
                                libc::usleep(20_000);
                            }
                        }
                    }
                }
            }
            unsafe { libc::_exit(0) };
        }
        child_pid => {
            // PARENT: close our copies of the ns fds.
            for fd in ns_fds {
                unsafe { libc::close(fd) };
            }
            // Wait for the helper.
            let mut status = 0i32;
            let ret = unsafe { libc::waitpid(child_pid, &mut status, 0) };
            if ret < 0 {
                return Err(io::Error::last_os_error());
            }
            if libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0 {
                Ok(())
            } else {
                Err(io::Error::other(
                    "createContainer/startContainer hook failed",
                ))
            }
        }
    }
}

// ---------------------------------------------------------------------------
// OCI subcommand implementations
// ---------------------------------------------------------------------------

/// Walk `/proc` to find the deepest descendant of `ancestor_pid`.
///
/// After the PID-namespace double-fork the process tree looks like:
///   shim (ancestor) → spawn child (intermediate/waitpid) → container
///
/// Scans `/proc/[pid]/status` PPid fields to build the chain.
/// Returns the leaf PID, or `None` if the tree can't be walked.
/// Find the grandchild of `ancestor_pid` — used for double-fork PID namespace cases.
/// The process tree is: shim → intermediate (P) → container (C).
/// We want C's host PID, not P's PID or C's children.
fn find_grandchild_of(ancestor_pid: i32) -> Option<i32> {
    for _ in 0..10 {
        if let Some(c1) = find_child_of(ancestor_pid) {
            if let Some(c2) = find_child_of(c1) {
                return Some(c2);
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    // Fallback: if grandchild not yet visible, return the direct child.
    find_child_of(ancestor_pid)
}

/// Find a child process of the given `parent_pid` by scanning `/proc`.
fn find_child_of(parent_pid: i32) -> Option<i32> {
    let parent_str = parent_pid.to_string();
    let entries = fs::read_dir("/proc").ok()?;
    for entry in entries {
        // Skip entries that vanish mid-scan (process exited).
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        let name = entry.file_name();
        let name_str = match name.to_str() {
            Some(s) => s,
            None => continue,
        };
        // Only look at numeric (PID) directories.
        if !name_str.starts_with(|c: char| c.is_ascii_digit()) {
            continue;
        }
        let status_path = entry.path().join("status");
        if let Ok(content) = fs::read_to_string(&status_path) {
            for line in content.lines() {
                if let Some(ppid) = line.strip_prefix("PPid:\t") {
                    if ppid.trim() == parent_str {
                        return name_str.parse().ok();
                    }
                    break;
                }
            }
        }
    }
    None
}

/// Send a file descriptor to a caller-created Unix socket via `SCM_RIGHTS`.
///
/// Used by `cmd_create` to hand the PTY master fd to the caller (e.g. containerd)
/// when `process.terminal: true`.  The caller must already be listening on the
/// socket; this function connects to it, sends a 1-byte dummy payload with the
/// fd as ancillary data, then closes the connection.
fn send_fd_to_console_socket(socket_path: &Path, fd: i32) -> io::Result<()> {
    use std::os::unix::io::AsRawFd;
    use std::os::unix::net::UnixStream;

    let stream = UnixStream::connect(socket_path)?;
    let sock_fd = stream.as_raw_fd();

    // Build the SCM_RIGHTS control message.
    let cmsg_space =
        unsafe { libc::CMSG_SPACE(std::mem::size_of::<i32>() as libc::c_uint) as usize };
    let mut cmsg_buf = vec![0u8; cmsg_space];

    let mut iov_buf = [0u8; 1];
    let mut iov = libc::iovec {
        iov_base: iov_buf.as_mut_ptr() as *mut libc::c_void,
        iov_len: 1,
    };

    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    msg.msg_control = cmsg_buf.as_mut_ptr() as *mut libc::c_void;
    msg.msg_controllen = cmsg_space as _;

    let cmsg = unsafe { libc::CMSG_FIRSTHDR(&msg) };
    if cmsg.is_null() {
        return Err(io::Error::other("CMSG_FIRSTHDR returned null"));
    }
    unsafe {
        (*cmsg).cmsg_level = libc::SOL_SOCKET;
        (*cmsg).cmsg_type = libc::SCM_RIGHTS;
        (*cmsg).cmsg_len = libc::CMSG_LEN(std::mem::size_of::<i32>() as _) as _;
        let data_ptr = libc::CMSG_DATA(cmsg) as *mut i32;
        *data_ptr = fd;
    }

    let ret = unsafe { libc::sendmsg(sock_fd, &msg, 0) };
    if ret < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// `remora create <id> --bundle <bundle>` — set up container, suspend before exec.
///
/// Forks a shim that calls `command.spawn()`. The container's pre_exec writes
/// its PID to a ready pipe (signalling "created"), then blocks on accept().
/// The parent reads the PID, writes state.json, and exits. The shim is orphaned
/// and waits for the container; `remora start` later unblocks it.
///
/// `console_socket` — when `process.terminal: true` the runtime allocates a PTY,
/// wires the slave to the container's stdio, and sends the master fd to this
/// Unix socket via `sendmsg(SCM_RIGHTS)`.
///
/// `pid_file` — if provided, the container's host PID is written to this file
/// after the container is created, for use by higher-level runtimes (containerd, CRI-O).
pub fn cmd_create(
    id: &str,
    bundle_path: &Path,
    console_socket: Option<&Path>,
    pid_file: Option<&Path>,
) -> io::Result<()> {
    let dir = state_dir(id);
    if dir.exists() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!(
                "container '{}' already exists — run 'remora delete {}' first",
                id, id
            ),
        ));
    }

    let bundle = bundle_path.canonicalize()?;
    let config = config_from_bundle(&bundle)?;
    fs::create_dir_all(&dir)?;

    // Ready pipe: grandchild writes PID → parent reads it.
    let mut pipe_fds = [0i32; 2];
    if unsafe { libc::pipe2(pipe_fds.as_mut_ptr(), 0) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let (ready_r, ready_w) = (pipe_fds[0], pipe_fds[1]);

    // Listen socket: grandchild blocks on accept() until "remora start" connects.
    let sock_path = exec_sock_path(id);
    let listen_fd = create_listen_socket(&sock_path).inspect_err(|_e| unsafe {
        libc::close(ready_r);
        libc::close(ready_w);
    })?;

    // Allocate PTY when the bundle requests a terminal (process.terminal = true)
    // AND a console socket path was provided by the caller.
    // master_raw: held by the parent until it is sent to console_socket.
    // slave_raw:  inherited by the shim → container pre_exec wires it to 0/1/2.
    let wants_terminal = config.process.as_ref().map(|p| p.terminal).unwrap_or(false);
    let pty_fds: Option<(i32, i32)> = if wants_terminal && console_socket.is_some() {
        let pty =
            nix::pty::openpty(None, None).map_err(|e| io::Error::other(format!("openpty: {e}")))?;
        use std::os::fd::IntoRawFd;
        let master_raw = pty.master.into_raw_fd();
        let slave_raw = pty.slave.into_raw_fd();
        // Master: CLOEXEC so the container's exec doesn't inherit it.
        unsafe { libc::fcntl(master_raw, libc::F_SETFD, libc::FD_CLOEXEC) };
        // Slave: explicitly NOT CLOEXEC — must survive the fork chain into pre_exec.
        unsafe {
            let flags = libc::fcntl(slave_raw, libc::F_GETFD);
            libc::fcntl(slave_raw, libc::F_SETFD, flags & !libc::FD_CLOEXEC);
        }
        Some((master_raw, slave_raw))
    } else {
        None
    };

    // Build the container command with OCI sync hooks (and optional PTY slave).
    let command = match build_command(&config, &bundle) {
        Ok(mut c) => {
            c = c.with_oci_sync(ready_w, listen_fd);
            if let Some((_, slave_raw)) = pty_fds {
                c = c.with_pty_slave(slave_raw);
            }
            c
        }
        Err(e) => {
            unsafe {
                libc::close(ready_r);
                libc::close(ready_w);
                libc::close(listen_fd);
                if let Some((m, s)) = pty_fds {
                    libc::close(m);
                    libc::close(s);
                }
            }
            let _ = fs::remove_dir_all(&dir);
            return Err(e);
        }
    };

    // Fork shim. The shim calls command.spawn() (which forks AGAIN to create
    // the actual container). Rust's spawn() blocks the shim until the container
    // execs; the parent reads the ready pipe and exits without waiting.
    match unsafe { libc::fork() } {
        -1 => {
            unsafe {
                libc::close(ready_r);
                libc::close(ready_w);
                libc::close(listen_fd);
                if let Some((m, s)) = pty_fds {
                    libc::close(m);
                    libc::close(s);
                }
            }
            let _ = fs::remove_dir_all(&dir);
            Err(io::Error::last_os_error())
        }
        0 => {
            // SHIM: close the master fd (only the parent sends it to console_socket).
            if let Some((master_raw, _)) = pty_fds {
                unsafe { libc::close(master_raw) };
            }
            // Close the read end of the ready pipe (shim doesn't need it).
            // Keep fds 1 and 2 so container output flows to whatever the caller
            // (e.g. runtime-tools) has set as stdout/stderr — the conformance test
            // harness captures runtimetest's TAP output through this pipe chain.
            // Redirect only stdin to /dev/null since the shim has no interactive input.
            unsafe {
                libc::close(ready_r);
                let dev_null = libc::open(c"/dev/null".as_ptr(), libc::O_RDONLY, 0);
                if dev_null >= 0 {
                    libc::dup2(dev_null, 0);
                    if dev_null > 0 {
                        libc::close(dev_null);
                    }
                }
            }
            let mut child = match command.spawn() {
                Ok(c) => c,
                Err(_) => {
                    unsafe { libc::_exit(1) };
                }
            };
            // Container exec'd. Wait for it, then exit (shim is orphaned at this point).
            child.wait().ok();
            unsafe { libc::_exit(0) };
        }
        shim_pid => {
            // PARENT: close the write ends (child has them).
            // Also close the slave fd — only the container's pre_exec should use it.
            unsafe { libc::close(ready_w) };
            unsafe { libc::close(listen_fd) };
            if let Some((_, slave_raw)) = pty_fds {
                unsafe { libc::close(slave_raw) };
            }

            // Wait for the ready signal (4 bytes) written by the container's pre_exec.
            // The bytes carry a PID, but after the PID-namespace double-fork the
            // container sees itself as PID 1 (namespace-local), which is useless on
            // the host.  We only use the read as a "setup complete" gate; the actual
            // host-visible PID comes from the shim's child.pid() below.
            let mut pid_buf = [0u8; 4];
            let n = unsafe { libc::read(ready_r, pid_buf.as_mut_ptr() as *mut libc::c_void, 4) };
            unsafe { libc::close(ready_r) };

            if n != 4 {
                let _ = fs::remove_dir_all(&dir);
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "container setup failed (ready pipe closed before PID was written)",
                ));
            }

            // The pre_exec pipe carries `getpid()` from inside the container.
            // Without a PID namespace this is the host-visible PID (correct).
            // With a PID namespace + double-fork (create OR join-by-path), the
            // container's getpid() returns a namespace-local PID, which is
            // useless on the host.
            //
            // Two cases require a double-fork (both produce namespace-local PIDs):
            //   A. linux.namespaces has {type:"pid"} without a path → creates new ns; container is PID 1
            //   B. linux.namespaces has {type:"pid", path:"..."} → joins existing ns; container is PID 2+
            //
            // For Case A the sentinel is pipe_pid==1. For Case B pipe_pid>1 but
            // is still namespace-local, so we must detect Case B via the config.
            let has_pid_ns_join = config
                .linux
                .as_ref()
                .map(|l| {
                    l.namespaces
                        .iter()
                        .any(|ns| ns.ns_type == "pid" && ns.path.is_some())
                })
                .unwrap_or(false);

            let pipe_pid = i32::from_ne_bytes(pid_buf);
            // When a double-fork occurs (Case A: new PID namespace, or Case B: join PID ns),
            // the container process is the GRANDCHILD of the shim:
            //   shim → intermediate(P) → container(C)
            // We want C's host PID.  The intermediate P just waits and exits.
            // pipe_pid==1 signals Case A; has_pid_ns_join signals Case B.
            let container_pid = if pipe_pid <= 1 || has_pid_ns_join {
                let found = find_grandchild_of(shim_pid);
                log::debug!(
                    "OCI create: id={} shim_pid={} pipe_pid={} found_grandchild={:?}",
                    id,
                    shim_pid,
                    pipe_pid,
                    found
                );
                found.unwrap_or(pipe_pid)
            } else {
                pipe_pid
            };
            log::debug!(
                "OCI create: id={} container_pid={} has_pid_ns_join={}",
                id,
                container_pid,
                has_pid_ns_join
            );

            // Write state.json with status=created.
            let state = OciState {
                oci_version: "1.0.2".to_string(),
                id: id.to_string(),
                status: "created".to_string(),
                pid: container_pid,
                bundle: bundle.to_string_lossy().into_owned(),
                annotations: config.annotations.clone(),
                bridge_ip: None,
                pid_start_time: read_pid_start_time(container_pid),
            };
            write_state(id, &state)?;

            // Write PID to --pid-file if requested (used by containerd / CRI-O).
            if let Some(pf) = pid_file {
                fs::write(pf, format!("{}", container_pid))?;
            }

            // Send PTY master fd to caller via console_socket (SCM_RIGHTS).
            // The container's stdio is already wired to the slave in pre_exec;
            // now give the caller the master so it can relay I/O.
            if let Some((master_raw, _)) = pty_fds {
                if let Some(sock_path) = console_socket {
                    if let Err(e) = send_fd_to_console_socket(sock_path, master_raw) {
                        log::warn!("console-socket: failed to send PTY master fd: {}", e);
                    }
                }
                unsafe { libc::close(master_raw) };
            }

            // Run lifecycle hooks after container is in "created" state.
            if let Some(ref hooks) = config.hooks {
                // prestart + createRuntime: host namespace (per OCI spec)
                if !hooks.prestart.is_empty() {
                    run_hooks(&hooks.prestart, &state)?;
                }
                if !hooks.create_runtime.is_empty() {
                    run_hooks(&hooks.create_runtime, &state)?;
                }
                // createContainer: container namespace (per OCI spec)
                if !hooks.create_container.is_empty() {
                    run_hooks_in_ns(&hooks.create_container, &state, container_pid)?;
                }
            }

            // Parent exits; the shim (blocking in spawn()) is adopted by init.
            Ok(())
        }
    }
}

/// `remora start <id>` — signal the container to exec.
pub fn cmd_start(id: &str) -> io::Result<()> {
    let state = read_state(id)?;
    if state.status != "created" {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "container '{}' is not in 'created' state (current: {})",
                id, state.status
            ),
        ));
    }

    // Run startContainer hooks in the container's namespace BEFORE exec.
    // These must run after "created" state but before the user process starts.
    if let Ok(config) = config_from_bundle(std::path::Path::new(&state.bundle)) {
        if let Some(ref hooks) = config.hooks {
            if !hooks.start_container.is_empty() {
                run_hooks_in_ns(&hooks.start_container, &state, state.pid)?;
            }
        }
    }

    // Connect to exec.sock and send the start byte.
    let sock_path = exec_sock_path(id);
    let fd = connect_socket(&sock_path)?;
    unsafe {
        let buf = [1u8];
        libc::write(fd, buf.as_ptr() as *const libc::c_void, 1);
        libc::close(fd);
    }

    // Update state to running.
    let mut state = state;
    state.status = "running".to_string();
    write_state(id, &state)?;

    // Remove exec.sock — the container has exec'd and no longer listens.
    let _ = fs::remove_file(&sock_path);

    // Run poststart hooks (best-effort — don't fail the start command if hooks fail).
    if let Ok(config) = config_from_bundle(std::path::Path::new(&state.bundle)) {
        if let Some(ref hooks) = config.hooks {
            if !hooks.poststart.is_empty() {
                let _ = run_hooks(&hooks.poststart, &state);
            }
        }
    }

    Ok(())
}

/// Returns true if `pid` is a zombie process (state 'Z' in /proc/<pid>/stat).
/// Zombies pass `kill(pid, 0)` but are effectively stopped.
fn is_zombie_pid(pid: libc::pid_t) -> bool {
    let stat_path = format!("/proc/{}/stat", pid);
    fs::read_to_string(&stat_path)
        .ok()
        .and_then(|s| {
            // /proc/pid/stat: pid (comm) state ... — comm can contain spaces and ')',
            // so find the last ')' to reliably locate the state character.
            s.rfind(')')
                .map(|i| s[i + 1..].trim_start().starts_with('Z'))
        })
        .unwrap_or(false)
}

/// Read the process start time (field 22) from /proc/<pid>/stat.
///
/// The starttime field is the number of clock ticks since boot at which the
/// process started. It is monotonic, never reused for a different process
/// instance, and survives as long as the kernel holds the process table entry.
///
/// Used to detect PID reuse: if `state.pid_start_time` differs from the value
/// read here, a new unrelated process has claimed the original container's PID.
///
/// Returns None if /proc/<pid>/stat is unreadable or unparseable.
pub fn read_pid_start_time(pid: libc::pid_t) -> Option<u64> {
    let stat_path = format!("/proc/{}/stat", pid);
    let contents = fs::read_to_string(&stat_path).ok()?;
    // Fields after the comm (which may contain spaces/parens) start after the last ')'.
    let after_comm = contents.rfind(')')?;
    let rest = contents[after_comm + 1..].trim_start();
    // Remaining fields are space-separated: state ppid pgrp session tty_nr ...
    // Field 22 in the full stat line = index 19 in the post-comm remainder (0-based).
    rest.split_whitespace().nth(19)?.parse::<u64>().ok()
}

pub fn cmd_state(id: &str) -> io::Result<()> {
    let mut state = read_state(id)?;

    // Determine actual liveness via kill(pid, 0), zombie check, and PID reuse detection.
    if state.status == "created" || state.status == "running" {
        let alive = unsafe { libc::kill(state.pid, 0) } == 0;
        let zombie = alive && is_zombie_pid(state.pid);
        log::debug!(
            "OCI state: id={} state.pid={} alive={} zombie={}",
            id,
            state.pid,
            alive,
            zombie
        );

        // PID reuse detection: if the process appears alive but its starttime differs
        // from what we recorded at create time, a new unrelated process has claimed
        // state.pid — the original container has exited without us noticing.
        let pid_reused = if alive && !zombie {
            if let Some(stored) = state.pid_start_time {
                match read_pid_start_time(state.pid) {
                    Some(current) if current != stored => {
                        log::warn!(
                            "container '{}': PID {} reused (stored starttime={}, current={}); \
                             treating as stopped",
                            id,
                            state.pid,
                            stored,
                            current
                        );
                        true
                    }
                    _ => false,
                }
            } else {
                false
            }
        } else {
            false
        };

        if !alive || zombie || pid_reused {
            state.status = "stopped".to_string();
            // Persist the stopped status to state.json so cmd_kill can observe it.
            // This is the authoritative state transition: once "stopped" is on disk,
            // subsequent kill calls will be refused (OCI spec compliance).
            if let Err(e) = write_state(id, &state) {
                log::warn!("container '{}': failed to persist stopped state: {}", id, e);
            }
        }
    }

    let json = serde_json::to_string_pretty(&state).map_err(io::Error::other)?;
    println!("{}", json);
    Ok(())
}

/// `remora kill <id> <signal>` — send a signal to the container process.
pub fn cmd_kill(id: &str, signal: &str) -> io::Result<()> {
    let state = read_state(id)?;

    // OCI spec: kill on a container that is "stopped" MUST fail.
    // We gate only on state.json status, not on process liveness.
    //
    // Rationale: cmd_state writes "stopped" to state.json when it detects the process
    // has exited, which gates cmd_kill for kill.t test 4. Containers that exit before
    // kill is called (e.g. pidfile.t with `true`) are still killable because state.json
    // still says "running" — cmd_state hasn't been called yet.
    if state.status == "stopped" {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "container '{}' is stopped — kill only valid on created/running containers",
                id
            ),
        ));
    }

    // PID reuse detection: before sending the signal, verify that state.pid still
    // belongs to the original container process by comparing starttime. Only block
    // the kill if we positively confirm a different process has claimed the PID.
    //
    // If /proc/<pid>/stat is unreadable (process already gone), fall through to the
    // actual kill() call, which will return ESRCH and be treated as success — the
    // container's intent (stop it) is fulfilled. This preserves the short-lived
    // container case (pidfile.t: `true` exits before kill is called).
    if let Some(stored) = state.pid_start_time {
        if let Some(current) = read_pid_start_time(state.pid) {
            if current != stored {
                log::warn!(
                    "container '{}': PID {} reused before kill (stored starttime={}, \
                     current={}); refusing to signal unrelated process",
                    id,
                    state.pid,
                    stored,
                    current
                );
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!(
                        "container '{}' is stopped (PID {} reused by another process)",
                        id, state.pid
                    ),
                ));
            }
        }
        // None: process gone → kill() will return ESRCH → treated as success below.
    }

    // Accept signal as name (with or without "SIG" prefix) or number.
    let sig: i32 = match signal.to_ascii_uppercase().trim_start_matches("SIG") {
        "HUP" | "1" => libc::SIGHUP,
        "INT" | "2" => libc::SIGINT,
        "QUIT" | "3" => libc::SIGQUIT,
        "ILL" | "4" => libc::SIGILL,
        "TRAP" | "5" => libc::SIGTRAP,
        "ABRT" | "6" => libc::SIGABRT,
        "BUS" | "7" => libc::SIGBUS,
        "FPE" | "8" => libc::SIGFPE,
        "KILL" | "9" => libc::SIGKILL,
        "USR1" | "10" => libc::SIGUSR1,
        "SEGV" | "11" => libc::SIGSEGV,
        "USR2" | "12" => libc::SIGUSR2,
        "PIPE" | "13" => libc::SIGPIPE,
        "ALRM" | "14" => libc::SIGALRM,
        "TERM" | "15" => libc::SIGTERM,
        "CHLD" | "17" => libc::SIGCHLD,
        "CONT" | "18" => libc::SIGCONT,
        "STOP" | "19" => libc::SIGSTOP,
        "TSTP" | "20" => libc::SIGTSTP,
        "TTIN" | "21" => libc::SIGTTIN,
        "TTOU" | "22" => libc::SIGTTOU,
        "URG" | "23" => libc::SIGURG,
        "XCPU" | "24" => libc::SIGXCPU,
        "XFSZ" | "25" => libc::SIGXFSZ,
        "VTALRM" | "26" => libc::SIGVTALRM,
        "PROF" | "27" => libc::SIGPROF,
        "WINCH" | "28" => libc::SIGWINCH,
        "IO" | "POLL" | "29" => libc::SIGIO,
        "PWR" | "30" => libc::SIGPWR,
        "SYS" | "31" => libc::SIGSYS,
        s => s.parse::<i32>().map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "unknown signal '{}' — use a name (SIGTERM) or number (15)",
                    signal
                ),
            )
        })?,
    };

    // Check if the container process is its own process group leader.
    // When it is, we can kill the entire group (covers child processes like sleep)
    // in addition to the init PID.  This is necessary when busybox ash is PID 1 and
    // has installed a signal trap: ash handles the signal but resumes `wait $!`
    // instead of exiting; killing the group causes background children to die first,
    // which wakes ash from `wait`, allowing the script to finish and the container
    // to stop.
    let pgid = unsafe { libc::getpgid(state.pid) };
    let is_own_pgid = pgid == state.pid;

    log::debug!(
        "OCI kill: id={} state.pid={} pgid={} sig={}",
        id,
        state.pid,
        pgid,
        sig
    );

    // Send to the init PID (required by OCI spec).
    let ret = unsafe { libc::kill(state.pid, sig) };
    if ret != 0 {
        let e = io::Error::last_os_error();
        // ESRCH: process died concurrently between our state check and signal delivery.
        // Treat as success — the container was in running state when we checked, and
        // the intent (stop it) is fulfilled. This handles the race between a very
        // short-lived container and the kill call.
        if e.raw_os_error() != Some(libc::ESRCH) {
            return Err(e);
        }
    }

    // Additionally send to the process group when the container is its own PGID.
    // This is a best-effort: errors are ignored since init already received the signal.
    if is_own_pgid && pgid > 1 {
        unsafe { libc::kill(-pgid, sig) };
    }

    // Also scan /proc for any other processes in the same PID namespace and signal them.
    // This is necessary when the container's init (e.g. busybox ash) runs background jobs:
    // ash may restart wait() after a trap fires, so the shell only exits once all background
    // jobs are also terminated.
    let ns_path = format!("/proc/{}/ns/pid", state.pid);
    if let Ok(target_ns) = fs::read_link(&ns_path) {
        if let Ok(entries) = fs::read_dir("/proc") {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let Some(name_str) = name.to_str() else {
                    continue;
                };
                let Ok(pid) = name_str.parse::<i32>() else {
                    continue;
                };
                if pid == state.pid {
                    continue;
                }
                let pid_ns_path = format!("/proc/{}/ns/pid", pid);
                if let Ok(proc_ns) = fs::read_link(&pid_ns_path) {
                    if proc_ns == target_ns {
                        log::debug!("OCI kill: also signaling pid={} sig={} (same ns)", pid, sig);
                        unsafe { libc::kill(pid, sig) };
                    }
                }
            }
        }
    }

    Ok(())
}

/// `remora delete <id>` — remove state dir after container has stopped.
pub fn cmd_delete_force(id: &str) -> io::Result<()> {
    // Force-delete: kill the container first if it's still running.
    if let Ok(state) = read_state(id) {
        let alive = unsafe { libc::kill(state.pid, 0) } == 0;
        if alive {
            unsafe { libc::kill(state.pid, libc::SIGKILL) };
            // Brief wait for the process to actually die.
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            loop {
                std::thread::sleep(std::time::Duration::from_millis(50));
                if unsafe { libc::kill(state.pid, 0) } != 0 {
                    break;
                }
                if std::time::Instant::now() >= deadline {
                    break;
                }
            }
        }
    }
    cmd_delete(id)
}

pub fn cmd_delete(id: &str) -> io::Result<()> {
    let state = read_state(id)?;

    // Allow delete if process is gone or is a zombie.
    // Zombies pass kill(pid,0) but are effectively stopped — check /proc/stat for 'Z'.
    let alive = unsafe { libc::kill(state.pid, 0) } == 0;
    let is_zombie = alive && is_zombie_pid(state.pid);
    if alive && !is_zombie {
        // Log the command line of the still-alive process for diagnostics.
        let cmdline = fs::read_to_string(format!("/proc/{}/cmdline", state.pid))
            .unwrap_or_default()
            .replace('\0', " ");
        let status_line = fs::read_to_string(format!("/proc/{}/status", state.pid))
            .unwrap_or_default()
            .lines()
            .find(|l| l.starts_with("State:"))
            .unwrap_or("")
            .to_string();
        log::debug!(
            "OCI delete: id={} state.pid={} alive={} cmdline={:?} status={:?}",
            id,
            state.pid,
            alive,
            cmdline.trim(),
            status_line
        );
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "container '{}' is still running (pid {}); stop it first",
                id, state.pid
            ),
        ));
    } else {
        log::debug!(
            "OCI delete: id={} state.pid={} alive={} zombie={}",
            id,
            state.pid,
            alive,
            is_zombie
        );
    }

    // Load config before removing state dir so we can run poststop hooks.
    let bundle_path = state.bundle.clone();
    fs::remove_dir_all(state_dir(id))?;

    // Run poststop hooks (best-effort — state dir is already gone).
    if let Ok(config) = config_from_bundle(std::path::Path::new(&bundle_path)) {
        if let Some(ref hooks) = config.hooks {
            if !hooks.poststop.is_empty() {
                let stopped_state = OciState {
                    status: "stopped".to_string(),
                    ..state
                };
                let _ = run_hooks(&hooks.poststop, &stopped_state);
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_oci_cap_all_known_names_round_trip() {
        // Every OCI capability name used in Docker's default profile must map to a flag.
        let known = [
            "CAP_CHOWN",
            "CAP_DAC_OVERRIDE",
            "CAP_DAC_READ_SEARCH",
            "CAP_FOWNER",
            "CAP_FSETID",
            "CAP_KILL",
            "CAP_SETGID",
            "CAP_SETUID",
            "CAP_SETPCAP",
            "CAP_LINUX_IMMUTABLE",
            "CAP_NET_BIND_SERVICE",
            "CAP_NET_BROADCAST",
            "CAP_NET_ADMIN",
            "CAP_NET_RAW",
            "CAP_IPC_LOCK",
            "CAP_IPC_OWNER",
            "CAP_SYS_MODULE",
            "CAP_SYS_RAWIO",
            "CAP_SYS_CHROOT",
            "CAP_SYS_PTRACE",
            "CAP_SYS_PACCT",
            "CAP_SYS_ADMIN",
            "CAP_SYS_BOOT",
            "CAP_SYS_NICE",
            "CAP_SYS_RESOURCE",
            "CAP_SYS_TIME",
            "CAP_SYS_TTY_CONFIG",
            "CAP_MKNOD",
            "CAP_LEASE",
            "CAP_AUDIT_WRITE",
            "CAP_AUDIT_CONTROL",
            "CAP_SETFCAP",
            "CAP_MAC_OVERRIDE",
            "CAP_MAC_ADMIN",
            "CAP_SYSLOG",
            "CAP_WAKE_ALARM",
            "CAP_BLOCK_SUSPEND",
            "CAP_AUDIT_READ",
            "CAP_PERFMON",
            "CAP_BPF",
            "CAP_CHECKPOINT_RESTORE",
        ];
        for name in &known {
            assert!(
                oci_cap_to_flag(name).is_some(),
                "oci_cap_to_flag returned None for {}",
                name
            );
        }
    }

    #[test]
    fn test_oci_cap_without_prefix() {
        // OCI bundles may omit the CAP_ prefix.
        assert!(oci_cap_to_flag("CHOWN").is_some());
        assert!(oci_cap_to_flag("NET_ADMIN").is_some());
        assert!(oci_cap_to_flag("BPF").is_some());
        assert!(oci_cap_to_flag("UNKNOWN_CAP").is_none());
    }

    #[test]
    fn test_oci_signal_names() {
        // The signal parsing in cmd_kill must accept names from runtime-tools.
        let cases: &[(&str, i32)] = &[
            ("SIGTERM", libc::SIGTERM),
            ("TERM", libc::SIGTERM),
            ("15", libc::SIGTERM),
            ("SIGKILL", libc::SIGKILL),
            ("9", libc::SIGKILL),
            ("SIGHUP", libc::SIGHUP),
            ("SIGWINCH", libc::SIGWINCH),
            ("SIGCHLD", libc::SIGCHLD),
            ("SIGCONT", libc::SIGCONT),
            ("SIGSTOP", libc::SIGSTOP),
            ("SIGQUIT", libc::SIGQUIT),
            ("SIGUSR1", libc::SIGUSR1),
            ("SIGUSR2", libc::SIGUSR2),
            ("SIGPIPE", libc::SIGPIPE),
            ("SIGALRM", libc::SIGALRM),
            ("SIGSEGV", libc::SIGSEGV),
            ("SIGABRT", libc::SIGABRT),
            ("SIGSYS", libc::SIGSYS),
        ];
        for (name, expected) in cases {
            let got = match name.to_ascii_uppercase().trim_start_matches("SIG") {
                "HUP" | "1" => libc::SIGHUP,
                "INT" | "2" => libc::SIGINT,
                "QUIT" | "3" => libc::SIGQUIT,
                "ILL" | "4" => libc::SIGILL,
                "TRAP" | "5" => libc::SIGTRAP,
                "ABRT" | "6" => libc::SIGABRT,
                "BUS" | "7" => libc::SIGBUS,
                "FPE" | "8" => libc::SIGFPE,
                "KILL" | "9" => libc::SIGKILL,
                "USR1" | "10" => libc::SIGUSR1,
                "SEGV" | "11" => libc::SIGSEGV,
                "USR2" | "12" => libc::SIGUSR2,
                "PIPE" | "13" => libc::SIGPIPE,
                "ALRM" | "14" => libc::SIGALRM,
                "TERM" | "15" => libc::SIGTERM,
                "CHLD" | "17" => libc::SIGCHLD,
                "CONT" | "18" => libc::SIGCONT,
                "STOP" | "19" => libc::SIGSTOP,
                "TSTP" | "20" => libc::SIGTSTP,
                "TTIN" | "21" => libc::SIGTTIN,
                "TTOU" | "22" => libc::SIGTTOU,
                "URG" | "23" => libc::SIGURG,
                "XCPU" | "24" => libc::SIGXCPU,
                "XFSZ" | "25" => libc::SIGXFSZ,
                "VTALRM" | "26" => libc::SIGVTALRM,
                "PROF" | "27" => libc::SIGPROF,
                "WINCH" | "28" => libc::SIGWINCH,
                "IO" | "POLL" | "29" => libc::SIGIO,
                "PWR" | "30" => libc::SIGPWR,
                "SYS" | "31" => libc::SIGSYS,
                s => s.parse::<i32>().unwrap_or(-1),
            };
            assert_eq!(
                got, *expected,
                "signal '{}' mapped to {} not {}",
                name, got, expected
            );
        }
    }
}
