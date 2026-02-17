//! Cgroups v2 resource management for containers.
//!
//! This module wraps the `cgroups-rs` crate to provide an ergonomic interface for
//! creating, configuring, and tearing down cgroups for containerized processes.
//!
//! The cgroup lifecycle is managed entirely from the **parent process**:
//! 1. [`CgroupConfig`] is built via [`Command`] builder methods.
//! 2. After fork+exec, the parent creates the cgroup and adds the child PID.
//! 3. After the child exits, the parent deletes the cgroup.
//!
//! # Naming
//!
//! Each container's cgroup is named `remora-{child_pid}` to guarantee uniqueness
//! across concurrent containers.

use cgroups_rs::{
    fs::{
        cgroup_builder::CgroupBuilder,
        cpu::CpuController,
        hierarchies,
        memory::MemController,
        pid::PidController,
        Cgroup,
    },
    fs::MaxValue,
    CgroupPid,
};
use std::io;

/// Resource limits to apply via cgroups v2.
///
/// All fields are optional — set only what you need. Unset fields use the
/// kernel's default (no limit). Coexists with rlimits without conflict.
///
/// # Examples
///
/// ```ignore
/// Command::new("/bin/sh")
///     .with_cgroup_memory(256 * 1024 * 1024)   // 256 MB
///     .with_cgroup_cpu_shares(512)              // half default weight
///     .with_cgroup_pids_limit(64)               // max 64 processes
///     .spawn()?;
/// ```
#[derive(Debug, Clone, Default)]
pub struct CgroupConfig {
    /// Memory hard limit in bytes (`memory.max`).
    /// The process is OOM-killed if it exceeds this limit.
    pub memory_limit: Option<i64>,

    /// CPU weight (1–10000). Maps to `cpu.weight` in v2, `cpu.shares` in v1.
    /// Default kernel weight is 100. Higher values get proportionally more CPU.
    pub cpu_shares: Option<u64>,

    /// CPU quota: `(quota_microseconds, period_microseconds)`.
    /// Example: `(50_000, 100_000)` = 50% of one CPU core per 100 ms period.
    pub cpu_quota: Option<(i64, u64)>,

    /// Maximum number of live processes/threads in the cgroup (`pids.max`).
    pub pids_limit: Option<u64>,
}

/// Create a cgroup named `remora-{child_pid}`, apply configured limits, and add
/// the child process to it.
///
/// Returns the live [`Cgroup`] handle — the caller must call [`teardown_cgroup`]
/// after the child exits.
///
/// # Errors
///
/// Returns an error if the cgroup cannot be created (e.g. missing permissions,
/// cgroup fs not mounted) or if the PID cannot be added.
pub fn setup_cgroup(cfg: &CgroupConfig, child_pid: u32) -> io::Result<Cgroup> {
    let name = format!("remora-{}", child_pid);
    let hier = hierarchies::auto();

    let mut builder = CgroupBuilder::new(&name);

    if let Some(mem) = cfg.memory_limit {
        builder = builder.memory().memory_hard_limit(mem).done();
    }

    match (cfg.cpu_shares, cfg.cpu_quota) {
        (Some(shares), Some((quota, period))) => {
            builder = builder.cpu().shares(shares).quota(quota).period(period).done();
        }
        (Some(shares), None) => {
            builder = builder.cpu().shares(shares).done();
        }
        (None, Some((quota, period))) => {
            builder = builder.cpu().quota(quota).period(period).done();
        }
        (None, None) => {}
    }

    if let Some(max_pids) = cfg.pids_limit {
        builder = builder
            .pid()
            .maximum_number_of_processes(MaxValue::Value(max_pids as i64))
            .done();
    }

    let cg = builder
        .build(hier)
        .map_err(|e| io::Error::other(format!("cgroup create '{}': {}", name, e)))?;

    cg.add_task_by_tgid(CgroupPid::from(child_pid as u64))
        .map_err(|e| io::Error::other(format!("cgroup add_task pid={}: {}", child_pid, e)))?;

    Ok(cg)
}

/// Delete a cgroup after the container process has exited.
///
/// Errors are logged at `warn` level but not propagated — cleanup failures
/// are non-fatal since the kernel will reclaim resources automatically once
/// all tasks have exited.
pub fn teardown_cgroup(cg: Cgroup) {
    if let Err(e) = cg.delete() {
        log::warn!("cgroup delete failed (non-fatal): {}", e);
    }
}

/// Resource usage statistics read from a container's live cgroup.
#[derive(Debug, Clone, Default)]
pub struct ResourceStats {
    /// Current memory usage in bytes (`memory.current`).
    pub memory_current_bytes: u64,
    /// Total CPU time consumed in nanoseconds (from `cpu.stat usage_usec * 1000`).
    pub cpu_usage_ns: u64,
    /// Current number of live processes/threads (`pids.current`).
    pub pids_current: u64,
}

/// Read current resource usage from a container's cgroup.
///
/// Controllers that are unavailable (e.g. not enabled in the hierarchy) return 0
/// for their respective fields rather than failing.
pub fn read_stats(cg: &Cgroup) -> io::Result<ResourceStats> {
    let mut stats = ResourceStats::default();

    // Memory: usage_in_bytes from memory.current (v2) or memory.usage_in_bytes (v1)
    if let Some(mem_ctrl) = cg.controller_of::<MemController>() {
        stats.memory_current_bytes = mem_ctrl.memory_stat().usage_in_bytes;
    }

    // CPU: parse "usage_usec N" from the raw cpu.stat string
    if let Some(cpu_ctrl) = cg.controller_of::<CpuController>() {
        let raw = cpu_ctrl.cpu().stat;
        for line in raw.lines() {
            if let Some(rest) = line.strip_prefix("usage_usec ") {
                if let Ok(usec) = rest.trim().parse::<u64>() {
                    stats.cpu_usage_ns = usec.saturating_mul(1000);
                }
                break;
            }
        }
    }

    // PIDs: pids.current
    if let Some(pid_ctrl) = cg.controller_of::<PidController>() {
        if let Ok(current) = pid_ctrl.get_pid_current() {
            stats.pids_current = current;
        }
    }

    Ok(stats)
}
