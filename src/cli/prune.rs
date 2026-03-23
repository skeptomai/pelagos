//! `pelagos prune` — remove all stopped containers.
//!
//! Deletes the `state.json` directory for every container whose status is not
//! `running` (or whose PID is no longer alive).  Equivalent to calling
//! `pelagos rm` on each stopped container individually.

use super::{check_liveness, container_dir, list_containers, ContainerStatus};

pub fn cmd_prune() -> Result<(), Box<dyn std::error::Error>> {
    let containers = list_containers();
    let mut removed = 0u32;
    let mut skipped = 0u32;

    for state in containers {
        let is_running = state.status == ContainerStatus::Running && check_liveness(state.pid);
        if is_running {
            skipped += 1;
            continue;
        }
        let dir = container_dir(&state.name);
        match std::fs::remove_dir_all(&dir) {
            Ok(()) => {
                println!("{}", state.name);
                removed += 1;
            }
            Err(e) => {
                log::warn!("prune: failed to remove {}: {}", dir.display(), e);
            }
        }
    }

    if removed == 0 && skipped == 0 {
        println!("No containers to prune.");
    } else {
        println!("\nRemoved {} container(s).", removed);
        if skipped > 0 {
            println!("Skipped {} running container(s).", skipped);
        }
    }
    Ok(())
}
