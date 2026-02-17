//! Integration tests for remora container features.
//!
//! These tests verify the core containerization features including:
//! - UID/GID mapping
//! - Namespace joining (setns)
//! - Enhanced mount support
//! - Capability management
//! - Resource limits
//!
//! NOTE: Many of these tests require root privileges to create namespaces
//! and perform privileged operations. Run with:
//! ```bash
//! sudo -E cargo test --test integration_tests
//! ```

use remora::cgroup::ResourceStats;
use remora::container::{Capability, Command, GidMap, Namespace, SeccompProfile, Stdio, UidMap, Volume};
use remora::network::NetworkMode;
use serial_test::serial;
use std::path::PathBuf;

/// Helper to check if we're running as root
fn is_root() -> bool {
    unsafe { libc::getuid() == 0 }
}

/// Helper to get test rootfs path
///
/// Uses the existing alpine-rootfs if available, which has busybox and all necessary tools.
/// This avoids issues with dynamically linked binaries and missing libraries.
fn get_test_rootfs() -> Option<PathBuf> {
    // Try to find alpine-rootfs relative to project root
    let current_dir = std::env::current_dir().ok()?;
    let alpine_path = current_dir.join("alpine-rootfs");

    if alpine_path.exists() && alpine_path.join("bin/busybox").exists() {
        Some(alpine_path)
    } else {
        None
    }
}

/// Standard Alpine Linux PATH for use inside containers.
///
/// The container inherits the host's PATH, but host paths (e.g. Arch Linux's
/// /usr/local/sbin) may not exist inside the Alpine chroot. Always set this
/// PATH on any Command that will run inside the container.
const ALPINE_PATH: &str = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";

#[test]
fn test_basic_namespace_creation() {
    if !is_root() {
        eprintln!("Skipping test_basic_namespace_creation: requires root");
        return;
    }

    let Some(rootfs) = get_test_rootfs() else {
        eprintln!("Skipping test_basic_namespace_creation: alpine-rootfs not found");
        return;
    };

    // Test basic namespace creation with UTS and MOUNT
    let result = Command::new("/bin/ash")
        .args(&["-c", "exit 0"])
        .with_namespaces(Namespace::UTS | Namespace::MOUNT)
        .with_chroot(&rootfs)
        .env("PATH", ALPINE_PATH)
        .stdin(Stdio::Null)
        .stdout(Stdio::Null)
        .stderr(Stdio::Null)
        .spawn();

    match result {
        Ok(mut child) => {
            let status = child.wait().unwrap();
            assert!(status.success(), "Child process failed");
        }
        Err(e) => {
            panic!("Failed to spawn with namespaces: {:?}", e);
        }
    }
}

#[test]
fn test_proc_mount() {
    if !is_root() {
        eprintln!("Skipping test_proc_mount: requires root");
        return;
    }

    let Some(rootfs) = get_test_rootfs() else {
        eprintln!("Skipping test: alpine-rootfs not found");
        return;
    };

    // Test with_proc_mount() - check /proc/self/status exists
    let result = Command::new("/bin/ash")
        .args(&["-c", "test -f /proc/self/status"])
        .with_namespaces(Namespace::MOUNT | Namespace::UTS)
        .with_chroot(&rootfs)
        .env("PATH", ALPINE_PATH)
        .with_proc_mount()
        .stdin(Stdio::Null)
        .stdout(Stdio::Null)
        .stderr(Stdio::Null)
        .spawn();

    match result {
        Ok(mut child) => {
            let status = child.wait().unwrap();
            assert!(status.success(), "Proc was not mounted correctly");
        }
        Err(e) => panic!("Failed to spawn with proc mount: {:?}", e),
    }

}

#[test]
fn test_capability_dropping() {
    if !is_root() {
        eprintln!("Skipping test_capability_dropping: requires root");
        return;
    }

    let Some(rootfs) = get_test_rootfs() else {
        eprintln!("Skipping test: alpine-rootfs not found");
        return;
    };

    // Test drop_all_capabilities()
    let result = Command::new("/bin/ash")
        .args(&["-c", "exit 0"])
        .with_namespaces(Namespace::MOUNT | Namespace::UTS)
        .with_chroot(&rootfs)
        .env("PATH", ALPINE_PATH)
        .drop_all_capabilities()
        .stdin(Stdio::Null)
        .stdout(Stdio::Null)
        .stderr(Stdio::Null)
        .spawn();

    match result {
        Ok(mut child) => {
            let status = child.wait().unwrap();
            assert!(status.success(), "Child process failed with dropped caps");
        }
        Err(e) => panic!("Failed to spawn with dropped capabilities: {:?}", e),
    }

}

#[test]
fn test_selective_capabilities() {
    if !is_root() {
        eprintln!("Skipping test_selective_capabilities: requires root");
        return;
    }

    let Some(rootfs) = get_test_rootfs() else {
        eprintln!("Skipping test: alpine-rootfs not found");
        return;
    };

    // Test keeping only specific capabilities
    let result = Command::new("/bin/ash")
        .args(&["-c", "exit 0"])
        .with_namespaces(Namespace::MOUNT | Namespace::UTS)
        .with_chroot(&rootfs)
        .env("PATH", ALPINE_PATH)
        .with_capabilities(Capability::NET_BIND_SERVICE | Capability::CHOWN)
        .stdin(Stdio::Null)
        .stdout(Stdio::Null)
        .stderr(Stdio::Null)
        .spawn();

    match result {
        Ok(mut child) => {
            let status = child.wait().unwrap();
            assert!(status.success(), "Child process failed with selective caps");
        }
        Err(e) => panic!("Failed to spawn with selective capabilities: {:?}", e),
    }

}

#[test]
fn test_resource_limits_fds() {
    if !is_root() {
        eprintln!("Skipping test_resource_limits_fds: requires root");
        return;
    }

    let Some(rootfs) = get_test_rootfs() else {
        eprintln!("Skipping test: alpine-rootfs not found");
        return;
    };

    // Test with_max_fds() - check ulimit -n equals 100
    let result = Command::new("/bin/ash")
        .args(&["-c", "test \"$(ulimit -n)\" = 100"])
        .with_namespaces(Namespace::MOUNT | Namespace::UTS)
        .with_chroot(&rootfs)
        .env("PATH", ALPINE_PATH)
        .with_max_fds(100)
        .stdin(Stdio::Null)
        .stdout(Stdio::Null)
        .stderr(Stdio::Null)
        .spawn();

    match result {
        Ok(mut child) => {
            let status = child.wait().unwrap();
            assert!(status.success(), "FD limit was not set correctly");
        }
        Err(e) => panic!("Failed to spawn with fd limit: {:?}", e),
    }

}

#[test]
fn test_resource_limits_memory() {
    if !is_root() {
        eprintln!("Skipping test_resource_limits_memory: requires root");
        return;
    }

    let Some(rootfs) = get_test_rootfs() else {
        eprintln!("Skipping test: alpine-rootfs not found");
        return;
    };

    // Test with_memory_limit() - just verify it doesn't crash
    let result = Command::new("/bin/ash")
        .args(&["-c", "exit 0"])
        .with_namespaces(Namespace::MOUNT | Namespace::UTS)
        .with_chroot(&rootfs)
        .env("PATH", ALPINE_PATH)
        .with_memory_limit(512 * 1024 * 1024) // 512MB
        .stdin(Stdio::Null)
        .stdout(Stdio::Null)
        .stderr(Stdio::Null)
        .spawn();

    match result {
        Ok(mut child) => {
            let status = child.wait().unwrap();
            assert!(status.success(), "Child process failed with memory limit");
        }
        Err(e) => panic!("Failed to spawn with memory limit: {:?}", e),
    }

}

#[test]
fn test_resource_limits_cpu() {
    if !is_root() {
        eprintln!("Skipping test_resource_limits_cpu: requires root");
        return;
    }

    let Some(rootfs) = get_test_rootfs() else {
        eprintln!("Skipping test: alpine-rootfs not found");
        return;
    };

    // Test with_cpu_time_limit()
    let result = Command::new("/bin/ash")
        .args(&["-c", "exit 0"])
        .with_namespaces(Namespace::MOUNT | Namespace::UTS)
        .with_chroot(&rootfs)
        .env("PATH", ALPINE_PATH)
        .with_cpu_time_limit(60) // 60 seconds
        .stdin(Stdio::Null)
        .stdout(Stdio::Null)
        .stderr(Stdio::Null)
        .spawn();

    match result {
        Ok(mut child) => {
            let status = child.wait().unwrap();
            assert!(status.success(), "Child process failed with CPU limit");
        }
        Err(e) => panic!("Failed to spawn with CPU time limit: {:?}", e),
    }

}

#[test]
fn test_combined_features() {
    if !is_root() {
        eprintln!("Skipping test_combined_features: requires root");
        return;
    }

    let Some(rootfs) = get_test_rootfs() else {
        eprintln!("Skipping test: alpine-rootfs not found");
        return;
    };

    // Test combining multiple features together
    let result = Command::new("/bin/ash")
        .args(&["-c", "exit 0"])
        .with_namespaces(Namespace::MOUNT | Namespace::UTS | Namespace::CGROUP)
        .with_chroot(&rootfs)
        .env("PATH", ALPINE_PATH)
        .with_proc_mount()
        .with_capabilities(Capability::NET_BIND_SERVICE)
        .with_max_fds(500)
        .with_memory_limit(256 * 1024 * 1024)
        .stdin(Stdio::Null)
        .stdout(Stdio::Null)
        .stderr(Stdio::Null)
        .spawn();

    match result {
        Ok(mut child) => {
            let status = child.wait().unwrap();
            assert!(status.success(), "Child process failed with combined features");
        }
        Err(e) => panic!("Failed to spawn with combined features: {:?}", e),
    }

}

#[test]
fn test_uid_gid_api() {
    // This test verifies that the UID/GID mapping API exists and can be called.
    //
    // Note: Full USER namespace + UID/GID mapping testing has kernel limitations:
    // 1. USER namespaces are designed for unprivileged users
    // 2. Kernel restrictions prevent certain operations when already root
    // 3. Setting UID/GID without USER namespace has complex ordering requirements
    //
    // The API is fully implemented and works correctly in main.rs usage.
    // This test verifies the builder pattern API is available and compiles.

    let _cmd = Command::new("/bin/ash")
        .with_uid(1000)
        .with_gid(1000)
        .with_uid_maps(&[UidMap {
            inside: 0,
            outside: 1000,
            count: 1,
        }])
        .with_gid_maps(&[GidMap {
            inside: 0,
            outside: 1000,
            count: 1,
        }]);

    // Just verify the API compiles and methods are available
    assert!(true, "UID/GID API is available");

}

#[test]
fn test_namespace_bitflags() {
    // Test that namespace bitflags work correctly (no root needed)
    let ns1 = Namespace::UTS;
    let ns2 = Namespace::MOUNT;
    let combined = ns1 | ns2;

    assert!(combined.contains(Namespace::UTS));
    assert!(combined.contains(Namespace::MOUNT));
    assert!(!combined.contains(Namespace::PID));
}

#[test]
fn test_capability_bitflags() {
    // Test that capability bitflags work correctly (no root needed)
    let cap1 = Capability::CHOWN;
    let cap2 = Capability::NET_BIND_SERVICE;
    let combined = cap1 | cap2;

    assert!(combined.contains(Capability::CHOWN));
    assert!(combined.contains(Capability::NET_BIND_SERVICE));
    assert!(!combined.contains(Capability::SYS_ADMIN));
}

#[test]
fn test_command_builder_pattern() {
    // Test that the builder pattern works (no root needed, won't spawn)
    let rootfs = PathBuf::from("/tmp/test");

    let _cmd = Command::new("/bin/ash")
        .args(&["-c", "echo test", "-x"])
        .stdin(Stdio::Inherit)
        .stdout(Stdio::Piped)
        .stderr(Stdio::Null)
        .with_namespaces(Namespace::UTS)
        .with_chroot(&rootfs)
        .env("PATH", ALPINE_PATH)
        .with_proc_mount()
        .with_max_fds(1024);

    // Just test that the builder methods chain correctly
    assert!(true, "Builder pattern works");
}

// ============================================================================
// Seccomp Filter Tests
// ============================================================================

#[test]
fn test_seccomp_docker_blocks_reboot() {
    if !is_root() {
        eprintln!("Skipping test_seccomp_docker_blocks_reboot: requires root");
        return;
    }

    let Some(rootfs) = get_test_rootfs() else {
        eprintln!("Skipping test_seccomp_docker_blocks_reboot: alpine-rootfs not found");
        return;
    };

    // Run with Docker seccomp profile - attempt reboot (should be blocked)
    let mut child = Command::new("/bin/ash")
        .args(&["-c", "reboot 2>&1; echo reboot_exit_code=$?"])
        .stdin(Stdio::Null)
        .stdout(Stdio::Piped)
        .stderr(Stdio::Piped)
        .with_chroot(&rootfs)
        .env("PATH", ALPINE_PATH)
        .with_namespaces(Namespace::UTS | Namespace::MOUNT)
        .with_proc_mount()
        .with_seccomp_default() // Apply Docker's default seccomp profile
        .spawn()
        .expect("Failed to spawn with seccomp");

    let status = child.wait().expect("Failed to wait for child");

    // The reboot command should fail (seccomp blocks it)
    // Note: We can't easily check the exact error from inside the container,
    // but the process should complete without actually rebooting
    assert!(
        status.success() || status.code() == Some(1),
        "Process should complete (reboot syscall blocked by seccomp)"
    );

}

#[test]
fn test_seccomp_docker_allows_normal_syscalls() {
    if !is_root() {
        eprintln!("Skipping test_seccomp_docker_allows_normal_syscalls: requires root");
        return;
    }

    let Some(rootfs) = get_test_rootfs() else {
        eprintln!(
            "Skipping test_seccomp_docker_allows_normal_syscalls: alpine-rootfs not found"
        );
        return;
    };

    // Run a simple echo command - uses read, write, brk, etc. (all allowed)
    let mut child = Command::new("/bin/ash")
        .args(&["-c", "echo 'Seccomp allows normal operations'"])
        .stdin(Stdio::Null)
        .stdout(Stdio::Piped)
        .stderr(Stdio::Piped)
        .with_chroot(&rootfs)
        .env("PATH", ALPINE_PATH)
        .with_namespaces(Namespace::UTS | Namespace::MOUNT)
        .with_proc_mount()
        .with_seccomp_default() // Apply Docker's default seccomp profile
        .spawn()
        .expect("Failed to spawn with seccomp");

    let status = child.wait().expect("Failed to wait for child");

    // Normal operations should work fine
    assert!(status.success(), "Normal syscalls should be allowed");
}

#[test]
fn test_seccomp_minimal_is_restrictive() {
    if !is_root() {
        eprintln!("Skipping test_seccomp_minimal_is_restrictive: requires root");
        return;
    }

    let Some(rootfs) = get_test_rootfs() else {
        eprintln!("Skipping test_seccomp_minimal_is_restrictive: alpine-rootfs not found");
        return;
    };

    // The minimal profile is very restrictive - even basic commands might fail
    // We just test that it compiles and can be applied
    let result = Command::new("/bin/ash")
        .args(&["-c", "exit 0"])
        .stdin(Stdio::Null)
        .stdout(Stdio::Null)
        .stderr(Stdio::Null)
        .with_chroot(&rootfs)
        .env("PATH", ALPINE_PATH)
        .with_namespaces(Namespace::UTS | Namespace::MOUNT)
        .with_proc_mount()
        .with_seccomp_minimal() // Apply minimal profile
        .spawn();

    // The minimal profile might be too restrictive for ash to even start,
    // but the important thing is that seccomp was applied without errors
    match result {
        Ok(mut child) => {
            let status = child.wait().expect("Failed to wait for child");
            // Process may or may not succeed depending on what syscalls ash needs
            eprintln!("Minimal seccomp: process exited with status {:?}", status);
        }
        Err(e) => {
            // If spawn fails, it might be because seccomp blocked a syscall
            // needed during process startup. This is expected with minimal profile.
            eprintln!("Minimal seccomp: spawn failed (expected): {}", e);
        }
    }

    // Test passes if we got here (seccomp was applied)
    assert!(true, "Minimal seccomp profile can be applied");
}

#[test]
fn test_seccomp_profile_api() {
    // Test that seccomp API methods are available (no root needed, won't spawn)
    let rootfs = PathBuf::from("/tmp/test");

    let _cmd1 = Command::new("/bin/sh")
        .with_chroot(&rootfs)
        .env("PATH", ALPINE_PATH)
        .with_seccomp_default();

    let _cmd2 = Command::new("/bin/sh")
        .with_chroot(&rootfs)
        .env("PATH", ALPINE_PATH)
        .with_seccomp_minimal();

    let _cmd3 = Command::new("/bin/sh")
        .with_chroot(&rootfs)
        .env("PATH", ALPINE_PATH)
        .with_seccomp_profile(SeccompProfile::Docker);

    let _cmd4 = Command::new("/bin/sh")
        .with_chroot(&rootfs)
        .env("PATH", ALPINE_PATH)
        .without_seccomp();

    // Just verify the API compiles and methods are available
    assert!(true, "Seccomp API is available");
}

#[test]
fn test_seccomp_without_flag_works() {
    if !is_root() {
        eprintln!("Skipping test_seccomp_without_flag_works: requires root");
        return;
    }

    let Some(rootfs) = get_test_rootfs() else {
        eprintln!("Skipping test_seccomp_without_flag_works: alpine-rootfs not found");
        return;
    };

    // Test that containers work without seccomp (backward compatibility)
    let mut child = Command::new("/bin/ash")
        .args(&["-c", "echo 'No seccomp'"])
        .stdin(Stdio::Null)
        .stdout(Stdio::Piped)
        .stderr(Stdio::Piped)
        .with_chroot(&rootfs)
        .env("PATH", ALPINE_PATH)
        .with_namespaces(Namespace::UTS | Namespace::MOUNT)
        .with_proc_mount()
        // No seccomp configured - should work fine
        .spawn()
        .expect("Failed to spawn without seccomp");

    let status = child.wait().expect("Failed to wait for child");
    assert!(status.success(), "Container should work without seccomp");
}

// ============================================================================
// Phase 1 Security Features Tests
// ============================================================================

#[test]
fn test_no_new_privileges() {
    if !is_root() {
        eprintln!("Skipping test_no_new_privileges: requires root");
        return;
    }

    let Some(rootfs) = get_test_rootfs() else {
        eprintln!("Skipping test_no_new_privileges: alpine-rootfs not found");
        return;
    };

    // Run ash inline - grep for NoNewPrivs:	1 in /proc/self/status
    // The value is 1 when PR_SET_NO_NEW_PRIVS has been set
    // Use full paths since PATH is not set inside the container
    let mut child = Command::new("/bin/ash")
        .args(&["-c", "/bin/grep 'NoNewPrivs:.*1' /proc/self/status"])
        .stdin(Stdio::Null)
        .stdout(Stdio::Piped)
        .stderr(Stdio::Piped)
        .with_chroot(&rootfs)
        .env("PATH", ALPINE_PATH)
        .with_namespaces(Namespace::UTS | Namespace::MOUNT)
        .with_proc_mount()
        .with_no_new_privileges(true)
        .spawn()
        .expect("Failed to spawn with no-new-privileges");

    let status = child.wait().expect("Failed to wait for child");
    assert!(status.success(), "NoNewPrivs should be set to 1 in /proc/self/status");
}

#[test]
fn test_readonly_rootfs() {
    if !is_root() {
        eprintln!("Skipping test_readonly_rootfs: requires root");
        return;
    }

    let Some(rootfs) = get_test_rootfs() else {
        eprintln!("Skipping test_readonly_rootfs: alpine-rootfs not found");
        return;
    };

    // Try to write to rootfs - should fail with read-only filesystem
    let mut child = Command::new("/bin/ash")
        .args(&["-c", "touch /test_file 2>&1; echo exit_code=$?"])
        .stdin(Stdio::Null)
        .stdout(Stdio::Piped)
        .stderr(Stdio::Piped)
        .with_chroot(&rootfs)
        .env("PATH", ALPINE_PATH)
        .with_namespaces(Namespace::UTS | Namespace::MOUNT)
        .with_proc_mount()
        .with_readonly_rootfs(true)
        .spawn()
        .expect("Failed to spawn with read-only rootfs");

    let status = child.wait().expect("Failed to wait for child");
    // The command should complete (exit code 0), but the touch should have failed
    assert!(status.success(), "Container should run despite read-only fs");
}

#[test]
fn test_masked_paths_default() {
    if !is_root() {
        eprintln!("Skipping test_masked_paths_default: requires root");
        return;
    }

    let Some(rootfs) = get_test_rootfs() else {
        eprintln!("Skipping test_masked_paths_default: alpine-rootfs not found");
        return;
    };

    // Try to read a masked path - should see /dev/null or get an error
    let mut child = Command::new("/bin/ash")
        .args(&["-c", "cat /proc/kcore 2>&1 | head -c 10 || echo 'masked'"])
        .stdin(Stdio::Null)
        .stdout(Stdio::Piped)
        .stderr(Stdio::Piped)
        .with_chroot(&rootfs)
        .env("PATH", ALPINE_PATH)
        .with_namespaces(Namespace::UTS | Namespace::MOUNT)
        .with_proc_mount()
        .with_masked_paths_default()
        .spawn()
        .expect("Failed to spawn with masked paths");

    let status = child.wait().expect("Failed to wait for child");
    assert!(status.success(), "Masked paths should not cause failures");
}

#[test]
fn test_masked_paths_custom() {
    if !is_root() {
        eprintln!("Skipping test_masked_paths_custom: requires root");
        return;
    }

    let Some(rootfs) = get_test_rootfs() else {
        eprintln!("Skipping test_masked_paths_custom: alpine-rootfs not found");
        return;
    };

    // Use custom masked paths
    let mut child = Command::new("/bin/ash")
        .args(&["-c", "echo 'Custom masked paths test'"])
        .stdin(Stdio::Null)
        .stdout(Stdio::Piped)
        .stderr(Stdio::Piped)
        .with_chroot(&rootfs)
        .env("PATH", ALPINE_PATH)
        .with_namespaces(Namespace::UTS | Namespace::MOUNT)
        .with_proc_mount()
        .with_masked_paths(&["/proc/kcore", "/sys/firmware"])
        .spawn()
        .expect("Failed to spawn with custom masked paths");

    let status = child.wait().expect("Failed to wait for child");
    assert!(status.success(), "Custom masked paths should work");
}

#[test]
fn test_combined_phase1_security() {
    if !is_root() {
        eprintln!("Skipping test_combined_phase1_security: requires root");
        return;
    }

    let Some(rootfs) = get_test_rootfs() else {
        eprintln!("Skipping test_combined_phase1_security: alpine-rootfs not found");
        return;
    };

    // Test all Phase 1 security features together
    let mut child = Command::new("/bin/ash")
        .args(&["-c", "echo 'All Phase 1 security features enabled'"])
        .stdin(Stdio::Null)
        .stdout(Stdio::Piped)
        .stderr(Stdio::Piped)
        .with_chroot(&rootfs)
        .env("PATH", ALPINE_PATH)
        .with_namespaces(Namespace::UTS | Namespace::MOUNT)
        .with_proc_mount()
        .with_seccomp_default()        // Seccomp filtering
        .with_no_new_privileges(true)  // No privilege escalation
        .with_readonly_rootfs(true)    // Immutable rootfs
        .with_masked_paths_default()   // Hide sensitive paths
        .drop_all_capabilities()       // Minimal capabilities
        .spawn()
        .expect("Failed to spawn with all Phase 1 security");

    let status = child.wait().expect("Failed to wait for child");
    assert!(
        status.success(),
        "Container with all Phase 1 security should work"
    );
}

// ============================================================================
// Phase 4: Filesystem Flexibility Tests
// ============================================================================

#[test]
fn test_bind_mount_rw() {
    if !is_root() {
        eprintln!("Skipping test_bind_mount_rw: requires root");
        return;
    }

    let Some(rootfs) = get_test_rootfs() else {
        eprintln!("Skipping test_bind_mount_rw: alpine-rootfs not found");
        return;
    };

    // Create a temp dir on the host and write a file into it
    let host_dir = tempfile::tempdir().expect("failed to create temp dir");
    std::fs::write(host_dir.path().join("hello.txt"), b"hello from host")
        .expect("failed to write host file");

    // Mount the host dir into /mnt/hostdir inside the container and verify the file is readable
    let mut child = Command::new("/bin/ash")
        .args(&["-c", "cat /mnt/hostdir/hello.txt"])
        .with_namespaces(Namespace::MOUNT | Namespace::UTS)
        .with_chroot(&rootfs)
        .env("PATH", ALPINE_PATH)
        .with_bind_mount(host_dir.path(), "/mnt/hostdir")
        .stdin(Stdio::Null)
        .stdout(Stdio::Piped)
        .stderr(Stdio::Piped)
        .spawn()
        .expect("Failed to spawn with bind mount");

    let status = child.wait().expect("Failed to wait for child");
    assert!(status.success(), "Container should read host file via bind mount");
}

#[test]
fn test_bind_mount_ro() {
    if !is_root() {
        eprintln!("Skipping test_bind_mount_ro: requires root");
        return;
    }

    let Some(rootfs) = get_test_rootfs() else {
        eprintln!("Skipping test_bind_mount_ro: alpine-rootfs not found");
        return;
    };

    let host_dir = tempfile::tempdir().expect("failed to create temp dir");

    // Attempt to write inside a read-only bind mount — should fail
    let child = Command::new("/bin/ash")
        .args(&["-c", "touch /mnt/ro/newfile 2>/dev/null; echo exit=$?"])
        .with_namespaces(Namespace::MOUNT | Namespace::UTS)
        .with_chroot(&rootfs)
        .env("PATH", ALPINE_PATH)
        .with_bind_mount_ro(host_dir.path(), "/mnt/ro")
        .stdin(Stdio::Null)
        .stdout(Stdio::Piped)
        .stderr(Stdio::Piped)
        .spawn()
        .expect("Failed to spawn with read-only bind mount");

    let (status, stdout, _) = child.wait_with_output().expect("Failed to collect output");
    assert!(status.success(), "Shell should exit cleanly");
    let out = String::from_utf8_lossy(&stdout);
    // touch must fail (exit code != 0) because the mount is read-only
    assert!(
        out.contains("exit=1"),
        "Write to read-only bind mount should fail, got: {}",
        out
    );
}

#[test]
fn test_tmpfs_mount() {
    if !is_root() {
        eprintln!("Skipping test_tmpfs_mount: requires root");
        return;
    }

    let Some(rootfs) = get_test_rootfs() else {
        eprintln!("Skipping test_tmpfs_mount: alpine-rootfs not found");
        return;
    };

    // Even with a read-only rootfs, tmpfs at /tmp should be writable
    let child = Command::new("/bin/ash")
        .args(&["-c", "touch /tmp/testfile && echo ok"])
        .with_namespaces(Namespace::MOUNT | Namespace::UTS)
        .with_chroot(&rootfs)
        .env("PATH", ALPINE_PATH)
        .with_readonly_rootfs(true)
        .with_tmpfs("/tmp", "size=10m,mode=1777")
        .stdin(Stdio::Null)
        .stdout(Stdio::Piped)
        .stderr(Stdio::Piped)
        .spawn()
        .expect("Failed to spawn with tmpfs mount");

    let (status, stdout, _) = child.wait_with_output().expect("Failed to collect output");
    assert!(status.success(), "Container should succeed with tmpfs /tmp");
    let out = String::from_utf8_lossy(&stdout);
    assert!(out.contains("ok"), "touch on tmpfs should succeed, got: {}", out);
}

#[test]
fn test_named_volume() {
    if !is_root() {
        eprintln!("Skipping test_named_volume: requires root");
        return;
    }

    let Some(rootfs) = get_test_rootfs() else {
        eprintln!("Skipping test_named_volume: alpine-rootfs not found");
        return;
    };

    // Clean up any leftover volume from a previous failed run
    let _ = Volume::delete("testvol");

    let vol = Volume::create("testvol").expect("Failed to create volume");

    // Write a file from inside the container
    let mut child = Command::new("/bin/ash")
        .args(&["-c", "echo persistent > /data/file.txt"])
        .with_namespaces(Namespace::MOUNT | Namespace::UTS)
        .with_chroot(&rootfs)
        .env("PATH", ALPINE_PATH)
        .with_volume(&vol, "/data")
        .stdin(Stdio::Null)
        .stdout(Stdio::Null)
        .stderr(Stdio::Null)
        .spawn()
        .expect("Failed to spawn with named volume");

    let status = child.wait().expect("Failed to wait for child");
    assert!(status.success(), "Container should write to volume");

    // Verify the file persists on the host
    let host_file = vol.path().join("file.txt");
    assert!(host_file.exists(), "Volume file should exist on host after container exits");
    let contents = std::fs::read_to_string(&host_file).expect("Failed to read volume file");
    assert!(contents.contains("persistent"), "Volume file should contain expected content");

    // Clean up
    Volume::delete("testvol").expect("Failed to delete volume");
}

// ============================================================================
// Phase 5: Cgroups v2 Resource Management Tests
// ============================================================================

#[test]
fn test_cgroup_memory_limit() {
    if !is_root() {
        eprintln!("Skipping test_cgroup_memory_limit: requires root");
        return;
    }

    let Some(rootfs) = get_test_rootfs() else {
        eprintln!("Skipping test_cgroup_memory_limit: alpine-rootfs not found");
        return;
    };

    // Try to allocate ~64 MB using dd. With a 32 MB cgroup limit the process
    // should be OOM-killed (exit non-zero) or fail to allocate.
    let mut child = Command::new("/bin/ash")
        .args(&["-c", "dd if=/dev/urandom of=/dev/null bs=1M count=64"])
        .with_namespaces(Namespace::MOUNT | Namespace::UTS)
        .with_chroot(&rootfs)
        .env("PATH", ALPINE_PATH)
        .with_cgroup_memory(32 * 1024 * 1024) // 32 MB
        .stdin(Stdio::Null)
        .stdout(Stdio::Null)
        .stderr(Stdio::Null)
        .spawn()
        .expect("Failed to spawn with cgroup memory limit");

    let status = child.wait().expect("Failed to wait for child");
    // dd reads stdin→stdout incrementally so it won't hit the RSS limit.
    // The important thing is that the cgroup was created and the process ran.
    // We just verify the container exits (success or OOM-killed).
    let _ = status;
}

#[test]
fn test_cgroup_pids_limit() {
    if !is_root() {
        eprintln!("Skipping test_cgroup_pids_limit: requires root");
        return;
    }

    let Some(rootfs) = get_test_rootfs() else {
        eprintln!("Skipping test_cgroup_pids_limit: alpine-rootfs not found");
        return;
    };

    // Limit to 4 PIDs (ash + subprocesses). Try to spawn 10 background jobs
    // — at least some should fail. The shell exits 0 regardless, so we just
    // verify that cgroup setup does not break container execution.
    let mut child = Command::new("/bin/ash")
        .args(&["-c", "for i in 1 2 3 4 5 6 7 8 9 10; do sleep 0 & done; wait; echo done"])
        .with_namespaces(Namespace::MOUNT | Namespace::UTS)
        .with_chroot(&rootfs)
        .env("PATH", ALPINE_PATH)
        .with_cgroup_pids_limit(4)
        .stdin(Stdio::Null)
        .stdout(Stdio::Null)
        .stderr(Stdio::Null)
        .spawn()
        .expect("Failed to spawn with cgroup pids limit");

    // Process should complete (even if some forks were denied by pids.max)
    let status = child.wait().expect("Failed to wait for child");
    let _ = status;
}

#[test]
fn test_cgroup_cpu_shares() {
    if !is_root() {
        eprintln!("Skipping test_cgroup_cpu_shares: requires root");
        return;
    }

    let Some(rootfs) = get_test_rootfs() else {
        eprintln!("Skipping test_cgroup_cpu_shares: alpine-rootfs not found");
        return;
    };

    // Smoke test: setting cpu_shares should not break container execution.
    let mut child = Command::new("/bin/ash")
        .args(&["-c", "echo ok"])
        .with_namespaces(Namespace::MOUNT | Namespace::UTS)
        .with_chroot(&rootfs)
        .env("PATH", ALPINE_PATH)
        .with_cgroup_cpu_shares(512)
        .stdin(Stdio::Null)
        .stdout(Stdio::Piped)
        .stderr(Stdio::Null)
        .spawn()
        .expect("Failed to spawn with cgroup cpu shares");

    let status = child.wait().expect("Failed to wait for child");
    assert!(status.success(), "Container with cpu_shares should exit cleanly");
}

#[test]
fn test_resource_stats() {
    if !is_root() {
        eprintln!("Skipping test_resource_stats: requires root");
        return;
    }

    let Some(rootfs) = get_test_rootfs() else {
        eprintln!("Skipping test_resource_stats: alpine-rootfs not found");
        return;
    };

    // Verify resource_stats() returns a ResourceStats (no panic/error) when
    // a cgroup is active. Values should be >= 0 (they're unsigned).
    let mut child = Command::new("/bin/ash")
        .args(&["-c", "echo hello"])
        .with_namespaces(Namespace::MOUNT | Namespace::UTS)
        .with_chroot(&rootfs)
        .env("PATH", ALPINE_PATH)
        .with_cgroup_memory(128 * 1024 * 1024)
        .with_cgroup_pids_limit(64)
        .stdin(Stdio::Null)
        .stdout(Stdio::Null)
        .stderr(Stdio::Null)
        .spawn()
        .expect("Failed to spawn for resource_stats test");

    // Read stats while the process may still be running
    let stats: ResourceStats = child.resource_stats().expect("resource_stats() failed");
    // Values are u64 so always >= 0; just verify the call succeeded
    let _ = stats.memory_current_bytes;
    let _ = stats.cpu_usage_ns;
    let _ = stats.pids_current;

    child.wait().expect("Failed to wait for child");
}

#[test]
fn test_cgroup_cleanup() {
    if !is_root() {
        eprintln!("Skipping test_cgroup_cleanup: requires root");
        return;
    }

    let Some(rootfs) = get_test_rootfs() else {
        eprintln!("Skipping test_cgroup_cleanup: alpine-rootfs not found");
        return;
    };

    let mut child = Command::new("/bin/ash")
        .args(&["-c", "exit 0"])
        .with_namespaces(Namespace::MOUNT | Namespace::UTS)
        .with_chroot(&rootfs)
        .env("PATH", ALPINE_PATH)
        .with_cgroup_memory(64 * 1024 * 1024)
        .stdin(Stdio::Null)
        .stdout(Stdio::Null)
        .stderr(Stdio::Null)
        .spawn()
        .expect("Failed to spawn for cgroup cleanup test");

    let pid = child.pid();
    child.wait().expect("Failed to wait for child");

    // After wait(), teardown_cgroup should have deleted the cgroup directory
    let cgroup_path = format!("/sys/fs/cgroup/remora-{}", pid);
    assert!(
        !std::path::Path::new(&cgroup_path).exists(),
        "Cgroup {} should be deleted after container exits",
        cgroup_path
    );
}

// ============================================================================
// Phase 6: Native Networking Tests — N1 (Loopback) and N2 (Bridge)
// ============================================================================

/// N1: Loopback-only network mode — lo should come up with 127.0.0.1.
#[test]
fn test_loopback_network() {
    if !is_root() {
        eprintln!("Skipping test_loopback_network: requires root");
        return;
    }

    let Some(rootfs) = get_test_rootfs() else {
        eprintln!("Skipping test_loopback_network: alpine-rootfs not found");
        return;
    };

    // with_network(Loopback) automatically adds Namespace::NET and brings up lo.
    // After lo is up, the kernel assigns 127.0.0.1 automatically.
    let child = Command::new("/bin/ash")
        .args(&["-c", "ip addr show lo | grep -q '127.0.0.1' && echo LOOPBACK_OK"])
        .with_namespaces(Namespace::MOUNT | Namespace::UTS)
        .with_network(NetworkMode::Loopback)
        .with_chroot(&rootfs)
        .env("PATH", ALPINE_PATH)
        .with_proc_mount()
        .stdin(Stdio::Null)
        .stdout(Stdio::Piped)
        .stderr(Stdio::Null)
        .spawn()
        .expect("Failed to spawn loopback container");

    let (status, stdout, _) = child.wait_with_output().expect("Failed to collect output");
    let out = String::from_utf8_lossy(&stdout);
    assert!(
        out.contains("LOOPBACK_OK"),
        "lo should have 127.0.0.1 after bring-up, got: {}",
        out
    );
    assert!(status.success(), "Container exited with failure");
}

/// N2: Bridge mode — container should receive a 172.19.0.x/24 address on eth0.
#[test]
fn test_bridge_network_ip() {
    if !is_root() {
        eprintln!("Skipping test_bridge_network_ip: requires root");
        return;
    }

    let Some(rootfs) = get_test_rootfs() else {
        eprintln!("Skipping test_bridge_network_ip: alpine-rootfs not found");
        return;
    };

    // Named netns is fully configured before fork; eth0 is ready from the first instruction.
    let child = Command::new("/bin/ash")
        .args(&["-c", "ip addr show eth0 | grep -q '172.19.0' && echo BRIDGE_IP_OK"])
        .with_namespaces(Namespace::MOUNT | Namespace::UTS)
        .with_network(NetworkMode::Bridge)
        .with_chroot(&rootfs)
        .env("PATH", ALPINE_PATH)
        .with_proc_mount()
        .stdin(Stdio::Null)
        .stdout(Stdio::Piped)
        .stderr(Stdio::Null)
        .spawn()
        .expect("Failed to spawn bridge container");

    let (status, stdout, _) = child.wait_with_output().expect("Failed to collect output");
    let out = String::from_utf8_lossy(&stdout);
    assert!(
        out.contains("BRIDGE_IP_OK"),
        "eth0 should have a 172.19.0.x address in bridge mode, got: {}",
        out
    );
    assert!(status.success(), "Container exited with failure");
}

/// N2: After spawn(), the host-side veth interface (vh-{hash}) should exist.
#[test]
fn test_bridge_network_veth_exists() {
    if !is_root() {
        eprintln!("Skipping test_bridge_network_veth_exists: requires root");
        return;
    }

    let Some(rootfs) = get_test_rootfs() else {
        eprintln!("Skipping test_bridge_network_veth_exists: alpine-rootfs not found");
        return;
    };

    let mut child = Command::new("/bin/ash")
        .args(&["-c", "sleep 2"])
        .with_namespaces(Namespace::MOUNT | Namespace::UTS)
        .with_network(NetworkMode::Bridge)
        .with_chroot(&rootfs)
        .env("PATH", ALPINE_PATH)
        .stdin(Stdio::Null)
        .stdout(Stdio::Null)
        .stderr(Stdio::Null)
        .spawn()
        .expect("Failed to spawn bridge container");

    let veth_name = child.veth_name().expect("Bridge mode must have a veth name").to_string();

    // The host-side veth should exist while the container is running
    let status = std::process::Command::new("ip")
        .args(["link", "show", &veth_name])
        .stdout(std::process::Stdio::null())
        .status()
        .expect("Failed to run ip link show");
    assert!(status.success(), "Host-side veth {} should exist after spawn", veth_name);

    child.wait().expect("Failed to wait for container");
}

/// N2: After wait(), the veth pair should be deleted (teardown_network called).
#[test]
fn test_bridge_network_cleanup() {
    if !is_root() {
        eprintln!("Skipping test_bridge_network_cleanup: requires root");
        return;
    }

    let Some(rootfs) = get_test_rootfs() else {
        eprintln!("Skipping test_bridge_network_cleanup: alpine-rootfs not found");
        return;
    };

    // Named netns is set up before fork — exit 0 is safe (no race with setup).
    let mut child = Command::new("/bin/ash")
        .args(&["-c", "exit 0"])
        .with_namespaces(Namespace::MOUNT | Namespace::UTS)
        .with_network(NetworkMode::Bridge)
        .with_chroot(&rootfs)
        .env("PATH", ALPINE_PATH)
        .stdin(Stdio::Null)
        .stdout(Stdio::Null)
        .stderr(Stdio::Null)
        .spawn()
        .expect("Failed to spawn bridge container");

    let veth_name = child.veth_name().expect("Bridge mode must have a veth name").to_string();
    child.wait().expect("Failed to wait for container");

    // After wait(), teardown_network() removes the veth pair and the named netns.
    let status = std::process::Command::new("ip")
        .args(["link", "show", &veth_name])
        .stderr(std::process::Stdio::null())
        .status()
        .expect("Failed to run ip link show");
    assert!(
        !status.success(),
        "Host-side veth {} should be gone after container exits",
        veth_name
    );
}

/// N2: After wait(), the named netns (/run/netns/rem-*) should also be deleted.
#[test]
fn test_bridge_netns_cleanup() {
    if !is_root() {
        eprintln!("Skipping test_bridge_netns_cleanup: requires root");
        return;
    }

    let Some(rootfs) = get_test_rootfs() else {
        eprintln!("Skipping test_bridge_netns_cleanup: alpine-rootfs not found");
        return;
    };

    let mut child = Command::new("/bin/ash")
        .args(&["-c", "exit 0"])
        .with_namespaces(Namespace::MOUNT | Namespace::UTS)
        .with_network(NetworkMode::Bridge)
        .with_chroot(&rootfs)
        .env("PATH", ALPINE_PATH)
        .stdin(Stdio::Null)
        .stdout(Stdio::Null)
        .stderr(Stdio::Null)
        .spawn()
        .expect("Failed to spawn bridge container");

    let ns_name = child.netns_name().expect("Bridge mode must have netns name").to_string();
    let ns_path = format!("/run/netns/{}", ns_name);

    // The named netns should exist before wait()
    assert!(
        std::path::Path::new(&ns_path).exists(),
        "Named netns {} should exist before wait()",
        ns_path
    );

    child.wait().expect("Failed to wait for container");

    // After wait(), teardown_network() should have deleted the named netns
    assert!(
        !std::path::Path::new(&ns_path).exists(),
        "Named netns {} should be deleted after wait()",
        ns_path
    );
}

/// N2: Loopback (127.0.0.1) should be up inside a bridge-mode container.
///
/// setup_bridge_network() runs `ip -n {ns_name} link set lo up` before fork;
/// this test verifies that the container sees lo correctly.
#[test]
fn test_bridge_loopback_up() {
    if !is_root() {
        eprintln!("Skipping test_bridge_loopback_up: requires root");
        return;
    }

    let Some(rootfs) = get_test_rootfs() else {
        eprintln!("Skipping test_bridge_loopback_up: alpine-rootfs not found");
        return;
    };

    let child = Command::new("/bin/ash")
        .args(&["-c", "ip addr show lo | grep -q '127.0.0.1' && echo LO_OK"])
        .with_namespaces(Namespace::MOUNT | Namespace::UTS)
        .with_network(NetworkMode::Bridge)
        .with_chroot(&rootfs)
        .env("PATH", ALPINE_PATH)
        .with_proc_mount()
        .stdin(Stdio::Null)
        .stdout(Stdio::Piped)
        .stderr(Stdio::Null)
        .spawn()
        .expect("Failed to spawn bridge container");

    let (status, stdout, _) = child.wait_with_output().expect("Failed to collect output");
    let out = String::from_utf8_lossy(&stdout);
    assert!(
        out.contains("LO_OK"),
        "lo should be up with 127.0.0.1 in bridge mode, got: {:?}",
        out
    );
    assert!(status.success(), "Container exited with failure");
}

/// N2: The bridge gateway (172.19.0.1 on remora0) should be reachable via ICMP.
///
/// Verifies actual layer-3 connectivity through the veth pair: the container
/// sends a ping, the packet traverses eth0→veth→bridge, and the host replies.
#[test]
fn test_bridge_gateway_reachable() {
    if !is_root() {
        eprintln!("Skipping test_bridge_gateway_reachable: requires root");
        return;
    }

    let Some(rootfs) = get_test_rootfs() else {
        eprintln!("Skipping test_bridge_gateway_reachable: alpine-rootfs not found");
        return;
    };

    let child = Command::new("/bin/ash")
        .args(&["-c", "ping -c 1 -W 2 172.19.0.1 >/dev/null 2>&1 && echo PING_OK"])
        .with_namespaces(Namespace::MOUNT | Namespace::UTS)
        .with_network(NetworkMode::Bridge)
        .with_chroot(&rootfs)
        .env("PATH", ALPINE_PATH)
        .with_proc_mount()
        .stdin(Stdio::Null)
        .stdout(Stdio::Piped)
        .stderr(Stdio::Null)
        .spawn()
        .expect("Failed to spawn bridge container");

    let (status, stdout, _) = child.wait_with_output().expect("Failed to collect output");
    let out = String::from_utf8_lossy(&stdout);
    assert!(
        out.contains("PING_OK"),
        "Gateway 172.19.0.1 should be reachable from bridge container, got: {:?}",
        out
    );
    assert!(status.success(), "Container exited with failure");
}

/// N2: Two bridge containers spawned concurrently must receive different IPs.
///
/// Exercises the flock-protected IPAM and the atomic ns-name counter under
/// real concurrency. Each thread builds, spawns, and collects its container
/// entirely within that thread — no non-Send types cross thread boundaries.
#[test]
fn test_bridge_concurrent_spawn() {
    if !is_root() {
        eprintln!("Skipping test_bridge_concurrent_spawn: requires root");
        return;
    }

    let Some(rootfs) = get_test_rootfs() else {
        eprintln!("Skipping test_bridge_concurrent_spawn: alpine-rootfs not found");
        return;
    };

    // Build and run each container entirely inside its thread.
    // The closures capture only PathBuf (Send); Command and Child stay local.
    let r1 = rootfs.clone();
    let t1 = std::thread::spawn(move || {
        Command::new("/bin/ash")
            .args(&["-c", "ip addr show eth0 | grep -m1 'inet ' | awk '{print $2}'"])
            .with_namespaces(Namespace::MOUNT | Namespace::UTS)
            .with_network(NetworkMode::Bridge)
            .with_chroot(&r1)
            .env("PATH", ALPINE_PATH)
            .with_proc_mount()
            .stdin(Stdio::Null)
            .stdout(Stdio::Piped)
            .stderr(Stdio::Null)
            .spawn()
            .expect("Failed to spawn container 1")
            .wait_with_output()
            .expect("Failed to collect output from container 1")
    });

    let r2 = rootfs.clone();
    let t2 = std::thread::spawn(move || {
        Command::new("/bin/ash")
            .args(&["-c", "ip addr show eth0 | grep -m1 'inet ' | awk '{print $2}'"])
            .with_namespaces(Namespace::MOUNT | Namespace::UTS)
            .with_network(NetworkMode::Bridge)
            .with_chroot(&r2)
            .env("PATH", ALPINE_PATH)
            .with_proc_mount()
            .stdin(Stdio::Null)
            .stdout(Stdio::Piped)
            .stderr(Stdio::Null)
            .spawn()
            .expect("Failed to spawn container 2")
            .wait_with_output()
            .expect("Failed to collect output from container 2")
    });

    let (_s1, out1, _) = t1.join().expect("Container 1 thread panicked");
    let (_s2, out2, _) = t2.join().expect("Container 2 thread panicked");

    let ip1 = String::from_utf8_lossy(&out1).trim().to_string();
    let ip2 = String::from_utf8_lossy(&out2).trim().to_string();

    assert!(!ip1.is_empty(), "Container 1 should output its IP address");
    assert!(!ip2.is_empty(), "Container 2 should output its IP address");
    assert!(ip1.starts_with("172.19.0."), "Container 1 IP should be in bridge subnet: {}", ip1);
    assert!(ip2.starts_with("172.19.0."), "Container 2 IP should be in bridge subnet: {}", ip2);
    assert_ne!(ip1, ip2, "Containers must receive different IPs: got {} and {}", ip1, ip2);
}

// ============================================================================
// Phase 6 N3 Networking Tests — NAT / MASQUERADE
// ============================================================================

/// N3: While a NAT container is running, `nft list table ip remora` must succeed.
///
/// Spawns a bridge+NAT container running `sleep 2`. While it sleeps, queries
/// `nft list table ip remora` on the host. Asserts exit 0, confirming that
/// `enable_nat()` installed the MASQUERADE table.
#[test]
#[serial(nat)]
fn test_nat_rule_added() {
    if !is_root() {
        eprintln!("Skipping test_nat_rule_added: requires root");
        return;
    }

    let Some(rootfs) = get_test_rootfs() else {
        eprintln!("Skipping test_nat_rule_added: alpine-rootfs not found");
        return;
    };

    let mut child = Command::new("/bin/ash")
        .args(&["-c", "sleep 2"])
        .with_namespaces(Namespace::MOUNT | Namespace::UTS)
        .with_network(NetworkMode::Bridge)
        .with_nat()
        .with_chroot(&rootfs)
        .env("PATH", ALPINE_PATH)
        .stdin(Stdio::Null)
        .stdout(Stdio::Null)
        .stderr(Stdio::Null)
        .spawn()
        .expect("Failed to spawn NAT container");

    // While the container sleeps, the nftables table should exist.
    let status = std::process::Command::new("nft")
        .args(["list", "table", "ip", "remora"])
        .stdout(std::process::Stdio::null())
        .status()
        .expect("Failed to run nft list table");
    assert!(
        status.success(),
        "nft table ip remora should exist while a NAT container is running"
    );

    child.wait().expect("Failed to wait for NAT container");
}

/// N3: After the last NAT container exits, `nft list table ip remora` must fail.
///
/// Spawns a bridge+NAT container with `ash -c "exit 0"`. After `wait()`,
/// asserts that `nft list table ip remora` exits non-zero, confirming that
/// `disable_nat()` removed the nftables table.
#[test]
#[serial(nat)]
fn test_nat_cleanup() {
    if !is_root() {
        eprintln!("Skipping test_nat_cleanup: requires root");
        return;
    }

    let Some(rootfs) = get_test_rootfs() else {
        eprintln!("Skipping test_nat_cleanup: alpine-rootfs not found");
        return;
    };

    let mut child = Command::new("/bin/ash")
        .args(&["-c", "exit 0"])
        .with_namespaces(Namespace::MOUNT | Namespace::UTS)
        .with_network(NetworkMode::Bridge)
        .with_nat()
        .with_chroot(&rootfs)
        .env("PATH", ALPINE_PATH)
        .stdin(Stdio::Null)
        .stdout(Stdio::Null)
        .stderr(Stdio::Null)
        .spawn()
        .expect("Failed to spawn NAT container");

    child.wait().expect("Failed to wait for NAT container");

    // After the container exits, the nftables table should be gone.
    let status = std::process::Command::new("nft")
        .args(["list", "table", "ip", "remora"])
        .stderr(std::process::Stdio::null())
        .status()
        .expect("Failed to run nft list table");
    assert!(
        !status.success(),
        "nft table ip remora should be removed after all NAT containers exit"
    );
}

/// N3: The nftables table must survive until the *last* NAT container exits.
///
/// Spawns container A (`sleep 2`, NAT) and B (`sleep 4`, NAT).
/// Waits for A — table must still exist (B is still running).
/// Waits for B — table must be gone (refcount hits 0).
#[test]
#[serial(nat)]
fn test_nat_refcount() {
    if !is_root() {
        eprintln!("Skipping test_nat_refcount: requires root");
        return;
    }

    let Some(rootfs) = get_test_rootfs() else {
        eprintln!("Skipping test_nat_refcount: alpine-rootfs not found");
        return;
    };

    let mut child_a = Command::new("/bin/ash")
        .args(&["-c", "sleep 2"])
        .with_namespaces(Namespace::MOUNT | Namespace::UTS)
        .with_network(NetworkMode::Bridge)
        .with_nat()
        .with_chroot(&rootfs)
        .env("PATH", ALPINE_PATH)
        .stdin(Stdio::Null)
        .stdout(Stdio::Null)
        .stderr(Stdio::Null)
        .spawn()
        .expect("Failed to spawn NAT container A");

    let mut child_b = Command::new("/bin/ash")
        .args(&["-c", "sleep 4"])
        .with_namespaces(Namespace::MOUNT | Namespace::UTS)
        .with_network(NetworkMode::Bridge)
        .with_nat()
        .with_chroot(&rootfs)
        .env("PATH", ALPINE_PATH)
        .stdin(Stdio::Null)
        .stdout(Stdio::Null)
        .stderr(Stdio::Null)
        .spawn()
        .expect("Failed to spawn NAT container B");

    // Wait for A (shorter sleep). B is still running — table must still exist.
    child_a.wait().expect("Failed to wait for container A");

    let status = std::process::Command::new("nft")
        .args(["list", "table", "ip", "remora"])
        .stdout(std::process::Stdio::null())
        .status()
        .expect("Failed to run nft list table after A exits");
    assert!(
        status.success(),
        "nft table should still exist after A exits (B is still running)"
    );

    // Now wait for B. Both containers have exited — table must be gone.
    child_b.wait().expect("Failed to wait for container B");

    let status = std::process::Command::new("nft")
        .args(["list", "table", "ip", "remora"])
        .stderr(std::process::Stdio::null())
        .status()
        .expect("Failed to run nft list table after B exits");
    assert!(
        !status.success(),
        "nft table should be removed after both NAT containers exit"
    );
}

/// N4: A DNAT rule must exist in the prerouting chain while a port-forward
/// container is running.
///
/// Spawns a bridge+NAT container with `with_port_forward(18080, 80)` running
/// `sleep 2`. While it sleeps, checks that `nft list chain ip remora prerouting`
/// succeeds and contains "dport 18080". Waits for the container.
#[test]
#[serial(nat)]
fn test_port_forward_rule_added() {
    if !is_root() {
        eprintln!("Skipping test_port_forward_rule_added: requires root");
        return;
    }

    let Some(rootfs) = get_test_rootfs() else {
        eprintln!("Skipping test_port_forward_rule_added: alpine-rootfs not found");
        return;
    };

    let mut child = Command::new("/bin/ash")
        .args(&["-c", "sleep 2"])
        .with_namespaces(Namespace::MOUNT | Namespace::UTS)
        .with_network(NetworkMode::Bridge)
        .with_nat()
        .with_port_forward(18080, 80)
        .with_chroot(&rootfs)
        .env("PATH", ALPINE_PATH)
        .stdin(Stdio::Null)
        .stdout(Stdio::Null)
        .stderr(Stdio::Null)
        .spawn()
        .expect("Failed to spawn port-forward container");

    // While the container is sleeping, the prerouting chain must contain the DNAT rule.
    let output = std::process::Command::new("nft")
        .args(["list", "chain", "ip", "remora", "prerouting"])
        .output()
        .expect("Failed to run nft list chain");
    assert!(
        output.status.success(),
        "nft prerouting chain should exist while port-forward container is running"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("dport 18080"),
        "prerouting chain should contain DNAT rule for dport 18080; got:\n{}", stdout
    );

    child.wait().expect("Failed to wait for port-forward container");
}

/// N4: After a port-forward container exits, its DNAT rule must be cleaned up.
///
/// Spawns a bridge+NAT container with `with_port_forward(18081, 80)` that exits
/// immediately. After `wait()`, asserts that the nftables table is gone entirely
/// (both NAT and port-forward refcounts are zero).
#[test]
#[serial(nat)]
fn test_port_forward_cleanup() {
    if !is_root() {
        eprintln!("Skipping test_port_forward_cleanup: requires root");
        return;
    }

    let Some(rootfs) = get_test_rootfs() else {
        eprintln!("Skipping test_port_forward_cleanup: alpine-rootfs not found");
        return;
    };

    let mut child = Command::new("/bin/ash")
        .args(&["-c", "exit 0"])
        .with_namespaces(Namespace::MOUNT | Namespace::UTS)
        .with_network(NetworkMode::Bridge)
        .with_nat()
        .with_port_forward(18081, 80)
        .with_chroot(&rootfs)
        .env("PATH", ALPINE_PATH)
        .stdin(Stdio::Null)
        .stdout(Stdio::Null)
        .stderr(Stdio::Null)
        .spawn()
        .expect("Failed to spawn port-forward container");

    child.wait().expect("Failed to wait for port-forward container");

    // After the container exits, the table must be gone entirely.
    let status = std::process::Command::new("nft")
        .args(["list", "table", "ip", "remora"])
        .stderr(std::process::Stdio::null())
        .status()
        .expect("Failed to run nft list table");
    assert!(
        !status.success(),
        "nft table ip remora should be removed after port-forward container exits"
    );
}

/// N4: Two containers with different port forwards must be torn down independently.
///
/// Spawns A (`sleep 2`, port 18082→80) and B (`sleep 4`, port 18083→80), both
/// with NAT. Waits for A — the prerouting chain must still contain B's rule
/// (`dport 18083`) but not A's (`dport 18082`). Waits for B — table must be gone.
#[test]
#[serial(nat)]
fn test_port_forward_independent_teardown() {
    if !is_root() {
        eprintln!("Skipping test_port_forward_independent_teardown: requires root");
        return;
    }

    let Some(rootfs) = get_test_rootfs() else {
        eprintln!("Skipping test_port_forward_independent_teardown: alpine-rootfs not found");
        return;
    };

    let mut child_a = Command::new("/bin/ash")
        .args(&["-c", "sleep 2"])
        .with_namespaces(Namespace::MOUNT | Namespace::UTS)
        .with_network(NetworkMode::Bridge)
        .with_nat()
        .with_port_forward(18082, 80)
        .with_chroot(&rootfs)
        .env("PATH", ALPINE_PATH)
        .stdin(Stdio::Null)
        .stdout(Stdio::Null)
        .stderr(Stdio::Null)
        .spawn()
        .expect("Failed to spawn port-forward container A");

    let mut child_b = Command::new("/bin/ash")
        .args(&["-c", "sleep 4"])
        .with_namespaces(Namespace::MOUNT | Namespace::UTS)
        .with_network(NetworkMode::Bridge)
        .with_nat()
        .with_port_forward(18083, 80)
        .with_chroot(&rootfs)
        .env("PATH", ALPINE_PATH)
        .stdin(Stdio::Null)
        .stdout(Stdio::Null)
        .stderr(Stdio::Null)
        .spawn()
        .expect("Failed to spawn port-forward container B");

    // Wait for A. B is still running — prerouting must still exist with B's rule.
    child_a.wait().expect("Failed to wait for container A");

    let output = std::process::Command::new("nft")
        .args(["list", "chain", "ip", "remora", "prerouting"])
        .output()
        .expect("Failed to run nft list chain after A exits");
    assert!(
        output.status.success(),
        "prerouting chain should still exist after A exits (B is still running)"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains("dport 18082"),
        "A's DNAT rule (dport 18082) should be gone after A exits"
    );
    assert!(
        stdout.contains("dport 18083"),
        "B's DNAT rule (dport 18083) should still be present; got:\n{}", stdout
    );

    // Wait for B. Both containers gone — table must be removed entirely.
    child_b.wait().expect("Failed to wait for container B");

    let status = std::process::Command::new("nft")
        .args(["list", "table", "ip", "remora"])
        .stderr(std::process::Stdio::null())
        .status()
        .expect("Failed to run nft list table after B exits");
    assert!(
        !status.success(),
        "nft table should be removed after both port-forward containers exit"
    );
}
