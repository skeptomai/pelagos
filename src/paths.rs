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
/// - Root: `/var/lib/remora/`
/// - Rootless: `$XDG_DATA_HOME/remora/` (default `~/.local/share/remora/`)
pub fn data_dir() -> PathBuf {
    if is_rootless() {
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
    } else {
        PathBuf::from("/var/lib/remora")
    }
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
}
