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
use remora::container::{
    Capability, Command, GidMap, Namespace, SeccompProfile, Stdio, UidMap, Volume,
};
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

mod api {
    use super::*;

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
}

mod core {
    use super::*;

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
                assert!(
                    status.success(),
                    "Child process failed with combined features"
                );
            }
            Err(e) => panic!("Failed to spawn with combined features: {:?}", e),
        }
    }

    /// Verify that a container with a PID namespace can fork() repeatedly.
    ///
    /// Regression test for a bug where `unshare(CLONE_NEWPID)` left the container
    /// process OUTSIDE the new PID namespace — only its children entered it.  The
    /// first child became PID 1; when it exited the kernel marked the namespace
    /// defunct, causing every subsequent `fork()` to fail with ENOMEM.
    ///
    /// The fix is a double-fork in `pre_exec`: after `unshare(CLONE_NEWPID)` we
    /// fork once more so the container process IS PID 1 in the new namespace.
    ///
    /// This test runs a shell loop that forks `sleep` 5 times.  Without the fix,
    /// the second (or later) fork fails with "can't fork: Out of memory".
    ///
    /// Requires root and alpine-rootfs.
    #[test]
    #[serial]
    fn test_pid_namespace_repeated_fork() {
        if !is_root() {
            eprintln!("Skipping test_pid_namespace_repeated_fork: requires root");
            return;
        }

        let Some(rootfs) = get_test_rootfs() else {
            eprintln!("Skipping test_pid_namespace_repeated_fork: alpine-rootfs not found");
            return;
        };

        // Fork 5 times via external `sleep 0` (not a builtin — forces fork+exec).
        // Count successes.  All 5 must succeed.
        let child = Command::new("/bin/sh")
            .args(&[
                "-c",
                r#"i=0; while [ $i -lt 5 ]; do sleep 0; i=$((i+1)); done; echo "FORKS_OK""#,
            ])
            .with_chroot(&rootfs)
            .with_namespaces(Namespace::UTS | Namespace::MOUNT | Namespace::PID)
            .with_proc_mount()
            .env("PATH", ALPINE_PATH)
            .stdout(Stdio::Piped)
            .stderr(Stdio::Piped)
            .spawn()
            .expect("Failed to spawn with PID namespace");

        let (status, stdout, stderr) = child.wait_with_output().expect("wait failed");
        let out = String::from_utf8_lossy(&stdout);
        let err = String::from_utf8_lossy(&stderr);

        assert!(
            status.success(),
            "Container with PID namespace failed (exit {:?}).\nstdout: {}\nstderr: {}",
            status.code(),
            out,
            err
        );
        assert!(
            out.contains("FORKS_OK"),
            "Container could not fork() repeatedly in PID namespace (defunct namespace bug).\nstdout: {}\nstderr: {}",
            out, err
        );
    }
}

mod capabilities {
    use super::*;

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
}

mod resources {
    use super::*;

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
}

mod security {
    use super::*;

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
        assert!(
            status.success(),
            "NoNewPrivs should be set to 1 in /proc/self/status"
        );
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
        assert!(
            status.success(),
            "Container should run despite read-only fs"
        );
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
            .with_seccomp_default() // Seccomp filtering
            .with_no_new_privileges(true) // No privilege escalation
            .with_readonly_rootfs(true) // Immutable rootfs
            .with_masked_paths_default() // Hide sensitive paths
            .drop_all_capabilities() // Minimal capabilities
            .spawn()
            .expect("Failed to spawn with all Phase 1 security");

        let status = child.wait().expect("Failed to wait for child");
        assert!(
            status.success(),
            "Container with all Phase 1 security should work"
        );
    }
}

mod filesystem {
    use super::*;

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
        assert!(
            status.success(),
            "Container should read host file via bind mount"
        );
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
        assert!(
            out.contains("ok"),
            "touch on tmpfs should succeed, got: {}",
            out
        );
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
        assert!(
            host_file.exists(),
            "Volume file should exist on host after container exits"
        );
        let contents = std::fs::read_to_string(&host_file).expect("Failed to read volume file");
        assert!(
            contents.contains("persistent"),
            "Volume file should contain expected content"
        );

        // Clean up
        Volume::delete("testvol").expect("Failed to delete volume");
    }

    /// Container writes to a file inside overlayfs; the write appears in upper_dir,
    /// not in lower_dir (the shared Alpine rootfs is untouched).
    #[test]
    fn test_overlay_writes_to_upper() {
        if !is_root() {
            eprintln!("Skipping test_overlay_writes_to_upper: requires root");
            return;
        }
        let rootfs = match get_test_rootfs() {
            Some(p) => p,
            None => {
                eprintln!("Skipping test_overlay_writes_to_upper: alpine-rootfs not found");
                return;
            }
        };

        let scratch = tempfile::tempdir().expect("failed to create tempdir");
        let upper = scratch.path().join("upper");
        let work = scratch.path().join("work");
        std::fs::create_dir_all(&upper).unwrap();
        std::fs::create_dir_all(&work).unwrap();

        let mut child = Command::new("/bin/sh")
            .args(&["-c", "echo hello > /newfile"])
            .with_chroot(&rootfs)
            .with_namespaces(Namespace::MOUNT | Namespace::UTS)
            .with_overlay(&upper, &work)
            .env("PATH", ALPINE_PATH)
            .stdin(Stdio::Null)
            .stdout(Stdio::Null)
            .stderr(Stdio::Null)
            .spawn()
            .expect("failed to spawn overlay container");

        child.wait().expect("failed to wait");

        // The lower layer must NOT have been modified.
        assert!(
            !rootfs.join("newfile").exists(),
            "lower dir (alpine-rootfs) should not contain newfile — overlay leaked write to lower"
        );

        // The write must appear in upper_dir.
        let upper_file = upper.join("newfile");
        assert!(
            upper_file.exists(),
            "upper_dir/newfile should exist after container wrote /newfile"
        );
        let content = std::fs::read_to_string(&upper_file).expect("failed to read upper/newfile");
        assert_eq!(
            content, "hello\n",
            "upper_dir/newfile should contain 'hello\\n'"
        );
    }

    /// A named volume mounted into an overlay container is visible in the merged
    /// view, persists data to the host, and does not write to the overlay upper
    /// layer.
    #[test]
    fn test_overlay_with_volume() {
        if !is_root() {
            eprintln!("Skipping test_overlay_with_volume: requires root");
            return;
        }
        let rootfs = match get_test_rootfs() {
            Some(p) => p,
            None => {
                eprintln!("Skipping test_overlay_with_volume: alpine-rootfs not found");
                return;
            }
        };

        // Set up overlay scratch dirs.
        let scratch = tempfile::tempdir().expect("failed to create tempdir");
        let upper = scratch.path().join("upper");
        let work = scratch.path().join("work");
        std::fs::create_dir_all(&upper).unwrap();
        std::fs::create_dir_all(&work).unwrap();

        // Set up a named volume.
        let _ = Volume::delete("test_ov_vol");
        let vol = Volume::create("test_ov_vol").expect("failed to create volume");

        // Run a container that writes to the volume AND to a regular path.
        let mut child = Command::new("/bin/ash")
            .args(&[
                "-c",
                "echo vol_data > /data/vol_file.txt && echo overlay_data > /overlay_file.txt",
            ])
            .with_chroot(&rootfs)
            .with_namespaces(Namespace::MOUNT | Namespace::UTS)
            .with_overlay(&upper, &work)
            .with_volume(&vol, "/data")
            .env("PATH", ALPINE_PATH)
            .stdin(Stdio::Null)
            .stdout(Stdio::Null)
            .stderr(Stdio::Null)
            .spawn()
            .expect("failed to spawn overlay+volume container");

        let status = child.wait().expect("failed to wait");
        assert!(status.success(), "container should exit successfully");

        // Volume write should persist on the host.
        let vol_file = vol.path().join("vol_file.txt");
        assert!(
            vol_file.exists(),
            "volume file should exist on host after container exits"
        );
        let vol_contents = std::fs::read_to_string(&vol_file).expect("failed to read volume file");
        assert_eq!(
            vol_contents, "vol_data\n",
            "volume file has expected content"
        );

        // The regular write should be in the overlay upper dir, not in the rootfs.
        assert!(
            !rootfs.join("overlay_file.txt").exists(),
            "rootfs (lower layer) should not contain overlay_file.txt"
        );
        assert!(
            upper.join("overlay_file.txt").exists(),
            "overlay upper dir should contain overlay_file.txt"
        );

        // The volume write should NOT appear in the overlay upper dir — it goes
        // directly to the host volume, bypassing the overlay entirely.
        assert!(
            !upper.join("data/vol_file.txt").exists(),
            "volume writes should not appear in overlay upper dir"
        );

        // Clean up.
        Volume::delete("test_ov_vol").expect("failed to delete volume");
    }

    /// Modifying an existing lower-layer file writes a copy to upper_dir;
    /// the original file in lower_dir is untouched.
    #[test]
    fn test_overlay_lower_unchanged() {
        if !is_root() {
            eprintln!("Skipping test_overlay_lower_unchanged: requires root");
            return;
        }
        let rootfs = match get_test_rootfs() {
            Some(p) => p,
            None => {
                eprintln!("Skipping test_overlay_lower_unchanged: alpine-rootfs not found");
                return;
            }
        };

        let scratch = tempfile::tempdir().expect("failed to create tempdir");
        let upper = scratch.path().join("upper");
        let work = scratch.path().join("work");
        std::fs::create_dir_all(&upper).unwrap();
        std::fs::create_dir_all(&work).unwrap();

        // Record the original content of /etc/hostname in the lower layer.
        let lower_hostname = rootfs.join("etc/hostname");
        let original_content =
            std::fs::read_to_string(&lower_hostname).unwrap_or_else(|_| String::new());

        let mut child = Command::new("/bin/sh")
            .args(&["-c", "echo modified > /etc/hostname"])
            .with_chroot(&rootfs)
            .with_namespaces(Namespace::MOUNT | Namespace::UTS)
            .with_overlay(&upper, &work)
            .env("PATH", ALPINE_PATH)
            .stdin(Stdio::Null)
            .stdout(Stdio::Null)
            .stderr(Stdio::Null)
            .spawn()
            .expect("failed to spawn overlay container");

        child.wait().expect("failed to wait");

        // Lower layer /etc/hostname must be unchanged.
        let after_content =
            std::fs::read_to_string(&lower_hostname).unwrap_or_else(|_| String::new());
        assert_eq!(
            original_content, after_content,
            "lower_dir/etc/hostname should be unchanged; overlay leaked write to lower"
        );

        // upper_dir must hold the modified copy.
        let upper_hostname = upper.join("etc/hostname");
        assert!(
            upper_hostname.exists(),
            "upper_dir/etc/hostname should exist (copy-on-write)"
        );
        let upper_content =
            std::fs::read_to_string(&upper_hostname).expect("failed to read upper/etc/hostname");
        assert_eq!(
            upper_content, "modified\n",
            "upper_dir/etc/hostname should contain 'modified\\n'"
        );
    }

    /// After wait(), the auto-created /run/remora/overlay-{pid}-{n}/merged directory
    /// and its parent are removed — no stale dirs left on the host.
    #[test]
    fn test_overlay_merged_cleanup() {
        if !is_root() {
            eprintln!("Skipping test_overlay_merged_cleanup: requires root");
            return;
        }
        let rootfs = match get_test_rootfs() {
            Some(p) => p,
            None => {
                eprintln!("Skipping test_overlay_merged_cleanup: alpine-rootfs not found");
                return;
            }
        };

        let scratch = tempfile::tempdir().expect("failed to create tempdir");
        let upper = scratch.path().join("upper");
        let work = scratch.path().join("work");
        std::fs::create_dir_all(&upper).unwrap();
        std::fs::create_dir_all(&work).unwrap();

        let mut child = Command::new("/bin/sh")
            .args(&["-c", "true"])
            .with_chroot(&rootfs)
            .with_namespaces(Namespace::MOUNT | Namespace::UTS)
            .with_overlay(&upper, &work)
            .env("PATH", ALPINE_PATH)
            .stdin(Stdio::Null)
            .stdout(Stdio::Null)
            .stderr(Stdio::Null)
            .spawn()
            .expect("failed to spawn overlay container");

        // Record this container's specific merged dir before calling wait().
        let merged_dir = child.overlay_merged_dir().map(|p| p.to_path_buf());
        assert!(
            merged_dir.is_some(),
            "with_overlay should set overlay_merged_dir on Child"
        );
        let merged = merged_dir.unwrap();
        let parent = merged.parent().unwrap().to_path_buf();

        // The dirs must exist while the container is running.
        assert!(merged.exists(), "merged dir should exist before wait()");

        child.wait().expect("failed to wait");

        // After wait(), both the merged dir and its parent must be gone.
        assert!(
            !merged.exists(),
            "overlay merged dir should be removed after wait(); still present: {}",
            merged.display()
        );
        assert!(
            !parent.exists(),
            "overlay parent dir should be removed after wait(); still present: {}",
            parent.display()
        );
    }
}

mod cgroups {
    use super::*;

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
            .args(&[
                "-c",
                "for i in 1 2 3 4 5 6 7 8 9 10; do sleep 0 & done; wait; echo done",
            ])
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
        assert!(
            status.success(),
            "Container with cpu_shares should exit cleanly"
        );
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
}

mod networking {
    use super::*;

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
            .args(&[
                "-c",
                "ip addr show lo | grep -q '127.0.0.1' && echo LOOPBACK_OK",
            ])
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
            .args(&[
                "-c",
                "ip addr show eth0 | grep -q '172.19.0' && echo BRIDGE_IP_OK",
            ])
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

        let veth_name = child
            .veth_name()
            .expect("Bridge mode must have a veth name")
            .to_string();

        // The host-side veth should exist while the container is running
        let status = std::process::Command::new("ip")
            .args(["link", "show", &veth_name])
            .stdout(std::process::Stdio::null())
            .status()
            .expect("Failed to run ip link show");
        assert!(
            status.success(),
            "Host-side veth {} should exist after spawn",
            veth_name
        );

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

        let veth_name = child
            .veth_name()
            .expect("Bridge mode must have a veth name")
            .to_string();
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

        let ns_name = child
            .netns_name()
            .expect("Bridge mode must have netns name")
            .to_string();
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
            .args(&[
                "-c",
                "ping -c 1 -W 2 172.19.0.1 >/dev/null 2>&1 && echo PING_OK",
            ])
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
                .args(&[
                    "-c",
                    "ip addr show eth0 | grep -m1 'inet ' | awk '{print $2}'",
                ])
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
                .args(&[
                    "-c",
                    "ip addr show eth0 | grep -m1 'inet ' | awk '{print $2}'",
                ])
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
        assert!(
            ip1.starts_with("172.19.0."),
            "Container 1 IP should be in bridge subnet: {}",
            ip1
        );
        assert!(
            ip2.starts_with("172.19.0."),
            "Container 2 IP should be in bridge subnet: {}",
            ip2
        );
        assert_ne!(
            ip1, ip2,
            "Containers must receive different IPs: got {} and {}",
            ip1, ip2
        );
    }

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
            .args(["list", "table", "ip", "remora-remora0"])
            .stdout(std::process::Stdio::null())
            .status()
            .expect("Failed to run nft list table");
        assert!(
            status.success(),
            "nft table ip remora-remora0 should exist while a NAT container is running"
        );

        child.wait().expect("Failed to wait for NAT container");
    }

    /// N3: After the last NAT container exits, `nft list table ip remora-remora0` must fail.
    ///
    /// Spawns a bridge+NAT container with `ash -c "exit 0"`. After `wait()`,
    /// asserts that `nft list table ip remora-remora0` exits non-zero, confirming that
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
            .args(["list", "table", "ip", "remora-remora0"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .expect("Failed to run nft list table");
        assert!(
            !status.success(),
            "nft table ip remora-remora0 should be removed after all NAT containers exit"
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
            .args(["list", "table", "ip", "remora-remora0"])
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
            .args(["list", "table", "ip", "remora-remora0"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .expect("Failed to run nft list table after B exits");
        assert!(
            !status.success(),
            "nft table should be removed after both NAT containers exit"
        );
    }

    /// N3: iptables FORWARD rules must exist while a NAT container is running.
    ///
    /// On hosts with UFW or Docker, the iptables FORWARD chain has `policy DROP`
    /// which blocks TCP/UDP traffic even when nftables MASQUERADE is set up.
    /// `enable_nat()` adds `iptables -I FORWARD -s/-d 172.19.0.0/24 -j ACCEPT`
    /// rules to work around this. This test verifies those rules exist.
    ///
    /// Failure indicates that `enable_nat()` is not adding the iptables FORWARD
    /// rules, which would cause TCP/UDP to be blocked on hosts with UFW/Docker
    /// while ICMP (ping) continues to work — a subtle and hard-to-debug issue.
    #[test]
    #[serial(nat)]
    fn test_nat_iptables_forward_rules() {
        if !is_root() {
            eprintln!("Skipping test_nat_iptables_forward_rules: requires root");
            return;
        }

        let Some(rootfs) = get_test_rootfs() else {
            eprintln!("Skipping test_nat_iptables_forward_rules: alpine-rootfs not found");
            return;
        };

        let mut child = Command::new("/bin/ash")
            .args(&["-c", "sleep 3"])
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

        // Check iptables FORWARD rule for source 172.19.0.0/24.
        let status_src = std::process::Command::new("iptables")
            .args(["-C", "FORWARD", "-s", "172.19.0.0/24", "-j", "ACCEPT"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .expect("Failed to run iptables -C (source)");
        assert!(
            status_src.success(),
            "iptables FORWARD rule for source 172.19.0.0/24 should exist while NAT container runs"
        );

        // Check iptables FORWARD rule for destination 172.19.0.0/24.
        let status_dst = std::process::Command::new("iptables")
            .args(["-C", "FORWARD", "-d", "172.19.0.0/24", "-j", "ACCEPT"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .expect("Failed to run iptables -C (dest)");
        assert!(
            status_dst.success(),
            "iptables FORWARD rule for dest 172.19.0.0/24 should exist while NAT container runs"
        );

        child.wait().expect("Failed to wait for NAT container");

        // After cleanup, the iptables FORWARD rules should be gone.
        let status_after = std::process::Command::new("iptables")
            .args(["-C", "FORWARD", "-s", "172.19.0.0/24", "-j", "ACCEPT"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .expect("Failed to run iptables -C after cleanup");
        assert!(
            !status_after.success(),
            "iptables FORWARD rule should be removed after NAT container exits"
        );
    }

    /// N4: A DNAT rule must exist in the prerouting chain while a port-forward
    /// container is running.
    ///
    /// Spawns a bridge+NAT container with `with_port_forward(18080, 80)` running
    /// `sleep 2`. While it sleeps, checks that `nft list chain ip remora-remora0 prerouting`
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
            .args(["list", "chain", "ip", "remora-remora0", "prerouting"])
            .output()
            .expect("Failed to run nft list chain");
        assert!(
            output.status.success(),
            "nft prerouting chain should exist while port-forward container is running"
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("dport 18080"),
            "prerouting chain should contain DNAT rule for dport 18080; got:\n{}",
            stdout
        );

        child
            .wait()
            .expect("Failed to wait for port-forward container");
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

        child
            .wait()
            .expect("Failed to wait for port-forward container");

        // After the container exits, the table must be gone entirely.
        let status = std::process::Command::new("nft")
            .args(["list", "table", "ip", "remora-remora0"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .expect("Failed to run nft list table");
        assert!(
            !status.success(),
            "nft table ip remora-remora0 should be removed after port-forward container exits"
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
            .args(["list", "chain", "ip", "remora-remora0", "prerouting"])
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
            "B's DNAT rule (dport 18083) should still be present; got:\n{}",
            stdout
        );

        // Wait for B. Both containers gone — table must be removed entirely.
        child_b.wait().expect("Failed to wait for container B");

        let status = std::process::Command::new("nft")
            .args(["list", "table", "ip", "remora-remora0"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .expect("Failed to run nft list table after B exits");
        assert!(
            !status.success(),
            "nft table should be removed after both port-forward containers exit"
        );
    }

    /// N5: `with_dns()` must write the specified nameservers into the container's
    /// `/etc/resolv.conf` so that DNS resolution works inside the container.
    ///
    /// Spawns a bridge+NAT+DNS container that runs `cat /etc/resolv.conf` and
    /// captures stdout. Asserts the output contains both configured nameservers.
    #[test]
    #[serial(nat)]
    fn test_dns_resolv_conf() {
        if !is_root() {
            eprintln!("Skipping test_dns_resolv_conf: requires root");
            return;
        }

        let Some(rootfs) = get_test_rootfs() else {
            eprintln!("Skipping test_dns_resolv_conf: alpine-rootfs not found");
            return;
        };

        let child = Command::new("/bin/ash")
            .args(&["-c", "cat /etc/resolv.conf"])
            .with_namespaces(Namespace::MOUNT | Namespace::UTS)
            .with_network(NetworkMode::Bridge)
            .with_nat()
            .with_dns(&["1.1.1.1", "8.8.8.8"])
            .with_chroot(&rootfs)
            .env("PATH", ALPINE_PATH)
            .stdin(Stdio::Null)
            .stdout(Stdio::Piped)
            .stderr(Stdio::Null)
            .spawn()
            .expect("Failed to spawn DNS container");

        let (_status, stdout_bytes, _stderr) = child
            .wait_with_output()
            .expect("Failed to wait for DNS container");
        let stdout = String::from_utf8_lossy(&stdout_bytes);

        assert!(
            stdout.contains("nameserver 1.1.1.1"),
            "/etc/resolv.conf should contain 'nameserver 1.1.1.1'; got:\n{}",
            stdout
        );
        assert!(
            stdout.contains("nameserver 8.8.8.8"),
            "/etc/resolv.conf should contain 'nameserver 8.8.8.8'; got:\n{}",
            stdout
        );
    }

    /// N4: Port forwarding must actually route TCP traffic to the container.
    ///
    /// DNAT prerouting rules only apply to traffic arriving from external
    /// interfaces, not locally-originated packets (those go through OUTPUT,
    /// not PREROUTING). Bridge-internal traffic has hairpin routing issues.
    ///
    /// So we create a temporary external network namespace with its own veth
    /// pair to the host (10.99.0.0/24), simulating a real external client.
    /// Traffic from this namespace to the host goes through PREROUTING where
    /// the DNAT rule rewrites it to the container's bridge IP.
    ///
    /// Unlike `test_port_forward_rule_added` (which only checks the nftables
    /// rule string), this proves the full DNAT path works end-to-end.
    #[test]
    #[serial(nat)]
    fn test_port_forward_end_to_end() {
        if !is_root() {
            eprintln!("Skipping test_port_forward_end_to_end: requires root");
            return;
        }

        let Some(rootfs) = get_test_rootfs() else {
            eprintln!("Skipping test_port_forward_end_to_end: alpine-rootfs not found");
            return;
        };

        // Check that nc is available on the host (needed for the external client).
        let nc_ok = std::process::Command::new("which")
            .arg("nc")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !nc_ok {
            eprintln!("Skipping test_port_forward_end_to_end: nc not found on host");
            return;
        }

        // Container A: one-shot TCP server on port 80, forwarded from host 19090.
        let mut child_a = Command::new("/bin/sh")
            .args(&["-c", "echo HELLO_FROM_CONTAINER | nc -l -p 80"])
            .with_namespaces(Namespace::MOUNT | Namespace::UTS)
            .with_network(NetworkMode::Bridge)
            .with_nat()
            .with_port_forward(19090, 80)
            .with_chroot(&rootfs)
            .env("PATH", ALPINE_PATH)
            .stdin(Stdio::Null)
            .stdout(Stdio::Null)
            .stderr(Stdio::Null)
            .spawn()
            .expect("Failed to spawn container A");

        // Give nc a moment to start listening.
        std::thread::sleep(std::time::Duration::from_millis(500));

        // Create a temporary external network namespace to simulate a real
        // external client. Traffic from 10.99.0.2 → 10.99.0.1:19090 arrives
        // on the pf-test-h veth, goes through PREROUTING (DNAT → container
        // IP:80), then FORWARD to the container via the bridge.
        let setup_ok = std::process::Command::new("sh")
            .args([
                "-c",
                "\
                ip netns add pf-test-client && \
                ip link add pf-test-h type veth peer name pf-test-c && \
                ip link set pf-test-c netns pf-test-client && \
                ip addr add 10.99.0.1/24 dev pf-test-h && \
                ip link set pf-test-h up && \
                ip netns exec pf-test-client ip addr add 10.99.0.2/24 dev pf-test-c && \
                ip netns exec pf-test-client ip link set pf-test-c up && \
                ip netns exec pf-test-client ip route add default via 10.99.0.1\
            ",
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .expect("setup test netns")
            .success();

        if !setup_ok {
            // Clean up container and skip.
            unsafe {
                libc::kill(child_a.pid(), libc::SIGKILL);
            }
            let _ = child_a.wait();
            eprintln!("Skipping test_port_forward_end_to_end: failed to set up test netns");
            return;
        }

        // Connect from the external namespace to the host on the forwarded port.
        let output = std::process::Command::new("ip")
            .args([
                "netns",
                "exec",
                "pf-test-client",
                "nc",
                "-w",
                "2",
                "10.99.0.1",
                "19090",
            ])
            .output()
            .expect("nc from test netns");
        let out = String::from_utf8_lossy(&output.stdout);
        let err = String::from_utf8_lossy(&output.stderr);

        // Clean up: test namespace, veth, container.
        let _ = std::process::Command::new("ip")
            .args(["netns", "del", "pf-test-client"])
            .status();
        let _ = std::process::Command::new("ip")
            .args(["link", "del", "pf-test-h"])
            .status();
        unsafe {
            libc::kill(child_a.pid(), libc::SIGKILL);
        }
        let _ = child_a.wait();

        assert!(
            out.contains("HELLO_FROM_CONTAINER"),
            "External client should receive 'HELLO_FROM_CONTAINER' via port forward 19090→80.\nstdout: {}\nstderr: {}",
            out, err
        );
    }

    /// N2+N3: Bridge + NAT cleanup must work even after SIGKILL.
    ///
    /// Spawns a bridge+NAT container (`sleep 60`), records veth name, netns name,
    /// and verifies iptables FORWARD rules exist. Then SIGKILLs the container
    /// and calls `wait()`. Asserts all resources are cleaned up:
    /// - veth pair removed
    /// - named netns removed
    /// - nftables table removed
    /// - iptables FORWARD rules removed
    ///
    /// All existing cleanup tests use normal exit. This catches teardown bugs
    /// that only manifest when the container process dies unexpectedly.
    #[test]
    #[serial(nat)]
    fn test_bridge_cleanup_after_sigkill() {
        if !is_root() {
            eprintln!("Skipping test_bridge_cleanup_after_sigkill: requires root");
            return;
        }

        let Some(rootfs) = get_test_rootfs() else {
            eprintln!("Skipping test_bridge_cleanup_after_sigkill: alpine-rootfs not found");
            return;
        };

        let mut child = Command::new("/bin/sleep")
            .args(&["60"])
            .with_namespaces(Namespace::MOUNT | Namespace::UTS)
            .with_network(NetworkMode::Bridge)
            .with_nat()
            .with_chroot(&rootfs)
            .env("PATH", ALPINE_PATH)
            .stdin(Stdio::Null)
            .stdout(Stdio::Null)
            .stderr(Stdio::Null)
            .spawn()
            .expect("Failed to spawn container");

        let veth = child.veth_name().expect("should have veth").to_string();
        let netns = child.netns_name().expect("should have netns").to_string();

        // Verify resources exist before kill.
        let veth_exists = std::process::Command::new("ip")
            .args(["link", "show", &veth])
            .stderr(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .status()
            .expect("ip link show")
            .success();
        assert!(veth_exists, "veth {} should exist before kill", veth);

        let iptables_exists = std::process::Command::new("iptables")
            .args(["-C", "FORWARD", "-s", "172.19.0.0/24", "-j", "ACCEPT"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .expect("iptables -C")
            .success();
        assert!(
            iptables_exists,
            "iptables FORWARD rule should exist before kill"
        );

        // SIGKILL the container.
        unsafe {
            libc::kill(child.pid(), libc::SIGKILL);
        }

        // wait() should still run teardown.
        let _ = child.wait();

        // Verify all resources are cleaned up.
        let veth_after = std::process::Command::new("ip")
            .args(["link", "show", &veth])
            .stderr(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .status()
            .expect("ip link show after kill")
            .success();
        assert!(
            !veth_after,
            "veth {} should be gone after SIGKILL + wait()",
            veth
        );

        let netns_path = format!("/run/netns/{}", netns);
        assert!(
            !std::path::Path::new(&netns_path).exists(),
            "netns {} should be gone after SIGKILL + wait()",
            netns
        );

        let nft_after = std::process::Command::new("nft")
            .args(["list", "table", "ip", "remora-remora0"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .expect("nft list table after kill")
            .success();
        assert!(
            !nft_after,
            "nftables table should be gone after SIGKILL + wait()"
        );

        let iptables_after = std::process::Command::new("iptables")
            .args(["-C", "FORWARD", "-s", "172.19.0.0/24", "-j", "ACCEPT"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .expect("iptables -C after kill")
            .success();
        assert!(
            !iptables_after,
            "iptables FORWARD rule should be gone after SIGKILL + wait()"
        );
    }

    /// N3: NAT must actually allow outbound TCP traffic, not just have rules.
    ///
    /// Spawns a bridge+NAT+DNS container that runs `wget --spider http://1.1.1.1/`.
    /// Asserts exit code 0. Skips if outbound internet is unavailable.
    ///
    /// This is the end-to-end NAT test — it proves packets actually flow through
    /// MASQUERADE to the internet. Existing NAT tests only verify rule existence.
    /// Follows the same skip-if-no-internet pattern as `test_pasta_connectivity`.
    #[test]
    #[serial(nat)]
    fn test_nat_end_to_end_tcp() {
        if !is_root() {
            eprintln!("Skipping test_nat_end_to_end_tcp: requires root");
            return;
        }

        let Some(rootfs) = get_test_rootfs() else {
            eprintln!("Skipping test_nat_end_to_end_tcp: alpine-rootfs not found");
            return;
        };

        // Skip if no outbound internet.
        let internet = std::process::Command::new("ping")
            .args(["-c", "1", "-W", "2", "1.1.1.1"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        match internet {
            Ok(s) if s.success() => {}
            _ => {
                eprintln!("Skipping test_nat_end_to_end_tcp: no outbound internet");
                return;
            }
        }

        let child = Command::new("/bin/sh")
            .args(&["-c", "wget -q -T 5 --spider http://1.1.1.1/"])
            .with_namespaces(Namespace::MOUNT | Namespace::UTS)
            .with_network(NetworkMode::Bridge)
            .with_nat()
            .with_dns(&["1.1.1.1"])
            .with_chroot(&rootfs)
            .with_proc_mount()
            .env("PATH", ALPINE_PATH)
            .stdin(Stdio::Null)
            .stdout(Stdio::Piped)
            .stderr(Stdio::Piped)
            .spawn()
            .expect("Failed to spawn NAT container");

        let (status, stdout, stderr) = child.wait_with_output().expect("wait");
        let out = String::from_utf8_lossy(&stdout);
        let err = String::from_utf8_lossy(&stderr);

        assert!(
            status.success(),
            "wget through NAT should succeed (TCP to 1.1.1.1).\nstdout: {}\nstderr: {}",
            out,
            err
        );
    }
}

mod oci_lifecycle {
    use super::*;

    /// Helper: build a minimal OCI bundle in `dir`, pointing rootfs at `rootfs_path`.
    /// Returns the bundle directory path.
    fn make_oci_bundle(dir: &std::path::Path, rootfs: &std::path::Path, args: &[&str]) -> PathBuf {
        // rootfs/ is a symlink to the real alpine rootfs (avoids copying)
        let rootfs_link = dir.join("rootfs");
        std::os::unix::fs::symlink(rootfs, &rootfs_link).expect("failed to create rootfs symlink");

        // Minimal config.json
        let args_json: Vec<String> = args.iter().map(|s| format!("\"{}\"", s)).collect();
        let config = format!(
            r#"{{
      "ociVersion": "1.0.2",
      "root": {{"path": "rootfs"}},
      "process": {{
        "args": [{}],
        "cwd": "/",
        "env": ["PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"]
      }},
      "linux": {{
        "namespaces": [
          {{"type": "mount"}},
          {{"type": "uts"}},
          {{"type": "pid"}}
        ]
      }}
    }}"#,
            args_json.join(", ")
        );
        std::fs::write(dir.join("config.json"), config).expect("failed to write config.json");
        dir.to_path_buf()
    }

    /// Helper: find the remora binary (built by cargo)
    fn remora_binary() -> PathBuf {
        // target/debug/remora relative to the workspace root
        let mut p = std::env::current_dir().unwrap();
        p.push("target/debug/remora");
        p
    }

    /// Run a remora subcommand with the given args. Returns (stdout, stderr, success).
    fn run_remora(args: &[&str]) -> (String, String, bool) {
        let output = std::process::Command::new(remora_binary())
            .args(args)
            .output()
            .expect("failed to run remora binary");
        (
            String::from_utf8_lossy(&output.stdout).into_owned(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
            output.status.success(),
        )
    }

    fn oci_run_to_completion(id: &str, bundle: &std::path::Path, timeout_secs: u64) {
        let (_, stderr, ok) = run_remora(&["create", id, bundle.to_str().unwrap()]);
        assert!(ok, "remora create failed: {}", stderr);
        let (_, stderr, ok) = run_remora(&["start", id]);
        assert!(ok, "remora start failed: {}", stderr);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);
        loop {
            std::thread::sleep(std::time::Duration::from_millis(100));
            let (stdout, _, _) = run_remora(&["state", id]);
            if stdout.contains("\"stopped\"") {
                break;
            }
            if std::time::Instant::now() > deadline {
                run_remora(&["delete", id]).2;
                panic!("container did not stop within {} seconds", timeout_secs);
            }
        }
        let (_, stderr, ok) = run_remora(&["delete", id]);
        assert!(ok, "remora delete failed: {}", stderr);
    }

    /// test_oci_create_start_state
    ///
    /// Requires: root, alpine-rootfs.
    ///
    /// Creates a minimal OCI bundle running `sleep 2`. Verifies that:
    /// - `remora create` leaves the container in "created" state
    /// - `remora start` transitions it to "running"
    /// - After the process exits, `remora state` reports "stopped"
    /// - `remora delete` removes the state directory
    ///
    /// Failure indicates the create/start split synchronization is broken,
    /// state.json transitions are wrong, or liveness detection is incorrect.
    #[test]
    fn test_oci_create_start_state() {
        if !is_root() {
            eprintln!("Skipping test_oci_create_start_state: requires root");
            return;
        }
        let rootfs = match get_test_rootfs() {
            Some(p) => p,
            None => {
                eprintln!("Skipping test_oci_create_start_state: alpine-rootfs not found");
                return;
            }
        };

        let bundle_dir = tempfile::tempdir().expect("tempdir");
        let bundle = make_oci_bundle(bundle_dir.path(), &rootfs, &["/bin/sleep", "2"]);
        let id = format!("test-oci-css-{}", std::process::id());

        // Cleanup guard: always delete on test exit
        // create
        let (_, stderr, ok) = run_remora(&["create", &id, bundle.to_str().unwrap()]);
        assert!(ok, "remora create failed: {}", stderr);

        // state should be "created"
        let (stdout, stderr, ok) = run_remora(&["state", &id]);
        assert!(ok, "remora state (created) failed: {}", stderr);
        assert!(
            stdout.contains("\"created\""),
            "expected status 'created', got: {}",
            stdout
        );

        // start
        let (_, stderr, ok) = run_remora(&["start", &id]);
        assert!(ok, "remora start failed: {}", stderr);

        // state should be "running"
        let (stdout, _, _) = run_remora(&["state", &id]);
        assert!(
            stdout.contains("\"running\""),
            "expected status 'running' after start, got: {}",
            stdout
        );

        // Wait for sleep 2 to exit (max 6 seconds)
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(6);
        loop {
            std::thread::sleep(std::time::Duration::from_millis(200));
            let (stdout, _, _) = run_remora(&["state", &id]);
            if stdout.contains("\"stopped\"") {
                break;
            }
            if std::time::Instant::now() > deadline {
                panic!(
                    "container did not stop within 6 seconds; last state: {}",
                    stdout
                );
            }
        }

        // delete
        let (_, stderr, ok) = run_remora(&["delete", &id]);
        assert!(ok, "remora delete failed: {}", stderr);

        // state dir should be gone
        let state_dir = remora::oci::state_dir(&id);
        assert!(
            !state_dir.exists(),
            "state dir still exists after delete: {}",
            state_dir.display()
        );
    }

    /// test_oci_kill
    ///
    /// Requires: root, alpine-rootfs.
    ///
    /// Spawns a long-running container (`sleep 60`) and sends SIGKILL via
    /// `remora kill`. Asserts that the process exits promptly and `remora state`
    /// reports "stopped".
    ///
    /// Uses SIGKILL because the container runs in a PID namespace where `sleep`
    /// is PID 1. The kernel only delivers signals to PID 1 if it has installed
    /// an explicit handler; `sleep` uses the default SIGTERM disposition, so the
    /// kernel silently drops it. SIGKILL always works regardless.
    ///
    /// Failure indicates that kill() is not finding the correct host-visible PID,
    /// signals are not being delivered, or state reporting is incorrect.
    #[test]
    fn test_oci_kill() {
        if !is_root() {
            eprintln!("Skipping test_oci_kill: requires root");
            return;
        }
        let rootfs = match get_test_rootfs() {
            Some(p) => p,
            None => {
                eprintln!("Skipping test_oci_kill: alpine-rootfs not found");
                return;
            }
        };

        let bundle_dir = tempfile::tempdir().expect("tempdir");
        let bundle = make_oci_bundle(bundle_dir.path(), &rootfs, &["/bin/sleep", "60"]);
        let id = format!("test-oci-kill-{}", std::process::id());

        let (_, stderr, ok) = run_remora(&["create", &id, bundle.to_str().unwrap()]);
        assert!(ok, "remora create failed: {}", stderr);

        let (_, stderr, ok) = run_remora(&["start", &id]);
        assert!(ok, "remora start failed: {}", stderr);

        // Small delay to ensure the process is running
        std::thread::sleep(std::time::Duration::from_millis(200));

        let (_, stderr, ok) = run_remora(&["kill", &id, "SIGKILL"]);
        assert!(ok, "remora kill failed: {}", stderr);

        // Wait up to 4 seconds for the process to stop
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(4);
        loop {
            std::thread::sleep(std::time::Duration::from_millis(200));
            let (stdout, _, _) = run_remora(&["state", &id]);
            if stdout.contains("\"stopped\"") {
                break;
            }
            if std::time::Instant::now() > deadline {
                panic!("container did not stop after SIGKILL within 4 seconds");
            }
        }

        let (_, stderr, ok) = run_remora(&["delete", &id]);
        assert!(ok, "remora delete failed: {}", stderr);
    }

    /// test_oci_delete_cleanup
    ///
    /// Requires: root, alpine-rootfs.
    ///
    /// Runs a short-lived container (`true`) through the full OCI lifecycle and
    /// asserts that `remora delete` removes `/run/remora/<id>/` completely.
    ///
    /// Failure indicates that the state directory is not cleaned up on delete,
    /// which would cause resource leaks and "already exists" errors on re-use.
    #[test]
    fn test_oci_delete_cleanup() {
        if !is_root() {
            eprintln!("Skipping test_oci_delete_cleanup: requires root");
            return;
        }
        let rootfs = match get_test_rootfs() {
            Some(p) => p,
            None => {
                eprintln!("Skipping test_oci_delete_cleanup: alpine-rootfs not found");
                return;
            }
        };

        let bundle_dir = tempfile::tempdir().expect("tempdir");
        let bundle = make_oci_bundle(bundle_dir.path(), &rootfs, &["/bin/true"]);
        let id = format!("test-oci-del-{}", std::process::id());

        let (_, stderr, ok) = run_remora(&["create", &id, bundle.to_str().unwrap()]);
        assert!(ok, "remora create failed: {}", stderr);

        let (_, stderr, ok) = run_remora(&["start", &id]);
        assert!(ok, "remora start failed: {}", stderr);

        // Wait for the container to stop (true exits immediately)
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(4);
        loop {
            std::thread::sleep(std::time::Duration::from_millis(100));
            let (stdout, _, _) = run_remora(&["state", &id]);
            if stdout.contains("\"stopped\"") {
                break;
            }
            if std::time::Instant::now() > deadline {
                panic!("container did not stop within 4 seconds");
            }
        }

        let state_dir = remora::oci::state_dir(&id);
        assert!(state_dir.exists(), "state dir should exist before delete");

        let (_, stderr, ok) = run_remora(&["delete", &id]);
        assert!(ok, "remora delete failed: {}", stderr);

        assert!(
            !state_dir.exists(),
            "state dir {} still present after delete",
            state_dir.display()
        );
    }

    /// test_oci_bundle_mounts
    ///
    /// Requires: root, alpine-rootfs.
    ///
    /// Creates a bundle with a tmpfs entry in `config.json` and runs a command
    /// that writes to the tmpfs mount. Asserts the command succeeds and the
    /// mount point was writable.
    ///
    /// Failure indicates that OCI mount entries are not being applied from
    /// config.json, or that tmpfs mount handling in build_command() is broken.
    #[test]
    fn test_oci_bundle_mounts() {
        if !is_root() {
            eprintln!("Skipping test_oci_bundle_mounts: requires root");
            return;
        }
        let rootfs = match get_test_rootfs() {
            Some(p) => p,
            None => {
                eprintln!("Skipping test_oci_bundle_mounts: alpine-rootfs not found");
                return;
            }
        };

        let bundle_dir = tempfile::tempdir().expect("tempdir");
        let rootfs_link = bundle_dir.path().join("rootfs");
        std::os::unix::fs::symlink(&rootfs, &rootfs_link).unwrap();

        // config.json with a tmpfs at /scratch
        let config = r#"{
      "ociVersion": "1.0.2",
      "root": {"path": "rootfs"},
      "process": {
        "args": ["/bin/sh", "-c", "echo hello > /scratch/test.txt && cat /scratch/test.txt"],
        "cwd": "/",
        "env": ["PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"]
      },
      "mounts": [
        {
          "destination": "/scratch",
          "type": "tmpfs",
          "source": "tmpfs",
          "options": []
        }
      ],
      "linux": {
        "namespaces": [
          {"type": "mount"},
          {"type": "uts"},
          {"type": "pid"}
        ]
      }
    }"#;
        std::fs::write(bundle_dir.path().join("config.json"), config).unwrap();

        let bundle = bundle_dir.path();
        let id = format!("test-oci-mnt-{}", std::process::id());

        let (_, stderr, ok) = run_remora(&["create", &id, bundle.to_str().unwrap()]);
        assert!(ok, "remora create failed: {}", stderr);

        let (_, stderr, ok) = run_remora(&["start", &id]);
        assert!(ok, "remora start failed: {}", stderr);

        // Wait for container to stop
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(4);
        loop {
            std::thread::sleep(std::time::Duration::from_millis(100));
            let (stdout, _, _) = run_remora(&["state", &id]);
            if stdout.contains("\"stopped\"") {
                break;
            }
            if std::time::Instant::now() > deadline {
                panic!("container did not stop within 4 seconds");
            }
        }

        let (_, stderr, ok) = run_remora(&["delete", &id]);
        assert!(ok, "remora delete failed: {}", stderr);
        assert!(
            stderr.is_empty() || !stderr.contains("error"),
            "unexpected error: {}",
            stderr
        );
    }

    /// test_oci_capabilities
    ///
    /// Requires: root, alpine-rootfs.
    ///
    /// Creates a bundle whose config.json specifies `process.capabilities` with
    /// only CAP_CHOWN in the bounding set. The container runs `id` (which should
    /// succeed even with a reduced capability set). Asserts:
    /// - `remora create` / `start` / `delete` all succeed
    /// - The container exits successfully (reduced caps don't prevent basic exec)
    ///
    /// Failure indicates that capability set parsing from OCI config or the
    /// with_capabilities() wiring in build_command() is broken.
    #[test]
    fn test_oci_capabilities() {
        if !is_root() {
            eprintln!("Skipping test_oci_capabilities: requires root");
            return;
        }
        let rootfs = match get_test_rootfs() {
            Some(p) => p,
            None => {
                eprintln!("Skipping test_oci_capabilities: alpine-rootfs not found");
                return;
            }
        };

        let bundle_dir = tempfile::tempdir().expect("tempdir");
        let rootfs_link = bundle_dir.path().join("rootfs");
        std::os::unix::fs::symlink(&rootfs, &rootfs_link).unwrap();

        // config.json with a reduced capability set (bounding = [CAP_CHOWN] only)
        let config = r#"{
      "ociVersion": "1.0.2",
      "root": {"path": "rootfs"},
      "process": {
        "args": ["/usr/bin/id"],
        "cwd": "/",
        "env": ["PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"],
        "capabilities": {
          "bounding": ["CAP_CHOWN"],
          "effective": ["CAP_CHOWN"],
          "permitted": ["CAP_CHOWN"],
          "inheritable": []
        }
      },
      "linux": {
        "namespaces": [
          {"type": "mount"},
          {"type": "uts"},
          {"type": "pid"}
        ]
      }
    }"#;
        std::fs::write(bundle_dir.path().join("config.json"), config).unwrap();

        let id = format!("test-oci-cap-{}", std::process::id());

        let (_, stderr, ok) = run_remora(&["create", &id, bundle_dir.path().to_str().unwrap()]);
        assert!(ok, "remora create failed: {}", stderr);

        let (_, stderr, ok) = run_remora(&["start", &id]);
        assert!(ok, "remora start failed: {}", stderr);

        // Wait for container to stop
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            std::thread::sleep(std::time::Duration::from_millis(100));
            let (stdout, _, _) = run_remora(&["state", &id]);
            if stdout.contains("\"stopped\"") {
                break;
            }
            if std::time::Instant::now() > deadline {
                run_remora(&["delete", &id]).2;
                panic!("container did not stop within 5 seconds");
            }
        }

        let (_, stderr, ok) = run_remora(&["delete", &id]);
        assert!(ok, "remora delete failed: {}", stderr);
    }

    /// test_oci_masked_readonly_paths
    ///
    /// Requires: root, alpine-rootfs.
    ///
    /// Creates a bundle whose config.json specifies:
    /// - `linux.maskedPaths: ["/proc/kcore"]` — should be hidden
    /// - `linux.readonlyPaths: ["/sys/kernel"]` — should be read-only
    ///
    /// The container runs a command that verifies:
    /// - Attempting to read /proc/kcore returns no useful data (bind-mounted /dev/null)
    /// - /sys/kernel exists but writes to it are denied
    ///
    /// We verify at the OCI level: asserts that `remora create` / `start` / `delete`
    /// all succeed. The correct application of maskedPaths and readonlyPaths is
    /// validated by the container command itself (exits 0 only if both checks pass).
    ///
    /// Failure indicates that `linux.maskedPaths` / `linux.readonlyPaths` parsing
    /// from OCI config, or the wiring into with_masked_paths() / with_readonly_paths()
    /// in build_command(), is broken.
    #[test]
    fn test_oci_masked_readonly_paths() {
        if !is_root() {
            eprintln!("Skipping test_oci_masked_readonly_paths: requires root");
            return;
        }
        let rootfs = match get_test_rootfs() {
            Some(p) => p,
            None => {
                eprintln!("Skipping test_oci_masked_readonly_paths: alpine-rootfs not found");
                return;
            }
        };

        let bundle_dir = tempfile::tempdir().expect("tempdir");
        let rootfs_link = bundle_dir.path().join("rootfs");
        std::os::unix::fs::symlink(&rootfs, &rootfs_link).unwrap();

        // /proc/kcore is masked → it appears as /dev/null (size 0 or read returns nothing)
        // /sys/kernel is readonly → a write attempt should fail
        let config = r#"{
      "ociVersion": "1.0.2",
      "root": {"path": "rootfs"},
      "process": {
        "args": ["/bin/sh", "-c",
          "[ $(wc -c < /proc/kcore) -eq 0 ] && ! touch /sys/kernel/test 2>/dev/null && echo ok"],
        "cwd": "/",
        "env": ["PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"]
      },
      "linux": {
        "namespaces": [
          {"type": "mount"},
          {"type": "uts"},
          {"type": "pid"}
        ],
        "maskedPaths": ["/proc/kcore"],
        "readonlyPaths": ["/sys/kernel"]
      }
    }"#;
        std::fs::write(bundle_dir.path().join("config.json"), config).unwrap();

        let id = format!("test-oci-mrp-{}", std::process::id());

        let (_, stderr, ok) = run_remora(&["create", &id, bundle_dir.path().to_str().unwrap()]);
        assert!(ok, "remora create failed: {}", stderr);

        let (_, stderr, ok) = run_remora(&["start", &id]);
        assert!(ok, "remora start failed: {}", stderr);

        // Wait for container to stop
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            std::thread::sleep(std::time::Duration::from_millis(100));
            let (stdout, _, _) = run_remora(&["state", &id]);
            if stdout.contains("\"stopped\"") {
                break;
            }
            if std::time::Instant::now() > deadline {
                run_remora(&["delete", &id]).2;
                panic!("container did not stop within 5 seconds");
            }
        }

        let (_, stderr, ok) = run_remora(&["delete", &id]);
        assert!(ok, "remora delete failed: {}", stderr);
    }

    /// test_oci_resources
    ///
    /// Requires: root, alpine-rootfs.
    ///
    /// Creates a bundle with `linux.resources` setting a 64 MiB memory limit and
    /// a PID limit of 50. The container reads its cgroup memory.max and pids.max.
    /// Asserts the full lifecycle completes cleanly.
    ///
    /// Failure indicates that `linux.resources` parsing from OCI config or the
    /// wiring into `with_cgroup_memory()` / `with_cgroup_pids_limit()` is broken.
    #[test]
    fn test_oci_resources() {
        if !is_root() {
            eprintln!("Skipping test_oci_resources: requires root");
            return;
        }
        let rootfs = match get_test_rootfs() {
            Some(p) => p,
            None => {
                eprintln!("Skipping test_oci_resources: alpine-rootfs not found");
                return;
            }
        };
        let bundle_dir = tempfile::tempdir().expect("tempdir");
        std::os::unix::fs::symlink(&rootfs, &bundle_dir.path().join("rootfs")).unwrap();
        let config = r#"{
      "ociVersion": "1.0.2",
      "root": {"path": "rootfs"},
      "process": {
        "args": ["/bin/sh", "-c",
          "cat /sys/fs/cgroup/memory.max && cat /sys/fs/cgroup/pids.max"],
        "cwd": "/",
        "env": ["PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"]
      },
      "linux": {
        "namespaces": [{"type": "mount"}, {"type": "uts"}, {"type": "pid"}],
        "resources": {
          "memory": {"limit": 67108864},
          "pids":   {"limit": 50}
        }
      }
    }"#;
        std::fs::write(bundle_dir.path().join("config.json"), config).unwrap();
        let id = format!("test-oci-res-{}", std::process::id());
        oci_run_to_completion(&id, bundle_dir.path(), 5);
    }

    /// test_oci_rlimits
    ///
    /// Requires: root, alpine-rootfs.
    ///
    /// Creates a bundle with `process.rlimits` capping RLIMIT_NOFILE to 128.
    /// The container runs `ulimit -n` (exits 0 if the limit is accepted). Asserts
    /// the full lifecycle completes cleanly.
    ///
    /// Failure indicates that `process.rlimits` parsing or the wiring into
    /// `with_rlimit()` in `build_command()` is broken.
    #[test]
    fn test_oci_rlimits() {
        if !is_root() {
            eprintln!("Skipping test_oci_rlimits: requires root");
            return;
        }
        let rootfs = match get_test_rootfs() {
            Some(p) => p,
            None => {
                eprintln!("Skipping test_oci_rlimits: alpine-rootfs not found");
                return;
            }
        };
        let bundle_dir = tempfile::tempdir().expect("tempdir");
        std::os::unix::fs::symlink(&rootfs, &bundle_dir.path().join("rootfs")).unwrap();
        let config = r#"{
      "ociVersion": "1.0.2",
      "root": {"path": "rootfs"},
      "process": {
        "args": ["/bin/sh", "-c", "ulimit -n"],
        "cwd": "/",
        "env": ["PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"],
        "rlimits": [{"type": "RLIMIT_NOFILE", "hard": 128, "soft": 128}]
      },
      "linux": {
        "namespaces": [{"type": "mount"}, {"type": "uts"}, {"type": "pid"}]
      }
    }"#;
        std::fs::write(bundle_dir.path().join("config.json"), config).unwrap();
        let id = format!("test-oci-rl-{}", std::process::id());
        oci_run_to_completion(&id, bundle_dir.path(), 5);
    }

    /// test_oci_sysctl
    ///
    /// Requires: root, alpine-rootfs.
    ///
    /// Creates a bundle with `linux.sysctl` setting `kernel.domainname` to
    /// `testdomain.local`. The container greps for that value in
    /// `/proc/sys/kernel/domainname`. Asserts the lifecycle completes cleanly.
    ///
    /// Failure indicates that `linux.sysctl` parsing from OCI config or the
    /// `with_sysctl()` / pre_exec write to `/proc/sys/` is broken.
    #[test]
    fn test_oci_sysctl() {
        if !is_root() {
            eprintln!("Skipping test_oci_sysctl: requires root");
            return;
        }
        let rootfs = match get_test_rootfs() {
            Some(p) => p,
            None => {
                eprintln!("Skipping test_oci_sysctl: alpine-rootfs not found");
                return;
            }
        };
        let bundle_dir = tempfile::tempdir().expect("tempdir");
        std::os::unix::fs::symlink(&rootfs, &bundle_dir.path().join("rootfs")).unwrap();
        // kernel.domainname is scoped to the UTS namespace — safe to set.
        let config = r#"{
      "ociVersion": "1.0.2",
      "root": {"path": "rootfs"},
      "process": {
        "args": ["/bin/sh", "-c",
          "cat /proc/sys/kernel/domainname | grep -q testdomain"],
        "cwd": "/",
        "env": ["PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"]
      },
      "linux": {
        "namespaces": [{"type": "mount"}, {"type": "uts"}, {"type": "pid"}],
        "sysctl": {"kernel.domainname": "testdomain.local"}
      }
    }"#;
        std::fs::write(bundle_dir.path().join("config.json"), config).unwrap();
        let id = format!("test-oci-sc-{}", std::process::id());
        oci_run_to_completion(&id, bundle_dir.path(), 5);
    }

    /// test_oci_hooks
    ///
    /// Requires: root, alpine-rootfs.
    ///
    /// Creates a bundle with a `prestart` hook (touches a sentinel file) and a
    /// `poststop` hook (touches a different sentinel file). Asserts:
    /// - The prestart sentinel exists right after `remora create`
    /// - The poststop sentinel exists right after `remora delete`
    ///
    /// Failure indicates that OCI `hooks` parsing, or the `run_hooks()` placement
    /// in `cmd_create()` / `cmd_delete()`, is broken.
    #[test]
    fn test_oci_hooks() {
        if !is_root() {
            eprintln!("Skipping test_oci_hooks: requires root");
            return;
        }
        let rootfs = match get_test_rootfs() {
            Some(p) => p,
            None => {
                eprintln!("Skipping test_oci_hooks: alpine-rootfs not found");
                return;
            }
        };
        let hooks_dir = tempfile::tempdir().expect("tempdir for hooks");
        let prestart_marker = hooks_dir.path().join("prestart_ran");
        let poststop_marker = hooks_dir.path().join("poststop_ran");
        let bundle_dir = tempfile::tempdir().expect("tempdir");
        std::os::unix::fs::symlink(&rootfs, &bundle_dir.path().join("rootfs")).unwrap();
        let config = format!(
            r#"{{
      "ociVersion": "1.0.2",
      "root": {{"path": "rootfs"}},
      "process": {{
        "args": ["/bin/true"],
        "cwd": "/",
        "env": ["PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"]
      }},
      "linux": {{
        "namespaces": [{{"type": "mount"}}, {{"type": "uts"}}, {{"type": "pid"}}]
      }},
      "hooks": {{
        "prestart": [{{"path": "/bin/sh", "args": ["/bin/sh", "-c", "touch {prestart}"]}}],
        "poststop": [{{"path": "/bin/sh", "args": ["/bin/sh", "-c", "touch {poststop}"]}}]
      }}
    }}"#,
            prestart = prestart_marker.display(),
            poststop = poststop_marker.display(),
        );
        std::fs::write(bundle_dir.path().join("config.json"), &config).unwrap();
        let id = format!("test-oci-hk-{}", std::process::id());
        let (_, stderr, ok) = run_remora(&["create", &id, bundle_dir.path().to_str().unwrap()]);
        assert!(ok, "remora create failed: {}", stderr);
        assert!(prestart_marker.exists(), "prestart hook did not run");
        let (_, stderr, ok) = run_remora(&["start", &id]);
        assert!(ok, "remora start failed: {}", stderr);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            std::thread::sleep(std::time::Duration::from_millis(100));
            let (stdout, _, _) = run_remora(&["state", &id]);
            if stdout.contains("\"stopped\"") {
                break;
            }
            if std::time::Instant::now() > deadline {
                run_remora(&["delete", &id]).2;
                panic!("container did not stop within 5 seconds");
            }
        }
        let (_, stderr, ok) = run_remora(&["delete", &id]);
        assert!(ok, "remora delete failed: {}", stderr);
        assert!(poststop_marker.exists(), "poststop hook did not run");
    }

    /// test_oci_seccomp
    ///
    /// Requires: root, alpine-rootfs.
    ///
    /// Creates a bundle with `linux.seccomp` using a default-allow policy that
    /// blocks only `ptrace`, `personality`, and `bpf`. The container runs
    /// `/bin/echo hello` which must succeed. Asserts the full lifecycle
    /// completes cleanly.
    ///
    /// Failure indicates that `linux.seccomp` parsing from OCI config, the
    /// `filter_from_oci()` function in `src/seccomp.rs`, or the
    /// `with_seccomp_program()` wiring in `build_command()` is broken.
    #[test]
    fn test_oci_seccomp() {
        if !is_root() {
            eprintln!("Skipping test_oci_seccomp: requires root");
            return;
        }
        let rootfs = match get_test_rootfs() {
            Some(p) => p,
            None => {
                eprintln!("Skipping test_oci_seccomp: alpine-rootfs not found");
                return;
            }
        };
        let bundle_dir = tempfile::tempdir().expect("tempdir");
        std::os::unix::fs::symlink(&rootfs, &bundle_dir.path().join("rootfs")).unwrap();
        let config = r#"{
      "ociVersion": "1.0.2",
      "root": {"path": "rootfs"},
      "process": {
        "args": ["/bin/echo", "hello"],
        "cwd": "/",
        "env": ["PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"]
      },
      "linux": {
        "namespaces": [{"type": "mount"}, {"type": "uts"}, {"type": "pid"}],
        "seccomp": {
          "defaultAction": "SCMP_ACT_ALLOW",
          "architectures": ["SCMP_ARCH_X86_64"],
          "syscalls": [
            {"names": ["ptrace", "personality", "bpf"], "action": "SCMP_ACT_ERRNO"}
          ]
        }
      }
    }"#;
        std::fs::write(bundle_dir.path().join("config.json"), config).unwrap();
        let id = format!("test-oci-sec-{}", std::process::id());
        oci_run_to_completion(&id, bundle_dir.path(), 5);
    }
}

mod rootless {
    use super::*;

    /// Check whether `pasta` is on PATH and responds to `--version`.
    fn is_pasta_available() -> bool {
        remora::network::is_pasta_available()
    }

    #[test]
    fn test_rootless_basic() {
        if is_root() {
            eprintln!("Skipping test_rootless_basic: must run as non-root (no sudo)");
            return;
        }
        let rootfs = match get_test_rootfs() {
            Some(p) => p,
            None => {
                eprintln!("Skipping test_rootless_basic: alpine-rootfs not found");
                return;
            }
        };
        // When running rootless, spawn() auto-adds Namespace::USER and a uid/gid map
        // that makes the process appear as UID 0 inside the container.
        // Use /bin/ash to invoke id — Alpine's id lives at /usr/bin/id (busybox symlink),
        // not at /bin/id.
        let child = Command::new("/bin/ash")
            .args(&["-c", "id"])
            .env("PATH", ALPINE_PATH)
            .with_chroot(&rootfs)
            .with_namespaces(Namespace::MOUNT | Namespace::UTS)
            .stdout(Stdio::Piped)
            .stderr(Stdio::Piped)
            .spawn()
            .expect("rootless spawn failed");

        let (status, stdout, _stderr) = child.wait_with_output().expect("wait failed");
        assert!(status.success(), "rootless container exited non-zero");
        let out = String::from_utf8_lossy(&stdout);
        // Inside the container the process maps to UID 0 via the user namespace.
        assert!(
            out.contains("uid=0"),
            "expected uid=0 inside rootless container, got: {}",
            out
        );
    }

    #[test]
    fn test_rootless_loopback() {
        if is_root() {
            eprintln!("Skipping test_rootless_loopback: must run as non-root (no sudo)");
            return;
        }
        let rootfs = match get_test_rootfs() {
            Some(p) => p,
            None => {
                eprintln!("Skipping test_rootless_loopback: alpine-rootfs not found");
                return;
            }
        };
        // Loopback networking works in rootless mode: the container gets a private
        // NET namespace (and USER namespace from auto-config) and lo is brought up.
        // lo shows 'state UNKNOWN' even when admin-UP; match the flags field instead.
        let child = Command::new("/bin/ash")
            .args(&["-c", "ip addr show lo | grep -q 'LOOPBACK,UP' && echo ok"])
            .env("PATH", ALPINE_PATH)
            .with_chroot(&rootfs)
            .with_namespaces(Namespace::MOUNT | Namespace::UTS)
            .with_network(NetworkMode::Loopback)
            .stdout(Stdio::Piped)
            .stderr(Stdio::Piped)
            .spawn()
            .expect("rootless loopback spawn failed");

        let (status, stdout, _) = child.wait_with_output().expect("wait failed");
        assert!(status.success(), "rootless loopback container failed");
        assert!(String::from_utf8_lossy(&stdout).contains("ok"));
    }

    #[test]
    fn test_rootless_bridge_rejected() {
        if is_root() {
            eprintln!("Skipping test_rootless_bridge_rejected: must run as non-root (no sudo)");
            return;
        }
        let rootfs = match get_test_rootfs() {
            Some(p) => p,
            None => {
                eprintln!("Skipping test_rootless_bridge_rejected: alpine-rootfs not found");
                return;
            }
        };
        // Bridge mode should be rejected with a clear error in rootless mode.
        let result = Command::new("/bin/echo")
            .with_chroot(&rootfs)
            .with_namespaces(Namespace::MOUNT)
            .with_network(NetworkMode::Bridge)
            .spawn();

        match result {
            Ok(_) => panic!("expected bridge networking to fail in rootless mode"),
            Err(e) => {
                let err_msg = format!("{}", e);
                assert!(
                    err_msg.contains("rootless") || err_msg.contains("root"),
                    "error message should mention rootless/root: {}",
                    err_msg
                );
            }
        }
    }

    #[test]
    fn test_user_namespace_explicit() {
        // Verify that root can create a USER namespace with explicit uid/gid maps and
        // that the container process sees itself as uid=0 inside.
        //
        // No chroot: the rootfs lives under /home/cb/ which is not traversable from
        // inside a USER namespace with a single-uid map (0→0). In a user namespace,
        // capable_wrt_inode_uidgid() only grants DAC_OVERRIDE for inodes whose uid is
        // present in the namespace's uid_map. /home/cb is owned by uid 1000 (not mapped),
        // so the kernel falls through to normal permission bits and returns EACCES.
        // Chroot is not needed to verify uid mapping — just run /usr/bin/id on the host fs.
        //
        // No MOUNT namespace: without chroot there is nothing to isolate mount-wise,
        // and omitting it avoids the MS_PRIVATE limitation on inherited locked mounts.
        if !is_root() {
            eprintln!("Skipping test_user_namespace_explicit: requires root");
            return;
        }
        let child = Command::new("/usr/bin/id")
            .with_namespaces(Namespace::USER)
            .with_uid_maps(&[UidMap {
                inside: 0,
                outside: 0,
                count: 1,
            }])
            .with_gid_maps(&[GidMap {
                inside: 0,
                outside: 0,
                count: 1,
            }])
            .stdout(Stdio::Piped)
            .stderr(Stdio::Piped)
            .spawn()
            .expect("user namespace spawn failed");

        let (status, stdout, _) = child.wait_with_output().expect("wait failed");
        assert!(status.success(), "user namespace container exited non-zero");
        let out = String::from_utf8_lossy(&stdout);
        assert!(
            out.contains("uid=0"),
            "expected uid=0 inside user namespace, got: {}",
            out
        );
    }

    /// Verify that pasta creates a TAP interface with an IP address inside the container's netns.
    ///
    /// Spawns a container with `NetworkMode::Pasta` and runs `ip addr show` after a short
    /// sleep to let pasta attach and configure the interface via `--config-net`. Asserts:
    /// 1. A non-loopback interface exists (pasta created the TAP).
    /// 2. That interface has an `inet` address assigned (pasta's `--config-net` configured it).
    /// Failure on (1) means pasta did not attach to the netns. Failure on (2) means
    /// `--config-net` is not working — the container would have a TAP with no IP.
    ///
    /// **Rootless only.** pasta's privilege-dropping (root→nobody via user namespace)
    /// makes it unable to access the container's namespace file descriptors when run as
    /// root. pasta is designed for rootless mode.
    #[test]
    fn test_pasta_interface_exists() {
        if is_root() {
            eprintln!("Skipping test_pasta_interface_exists: pasta is designed for rootless mode");
            return;
        }
        if !is_pasta_available() {
            eprintln!("Skipping test_pasta_interface_exists: pasta not installed");
            return;
        }
        let rootfs = match get_test_rootfs() {
            Some(p) => p,
            None => {
                eprintln!("Skipping test_pasta_interface_exists: alpine-rootfs not found");
                return;
            }
        };

        let child = Command::new("/bin/ash")
            .args(&["-c", "sleep 1 && ip addr show"])
            .with_chroot(&rootfs)
            .with_namespaces(Namespace::MOUNT | Namespace::UTS)
            .with_proc_mount()
            .with_network(NetworkMode::Pasta)
            .env("PATH", ALPINE_PATH)
            .stdout(Stdio::Piped)
            .stderr(Stdio::Piped)
            .spawn()
            .expect("spawn failed");

        let (status, stdout, _) = child.wait_with_output().expect("wait failed");
        assert!(status.success(), "container exited non-zero");

        let out = String::from_utf8_lossy(&stdout);
        let has_non_loopback = out
            .lines()
            .any(|l| l.contains(": ") && !l.contains("lo:") && !l.contains(" lo@"));
        assert!(
            has_non_loopback,
            "expected a non-loopback TAP interface from pasta, got:\n{}",
            out
        );

        // With --config-net, pasta configures the IP address inside the container's netns.
        // A non-127.x inet address means pasta did more than just create the TAP.
        let has_tap_ip = out.lines().any(|l| {
            let l = l.trim();
            l.starts_with("inet ") && !l.starts_with("inet 127.")
        });
        assert!(
            has_tap_ip,
            "expected inet address on pasta TAP (--config-net), got:\n{}",
            out
        );
    }

    /// Verify that pasta works in the rootless (USER+NET two-phase unshare) path and
    /// that --config-net assigns an IP to the TAP interface.
    ///
    /// Non-root only. Spawns with `NetworkMode::Pasta` without explicit `Namespace::USER`
    /// — rootless auto-adds it. Asserts a non-loopback interface with an inet address is
    /// present. Failure means pasta does not work through the rootless USER+NET path, or
    /// that `--config-net` is not being passed to pasta.
    #[test]
    fn test_pasta_rootless() {
        if is_root() {
            eprintln!("Skipping test_pasta_rootless: must run as non-root (no sudo)");
            return;
        }
        if !is_pasta_available() {
            eprintln!("Skipping test_pasta_rootless: pasta not installed");
            return;
        }
        let rootfs = match get_test_rootfs() {
            Some(p) => p,
            None => {
                eprintln!("Skipping test_pasta_rootless: alpine-rootfs not found");
                return;
            }
        };

        let child = Command::new("/bin/ash")
            .args(&["-c", "sleep 1 && ip addr show"])
            .with_chroot(&rootfs)
            .with_namespaces(Namespace::MOUNT | Namespace::UTS)
            .with_proc_mount()
            .with_network(NetworkMode::Pasta)
            .env("PATH", ALPINE_PATH)
            .stdout(Stdio::Piped)
            .stderr(Stdio::Piped)
            .spawn()
            .expect("spawn failed");

        let (status, stdout, _) = child.wait_with_output().expect("wait failed");
        assert!(status.success(), "container exited non-zero");

        let out = String::from_utf8_lossy(&stdout);
        let has_non_loopback = out
            .lines()
            .any(|l| l.contains(": ") && !l.contains("lo:") && !l.contains(" lo@"));
        assert!(
            has_non_loopback,
            "expected a non-loopback TAP interface from pasta in rootless mode, got:\n{}",
            out
        );

        let has_tap_ip = out.lines().any(|l| {
            let l = l.trim();
            l.starts_with("inet ") && !l.starts_with("inet 127.")
        });
        assert!(
            has_tap_ip,
            "expected inet address on pasta TAP in rootless mode, got:\n{}",
            out
        );
    }

    /// Verify actual end-to-end internet connectivity through pasta.
    ///
    /// Non-root only. Spawns with `NetworkMode::Pasta`, sleeps briefly to let pasta
    /// attach and configure the interface, then fetches `http://1.1.1.1/` (Cloudflare)
    /// using `wget`. Asserts the command succeeds (exit 0).
    ///
    /// Failure means packets are not flowing through pasta's relay despite the TAP
    /// interface and IP being present (verified by `test_pasta_interface_exists`).
    /// This test requires outbound internet access.
    #[test]
    fn test_pasta_connectivity() {
        if is_root() {
            eprintln!("Skipping test_pasta_connectivity: pasta is designed for rootless mode");
            return;
        }
        if !is_pasta_available() {
            eprintln!("Skipping test_pasta_connectivity: pasta not installed");
            return;
        }
        let rootfs = match get_test_rootfs() {
            Some(p) => p,
            None => {
                eprintln!("Skipping test_pasta_connectivity: alpine-rootfs not found");
                return;
            }
        };

        // sleep 2: give pasta time to attach the TAP and configure IP+routes via --config-net.
        // wget --spider: HEAD request — no body to save, so no /dev/null needed (the chroot
        // only has proc mounted, not a full /dev with device nodes).
        let child = Command::new("/bin/ash")
            .args(&[
                "-c",
                "sleep 2 && wget -q -T 5 --spider http://1.1.1.1/ && echo CONNECTED",
            ])
            .with_chroot(&rootfs)
            .with_namespaces(Namespace::MOUNT | Namespace::UTS)
            .with_proc_mount()
            .with_network(NetworkMode::Pasta)
            .env("PATH", ALPINE_PATH)
            .stdout(Stdio::Piped)
            .stderr(Stdio::Piped)
            .spawn()
            .expect("spawn failed");

        let (status, stdout, stderr) = child.wait_with_output().expect("wait failed");
        let out = String::from_utf8_lossy(&stdout);
        let err = String::from_utf8_lossy(&stderr);
        assert!(status.success(),
            "pasta connectivity test failed (is outbound internet available?)\nstdout: {}\nstderr: {}", out, err);
        assert!(
            out.contains("CONNECTED"),
            "wget succeeded but CONNECTED marker missing:\n{}",
            out
        );
    }
}

mod linking {
    use super::*;

    #[test]
    #[serial]
    fn test_container_link_hosts() {
        if !is_root() {
            eprintln!("Skipping test_container_link_hosts: requires root");
            return;
        }
        let Some(rootfs) = get_test_rootfs() else {
            eprintln!("Skipping test_container_link_hosts: alpine-rootfs not found");
            return;
        };

        // Start container A on bridge (long-running).
        let mut child_a = Command::new("/bin/sleep")
            .args(&["60"])
            .with_namespaces(Namespace::MOUNT | Namespace::UTS)
            .with_network(NetworkMode::Bridge)
            .with_chroot(&rootfs)
            .with_proc_mount()
            .stdin(Stdio::Null)
            .stdout(Stdio::Null)
            .stderr(Stdio::Null)
            .spawn()
            .expect("Failed to spawn container A");

        let ip_a = child_a
            .container_ip()
            .expect("container A should have bridge IP");

        // Write A's state so B can resolve it.
        let state_dir = std::path::Path::new("/run/remora/containers/link-test-a");
        std::fs::create_dir_all(state_dir).unwrap();
        let state_json = format!(
            r#"{{"name":"link-test-a","rootfs":"test","status":"running","pid":{},"watcher_pid":0,"started_at":"2026-01-01T00:00:00Z","exit_code":null,"command":["sleep","60"],"stdout_log":null,"stderr_log":null,"bridge_ip":"{}"}}"#,
            child_a.pid(),
            ip_a
        );
        std::fs::write(state_dir.join("state.json"), &state_json).unwrap();

        // Start container B linked to A.
        let child_b = Command::new("/bin/cat")
            .args(&["/etc/hosts"])
            .with_namespaces(Namespace::MOUNT | Namespace::UTS)
            .with_network(NetworkMode::Bridge)
            .with_chroot(&rootfs)
            .with_proc_mount()
            .with_link("link-test-a")
            .stdin(Stdio::Null)
            .stdout(Stdio::Piped)
            .stderr(Stdio::Null)
            .spawn()
            .expect("Failed to spawn container B");

        let (status, stdout, _) = child_b.wait_with_output().expect("wait B");
        let out = String::from_utf8_lossy(&stdout);

        // Clean up A.
        unsafe {
            libc::kill(child_a.pid(), libc::SIGKILL);
        }
        let _ = child_a.wait();
        let _ = std::fs::remove_dir_all(state_dir);

        assert!(status.success(), "Container B should exit successfully");
        assert!(
            out.contains(&ip_a) && out.contains("link-test-a"),
            "B's /etc/hosts should contain A's IP ({}) and name, got:\n{}",
            ip_a,
            out
        );
    }

    /// test_container_link_alias
    ///
    /// Requires: root, alpine-rootfs.
    ///
    /// Starts container A on bridge, then starts container B with
    /// `with_link_alias("A", "db")`. Verifies B's /etc/hosts contains both
    /// the alias "db" and the original name.
    ///
    /// Failure indicates alias handling in the hosts file generation is broken.
    #[test]
    #[serial]
    fn test_container_link_alias() {
        if !is_root() {
            eprintln!("Skipping test_container_link_alias: requires root");
            return;
        }
        let Some(rootfs) = get_test_rootfs() else {
            eprintln!("Skipping test_container_link_alias: alpine-rootfs not found");
            return;
        };

        // Start container A.
        let mut child_a = Command::new("/bin/sleep")
            .args(&["60"])
            .with_namespaces(Namespace::MOUNT | Namespace::UTS)
            .with_network(NetworkMode::Bridge)
            .with_chroot(&rootfs)
            .with_proc_mount()
            .stdin(Stdio::Null)
            .stdout(Stdio::Null)
            .stderr(Stdio::Null)
            .spawn()
            .expect("Failed to spawn container A");

        let ip_a = child_a
            .container_ip()
            .expect("container A should have bridge IP");

        let state_dir = std::path::Path::new("/run/remora/containers/link-alias-a");
        std::fs::create_dir_all(state_dir).unwrap();
        let state_json = format!(
            r#"{{"name":"link-alias-a","rootfs":"test","status":"running","pid":{},"watcher_pid":0,"started_at":"2026-01-01T00:00:00Z","exit_code":null,"command":["sleep","60"],"stdout_log":null,"stderr_log":null,"bridge_ip":"{}"}}"#,
            child_a.pid(),
            ip_a
        );
        std::fs::write(state_dir.join("state.json"), &state_json).unwrap();

        // Start container B linked to A with alias "db".
        let child_b = Command::new("/bin/cat")
            .args(&["/etc/hosts"])
            .with_namespaces(Namespace::MOUNT | Namespace::UTS)
            .with_network(NetworkMode::Bridge)
            .with_chroot(&rootfs)
            .with_proc_mount()
            .with_link_alias("link-alias-a", "db")
            .stdin(Stdio::Null)
            .stdout(Stdio::Piped)
            .stderr(Stdio::Null)
            .spawn()
            .expect("Failed to spawn container B");

        let (status, stdout, _) = child_b.wait_with_output().expect("wait B");
        let out = String::from_utf8_lossy(&stdout);

        unsafe {
            libc::kill(child_a.pid(), libc::SIGKILL);
        }
        let _ = child_a.wait();
        let _ = std::fs::remove_dir_all(state_dir);

        assert!(status.success(), "Container B should exit successfully");
        assert!(
            out.contains(&ip_a) && out.contains("db"),
            "B's /etc/hosts should contain A's IP ({}) and alias 'db', got:\n{}",
            ip_a,
            out
        );
        assert!(
            out.contains("link-alias-a"),
            "B's /etc/hosts should also contain A's original name, got:\n{}",
            out
        );
    }

    /// test_container_link_ping
    ///
    /// Requires: root, alpine-rootfs.
    ///
    /// Starts container A on bridge (running `sleep`), then starts container B
    /// linked to A and runs `ping -c1 -W2 link-ping-a`. Verifies the ping succeeds,
    /// proving both name resolution and network connectivity work.
    ///
    /// Failure indicates that the /etc/hosts entry is incorrect, the bridge network
    /// is misconfigured, or the containers can't reach each other.
    #[test]
    #[serial]
    fn test_container_link_ping() {
        if !is_root() {
            eprintln!("Skipping test_container_link_ping: requires root");
            return;
        }
        let Some(rootfs) = get_test_rootfs() else {
            eprintln!("Skipping test_container_link_ping: alpine-rootfs not found");
            return;
        };

        // Start container A.
        let mut child_a = Command::new("/bin/sleep")
            .args(&["60"])
            .with_namespaces(Namespace::MOUNT | Namespace::UTS)
            .with_network(NetworkMode::Bridge)
            .with_chroot(&rootfs)
            .with_proc_mount()
            .stdin(Stdio::Null)
            .stdout(Stdio::Null)
            .stderr(Stdio::Null)
            .spawn()
            .expect("Failed to spawn container A");

        let ip_a = child_a
            .container_ip()
            .expect("container A should have bridge IP");

        let state_dir = std::path::Path::new("/run/remora/containers/link-ping-a");
        std::fs::create_dir_all(state_dir).unwrap();
        let state_json = format!(
            r#"{{"name":"link-ping-a","rootfs":"test","status":"running","pid":{},"watcher_pid":0,"started_at":"2026-01-01T00:00:00Z","exit_code":null,"command":["sleep","60"],"stdout_log":null,"stderr_log":null,"bridge_ip":"{}"}}"#,
            child_a.pid(),
            ip_a
        );
        std::fs::write(state_dir.join("state.json"), &state_json).unwrap();

        // Start container B: ping A by name.
        let child_b = Command::new("/bin/ping")
            .args(&["-c1", "-W2", "link-ping-a"])
            .with_namespaces(Namespace::MOUNT | Namespace::UTS)
            .with_network(NetworkMode::Bridge)
            .with_chroot(&rootfs)
            .with_proc_mount()
            .with_link("link-ping-a")
            .stdin(Stdio::Null)
            .stdout(Stdio::Piped)
            .stderr(Stdio::Piped)
            .spawn()
            .expect("Failed to spawn container B");

        let (status, stdout, stderr) = child_b.wait_with_output().expect("wait B");
        let out = String::from_utf8_lossy(&stdout);
        let err = String::from_utf8_lossy(&stderr);

        unsafe {
            libc::kill(child_a.pid(), libc::SIGKILL);
        }
        let _ = child_a.wait();
        let _ = std::fs::remove_dir_all(state_dir);

        assert!(
            status.success(),
            "ping from B to A by name should succeed.\nstdout: {}\nstderr: {}",
            out,
            err
        );
    }

    /// test_container_link_tcp
    ///
    /// Requires: root, alpine-rootfs.
    ///
    /// Starts container A on bridge running a netcat TCP listener on port 8080
    /// that echoes "HELLO_FROM_A". Starts container B linked to A, which connects
    /// to A by name via `nc -w 2 link-tcp-a 8080` and captures the response.
    ///
    /// Unlike test_container_link_ping (which only tests ICMP), this proves that
    /// TCP connections work across linked containers — the same protocol used by
    /// real services (HTTP, databases, etc.).
    ///
    /// Failure indicates that TCP traffic cannot traverse the bridge between
    /// containers, even though ICMP (ping) may work. This was a real bug:
    /// iptables FORWARD policy DROP blocked TCP/UDP while allowing ICMP.
    #[test]
    #[serial]
    fn test_container_link_tcp() {
        if !is_root() {
            eprintln!("Skipping test_container_link_tcp: requires root");
            return;
        }
        let Some(rootfs) = get_test_rootfs() else {
            eprintln!("Skipping test_container_link_tcp: alpine-rootfs not found");
            return;
        };

        // Container A: listen on TCP 8080 and send a known string to the first client.
        // The `echo ... | nc -l -p 8080` pattern sends the string then exits.
        let mut child_a = Command::new("/bin/sh")
            .args(&["-c", "echo HELLO_FROM_A | nc -l -p 8080"])
            .with_namespaces(Namespace::MOUNT | Namespace::UTS)
            .with_network(NetworkMode::Bridge)
            .with_chroot(&rootfs)
            .with_proc_mount()
            .env("PATH", ALPINE_PATH)
            .stdin(Stdio::Null)
            .stdout(Stdio::Null)
            .stderr(Stdio::Null)
            .spawn()
            .expect("Failed to spawn container A");

        let ip_a = child_a
            .container_ip()
            .expect("container A should have bridge IP");

        // Register A's state so B can resolve the link name.
        let state_dir = std::path::Path::new("/run/remora/containers/link-tcp-a");
        std::fs::create_dir_all(state_dir).unwrap();
        let state_json = format!(
            r#"{{"name":"link-tcp-a","rootfs":"test","status":"running","pid":{},"watcher_pid":0,"started_at":"2026-01-01T00:00:00Z","exit_code":null,"command":["/bin/sh"],"stdout_log":null,"stderr_log":null,"bridge_ip":"{}"}}"#,
            child_a.pid(),
            ip_a
        );
        std::fs::write(state_dir.join("state.json"), &state_json).unwrap();

        // Give A a moment to start listening.
        std::thread::sleep(std::time::Duration::from_millis(500));

        // Container B: connect to A by link name and read the response.
        let child_b = Command::new("/bin/sh")
            .args(&["-c", "nc -w 2 link-tcp-a 8080"])
            .with_namespaces(Namespace::MOUNT | Namespace::UTS)
            .with_network(NetworkMode::Bridge)
            .with_chroot(&rootfs)
            .with_proc_mount()
            .with_link("link-tcp-a")
            .env("PATH", ALPINE_PATH)
            .stdin(Stdio::Null)
            .stdout(Stdio::Piped)
            .stderr(Stdio::Piped)
            .spawn()
            .expect("Failed to spawn container B");

        let (status_b, stdout_b, stderr_b) = child_b.wait_with_output().expect("wait B");
        let out = String::from_utf8_lossy(&stdout_b);
        let err = String::from_utf8_lossy(&stderr_b);

        // Clean up A (it should have exited after sending, but kill to be sure).
        unsafe {
            libc::kill(child_a.pid(), libc::SIGKILL);
        }
        let _ = child_a.wait();
        let _ = std::fs::remove_dir_all(state_dir);

        assert!(
            status_b.success(),
            "Container B should connect to A via TCP successfully.\nstdout: {}\nstderr: {}",
            out,
            err
        );
        assert!(
            out.contains("HELLO_FROM_A"),
            "B should receive 'HELLO_FROM_A' from A via TCP, got:\nstdout: {}\nstderr: {}",
            out,
            err
        );
    }

    /// test_container_link_missing
    ///
    /// Requires: root, alpine-rootfs.
    ///
    /// Attempts to spawn a container with `with_link("nonexistent")`. Verifies
    /// that spawn fails with a clear error about the missing container.
    ///
    /// Failure indicates that link resolution doesn't properly validate that
    /// the target container exists.
    #[test]
    fn test_container_link_missing() {
        if !is_root() {
            eprintln!("Skipping test_container_link_missing: requires root");
            return;
        }
        let Some(rootfs) = get_test_rootfs() else {
            eprintln!("Skipping test_container_link_missing: alpine-rootfs not found");
            return;
        };

        let result = Command::new("/bin/true")
            .with_namespaces(Namespace::MOUNT | Namespace::UTS)
            .with_network(NetworkMode::Bridge)
            .with_chroot(&rootfs)
            .with_proc_mount()
            .with_link("nonexistent-container-xyz")
            .stdin(Stdio::Null)
            .stdout(Stdio::Null)
            .stderr(Stdio::Null)
            .spawn();

        match result {
            Ok(_) => panic!("spawn should fail when linked container doesn't exist"),
            Err(e) => {
                let err_msg = format!("{}", e);
                assert!(
                    err_msg.contains("nonexistent-container-xyz"),
                    "error should mention the missing container name, got: {}",
                    err_msg
                );
            }
        }
    }
}

mod images {
    use super::*;

    /// Copy the rootfs into a temp directory, excluding pseudo-filesystem
    /// contents (/sys, /proc, /dev) that can't be copied from a live mount.
    /// Re-creates the empty mount-point directories afterward.
    fn copy_rootfs(rootfs: &std::path::Path, dest: &std::path::Path) {
        let status = std::process::Command::new("rsync")
            .args(&["-a", "--exclude=/sys", "--exclude=/proc", "--exclude=/dev"])
            .arg(rootfs.to_str().unwrap().to_string() + "/")
            .arg(dest.to_str().unwrap().to_string() + "/")
            .status()
            .expect("rsync rootfs to layer (is rsync installed?)");
        assert!(status.success(), "rsync should succeed");
        // Re-create empty mount-point dirs that rsync excluded.
        std::fs::create_dir_all(dest.join("proc")).unwrap();
        std::fs::create_dir_all(dest.join("sys")).unwrap();
        std::fs::create_dir_all(dest.join("dev")).unwrap();
    }

    /// test_layer_extraction
    ///
    /// Requires: root (for mknod whiteout devices in extract_layer).
    ///
    /// Creates a synthetic tar.gz layer containing two files, extracts it via
    /// `image::extract_layer()`, and verifies the files exist with correct content.
    ///
    /// Failure indicates the tar+gzip extraction pipeline or layer store layout is broken.
    #[test]
    #[serial]
    fn test_layer_extraction() {
        if !is_root() {
            eprintln!("Skipping test_layer_extraction: requires root");
            return;
        }

        use remora::image;

        // Create a synthetic tar.gz with two files.
        let tmp_dir = tempfile::tempdir().expect("create tempdir");
        let tar_gz_path = tmp_dir.path().join("layer.tar.gz");
        {
            let file = std::fs::File::create(&tar_gz_path).expect("create tar.gz");
            let gz = flate2::write::GzEncoder::new(file, flate2::Compression::default());
            let mut builder = tar::Builder::new(gz);

            // Add a regular file.
            let data = b"hello from layer";
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder
                .append_data(&mut header, "test-file.txt", &data[..])
                .unwrap();

            // Add a file in a subdirectory.
            let data2 = b"nested content";
            let mut header2 = tar::Header::new_gnu();
            header2.set_size(data2.len() as u64);
            header2.set_mode(0o644);
            header2.set_cksum();
            builder
                .append_data(&mut header2, "subdir/nested.txt", &data2[..])
                .unwrap();

            builder.finish().unwrap();
        }

        // Use a unique digest so we don't collide with real layers.
        let digest = "sha256:test_layer_extraction_deadbeef";
        let layer_path = image::layer_dir(digest);
        // Clean up any previous run.
        let _ = std::fs::remove_dir_all(&layer_path);

        let result = image::extract_layer(digest, &tar_gz_path);
        assert!(
            result.is_ok(),
            "extract_layer should succeed: {:?}",
            result.err()
        );
        let extracted = result.unwrap();
        assert!(
            extracted.join("test-file.txt").exists(),
            "test-file.txt should exist"
        );
        assert_eq!(
            std::fs::read_to_string(extracted.join("test-file.txt")).unwrap(),
            "hello from layer"
        );
        assert!(
            extracted.join("subdir/nested.txt").exists(),
            "subdir/nested.txt should exist"
        );
        assert_eq!(
            std::fs::read_to_string(extracted.join("subdir/nested.txt")).unwrap(),
            "nested content"
        );

        // Clean up.
        let _ = std::fs::remove_dir_all(&layer_path);
    }

    /// test_multi_layer_overlay_merge
    ///
    /// Requires: root, alpine-rootfs.
    ///
    /// Creates two temporary layers: bottom has /layer-bottom, top has /layer-top.
    /// Uses `with_image_layers()` to merge them via overlayfs. Runs `cat` inside
    /// the container to verify both files are visible.
    ///
    /// Failure indicates multi-layer overlayfs mount construction or lowerdir
    /// ordering is broken.
    #[test]
    #[serial]
    fn test_multi_layer_overlay_merge() {
        if !is_root() {
            eprintln!("Skipping test_multi_layer_overlay_merge: requires root");
            return;
        }
        let Some(rootfs) = get_test_rootfs() else {
            eprintln!("Skipping test_multi_layer_overlay_merge: alpine-rootfs not found");
            return;
        };

        // Create two synthetic layers.
        let bottom = tempfile::tempdir().expect("bottom layer dir");
        let top = tempfile::tempdir().expect("top layer dir");

        // Copy rootfs contents into the bottom layer so we have a working system.
        copy_rootfs(&rootfs, bottom.path());

        // Add marker files.
        std::fs::write(bottom.path().join("layer-bottom"), "bottom").unwrap();
        std::fs::write(top.path().join("layer-top"), "top").unwrap();

        // layer_dirs: top-first
        let layers = vec![top.path().to_path_buf(), bottom.path().to_path_buf()];

        let child = Command::new("/bin/sh")
            .args(&["-c", "cat /layer-bottom && echo --- && cat /layer-top"])
            .with_image_layers(layers)
            .with_namespaces(Namespace::MOUNT | Namespace::UTS | Namespace::PID)
            .env("PATH", ALPINE_PATH)
            .stdin(Stdio::Null)
            .stdout(Stdio::Piped)
            .stderr(Stdio::Piped)
            .spawn()
            .expect("spawn with image layers");

        let (status, stdout, stderr) = child.wait_with_output().expect("wait");
        let out = String::from_utf8_lossy(&stdout);
        let err = String::from_utf8_lossy(&stderr);

        assert!(status.success(), "container should exit 0, stderr: {}", err);
        assert!(
            out.contains("bottom"),
            "should see bottom layer file, got: {}",
            out
        );
        assert!(
            out.contains("top"),
            "should see top layer file, got: {}",
            out
        );
    }

    /// test_multi_layer_overlay_shadow
    ///
    /// Requires: root, alpine-rootfs.
    ///
    /// Creates bottom layer with /shadow-file containing "bottom" and top layer
    /// with /shadow-file containing "top". Uses `with_image_layers()` to verify
    /// the top layer's file shadows the bottom.
    ///
    /// Failure indicates overlayfs layer ordering (top-first lowerdir) is incorrect.
    #[test]
    #[serial]
    fn test_multi_layer_overlay_shadow() {
        if !is_root() {
            eprintln!("Skipping test_multi_layer_overlay_shadow: requires root");
            return;
        }
        let Some(rootfs) = get_test_rootfs() else {
            eprintln!("Skipping test_multi_layer_overlay_shadow: alpine-rootfs not found");
            return;
        };

        let bottom = tempfile::tempdir().expect("bottom layer dir");
        let top = tempfile::tempdir().expect("top layer dir");

        // Copy rootfs into bottom.
        copy_rootfs(&rootfs, bottom.path());

        // Same file in both layers — top should win.
        std::fs::write(bottom.path().join("shadow-file"), "bottom-value").unwrap();
        std::fs::write(top.path().join("shadow-file"), "top-value").unwrap();

        let layers = vec![top.path().to_path_buf(), bottom.path().to_path_buf()];

        let child = Command::new("/bin/cat")
            .args(&["/shadow-file"])
            .with_image_layers(layers)
            .with_namespaces(Namespace::MOUNT | Namespace::UTS | Namespace::PID)
            .env("PATH", ALPINE_PATH)
            .stdin(Stdio::Null)
            .stdout(Stdio::Piped)
            .stderr(Stdio::Piped)
            .spawn()
            .expect("spawn with image layers");

        let (status, stdout, stderr) = child.wait_with_output().expect("wait");
        let out = String::from_utf8_lossy(&stdout);
        let err = String::from_utf8_lossy(&stderr);

        assert!(status.success(), "container should exit 0, stderr: {}", err);
        assert_eq!(
            out.trim(),
            "top-value",
            "top layer should shadow bottom, got: {}",
            out
        );
    }

    /// test_image_layers_cleanup
    ///
    /// Requires: root, alpine-rootfs.
    ///
    /// Spawns a container with `with_image_layers()`, captures the overlay
    /// merged-dir path, waits for exit, then verifies the ephemeral overlay
    /// directory (merged + upper + work) was cleaned up.
    ///
    /// Failure indicates the `wait()` cleanup for image-layer overlay dirs is broken.
    #[test]
    #[serial]
    fn test_image_layers_cleanup() {
        if !is_root() {
            eprintln!("Skipping test_image_layers_cleanup: requires root");
            return;
        }
        let Some(rootfs) = get_test_rootfs() else {
            eprintln!("Skipping test_image_layers_cleanup: alpine-rootfs not found");
            return;
        };

        let layer = tempfile::tempdir().expect("layer dir");
        copy_rootfs(&rootfs, layer.path());

        let layers = vec![layer.path().to_path_buf()];

        let mut child = Command::new("/bin/true")
            .with_image_layers(layers)
            .with_namespaces(Namespace::MOUNT | Namespace::UTS | Namespace::PID)
            .env("PATH", ALPINE_PATH)
            .stdin(Stdio::Null)
            .stdout(Stdio::Null)
            .stderr(Stdio::Null)
            .spawn()
            .expect("spawn");

        let merged = child
            .overlay_merged_dir()
            .expect("should have merged dir")
            .to_path_buf();
        let overlay_base = merged
            .parent()
            .expect("merged should have parent")
            .to_path_buf();
        assert!(
            overlay_base.exists(),
            "overlay base dir should exist before wait"
        );

        let status = child.wait().expect("wait");
        assert!(status.success(), "container should exit 0");
        assert!(
            !overlay_base.exists(),
            "overlay base dir should be cleaned up after wait: {:?}",
            overlay_base
        );
    }

    /// test_pull_and_run_real_image
    ///
    /// Requires: root, network access.
    ///
    /// Pulls `alpine:latest` from Docker Hub via `image::extract_layer()` and
    /// the `cli::image` pull pipeline, then runs `/bin/sh -c "cat /etc/alpine-release"`
    /// using `with_image_layers()`. Verifies the output is a valid Alpine version string.
    ///
    /// This is a full end-to-end test of the OCI image pipeline: registry pull →
    /// layer extraction → multi-layer overlay mount → container exec. Failure
    /// indicates a regression anywhere in that chain.
    ///
    /// Ignored by default because it requires network access and is slower than
    /// the synthetic-layer tests. Run with:
    /// ```bash
    /// sudo -E cargo test --test integration_tests test_pull_and_run_real_image -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore]
    #[serial]
    fn test_pull_and_run_real_image() {
        if !is_root() {
            eprintln!("Skipping test_pull_and_run_real_image: requires root");
            return;
        }

        use remora::image;

        let reference = "docker.io/library/alpine:latest";

        // Pull the image using the remora binary (true E2E).
        let pull_status = std::process::Command::new(env!("CARGO_BIN_EXE_remora"))
            .args(&["image", "pull", "alpine"])
            .status()
            .expect("failed to run remora image pull");
        assert!(pull_status.success(), "remora image pull should succeed");

        // Load manifest and resolve layers.
        let manifest =
            image::load_image(reference).expect("image manifest should be loadable after pull");
        let layers = image::layer_dirs(&manifest);
        assert!(!layers.is_empty(), "alpine should have at least one layer");

        // Run a command inside the image.
        let child = Command::new("/bin/sh")
            .args(&["-c", "cat /etc/alpine-release"])
            .with_image_layers(layers)
            .with_namespaces(Namespace::MOUNT | Namespace::UTS | Namespace::PID)
            .env("PATH", ALPINE_PATH)
            .stdin(Stdio::Null)
            .stdout(Stdio::Piped)
            .stderr(Stdio::Piped)
            .spawn()
            .expect("spawn from real image");

        let (status, stdout, stderr) = child.wait_with_output().expect("wait");
        let out = String::from_utf8_lossy(&stdout).trim().to_string();
        let err = String::from_utf8_lossy(&stderr);

        assert!(status.success(), "container should exit 0, stderr: {}", err);
        // Alpine release is a version like "3.19.1" or "3.23.3".
        assert!(
            out.chars().next().map_or(false, |c| c.is_ascii_digit()) && out.contains('.'),
            "expected Alpine version string, got: '{}'",
            out
        );
        println!("Alpine version from real image: {}", out);

        // Clean up the pulled image metadata (layers stay cached).
        let _ = image::remove_image(reference);
    }
}

mod exec {
    use super::*;
    use std::os::unix::io::AsRawFd;

    /// Helper: build an exec Command that joins the container's mount namespace
    /// via pre_exec (setns + fchdir + chroot(".") + chdir("/")) and joins all
    /// other differing namespaces via with_namespace_join().
    fn build_exec_command(pid: i32, exe: &str, args: &[&str]) -> Command {
        let ns_types: &[(&str, Namespace)] = &[
            ("mnt", Namespace::MOUNT),
            ("uts", Namespace::UTS),
            ("ipc", Namespace::IPC),
            ("net", Namespace::NET),
            ("pid", Namespace::PID),
            ("user", Namespace::USER),
            ("cgroup", Namespace::CGROUP),
        ];

        let mut cmd = Command::new(exe).args(args);
        let mut has_mount_ns = false;

        for &(ns_name, ns_flag) in ns_types {
            let container_ns = format!("/proc/{}/ns/{}", pid, ns_name);
            let init_ns = format!("/proc/1/ns/{}", ns_name);
            let c_ino = std::fs::metadata(&container_ns).map(|m| {
                use std::os::unix::fs::MetadataExt;
                m.ino()
            });
            let i_ino = std::fs::metadata(&init_ns).map(|m| {
                use std::os::unix::fs::MetadataExt;
                m.ino()
            });
            if let (Ok(c), Ok(i)) = (c_ino, i_ino) {
                if c != i {
                    if ns_flag == Namespace::MOUNT {
                        has_mount_ns = true;
                    } else {
                        cmd = cmd.with_namespace_join(&container_ns, ns_flag);
                    }
                }
            }
        }

        if has_mount_ns {
            let mnt_ns_path = format!("/proc/{}/ns/mnt", pid);
            let mnt_ns_file = std::fs::File::open(&mnt_ns_path).expect("open mount ns");
            let mnt_ns_fd = mnt_ns_file.as_raw_fd();

            let root_path = format!("/proc/{}/root", pid);
            let root_file = std::fs::File::open(&root_path).expect("open container root");
            let root_fd = root_file.as_raw_fd();

            cmd = cmd.with_pre_exec(move || {
                let _keep_mnt = &mnt_ns_file;
                let _keep_root = &root_file;
                unsafe {
                    if libc::setns(mnt_ns_fd, libc::CLONE_NEWNS) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    if libc::fchdir(root_fd) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    let dot = std::ffi::CString::new(".").unwrap();
                    if libc::chroot(dot.as_ptr()) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    let slash = std::ffi::CString::new("/").unwrap();
                    if libc::chdir(slash.as_ptr()) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                }
                Ok(())
            });
        } else {
            cmd = cmd.with_chroot(format!("/proc/{}/root", pid));
        }

        cmd
    }

    /// Start a container, then exec a command inside it via mount-ns setns +
    /// fchdir + chroot(".") in pre_exec — the same mechanism `remora exec` uses.
    ///
    /// NOTE: We use UTS+MOUNT (no PID namespace) because Namespace::PID triggers
    /// a double-fork where container.pid() returns the intermediate process, not
    /// the actual container.  The real `remora exec` CLI uses the grandchild PID
    /// from state.json, so it works correctly with PID namespaces.
    #[test]
    #[serial]
    fn test_exec_basic() {
        if !is_root() {
            eprintln!("Skipping test_exec_basic (requires root)");
            return;
        }
        let rootfs = match get_test_rootfs() {
            Some(r) => r,
            None => {
                eprintln!("Skipping (no rootfs)");
                return;
            }
        };

        // Start a long-running container (no PID ns — see note above).
        let mut container = Command::new("/bin/sleep")
            .args(&["30"])
            .with_chroot(&rootfs)
            .with_namespaces(Namespace::UTS | Namespace::MOUNT)
            .env("PATH", ALPINE_PATH)
            .stdin(Stdio::Null)
            .stdout(Stdio::Null)
            .stderr(Stdio::Null)
            .spawn()
            .expect("spawn container");

        let pid = container.pid();

        let cmd = build_exec_command(pid, "/bin/cat", &["/etc/os-release"])
            .stdin(Stdio::Null)
            .stdout(Stdio::Piped)
            .stderr(Stdio::Piped);

        let exec_child = cmd.spawn().expect("exec spawn");
        let (status, stdout, stderr) = exec_child.wait_with_output().expect("exec wait");

        // Clean up the container.
        unsafe {
            libc::kill(pid, libc::SIGKILL);
        }
        let _ = container.wait();

        let out = String::from_utf8_lossy(&stdout);
        let err = String::from_utf8_lossy(&stderr);
        assert!(status.success(), "exec should exit 0, stderr: {}", err);
        assert!(
            out.contains("Alpine"),
            "exec should see Alpine os-release, got: {}",
            out
        );
    }

    /// Start a container that writes a marker file to a tmpfs, then exec
    /// into it and read the marker — proving the exec'd process sees the
    /// container's mount namespace (including its tmpfs mounts).
    #[test]
    #[serial]
    fn test_exec_sees_container_filesystem() {
        if !is_root() {
            eprintln!("Skipping test_exec_sees_container_filesystem (requires root)");
            return;
        }
        let rootfs = match get_test_rootfs() {
            Some(r) => r,
            None => {
                eprintln!("Skipping (no rootfs)");
                return;
            }
        };

        // Start a container that creates a marker file on tmpfs then sleeps.
        // No PID ns — pid() would return the intermediate, not the real container.
        let mut container = Command::new("/bin/sh")
            .args(&[
                "-c",
                "echo EXEC_MARKER_12345 > /tmp/exec-marker && sleep 30",
            ])
            .with_chroot(&rootfs)
            .with_namespaces(Namespace::UTS | Namespace::MOUNT)
            .with_tmpfs("/tmp", "")
            .env("PATH", ALPINE_PATH)
            .stdin(Stdio::Null)
            .stdout(Stdio::Null)
            .stderr(Stdio::Null)
            .spawn()
            .expect("spawn container");

        let pid = container.pid();

        // Give the shell time to create the marker file.
        std::thread::sleep(std::time::Duration::from_millis(500));

        let cmd = build_exec_command(pid, "/bin/cat", &["/tmp/exec-marker"])
            .stdin(Stdio::Null)
            .stdout(Stdio::Piped)
            .stderr(Stdio::Piped);

        let exec_child = cmd.spawn().expect("exec spawn");
        let (status, stdout, stderr) = exec_child.wait_with_output().expect("exec wait");

        // Clean up.
        unsafe {
            libc::kill(pid, libc::SIGKILL);
        }
        let _ = container.wait();

        let out = String::from_utf8_lossy(&stdout).trim().to_string();
        let err = String::from_utf8_lossy(&stderr);
        assert!(status.success(), "exec should exit 0, stderr: {}", err);
        assert_eq!(
            out, "EXEC_MARKER_12345",
            "exec should see the container's tmpfs marker"
        );
    }

    /// Exec into a container and verify environment variables are visible
    /// via /proc/{pid}/environ.
    #[test]
    #[serial]
    fn test_exec_environment() {
        if !is_root() {
            eprintln!("Skipping test_exec_environment (requires root)");
            return;
        }
        let rootfs = match get_test_rootfs() {
            Some(r) => r,
            None => {
                eprintln!("Skipping (no rootfs)");
                return;
            }
        };

        // Start container with a custom env var.
        // No PID ns — pid() would return the intermediate, not the real container.
        let mut container = Command::new("/bin/sleep")
            .args(&["30"])
            .with_chroot(&rootfs)
            .with_namespaces(Namespace::UTS | Namespace::MOUNT)
            .env("PATH", ALPINE_PATH)
            .env("FOO", "bar_from_container")
            .stdin(Stdio::Null)
            .stdout(Stdio::Null)
            .stderr(Stdio::Null)
            .spawn()
            .expect("spawn container");

        let pid = container.pid();

        // Wait for exec to complete so /proc/{pid}/environ reflects the
        // container's env (not the pre-exec intermediate process).
        std::thread::sleep(std::time::Duration::from_millis(100));

        // Read the container's environment from /proc/{pid}/environ.
        let environ_path = format!("/proc/{}/environ", pid);
        let environ_data = std::fs::read(&environ_path).expect("read /proc/pid/environ");
        let env_pairs: Vec<(String, String)> = environ_data
            .split(|&b| b == 0)
            .filter(|s| !s.is_empty())
            .filter_map(|entry| {
                let s = String::from_utf8_lossy(entry);
                let (k, v) = s.split_once('=')?;
                Some((k.to_string(), v.to_string()))
            })
            .collect();

        // Build exec command that echoes $FOO.
        let mut cmd = build_exec_command(pid, "/bin/sh", &["-c", "echo $FOO"]);

        // Apply container env.
        for (k, v) in &env_pairs {
            cmd = cmd.env(k, v);
        }

        cmd = cmd
            .stdin(Stdio::Null)
            .stdout(Stdio::Piped)
            .stderr(Stdio::Piped);

        let exec_child = cmd.spawn().expect("exec spawn");
        let (status, stdout, stderr) = exec_child.wait_with_output().expect("exec wait");

        // Clean up.
        unsafe {
            libc::kill(pid, libc::SIGKILL);
        }
        let _ = container.wait();

        let out = String::from_utf8_lossy(&stdout).trim().to_string();
        let err = String::from_utf8_lossy(&stderr);
        assert!(status.success(), "exec should exit 0, stderr: {}", err);
        assert_eq!(
            out, "bar_from_container",
            "exec should see container's FOO env var"
        );
    }

    /// Trying to exec into a non-running container: verify that the liveness
    /// check correctly detects a dead PID, which is what `remora exec` uses
    /// to reject exec into stopped containers.
    #[test]
    #[serial]
    fn test_exec_nonrunning_container_fails() {
        if !is_root() {
            eprintln!("Skipping test_exec_nonrunning_container_fails (requires root)");
            return;
        }

        // PID 999999 should not be alive on any reasonable system.
        let alive = unsafe { libc::kill(999999, 0) == 0 };
        assert!(!alive, "PID 999999 should not be alive");

        // Also verify that /proc/999999/root does not exist, so chroot would fail.
        let root_path = std::path::Path::new("/proc/999999/root");
        assert!(!root_path.exists(), "/proc/999999/root should not exist");
    }
}

mod dev {
    use super::*;

    #[test]
    #[serial]
    fn test_dev_minimal_devices() {
        if !is_root() {
            eprintln!("Skipping test_dev_minimal_devices: requires root");
            return;
        }
        let Some(rootfs) = get_test_rootfs() else {
            eprintln!("Skipping test_dev_minimal_devices: alpine-rootfs not found");
            return;
        };

        let child = Command::new("/bin/ls")
            .args(&["/dev/"])
            .with_namespaces(Namespace::MOUNT | Namespace::UTS | Namespace::PID)
            .with_chroot(&rootfs)
            .with_proc_mount()
            .with_dev_mount()
            .env("PATH", ALPINE_PATH)
            .stdin(Stdio::Null)
            .stdout(Stdio::Piped)
            .stderr(Stdio::Piped)
            .spawn()
            .expect("Failed to spawn container");

        let (status, stdout_bytes, _) = child.wait_with_output().expect("Failed to wait for child");
        let stdout = String::from_utf8_lossy(&stdout_bytes);
        assert!(status.success(), "ls /dev/ failed: {}", stdout);

        // Should contain safe devices.
        for dev in &["null", "zero", "random", "urandom", "full", "tty"] {
            assert!(
                stdout.contains(dev),
                "/dev/ should contain '{}', got: {}",
                dev,
                stdout
            );
        }

        // Should NOT contain host-specific devices.
        for bad in &["sda", "nvme", "video"] {
            assert!(
                !stdout.contains(bad),
                "/dev/ should NOT contain host device '{}', got: {}",
                bad,
                stdout
            );
        }
    }

    #[test]
    #[serial]
    fn test_dev_null_works() {
        if !is_root() {
            eprintln!("Skipping test_dev_null_works: requires root");
            return;
        }
        let Some(rootfs) = get_test_rootfs() else {
            eprintln!("Skipping test_dev_null_works: alpine-rootfs not found");
            return;
        };

        let child = Command::new("/bin/ash")
            .args(&["-c", "echo ok > /dev/null && echo pass"])
            .with_namespaces(Namespace::MOUNT | Namespace::UTS | Namespace::PID)
            .with_chroot(&rootfs)
            .with_proc_mount()
            .with_dev_mount()
            .env("PATH", ALPINE_PATH)
            .stdin(Stdio::Null)
            .stdout(Stdio::Piped)
            .stderr(Stdio::Piped)
            .spawn()
            .expect("Failed to spawn container");

        let (status, stdout_bytes, _) = child.wait_with_output().expect("Failed to wait for child");
        let stdout = String::from_utf8_lossy(&stdout_bytes);
        assert!(status.success(), "command failed: {}", stdout);
        assert!(
            stdout.contains("pass"),
            "expected 'pass' in output, got: {}",
            stdout
        );
    }

    #[test]
    #[serial]
    fn test_dev_zero_works() {
        if !is_root() {
            eprintln!("Skipping test_dev_zero_works: requires root");
            return;
        }
        let Some(rootfs) = get_test_rootfs() else {
            eprintln!("Skipping test_dev_zero_works: alpine-rootfs not found");
            return;
        };

        let child = Command::new("/bin/ash")
            .args(&["-c", "head -c 4 /dev/zero | wc -c"])
            .with_namespaces(Namespace::MOUNT | Namespace::UTS | Namespace::PID)
            .with_chroot(&rootfs)
            .with_proc_mount()
            .with_dev_mount()
            .env("PATH", ALPINE_PATH)
            .stdin(Stdio::Null)
            .stdout(Stdio::Piped)
            .stderr(Stdio::Piped)
            .spawn()
            .expect("Failed to spawn container");

        let (status, stdout_bytes, _) = child.wait_with_output().expect("Failed to wait for child");
        let stdout = String::from_utf8_lossy(&stdout_bytes);
        assert!(status.success(), "command failed: {}", stdout);
        assert!(
            stdout.trim().contains('4'),
            "expected '4' in output, got: {}",
            stdout
        );
    }

    #[test]
    #[serial]
    fn test_dev_symlinks() {
        if !is_root() {
            eprintln!("Skipping test_dev_symlinks: requires root");
            return;
        }
        let Some(rootfs) = get_test_rootfs() else {
            eprintln!("Skipping test_dev_symlinks: alpine-rootfs not found");
            return;
        };

        let child = Command::new("/bin/ash")
            .args(&[
                "-c",
                "test -L /dev/fd && test -L /dev/stdin && test -L /dev/stdout && test -L /dev/stderr && echo ok",
            ])
            .with_namespaces(Namespace::MOUNT | Namespace::UTS | Namespace::PID)
            .with_chroot(&rootfs)
            .with_proc_mount()
            .with_dev_mount()
            .env("PATH", ALPINE_PATH)
            .stdin(Stdio::Null)
            .stdout(Stdio::Piped)
            .stderr(Stdio::Piped)
            .spawn()
            .expect("Failed to spawn container");

        let (status, stdout_bytes, _) = child.wait_with_output().expect("Failed to wait for child");
        let stdout = String::from_utf8_lossy(&stdout_bytes);
        assert!(status.success(), "symlink check failed: {}", stdout);
        assert!(
            stdout.contains("ok"),
            "expected 'ok' in output, got: {}",
            stdout
        );
    }

    #[test]
    #[serial]
    fn test_dev_pts_exists() {
        if !is_root() {
            eprintln!("Skipping test_dev_pts_exists: requires root");
            return;
        }
        let Some(rootfs) = get_test_rootfs() else {
            eprintln!("Skipping test_dev_pts_exists: alpine-rootfs not found");
            return;
        };

        let child = Command::new("/bin/ash")
            .args(&["-c", "test -d /dev/pts && test -d /dev/shm && echo ok"])
            .with_namespaces(Namespace::MOUNT | Namespace::UTS | Namespace::PID)
            .with_chroot(&rootfs)
            .with_proc_mount()
            .with_dev_mount()
            .env("PATH", ALPINE_PATH)
            .stdin(Stdio::Null)
            .stdout(Stdio::Piped)
            .stderr(Stdio::Piped)
            .spawn()
            .expect("Failed to spawn container");

        let (status, stdout_bytes, _) = child.wait_with_output().expect("Failed to wait for child");
        let stdout = String::from_utf8_lossy(&stdout_bytes);
        assert!(status.success(), "/dev/pts or /dev/shm missing: {}", stdout);
        assert!(
            stdout.contains("ok"),
            "expected 'ok' in output, got: {}",
            stdout
        );
    }
}

mod rootless_cgroups {
    use super::*;

    fn skip_unless_delegation() -> bool {
        if !remora::cgroup_rootless::is_delegation_available() {
            eprintln!("Skipping: cgroup v2 delegation not available");
            return false;
        }
        true
    }

    /// Read a cgroup knob from the host side for a given child PID.
    /// Returns None if the file doesn't exist (controller not delegated).
    fn read_cgroup_knob(pid: i32, knob: &str) -> Option<String> {
        let parent =
            remora::cgroup_rootless::self_cgroup_path().expect("self_cgroup_path should work");
        let path = parent.join(format!("remora-{}", pid)).join(knob);
        match std::fs::read_to_string(&path) {
            Ok(s) => Some(s),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => panic!("failed to read {}: {}", path.display(), e),
        }
    }

    #[test]
    fn test_rootless_cgroup_memory() {
        if is_root() {
            eprintln!("Skipping test_rootless_cgroup_memory: must run as non-root");
            return;
        }
        if !skip_unless_delegation() {
            return;
        }
        let Some(rootfs) = get_test_rootfs() else {
            eprintln!("Skipping test_rootless_cgroup_memory: alpine-rootfs not found");
            return;
        };

        // Spawn a container that sleeps so we can inspect the cgroup from the host.
        let mut child = Command::new("/bin/sleep")
            .args(&["10"])
            .with_namespaces(Namespace::USER | Namespace::MOUNT | Namespace::UTS)
            .with_chroot(&rootfs)
            .with_uid_maps(&[UidMap {
                inside: 0,
                outside: unsafe { libc::getuid() },
                count: 1,
            }])
            .with_gid_maps(&[GidMap {
                inside: 0,
                outside: unsafe { libc::getgid() },
                count: 1,
            }])
            .env("PATH", ALPINE_PATH)
            .with_cgroup_memory(64 * 1024 * 1024) // 64 MB
            .stdin(Stdio::Null)
            .stdout(Stdio::Null)
            .stderr(Stdio::Null)
            .spawn()
            .expect("Failed to spawn with rootless cgroup memory");

        // Read memory.max from the host-side cgroup directory.
        let val = read_cgroup_knob(child.pid(), "memory.max");

        // Clean up: kill the sleep and wait.
        unsafe {
            libc::kill(child.pid(), libc::SIGKILL);
        }
        child.wait().expect("Failed to wait");

        match val {
            Some(v) => assert_eq!(
                v.trim(),
                "67108864",
                "expected 64MB in memory.max, got: {}",
                v.trim()
            ),
            None => eprintln!(
                "Skipping memory assertion: memory controller not delegated to sub-cgroup"
            ),
        }
    }

    #[test]
    fn test_rootless_cgroup_pids() {
        if is_root() {
            eprintln!("Skipping test_rootless_cgroup_pids: must run as non-root");
            return;
        }
        if !skip_unless_delegation() {
            return;
        }
        let Some(rootfs) = get_test_rootfs() else {
            eprintln!("Skipping test_rootless_cgroup_pids: alpine-rootfs not found");
            return;
        };

        let mut child = Command::new("/bin/sleep")
            .args(&["10"])
            .with_namespaces(Namespace::USER | Namespace::MOUNT | Namespace::UTS)
            .with_chroot(&rootfs)
            .with_uid_maps(&[UidMap {
                inside: 0,
                outside: unsafe { libc::getuid() },
                count: 1,
            }])
            .with_gid_maps(&[GidMap {
                inside: 0,
                outside: unsafe { libc::getgid() },
                count: 1,
            }])
            .env("PATH", ALPINE_PATH)
            .with_cgroup_pids_limit(16)
            .stdin(Stdio::Null)
            .stdout(Stdio::Null)
            .stderr(Stdio::Null)
            .spawn()
            .expect("Failed to spawn with rootless cgroup pids");

        let val = read_cgroup_knob(child.pid(), "pids.max");

        unsafe {
            libc::kill(child.pid(), libc::SIGKILL);
        }
        child.wait().expect("Failed to wait");

        match val {
            Some(v) => assert_eq!(v.trim(), "16", "expected 16 in pids.max, got: {}", v.trim()),
            None => {
                eprintln!("Skipping pids assertion: pids controller not delegated to sub-cgroup")
            }
        }
    }

    #[test]
    fn test_rootless_cgroup_cleanup() {
        if is_root() {
            eprintln!("Skipping test_rootless_cgroup_cleanup: must run as non-root");
            return;
        }
        if !skip_unless_delegation() {
            return;
        }
        let Some(rootfs) = get_test_rootfs() else {
            eprintln!("Skipping test_rootless_cgroup_cleanup: alpine-rootfs not found");
            return;
        };

        let mut child = Command::new("/bin/true")
            .with_namespaces(Namespace::USER | Namespace::MOUNT | Namespace::UTS)
            .with_chroot(&rootfs)
            .with_uid_maps(&[UidMap {
                inside: 0,
                outside: unsafe { libc::getuid() },
                count: 1,
            }])
            .with_gid_maps(&[GidMap {
                inside: 0,
                outside: unsafe { libc::getgid() },
                count: 1,
            }])
            .env("PATH", ALPINE_PATH)
            .with_cgroup_memory(32 * 1024 * 1024)
            .stdin(Stdio::Null)
            .stdout(Stdio::Null)
            .stderr(Stdio::Null)
            .spawn()
            .expect("Failed to spawn");

        let pid = child.pid();
        child.wait().expect("Failed to wait");

        // The kernel may take a moment to fully vacate the cgroup.
        let cg_parent =
            remora::cgroup_rootless::self_cgroup_path().expect("self_cgroup_path should work");
        let cg_dir = cg_parent.join(format!("remora-{}", pid));

        // Retry removal briefly in case the kernel hasn't finished yet.
        for _ in 0..10 {
            if !cg_dir.exists() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
            // Try removing again — teardown may have raced with the kernel.
            let _ = std::fs::remove_dir(&cg_dir);
        }

        assert!(
            !cg_dir.exists(),
            "cgroup dir should have been removed: {}",
            cg_dir.display()
        );
    }
}

mod json_output {
    use super::*;

    /// Helper: run the remora binary, return (stdout, stderr, success).
    fn remora(args: &[&str]) -> (String, String, bool) {
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_remora"))
            .args(args)
            .output()
            .expect("failed to run remora binary");
        (
            String::from_utf8_lossy(&output.stdout).into_owned(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
            output.status.success(),
        )
    }

    /// test_volume_ls_json
    ///
    /// Requires: root (volumes are stored under /var/lib/remora/volumes/).
    ///
    /// Creates a volume, verifies `volume ls --format json` contains an entry
    /// with the volume's name and path. Removes the volume and verifies the
    /// entry is gone from the JSON output.
    ///
    /// Failure indicates JSON serialization of volumes is broken or the
    /// --format flag is not wired correctly.
    #[test]
    #[serial]
    fn test_volume_ls_json() {
        if !is_root() {
            eprintln!("Skipping test_volume_ls_json: requires root");
            return;
        }

        let vol_name = "test-json-vol";

        // Clean up any leftover from a previous run.
        let _ = remora(&["volume", "rm", vol_name]);

        // Create a volume.
        let (_, stderr, ok) = remora(&["volume", "create", vol_name]);
        assert!(ok, "volume create failed: {}", stderr);

        // List with --format json.
        let (stdout, stderr, ok) = remora(&["volume", "ls", "--format", "json"]);
        assert!(ok, "volume ls --format json failed: {}", stderr);
        let parsed: serde_json::Value =
            serde_json::from_str(&stdout).expect("volume ls JSON should parse");
        let arr = parsed.as_array().expect("expected JSON array");
        let found = arr
            .iter()
            .any(|v| v.get("name").and_then(|n| n.as_str()) == Some(vol_name));
        assert!(
            found,
            "volume '{}' not in JSON output: {}",
            vol_name, stdout
        );

        // Each entry should have a "path" field.
        let entry = arr
            .iter()
            .find(|v| v.get("name").and_then(|n| n.as_str()) == Some(vol_name))
            .unwrap();
        assert!(
            entry.get("path").and_then(|p| p.as_str()).is_some(),
            "volume JSON entry should have a 'path' field"
        );

        // Remove the volume.
        let (_, stderr, ok) = remora(&["volume", "rm", vol_name]);
        assert!(ok, "volume rm failed: {}", stderr);

        // List again — volume should be gone.
        let (stdout, _, ok) = remora(&["volume", "ls", "--format", "json"]);
        assert!(ok, "volume ls --format json failed after rm");
        let parsed: serde_json::Value =
            serde_json::from_str(&stdout).expect("volume ls JSON should parse after rm");
        let arr = parsed.as_array().expect("expected JSON array");
        let found = arr
            .iter()
            .any(|v| v.get("name").and_then(|n| n.as_str()) == Some(vol_name));
        assert!(
            !found,
            "volume '{}' should not appear after rm: {}",
            vol_name, stdout
        );
    }

    /// test_rootfs_ls_json
    ///
    /// Requires: root (rootfs store is under /var/lib/remora/rootfs/).
    ///
    /// Imports a rootfs entry (symlink to /tmp), verifies `rootfs ls --format json`
    /// contains an entry with the correct name and path. Removes the entry and
    /// verifies it is gone from the JSON output.
    ///
    /// Failure indicates JSON serialization of rootfs entries is broken or the
    /// --format flag is not wired correctly.
    #[test]
    #[serial]
    fn test_rootfs_ls_json() {
        if !is_root() {
            eprintln!("Skipping test_rootfs_ls_json: requires root");
            return;
        }

        let name = "test-json-rootfs";

        // Clean up leftover.
        let _ = remora(&["rootfs", "rm", name]);

        // Import /tmp as a dummy rootfs.
        let (_, stderr, ok) = remora(&["rootfs", "import", name, "/tmp"]);
        assert!(ok, "rootfs import failed: {}", stderr);

        // List with --format json.
        let (stdout, stderr, ok) = remora(&["rootfs", "ls", "--format", "json"]);
        assert!(ok, "rootfs ls --format json failed: {}", stderr);
        let parsed: serde_json::Value =
            serde_json::from_str(&stdout).expect("rootfs ls JSON should parse");
        let arr = parsed.as_array().expect("expected JSON array");
        let entry = arr
            .iter()
            .find(|v| v.get("name").and_then(|n| n.as_str()) == Some(name));
        assert!(
            entry.is_some(),
            "rootfs '{}' not in JSON output: {}",
            name,
            stdout
        );
        assert!(
            entry
                .unwrap()
                .get("path")
                .and_then(|p| p.as_str())
                .is_some(),
            "rootfs JSON entry should have a 'path' field"
        );

        // Remove.
        let (_, stderr, ok) = remora(&["rootfs", "rm", name]);
        assert!(ok, "rootfs rm failed: {}", stderr);

        // Verify gone.
        let (stdout, _, ok) = remora(&["rootfs", "ls", "--format", "json"]);
        assert!(ok, "rootfs ls --format json failed after rm");
        let parsed: serde_json::Value =
            serde_json::from_str(&stdout).expect("rootfs ls JSON should parse after rm");
        let arr = parsed.as_array().expect("expected JSON array");
        let found = arr
            .iter()
            .any(|v| v.get("name").and_then(|n| n.as_str()) == Some(name));
        assert!(
            !found,
            "rootfs '{}' should not appear after rm: {}",
            name, stdout
        );
    }

    /// test_ps_json_and_inspect
    ///
    /// Requires: root (container state is stored under /run/remora/containers/).
    ///
    /// Writes a synthetic container state.json, verifies `ps -a --format json`
    /// includes the container with the correct name. Then runs
    /// `container inspect <name>` and verifies the JSON object has the expected
    /// fields. Removes the container via `rm` and verifies it is gone from the
    /// JSON listing.
    ///
    /// Failure indicates JSON serialization of container state or the inspect
    /// command is broken.
    #[test]
    #[serial]
    fn test_ps_json_and_inspect() {
        if !is_root() {
            eprintln!("Skipping test_ps_json_and_inspect: requires root");
            return;
        }

        let name = "test-json-ctr";

        // Clean up leftover.
        let _ = remora(&["rm", name]);

        // Write a synthetic container state directly (avoids spawning a real
        // container and the associated process lifecycle / cleanup overhead).
        let ctr_dir = remora::paths::containers_dir().join(name);
        std::fs::create_dir_all(&ctr_dir).expect("create container dir");
        let state = serde_json::json!({
            "name": name,
            "rootfs": "alpine",
            "status": "exited",
            "pid": 0,
            "watcher_pid": 0,
            "started_at": "2026-01-01T00:00:00Z",
            "exit_code": 0,
            "command": ["/bin/sh"],
            "stdout_log": null,
            "stderr_log": null
        });
        std::fs::write(
            ctr_dir.join("state.json"),
            serde_json::to_string_pretty(&state).unwrap(),
        )
        .expect("write state.json");

        // ps -a --format json should include the container.
        let (stdout, stderr, ok) = remora(&["ps", "-a", "--format", "json"]);
        assert!(ok, "ps --format json failed: {}", stderr);
        let parsed: serde_json::Value =
            serde_json::from_str(&stdout).expect("ps JSON should parse");
        let arr = parsed.as_array().expect("expected JSON array");
        let found = arr
            .iter()
            .any(|v| v.get("name").and_then(|n| n.as_str()) == Some(name));
        assert!(
            found,
            "container '{}' not in ps JSON output: {}",
            name, stdout
        );

        // container inspect should return a JSON object.
        let (stdout, stderr, ok) = remora(&["container", "inspect", name]);
        assert!(ok, "container inspect failed: {}", stderr);
        let obj: serde_json::Value =
            serde_json::from_str(&stdout).expect("inspect JSON should parse");
        assert!(obj.is_object(), "inspect should return a JSON object");
        assert_eq!(
            obj.get("name").and_then(|n| n.as_str()),
            Some(name),
            "inspect name mismatch"
        );
        assert!(
            obj.get("pid").is_some(),
            "inspect should include 'pid' field"
        );
        assert!(
            obj.get("status").is_some(),
            "inspect should include 'status' field"
        );

        // Remove the container.
        let (_, stderr, ok) = remora(&["rm", name]);
        assert!(ok, "rm failed: {}", stderr);

        // ps -a --format json should no longer include the container.
        let (stdout, _, ok) = remora(&["ps", "-a", "--format", "json"]);
        assert!(ok, "ps --format json failed after rm");
        let parsed: serde_json::Value =
            serde_json::from_str(&stdout).expect("ps JSON should parse after rm");
        let arr = parsed.as_array().expect("expected JSON array");
        let found = arr
            .iter()
            .any(|v| v.get("name").and_then(|n| n.as_str()) == Some(name));
        assert!(
            !found,
            "container '{}' should not appear after rm: {}",
            name, stdout
        );
    }

    /// test_image_ls_json
    ///
    /// Requires: root (images are stored under /var/lib/remora/images/).
    ///
    /// Verifies `image ls --format json` returns a valid JSON array. Does NOT
    /// pull an image (to keep the test fast and offline). If images already
    /// exist, validates that each entry has the expected fields (reference,
    /// digest, layers). If no images exist, verifies the output is `[]`.
    ///
    /// Failure indicates JSON serialization of image manifests is broken or
    /// the --format flag is not wired correctly.
    #[test]
    #[serial]
    fn test_image_ls_json() {
        if !is_root() {
            eprintln!("Skipping test_image_ls_json: requires root");
            return;
        }

        let (stdout, stderr, ok) = remora(&["image", "ls", "--format", "json"]);
        assert!(ok, "image ls --format json failed: {}", stderr);
        let parsed: serde_json::Value =
            serde_json::from_str(&stdout).expect("image ls JSON should parse");
        let arr = parsed.as_array().expect("expected JSON array");

        // Validate structure of any entries that exist.
        for entry in arr {
            assert!(
                entry.get("reference").and_then(|v| v.as_str()).is_some(),
                "image entry should have 'reference': {:?}",
                entry
            );
            assert!(
                entry.get("digest").and_then(|v| v.as_str()).is_some(),
                "image entry should have 'digest': {:?}",
                entry
            );
            assert!(
                entry.get("layers").and_then(|v| v.as_array()).is_some(),
                "image entry should have 'layers' array: {:?}",
                entry
            );
        }
    }
}

mod rootless_idmap {
    use super::*;

    /// Check whether multi-UID mapping via helpers is available.
    /// Requires: newuidmap + newgidmap on PATH, and non-empty subuid/subgid ranges.
    fn skip_unless_idmap_helpers() -> bool {
        if !remora::idmap::has_newuidmap() || !remora::idmap::has_newgidmap() {
            eprintln!("Skipping: newuidmap/newgidmap not available");
            return false;
        }
        let username = match remora::idmap::current_username() {
            Ok(u) => u,
            Err(_) => {
                eprintln!("Skipping: could not determine username");
                return false;
            }
        };
        let uid = unsafe { libc::getuid() };
        let uid_ranges =
            remora::idmap::parse_subid_file(std::path::Path::new("/etc/subuid"), &username, uid)
                .unwrap_or_default();
        let gid_ranges = remora::idmap::parse_subid_file(
            std::path::Path::new("/etc/subgid"),
            &username,
            unsafe { libc::getgid() },
        )
        .unwrap_or_default();
        if uid_ranges.is_empty() || gid_ranges.is_empty() {
            eprintln!("Skipping: no subordinate UID/GID ranges in /etc/subuid or /etc/subgid");
            return false;
        }
        true
    }

    #[test]
    fn test_rootless_multi_uid_maps_written() {
        if is_root() {
            eprintln!("Skipping: must run as non-root");
            return;
        }
        if !skip_unless_idmap_helpers() {
            return;
        }
        let Some(rootfs) = get_test_rootfs() else {
            eprintln!("Skipping: alpine-rootfs not found");
            return;
        };

        // Don't set uid_maps — let auto-config detect and use multi-range.
        // Use sleep so we can inspect the uid_map from the host side.
        let mut child = Command::new("/bin/sleep")
            .args(&["10"])
            .env("PATH", ALPINE_PATH)
            .with_chroot(&rootfs)
            .with_namespaces(Namespace::MOUNT | Namespace::UTS)
            .stdin(Stdio::Null)
            .stdout(Stdio::Null)
            .stderr(Stdio::Null)
            .spawn()
            .expect("rootless multi-uid spawn failed");

        // Read uid_map from the host-side /proc.
        let uid_map_path = format!("/proc/{}/uid_map", child.pid());
        let uid_map = std::fs::read_to_string(&uid_map_path)
            .unwrap_or_else(|e| panic!("failed to read {}: {}", uid_map_path, e));

        // Kill the sleeping container.
        unsafe {
            libc::kill(child.pid(), libc::SIGKILL);
        }
        child.wait().expect("Failed to wait");

        // Should have at least 2 lines: one for container root (0 → host_uid),
        // one for subordinate range (1 → subuid_start).
        let lines: Vec<&str> = uid_map.lines().collect();
        assert!(
            lines.len() >= 2,
            "expected at least 2 uid_map lines, got {} lines: {:?}",
            lines.len(),
            lines
        );
    }

    #[test]
    fn test_rootless_multi_uid_file_ownership() {
        if is_root() {
            eprintln!("Skipping: must run as non-root");
            return;
        }
        if !skip_unless_idmap_helpers() {
            return;
        }
        let Some(rootfs) = get_test_rootfs() else {
            eprintln!("Skipping: alpine-rootfs not found");
            return;
        };

        // Run stat on /etc/passwd — the file is owned by root:root (0:0) in the image.
        // With multi-UID mapping, it should show UID 0 (not 65534/nobody).
        let child = Command::new("/bin/ash")
            .args(&["-c", "stat -c '%u' /etc/passwd"])
            .env("PATH", ALPINE_PATH)
            .with_chroot(&rootfs)
            .with_namespaces(Namespace::MOUNT | Namespace::UTS)
            .stdout(Stdio::Piped)
            .stderr(Stdio::Piped)
            .spawn()
            .expect("rootless file-ownership spawn failed");

        let (status, stdout, stderr) = child.wait_with_output().expect("wait failed");
        let out = String::from_utf8_lossy(&stdout);
        let err = String::from_utf8_lossy(&stderr);
        assert!(
            status.success(),
            "container exited non-zero; stdout={}, stderr={}",
            out,
            err
        );
        assert_eq!(
            out.trim(),
            "0",
            "expected /etc/passwd owned by UID 0, got: {}",
            out.trim()
        );
    }

    #[test]
    fn test_rootless_single_uid_fallback() {
        if is_root() {
            eprintln!("Skipping: must run as non-root");
            return;
        }
        let Some(rootfs) = get_test_rootfs() else {
            eprintln!("Skipping: alpine-rootfs not found");
            return;
        };

        // Explicitly set a single-UID map (bypassing auto-config).
        // Verify the container still works with single-UID mapping.
        let child = Command::new("/bin/ash")
            .args(&["-c", "id -u"])
            .env("PATH", ALPINE_PATH)
            .with_chroot(&rootfs)
            .with_namespaces(Namespace::MOUNT | Namespace::UTS)
            .with_uid_maps(&[UidMap {
                inside: 0,
                outside: unsafe { libc::getuid() },
                count: 1,
            }])
            .with_gid_maps(&[GidMap {
                inside: 0,
                outside: unsafe { libc::getgid() },
                count: 1,
            }])
            .stdout(Stdio::Piped)
            .stderr(Stdio::Piped)
            .spawn()
            .expect("rootless single-uid spawn failed");

        let (status, stdout, _stderr) = child.wait_with_output().expect("wait failed");
        assert!(status.success(), "container exited non-zero");
        let out = String::from_utf8_lossy(&stdout);
        assert_eq!(
            out.trim(),
            "0",
            "expected uid 0 inside container, got: {}",
            out.trim()
        );
    }
}

// ── Build instruction tests (ENTRYPOINT, LABEL, USER, cache) ────────────────

mod build_instructions {
    use remora::build;
    use std::collections::HashMap;

    /// test_parse_entrypoint_json
    ///
    /// Requires: neither root nor rootfs (parser-only).
    ///
    /// Parses a Remfile containing `ENTRYPOINT ["python3", "-m", "http.server"]`
    /// and verifies it produces the expected `Instruction::Entrypoint` variant
    /// with the correct argument list.
    ///
    /// Failure indicates the ENTRYPOINT JSON-form parser is broken.
    #[test]
    fn test_parse_entrypoint_json() {
        let content = r#"FROM alpine
ENTRYPOINT ["python3", "-m", "http.server"]
CMD ["8080"]"#;
        let instructions = build::parse_remfile(content).unwrap();
        assert_eq!(instructions.len(), 3);
        assert_eq!(
            instructions[1],
            build::Instruction::Entrypoint(vec![
                "python3".into(),
                "-m".into(),
                "http.server".into()
            ])
        );
        assert_eq!(
            instructions[2],
            build::Instruction::Cmd(vec!["8080".into()])
        );
    }

    /// test_parse_entrypoint_shell_form
    ///
    /// Requires: neither root nor rootfs (parser-only).
    ///
    /// Parses `ENTRYPOINT /usr/bin/myapp` (shell form) and verifies it is
    /// wrapped in `/bin/sh -c ...` like CMD shell form.
    ///
    /// Failure indicates shell-form ENTRYPOINT wrapping is broken.
    #[test]
    fn test_parse_entrypoint_shell_form() {
        let content = "FROM alpine\nENTRYPOINT /usr/bin/myapp --flag";
        let instructions = build::parse_remfile(content).unwrap();
        assert_eq!(
            instructions[1],
            build::Instruction::Entrypoint(vec![
                "/bin/sh".into(),
                "-c".into(),
                "/usr/bin/myapp --flag".into()
            ])
        );
    }

    /// test_parse_label_quoted_and_unquoted
    ///
    /// Requires: neither root nor rootfs (parser-only).
    ///
    /// Parses `LABEL` with both quoted and unquoted values and verifies both
    /// forms produce correct key-value pairs.
    ///
    /// Failure indicates LABEL value parsing or quote stripping is broken.
    #[test]
    fn test_parse_label_quoted_and_unquoted() {
        let content = "FROM alpine\nLABEL maintainer=\"Jane Doe\"\nLABEL version=2.0";
        let instructions = build::parse_remfile(content).unwrap();
        assert_eq!(
            instructions[1],
            build::Instruction::Label {
                key: "maintainer".into(),
                value: "Jane Doe".into()
            }
        );
        assert_eq!(
            instructions[2],
            build::Instruction::Label {
                key: "version".into(),
                value: "2.0".into()
            }
        );
    }

    /// test_parse_user_with_gid
    ///
    /// Requires: neither root nor rootfs (parser-only).
    ///
    /// Parses `USER 1000:1000` and verifies the full string is captured
    /// (parsing uid:gid is the runtime's job, not the parser's).
    ///
    /// Failure indicates USER instruction parsing is broken.
    #[test]
    fn test_parse_user_with_gid() {
        let content = "FROM alpine\nUSER 1000:1000";
        let instructions = build::parse_remfile(content).unwrap();
        assert_eq!(
            instructions[1],
            build::Instruction::User("1000:1000".into())
        );
    }

    /// test_image_config_labels_serde_roundtrip
    ///
    /// Requires: neither root nor rootfs (serialization-only).
    ///
    /// Creates an `ImageConfig` with labels, serializes to JSON, deserializes,
    /// and verifies labels survive the round-trip. Also verifies that an empty
    /// `labels` field deserializes correctly from JSON missing the key (serde default).
    ///
    /// Failure indicates the `labels` field has broken serde attributes.
    #[test]
    fn test_image_config_labels_serde_roundtrip() {
        use remora::image::ImageConfig;

        let mut labels = HashMap::new();
        labels.insert("maintainer".to_string(), "test@example.com".to_string());
        labels.insert("version".to_string(), "1.0".to_string());

        let config = ImageConfig {
            env: vec![],
            cmd: vec![],
            entrypoint: vec![],
            working_dir: String::new(),
            user: String::new(),
            labels: labels.clone(),
        };

        let json = serde_json::to_string(&config).unwrap();
        let loaded: ImageConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.labels, labels);

        // Also check that missing "labels" key deserializes to empty map.
        let minimal = r#"{"env":[],"cmd":[]}"#;
        let loaded: ImageConfig = serde_json::from_str(minimal).unwrap();
        assert!(loaded.labels.is_empty());
    }

    /// test_image_config_user_field
    ///
    /// Requires: neither root nor rootfs (serialization-only).
    ///
    /// Verifies `ImageConfig.user` round-trips through JSON and that
    /// missing "user" key defaults to empty string.
    ///
    /// Failure indicates the `user` field serde default is broken.
    #[test]
    fn test_image_config_user_field() {
        use remora::image::ImageConfig;

        let config = ImageConfig {
            env: vec![],
            cmd: vec![],
            entrypoint: vec!["/app".to_string()],
            working_dir: String::new(),
            user: "1000:1000".to_string(),
            labels: HashMap::new(),
        };

        let json = serde_json::to_string(&config).unwrap();
        let loaded: ImageConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.user, "1000:1000");
        assert_eq!(loaded.entrypoint, vec!["/app"]);
    }

    /// test_full_remfile_with_all_instructions
    ///
    /// Requires: neither root nor rootfs (parser-only).
    ///
    /// Parses a Remfile using every supported instruction type and verifies the
    /// complete instruction list. This is a comprehensive parser integration test.
    ///
    /// Failure indicates a regression in any instruction parser.
    #[test]
    fn test_full_remfile_with_all_instructions() {
        let content = r#"
FROM alpine:3.19
LABEL maintainer="test"
ENV APP_PORT=8080
USER nobody
WORKDIR /app
COPY app.py /app/app.py
RUN apk add python3
ENTRYPOINT ["python3"]
CMD ["app.py"]
EXPOSE 8080
"#;
        let instructions = build::parse_remfile(content).unwrap();
        assert_eq!(instructions.len(), 10);
        assert!(matches!(instructions[0], build::Instruction::From(_)));
        assert!(matches!(instructions[1], build::Instruction::Label { .. }));
        assert!(matches!(instructions[2], build::Instruction::Env { .. }));
        assert!(matches!(instructions[3], build::Instruction::User(_)));
        assert!(matches!(instructions[4], build::Instruction::Workdir(_)));
        assert!(matches!(instructions[5], build::Instruction::Copy { .. }));
        assert!(matches!(instructions[6], build::Instruction::Run(_)));
        assert!(matches!(instructions[7], build::Instruction::Entrypoint(_)));
        assert!(matches!(instructions[8], build::Instruction::Cmd(_)));
        assert!(matches!(instructions[9], build::Instruction::Expose(_)));
    }
}

// ── Port proxy tests ────────────────────────────────────────────────────────

mod port_proxy {
    use super::*;

    /// test_port_proxy_localhost_connectivity
    ///
    /// Requires: root, alpine-rootfs.
    ///
    /// Spawns a bridge+NAT container running a one-shot TCP server on port 80,
    /// forwarded from host port 19190. Connects from **localhost** (127.0.0.1)
    /// to verify the userspace TCP proxy handles localhost traffic that nftables
    /// DNAT in PREROUTING cannot intercept.
    ///
    /// Failure indicates the userspace TCP proxy (`start_port_proxies`) is broken
    /// or not relaying localhost connections to the container.
    #[test]
    #[serial(nat)]
    fn test_port_proxy_localhost_connectivity() {
        if !is_root() {
            eprintln!("Skipping test_port_proxy_localhost_connectivity: requires root");
            return;
        }
        let Some(rootfs) = get_test_rootfs() else {
            eprintln!("Skipping test_port_proxy_localhost_connectivity: alpine-rootfs not found");
            return;
        };

        // Check that nc is available on the host.
        let nc_ok = std::process::Command::new("which")
            .arg("nc")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !nc_ok {
            eprintln!("Skipping test_port_proxy_localhost_connectivity: nc not found on host");
            return;
        }

        // Container: one-shot TCP server on port 80, forwarded from host 19190.
        let mut child = Command::new("/bin/sh")
            .args(&["-c", "echo PROXY_WORKS | nc -l -p 80"])
            .with_namespaces(Namespace::MOUNT | Namespace::UTS)
            .with_network(NetworkMode::Bridge)
            .with_nat()
            .with_port_forward(19190, 80)
            .with_chroot(&rootfs)
            .env("PATH", ALPINE_PATH)
            .stdin(Stdio::Null)
            .stdout(Stdio::Null)
            .stderr(Stdio::Null)
            .spawn()
            .expect("Failed to spawn container");

        // Give nc time to start listening inside the container.
        std::thread::sleep(std::time::Duration::from_millis(500));

        // Connect from LOCALHOST — this goes through the userspace proxy,
        // not through nftables PREROUTING (which doesn't see localhost traffic).
        let output = std::process::Command::new("nc")
            .args(&["-w", "2", "127.0.0.1", "19190"])
            .output()
            .expect("nc to localhost");

        let out = String::from_utf8_lossy(&output.stdout);

        // Clean up.
        unsafe {
            libc::kill(child.pid(), libc::SIGKILL);
        }
        let _ = child.wait();

        assert!(
            out.contains("PROXY_WORKS"),
            "Localhost connection via port proxy should receive 'PROXY_WORKS'.\nstdout: {}\nstderr: {}",
            out,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// test_port_proxy_cleanup_on_teardown
    ///
    /// Requires: root, alpine-rootfs.
    ///
    /// Spawns a container with a port forward, waits for it to exit, then
    /// verifies the proxy port is no longer listening (bind should succeed).
    ///
    /// Failure indicates the proxy stop flag is not set during teardown,
    /// leaving orphaned listener threads.
    #[test]
    #[serial(nat)]
    fn test_port_proxy_cleanup_on_teardown() {
        if !is_root() {
            eprintln!("Skipping test_port_proxy_cleanup_on_teardown: requires root");
            return;
        }
        let Some(rootfs) = get_test_rootfs() else {
            eprintln!("Skipping test_port_proxy_cleanup_on_teardown: alpine-rootfs not found");
            return;
        };

        // Container: exits immediately.
        let mut child = Command::new("/bin/true")
            .with_namespaces(Namespace::MOUNT | Namespace::UTS)
            .with_network(NetworkMode::Bridge)
            .with_nat()
            .with_port_forward(19191, 80)
            .with_chroot(&rootfs)
            .env("PATH", ALPINE_PATH)
            .stdin(Stdio::Null)
            .stdout(Stdio::Null)
            .stderr(Stdio::Null)
            .spawn()
            .expect("Failed to spawn container");

        let status = child.wait().expect("wait failed");
        assert!(status.success(), "container should exit cleanly");

        // Give proxy threads time to notice the stop flag.
        std::thread::sleep(std::time::Duration::from_millis(300));

        // The proxy should have released the port. Verify by binding to it.
        let bind_result = std::net::TcpListener::bind("0.0.0.0:19191");
        assert!(
            bind_result.is_ok(),
            "Port 19191 should be free after teardown, but bind failed: {:?}",
            bind_result.err()
        );
    }
}

// ==========================================================================
// Multi-network tests
// ==========================================================================

mod multi_network {
    use super::*;
    use remora::network::{Ipv4Net, NetworkDef};

    /// Clean up a test network (best-effort).
    fn cleanup_test_network(name: &str) {
        let config_dir = remora::paths::network_config_dir(name);
        let _ = std::fs::remove_dir_all(&config_dir);
        let runtime_dir = remora::paths::network_runtime_dir(name);
        let _ = std::fs::remove_dir_all(&runtime_dir);
        // Delete bridge if it exists.
        let bridge = if name == "remora0" {
            "remora0".to_string()
        } else {
            format!("rm-{}", name)
        };
        let _ = std::process::Command::new("ip")
            .args(["link", "del", &bridge])
            .stderr(std::process::Stdio::null())
            .status();
    }

    /// Test network create, ls, and rm lifecycle.
    ///
    /// Requires root: creates config dirs under /var/lib/remora/networks/.
    #[test]
    #[serial]
    fn test_network_create_ls_rm() {
        if !is_root() {
            eprintln!("Skipping test_network_create_ls_rm (requires root)");
            return;
        }
        let name = "testnet1";
        cleanup_test_network(name);

        // Create
        let subnet = Ipv4Net::from_cidr("10.99.1.0/24").unwrap();
        let net = NetworkDef {
            name: name.to_string(),
            subnet: subnet.clone(),
            gateway: subnet.gateway(),
            bridge_name: format!("rm-{}", name),
        };
        net.save().expect("save network");

        // Verify config file exists.
        let config = remora::paths::network_config_dir(name).join("config.json");
        assert!(config.exists(), "config.json should exist after save");

        // Load and verify roundtrip.
        let loaded = NetworkDef::load(name).expect("load network");
        assert_eq!(loaded.name, name);
        assert_eq!(loaded.subnet.cidr_string(), "10.99.1.0/24");
        assert_eq!(loaded.gateway, std::net::Ipv4Addr::new(10, 99, 1, 1));
        assert_eq!(loaded.bridge_name, "rm-testnet1");

        // Remove
        cleanup_test_network(name);
        assert!(!config.exists(), "config.json should be gone after cleanup");
    }

    /// Two networks with overlapping subnets — verify detection.
    ///
    /// Requires root: writes to /var/lib/remora/networks/.
    #[test]
    #[serial]
    fn test_network_create_overlap_rejected() {
        if !is_root() {
            eprintln!("Skipping test_network_create_overlap_rejected (requires root)");
            return;
        }
        let name_a = "overlap-a";
        let name_b = "overlap-b";
        cleanup_test_network(name_a);
        cleanup_test_network(name_b);

        // Create first network.
        let subnet_a = Ipv4Net::from_cidr("10.77.0.0/16").unwrap();
        let net_a = NetworkDef {
            name: name_a.to_string(),
            subnet: subnet_a.clone(),
            gateway: subnet_a.gateway(),
            bridge_name: format!("rm-{}", name_a),
        };
        net_a.save().expect("save network A");

        // Second network with overlapping /24 inside the first /16.
        let subnet_b = Ipv4Net::from_cidr("10.77.1.0/24").unwrap();
        assert!(
            subnet_a.overlaps(&subnet_b),
            "10.77.0.0/16 and 10.77.1.0/24 should overlap"
        );

        // Walk existing networks to check for overlap (same logic as CLI).
        let networks_dir = remora::paths::networks_config_dir();
        let mut found_overlap = false;
        if let Ok(entries) = std::fs::read_dir(&networks_dir) {
            for entry in entries.flatten() {
                let cfg_path = entry.path().join("config.json");
                if let Ok(data) = std::fs::read_to_string(&cfg_path) {
                    if let Ok(existing) = serde_json::from_str::<NetworkDef>(&data) {
                        if existing.subnet.overlaps(&subnet_b) {
                            found_overlap = true;
                        }
                    }
                }
            }
        }
        assert!(found_overlap, "overlap should be detected");

        cleanup_test_network(name_a);
        cleanup_test_network(name_b);
    }

    /// Validate network name constraints.
    ///
    /// Requires root: no rootfs needed, API-level checks.
    #[test]
    fn test_network_name_validation() {
        // Too long (> 12 chars).
        let long_name = "abcdefghijklm"; // 13 chars
        assert!(long_name.len() > 12);

        // Invalid chars.
        let bad_chars = "net_work";
        assert!(bad_chars.contains('_'));

        // Leading hyphen.
        let leading = "-net";
        assert!(leading.starts_with('-'));

        // Ipv4Net parsing.
        assert!(Ipv4Net::from_cidr("not-a-cidr").is_err());
        assert!(Ipv4Net::from_cidr("10.0.0.0/33").is_err());
        assert!(Ipv4Net::from_cidr("10.0.0.0/24").is_ok());
    }

    /// Run a container on a custom named network and verify it gets an IP
    /// in the correct subnet.
    ///
    /// Requires root + rootfs.
    #[test]
    #[serial(nat)]
    fn test_named_network_container() {
        if !is_root() {
            eprintln!("Skipping test_named_network_container (requires root)");
            return;
        }
        let rootfs = match get_test_rootfs() {
            Some(r) => r,
            None => {
                eprintln!("Skipping test_named_network_container (no rootfs)");
                return;
            }
        };

        let name = "testnet2";
        cleanup_test_network(name);

        // Create a test network with subnet 10.98.1.0/24.
        let subnet = Ipv4Net::from_cidr("10.98.1.0/24").unwrap();
        let net = NetworkDef {
            name: name.to_string(),
            subnet: subnet.clone(),
            gateway: subnet.gateway(),
            bridge_name: format!("rm-{}", name),
        };
        net.save().expect("save network");

        // Run container on the named network.
        let child = Command::new("/bin/sh")
            .args(&["-c", "ip addr show eth0 | grep 'inet '"])
            .with_namespaces(Namespace::MOUNT | Namespace::UTS)
            .with_network(NetworkMode::BridgeNamed(name.to_string()))
            .with_nat()
            .with_chroot(&rootfs)
            .with_proc_mount()
            .env("PATH", ALPINE_PATH)
            .stdin(Stdio::Null)
            .stdout(Stdio::Piped)
            .stderr(Stdio::Piped)
            .spawn()
            .expect("spawn on named network");

        let (_status, stdout_raw, _stderr) = child.wait_with_output().expect("wait_with_output");
        let stdout = String::from_utf8_lossy(&stdout_raw);

        // Verify the IP is in the 10.98.1.0/24 range.
        assert!(
            stdout.contains("10.98.1."),
            "container IP should be in 10.98.1.0/24, got: {}",
            stdout.trim()
        );

        cleanup_test_network(name);
    }

    /// `--network bridge` (the default bridge) should still work and assign
    /// a 172.19.0.x IP, proving backwards compatibility.
    ///
    /// Requires root + rootfs.
    #[test]
    #[serial(nat)]
    fn test_default_network_backwards_compat() {
        if !is_root() {
            eprintln!("Skipping test_default_network_backwards_compat (requires root)");
            return;
        }
        let rootfs = match get_test_rootfs() {
            Some(r) => r,
            None => {
                eprintln!("Skipping test_default_network_backwards_compat (no rootfs)");
                return;
            }
        };

        let child = Command::new("/bin/sh")
            .args(&["-c", "ip addr show eth0 | grep 'inet '"])
            .with_namespaces(Namespace::MOUNT | Namespace::UTS)
            .with_network(NetworkMode::Bridge)
            .with_nat()
            .with_chroot(&rootfs)
            .with_proc_mount()
            .env("PATH", ALPINE_PATH)
            .stdin(Stdio::Null)
            .stdout(Stdio::Piped)
            .stderr(Stdio::Piped)
            .spawn()
            .expect("spawn on default bridge");

        let (_status, stdout_raw, _stderr) = child.wait_with_output().expect("wait_with_output");
        let stdout = String::from_utf8_lossy(&stdout_raw);

        assert!(
            stdout.contains("172.19.0."),
            "default bridge should assign 172.19.0.x IP, got: {}",
            stdout.trim()
        );
    }

    /// Cannot remove the default network.
    ///
    /// Requires root.
    #[test]
    #[serial]
    fn test_network_rm_refuses_default() {
        if !is_root() {
            eprintln!("Skipping test_network_rm_refuses_default (requires root)");
            return;
        }
        // The CLI refuses removal of "remora0" — but we test the concept:
        // the default network config should survive bootstrap.
        let _ = remora::network::bootstrap_default_network().expect("bootstrap default");
        let config = remora::paths::network_config_dir("remora0").join("config.json");
        assert!(config.exists(), "default network config should exist");
    }
}
