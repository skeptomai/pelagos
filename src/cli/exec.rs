//! `pelagos exec` — run a command inside a running container.

use super::{check_liveness, parse_user, read_state, verify_pid_not_recycled, ContainerStatus};
use pelagos::container::{Command, Namespace, Stdio};
use pelagos::image;
use std::os::unix::io::AsRawFd;
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicI32, Ordering},
    Arc,
};

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

    /// `UID[:GID]` to run as (e.g. 1000 or 1000:1000)
    #[clap(long = "user", short = 'u')]
    pub user: Option<String>,

    /// Command and arguments to run
    #[clap(multiple_values = true, required = true, allow_hyphen_values = true)]
    pub args: Vec<String>,
}

pub fn cmd_exec(args: ExecArgs) -> Result<(), Box<dyn std::error::Error>> {
    // 1. Validate container is running.
    //
    // In detached mode there is a brief window where pid==0: the watcher has
    // forked and written state.json (so ps shows it "running" via watcher_pid)
    // but hasn't yet spawned the container process and updated the PID.  Poll
    // for up to 2 s so that `exec` called immediately after `run --detach`
    // doesn't spuriously fail.
    let state = {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let mut s = read_state(&args.name)
            .map_err(|e| format!("container '{}' not found: {}", args.name, e))?;
        while s.pid == 0
            && s.status == ContainerStatus::Running
            && check_liveness(s.watcher_pid)
            && std::time::Instant::now() < deadline
        {
            std::thread::sleep(std::time::Duration::from_millis(50));
            s = read_state(&args.name)
                .map_err(|e| format!("container '{}' not found: {}", args.name, e))?;
        }
        s
    };

    if state.status != ContainerStatus::Running || !check_liveness(state.pid) {
        return Err(format!("container '{}' is not running", args.name).into());
    }

    let pid = state.pid;

    // 2a. Verify PID identity before opening any namespace fds.
    //     If the container exited and the OS recycled its PID, the mount-namespace
    //     inode will differ from what was recorded at spawn time.  This catches the
    //     race before we silently enter the wrong process's namespaces.
    verify_pid_not_recycled(pid, &state).map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;

    // 2b. Discover which namespaces the container has
    let ns_entries = discover_namespaces(pid)?;

    // 3. Build the base environment for the exec'd process.
    //
    // Priority (lowest → highest):
    //   a) Image config Env (Dockerfile ENV instructions) — authoritative base
    //   b) Container's live /proc/<pid>/environ — used only for rootfs containers
    //      (no image manifest) and to pick up any runtime vars not in the image
    //   c) CLI -e overrides — always win
    //
    // For image-based containers we load the image manifest and use its config.env
    // as the base.  This matches Docker's `exec` semantics: the exec'd process
    // inherits the OCI image config env regardless of what the running init process
    // currently has in /proc/environ (which may reflect a pre-#114 spawn where PATH
    // was incorrectly overwritten).
    //
    // For rootfs-based containers (no spawn_config.image), we fall back to the
    // live /proc/environ of the container's actual process (grandchild in PID-ns).
    let image_env: Vec<(String, String)> = state
        .spawn_config
        .as_ref()
        .and_then(|sc| sc.image.as_deref())
        .and_then(|img| image::load_image(img).ok())
        .map(|m| {
            m.config
                .env
                .iter()
                .filter_map(|e| {
                    let (k, v) = e.split_once('=')?;
                    Some((k.to_string(), v.to_string()))
                })
                .collect()
        })
        .unwrap_or_default();

    // For containers with a PID namespace, state.pid is the INTERMEDIATE process
    // (it ran pre_exec but never called exec(), so its /proc/pid/environ reflects
    // the fork-inherited host environment, not the --env vars).  The actual
    // container (grandchild) has the correct environ.  Find it via the children
    // list, falling back to state.pid if no grandchild exists (non-PID-ns case).
    let environ_pid = {
        let children_path = format!("/proc/{}/task/{}/children", pid, pid);
        std::fs::read_to_string(&children_path)
            .ok()
            .and_then(|s| s.split_whitespace().next()?.parse::<i32>().ok())
            .unwrap_or(pid)
    };
    // Read the container's live /proc/environ to pick up runtime --env vars
    // (e.g. MY_EXEC_VAR=hello set via `pelagos run --env`).  These are NOT
    // in the image config and must still be inherited by exec'd processes.
    //
    // Merge strategy: image_env is the base, /proc/environ supplies any key
    // that is NOT already in image_env.  This ensures:
    //   • PATH (and other Dockerfile ENV vars) always come from the image config
    //     even when the container was started before the #114 fix (where the
    //     live environ had the wrong default PATH overwriting the image's PATH).
    //   • Runtime --env vars that are absent from the image config are still
    //     inherited by the exec'd process.
    //   • CLI -e flags override everything (applied after this block).
    let proc_env = read_proc_environ(environ_pid);
    let image_env_keys: std::collections::HashSet<&str> =
        image_env.iter().map(|(k, _)| k.as_str()).collect();
    let container_env: Vec<(String, String)> = proc_env
        .into_iter()
        .filter(|(k, _)| !image_env_keys.contains(k.as_str()))
        .collect();

    // 4. Build Command
    let exe = &args.args[0];
    let rest = &args.args[1..];

    let mut cmd = Command::new(exe).args(rest);

    // Rootless exec namespace join ordering constraint
    // ------------------------------------------------
    // container.rs pre_exec processes namespaces in this order:
    //   (a) join_ns_fds loop — step 4.855 in spawn(), step 6138 in
    //       spawn_interactive() — runs BEFORE the user_pre_exec callback
    //   (b) user_pre_exec callback — where we join USER + MOUNT + chroot
    //
    // Joining any namespace (UTS, IPC, NET, PID) requires CAP_SYS_ADMIN in
    // its *owning* user namespace.  For a rootless container every namespace
    // is owned by the container's user namespace.  We don't have those
    // capabilities until we join the user namespace in step (b) — but
    // join_ns_fds runs in step (a), before we join USER.
    //
    // Fix: in rootless mode we bypass join_ns_fds entirely and instead
    // handle all namespace joins (USER, MOUNT, UTS, IPC, NET, CGROUP) in
    // the user_pre_exec callback, in the correct order:
    //   1. setns(USER)  — gain caps in container's user namespace
    //   2. setns(MOUNT) — join mount namespace
    //   3. fchdir + chroot — enter container rootfs
    //   4. setns(UTS/IPC/NET/CGROUP) — now have caps, these succeed
    //
    // PID is always skipped: joining a PID namespace via setns() only
    // updates pid_for_children; a subsequent fork() is required to actually
    // enter it.  container.rs handles this double-fork at step 1.65, which
    // also runs BEFORE the user_pre_exec callback — the same ordering
    // problem.  The exec'd process therefore runs in the host PID namespace
    // (known limitation for rootless exec).
    let is_rootless = unsafe { libc::getuid() } != 0;
    // In rootless mode, any namespace owned by the container's user namespace
    // must be joined AFTER the user namespace.  Collect them for the callback.
    let has_user_ns = ns_entries.iter().any(|(_, ns)| *ns == Namespace::USER);

    let mut has_mount_ns = false;
    let mut user_ns_path: Option<PathBuf> = None;
    // Late namespaces: joined after USER inside the user_pre_exec callback.
    let mut late_ns_paths: Vec<(PathBuf, Namespace)> = Vec::new();
    // PID namespace path for root exec — joined in the parent before fork.
    let mut pid_ns_path_for_root: Option<PathBuf> = None;
    for (path, ns) in &ns_entries {
        match *ns {
            Namespace::MOUNT => has_mount_ns = true,
            Namespace::USER => {
                // Handled in user_pre_exec callback, not via join_ns_fds.
                user_ns_path = Some(path.clone());
            }
            Namespace::PID => {
                if !is_rootless {
                    // Root exec: join in the parent process before fork.
                    // setns(CLONE_NEWPID) only updates pid_for_children for the
                    // calling thread; the fork inside spawn() then creates the child
                    // in the container's PID namespace so exec'd processes appear in
                    // the container's /proc and /proc/self resolves correctly.
                    pid_ns_path_for_root = Some(path.clone());
                } else {
                    // Rootless: the PID namespace is owned by the container's user
                    // namespace.  Joining it requires being in that user namespace
                    // first, which we cannot do in the parent without changing our
                    // own credentials.  Known limitation for rootless exec.
                    log::debug!("exec: skipping PID namespace join in rootless mode");
                }
            }
            _ if is_rootless && has_user_ns => {
                // UTS/IPC/NET/CGROUP: join after USER in the callback.
                late_ns_paths.push((path.clone(), *ns));
            }
            _ => {
                // Root exec (no user namespace): use the normal mechanism.
                cmd = cmd.with_namespace_join(path, *ns);
            }
        }
    }

    // Tell spawn() not to auto-create a new user namespace in rootless mode:
    // we're joining the container's existing user namespace ourselves.
    if user_ns_path.is_some() {
        cmd = cmd.skip_rootless_user_ns();
    }

    // Capture workdir for use in the pre_exec callback.
    let exec_workdir = args.workdir.clone();

    if has_mount_ns {
        // Open all namespace fds in the parent (before fork) so they are
        // inherited across fork and remain valid in the child's pre_exec.
        let mnt_ns_path = format!("/proc/{}/ns/mnt", pid);
        let mnt_ns_file = std::fs::File::open(&mnt_ns_path)
            .map_err(|e| format!("open {}: {}", mnt_ns_path, e))?;
        let mnt_ns_fd = mnt_ns_file.as_raw_fd();

        // Open user namespace fd if present — must be joined before MOUNT.
        let user_ns_file = user_ns_path
            .as_ref()
            .map(|p| std::fs::File::open(p).map_err(|e| format!("open {:?}: {}", p, e)))
            .transpose()?;
        let user_ns_fd = user_ns_file.as_ref().map(|f| f.as_raw_fd());

        // Open "late" namespaces (UTS, IPC, NET, CGROUP) that must be joined
        // AFTER the user namespace to satisfy the CAP_SYS_ADMIN requirement.
        let late_ns_files: Vec<(std::fs::File, Namespace)> = late_ns_paths
            .iter()
            .map(|(p, ns)| {
                std::fs::File::open(p)
                    .map(|f| (f, *ns))
                    .map_err(|e| format!("open {:?}: {}", p, e))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let late_ns_fds: Vec<(i32, Namespace)> = late_ns_files
            .iter()
            .map(|(f, ns)| (f.as_raw_fd(), *ns))
            .collect();

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
            // Keep File objects alive so fds remain valid in the child.
            let _keep_mnt = &mnt_ns_file;
            let _keep_root = &root_file;
            let _keep_user = &user_ns_file;
            let _keep_late = &late_ns_files;
            unsafe {
                // 1. Join user namespace first — the mount namespace and all
                //    other container namespaces are owned by it, so we need
                //    its credentials (cap_effective=all for uid 0) before
                //    we can join MOUNT, UTS, IPC, etc.
                if let Some(user_fd) = user_ns_fd {
                    if libc::setns(user_fd, libc::CLONE_NEWUSER) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                }
                // 2. Join mount namespace.
                if libc::setns(mnt_ns_fd, libc::CLONE_NEWNS) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                // 3. Enter the container's rootfs.
                if libc::fchdir(root_fd) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                let dot = std::ffi::CString::new(".").unwrap();
                if libc::chroot(dot.as_ptr()) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                let target = exec_workdir.as_deref().unwrap_or("/");
                let target_c = std::ffi::CString::new(target).unwrap();
                if libc::chdir(target_c.as_ptr()) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                // 4. Join UTS/IPC/NET/CGROUP — now inside the container's
                //    user namespace so CAP_SYS_ADMIN is satisfied.
                for (fd, _ns) in &late_ns_fds {
                    if libc::setns(*fd, 0) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
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

    // Apply base environment: image config env (image-based containers) or
    // live /proc/environ (rootfs containers).  Exactly one of these is non-empty.
    for (k, v) in image_env.iter().chain(container_env.iter()) {
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

        // Validate UID against the container's user namespace uid_map before
        // attempting to spawn.  setuid(uid) inside the user namespace fails
        // with EINVAL when uid is not covered by any uid_map entry — e.g.
        // when the container was started from a `newgrp`/`sg` shell where the
        // effective GID ≠ primary GID, causing newuidmap to be skipped and the
        // uid_map to collapse to a single entry `0 host_uid 1`.
        let uid_map_path = format!("/proc/{}/uid_map", environ_pid);
        if let Ok(uid_map) = std::fs::read_to_string(&uid_map_path) {
            if !uid_in_ns_map(uid, &uid_map) {
                return Err(format!(
                    "UID {} is not mapped in container '{}' user namespace\n\
                     uid_map: {}\n\
                     Hint: restart the container from a login shell rather than a \
                     'newgrp'/'sg' shell to enable subordinate UID mapping (newuidmap).",
                    uid,
                    args.name,
                    uid_map.split_whitespace().collect::<Vec<_>>().join(" ")
                )
                .into());
            }
        }

        if let Some(g) = gid {
            let gid_map_path = format!("/proc/{}/gid_map", environ_pid);
            if let Ok(gid_map) = std::fs::read_to_string(&gid_map_path) {
                if !uid_in_ns_map(g, &gid_map) {
                    return Err(format!(
                        "GID {} is not mapped in container '{}' user namespace\n\
                         gid_map: {}\n\
                         Hint: restart the container from a login shell rather than a \
                         'newgrp'/'sg' shell to enable subordinate GID mapping (newgidmap).",
                        g,
                        args.name,
                        gid_map.split_whitespace().collect::<Vec<_>>().join(" ")
                    )
                    .into());
                }
            }
            cmd = cmd.with_gid(g);
        }
        cmd = cmd.with_uid(uid);
    }

    // 5. Join PID namespace in the parent thread before fork (root exec only).
    //
    // setns(CLONE_NEWPID) updates this thread's pid_for_children to the
    // container's PID namespace.  The fork() inside spawn() then creates the
    // child inside that namespace, so the exec'd process:
    //   • gets a valid PID entry in the container's /proc
    //   • sees /proc/self as a live symlink (not a dangling 0-byte one)
    //   • can readlink /proc/self/ns/mnt without error
    //
    // This must happen after all command setup is complete and immediately
    // before spawn() so that no other forking happens in between.
    let _pid_ns_file_keep; // keep alive until after spawn()
    if let Some(ref p) = pid_ns_path_for_root {
        let f = std::fs::File::open(p).map_err(|e| format!("open pid ns {:?}: {}", p, e))?;
        let r = unsafe { libc::setns(f.as_raw_fd(), libc::CLONE_NEWPID) };
        if r != 0 {
            return Err(format!("setns(CLONE_NEWPID): {}", std::io::Error::last_os_error()).into());
        }
        _pid_ns_file_keep = Some(f);
    } else {
        _pid_ns_file_keep = None;
    }

    // 6. Spawn
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

/// Like [`exec_in_container`] but stores the spawned child's host PID into
/// `child_pid_sink` (via `Relaxed` store) before blocking on `wait()`.
///
/// This lets a caller that enforces a timeout read the PID and send `SIGKILL`
/// to the child if the wait does not complete in time.  The sink is set to
/// `0` if spawn fails.
pub fn exec_in_container_with_pid_sink(
    pid: i32,
    args: &[String],
    child_pid_sink: Arc<AtomicI32>,
) -> Option<bool> {
    if args.is_empty() || pid <= 0 {
        return None;
    }

    let ns_entries = discover_namespaces(pid).ok()?;

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
        Ok(mut child) => {
            child_pid_sink.store(child.pid(), Ordering::Relaxed);
            child.wait().map(|s| s.success()).ok()
        }
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
///
/// PID namespace special case: when a PID namespace is active, `pid` may be the
/// intermediate process P (which lives in the host PID namespace). P's
/// `/proc/P/ns/pid` matches init, so the normal check misses it. P's children
/// (the container's PID 1) inhabit the namespace pointed to by
/// `/proc/P/ns/pid_for_children`. We check that symlink after the main loop and
/// add it as `Namespace::PID` if it differs from init's PID namespace. Calling
/// `setns(pid_for_children_fd, CLONE_NEWPID)` in pre_exec followed by `exec()`
/// then moves the exec'd process into the container's PID namespace.
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
        // /proc/1/ns/user is unreadable by non-root; fall back to /proc/self/ns/<ns>
        // which is always readable. For non-root callers both point to the same
        // initial namespace, so the comparison is equivalent.
        let self_ns = format!("/proc/self/ns/{}", ns_name);
        let init_ino = match std::fs::metadata(&init_ns) {
            Ok(m) => {
                use std::os::unix::fs::MetadataExt;
                m.ino()
            }
            Err(_) => match std::fs::metadata(&self_ns) {
                Ok(m) => {
                    use std::os::unix::fs::MetadataExt;
                    m.ino()
                }
                Err(_) => continue,
            },
        };

        if container_ino != init_ino {
            result.push((PathBuf::from(container_ns), ns_flag));
        }
    }

    // If PID namespace was not found above (because `pid` is the intermediate
    // process P that lives in the host PID namespace), check pid_for_children.
    // This symlink points to the namespace that P's children (the container's
    // PID 1) actually inhabit.
    let pid_already_found = result.iter().any(|(_, ns)| *ns == Namespace::PID);
    if !pid_already_found {
        let pfc_path = format!("/proc/{}/ns/pid_for_children", pid);
        let init_pid_path = "/proc/1/ns/pid";
        let pfc_ino = std::fs::metadata(&pfc_path).ok().map(|m| {
            use std::os::unix::fs::MetadataExt;
            m.ino()
        });
        let init_pid_ino = std::fs::metadata(init_pid_path).ok().map(|m| {
            use std::os::unix::fs::MetadataExt;
            m.ino()
        });
        if let (Some(pfc), Some(init)) = (pfc_ino, init_pid_ino) {
            if pfc != init {
                result.push((PathBuf::from(pfc_path), Namespace::PID));
            }
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

/// Return true if `uid` is covered by at least one entry in a `/proc/*/uid_map`
/// file.  Each line is `container_start host_start count`; uid is valid inside
/// the namespace when `container_start <= uid < container_start + count`.
fn uid_in_ns_map(uid: u32, uid_map: &str) -> bool {
    for line in uid_map.lines() {
        let mut parts = line.split_whitespace();
        let start: Option<u32> = parts.next().and_then(|s| s.parse().ok());
        let _host: Option<u32> = parts.next().and_then(|s| s.parse().ok());
        let count: Option<u32> = parts.next().and_then(|s| s.parse().ok());
        if let (Some(start), Some(count)) = (start, count) {
            if uid >= start && uid < start.saturating_add(count) {
                return true;
            }
        }
    }
    false
}
