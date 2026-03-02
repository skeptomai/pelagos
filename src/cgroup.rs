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
    fs::MaxValue,
    fs::{
        cgroup_builder::CgroupBuilder, cpu::CpuController, cpuset::CpuSetController, hierarchies,
        memory::MemController, net_cls::NetClsController, net_prio::NetPrioController,
        pid::PidController, Cgroup,
    },
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

    /// Memory + swap combined limit in bytes.
    /// Maps to `memory.swap.max` on v2, `memory.memsw.limit_in_bytes` on v1.
    /// -1 means unlimited swap on top of the memory limit.
    pub memory_swap: Option<i64>,

    /// Soft memory limit / low-water mark in bytes.
    /// Maps to `memory.low` on v2, `memory.soft_limit_in_bytes` on v1.
    pub memory_reservation: Option<i64>,

    /// Swappiness hint (0–100) for the memory controller (v1 only; silently ignored on v2).
    pub memory_swappiness: Option<u64>,

    /// CPU weight (1–10000). Maps to `cpu.weight` in v2, `cpu.shares` in v1.
    /// Default kernel weight is 100. Higher values get proportionally more CPU.
    pub cpu_shares: Option<u64>,

    /// CPU quota: `(quota_microseconds, period_microseconds)`.
    /// Example: `(50_000, 100_000)` = 50% of one CPU core per 100 ms period.
    pub cpu_quota: Option<(i64, u64)>,

    /// CPUs this cgroup may use (cpuset string, e.g. `"0-3,6"`).
    /// Maps to `cpuset.cpus`.
    pub cpuset_cpus: Option<String>,

    /// Memory nodes this cgroup may use (cpuset string, e.g. `"0-1"`).
    /// Maps to `cpuset.mems`.
    pub cpuset_mems: Option<String>,

    /// Maximum number of live processes/threads in the cgroup (`pids.max`).
    pub pids_limit: Option<u64>,

    /// Block I/O weight (10–1000). Maps to `io.weight` on v2, `blkio.weight` on v1.
    pub blkio_weight: Option<u16>,

    /// Per-device block I/O throttle rules (major, minor, bytes_per_sec).
    pub blkio_throttle_read_bps: Vec<(u64, u64, u64)>,
    pub blkio_throttle_write_bps: Vec<(u64, u64, u64)>,
    pub blkio_throttle_read_iops: Vec<(u64, u64, u64)>,
    pub blkio_throttle_write_iops: Vec<(u64, u64, u64)>,

    /// Device cgroup allow/deny rules.
    /// Each entry: (allow, type_char, major, minor, access_string).
    /// Silently ignored on cgroupv2 (requires eBPF; not implemented).
    pub device_rules: Vec<CgroupDeviceRule>,

    /// net_cls classid (v1 only; silently ignored on v2).
    pub net_classid: Option<u64>,

    /// net_prio interface priority map (v1 only; silently ignored on v2).
    pub net_priorities: Vec<(String, u64)>,

    /// Explicit cgroup path from OCI `linux.cgroupsPath`.
    /// If set, used as-is as the cgroup name/path; otherwise defaults to `remora-{pid}`.
    pub path: Option<String>,
}

/// A single device cgroup allow/deny rule.
#[derive(Debug, Clone)]
pub struct CgroupDeviceRule {
    pub allow: bool,
    /// Device type: 'c' (char), 'b' (block), 'a' (all).
    pub kind: char,
    /// -1 means wildcard.
    pub major: i64,
    /// -1 means wildcard.
    pub minor: i64,
    /// Access string: combination of 'r', 'w', 'm'.
    pub access: String,
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
    let name = cfg
        .path
        .clone()
        .unwrap_or_else(|| format!("remora-{}", child_pid));
    let hier = hierarchies::auto();

    let mut builder = CgroupBuilder::new(&name);

    // --- Memory ---
    if cfg.memory_limit.is_some()
        || cfg.memory_swap.is_some()
        || cfg.memory_reservation.is_some()
        || cfg.memory_swappiness.is_some()
    {
        let mut mb = builder.memory();
        if let Some(limit) = cfg.memory_limit {
            mb = mb.memory_hard_limit(limit);
        }
        if let Some(swap) = cfg.memory_swap {
            mb = mb.memory_swap_limit(swap);
        }
        if let Some(res) = cfg.memory_reservation {
            mb = mb.memory_soft_limit(res);
        }
        if let Some(swp) = cfg.memory_swappiness {
            mb = mb.swappiness(swp);
        }
        builder = mb.done();
    }

    // --- CPU ---
    let has_cpu = cfg.cpu_shares.is_some() || cfg.cpu_quota.is_some();
    if has_cpu {
        let mut cb = builder.cpu();
        if let Some(shares) = cfg.cpu_shares {
            cb = cb.shares(shares);
        }
        if let Some((quota, period)) = cfg.cpu_quota {
            cb = cb.quota(quota).period(period);
        }
        builder = cb.done();
    }

    // --- PIDs ---
    if let Some(max_pids) = cfg.pids_limit {
        builder = builder
            .pid()
            .maximum_number_of_processes(MaxValue::Value(max_pids as i64))
            .done();
    }

    // --- Block I/O ---
    let has_blkio = cfg.blkio_weight.is_some()
        || !cfg.blkio_throttle_read_bps.is_empty()
        || !cfg.blkio_throttle_write_bps.is_empty()
        || !cfg.blkio_throttle_read_iops.is_empty()
        || !cfg.blkio_throttle_write_iops.is_empty();
    if has_blkio {
        let mut bb = builder.blkio();
        if let Some(w) = cfg.blkio_weight {
            bb = bb.weight(w);
        }
        if !cfg.blkio_throttle_read_bps.is_empty() {
            bb = bb.throttle_bps();
            for &(major, minor, rate) in &cfg.blkio_throttle_read_bps {
                bb = bb.read(major, minor, rate);
            }
        }
        if !cfg.blkio_throttle_write_bps.is_empty() {
            bb = bb.throttle_bps();
            for &(major, minor, rate) in &cfg.blkio_throttle_write_bps {
                bb = bb.write(major, minor, rate);
            }
        }
        if !cfg.blkio_throttle_read_iops.is_empty() {
            bb = bb.throttle_iops();
            for &(major, minor, rate) in &cfg.blkio_throttle_read_iops {
                bb = bb.read(major, minor, rate);
            }
        }
        if !cfg.blkio_throttle_write_iops.is_empty() {
            bb = bb.throttle_iops();
            for &(major, minor, rate) in &cfg.blkio_throttle_write_iops {
                bb = bb.write(major, minor, rate);
            }
        }
        builder = bb.done();
    }

    // --- Network (v1 only; silently skip on v2-only systems) ---
    let has_net = cfg.net_classid.is_some() || !cfg.net_priorities.is_empty();
    if has_net {
        let mut nb = builder.network();
        if let Some(class_id) = cfg.net_classid {
            nb = nb.class_id(class_id);
        }
        for (name, prio) in &cfg.net_priorities {
            nb = nb.priority(name.clone(), *prio);
        }
        builder = nb.done();
    }

    // --- Device cgroup (v1 only; silently skip on v2-only systems) ---
    if !cfg.device_rules.is_empty() {
        use cgroups_rs::fs::devices::{DevicePermissions, DeviceType};
        let mut db = builder.devices();
        for rule in &cfg.device_rules {
            let devtype = match rule.kind {
                'b' => DeviceType::Block,
                'c' => DeviceType::Char,
                _ => DeviceType::All,
            };
            let access = DevicePermissions::from_str(&rule.access)
                .unwrap_or_else(|_| DevicePermissions::all());
            db = db.device(rule.major, rule.minor, devtype, rule.allow, access);
        }
        builder = db.done();
    }

    let cg = builder
        .build(hier)
        .map_err(|e| io::Error::other(format!("cgroup create '{}': {}", name, e)))?;

    // --- CpuSet (must be applied via controller_of after cgroup is created) ---
    if cfg.cpuset_cpus.is_some() || cfg.cpuset_mems.is_some() {
        if let Some(cs) = cg.controller_of::<CpuSetController>() {
            if let Some(ref cpus) = cfg.cpuset_cpus {
                if let Err(e) = cs.set_cpus(cpus) {
                    log::warn!("cgroup cpuset.cpus={} failed (non-fatal): {}", cpus, e);
                }
            }
            if let Some(ref mems) = cfg.cpuset_mems {
                if let Err(e) = cs.set_mems(mems) {
                    log::warn!("cgroup cpuset.mems={} failed (non-fatal): {}", mems, e);
                }
            }
        } else {
            log::debug!("cpuset controller unavailable; cpus/mems not applied");
        }
    }

    // --- Net class / prio validation (v1 only; log if unavailable) ---
    if cfg.net_classid.is_some() && cg.controller_of::<NetClsController>().is_none() {
        log::debug!("net_cls controller unavailable (v2-only system); classid not applied");
    }
    if !cfg.net_priorities.is_empty() && cg.controller_of::<NetPrioController>().is_none() {
        log::debug!("net_prio controller unavailable (v2-only system); priorities not applied");
    }

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
