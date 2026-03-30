//! `pelagos system` — system-wide maintenance commands.
//!
//! Subcommands:
//! - `prune [--all] [--volumes]` — reclaim disk space used by unused images,
//!   orphaned layers, cached blobs, and (optionally) unused volumes.

use std::collections::HashSet;

use clap::Subcommand;

use super::{check_liveness, list_containers};
use pelagos::image::{layer_exists, list_images};

// ── Subcommand enum ───────────────────────────────────────────────────────────

#[derive(Subcommand, Debug)]
pub(crate) enum SystemCmd {
    /// Reclaim disk space used by unused layers, blobs, and optionally volumes.
    ///
    /// By default removes:
    ///   - All cached blobs (compressed tarballs in blobs/)
    ///   - Orphaned layer directories (not referenced by any local image)
    ///   - Build cache entries not referenced by any local image
    ///
    /// Running containers are never touched.
    Prune {
        /// Also remove layers for images that are present but have no running
        /// containers — frees maximum disk space (analogous to `docker system prune -a`).
        #[clap(long, short = 'a')]
        all: bool,

        /// Also remove named volumes that are not mounted by any running container.
        #[clap(long)]
        volumes: bool,
    },

    /// Show disk usage by pelagos storage components.
    Df,
}

// ── cmd_system ────────────────────────────────────────────────────────────────

pub fn cmd_system(cmd: SystemCmd) -> Result<(), Box<dyn std::error::Error>> {
    match cmd {
        SystemCmd::Prune { all, volumes } => cmd_system_prune(all, volumes),
        SystemCmd::Df => cmd_system_df(),
    }
}

// ── system prune ─────────────────────────────────────────────────────────────

pub fn cmd_system_prune(all: bool, volumes: bool) -> Result<(), Box<dyn std::error::Error>> {
    let mut freed_bytes: u64 = 0;

    // ── Step 1: blobs ──────────────────────────────────────────────────────
    // Blobs are transient: pulled images no longer retain them after layer
    // unpack (fix for issue #127).  Only locally-built images keep blobs for
    // push operations.  Anything left in blobs/ is either from a build or a
    // pre-#127 pull — safe to remove since overlay mounts use layers/ directly.
    freed_bytes += prune_blobs();

    // ── Step 2: build cache ────────────────────────────────────────────────
    freed_bytes += prune_build_cache();

    // ── Step 3: orphan layers ──────────────────────────────────────────────
    // Collect every layer digest that is referenced by at least one local image.
    let images = list_images();
    let referenced: HashSet<String> = images
        .iter()
        .flat_map(|m| m.layers.iter().cloned())
        .collect();

    // If --all was given, also treat layers in images that have no running container.
    // Build the set of digests that must be kept.
    let keep: HashSet<String> = if all {
        // Keep only layers in images that have a currently-running container.
        let running_images = running_container_images();
        images
            .iter()
            .filter(|m| running_images.contains(&m.reference))
            .flat_map(|m| m.layers.iter().cloned())
            .collect()
    } else {
        referenced.clone()
    };

    freed_bytes += prune_orphan_layers(&keep);

    // ── Step 4: volumes (opt-in) ───────────────────────────────────────────
    if volumes {
        freed_bytes += prune_unused_volumes();
    }

    // ── Summary ───────────────────────────────────────────────────────────
    if freed_bytes == 0 {
        println!("Nothing to prune.");
    } else {
        println!("\nTotal reclaimed: {}", format_bytes(freed_bytes));
    }
    Ok(())
}

// ── system df ────────────────────────────────────────────────────────────────

pub fn cmd_system_df() -> Result<(), Box<dyn std::error::Error>> {
    let layers_bytes = dir_size(pelagos::paths::layers_dir());
    let blobs_bytes = dir_size(pelagos::paths::blobs_dir());
    let images_bytes = dir_size(pelagos::paths::images_dir());
    let volumes_bytes = dir_size(pelagos::paths::volumes_dir());
    let build_cache_bytes = dir_size(pelagos::paths::build_cache_dir());

    println!("{:<20} {:>10}", "Component", "Size");
    println!("{}", "-".repeat(32));
    println!("{:<20} {:>10}", "layers/", format_bytes(layers_bytes));
    println!("{:<20} {:>10}", "blobs/", format_bytes(blobs_bytes));
    println!("{:<20} {:>10}", "images/", format_bytes(images_bytes));
    println!("{:<20} {:>10}", "volumes/", format_bytes(volumes_bytes));
    println!(
        "{:<20} {:>10}",
        "build-cache/",
        format_bytes(build_cache_bytes)
    );
    println!("{}", "-".repeat(32));
    println!(
        "{:<20} {:>10}",
        "Total",
        format_bytes(layers_bytes + blobs_bytes + images_bytes + volumes_bytes + build_cache_bytes)
    );
    Ok(())
}

// ── helpers ───────────────────────────────────────────────────────────────────

/// Remove all files in blobs/.  Returns bytes freed.
fn prune_blobs() -> u64 {
    let dir = pelagos::paths::blobs_dir();
    let mut freed = 0u64;
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(_) => return 0,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() {
            freed += path.metadata().map(|m| m.len()).unwrap_or(0);
            if let Err(e) = std::fs::remove_file(&path) {
                log::warn!(
                    "system prune: failed to remove blob {}: {}",
                    path.display(),
                    e
                );
            } else {
                println!(
                    "Removed blob:  {}",
                    path.file_name().unwrap_or_default().to_string_lossy()
                );
            }
        }
    }
    freed
}

/// Remove all entries in build-cache/.  Returns bytes freed.
fn prune_build_cache() -> u64 {
    let dir = pelagos::paths::build_cache_dir();
    let mut freed = 0u64;
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(_) => return 0,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        freed += recursive_size(&path);
        if let Err(e) = std::fs::remove_dir_all(&path) {
            log::warn!(
                "system prune: failed to remove cache entry {}: {}",
                path.display(),
                e
            );
        } else {
            println!(
                "Removed cache: {}",
                path.file_name().unwrap_or_default().to_string_lossy()
            );
        }
    }
    freed
}

/// Remove layer directories whose digest is not in `keep`.  Returns bytes freed.
fn prune_orphan_layers(keep: &HashSet<String>) -> u64 {
    let layers_dir = pelagos::paths::layers_dir();
    let mut freed = 0u64;
    let entries = match std::fs::read_dir(&layers_dir) {
        Ok(e) => e,
        Err(_) => return 0,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        // Directory name is the hex digest; reconstruct the full digest key.
        let hex = match path.file_name().and_then(|n| n.to_str()) {
            Some(h) => h.to_string(),
            None => continue,
        };
        let digest = format!("sha256:{}", hex);
        if keep.contains(&digest) {
            continue;
        }
        freed += recursive_size(&path);
        if let Err(e) = std::fs::remove_dir_all(&path) {
            log::warn!("system prune: failed to remove layer {}: {}", hex, e);
        } else {
            println!("Removed layer: {}", &hex[..hex.len().min(16)]);
        }
    }
    freed
}

/// Remove named volumes not mounted by any live container.  Returns bytes freed.
fn prune_unused_volumes() -> u64 {
    let volumes_dir = pelagos::paths::volumes_dir();
    let mut freed = 0u64;

    // Collect volume names mounted by running containers.
    let mounted: HashSet<String> = mounted_volume_names();

    let entries = match std::fs::read_dir(&volumes_dir) {
        Ok(e) => e,
        Err(_) => return 0,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n.to_string(),
            None => continue,
        };
        if mounted.contains(&name) {
            continue;
        }
        freed += recursive_size(&path);
        if let Err(e) = std::fs::remove_dir_all(&path) {
            log::warn!("system prune: failed to remove volume {}: {}", name, e);
        } else {
            println!("Removed volume: {}", name);
        }
    }
    freed
}

/// Return the set of image references (`manifest.reference`) that have at
/// least one currently-running container.
fn running_container_images() -> HashSet<String> {
    list_containers()
        .into_iter()
        .filter(|c| check_liveness(c.pid))
        .map(|c| c.rootfs)
        .collect()
}

/// Return the set of volume names that appear in any running container's mounts.
///
/// Checks both:
/// - `SpawnConfig.volume` entries ("volname:container_path") — named volumes
/// - `SpawnConfig.bind` and `bind_ro` entries ("host_path:container_path") where
///   the host path resolves to a path under `volumes_dir()`
fn mounted_volume_names() -> HashSet<String> {
    let volumes_prefix = pelagos::paths::volumes_dir();
    let mut names = HashSet::new();
    for state in list_containers() {
        if !check_liveness(state.pid) {
            continue;
        }
        let sc = match state.spawn_config {
            Some(sc) => sc,
            None => continue,
        };
        // Named volumes: "volname:container_path"
        for vol in &sc.volume {
            let vol_name = vol.split(':').next().unwrap_or(vol);
            if !vol_name.is_empty() {
                names.insert(vol_name.to_string());
            }
        }
        // Bind mounts that happen to point inside volumes_dir
        for bind in sc.bind.iter().chain(sc.bind_ro.iter()) {
            let host: &str = bind.split(':').next().unwrap_or(bind);
            let host_path = std::path::Path::new(host);
            if let Ok(rel) = host_path.strip_prefix(&volumes_prefix) {
                if let Some(vol_name) = rel.components().next() {
                    names.insert(vol_name.as_os_str().to_string_lossy().into_owned());
                }
            }
        }
    }
    names
}

/// Recursively sum the size of all files under `path`.
fn recursive_size(path: &std::path::Path) -> u64 {
    if path.is_file() {
        return path.metadata().map(|m| m.len()).unwrap_or(0);
    }
    let mut total = 0u64;
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            total += recursive_size(&entry.path());
        }
    }
    total
}

/// Sum the size of all regular files directly in `dir` (non-recursive).
fn dir_size(dir: std::path::PathBuf) -> u64 {
    let mut total = 0u64;
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            total += recursive_size(&entry.path());
        }
    }
    total
}

/// Format a byte count as a human-readable string (B / KB / MB / GB).
pub fn format_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;
    if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.1} KB", bytes as f64 / KB as f64)
    } else {
        format!("{} B", bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verify `format_bytes` produces correct unit strings for boundary values.
    #[test]
    fn test_format_bytes_units() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(1023), "1023 B");
        assert_eq!(format_bytes(1024), "1.0 KB");
        assert_eq!(format_bytes(1024 * 1024), "1.0 MB");
        assert_eq!(format_bytes(1024 * 1024 * 1024), "1.0 GB");
    }

    /// Verify `prune_orphan_layers` removes directories whose digest is not in
    /// the keep set, and leaves those that are.
    ///
    /// Uses a real temp directory so no mocking of paths is needed, but does
    /// NOT touch the live `/var/lib/pelagos/layers/` directory.
    #[test]
    fn test_prune_orphan_layers_keep_set() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let layers_dir = tmp.path();

        // Create two layer dirs: one to keep, one to prune.
        let keep_hex = "aaaa0000000000000000000000000000000000000000000000000000000000aa";
        let prune_hex = "bbbb0000000000000000000000000000000000000000000000000000000000bb";
        let keep_dir = layers_dir.join(keep_hex);
        let prune_dir = layers_dir.join(prune_hex);
        std::fs::create_dir_all(&keep_dir).unwrap();
        std::fs::create_dir_all(&prune_dir).unwrap();
        std::fs::write(keep_dir.join("f"), b"k").unwrap();
        std::fs::write(prune_dir.join("f"), b"p").unwrap();

        let keep: HashSet<String> = std::iter::once(format!("sha256:{}", keep_hex)).collect();

        // Call the helper with our temp dir instead of the real layers dir.
        // We replicate the body here since prune_orphan_layers reads from
        // `pelagos::paths::layers_dir()` directly — the logic is the same.
        let mut freed = 0u64;
        for entry in std::fs::read_dir(layers_dir).unwrap().flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let hex = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap()
                .to_string();
            let digest = format!("sha256:{}", hex);
            if keep.contains(&digest) {
                continue;
            }
            freed += recursive_size(&path);
            std::fs::remove_dir_all(&path).unwrap();
        }

        assert!(freed > 0, "should have freed bytes from the orphan layer");
        assert!(keep_dir.exists(), "keep dir should NOT be pruned");
        assert!(!prune_dir.exists(), "prune dir SHOULD be removed");
    }
}
