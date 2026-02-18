//! `remora volume` — manage named volumes.

use remora::container::Volume;

pub fn cmd_volume_create(name: &str) -> Result<(), Box<dyn std::error::Error>> {
    Volume::create(name).map_err(|e| format!("create volume '{}': {}", name, e))?;
    println!("Created volume '{}'", name);
    Ok(())
}

pub fn cmd_volume_ls() -> Result<(), Box<dyn std::error::Error>> {
    let volumes_dir = std::path::PathBuf::from("/var/lib/remora/volumes");
    let entries = match std::fs::read_dir(&volumes_dir) {
        Ok(e) => e,
        Err(_) => {
            println!("No volumes found.");
            return Ok(());
        }
    };

    let mut names: Vec<String> = entries
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();

    if names.is_empty() {
        println!("No volumes. Use: remora volume create <name>");
        return Ok(());
    }

    println!("{}", "NAME");
    for name in &names {
        println!("{}", name);
    }
    Ok(())
}

pub fn cmd_volume_rm(name: &str) -> Result<(), Box<dyn std::error::Error>> {
    Volume::delete(name).map_err(|e| format!("remove volume '{}': {}", name, e))?;
    println!("Removed volume '{}'", name);
    Ok(())
}
