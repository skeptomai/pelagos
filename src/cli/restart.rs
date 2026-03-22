//! `pelagos restart` — stop then start a container.
//!
//! For a running container: send SIGTERM, wait up to `--time` seconds for a
//! clean exit, send SIGKILL if it does not stop in time, then re-run it with
//! its saved SpawnConfig (detached).
//!
//! For an exited container: equivalent to `pelagos start`.

use super::start::cmd_start;
use super::stop::cmd_stop;
use super::{check_liveness, read_state, ContainerStatus};

pub fn cmd_restart(name: &str, time: u64) -> Result<(), Box<dyn std::error::Error>> {
    let state = read_state(name).map_err(|_| format!("no container named '{}'", name))?;

    if state.status == ContainerStatus::Running {
        let pid = state.pid;

        // Send SIGTERM and mark state as Exited.
        cmd_stop(name)?;

        // Wait for the process to actually vacate its PID.
        let deadline =
            std::time::Instant::now() + std::time::Duration::from_secs(time.max(1));
        while check_liveness(pid) && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(100));
        }

        // If still alive after the grace period, escalate to SIGKILL.
        if check_liveness(pid) {
            unsafe { libc::kill(pid, libc::SIGKILL) };
            let kill_deadline =
                std::time::Instant::now() + std::time::Duration::from_secs(5);
            while check_liveness(pid) && std::time::Instant::now() < kill_deadline {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        }
    }

    cmd_start(&[name.to_string()], false, None)
}
