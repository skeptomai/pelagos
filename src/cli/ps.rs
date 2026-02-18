//! `remora ps` — list containers.

use super::{check_liveness, format_age, list_containers, write_state, ContainerStatus};

pub fn cmd_ps(all: bool) -> Result<(), Box<dyn std::error::Error>> {
    let mut states = list_containers();

    // Sort by started_at (lexicographic on ISO 8601 string = chronological).
    states.sort_by(|a, b| a.started_at.cmp(&b.started_at));

    // Refresh liveness for running containers and update stale states on disk.
    for s in &mut states {
        if s.status == ContainerStatus::Running && !check_liveness(s.pid) {
            s.status = ContainerStatus::Exited;
            let _ = write_state(s);
        }
    }

    if !all {
        states.retain(|s| s.status == ContainerStatus::Running);
    }

    if states.is_empty() {
        return Ok(());
    }

    // Column widths
    let name_w = states.iter().map(|s| s.name.len()).max().unwrap_or(4).max(4);
    let rootfs_w = states.iter().map(|s| s.rootfs.len()).max().unwrap_or(6).max(6);
    let cmd_w = 12usize;
    let _started_w = 14usize;

    println!(
        "{:<name_w$}  {:<8}  {:>7}  {:<rootfs_w$}  {:<cmd_w$}  {}",
        "NAME", "STATUS", "PID", "ROOTFS", "COMMAND", "STARTED",
        name_w = name_w, rootfs_w = rootfs_w, cmd_w = cmd_w,
    );

    for s in &states {
        let pid_str = if s.pid > 0 { s.pid.to_string() } else { "-".to_string() };
        let cmd_str = s.command.join(" ");
        let cmd_display = if cmd_str.len() > cmd_w {
            format!("{}…", &cmd_str[..cmd_w - 1])
        } else {
            cmd_str
        };
        let started = format_age(&s.started_at);

        println!(
            "{:<name_w$}  {:<8}  {:>7}  {:<rootfs_w$}  {:<cmd_w$}  {}",
            s.name,
            s.status.to_string(),
            pid_str,
            s.rootfs,
            cmd_display,
            started,
            name_w = name_w,
            rootfs_w = rootfs_w,
            cmd_w = cmd_w,
        );
    }

    Ok(())
}
