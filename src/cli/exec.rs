//! `remora exec` — run a command inside a running container.

use super::{check_liveness, parse_user, read_state, ContainerStatus};
use remora::container::{Command, Namespace, Stdio};
use std::os::unix::io::AsRawFd;
use std::path::PathBuf;

#[derive(Debug, clap::Args)]
pub struct ExecArgs {
    /// Container name
    pub name: String,

    /// Allocate a PTY for interactive use
    #[clap(long, short = 'i')]
    pub interactive: bool,

    /// Environment variable KEY=VALUE (repeatable)
    #[clap(long = "env", short = 'e')]
    pub env: Vec<String>,

    /// Working directory inside the container
    #[clap(long = "workdir", short = 'w')]
    pub workdir: Option<String>,

    /// UID[:GID] to run as (e.g. 1000 or 1000:1000)
    #[clap(long = "user", short = 'u')]
    pub user: Option<String>,

    /// Command and arguments to run
    #[clap(multiple_values = true, required = true, allow_hyphen_values = true)]
    pub args: Vec<String>,
}

pub fn cmd_exec(args: ExecArgs) -> Result<(), Box<dyn std::error::Error>> {
    // 1. Validate container is running
    let state = read_state(&args.name)
        .map_err(|e| format!("container '{}' not found: {}", args.name, e))?;

    if state.status != ContainerStatus::Running || !check_liveness(state.pid) {
        return Err(format!("container '{}' is not running", args.name).into());
    }

    let pid = state.pid;

    // 2. Discover which namespaces the container has
    let ns_entries = discover_namespaces(pid)?;

    // 3. Read the container's environment
    let container_env = read_proc_environ(pid);

    // 4. Build Command
    let exe = &args.args[0];
    let rest = &args.args[1..];

    let mut cmd = Command::new(exe).args(rest);

    // The pre_exec order in container.rs is:
    //   chroot (step 4) → user callback (step 5) → setns (step 6)
    //
    // For exec we need setns(MOUNT) BEFORE chroot so the container's mount
    // table is active.  We handle mount-ns join via a pre_exec callback that
    // does: setns(mnt_fd) → fchdir(root_fd) → chroot(".") → chdir("/").
    // Non-mount namespaces use the normal with_namespace_join() path.
    let mut has_mount_ns = false;
    for (path, ns) in &ns_entries {
        if *ns == Namespace::MOUNT {
            has_mount_ns = true;
        } else {
            cmd = cmd.with_namespace_join(path, *ns);
        }
    }

    // Capture workdir for use in the pre_exec callback.
    let exec_workdir = args.workdir.clone();

    if has_mount_ns {
        // Open both fds in the parent (before fork) — inherited across fork.
        let mnt_ns_path = format!("/proc/{}/ns/mnt", pid);
        let mnt_ns_file = std::fs::File::open(&mnt_ns_path)
            .map_err(|e| format!("open {}: {}", mnt_ns_path, e))?;
        let mnt_ns_fd = mnt_ns_file.as_raw_fd();

        // Open the container's root directory as an fd.  After setns(MOUNT),
        // path-based resolution uses the host root (unchanged by setns).
        // fchdir(root_fd) + chroot(".") is the correct way to enter the
        // container's root — same technique as nsenter(1).
        //
        // IMPORTANT: with PID namespace enabled, state.pid = P (intermediate
        // process), which never called pivot_root — so /proc/P/root is the HOST
        // root.  Use find_root_pid() to find C (P's only child), which did
        // pivot_root and whose /proc/C/root is the container overlay root.
        let root_pid = find_root_pid(pid);
        let root_path = format!("/proc/{}/root", root_pid);
        let root_file =
            std::fs::File::open(&root_path).map_err(|e| format!("open {}: {}", root_path, e))?;
        let root_fd = root_file.as_raw_fd();

        cmd = cmd.with_pre_exec(move || {
            // Keep File objects alive so fds remain valid.
            let _keep_mnt = &mnt_ns_file;
            let _keep_root = &root_file;
            unsafe {
                if libc::setns(mnt_ns_fd, libc::CLONE_NEWNS) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::fchdir(root_fd) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                let dot = std::ffi::CString::new(".").unwrap();
                if libc::chroot(dot.as_ptr()) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                // chdir to the requested workdir (or "/" if none).
                let target = exec_workdir.as_deref().unwrap_or("/");
                let target_c = std::ffi::CString::new(target).unwrap();
                if libc::chdir(target_c.as_ptr()) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
            }
            Ok(())
        });
    } else {
        // No mount namespace to join — access rootfs via procfs.
        let root_pid = find_root_pid(pid);
        cmd = cmd.with_chroot(format!("/proc/{}/root", root_pid));
        // For non-mount-ns exec, use the normal with_cwd mechanism.
        if let Some(ref w) = exec_workdir {
            cmd = cmd.with_cwd(w);
        }
    }

    // Apply container environment as base
    for (k, v) in &container_env {
        cmd = cmd.env(k, v);
    }

    // Apply CLI -e overrides
    for e in &args.env {
        if let Some((k, v)) = e.split_once('=') {
            cmd = cmd.env(k, v);
        } else if let Ok(v) = std::env::var(e) {
            cmd = cmd.env(e, v);
        }
    }

    // User
    if let Some(ref u) = args.user {
        let (uid, gid) = parse_user(u)?;
        cmd = cmd.with_uid(uid);
        if let Some(g) = gid {
            cmd = cmd.with_gid(g);
        }
    }

    // 5. Spawn
    if args.interactive {
        let session = cmd
            .spawn_interactive()
            .map_err(|e| format!("spawn_interactive failed: {}", e))?;
        match session.run() {
            Ok(status) => {
                let code = status.code().unwrap_or(0);
                std::process::exit(code);
            }
            Err(e) => Err(format!("interactive session failed: {}", e).into()),
        }
    } else {
        cmd = cmd
            .stdin(Stdio::Inherit)
            .stdout(Stdio::Inherit)
            .stderr(Stdio::Inherit);

        let mut child = cmd
            .spawn()
            .map_err(|e| format!("exec spawn failed: {}", e))?;
        let exit = child
            .wait()
            .map_err(|e| format!("exec wait failed: {}", e))?;
        let code = exit.code().unwrap_or(1);
        std::process::exit(code);
    }
}

/// Run `args` in the container identified by `pid`'s namespaces.
///
/// Returns:
/// - `Some(true)` — command exited with status 0
/// - `Some(false)` — command exited non-zero
/// - `None` — the container is gone (pid dead, namespaces unreachable)
///
/// Discards all output (stdin/stdout/stderr → /dev/null).
pub fn exec_in_container(pid: i32, args: &[String]) -> Option<bool> {
    if args.is_empty() || pid <= 0 {
        return None;
    }

    let ns_entries = discover_namespaces(pid).ok()?;
    // If we can't discover any namespaces the container is probably gone.
    // But allow proceeding (ns_entries may be empty if no namespaces differ).

    let mut cmd = Command::new(&args[0]).args(&args[1..]);
    cmd = cmd
        .stdin(Stdio::Null)
        .stdout(Stdio::Null)
        .stderr(Stdio::Null);

    let mut has_mount_ns = false;
    for (path, ns) in &ns_entries {
        if *ns == Namespace::MOUNT {
            has_mount_ns = true;
        } else {
            cmd = cmd.with_namespace_join(path, *ns);
        }
    }

    if has_mount_ns {
        let mnt_ns_path = format!("/proc/{}/ns/mnt", pid);
        let mnt_ns_file = std::fs::File::open(&mnt_ns_path).ok()?;
        let mnt_ns_fd = mnt_ns_file.as_raw_fd();

        // See cmd_exec for the PID-namespace / intermediate-process explanation.
        let root_pid = find_root_pid(pid);
        let root_path = format!("/proc/{}/root", root_pid);
        let root_file = std::fs::File::open(&root_path).ok()?;
        let root_fd = root_file.as_raw_fd();

        cmd = cmd.with_pre_exec(move || {
            let _keep_mnt = &mnt_ns_file;
            let _keep_root = &root_file;
            unsafe {
                if libc::setns(mnt_ns_fd, libc::CLONE_NEWNS) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::fchdir(root_fd) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                let dot = std::ffi::CString::new(".").unwrap();
                if libc::chroot(dot.as_ptr()) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                let root_c = std::ffi::CString::new("/").unwrap();
                if libc::chdir(root_c.as_ptr()) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
            }
            Ok(())
        });
    } else {
        let root_pid = find_root_pid(pid);
        cmd = cmd.with_chroot(format!("/proc/{}/root", root_pid));
    }

    match cmd.spawn() {
        Ok(mut child) => child.wait().map(|s| s.success()).ok(),
        Err(_) => None,
    }
}

/// Given a PID, return the PID of the process that actually performed chroot/pivot_root.
///
/// With PID namespace enabled, `state.pid` is the intermediate process P, which
/// never calls pivot_root (that is done by C, PID 1 inside the container).  P has
/// exactly one child (C), so `/proc/P/root` still points at the HOST root — not the
/// container's overlay.  We detect this by checking `/proc/{pid}/task/{pid}/children`:
/// if there is exactly one child, that child is C and its `/proc/{child}/root` is the
/// correct container root.
///
/// Without PID namespace `state.pid` IS the container process, which may or may not
/// have children.  In that case we use `pid` directly (and if it has children those
/// processes are also inside the container, so either PID's root is correct).
fn find_root_pid(pid: i32) -> i32 {
    let path = format!("/proc/{}/task/{}/children", pid, pid);
    if let Ok(content) = std::fs::read_to_string(&path) {
        let children: Vec<i32> = content
            .split_whitespace()
            .filter_map(|s| s.parse().ok())
            .collect();
        if children.len() == 1 {
            return children[0];
        }
    }
    pid
}

/// Compare `/proc/{pid}/ns/{type}` inodes against `/proc/1/ns/{type}` to discover
/// which namespaces the container process is in (i.e., different from init).
pub fn discover_namespaces(
    pid: i32,
) -> Result<Vec<(PathBuf, Namespace)>, Box<dyn std::error::Error>> {
    let ns_map: &[(&str, Namespace)] = &[
        ("mnt", Namespace::MOUNT),
        ("uts", Namespace::UTS),
        ("ipc", Namespace::IPC),
        ("net", Namespace::NET),
        ("pid", Namespace::PID),
        ("user", Namespace::USER),
        ("cgroup", Namespace::CGROUP),
    ];

    let mut result = Vec::new();

    for &(ns_name, ns_flag) in ns_map {
        let container_ns = format!("/proc/{}/ns/{}", pid, ns_name);
        let init_ns = format!("/proc/1/ns/{}", ns_name);

        let container_ino = match std::fs::metadata(&container_ns) {
            Ok(m) => {
                use std::os::unix::fs::MetadataExt;
                m.ino()
            }
            Err(_) => continue,
        };
        let init_ino = match std::fs::metadata(&init_ns) {
            Ok(m) => {
                use std::os::unix::fs::MetadataExt;
                m.ino()
            }
            Err(_) => continue,
        };

        if container_ino != init_ino {
            result.push((PathBuf::from(container_ns), ns_flag));
        }
    }

    Ok(result)
}

/// Read `/proc/{pid}/environ` — NUL-separated KEY=VALUE pairs.
fn read_proc_environ(pid: i32) -> Vec<(String, String)> {
    let path = format!("/proc/{}/environ", pid);
    let data = match std::fs::read(&path) {
        Ok(d) => d,
        Err(_) => return Vec::new(),
    };

    data.split(|&b| b == 0)
        .filter(|s| !s.is_empty())
        .filter_map(|entry| {
            let s = String::from_utf8_lossy(entry);
            let (k, v) = s.split_once('=')?;
            Some((k.to_string(), v.to_string()))
        })
        .collect()
}
