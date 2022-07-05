#![crate_name = "remora"]
#![feature(core_c_str)]

use clap::Parser;
use core::{ffi::CStr, panic};
use libc::{MS_BIND, gid_t, uid_t};
use log::{info};
use std::{str::FromStr, env::current_dir, ffi::{CString,OsString}, fs::read_link, path::PathBuf, ptr};
use unshare::{Child, Command, Error, GidMap, Stdio, UidMap};

const ALPINE_ROOTFS : &str = "alpine-rootfs";
#[allow(dead_code)]
const ALPINE_CGROUP : &str = "/home/vagrant/Projects/remora/alpine-rootfs/sys/fs";
#[allow(dead_code)]
const SYSFS : &str = "sysfs";
const USERNAME : &str = "vagrant";

#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
struct Args {
    #[clap(short, long)]
    rootfs: String,
    #[clap(short, long, default_value="")]
    exe: String,
    #[clap(short, long, default_value="")]
    forked: String    
}

fn fork_exec(
    to_run: PathBuf,
    args: impl IntoIterator<Item = OsString>,
    uid_parent: uid_t,
    gid_parent: gid_t,
) -> Result<Child, Error> {
    let new_args: Vec<_> = args.into_iter().collect();
    let exe_path = read_link(to_run.as_path()).unwrap();
    info!("fork exec to_run: {:?}, args: {:?}", exe_path, new_args);
    Command::new(exe_path.as_os_str())
        .args(&new_args)
        .arg0(exe_path.as_os_str())
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .unshare(
            [
                unshare::Namespace::Uts,
                unshare::Namespace::Mount,
                unshare::Namespace::Pid,
                unshare::Namespace::Cgroup,
            ]
            .iter(),
        )
        .set_id_maps(
            vec![UidMap {
                inside_uid: 0,
                outside_uid: uid_parent,
                count: 1,
            }],
            vec![GidMap {
                inside_gid: 0,
                outside_gid: gid_parent,
                count: 1,
            }],
        )
        .uid(0)
        .gid(0)
        .spawn()
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
    _args: impl IntoIterator<Item = OsString>,
    uid_parent: uid_t,
    gid_parent: gid_t,
) -> Result<Child, Error> {
    unsafe {
        let new_args: Vec<OsString> = vec![];
        info!("child to_run: {:?}, new args: {:?}", to_run, new_args);
        let mut curdir = current_dir().unwrap();
        curdir.push(ALPINE_ROOTFS);
        Command::new(to_run)
            .args(&new_args)
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .chroot_dir(curdir)
            .pre_exec(&mount_proc)
            .uid(uid_parent)
            .gid(gid_parent)
            .spawn()
    }
}
fn main() {
    env_logger::init();
    info!("Entering main!");
    let cur_dir = std::env::current_dir().unwrap();
    info!("current dir: {:?}", cur_dir);

    let clap_args = Args::parse();
    info!("args: {:?}", clap_args);

    unsafe {
        let u_name =  CString::new(USERNAME).unwrap();
        let passwd = libc::getpwnam(u_name.as_ptr());
        let pw_uid = (*passwd).pw_uid;
        let pw_gid = (*passwd).pw_gid;

        info!("Before match for parent or child: uid: {}, gid: {}", pw_uid, pw_gid);

        match clap_args.forked.as_str() {
            "child" => {
                let pw_uid = libc::getuid();
                let pw_gid = libc::getgid();
                info!("match for child: uid: {}, gid: {}", pw_uid, pw_gid);
                info!("PID of CHILD: {}", std::process::id());
                let new_args = std::env::args_os();
                info!("new args in child: {:?}", new_args);                
                panic_spawn(
                    "child",
                    child,
                    PathBuf::from(clap_args.exe),
                    new_args,
                    pw_uid,
                    pw_gid,
                );
            }
            "" => {
                info!("PID of PARENT: {}", std::process::id());

                let mut path = PathBuf::new();
                path.push(std::env::current_dir().unwrap());
                path.push(r"alpine-rootfs");
                path.push(r"sys");
                let sys_mount = CString::new(path.into_os_string().into_string().unwrap().as_bytes()).unwrap();
                
                match mount_sys(sys_mount.as_ref()) {
                    Ok(_) => info!("mounted sys"),
                    Err(e) => info!("failed to mount sys: {:?}", e)
                }

                let self_exe = palaver::env::exe_path().unwrap();
                let mut new_args : Vec<OsString> = std::env::args_os().skip(1).collect();
                new_args.push(OsString::from_str("--forked").unwrap());
                new_args.push(OsString::from_str("child").unwrap());
                info!("new args: {:?}", new_args);
                
                panic_spawn(
                    "fork exec",
                    fork_exec,
                    self_exe,
                    new_args,
                    pw_uid,
                    pw_gid,
                );
                
                match umount_sys(sys_mount.as_ref()) {
                    Ok(_) => info!("unmounted sys"),
                    Err(e) => info!("failed to unmount sys {:?}",e)
                }
            },
            _ => { panic!("didn't understand command line");}
        }
    }
}

fn panic_spawn<I>(
    which: &'static str,
    p: impl Fn(PathBuf, I, uid_t, gid_t) -> Result<Child, Error>,
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

/// callback that mounts a new proc filesystem
/// this cannot allocate
fn mount_proc() -> std::io::Result<()> {
    unsafe {
        let proc_str = CString::new("proc")?;
        let proc_str_ptr = proc_str.as_ptr();
        match libc::mount(
            proc_str_ptr,
            proc_str_ptr,
            proc_str_ptr,
            0,
            ptr::null(),
        ) {
            0 => Ok(()),
            _ => Err(std::io::Error::last_os_error()),
        }
    }
}

#[allow(dead_code)]
fn mount_cgroup() -> std::io::Result<()> {
    unsafe {
        let src_str = "cgroup_root";
        info!("source is {:?}", src_str);        
        let fs_type_str = "cgroup";
        info!("fs_type is {:?}", fs_type_str);        
        let cgroups_str = CString::new(ALPINE_CGROUP)?;        
        let cgroups_str_ptr = cgroups_str.as_ptr();
        let src_str_ptr = src_str.as_ptr();
        
        let fs_type_str_ptr = fs_type_str.as_ptr();
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

#[allow(dead_code)]
fn mounts() -> std::io::Result<()> {
    match mount_proc() {
        Ok(_) => info!("mounted proc"),
        Err(e) => info!("failed to mount sys {:?}",e)
    }

    match mount_cgroup() {
        Ok(_) => info!("mounted cgroup"),
        Err(e) => info!("failed to mount cgroup {:?}",e)        
    }
    Ok(())
}
