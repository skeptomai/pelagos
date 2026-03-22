//! `pelagos start` — restart an exited container.
//!
//! Reads the saved `SpawnConfig` from the container's `state.json`, converts it
//! back into `RunArgs`, and calls `cmd_run` in detached mode.  The container
//! always restarts detached; use `pelagos logs -f <name>` to follow output.
//!
//! Overlay semantics: the restarted container gets a fresh upper/work dir on top
//! of the same image layers.  Writable-layer changes from the previous run are
//! NOT preserved (future enhancement).

use super::run::{cmd_run, RunArgs};
use super::{read_state, ContainerStatus};

/// Restart one or more exited pelagos containers by name.
///
/// Returns `Ok(())` after all named containers have been started.  If any
/// name fails the error is returned immediately (remaining names are not started).
pub fn cmd_start(names: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    for name in names {
        start_one(name)?;
    }
    Ok(())
}

fn start_one(name: &str) -> Result<(), Box<dyn std::error::Error>> {
    let state = read_state(name).map_err(|_| format!("container '{}' not found", name))?;

    match state.status {
        ContainerStatus::Running => {
            return Err(format!(
                "container '{}' is already running (pid {})",
                name, state.pid
            )
            .into());
        }
        ContainerStatus::Exited => {}
    }

    let sc = state.spawn_config.ok_or_else(|| {
        format!(
            "container '{}' has no spawn config — cannot restart \
             (was it created with an older version of pelagos?)",
            name
        )
    })?;

    let mut run_args = spawn_config_to_run_args(name, sc, &state.rootfs);
    // Reuse the persisted writable layer if it still exists on disk.
    run_args.upper_dir = state.upper_dir.filter(|p| p.is_dir());
    cmd_run(run_args)
}

/// Convert a `SpawnConfig` back into `RunArgs` for restart.
///
/// The restarted container always runs detached.  Resource limits (memory,
/// cpus, ulimits) are not persisted in SpawnConfig and will use defaults on
/// restart — this matches the VS Code devcontainer use case where the container
/// is restarted by the IDE, not by the user tuning resources.
fn spawn_config_to_run_args(
    container_name: &str,
    sc: super::SpawnConfig,
    rootfs_label: &str,
) -> RunArgs {
    // Reconstruct the positional args vector.
    // For image containers: [image_ref, exe, args...]
    // For rootfs containers: [exe, args...] (rootfs set via RunArgs.rootfs)
    let (rootfs_field, args) = if let Some(ref image_ref) = sc.image {
        let mut a = vec![image_ref.clone(), sc.exe.clone()];
        a.extend(sc.args.iter().cloned());
        (None, a)
    } else {
        let mut a = vec![sc.exe.clone()];
        a.extend(sc.args.iter().cloned());
        (Some(rootfs_label.to_string()), a)
    };

    RunArgs {
        name: Some(container_name.to_string()),
        detach: true,
        rm: false,
        interactive: false,
        network: sc.network,
        publish: sc.publish,
        nat: sc.nat,
        dns: sc.dns,
        volume: sc.volume,
        bind: sc.bind,
        bind_ro: sc.bind_ro,
        tmpfs: sc.tmpfs,
        read_only: sc.read_only,
        env: sc.env,
        env_file: None,
        workdir: sc.working_dir,
        user: sc.user,
        hostname: sc.hostname,
        memory: None,
        cpus: None,
        cpu_shares: None,
        pids_limit: None,
        ulimit: vec![],
        cap_drop: sc.cap_drop,
        cap_add: sc.cap_add,
        security_opt: sc.security_opt,
        apparmor_profile: None,
        selinux_label: None,
        link: vec![],
        sysctl: vec![],
        masked_path: vec![],
        dns_backend: None,
        rootfs: rootfs_field,
        args,
        label: sc.labels,
        attach: vec![],
        sig_proxy: None,
        upper_dir: None, // set by start_one after this call
    }
}
