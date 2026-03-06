//! `pelagos ps` — list containers.
//! `pelagos container inspect` — show detailed container state as JSON.

use super::{
    check_liveness, format_age, list_containers, read_state, write_state, ContainerStatus,
};

pub fn cmd_ps(all: bool, json: bool) -> Result<(), Box<dyn std::error::Error>> {
    let mut states = list_containers();

    // Sort by started_at (lexicographic on ISO 8601 string = chronological).
    states.sort_by(|a, b| a.started_at.cmp(&b.started_at));

    // Refresh liveness for running containers and update stale states on disk.
    // When pid==0 the watcher child hasn't written the container PID yet (detached
    // startup race); fall back to watcher_pid so we don't falsely mark as Exited.
    for s in &mut states {
        if s.status == ContainerStatus::Running {
            let pid_to_check = if s.pid > 0 { s.pid } else { s.watcher_pid };
            if !check_liveness(pid_to_check) {
                s.status = ContainerStatus::Exited;
                let _ = write_state(s);
            }
        }
    }

    if !all {
        states.retain(|s| s.status == ContainerStatus::Running);
    }

    if json {
        println!("{}", serde_json::to_string_pretty(&states)?);
        return Ok(());
    }

    if states.is_empty() {
        if all {
            println!("No containers found. Use 'pelagos run' to start one.");
        } else {
            println!("No containers running. Use 'pelagos run' to start one.");
        }
        return Ok(());
    }

    // Column widths
    let name_w = states
        .iter()
        .map(|s| s.name.len())
        .max()
        .unwrap_or(4)
        .max(4);
    let rootfs_w = states
        .iter()
        .map(|s| s.rootfs.len())
        .max()
        .unwrap_or(6)
        .max(6);
    let cmd_w = 12usize;
    let health_w = 10usize;

    println!(
        "{:<name_w$}  {:<8}  {:>7}  {:<rootfs_w$}  {:<cmd_w$}  {:<health_w$}  STARTED",
        "NAME",
        "STATUS",
        "PID",
        "ROOTFS",
        "COMMAND",
        "HEALTH",
        name_w = name_w,
        rootfs_w = rootfs_w,
        cmd_w = cmd_w,
        health_w = health_w,
    );

    for s in &states {
        let pid_str = if s.pid > 0 {
            s.pid.to_string()
        } else {
            "-".to_string()
        };
        let cmd_str = s.command.join(" ");
        let cmd_display = if cmd_str.len() > cmd_w {
            format!("{}…", &cmd_str[..cmd_w - 1])
        } else {
            cmd_str
        };
        let started = format_age(&s.started_at);
        let health_str = match &s.health {
            Some(super::HealthStatus::Starting) => "starting",
            Some(super::HealthStatus::Healthy) => "healthy",
            Some(super::HealthStatus::Unhealthy) => "unhealthy",
            Some(super::HealthStatus::None) | None => "",
        };

        println!(
            "{:<name_w$}  {:<8}  {:>7}  {:<rootfs_w$}  {:<cmd_w$}  {:<health_w$}  {}",
            s.name,
            s.status.to_string(),
            pid_str,
            s.rootfs,
            cmd_display,
            health_str,
            started,
            name_w = name_w,
            rootfs_w = rootfs_w,
            cmd_w = cmd_w,
            health_w = health_w,
        );
    }

    Ok(())
}

pub fn cmd_inspect(name: &str) -> Result<(), Box<dyn std::error::Error>> {
    let mut state = read_state(name).map_err(|e| format!("container '{}': {}", name, e))?;

    // Refresh liveness.
    if state.status == ContainerStatus::Running && !check_liveness(state.pid) {
        state.status = ContainerStatus::Exited;
        let _ = write_state(&state);
    }

    println!("{}", serde_json::to_string_pretty(&state)?);
    Ok(())
}
