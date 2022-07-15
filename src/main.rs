#![crate_name = "remora"]
#![feature(core_c_str)]
#![allow(unused_imports)]

use clap::Parser;
use core::{ffi::CStr, panic};
use libc::{MS_BIND, gid_t, uid_t};
use log::{info,error, warn};
use std::{str::FromStr, env::current_dir, ffi::{CString,OsString, OsStr}, fs::read_link, path::PathBuf, ptr, os::unix::prelude::OsStrExt};
use unshare::{Child, Command, Error, GidMap, Stdio, UidMap};
use nix::unistd::{chroot};

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
    uid_parent: uid_t,
    gid_parent: gid_t,
) -> Result<Child, Box<dyn std::error::Error>> {
    unsafe {
        let mut curdir = current_dir().unwrap();
        let clap_args = Args::parse();
        curdir.push(clap_args.rootfs);

        let netns = std::fs::File::options().read(true).write(false).open("/var/run/netns/con");

        match netns {
            Ok(nsf) => {
                
                info!("opened net namespace: {:?}", nsf);
                let mut cmd = Command::new(to_run);

                info!("setting command info");
                cmd.args(&(child_args.into_iter().collect::<Vec<OsString>>())[..])
                .stdin(Stdio::inherit())
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .chroot_dir(curdir)
                .pre_exec(&mount_proc)
                .unshare(
                    [
                        unshare::Namespace::Uts,
                        unshare::Namespace::Mount,
                        unshare::Namespace::Pid,
                        unshare::Namespace::Cgroup,
                        //unshare::Namespace::Net,
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
                .gid(0);
                
                info!("setting namespace");
                match cmd.set_namespace(&nsf, unshare::Namespace::Net){
                    Ok(c) => {info!("set network namespace in {:?}",c);},
                    Err(e) => {warn!("failed to set namespace {:?}", e);}
                };
 
                info!("spawning child process");
                match cmd.spawn() {
                    Ok(c) => Ok(c),
                    Err(e) => Err(Box::new(std::io::Error::new(std::io::ErrorKind::Other, e.to_string())))
                }
            
            },
            Err(e) => {info!("failed to open namespace {:?}", e); Err(Box::new(e))}
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
    let mut path = PathBuf::new();
    path.push(std::env::current_dir().unwrap());
    path.push(clap_args.rootfs);
    path.push(r"sys");
    let sys_mount = CString::new(path.into_os_string().into_string().unwrap().as_bytes()).unwrap();
    
    match mount_sys(sys_mount.as_ref()) {
        Ok(_) => info!("mounted sys"),
        Err(e) => info!("failed to mount sys: {:?}", e)
    }
    
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
    
    match umount_sys(sys_mount.as_ref()) {
        Ok(_) => info!("unmounted sys"),
        Err(e) => info!("failed to unmount sys {:?}",e)
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
