//! Container health monitor — runs as a thread inside the watcher process.
//!
//! Periodically executes the health check command inside the container's
//! namespaces (via namespace-join, same mechanism as `remora exec`) and
//! updates `state.json` with the current [`HealthStatus`].

use super::{check_liveness, read_state, write_state, HealthConfig, HealthStatus};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// Run the health monitor loop.
///
/// - Writes `HealthStatus::Starting` immediately.
/// - Sleeps through the `start_period_secs` grace period.
/// - Then polls every `interval_secs`, joining the container namespaces for
///   each check via [`super::exec::exec_in_container`].
/// - Transitions `Starting → Healthy` when a check passes; tracks consecutive
///   failures and transitions to `Unhealthy` after `retries` failures.
/// - Stops when `stop` is set (container exited) or the process disappears.
pub fn run_health_monitor(name: String, pid: i32, config: HealthConfig, stop: Arc<AtomicBool>) {
    log::debug!(
        "health: monitor starting for '{}' (pid={}, interval={}s, timeout={}s, retries={})",
        name,
        pid,
        config.interval_secs,
        config.timeout_secs,
        config.retries
    );

    update_health(&name, HealthStatus::Starting);

    // Grace period: skip checks while the container is warming up.
    if config.start_period_secs > 0 {
        sleep_interruptible(config.start_period_secs, &stop);
    }
    if stop.load(Ordering::Relaxed) {
        return;
    }

    let mut consecutive_failures: u32 = 0;
    let mut current_status = HealthStatus::Starting;

    loop {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        if !check_liveness(pid) {
            log::debug!(
                "health: container '{}' (pid={}) gone — stopping monitor",
                name,
                pid
            );
            break;
        }

        let passed = run_probe(pid, &config);

        let new_status = if passed {
            consecutive_failures = 0;
            HealthStatus::Healthy
        } else {
            consecutive_failures += 1;
            if consecutive_failures >= config.retries {
                HealthStatus::Unhealthy
            } else {
                // Still within retry budget — stay in current state.
                current_status.clone()
            }
        };

        if new_status != current_status {
            log::info!(
                "health: '{}' → {:?} (failures={})",
                name,
                new_status,
                consecutive_failures
            );
            update_health(&name, new_status.clone());
            current_status = new_status;
        }

        sleep_interruptible(config.interval_secs, &stop);
    }

    log::debug!("health: monitor exiting for '{}'", name);
}

// ---------------------------------------------------------------------------
// Probe execution
// ---------------------------------------------------------------------------

/// Run the health check probe inside the container's namespaces.
///
/// Enforces `timeout_secs` via a channel with `recv_timeout`.
/// Returns `false` on timeout.
fn run_probe(pid: i32, config: &HealthConfig) -> bool {
    if config.cmd.is_empty() {
        return false;
    }
    let args = config.cmd.clone();
    let timeout = Duration::from_secs(config.timeout_secs.max(1));

    // Shared slot: the probe thread writes the child's host PID here as soon
    // as it successfully spawns the process, before blocking on wait().
    // On timeout we read the PID and SIGKILL it so the child does not linger.
    let child_pid = Arc::new(AtomicI32::new(0));
    let child_pid_clone = Arc::clone(&child_pid);

    let (tx, rx) = std::sync::mpsc::channel::<bool>();
    std::thread::spawn(move || {
        let result = super::exec::exec_in_container_with_pid_sink(pid, &args, child_pid_clone)
            .unwrap_or(false);
        let _ = tx.send(result);
    });

    match rx.recv_timeout(timeout) {
        Ok(passed) => passed,
        Err(_) => {
            log::warn!("health: probe timed out after {}s", config.timeout_secs);
            let cpid = child_pid.load(Ordering::Relaxed);
            if cpid > 0 {
                log::warn!("health: killing timed-out probe child (pid={})", cpid);
                unsafe { libc::kill(cpid, libc::SIGKILL) };
            }
            false
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Update `state.json` with a new health status.
fn update_health(name: &str, status: HealthStatus) {
    match read_state(name) {
        Ok(mut state) => {
            state.health = Some(status);
            if let Err(e) = write_state(&state) {
                log::warn!("health: failed to write state for '{}': {}", name, e);
            }
        }
        Err(e) => {
            log::warn!("health: failed to read state for '{}': {}", name, e);
        }
    }
}

/// Sleep for `duration_secs`, waking every 100 ms to check the stop flag.
fn sleep_interruptible(duration_secs: u64, stop: &AtomicBool) {
    let total = Duration::from_secs(duration_secs);
    let tick = Duration::from_millis(100);
    let mut elapsed = Duration::ZERO;
    while elapsed < total {
        if stop.load(Ordering::Relaxed) {
            return;
        }
        std::thread::sleep(tick);
        elapsed += tick;
    }
}
