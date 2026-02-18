//! `remora stop` — send SIGTERM to a running container.

use super::{check_liveness, read_state, write_state, ContainerStatus};

pub fn cmd_stop(name: &str) -> Result<(), Box<dyn std::error::Error>> {
    let mut state = read_state(name)
        .map_err(|_| format!("no container named '{}'", name))?;

    if state.status != ContainerStatus::Running {
        return Err(format!("container '{}' is not running (status: {})", name, state.status).into());
    }

    if !check_liveness(state.pid) {
        // Already dead — update state and return.
        state.status = ContainerStatus::Exited;
        write_state(&state)?;
        return Ok(());
    }

    let r = unsafe { libc::kill(state.pid, libc::SIGTERM) };
    if r != 0 {
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::ESRCH) {
            // Process already gone.
        } else {
            return Err(format!("kill({}): {}", state.pid, err).into());
        }
    }

    // Update state to exited.
    state.status = ContainerStatus::Exited;
    write_state(&state)?;

    Ok(())
}
