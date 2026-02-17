#![allow(unused_imports)]

use clap::Parser;
use core::{ffi::CStr, panic};
use libc::{MS_BIND, gid_t, uid_t};
use log::{info,error, warn};
use std::{str::FromStr, env::current_dir, ffi::{CString,OsString, OsStr}, fs::read_link, path::PathBuf, ptr, os::unix::prelude::{OsStrExt, IntoRawFd}};
use remora::container::{Child, Command, Error, GidMap, Stdio, UidMap, Namespace};
use nix::unistd::chroot;

const SYSFS : &str = "sysfs";

#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
struct Args {
    #[clap(short, long)]
    rootfs: String,
    #[clap(short, long)]
    exe: String,
    #[clap(short, long)]
    uid: u32,
    #[clap(short, long)]
    gid: u32,
    /// Optional network namespace to join (e.g., "con" to join /var/run/netns/con)
    #[clap(short = 'n', long)]
    join_netns: Option<String>,
}

/* NOTE: do these in the alpine make rootfs instead?

mknod -m 666 ${chroot_dir}/dev/full c 1 7
mknod -m 666 ${chroot_dir}/dev/ptmx c 5 2
mknod -m 644 ${chroot_dir}/dev/random c 1 8
mknod -m 644 ${chroot_dir}/dev/urandom c 1 9
mknod -m 666 ${chroot_dir}/dev/zero c 1 5
mknod -m 666 ${chroot_dir}/dev/tty c 5 0

mount -t proc none ${chroot_dir}/get
mount -o bind /sys ${chroot_dir}/sys
*/

/// launch actual child process in new uts and pid namespaces
/// with chroot and new proc filesystem
fn child(
    to_run: PathBuf,
    child_args: impl IntoIterator<Item = OsString>,
    _uid_parent: uid_t,
    _gid_parent: gid_t,
) -> Result<Child, Box<dyn std::error::Error>> {
    unsafe {
        let mut curdir = current_dir().unwrap();
        let clap_args = Args::parse();
        curdir.push(clap_args.rootfs);

        info!("current user and group before spawn: uid {}, gid {}", libc::getuid(), libc::getgid());

        info!("setting command info and spawning");
        let mut cmd = Command::new(to_run)
            .args(&(child_args.into_iter().collect::<Vec<OsString>>())[..])
            .stdin(Stdio::Inherit)
            .stdout(Stdio::Inherit)
            .stderr(Stdio::Inherit)
            .env("PATH", "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin")
            .with_chroot(curdir)
            .with_proc_mount()  // Automatically mount /proc
            .with_namespaces(
                Namespace::UTS | Namespace::MOUNT | Namespace::CGROUP
                // NOTE: PID namespace is NOT created here because unshare(CLONE_NEWPID)
                // doesn't put the calling process into the new namespace. The exec'd
                // program would still be in the original PID namespace, and the new
                // namespace would be empty, causing "can't fork: Out of memory" errors.
                // Proper PID namespace support requires forking after unshare, which
                // isn't possible with the current pre_exec architecture.
            );

        // Phase 3 feature: Network namespace joining
        if let Some(netns_name) = &clap_args.join_netns {
            let netns_path = format!("/var/run/netns/{}", netns_name);
            info!("Joining network namespace: {}", netns_path);
            cmd = cmd.with_namespace_join(netns_path, Namespace::NET);
        }

        /*  Phase 3 features - UID/GID mapping (TODO: add CLI flags)
        cmd = cmd
            .with_uid_maps(&[UidMap { inside: 0, outside: _uid_parent, count: 1 }])
            .with_gid_maps(&[GidMap { inside: 0, outside: _gid_parent, count: 1 }])
            .with_uid(0)
            .with_gid(0);
        */

        let result = cmd.spawn_interactive();

        match result {
            Ok(session) => {
                info!("spawned child {}", session.child.pid());
                // run() blocks here: relays I/O, restores terminal on exit
                match session.run() {
                    Ok(_status) => {
                        // Return a placeholder Child — panic_spawn calls .wait() on it,
                        // but the process has already exited. We need a real child handle.
                        // Since spawn_interactive blocks until exit, we just need to not crash.
                        // Use a direct exit here instead.
                        std::process::exit(0);
                    }
                    Err(e) => {
                        error!("relay loop failed: {}", e);
                        std::process::exit(1);
                    }
                }
            }
            Err(e) => {
                error!("failed to spawn child: {}", e);
                Err(Box::new(e))
            }
        }
    }
}

fn main() {
    env_logger::init();
    info!("Entering main!");
    let cur_dir = std::env::current_dir().unwrap();
    info!("current dir: {:?}", cur_dir);

    let clap_args = Args::parse();
    info!("args: {:?}", clap_args);

    let p_uid = clap_args.uid;
    let p_gid = clap_args.gid;
    info!("uid: {}, gid: {}", p_uid, p_gid);

    // Save rootfs path for later use
    let rootfs_path = clap_args.rootfs.clone();

    let mut path = PathBuf::new();
    path.push(std::env::current_dir().unwrap());
    path.push(clap_args.rootfs);
    path.push(r"sys");
    let sys_mount = CString::new(path.into_os_string().into_string().unwrap().as_bytes()).unwrap();

    // mount sys dir from parent process, because we still have privilege
    // this also brings cgroups along?
    match mount_sys(sys_mount.as_ref()) {
        Ok(_) => info!("mounted sys"),
        Err(e) => info!("failed to mount sys: {:?}", e)
    }

    // spawn child
    let new_args : Vec<OsString> = vec![];
    let thing_to_launch = &clap_args.exe.as_str();
    panic_spawn(
        thing_to_launch,
        child,
        PathBuf::from(thing_to_launch),
        new_args,
        p_uid,
        p_gid,
    );
    
    // unmount filesystems when child returns
    match umount_sys(sys_mount.as_ref()) {
        Ok(_) => info!("unmounted sys"),
        Err(e) => info!("failed to unmount sys {:?}",e)
    }

    // Try to unmount proc if it leaked out of mount namespace
    let mut proc_path = std::env::current_dir().unwrap();
    proc_path.push(&rootfs_path);
    proc_path.push("proc");
    let proc_mount = CString::new(proc_path.into_os_string().into_string().unwrap().as_bytes()).unwrap();
    match umount_sys(proc_mount.as_ref()) {
        Ok(_) => info!("unmounted proc"),
        Err(_) => {} // Ignore error - proc might not be mounted
    }
}

fn panic_spawn<I>(
    which: &str,
    p: impl Fn(PathBuf, I, uid_t, gid_t) -> Result<Child, Box<dyn std::error::Error>>,
    to_run: PathBuf,
    args: I,
    uid_parent: uid_t,
    gid_parent: gid_t,
) where
    I: IntoIterator<Item = OsString>,
{
    info!("spawning '{}'", which);
    p(to_run, args, uid_parent, gid_parent)
        .expect(format!("panicking on {}", which).as_str())
        .wait()
        .expect(format!("failed to wait for {} to exit", which).as_str());
}

// NOTE: mount_proc() was removed - we now use .with_proc_mount() instead
// See Phase 3 enhanced mount support feature

#[allow(dead_code)]
fn mount_cgroup() -> std::io::Result<()> {
    unsafe {
        let src_str = CString::new("cgroup_root")?;
        let fs_type_str = CString::new("cgroup")?;

        let mut cwd = std::env::current_dir()?;
        cwd.push("alpine-rootfs/sys/fs");
        let alpine_cgroup_dir = cwd.as_os_str().as_bytes();
        let cgroups_str = CString::new(alpine_cgroup_dir)?;
        
        let src_str_ptr = src_str.as_ptr();
        let fs_type_str_ptr = fs_type_str.as_ptr();
        let cgroups_str_ptr = cgroups_str.as_ptr();

        match libc::mount(
            src_str_ptr,
            cgroups_str_ptr,
            fs_type_str_ptr,
            libc::MS_BIND,
            ptr::null(),
        ) {
            0 => Ok(()),
            _ => Err(std::io::Error::last_os_error()),
        }
    }
}

fn mount_sys(target_str: &CStr) -> std::io::Result<()> {
    let src_str = CString::new("/sys").unwrap();
    unsafe {
        let src_str_ptr = src_str.as_ptr();
        info!("source is {:?}", src_str);
        let target_str_ptr = target_str.as_ptr();
        info!("target is {:?}", target_str);
        let fs_type_str = CString::new(SYSFS)?;
        let fs_type_str_ptr = fs_type_str.as_ptr();
        info!("fs_type is {:?}", fs_type_str);

        match libc::mount(
            src_str_ptr,
            target_str_ptr,
            fs_type_str_ptr,
            // shared and recursive means
            // 1. you can't unmount on exit
            // 2. mounting sys also mounts all cgroup related stuff
            MS_BIND, // | MS_SHARED | MS_REC,
            ptr::null(),
        ) {
            0 => Ok(()),
            _ => Err(std::io::Error::last_os_error()),
        }
    }
}

fn umount_sys(sys_mount: &CStr) -> std::io::Result<()> {
    let target_str_ptr = sys_mount.as_ptr();

    unsafe {
        match libc::umount(target_str_ptr) {
            0 => Ok(()),
            _ => Err(std::io::Error::last_os_error()),
        }
    }
}
