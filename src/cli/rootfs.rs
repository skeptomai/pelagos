//! `remora rootfs` — manage the local rootfs store.

use super::rootfs_store;
use std::os::unix::fs::symlink;

pub fn cmd_rootfs_import(name: &str, path: &str) -> Result<(), Box<dyn std::error::Error>> {
    let store = rootfs_store();
    std::fs::create_dir_all(&store)?;

    // Resolve path to absolute.
    let src = std::fs::canonicalize(path)
        .map_err(|e| format!("cannot resolve '{}': {}", path, e))?;

    let link = store.join(name);
    if link.exists() || link.is_symlink() {
        std::fs::remove_file(&link)
            .map_err(|e| format!("remove existing '{}': {}", link.display(), e))?;
    }

    symlink(&src, &link)
        .map_err(|e| format!("symlink {} -> {}: {}", link.display(), src.display(), e))?;

    println!("Imported rootfs '{}' → {}", name, src.display());
    Ok(())
}

pub fn cmd_rootfs_ls() -> Result<(), Box<dyn std::error::Error>> {
    let store = rootfs_store();
    let entries = match std::fs::read_dir(&store) {
        Ok(e) => e,
        Err(_) => {
            println!("No rootfs store found at {}", store.display());
            return Ok(());
        }
    };

    let mut items: Vec<(String, String)> = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let target = std::fs::read_link(entry.path())
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| entry.path().to_string_lossy().into_owned());
        items.push((name, target));
    }
    items.sort_by(|a, b| a.0.cmp(&b.0));

    if items.is_empty() {
        println!("No rootfs images. Use: remora rootfs import <name> <path>");
        return Ok(());
    }

    let name_w = items.iter().map(|(n, _)| n.len()).max().unwrap_or(4).max(4);
    println!("{:<name_w$}  PATH", "NAME", name_w = name_w);
    for (name, target) in &items {
        println!("{:<name_w$}  {}", name, target, name_w = name_w);
    }
    Ok(())
}

pub fn cmd_rootfs_rm(name: &str) -> Result<(), Box<dyn std::error::Error>> {
    let link = rootfs_store().join(name);
    if !link.exists() && !link.is_symlink() {
        return Err(format!("rootfs '{}' not found", name).into());
    }
    std::fs::remove_file(&link)
        .map_err(|e| format!("remove '{}': {}", link.display(), e))?;
    println!("Removed rootfs '{}'", name);
    Ok(())
}
