//! CLI shared types, helpers, and state management for `remora run/ps/stop/rm/logs`.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

pub mod logs;
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
    PathBuf::from("/run/remora/containers")
}

pub fn container_dir(name: &str) -> PathBuf {
    containers_dir().join(name)
}

pub fn state_path(name: &str) -> PathBuf {
    container_dir(name).join("state.json")
}

pub fn rootfs_store() -> PathBuf {
    PathBuf::from("/var/lib/remora/rootfs")
}

/// Resolve a named rootfs to its absolute path.
pub fn rootfs_path(name: &str) -> std::io::Result<PathBuf> {
    let link = rootfs_store().join(name);
    // If the path is already a directory (not a symlink), use it directly.
    if link.is_dir() && !link.is_symlink() {
        return Ok(link);
    }
    std::fs::read_link(&link).map_err(|e| {
        std::io::Error::other(format!("rootfs '{}' not found ({}): {}", name, link.display(), e))
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
    let json = serde_json::to_string_pretty(state)
        .map_err(|e| std::io::Error::other(e.to_string()))?;
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

const COUNTER_FILE: &str = "/var/lib/remora/container_counter";

pub fn generate_name() -> std::io::Result<String> {
    std::fs::create_dir_all("/var/lib/remora")?;
    let n: u64 = std::fs::read_to_string(COUNTER_FILE)
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0);
    let next = n + 1;
    std::fs::write(COUNTER_FILE, next.to_string())?;
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
pub fn parse_user(s: &str) -> Result<(u32, Option<u32>), String> {
    if let Some((u, g)) = s.split_once(':') {
        let uid = u.trim().parse::<u32>().map_err(|e| format!("invalid uid '{}': {}", u, e))?;
        let gid = g.trim().parse::<u32>().map_err(|e| format!("invalid gid '{}': {}", g, e))?;
        Ok((uid, Some(gid)))
    } else {
        let uid = s.trim().parse::<u32>().map_err(|e| format!("invalid uid '{}': {}", s, e))?;
        Ok((uid, None))
    }
}

/// Parse --ulimit "nofile=1024:2048" → (resource_int, soft, hard).
/// Returns the libc resource constant.
pub fn parse_ulimit(s: &str) -> Result<(libc::__rlimit_resource_t, libc::rlim_t, libc::rlim_t), String> {
    let (name, limits) = s.split_once('=').ok_or_else(|| format!("invalid --ulimit '{}': expected RESOURCE=SOFT:HARD", s))?;
    let (soft_s, hard_s) = limits.split_once(':').ok_or_else(|| format!("invalid --ulimit '{}': expected SOFT:HARD", s))?;
    let soft = soft_s.trim().parse::<libc::rlim_t>().map_err(|e| format!("invalid soft limit '{}': {}", soft_s, e))?;
    let hard = hard_s.trim().parse::<libc::rlim_t>().map_err(|e| format!("invalid hard limit '{}': {}", hard_s, e))?;
    let resource = match name.trim().to_ascii_lowercase().as_str() {
        "nofile" | "openfiles" => libc::RLIMIT_NOFILE,
        "nproc"  | "maxproc"  => libc::RLIMIT_NPROC,
        "as"     | "vmem"     => libc::RLIMIT_AS,
        "cpu"                 => libc::RLIMIT_CPU,
        "fsize"               => libc::RLIMIT_FSIZE,
        "memlock"             => libc::RLIMIT_MEMLOCK,
        "stack"               => libc::RLIMIT_STACK,
        "core"                => libc::RLIMIT_CORE,
        "rss"                 => libc::RLIMIT_RSS,
        "msgqueue"            => libc::RLIMIT_MSGQUEUE,
        "nice"                => libc::RLIMIT_NICE,
        "rtprio"              => libc::RLIMIT_RTPRIO,
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
    if parts.len() != 2 { return None; }
    let date_parts: Vec<u64> = parts[0].split('-').filter_map(|x| x.parse().ok()).collect();
    let time_parts: Vec<u64> = parts[1].split(':').filter_map(|x| x.parse().ok()).collect();
    if date_parts.len() != 3 || time_parts.len() != 3 { return None; }
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
