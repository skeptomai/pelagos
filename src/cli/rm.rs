//! `remora rm` — remove a container.

use super::{check_liveness, container_dir, read_state, ContainerStatus};

pub fn cmd_rm(name: &str, force: bool) -> Result<(), Box<dyn std::error::Error>> {
    let state = read_state(name)
        .map_err(|_| format!("no container named '{}'", name))?;

    if state.status == ContainerStatus::Running && check_liveness(state.pid) {
        if force {
            // SIGKILL the container process.
            let r = unsafe { libc::kill(state.pid, libc::SIGKILL) };
            if r != 0 {
                let err = std::io::Error::last_os_error();
                if err.raw_os_error() != Some(libc::ESRCH) {
                    return Err(format!("kill({}): {}", state.pid, err).into());
                }
            }
            // Wait briefly for the process to die.
            for _ in 0..10 {
                std::thread::sleep(std::time::Duration::from_millis(100));
                if !check_liveness(state.pid) {
                    break;
                }
            }
        } else {
            return Err(format!(
                "container '{}' is running; use --force to remove it or `remora stop {}` first",
                name, name
            ).into());
        }
    }

    let dir = container_dir(name);
    std::fs::remove_dir_all(&dir)
        .map_err(|e| format!("remove {}: {}", dir.display(), e))?;

    Ok(())
}
