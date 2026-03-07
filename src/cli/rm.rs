//! `pelagos rm` — remove a container.

use super::{check_liveness, container_dir, read_state, ContainerStatus};

pub fn cmd_rm(name: &str, force: bool) -> Result<(), Box<dyn std::error::Error>> {
    let state = read_state(name).map_err(|_| format!("no container named '{}'", name))?;

    if state.status == ContainerStatus::Running && check_liveness(state.pid) {
        if force {
            // SIGTERM first so the watcher can run teardown (veth/netns/nftables cleanup).
            // The watcher's SIGTERM handler forwards the signal to the container; when the
            // container exits the watcher's wait() returns and teardown_resources() runs.
            let r = unsafe { libc::kill(state.pid, libc::SIGTERM) };
            if r != 0 {
                let err = std::io::Error::last_os_error();
                if err.raw_os_error() != Some(libc::ESRCH) {
                    return Err(format!("kill({}): {}", state.pid, err).into());
                }
            }
            // Give the container up to 5 s to exit cleanly before resorting to SIGKILL.
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while check_liveness(state.pid) && std::time::Instant::now() < deadline {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            if check_liveness(state.pid) {
                unsafe { libc::kill(state.pid, libc::SIGKILL) };
                for _ in 0..10 {
                    std::thread::sleep(std::time::Duration::from_millis(100));
                    if !check_liveness(state.pid) {
                        break;
                    }
                }
            }
        } else {
            return Err(format!(
                "container '{}' is running; use --force to remove it or `pelagos stop {}` first",
                name, name
            )
            .into());
        }
    }

    let dir = container_dir(name);
    std::fs::remove_dir_all(&dir).map_err(|e| format!("remove {}: {}", dir.display(), e))?;

    Ok(())
}
