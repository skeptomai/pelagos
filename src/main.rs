#![crate_name = "remora"]

use core::panic;
use libc::{MS_SHARED, MS_REC, MS_BIND};
use libc::{gid_t, uid_t};
use std::env::args;
use std::env::current_dir;
use std::ffi::CString;
use std::ffi::OsString;
use std::fs::read_link;
use std::path::PathBuf;
use std::ptr;
use unshare::{Child, Command, Error, GidMap, Stdio, UidMap};

const ALPINE_ROOTFS : &str = "alpine-rootfs";
const ALPINE_SYS : &str = "/home/christopherbrown/Projects/remora/alpine-rootfs/sys";
const SYSFS : &str = "sysfs";
const USERNAME : &str = "christopherbrown";

fn fork_exec(
    to_run: PathBuf,
    args: impl IntoIterator<Item = OsString>,
    uid_parent: uid_t,
    gid_parent: gid_t,
) -> Result<Child, Error> {
    let new_args: Vec<_> = args.into_iter().collect();
    let exe_path = read_link(to_run.as_path()).unwrap();
    println!("fork exec to_run: {:?}, args: {:?}", exe_path, new_args);
    Command::new(to_run)
        .args(&new_args)
        .arg0("child")
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

mount -t proc none ${chroot_dir}/proc
mount -o bind /sys ${chroot_dir}/sys
*/

/// launch actual child process in new uts and pid namespaces
/// with chroot and new proc filesystem
fn child(
    to_run: PathBuf,
    args: impl IntoIterator<Item = OsString>,
    uid_parent: uid_t,
    gid_parent: gid_t,
) -> Result<Child, Error> {
    unsafe {
        let new_args: Vec<_> = args.into_iter().collect();
        println!("child to_run: {:?}, new args: {:?}", to_run, new_args);
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
    println!("Entering main!");

    if args().len() < 2 {
        panic!("Not enough arguments supplied.  Gotta run something!")
    }

    unsafe {
        let u_name =  CString::new(USERNAME).unwrap();
        let u_name_ptr = u_name.as_ptr();
        let passwd = libc::getpwnam(u_name_ptr);
        let pw_uid = (*passwd).pw_uid;
        let pw_gid = (*passwd).pw_gid;


        println!("uid: {}, gid: {}", pw_uid, pw_gid);

        match args().nth(0).as_deref() {
            Some("child") => {
                let pw_uid = libc::getuid();
                let pw_gid = libc::getgid();

                println!("PID of CHILD: {}", std::process::id());
                let new_args = std::env::args_os().skip(2);
                panic_spawn(
                    "child",
                    child,
                    PathBuf::from(args().nth(1).unwrap()),
                    new_args,
                    pw_uid,
                    pw_gid,
                );
            }
            Some(_) => {
                println!("PID of PARENT: {}", std::process::id());
                println!("Child to run: '{:?}'", args());
                mount_sys().unwrap();

                let self_exe = palaver::env::exe_path().unwrap();
                let new_args = std::env::args_os().skip(1);
                panic_spawn(
                    "fork exec",
                    fork_exec,
                    self_exe,
                    new_args,
                    pw_uid,
                    pw_gid,
                );
            }
            _ => {
                panic!("NEITHER PARENT NOR CHILD?");
            }
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
    println!("spawning '{}'", which);
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
        let cgroups_str = CString::new("sys/fs/cgroup")?;
        let src_str = "cgroup_root";
        let fs_type_str = "cgroup";
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

fn mount_sys() -> std::io::Result<()> {
    unsafe {
        let src_str = CString::new("/sys")?;
        let _src_str_ptr = src_str.as_ptr();
        println!("source is {:?}", src_str);

        let target_str = CString::new(ALPINE_SYS)?;
        let target_str_ptr = target_str.as_ptr();
        println!("target is {:?}", target_str);

        let fs_type_str = CString::new(SYSFS)?;
        let fs_type_str_ptr = fs_type_str.as_ptr();
        println!("fs_type is {:?}", fs_type_str);

        match libc::mount(
            _src_str_ptr,
            target_str_ptr,
            fs_type_str_ptr,
            MS_BIND | MS_SHARED | MS_REC,
            ptr::null(),
        ) {
            0 => Ok(()),
            _ => Err(std::io::Error::last_os_error()),
        }
    }
}

#[allow(dead_code)]
fn mounts() -> std::io::Result<()> {
    mount_proc()?;
    //mount_cgroup()?;
    mount_sys()?;
    Ok(())
}
