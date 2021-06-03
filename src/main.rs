#![crate_name = "remora"]

use core::panic;
use std::env::args;
use std::ffi::CString;
use std::ptr;

use unshare::{Child, Command, Error, Stdio};

/// callback that mounts a new proc filesystem
/// this cannot allocate
fn mount_proc() -> std::io::Result<()> {
    let c_to_print = CString::new("proc")?;
    unsafe {
        libc::mount(
            c_to_print.as_ptr(),
            c_to_print.as_ptr(),
            c_to_print.as_ptr(),
            0,
            ptr::null(),
        );
    }
    Ok(())
}

/// launch actual child process in new uts and pid namespaces
/// with chroot and new proc filesystem
fn child() -> Result<Child, Error> {
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
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .chroot_dir("/home/rootfs")
            .pre_exec(mount_proc)
            .spawn()
    }
}

fn main() {
    println!("Hello, world!");

    if args().len() < 2 {
        panic!("Not enough argument supplied.  Gotta run something!")
    }

    panic_spawn(&child);
}

fn panic_spawn(p: &(dyn Fn() -> Result<Child, Error>)) {
    p().expect("failed to fork child")
        .wait()
        .expect("failed to wait for child to exit");
}
