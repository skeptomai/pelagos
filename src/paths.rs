//! Centralised path resolution for all Remora filesystem locations.
//!
//! Root mode uses system directories (`/var/lib/remora/`, `/run/remora/`).
//! Rootless mode uses per-user XDG directories (`~/.local/share/remora/`,
//! `$XDG_RUNTIME_DIR/remora/`).

use std::path::PathBuf;

/// Returns `true` when running as a non-root user.
pub fn is_rootless() -> bool {
    unsafe { libc::getuid() != 0 }
}

/// Persistent data directory.
///
/// - Root (or system store already initialised): `/var/lib/remora/`
/// - Rootless with no system store: `$XDG_DATA_HOME/remora/` (default `~/.local/share/remora/`)
///
/// If `/var/lib/remora/` already exists we always use it, regardless of the
/// current UID.  This means a non-root user can pull images into the same
/// store that `sudo remora` uses, once root has initialised the directory
/// (which happens automatically on the first root pull/run).
pub fn data_dir() -> PathBuf {
    let system_dir = PathBuf::from("/var/lib/remora");
    // Use the system store if it already exists OR if we are root.
    if system_dir.exists() || !is_rootless() {
        return system_dir;
    }
    // Pure rootless: system store has never been initialised, use XDG dir.
    if let Ok(xdg) = std::env::var("XDG_DATA_HOME") {
        if !xdg.is_empty() {
            return PathBuf::from(xdg).join("remora");
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home).join(".local/share/remora");
    }
    // Last resort: /tmp fallback (unlikely on any real system).
    PathBuf::from(format!("/tmp/remora-data-{}", unsafe { libc::getuid() }))
}

/// Ephemeral runtime directory.
///
/// - Root: `/run/remora/`
/// - Rootless: `$XDG_RUNTIME_DIR/remora/` (fallback `/tmp/remora-<uid>/`, mode 0700)
pub fn runtime_dir() -> PathBuf {
    if is_rootless() {
        if let Ok(xdg) = std::env::var("XDG_RUNTIME_DIR") {
            if !xdg.is_empty() {
                return PathBuf::from(xdg).join("remora");
            }
        }
        let uid = unsafe { libc::getuid() };
        let fallback = PathBuf::from(format!("/tmp/remora-{}", uid));
        // Best-effort create with 0700.
        if !fallback.exists() {
            let _ = std::fs::create_dir_all(&fallback);
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&fallback, std::fs::Permissions::from_mode(0o700));
            }
        }
        fallback
    } else {
        PathBuf::from("/run/remora")
    }
}

// ── Derived from data_dir() ─────────────────────────────────────────────────

/// Directory for OCI image manifests: `<data>/images/`.
pub fn images_dir() -> PathBuf {
    data_dir().join("images")
}

/// Content-addressable layer store: `<data>/layers/`.
pub fn layers_dir() -> PathBuf {
    data_dir().join("layers")
}

/// Named volumes: `<data>/volumes/`.
pub fn volumes_dir() -> PathBuf {
    data_dir().join("volumes")
}

/// Imported rootfs store: `<data>/rootfs/`.
pub fn rootfs_store_dir() -> PathBuf {
    data_dir().join("rootfs")
}

/// Auto-incrementing container name counter file: `<data>/container_counter`.
pub fn counter_file() -> PathBuf {
    data_dir().join("container_counter")
}

/// Build cache directory: `<data>/build-cache/`.
pub fn build_cache_dir() -> PathBuf {
    data_dir().join("build-cache")
}

/// Raw compressed blob store: `<data>/blobs/`.
///
/// Stores the original `.tar.gz` bytes for each layer, keyed by digest.
/// Required for `remora image push`.
pub fn blobs_dir() -> PathBuf {
    data_dir().join("blobs")
}

/// Path for a single blob: `<data>/blobs/<hex>.tar.gz`.
///
/// `digest` may include the `sha256:` prefix or be a bare hex string.
pub fn blob_path(digest: &str) -> PathBuf {
    let hex = digest.strip_prefix("sha256:").unwrap_or(digest);
    blobs_dir().join(format!("{}.tar.gz", hex))
}

/// Sidecar file storing the uncompressed-tar `diff_id` for a given blob digest.
///
/// Path: `<data>/blobs/<hex>.diffid`
pub fn blob_diffid_path(digest: &str) -> PathBuf {
    let hex = digest.strip_prefix("sha256:").unwrap_or(digest);
    blobs_dir().join(format!("{}.diffid", hex))
}

// ── Derived from runtime_dir() ──────────────────────────────────────────────

/// Per-container state directories: `<runtime>/containers/`.
pub fn containers_dir() -> PathBuf {
    runtime_dir().join("containers")
}

/// OCI runtime state directory: `<runtime>/<id>/`.
pub fn oci_state_dir(id: &str) -> PathBuf {
    runtime_dir().join(id)
}

/// Overlay scratch directory: `<runtime>/overlay-<pid>-<n>/`.
pub fn overlay_base(pid: i32, n: u32) -> PathBuf {
    runtime_dir().join(format!("overlay-{}-{}", pid, n))
}

/// DNS temp directory: `<runtime>/dns-<pid>-<n>/`.
pub fn dns_dir(pid: i32, n: u32) -> PathBuf {
    runtime_dir().join(format!("dns-{}-{}", pid, n))
}

/// Hosts temp directory: `<runtime>/hosts-<pid>-<n>/`.
pub fn hosts_dir(pid: i32, n: u32) -> PathBuf {
    runtime_dir().join(format!("hosts-{}-{}", pid, n))
}

/// IPAM next-IP file: `<runtime>/next_ip`.
pub fn ipam_file() -> PathBuf {
    runtime_dir().join("next_ip")
}

/// NAT reference count file: `<runtime>/nat_refcount`.
pub fn nat_refcount_file() -> PathBuf {
    runtime_dir().join("nat_refcount")
}

/// Port-forward entries file: `<runtime>/port_forwards`.
pub fn port_forwards_file() -> PathBuf {
    runtime_dir().join("port_forwards")
}

// ── DNS daemon paths ─────────────────────────────────────────────────────────

/// DNS daemon config directory: `<runtime>/dns/`.
pub fn dns_config_dir() -> PathBuf {
    runtime_dir().join("dns")
}

/// DNS daemon PID file: `<runtime>/dns/pid`.
pub fn dns_pid_file() -> PathBuf {
    dns_config_dir().join("pid")
}

/// Per-network DNS config file: `<runtime>/dns/<network_name>`.
pub fn dns_network_file(name: &str) -> PathBuf {
    dns_config_dir().join(name)
}

/// DNS backend marker file: `<runtime>/dns/backend`.
pub fn dns_backend_file() -> PathBuf {
    dns_config_dir().join("backend")
}

/// dnsmasq generated config: `<runtime>/dns/dnsmasq.conf`.
pub fn dns_dnsmasq_conf() -> PathBuf {
    dns_config_dir().join("dnsmasq.conf")
}

/// Per-network hosts file for dnsmasq: `<runtime>/dns/hosts.<network>`.
pub fn dns_hosts_file(network_name: &str) -> PathBuf {
    dns_config_dir().join(format!("hosts.{}", network_name))
}

// ── Compose directories ─────────────────────────────────────────────────────

/// Compose project root: `<runtime>/compose/`.
pub fn compose_dir() -> PathBuf {
    runtime_dir().join("compose")
}

/// Compose project directory: `<runtime>/compose/<project>/`.
pub fn compose_project_dir(project: &str) -> PathBuf {
    compose_dir().join(project)
}

/// Compose project state file: `<runtime>/compose/<project>/state.json`.
pub fn compose_state_file(project: &str) -> PathBuf {
    compose_project_dir(project).join("state.json")
}

// ── Per-network directories ─────────────────────────────────────────────────

/// Persistent config directory for all named networks: `<data>/networks/`.
pub fn networks_config_dir() -> PathBuf {
    data_dir().join("networks")
}

/// Config directory for a specific network: `<data>/networks/<name>/`.
pub fn network_config_dir(name: &str) -> PathBuf {
    networks_config_dir().join(name)
}

/// Runtime state directory for a specific network: `<runtime>/networks/<name>/`.
pub fn network_runtime_dir(name: &str) -> PathBuf {
    runtime_dir().join("networks").join(name)
}

/// Per-network IPAM next-IP file: `<runtime>/networks/<name>/next_ip`.
pub fn network_ipam_file(name: &str) -> PathBuf {
    network_runtime_dir(name).join("next_ip")
}

/// Per-network NAT refcount file: `<runtime>/networks/<name>/nat_refcount`.
pub fn network_nat_refcount_file(name: &str) -> PathBuf {
    network_runtime_dir(name).join("nat_refcount")
}

/// Per-network port-forward entries file: `<runtime>/networks/<name>/port_forwards`.
pub fn network_port_forwards_file(name: &str) -> PathBuf {
    network_runtime_dir(name).join("port_forwards")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_rootless_returns_bool() {
        // Just verify it doesn't panic. The actual value depends on who runs the test.
        let _ = is_rootless();
    }

    #[test]
    fn test_data_dir_is_absolute() {
        assert!(data_dir().is_absolute());
    }

    #[test]
    fn test_runtime_dir_is_absolute() {
        assert!(runtime_dir().is_absolute());
    }

    #[test]
    fn test_derived_paths_under_data_dir() {
        let data = data_dir();
        assert!(images_dir().starts_with(&data));
        assert!(layers_dir().starts_with(&data));
        assert!(volumes_dir().starts_with(&data));
        assert!(rootfs_store_dir().starts_with(&data));
        assert!(counter_file().starts_with(&data));
        assert!(blobs_dir().starts_with(&data));
    }

    #[test]
    fn test_blob_path() {
        let p = blob_path("sha256:abc123");
        assert_eq!(p, blobs_dir().join("abc123.tar.gz"));
        let p2 = blob_path("abc123");
        assert_eq!(p2, blobs_dir().join("abc123.tar.gz"));
    }

    #[test]
    fn test_derived_paths_under_runtime_dir() {
        let rt = runtime_dir();
        assert!(containers_dir().starts_with(&rt));
        assert!(oci_state_dir("test").starts_with(&rt));
        assert!(overlay_base(1, 0).starts_with(&rt));
        assert!(dns_dir(1, 0).starts_with(&rt));
        assert!(hosts_dir(1, 0).starts_with(&rt));
        assert!(ipam_file().starts_with(&rt));
        assert!(nat_refcount_file().starts_with(&rt));
        assert!(port_forwards_file().starts_with(&rt));
    }

    #[test]
    fn test_network_config_paths_under_data_dir() {
        let data = data_dir();
        assert!(networks_config_dir().starts_with(&data));
        assert!(network_config_dir("frontend").starts_with(&data));
        assert_eq!(
            network_config_dir("frontend"),
            networks_config_dir().join("frontend")
        );
    }

    #[test]
    fn test_network_runtime_paths_under_runtime_dir() {
        let rt = runtime_dir();
        assert!(network_runtime_dir("frontend").starts_with(&rt));
        assert!(network_ipam_file("frontend").starts_with(&rt));
        assert!(network_nat_refcount_file("frontend").starts_with(&rt));
        assert!(network_port_forwards_file("frontend").starts_with(&rt));
    }

    #[test]
    fn test_compose_paths_under_runtime_dir() {
        let rt = runtime_dir();
        assert!(compose_dir().starts_with(&rt));
        assert!(compose_project_dir("myapp").starts_with(&rt));
        assert!(compose_state_file("myapp").starts_with(&rt));
        assert_eq!(compose_project_dir("myapp"), compose_dir().join("myapp"));
        assert_eq!(
            compose_state_file("myapp"),
            compose_project_dir("myapp").join("state.json")
        );
    }

    #[test]
    fn test_dns_paths_under_runtime_dir() {
        let rt = runtime_dir();
        assert!(dns_config_dir().starts_with(&rt));
        assert!(dns_pid_file().starts_with(&rt));
        assert!(dns_network_file("remora0").starts_with(&rt));
        assert_eq!(
            dns_network_file("frontend"),
            dns_config_dir().join("frontend")
        );
    }

    #[test]
    fn test_dns_dnsmasq_paths_under_runtime_dir() {
        let rt = runtime_dir();
        assert!(dns_backend_file().starts_with(&rt));
        assert!(dns_dnsmasq_conf().starts_with(&rt));
        assert!(dns_hosts_file("remora0").starts_with(&rt));
        assert_eq!(
            dns_hosts_file("frontend"),
            dns_config_dir().join("hosts.frontend")
        );
    }
}
