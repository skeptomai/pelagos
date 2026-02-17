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

    // Test 2: Try to use a blocked syscall (reboot)
    println!("Test 2: Attempting to call reboot (blocked by seccomp)");
    println!("Expected: Reboot command should fail with 'Operation not permitted'\n");

    let mut child = Command::new("/bin/ash")
        .args(&[
            "-c",
            "reboot 2>&1 | head -1 || echo 'Reboot blocked (exit code: '$?')'",
        ])
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

    // Test 3: Container without seccomp (less secure)
    println!("Test 3: Running without seccomp (all syscalls allowed)");
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
    println!("- Normal application behavior is unaffected");
    println!("- Blocked syscalls (reboot, mount, ptrace, etc.) return EPERM");
    println!("- Always use .with_seccomp_default() for production containers!");
}
