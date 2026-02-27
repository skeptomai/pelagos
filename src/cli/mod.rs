//! CLI shared types, helpers, and state management for `remora run/ps/stop/rm/logs`.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

pub mod auth;
pub mod build;
pub mod cleanup;
pub mod compose;
pub mod exec;
pub mod image;
pub mod logs;
pub mod network;
pub mod ps;
pub mod rm;
pub mod rootfs;
pub mod run;
pub mod stop;
pub mod volume;

// ---------------------------------------------------------------------------
// Paths
// ---------------------------------------------------------------------------

pub fn containers_dir() -> PathBuf {
    remora::paths::containers_dir()
}

pub fn container_dir(name: &str) -> PathBuf {
    containers_dir().join(name)
}

pub fn state_path(name: &str) -> PathBuf {
    container_dir(name).join("state.json")
}

pub fn rootfs_store() -> PathBuf {
    remora::paths::rootfs_store_dir()
}

/// Resolve a named rootfs to its absolute path.
///
/// Accepts either a filesystem path (absolute or relative) that points to an
/// existing directory, or a name registered in the rootfs store
/// (`/var/lib/remora/rootfs/<name>` — a directory or symlink).
pub fn rootfs_path(name: &str) -> std::io::Result<PathBuf> {
    // If the argument is a path to an existing directory, use it directly.
    let as_path = PathBuf::from(name);
    if as_path.is_dir() {
        return as_path.canonicalize();
    }
    // Otherwise look it up in the rootfs store.
    let link = rootfs_store().join(name);
    if link.is_dir() && !link.is_symlink() {
        return Ok(link);
    }
    std::fs::read_link(&link).map_err(|e| {
        std::io::Error::other(format!(
            "rootfs '{}' not found ({}): {}",
            name,
            link.display(),
            e
        ))
    })
}

// ---------------------------------------------------------------------------
// Container state
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ContainerStatus {
    Running,
    Exited,
}

impl std::fmt::Display for ContainerStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ContainerStatus::Running => write!(f, "running"),
            ContainerStatus::Exited => write!(f, "exited"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContainerState {
    pub name: String,
    pub rootfs: String,
    pub status: ContainerStatus,
    /// PID of the container process (0 if not yet started).
    pub pid: i32,
    /// PID of the watcher process (detached mode only).
    pub watcher_pid: i32,
    /// ISO 8601 timestamp of container start.
    pub started_at: String,
    /// Exit code, populated when status == exited.
    pub exit_code: Option<i32>,
    /// The command run inside the container.
    pub command: Vec<String>,
    /// Path to captured stdout log (detached mode only).
    pub stdout_log: Option<String>,
    /// Path to captured stderr log (detached mode only).
    pub stderr_log: Option<String>,
    /// Bridge IP address (e.g. "172.19.0.5"), populated when using bridge networking.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bridge_ip: Option<String>,
    /// Per-network IP addresses: network_name → IP string.
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub network_ips: std::collections::HashMap<String, String>,
}

pub fn now_iso8601() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    // Simple RFC 3339 / ISO 8601 without chrono dependency.
    // Format: YYYY-MM-DDTHH:MM:SSZ
    let s = secs;
    let (y, mo, d, h, mi, sec) = epoch_to_datetime(s);
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z", y, mo, d, h, mi, sec)
}

/// Minimal epoch → (year, month, day, hour, min, sec) without chrono.
fn epoch_to_datetime(epoch: u64) -> (u64, u64, u64, u64, u64, u64) {
    let secs_per_day = 86400u64;
    let days = epoch / secs_per_day;
    let time = epoch % secs_per_day;
    let h = time / 3600;
    let mi = (time % 3600) / 60;
    let s = time % 60;

    // Civil date from days since Unix epoch (Jan 1, 1970).
    // Algorithm: https://howardhinnant.github.io/date_algorithms.html
    let z = days + 719468;
    let era = z / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if mo <= 2 { y + 1 } else { y };
    (y, mo, d, h, mi, s)
}

pub fn write_state(state: &ContainerState) -> std::io::Result<()> {
    let dir = container_dir(&state.name);
    std::fs::create_dir_all(&dir)?;
    let json =
        serde_json::to_string_pretty(state).map_err(|e| std::io::Error::other(e.to_string()))?;
    std::fs::write(state_path(&state.name), json)
}

pub fn read_state(name: &str) -> std::io::Result<ContainerState> {
    let data = std::fs::read_to_string(state_path(name))?;
    serde_json::from_str(&data).map_err(|e| std::io::Error::other(e.to_string()))
}

pub fn list_containers() -> Vec<ContainerState> {
    let dir = containers_dir();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut states = Vec::new();
    for entry in entries.flatten() {
        let state_file = entry.path().join("state.json");
        if let Ok(data) = std::fs::read_to_string(&state_file) {
            if let Ok(s) = serde_json::from_str::<ContainerState>(&data) {
                states.push(s);
            }
        }
    }
    states
}

/// Check if a PID is still alive.
pub fn check_liveness(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }
    unsafe { libc::kill(pid, 0) == 0 }
}

// ---------------------------------------------------------------------------
// Auto-name generation
// ---------------------------------------------------------------------------

pub fn generate_name() -> std::io::Result<String> {
    let counter = remora::paths::counter_file();
    if let Some(parent) = counter.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let n: u64 = std::fs::read_to_string(&counter)
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0);
    let next = n + 1;
    std::fs::write(&counter, next.to_string())?;
    Ok(format!("remora-{}", next))
}

// ---------------------------------------------------------------------------
// Parsing helpers
// ---------------------------------------------------------------------------

/// Parse memory string like "256m", "1g", "512k" → bytes as i64.
pub fn parse_memory(s: &str) -> Result<i64, String> {
    let s = s.trim();
    let (num, mult): (&str, i64) = if let Some(n) = s.strip_suffix(['k', 'K']) {
        (n, 1024)
    } else if let Some(n) = s.strip_suffix(['m', 'M']) {
        (n, 1024 * 1024)
    } else if let Some(n) = s.strip_suffix(['g', 'G']) {
        (n, 1024 * 1024 * 1024)
    } else if let Some(n) = s.strip_suffix(['b', 'B']) {
        (n, 1)
    } else {
        (s, 1)
    };
    num.trim()
        .parse::<i64>()
        .map(|n| n * mult)
        .map_err(|e| format!("invalid memory value '{}': {}", s, e))
}

/// Parse --cpus like "0.5" → (quota_us: i64, period_us: u64).
pub fn parse_cpus(s: &str) -> Result<(i64, u64), String> {
    let cpus: f64 = s
        .trim()
        .parse()
        .map_err(|e| format!("invalid --cpus '{}': {}", s, e))?;
    let period_us: u64 = 100_000;
    let quota_us = (cpus * period_us as f64) as i64;
    Ok((quota_us, period_us))
}

/// Parse --user "1000" or "1000:1000" → (uid, Option<gid>).
/// Resolves symbolic names against the host /etc/passwd only.
/// Use `parse_user_in_layers` when an image rootfs is available.
pub fn parse_user(s: &str) -> Result<(u32, Option<u32>), String> {
    if let Some((u, g)) = s.split_once(':') {
        let uid = resolve_uid(u.trim())?;
        let gid = resolve_gid(g.trim())?;
        Ok((uid, Some(gid)))
    } else {
        let uid = resolve_uid(s.trim())?;
        Ok((uid, None))
    }
}

/// Like `parse_user`, but for image-config users: resolves symbolic names
/// from the container's own layer stack (/etc/passwd inside the image) before
/// falling back to the host. When a bare username is given, also returns the
/// primary gid from the image's /etc/passwd (matching OCI spec behaviour).
pub fn parse_user_in_layers(
    s: &str,
    layer_dirs: &[std::path::PathBuf],
) -> Result<(u32, Option<u32>), String> {
    if let Some((u, g)) = s.split_once(':') {
        let uid = resolve_uid_in_layers(u.trim(), layer_dirs)?;
        let gid = resolve_gid_in_layers(g.trim(), layer_dirs)?;
        Ok((uid, Some(gid)))
    } else if let Ok(n) = s.trim().parse::<u32>() {
        Ok((n, None))
    } else {
        lookup_user_in_layers(s.trim(), layer_dirs)
    }
}

/// Look up a username in the container's layer stack and return (uid, gid).
fn lookup_user_in_layers(
    name: &str,
    layer_dirs: &[std::path::PathBuf],
) -> Result<(u32, Option<u32>), String> {
    // Layers are applied last-wins, so search in reverse.
    for layer_dir in layer_dirs.iter().rev() {
        let passwd_path = layer_dir.join("etc/passwd");
        if let Ok(contents) = std::fs::read_to_string(&passwd_path) {
            for line in contents.lines() {
                let fields: Vec<&str> = line.split(':').collect();
                // passwd format: name:password:uid:gid:...
                if fields.len() >= 4 && fields[0] == name {
                    if let (Ok(uid), Ok(gid)) = (fields[2].parse::<u32>(), fields[3].parse::<u32>())
                    {
                        return Ok((uid, Some(gid)));
                    }
                }
            }
        }
    }
    // Fall back to host /etc/passwd.
    use std::ffi::CString;
    let cname = CString::new(name).map_err(|_| format!("invalid user name: {}", name))?;
    let pw = unsafe { libc::getpwnam(cname.as_ptr()) };
    if pw.is_null() {
        Err(format!(
            "unknown user '{}': not found in container or host /etc/passwd",
            name
        ))
    } else {
        Ok(unsafe { ((*pw).pw_uid, Some((*pw).pw_gid)) })
    }
}

fn resolve_uid_in_layers(s: &str, layer_dirs: &[std::path::PathBuf]) -> Result<u32, String> {
    if let Ok(n) = s.parse::<u32>() {
        return Ok(n);
    }
    for layer_dir in layer_dirs.iter().rev() {
        let passwd_path = layer_dir.join("etc/passwd");
        if let Ok(contents) = std::fs::read_to_string(&passwd_path) {
            for line in contents.lines() {
                let fields: Vec<&str> = line.split(':').collect();
                if fields.len() >= 3 && fields[0] == s {
                    if let Ok(uid) = fields[2].parse::<u32>() {
                        return Ok(uid);
                    }
                }
            }
        }
    }
    use std::ffi::CString;
    let cname = CString::new(s).map_err(|_| format!("invalid user name: {}", s))?;
    let pw = unsafe { libc::getpwnam(cname.as_ptr()) };
    if pw.is_null() {
        Err(format!(
            "unknown user '{}': not found in container or host /etc/passwd",
            s
        ))
    } else {
        Ok(unsafe { (*pw).pw_uid })
    }
}

fn resolve_gid_in_layers(s: &str, layer_dirs: &[std::path::PathBuf]) -> Result<u32, String> {
    if let Ok(n) = s.parse::<u32>() {
        return Ok(n);
    }
    for layer_dir in layer_dirs.iter().rev() {
        let group_path = layer_dir.join("etc/group");
        if let Ok(contents) = std::fs::read_to_string(&group_path) {
            for line in contents.lines() {
                let fields: Vec<&str> = line.split(':').collect();
                // group format: name:password:gid:...
                if fields.len() >= 3 && fields[0] == s {
                    if let Ok(gid) = fields[2].parse::<u32>() {
                        return Ok(gid);
                    }
                }
            }
        }
    }
    use std::ffi::CString;
    let cname = CString::new(s).map_err(|_| format!("invalid group name: {}", s))?;
    let gr = unsafe { libc::getgrnam(cname.as_ptr()) };
    if gr.is_null() {
        Err(format!(
            "unknown group '{}': not found in container or host /etc/group",
            s
        ))
    } else {
        Ok(unsafe { (*gr).gr_gid })
    }
}

fn resolve_uid(s: &str) -> Result<u32, String> {
    if let Ok(n) = s.parse::<u32>() {
        return Ok(n);
    }
    // Symbolic name: look up via getpwnam.
    use std::ffi::CString;
    let name = CString::new(s).map_err(|_| format!("invalid user name: {}", s))?;
    let pw = unsafe { libc::getpwnam(name.as_ptr()) };
    if pw.is_null() {
        Err(format!("unknown user '{}': not found in /etc/passwd", s))
    } else {
        Ok(unsafe { (*pw).pw_uid })
    }
}

fn resolve_gid(s: &str) -> Result<u32, String> {
    if let Ok(n) = s.parse::<u32>() {
        return Ok(n);
    }
    use std::ffi::CString;
    let name = CString::new(s).map_err(|_| format!("invalid group name: {}", s))?;
    let gr = unsafe { libc::getgrnam(name.as_ptr()) };
    if gr.is_null() {
        Err(format!("unknown group '{}': not found in /etc/group", s))
    } else {
        Ok(unsafe { (*gr).gr_gid })
    }
}

/// Parse --ulimit "nofile=1024:2048" → (resource_int, soft, hard).
/// Returns the libc resource constant.
pub fn parse_ulimit(
    s: &str,
) -> Result<
    (
        remora::container::RlimitResource,
        libc::rlim_t,
        libc::rlim_t,
    ),
    String,
> {
    let (name, limits) = s
        .split_once('=')
        .ok_or_else(|| format!("invalid --ulimit '{}': expected RESOURCE=SOFT:HARD", s))?;
    let (soft_s, hard_s) = limits
        .split_once(':')
        .ok_or_else(|| format!("invalid --ulimit '{}': expected SOFT:HARD", s))?;
    let soft = soft_s
        .trim()
        .parse::<libc::rlim_t>()
        .map_err(|e| format!("invalid soft limit '{}': {}", soft_s, e))?;
    let hard = hard_s
        .trim()
        .parse::<libc::rlim_t>()
        .map_err(|e| format!("invalid hard limit '{}': {}", hard_s, e))?;
    let resource = match name.trim().to_ascii_lowercase().as_str() {
        "nofile" | "openfiles" => libc::RLIMIT_NOFILE,
        "nproc" | "maxproc" => libc::RLIMIT_NPROC,
        "as" | "vmem" => libc::RLIMIT_AS,
        "cpu" => libc::RLIMIT_CPU,
        "fsize" => libc::RLIMIT_FSIZE,
        "memlock" => libc::RLIMIT_MEMLOCK,
        "stack" => libc::RLIMIT_STACK,
        "core" => libc::RLIMIT_CORE,
        "rss" => libc::RLIMIT_RSS,
        "msgqueue" => libc::RLIMIT_MSGQUEUE,
        "nice" => libc::RLIMIT_NICE,
        "rtprio" => libc::RLIMIT_RTPRIO,
        other => return Err(format!("unknown ulimit resource '{}'", other)),
    };
    Ok((resource, soft, hard))
}

/// Parse a capability name like "CAP_NET_RAW" or "NET_RAW" to Capability bitflag.
pub fn parse_capability(s: &str) -> Result<remora::container::Capability, String> {
    use remora::container::Capability;
    let name = s.trim().to_ascii_uppercase();
    let name = name.strip_prefix("CAP_").unwrap_or(&name);
    match name {
        "CHOWN"           => Ok(Capability::CHOWN),
        "DAC_OVERRIDE"    => Ok(Capability::DAC_OVERRIDE),
        "FOWNER"          => Ok(Capability::FOWNER),
        "SETGID"          => Ok(Capability::SETGID),
        "SETUID"          => Ok(Capability::SETUID),
        "NET_BIND_SERVICE"=> Ok(Capability::NET_BIND_SERVICE),
        "NET_RAW"         => Ok(Capability::NET_RAW),
        "SYS_CHROOT"      => Ok(Capability::SYS_CHROOT),
        "SYS_ADMIN"       => Ok(Capability::SYS_ADMIN),
        "SYS_PTRACE"      => Ok(Capability::SYS_PTRACE),
        other => Err(format!("unknown capability '{}' (supported: CHOWN, DAC_OVERRIDE, FOWNER, SETGID, SETUID, NET_BIND_SERVICE, NET_RAW, SYS_CHROOT, SYS_ADMIN, SYS_PTRACE)", other)),
    }
}

/// Format a duration in seconds as a human-readable "X minutes ago" string.
pub fn format_age(started_at_iso: &str) -> String {
    // Parse back to epoch seconds (best-effort).
    if let Some(secs) = iso8601_to_epoch(started_at_iso) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let diff = now.saturating_sub(secs);
        if diff < 60 {
            return format!("{} seconds ago", diff);
        } else if diff < 3600 {
            return format!("{} minutes ago", diff / 60);
        } else if diff < 86400 {
            return format!("{} hours ago", diff / 3600);
        } else {
            return format!("{} days ago", diff / 86400);
        }
    }
    started_at_iso.to_string()
}

fn iso8601_to_epoch(s: &str) -> Option<u64> {
    // Expect: YYYY-MM-DDTHH:MM:SSZ
    let s = s.trim_end_matches('Z');
    let parts: Vec<&str> = s.splitn(2, 'T').collect();
    if parts.len() != 2 {
        return None;
    }
    let date_parts: Vec<u64> = parts[0].split('-').filter_map(|x| x.parse().ok()).collect();
    let time_parts: Vec<u64> = parts[1].split(':').filter_map(|x| x.parse().ok()).collect();
    if date_parts.len() != 3 || time_parts.len() != 3 {
        return None;
    }
    let (y, mo, d) = (date_parts[0], date_parts[1], date_parts[2]);
    let (h, mi, s) = (time_parts[0], time_parts[1], time_parts[2]);
    // Days since epoch using the inverse of our epoch_to_datetime algorithm.
    let y = if mo <= 2 { y - 1 } else { y };
    let era = y / 400;
    let yoe = y - era * 400;
    let mo_adj = if mo > 2 { mo - 3 } else { mo + 9 };
    let doy = (153 * mo_adj + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe;
    let epoch_days = days.checked_sub(719468)?;
    Some(epoch_days * 86400 + h * 3600 + mi * 60 + s)
}

/// Parse a list of capability name strings into a `Capability` bitflag mask.
///
/// Accepts bare names (`"net-raw"`, `"NET_RAW"`) or prefixed (`"CAP_NET_RAW"`),
/// case-insensitive, with `-` or `_` as separators.  Unrecognised names are
/// logged at warn level and skipped.
pub fn parse_capability_mask(names: &[String]) -> remora::container::Capability {
    use remora::container::Capability;
    let mut mask = Capability::empty();
    for name in names {
        let normalised = name
            .to_uppercase()
            .replace('-', "_")
            .trim_start_matches("CAP_")
            .to_string();
        match Capability::from_name(&normalised) {
            Some(cap) => mask |= cap,
            None => log::warn!("cap-add: unknown capability '{}' — skipping", name),
        }
    }
    mask
}

#[cfg(test)]
mod tests {
    use super::*;

    /// rootfs_path should accept an existing filesystem directory path and
    /// return its canonicalized form, without requiring it to be registered
    /// in the rootfs store.
    #[test]
    fn test_rootfs_path_accepts_filesystem_dir() {
        // /tmp always exists on Linux.
        let result = rootfs_path("/tmp").expect("rootfs_path should accept /tmp");
        assert!(result.is_absolute(), "result should be absolute");
        assert!(result.is_dir(), "result should point to a directory");
    }

    /// rootfs_path should fail for a name that is neither an existing directory
    /// nor a registered rootfs.
    #[test]
    fn test_rootfs_path_rejects_nonexistent() {
        let result = rootfs_path("nonexistent-rootfs-xyz-12345");
        assert!(
            result.is_err(),
            "rootfs_path should fail for nonexistent name"
        );
    }
}
