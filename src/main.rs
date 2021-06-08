#![crate_name = "remora"]

use core::panic;
use std::env::args;
use std::ffi::CString;
use std::ffi::OsString;
use std::path::PathBuf;
use std::ptr;
use unshare::{Child, Command, Error, GidMap, Stdio, UidMap};

fn fork_exec(to_run: PathBuf, args: Vec<OsString>) -> Result<Child, Error> {
    println!("fork exec to_run: {:?}, args: {:?}", to_run, args);
    Command::new(to_run)
        .args(&args)
        .arg0("child")
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .unshare(
            [
                unshare::Namespace::Uts,
                unshare::Namespace::Mount,
                unshare::Namespace::Pid,
            ]
            .iter(),
        )
        .set_id_maps(
            vec![UidMap {
                inside_uid: 0,
                outside_uid: 1000,
                count: 1,
            }],
            vec![GidMap {
                inside_gid: 0,
                outside_gid: 1000,
                count: 1,
            }],
        )
        .uid(0)
        .gid(0)
        .spawn()
}

/// launch actual child process in new uts and pid namespaces
/// with chroot and new proc filesystem
fn child(to_run: PathBuf, args: Vec<OsString>) -> Result<Child, Error> {
    unsafe {
        println!("child to_run: {:?}, new args: {:?}", to_run, args);
        Command::new(to_run)
            .args(&args)
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .chroot_dir("/home/rootfs")
            .pre_exec(&mount_proc)
            .uid(0)
            .gid(0)
            .spawn()
    }
}

fn main() {
    println!("Entering main!");

    if args().len() < 2 {
        panic!("Not enough arguments supplied.  Gotta run something!")
    }

    match args().nth(0).as_deref() {
        Some("child") => {
            println!("CHILD: {}", std::process::id());
            let new_args: Vec<OsString> = std::env::args_os().skip(2).collect();
            panic_spawn(
                "child",
                &child,
                PathBuf::from(args().nth(1).unwrap()),
                new_args,
            );
        }
        Some(_) => {
            println!("PARENT: {}", std::process::id());
            println!("Gonna run '{:?}'", args());

            let self_exe = palaver::env::exe_path().unwrap();
            let new_args: Vec<_> = std::env::args_os().skip(1).collect();
            panic_spawn("fork exec", &fork_exec, self_exe, new_args);
        }
        _ => {
            panic!("NEITHER PARENT NOR CHILD?");
        }
    }
}

fn panic_spawn(
    which: &'static str,
    p: &(dyn Fn(PathBuf, Vec<OsString>) -> Result<Child, Error>),
    to_run: PathBuf,
    args: Vec<OsString>,
) {
    println!("spawning '{}'", which);
    p(to_run, args)
        .expect(format!("panicking on {}", which).as_str())
        .wait()
        .expect(format!("failed to wait for {} to exit", which).as_str());
}

/// callback that mounts a new proc filesystem
/// this cannot allocate
fn mount_proc() -> std::io::Result<()> {
    unsafe {
        let c_to_print = CString::new("proc")?;
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
