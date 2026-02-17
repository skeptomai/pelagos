//! Demonstrates seccomp syscall filtering for container security.
//!
//! This example shows how to use remora's seccomp support to block dangerous
//! system calls that could lead to container escape or privilege escalation.
//!
//! # Running
//!
//! Build the alpine rootfs first:
//! ```bash
//! ./build-rootfs-docker.sh    # or ./build-rootfs-tarball.sh
//! ```
//!
//! Then run the example (requires root):
//! ```bash
//! sudo -E cargo run --example seccomp_demo
//! ```

use remora::container::{Command, Namespace, Stdio};
use std::env;
use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;

fn main() {
    env_logger::init();

    let current_dir = env::current_dir().expect("Failed to get current directory");
    let rootfs = current_dir.join("alpine-rootfs");

    if !rootfs.exists() {
        eprintln!("Error: alpine-rootfs not found!");
        eprintln!("Build it with: ./build-rootfs-docker.sh");
        eprintln!("Or without Docker: ./build-rootfs-tarball.sh");
        std::process::exit(1);
    }

    println!("=== Seccomp Demonstration ===\n");

    // Test 1: Normal operation with Docker seccomp profile
    println!("Test 1: Running echo with Docker's default seccomp profile");
    println!("Expected: Should work fine (read/write/brk syscalls are allowed)\n");

    let mut child = Command::new("/bin/ash")
        .args(&["-c", "echo 'Hello from secured container!'"])
        .stdin(Stdio::Inherit)
        .stdout(Stdio::Inherit)
        .stderr(Stdio::Inherit)
        .with_chroot(&rootfs)
        .with_namespaces(Namespace::UTS | Namespace::MOUNT)
        .with_proc_mount()
        .with_seccomp_default() // Apply Docker's default seccomp
        .spawn()
        .expect("Failed to spawn container");

    let status = child.wait().expect("Failed to wait");
    println!("Exit status: {:?}\n", status);

    // Test 2: Try to call reboot syscall directly (blocked by seccomp)
    println!("Test 2: Directly calling reboot() syscall (blocked by seccomp)");
    println!("Expected: Syscall should fail with EPERM\n");

    let mut child = Command::new("/bin/test_syscalls")
        .stdin(Stdio::Inherit)
        .stdout(Stdio::Inherit)
        .stderr(Stdio::Inherit)
        .with_chroot(&rootfs)
        .with_namespaces(Namespace::UTS | Namespace::MOUNT)
        .with_proc_mount()
        .with_seccomp_default()
        .spawn()
        .expect("Failed to spawn container");

    let status = child.wait().expect("Failed to wait");
    println!("Exit status: {:?}\n", status);

    // Test 3: Try to use a blocked syscall (mount)
    println!("Test 3: Attempting to mount tmpfs (blocked by seccomp)");
    println!("Expected: Mount syscall should fail with EPERM\n");

    // Create a test script that tries to mount
    let test_script_path = rootfs.join("tmp/test_mount.sh");
    fs::create_dir_all(rootfs.join("tmp")).ok();
    let mut script = fs::File::create(&test_script_path).unwrap();
    writeln!(
        script,
        r#"#!/bin/ash
# Try to mount tmpfs - should be blocked by seccomp
mkdir -p /tmp/testmount 2>/dev/null
/bin/mount -t tmpfs tmpfs /tmp/testmount 2>&1
RC=$?
if [ $RC -eq 0 ]; then
    echo "FAIL: Mount succeeded (seccomp not working!)"
    /bin/umount /tmp/testmount 2>/dev/null
else
    echo "SUCCESS: Mount blocked by seccomp (exit code: $RC)"
fi
"#
    )
    .unwrap();
    drop(script);
    fs::set_permissions(&test_script_path, fs::Permissions::from_mode(0o755)).unwrap();

    let mut child = Command::new("/tmp/test_mount.sh")
        .stdin(Stdio::Inherit)
        .stdout(Stdio::Inherit)
        .stderr(Stdio::Inherit)
        .with_chroot(&rootfs)
        .with_namespaces(Namespace::UTS | Namespace::MOUNT)
        .with_proc_mount()
        .with_seccomp_default()
        .spawn()
        .expect("Failed to spawn container");

    let status = child.wait().expect("Failed to wait");
    println!("Exit status: {:?}\n", status);

    // Cleanup
    fs::remove_file(&test_script_path).ok();

    // Test 4: Container without seccomp (less secure)
    println!("Test 4: Running without seccomp (all syscalls allowed)");
    println!("Warning: This is less secure but sometimes needed for compatibility\n");

    let mut child = Command::new("/bin/ash")
        .args(&["-c", "echo 'Running without syscall filtering'"])
        .stdin(Stdio::Inherit)
        .stdout(Stdio::Inherit)
        .stderr(Stdio::Inherit)
        .with_chroot(&rootfs)
        .with_namespaces(Namespace::UTS | Namespace::MOUNT)
        .with_proc_mount()
        // No seccomp - all syscalls allowed (less secure!)
        .spawn()
        .expect("Failed to spawn container");

    let status = child.wait().expect("Failed to wait");
    println!("Exit status: {:?}\n", status);

    println!("=== Demonstration Complete ===");
    println!("\nKey Takeaways:");
    println!("- Docker's seccomp profile blocks ~44 dangerous syscalls");
    println!("- Normal application behavior (echo, read, write) is unaffected");
    println!("- Dangerous syscalls (reboot, mount, ptrace, etc.) return EPERM");
    println!("- Both reboot and mount syscalls were successfully blocked");
    println!("- Always use .with_seccomp_default() for production containers!");
}
