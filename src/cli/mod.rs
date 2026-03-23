//! CLI shared types, helpers, and state management for `pelagos run/ps/stop/rm/logs`.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

pub use pelagos::image::HealthConfig;

/// Health status of a container (tracks persistent health monitor state).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum HealthStatus {
    #[default]
    None,
    Starting,
    Healthy,
    Unhealthy,
}

pub mod auth;
pub mod build;
pub mod cleanup;
pub mod compose;
pub mod exec;
pub mod health;
pub mod image;
pub mod logs;
pub mod network;
pub mod prune;
pub mod ps;
pub mod relay;
pub mod restart;
pub mod rm;
pub mod rootfs;
pub mod run;
pub mod start;
pub mod stop;
pub mod volume;

// ---------------------------------------------------------------------------
// Paths
// ---------------------------------------------------------------------------

pub fn containers_dir() -> PathBuf {
    pelagos::paths::containers_dir()
}

pub fn container_dir(name: &str) -> PathBuf {
    containers_dir().join(name)
}

pub fn state_path(name: &str) -> PathBuf {
    container_dir(name).join("state.json")
}

pub fn rootfs_store() -> PathBuf {
    pelagos::paths::rootfs_store_dir()
}

/// Resolve a named rootfs to its absolute path.
///
/// Accepts either a filesystem path (absolute or relative) that points to an
/// existing directory, or a name registered in the rootfs store
/// (`/var/lib/pelagos/rootfs/<name>` — a directory or symlink).
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
// Rootless bridge guard
// ---------------------------------------------------------------------------

/// Returns an error message if rootless mode is combined with bridge networking,
/// NAT, or port publishing.  Returns `None` if the combination is valid.
///
/// This is a pure function so it can be tested without side effects.
pub(crate) fn check_rootless_bridge(
    is_rootless: bool,
    network_mode: &pelagos::network::NetworkMode,
    nat: bool,
    has_ports: bool,
) -> Option<String> {
    if !is_rootless {
        return None;
    }
    if network_mode.is_bridge() || nat || has_ports {
        Some(
            "pelagos: bridge networking requires root (CAP_NET_ADMIN / nftables).\n\
             Use --network pasta for rootless internet access, or run with sudo."
                .to_string(),
        )
    } else {
        None
    }
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

/// Saved spawn configuration — enough to restart a container via `pelagos start`.
///
/// Populated by `cmd_run` at container creation and persisted in `state.json`.
/// On restart, `cmd_start` converts this back into `RunArgs` and calls `cmd_run`.
///
/// Note: the overlay writable layer is preserved between runs for non-`--rm`
/// containers.  The upper dir lives in the container state directory
/// (`data_dir/containers/<name>/upper/`) and is reused on `pelagos start`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SpawnConfig {
    /// Image reference used to pull/locate layer dirs (e.g. "ubuntu:22.04").
    /// None for rootfs-only containers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    /// Executable path or name.
    pub exe: String,
    /// Arguments passed to the executable.
    #[serde(default)]
    pub args: Vec<String>,
    /// Environment variables as KEY=VALUE strings.
    #[serde(default)]
    pub env: Vec<String>,
    /// Read-write bind mounts as "host:container" strings.
    #[serde(default)]
    pub bind: Vec<String>,
    /// Read-only bind mounts as "host:container" strings.
    #[serde(default)]
    pub bind_ro: Vec<String>,
    /// Named volume mounts as "vol:container" strings.
    #[serde(default)]
    pub volume: Vec<String>,
    /// Network modes (may include "pasta", "bridge", "bridge:name", "loopback").
    #[serde(default)]
    pub network: Vec<String>,
    /// Port mappings as "HOST:CONTAINER" strings.
    #[serde(default)]
    pub publish: Vec<String>,
    /// Explicit DNS servers.
    #[serde(default)]
    pub dns: Vec<String>,
    /// Working directory inside the container.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_dir: Option<String>,
    /// User as "uid" or "uid:gid" string.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    /// Container hostname.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hostname: Option<String>,
    /// Capabilities to drop.
    #[serde(default)]
    pub cap_drop: Vec<String>,
    /// Capabilities to add.
    #[serde(default)]
    pub cap_add: Vec<String>,
    /// Security options (e.g. "seccomp=unconfined", "apparmor=profile").
    #[serde(default)]
    pub security_opt: Vec<String>,
    /// Whether the rootfs was mounted read-only.
    #[serde(default)]
    pub read_only: bool,
    /// Whether the container should be removed on exit (--rm semantics).
    #[serde(default)]
    pub rm: bool,
    /// Whether NAT (MASQUERADE) was enabled.
    #[serde(default)]
    pub nat: bool,
    /// Container labels as KEY=VALUE strings (e.g. "env=staging").
    #[serde(default)]
    pub labels: Vec<String>,
    /// tmpfs mounts as "path" or "path:options" strings.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tmpfs: Vec<String>,
}

/// Persisted state for a running or exited container.
///
/// **Stable public interface.** The JSON serialisation of this struct is consumed by
/// `pelagos-ui`, `pelagos-mac` (vsock protocol), and external tooling. Do not rename
/// or remove fields without a deprecation cycle and major version bump. New optional
/// fields (`#[serde(default, skip_serializing_if = ...)]`) may be added freely.
///
/// Key serde invariants:
/// - [`ContainerStatus`] serialises as lowercase: `"running"` | `"exited"`
/// - [`HealthStatus`] serialises as lowercase: `"starting"` | `"healthy"` | `"unhealthy"` | `"none"`
/// - Optional/empty fields use `skip_serializing_if` — absent in JSON when empty, not `null`
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
    /// Current health status (set by the health monitor thread).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub health: Option<HealthStatus>,
    /// Health check configuration (from image HEALTHCHECK instruction).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub health_config: Option<HealthConfig>,
    /// Saved spawn configuration for `pelagos start` restarts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spawn_config: Option<SpawnConfig>,
    /// Container labels as KEY=VALUE strings.
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub labels: std::collections::HashMap<String, String>,
    /// Inode of the container's mount namespace (`/proc/<pid>/ns/mnt`) at creation
    /// time.  Used by `pelagos exec` to detect PID reuse: if the current inode
    /// doesn't match the stored one the original container has exited and the PID
    /// has been recycled by an unrelated process.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mnt_ns_inode: Option<u64>,
    /// Persisted writable overlay upper dir for this container.
    ///
    /// Set for non-`--rm` containers; `pelagos start` passes this back as the
    /// overlay upper dir so filesystem changes survive stop/start cycles.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upper_dir: Option<std::path::PathBuf>,
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

/// Returns true if a pelagos container state file exists for `name`.
/// Used to distinguish container-restart from OCI lifecycle in the `start` command.
pub fn container_state_exists(name: &str) -> bool {
    state_path(name).exists()
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
    let counter = pelagos::paths::counter_file();
    if let Some(parent) = counter.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let n: u64 = std::fs::read_to_string(&counter)
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0);
    let next = n + 1;
    std::fs::write(&counter, next.to_string())?;
    Ok(format!("pelagos-{}", next))
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

/// Parse --user "1000" or "1000:1000" → `(uid, Option<gid>)`.
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
        pelagos::container::RlimitResource,
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
pub fn parse_capability(s: &str) -> Result<pelagos::container::Capability, String> {
    use pelagos::container::Capability;
    let name = s.trim().to_ascii_uppercase().replace('-', "_");
    let name = name.strip_prefix("CAP_").unwrap_or(&name);
    Capability::from_name(name).ok_or_else(|| {
        format!(
            "unknown capability '{s}' (use Linux capability names, e.g. NET_RAW, SYS_ADMIN, CHOWN)"
        )
    })
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
pub fn parse_capability_mask(names: &[String]) -> pelagos::container::Capability {
    use pelagos::container::Capability;
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

/// Read the inode number of `/proc/<pid>/ns/mnt`.
///
/// Returns `None` if the file cannot be stat'd (process has exited or the caller
/// lacks permission).
pub fn read_mnt_ns_inode(pid: i32) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(format!("/proc/{}/ns/mnt", pid))
        .ok()
        .map(|m| m.ino())
}

/// Verify that `pid` still belongs to the expected container by comparing the
/// live mount-namespace inode against the inode stored in `state.mnt_ns_inode`
/// at container creation time.
///
/// Returns `Ok(())` when:
/// - No stored inode exists (old state files — backwards compatible).
/// - The stored inode matches the current one.
///
/// Returns `Err` with a user-facing message when the inode doesn't match
/// (PID has been recycled by an unrelated process) or when the namespace file
/// can no longer be read (process has exited).
pub fn verify_pid_not_recycled(pid: i32, state: &ContainerState) -> Result<(), String> {
    let expected = match state.mnt_ns_inode {
        Some(inode) => inode,
        None => return Ok(()), // old state file — skip check
    };
    match read_mnt_ns_inode(pid) {
        Some(current) if current == expected => Ok(()),
        Some(_) => Err(format!(
            "container '{}' process (pid {}) is no longer running \
             (PID was reused by another process) — \
             use 'pelagos start {}' to restart it",
            state.name, pid, state.name
        )),
        None => Err(format!(
            "container '{}' process (pid {}) is no longer running — \
             use 'pelagos start {}' to restart it",
            state.name, pid, state.name
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// rootfs_path should accept an existing filesystem directory path and
    /// return its canonicalized form, without requiring it to be registered
    /// in the rootfs store.
    /// test_spawn_config_serde_roundtrip
    ///
    /// Verifies that SpawnConfig serializes to JSON and deserializes correctly,
    /// including all optional fields.  Also verifies backward compatibility:
    /// a ContainerState JSON without `spawn_config` deserializes with `spawn_config == None`.
    #[test]
    fn test_spawn_config_serde_roundtrip() {
        let sc = SpawnConfig {
            image: Some("docker.io/library/alpine:latest".to_string()),
            exe: "/bin/sh".to_string(),
            args: vec!["-c".to_string(), "echo hello".to_string()],
            env: vec!["FOO=bar".to_string()],
            bind: vec!["/tmp:/tmp".to_string()],
            bind_ro: vec!["/etc:/etc:ro".to_string()],
            volume: vec!["mydata:/data".to_string()],
            network: vec!["bridge".to_string()],
            publish: vec!["8080:80".to_string()],
            dns: vec!["1.1.1.1".to_string()],
            working_dir: Some("/app".to_string()),
            user: Some("1000:1000".to_string()),
            hostname: Some("myhost".to_string()),
            cap_drop: vec!["ALL".to_string()],
            cap_add: vec!["NET_BIND_SERVICE".to_string()],
            security_opt: vec!["no-new-privileges".to_string()],
            read_only: true,
            rm: false,
            nat: true,
            labels: vec!["env=staging".to_string(), "managed=true".to_string()],
            tmpfs: vec!["/run".to_string(), "/tmp:size=64m".to_string()],
        };
        let json = serde_json::to_string(&sc).unwrap();
        let decoded: SpawnConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.image, sc.image);
        assert_eq!(decoded.exe, sc.exe);
        assert_eq!(decoded.args, sc.args);
        assert_eq!(decoded.env, sc.env);
        assert_eq!(decoded.bind, sc.bind);
        assert_eq!(decoded.bind_ro, sc.bind_ro);
        assert_eq!(decoded.network, sc.network);
        assert_eq!(decoded.publish, sc.publish);
        assert_eq!(decoded.dns, sc.dns);
        assert_eq!(decoded.working_dir, sc.working_dir);
        assert_eq!(decoded.user, sc.user);
        assert_eq!(decoded.hostname, sc.hostname);
        assert_eq!(decoded.cap_drop, sc.cap_drop);
        assert_eq!(decoded.cap_add, sc.cap_add);
        assert_eq!(decoded.security_opt, sc.security_opt);
        assert!(decoded.read_only);
        assert!(!decoded.rm);
        assert!(decoded.nat);
        assert_eq!(decoded.labels, sc.labels);
        assert_eq!(decoded.tmpfs, sc.tmpfs);
    }

    /// test_spawn_config_labels_roundtrip
    ///
    /// Verifies that SpawnConfig.labels serializes and deserializes correctly,
    /// and that an older state JSON without labels field deserializes with empty vec.
    #[test]
    fn test_spawn_config_labels_roundtrip() {
        let sc = SpawnConfig {
            exe: "/bin/sh".to_string(),
            labels: vec!["project=myapp".to_string(), "env=prod".to_string()],
            ..Default::default()
        };
        let json = serde_json::to_string(&sc).unwrap();
        let decoded: SpawnConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.labels, vec!["project=myapp", "env=prod"]);

        // Old state JSON without labels field → empty vec (default).
        let old_json = r#"{"exe":"/bin/sh"}"#;
        let old: SpawnConfig = serde_json::from_str(old_json).unwrap();
        assert!(old.labels.is_empty());
    }

    /// test_label_filter
    ///
    /// Verifies that apply_filters correctly filters containers by label,
    /// including key-only and key=value forms, and that unknown filters are ignored.
    #[test]
    fn test_label_filter() {
        use super::ps::apply_filters;

        fn make_state(name: &str, labels: &[(&str, &str)]) -> ContainerState {
            ContainerState {
                name: name.to_string(),
                rootfs: "alpine".to_string(),
                status: ContainerStatus::Running,
                pid: 1,
                watcher_pid: 0,
                started_at: "2026-01-01T00:00:00Z".to_string(),
                exit_code: None,
                command: vec!["/bin/sh".to_string()],
                stdout_log: None,
                stderr_log: None,
                bridge_ip: None,
                network_ips: std::collections::HashMap::new(),
                health: None,
                health_config: None,
                spawn_config: None,
                labels: labels
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect(),
                mnt_ns_inode: None,
            }
        }

        let mut states = vec![
            make_state("web", &[("env", "staging"), ("managed", "true")]),
            make_state("db", &[("env", "prod"), ("managed", "true")]),
            make_state("cache", &[("tier", "infra")]),
        ];

        // Filter by key only.
        let mut s = states.clone();
        apply_filters(&mut s, &["label=managed".to_string()]);
        assert_eq!(s.len(), 2);
        assert!(s.iter().any(|c| c.name == "web"));
        assert!(s.iter().any(|c| c.name == "db"));

        // Filter by key=value.
        let mut s = states.clone();
        apply_filters(&mut s, &["label=env=staging".to_string()]);
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].name, "web");

        // No matches.
        let mut s = states.clone();
        apply_filters(&mut s, &["label=env=dev".to_string()]);
        assert!(s.is_empty());

        // Unknown filter is silently ignored.
        apply_filters(&mut states, &["unknown=foo".to_string()]);
        assert_eq!(states.len(), 3);
    }

    /// test_container_state_labels_serde
    ///
    /// Labels on ContainerState round-trip through JSON. Old state files without
    /// `labels` field deserialize with an empty map.
    #[test]
    fn test_container_state_labels_serde() {
        let mut labels = std::collections::HashMap::new();
        labels.insert("env".to_string(), "prod".to_string());
        labels.insert("managed".to_string(), "true".to_string());

        let state = ContainerState {
            name: "test".to_string(),
            rootfs: "alpine".to_string(),
            status: ContainerStatus::Running,
            pid: 42,
            watcher_pid: 0,
            started_at: "2026-01-01T00:00:00Z".to_string(),
            exit_code: None,
            command: vec!["/bin/sh".to_string()],
            stdout_log: None,
            stderr_log: None,
            bridge_ip: None,
            network_ips: std::collections::HashMap::new(),
            health: None,
            health_config: None,
            spawn_config: None,
            labels,
            mnt_ns_inode: None,
        };

        let json = serde_json::to_string(&state).unwrap();
        let decoded: ContainerState = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.labels.get("env").map(|s| s.as_str()), Some("prod"));
        assert_eq!(
            decoded.labels.get("managed").map(|s| s.as_str()),
            Some("true")
        );

        // Old state without labels → empty map.
        let old_json = r#"{
            "name": "old", "rootfs": "alpine", "status": "exited",
            "pid": 0, "watcher_pid": 0, "started_at": "2026-01-01T00:00:00Z",
            "exit_code": 0, "command": ["/bin/sh"],
            "stdout_log": null, "stderr_log": null
        }"#;
        let old: ContainerState = serde_json::from_str(old_json).unwrap();
        assert!(old.labels.is_empty());
    }

    /// test_spawn_config_missing_from_state
    ///
    /// Verifies that a ContainerState JSON without `spawn_config` deserializes
    /// with `spawn_config == None` (backward-compatibility with older state files).
    #[test]
    fn test_spawn_config_missing_from_state() {
        let json = r#"{
            "name": "test",
            "rootfs": "alpine",
            "status": "exited",
            "pid": 0,
            "watcher_pid": 0,
            "started_at": "2026-01-01T00:00:00Z",
            "exit_code": 0,
            "command": ["/bin/sh"],
            "stdout_log": null,
            "stderr_log": null
        }"#;
        let state: ContainerState = serde_json::from_str(json).unwrap();
        assert!(state.spawn_config.is_none());
    }

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

    /// test_health_config_serde_roundtrip
    ///
    /// Verifies that HealthConfig serializes to JSON and deserializes back
    /// correctly, preserving all fields. Failure indicates a serde regression.
    #[test]
    fn test_health_config_serde_roundtrip() {
        let hc = HealthConfig {
            cmd: vec![
                "/bin/sh".into(),
                "-c".into(),
                "curl -f http://localhost/".into(),
            ],
            interval_secs: 15,
            timeout_secs: 5,
            start_period_secs: 10,
            retries: 2,
        };
        let json = serde_json::to_string(&hc).unwrap();
        let decoded: HealthConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.cmd, hc.cmd);
        assert_eq!(decoded.interval_secs, 15);
        assert_eq!(decoded.timeout_secs, 5);
        assert_eq!(decoded.start_period_secs, 10);
        assert_eq!(decoded.retries, 2);
    }

    /// test_health_status_missing_field
    ///
    /// Verifies that a ContainerState JSON without a `health` field
    /// deserializes successfully with `health == None` (backward compatibility).
    /// Failure indicates the serde default is broken.
    #[test]
    fn test_health_status_missing_field() {
        let json = r#"{
            "name": "test",
            "rootfs": "alpine",
            "status": "running",
            "pid": 1234,
            "watcher_pid": 0,
            "started_at": "2026-01-01T00:00:00Z",
            "exit_code": null,
            "command": ["/bin/sh"],
            "stdout_log": null,
            "stderr_log": null
        }"#;
        let state: ContainerState = serde_json::from_str(json).unwrap();
        assert_eq!(state.health, None);
        assert!(state.health_config.is_none());
    }

    // ---------------------------------------------------------------------------
    // check_rootless_bridge tests
    // ---------------------------------------------------------------------------

    #[test]
    fn rootless_bridge_errors() {
        use pelagos::network::NetworkMode;
        assert!(check_rootless_bridge(true, &NetworkMode::Bridge, false, false).is_some());
        assert!(
            check_rootless_bridge(true, &NetworkMode::BridgeNamed("foo".into()), false, false)
                .is_some()
        );
        assert!(
            check_rootless_bridge(true, &NetworkMode::Bridge, false, false)
                .unwrap()
                .contains("requires root")
        );
    }

    #[test]
    fn rootless_nat_or_ports_errors() {
        use pelagos::network::NetworkMode;
        // nat=true with non-bridge mode
        assert!(check_rootless_bridge(true, &NetworkMode::None, true, false).is_some());
        // has_ports=true with non-bridge mode
        assert!(check_rootless_bridge(true, &NetworkMode::None, false, true).is_some());
        // bridge + nat + ports
        assert!(check_rootless_bridge(true, &NetworkMode::Bridge, true, true).is_some());
    }

    #[test]
    fn rootless_pasta_and_loopback_ok() {
        use pelagos::network::NetworkMode;
        assert!(check_rootless_bridge(true, &NetworkMode::Pasta, false, false).is_none());
        assert!(check_rootless_bridge(true, &NetworkMode::Loopback, false, false).is_none());
        assert!(check_rootless_bridge(true, &NetworkMode::None, false, false).is_none());
    }

    #[test]
    fn root_bridge_ok() {
        use pelagos::network::NetworkMode;
        // root (is_rootless=false) should never return an error regardless of mode
        assert!(check_rootless_bridge(false, &NetworkMode::Bridge, true, true).is_none());
        assert!(
            check_rootless_bridge(false, &NetworkMode::BridgeNamed("net".into()), true, true)
                .is_none()
        );
    }
}
