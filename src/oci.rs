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
    pub process: OciProcess,
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
}

// ---------------------------------------------------------------------------
// linux.resources
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct OciResources {
    pub memory: Option<OciMemoryResources>,
    pub cpu: Option<OciCpuResources>,
    pub pids: Option<OciPidsResources>,
}

#[derive(Debug, Deserialize)]
pub struct OciMemoryResources {
    pub limit: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct OciCpuResources {
    pub shares: Option<u64>,
    pub quota: Option<i64>,
    pub period: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub struct OciPidsResources {
    pub limit: Option<i64>,
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
#[serde(rename_all = "camelCase")]
pub struct OciIdMapping {
    pub host_id: u32,
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
    /// Bridge IP address, populated when using bridge networking.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bridge_ip: Option<String>,
}

// ---------------------------------------------------------------------------
// Path helpers
// ---------------------------------------------------------------------------

pub fn state_dir(id: &str) -> PathBuf {
    PathBuf::from(format!("/run/remora/{}", id))
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
    let content =
        serde_json::to_vec_pretty(state).map_err(io::Error::other)?;
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
    // Strip optional "CAP_" prefix for case-insensitive matching.
    let n = name.strip_prefix("CAP_").unwrap_or(name);
    match n {
        "CHOWN" => Some(Capability::CHOWN),
        "DAC_OVERRIDE" => Some(Capability::DAC_OVERRIDE),
        "FOWNER" => Some(Capability::FOWNER),
        "FSETID" => Some(Capability::FSETID),
        "KILL" => Some(Capability::KILL),
        "SETGID" => Some(Capability::SETGID),
        "SETUID" => Some(Capability::SETUID),
        "NET_BIND_SERVICE" => Some(Capability::NET_BIND_SERVICE),
        "NET_RAW" => Some(Capability::NET_RAW),
        "SYS_CHROOT" => Some(Capability::SYS_CHROOT),
        "SYS_ADMIN" => Some(Capability::SYS_ADMIN),
        "SYS_PTRACE" => Some(Capability::SYS_PTRACE),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Build a container::Command from OCI config
// ---------------------------------------------------------------------------

pub fn build_command(config: &OciConfig, bundle: &Path) -> io::Result<crate::container::Command> {
    use crate::container::{Command, Namespace};

    let root_path = bundle.join(&config.root.path);
    let exe = config
        .process
        .args
        .first()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "process.args is empty"))?;

    let mut cmd = Command::new(exe)
        .env_clear()
        .with_chroot(&root_path)
        .with_cwd(&config.process.cwd)
        .stdout(crate::container::Stdio::Inherit)
        .stderr(crate::container::Stdio::Inherit);

    // Remaining args (exe is args[0])
    if config.process.args.len() > 1 {
        let rest: Vec<&str> = config.process.args[1..]
            .iter()
            .map(|s| s.as_str())
            .collect();
        cmd = cmd.args(&rest);
    }

    // Environment
    for entry in &config.process.env {
        if let Some(eq) = entry.find('=') {
            cmd = cmd.env(&entry[..eq], &entry[eq + 1..]);
        } else {
            cmd = cmd.env(entry, "");
        }
    }

    // User (uid/gid)
    if let Some(ref user) = config.process.user {
        cmd = cmd.with_uid(user.uid).with_gid(user.gid);
    }

    // Security flags
    if config.process.no_new_privileges {
        cmd = cmd.with_no_new_privileges(true);
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

        // Mount proc automatically when a mount namespace is requested
        let has_mount_ns = linux
            .namespaces
            .iter()
            .any(|n| n.ns_type == "mount" && n.path.is_none());
        if has_mount_ns {
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
    if let Some(ref caps) = config.process.capabilities {
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
            if let Some(ref mem) = res.memory {
                if let Some(limit) = mem.limit {
                    if limit > 0 {
                        cmd = cmd.with_cgroup_memory(limit);
                    }
                }
            }
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
            }
            if let Some(ref pids) = res.pids {
                if let Some(limit) = pids.limit {
                    if limit > 0 {
                        cmd = cmd.with_cgroup_pids_limit(limit as u64);
                    }
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
    }

    // process.rlimits
    for rl in &config.process.rlimits {
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

    // OCI mounts (processed in order)
    for mount in &config.mounts {
        let dest = &mount.destination;
        let is_ro = mount.options.iter().any(|o| o == "ro" || o == "readonly");
        let mount_type = mount.mount_type.as_deref().unwrap_or("bind");

        match mount_type {
            "tmpfs" => {
                let opts: Vec<&str> = mount.options.iter().map(|s| s.as_str()).collect();
                cmd = cmd.with_tmpfs(dest, &opts.join(","));
            }
            _ => {
                // bind mount
                if let Some(ref source) = mount.source {
                    if is_ro {
                        cmd = cmd.with_bind_mount_ro(source, dest);
                    } else {
                        cmd = cmd.with_bind_mount(source, dest);
                    }
                }
            }
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

    let state_json =
        serde_json::to_vec(state).map_err(io::Error::other)?;

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
                            return Err(io::Error::other(
                                format!("hook {} exited with status {}", hook.path, status),
                            ));
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
                return Err(io::Error::other(
                    format!("hook {} exited with status {}", hook.path, status),
                ));
            }
        }
    }

    Ok(())
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
fn find_descendant_pid(ancestor_pid: i32) -> Option<i32> {
    // Retry a few times in case /proc is momentarily inconsistent.
    for _ in 0..5 {
        if let Some(pid) = find_descendant_pid_once(ancestor_pid) {
            return Some(pid);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    None
}

fn find_descendant_pid_once(ancestor_pid: i32) -> Option<i32> {
    let mut current = ancestor_pid;
    loop {
        let child = find_child_of(current)?;
        // If this child has no further children, it's the leaf (container).
        if find_child_of(child).is_none() {
            return Some(child);
        }
        current = child;
    }
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

/// `remora create <id> <bundle>` — set up container, suspend before exec.
///
/// Forks a shim that calls `command.spawn()`. The container's pre_exec writes
/// its PID to a ready pipe (signalling "created"), then blocks on accept().
/// The parent reads the PID, writes state.json, and exits. The shim is orphaned
/// and waits for the container; `remora start` later unblocks it.
pub fn cmd_create(id: &str, bundle_path: &Path) -> io::Result<()> {
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
    let listen_fd = create_listen_socket(&sock_path).inspect_err(|_e| {
        unsafe {
            libc::close(ready_r);
            libc::close(ready_w);
        }
    })?;

    // Build the container command with OCI sync hooks.
    let command = match build_command(&config, &bundle) {
        Ok(c) => c.with_oci_sync(ready_w, listen_fd),
        Err(e) => {
            unsafe {
                libc::close(ready_r);
                libc::close(ready_w);
                libc::close(listen_fd);
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
            }
            let _ = fs::remove_dir_all(&dir);
            Err(io::Error::last_os_error())
        }
        0 => {
            // SHIM: detach from the parent's stdio so that the parent's
            // `output()` / pipe can receive EOF as soon as the parent exits.
            // Without this, the shim (and any grandchild) hold the write
            // ends of the test's stdout/stderr pipes open indefinitely,
            // causing `run_remora` to hang waiting for EOF.
            unsafe {
                libc::close(ready_r);
                let dev_null = libc::open(
                    c"/dev/null".as_ptr(),
                    libc::O_RDWR,
                    0,
                );
                if dev_null >= 0 {
                    libc::dup2(dev_null, 0);
                    libc::dup2(dev_null, 1);
                    libc::dup2(dev_null, 2);
                    if dev_null > 2 {
                        libc::close(dev_null);
                    }
                }
            }
            let mut child = match command.spawn() {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("remora: create: spawn failed: {}", e);
                    unsafe { libc::_exit(1) };
                }
            };
            // Container exec'd. Wait for it, then exit (shim is orphaned at this point).
            child.wait().ok();
            unsafe { libc::_exit(0) };
        }
        shim_pid => {
            // PARENT: close the write ends (child has them).
            unsafe { libc::close(ready_w) };
            unsafe { libc::close(listen_fd) };

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
            // With a PID namespace + double-fork, the container sees itself as
            // PID 1 (namespace-local), which is useless on the host.
            //
            // When the pipe PID looks namespace-local (<=1), walk the shim's
            // process tree via /proc to find the real host-visible PID.
            // Process tree: shim → intermediate (waitpid loop) → container.
            let pipe_pid = i32::from_ne_bytes(pid_buf);
            let container_pid = if pipe_pid <= 1 {
                find_descendant_pid(shim_pid).unwrap_or(pipe_pid)
            } else {
                pipe_pid
            };

            // Write state.json with status=created.
            let state = OciState {
                oci_version: "1.0.2".to_string(),
                id: id.to_string(),
                status: "created".to_string(),
                pid: container_pid,
                bundle: bundle.to_string_lossy().into_owned(),
                bridge_ip: None,
            };
            write_state(id, &state)?;

            // Run prestart hooks (host namespace, after container is "created").
            if let Some(ref hooks) = config.hooks {
                if !hooks.prestart.is_empty() {
                    run_hooks(&hooks.prestart, &state)?;
                }
                if !hooks.create_runtime.is_empty() {
                    run_hooks(&hooks.create_runtime, &state)?;
                }
                if !hooks.create_container.is_empty() {
                    run_hooks(&hooks.create_container, &state)?;
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

/// `remora state <id>` — print container state JSON to stdout.
pub fn cmd_state(id: &str) -> io::Result<()> {
    let mut state = read_state(id)?;

    // Determine actual liveness via kill(pid, 0).
    if state.status == "created" || state.status == "running" {
        let alive = unsafe { libc::kill(state.pid, 0) } == 0;
        if !alive {
            state.status = "stopped".to_string();
        }
    }

    let json = serde_json::to_string_pretty(&state)
        .map_err(io::Error::other)?;
    println!("{}", json);
    Ok(())
}

/// `remora kill <id> <signal>` — send a signal to the container process.
pub fn cmd_kill(id: &str, signal: &str) -> io::Result<()> {
    let state = read_state(id)?;

    let sig: i32 = match signal {
        "SIGTERM" | "TERM" | "15" => libc::SIGTERM,
        "SIGKILL" | "KILL" | "9" => libc::SIGKILL,
        "SIGHUP" | "HUP" | "1" => libc::SIGHUP,
        "SIGINT" | "INT" | "2" => libc::SIGINT,
        "SIGUSR1" | "USR1" | "10" => libc::SIGUSR1,
        "SIGUSR2" | "USR2" | "12" => libc::SIGUSR2,
        s => s.parse::<i32>().map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "unknown signal '{}' — use a name (SIGTERM) or number (15)",
                    s
                ),
            )
        })?,
    };

    let ret = unsafe { libc::kill(state.pid, sig) };
    if ret != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// `remora delete <id>` — remove state dir after container has stopped.
pub fn cmd_delete(id: &str) -> io::Result<()> {
    let state = read_state(id)?;

    // Allow delete if process is gone (stopped) regardless of state.json status.
    let alive = unsafe { libc::kill(state.pid, 0) } == 0;
    if alive {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "container '{}' is still running (pid {}); stop it first",
                id, state.pid
            ),
        ));
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
