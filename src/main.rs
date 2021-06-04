#![crate_name = "remora"]

use core::panic;
use std::env::args;
use std::ffi::CString;
use std::ptr;

use unshare::{Command, Error, ExitStatus, GidMap, Stdio, UidMap};

/// callback that mounts a new proc filesystem
/// this cannot allocate
fn mount_proc() -> std::io::Result<()> {
    let c_to_print = CString::new("proc")?;
    unsafe {
        match libc::mount(
            c_to_print.as_ptr(),
            c_to_print.as_ptr(),
            c_to_print.as_ptr(),
            0,
            ptr::null(),
        ) {
            0 => Ok(()),
            _ => Err(std::io::Error::last_os_error()),
        }
    }
}

/// launch actual child process in new uts and pid namespaces
/// with chroot and new proc filesystem
fn child() -> Result<ExitStatus, Error> {
    unsafe {
        Command::new(args().nth(1).unwrap())
            .unshare(
                [
                    unshare::Namespace::Uts,
                    unshare::Namespace::Pid,
                    unshare::Namespace::User,
                ]
                .iter(),
            )
            .set_id_maps(
                vec![UidMap {
                    inside_uid: 1000,
                    outside_uid: 1000,
                    count: 1,
                }],
                vec![GidMap {
                    inside_gid: 1000,
                    outside_gid: 1000,
                    count: 1,
                }],
            )
            .uid(1000)
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .chroot_dir("/home/rootfs")
            //.pre_exec(mount_proc)
            .status()
    }
}

fn main() {
    println!("Hello, world!");

    if args().len() < 2 {
        panic!("Not enough arguments supplied.  Gotta run something!")
    }

    child().expect("failed to spawn child");
}
