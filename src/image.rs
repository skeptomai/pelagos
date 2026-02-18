//! OCI image store — filesystem layout, layer extraction, and manifest persistence.
//!
//! This module is purely synchronous. Networking (registry pulls) lives in `cli::image`.

use serde::{Deserialize, Serialize};
use std::io;
use std::path::{Path, PathBuf};

pub const IMAGES_DIR: &str = "/var/lib/remora/images";
pub const LAYERS_DIR: &str = "/var/lib/remora/layers";

/// Image configuration extracted from the OCI config JSON.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageConfig {
    /// Environment variables, e.g. `["PATH=/usr/bin", "HOME=/root"]`.
    #[serde(default)]
    pub env: Vec<String>,
    /// Default command (Docker `CMD`).
    #[serde(default)]
    pub cmd: Vec<String>,
    /// Entrypoint prefix (Docker `ENTRYPOINT`).
    #[serde(default)]
    pub entrypoint: Vec<String>,
    /// Working directory inside the container, e.g. `"/app"`.
    #[serde(default)]
    pub working_dir: String,
    /// User string, e.g. `"1000"` or `"nobody"`.
    #[serde(default)]
    pub user: String,
}

/// Persisted metadata for a pulled image.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageManifest {
    /// Image reference, e.g. `"alpine:latest"`.
    pub reference: String,
    /// Manifest digest, e.g. `"sha256:abc123..."`.
    pub digest: String,
    /// Ordered layer digests, bottom to top.
    pub layers: Vec<String>,
    /// Parsed image configuration.
    pub config: ImageConfig,
}

/// Convert an image reference like `"alpine:latest"` to a safe directory name (`"alpine_latest"`).
pub fn reference_to_dirname(reference: &str) -> String {
    reference.replace([':', '/', '@'], "_")
}

/// Return the image metadata directory for the given reference.
pub fn image_dir(reference: &str) -> PathBuf {
    PathBuf::from(IMAGES_DIR).join(reference_to_dirname(reference))
}

/// Return the extracted layer directory for the given digest.
/// Strips the `sha256:` prefix if present.
pub fn layer_dir(digest: &str) -> PathBuf {
    let hex = digest.strip_prefix("sha256:").unwrap_or(digest);
    PathBuf::from(LAYERS_DIR).join(hex)
}

/// Check whether a layer has already been extracted.
pub fn layer_exists(digest: &str) -> bool {
    layer_dir(digest).is_dir()
}

/// Extract a gzipped tar layer into the content-addressable layer store.
///
/// Handles OCI whiteout files:
/// - `.wh.<name>` → creates an overlayfs character device (0,0) named `<name>`.
/// - `.wh..wh..opq` → sets the `trusted.overlay.opaque` xattr on the parent dir.
///
/// Returns the path to the extracted layer directory.
pub fn extract_layer(digest: &str, tar_gz_path: &Path) -> io::Result<PathBuf> {
    let dest = layer_dir(digest);
    if dest.is_dir() {
        return Ok(dest);
    }

    // Extract to a temporary sibling, then rename atomically.
    let partial = dest.with_extension("partial");
    if partial.exists() {
        std::fs::remove_dir_all(&partial)?;
    }
    std::fs::create_dir_all(&partial)?;

    let file = std::fs::File::open(tar_gz_path)?;
    let decoder = flate2::read::GzDecoder::new(file);
    let mut archive = tar::Archive::new(decoder);
    archive.set_preserve_permissions(true);
    archive.set_overwrite(true);

    for entry in archive.entries()? {
        let mut entry = entry?;
        let raw_path = entry.path()?.into_owned();
        let file_name = raw_path.file_name().unwrap_or_default().to_string_lossy().to_string();

        if file_name == ".wh..wh..opq" {
            // Opaque whiteout: mark parent as opaque for overlayfs.
            let parent = partial.join(raw_path.parent().unwrap_or(Path::new("")));
            std::fs::create_dir_all(&parent)?;
            // Best-effort xattr; requires appropriate privileges.
            let _ = set_opaque_xattr(&parent);
            continue;
        }

        if let Some(target_name) = file_name.strip_prefix(".wh.") {
            // Regular whiteout: create a char device (0,0) for overlayfs.
            let parent = partial.join(raw_path.parent().unwrap_or(Path::new("")));
            std::fs::create_dir_all(&parent)?;
            let whiteout_path = parent.join(target_name);
            create_whiteout_device(&whiteout_path)?;
            continue;
        }

        // Normal file — unpack.
        entry.unpack_in(&partial)?;
    }

    // Ensure parent dir exists and rename partial → final.
    std::fs::create_dir_all(dest.parent().unwrap())?;
    std::fs::rename(&partial, &dest)?;

    Ok(dest)
}

/// Create an overlayfs whiteout character device (major 0, minor 0).
fn create_whiteout_device(path: &Path) -> io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let c_path = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::other("invalid path for whiteout device"))?;
    let dev = libc::makedev(0, 0);
    let ret = unsafe {
        libc::mknod(c_path.as_ptr(), libc::S_IFCHR | 0o666, dev)
    };
    if ret != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Set the `trusted.overlay.opaque` extended attribute on a directory.
fn set_opaque_xattr(dir: &Path) -> io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let c_path = CString::new(dir.as_os_str().as_bytes())
        .map_err(|_| io::Error::other("invalid path for xattr"))?;
    let name = b"trusted.overlay.opaque\0";
    let value = b"y";
    let ret = unsafe {
        libc::setxattr(
            c_path.as_ptr(),
            name.as_ptr() as *const libc::c_char,
            value.as_ptr() as *const libc::c_void,
            value.len(),
            0,
        )
    };
    if ret != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Persist an image manifest to disk.
pub fn save_image(manifest: &ImageManifest) -> io::Result<()> {
    let dir = image_dir(&manifest.reference);
    std::fs::create_dir_all(&dir)?;
    let json = serde_json::to_string_pretty(manifest)
        .map_err(|e| io::Error::other(e.to_string()))?;
    std::fs::write(dir.join("manifest.json"), json)
}

/// Load an image manifest from disk.
pub fn load_image(reference: &str) -> io::Result<ImageManifest> {
    let path = image_dir(reference).join("manifest.json");
    let data = std::fs::read_to_string(&path)?;
    serde_json::from_str(&data).map_err(|e| io::Error::other(e.to_string()))
}

/// List all stored images.
pub fn list_images() -> Vec<ImageManifest> {
    let dir = PathBuf::from(IMAGES_DIR);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut manifests = Vec::new();
    for entry in entries.flatten() {
        let manifest_path = entry.path().join("manifest.json");
        if let Ok(data) = std::fs::read_to_string(&manifest_path) {
            if let Ok(m) = serde_json::from_str::<ImageManifest>(&data) {
                manifests.push(m);
            }
        }
    }
    manifests
}

/// Remove an image and its metadata (does not remove shared layers).
pub fn remove_image(reference: &str) -> io::Result<()> {
    let dir = image_dir(reference);
    if dir.is_dir() {
        std::fs::remove_dir_all(&dir)
    } else {
        Err(io::Error::other(format!("image '{}' not found", reference)))
    }
}

/// Return layer directories in top-first order (as overlayfs expects for `lowerdir=`).
pub fn layer_dirs(manifest: &ImageManifest) -> Vec<PathBuf> {
    manifest.layers.iter().rev().map(|d| layer_dir(d)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_reference_to_dirname() {
        assert_eq!(reference_to_dirname("alpine:latest"), "alpine_latest");
        assert_eq!(reference_to_dirname("docker.io/library/alpine:3.19"), "docker.io_library_alpine_3.19");
        assert_eq!(reference_to_dirname("registry.example.com/foo/bar:v1"), "registry.example.com_foo_bar_v1");
    }

    #[test]
    fn test_layer_dir_strips_prefix() {
        let d = layer_dir("sha256:abc123def456");
        assert_eq!(d, PathBuf::from("/var/lib/remora/layers/abc123def456"));
    }

    #[test]
    fn test_layer_dir_no_prefix() {
        let d = layer_dir("abc123def456");
        assert_eq!(d, PathBuf::from("/var/lib/remora/layers/abc123def456"));
    }

    #[test]
    fn test_manifest_roundtrip() {
        // This test writes to disk — only meaningful under root in the integration suite.
        // Here we just verify serialize/deserialize round-trip in memory.
        let manifest = ImageManifest {
            reference: "test:latest".to_string(),
            digest: "sha256:000".to_string(),
            layers: vec!["sha256:aaa".to_string(), "sha256:bbb".to_string()],
            config: ImageConfig {
                env: vec!["PATH=/usr/bin".to_string()],
                cmd: vec!["/bin/sh".to_string()],
                entrypoint: Vec::new(),
                working_dir: String::new(),
                user: String::new(),
            },
        };
        let json = serde_json::to_string(&manifest).unwrap();
        let loaded: ImageManifest = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.reference, "test:latest");
        assert_eq!(loaded.layers.len(), 2);
        assert_eq!(loaded.config.cmd, vec!["/bin/sh"]);
    }

    #[test]
    fn test_layer_dirs_order() {
        let manifest = ImageManifest {
            reference: "test:latest".to_string(),
            digest: "sha256:000".to_string(),
            layers: vec!["sha256:bottom".to_string(), "sha256:top".to_string()],
            config: ImageConfig {
                env: Vec::new(),
                cmd: Vec::new(),
                entrypoint: Vec::new(),
                working_dir: String::new(),
                user: String::new(),
            },
        };
        let dirs = layer_dirs(&manifest);
        // Top-first for overlayfs lowerdir
        assert_eq!(dirs[0], PathBuf::from("/var/lib/remora/layers/top"));
        assert_eq!(dirs[1], PathBuf::from("/var/lib/remora/layers/bottom"));
    }
}
