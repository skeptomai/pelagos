//! Landlock LSM integration — filesystem access control via Linux 5.13+ syscalls.
//!
//! Landlock is a self-contained, unprivileged sandboxing mechanism that lets a process
//! restrict its own filesystem access without requiring root, external profiles, or a
//! running LSM daemon.  It complements seccomp (syscall restriction) and capabilities
//! (privilege restriction) with path-based access restriction.
//!
//! # ABI versions
//!
//! | ABI | Kernel | New access rights |
//! |-----|--------|-------------------|
//! |  1  | 5.13   | FS execute, read/write, dir ops, mknod, symlink |
//! |  2  | 5.19   | `REFER` (cross-dir rename/link) |
//! |  3  | 6.2    | `TRUNCATE` |
//! |  4  | 6.7    | `IOCTL_DEV`, net bind/connect TCP |
//!
//! # Usage in pre_exec
//!
//! `apply_landlock` must be called:
//! - **After** `chroot`/`pivot_root` — paths are resolved in the container root.
//! - **After** `CAP_SYS_ADMIN` is still held OR after `PR_SET_NO_NEW_PRIVS` is set —
//!   `landlock_restrict_self` requires one or the other.
//! - **Before** seccomp — the Landlock syscalls (444–446) must not be blocked.
//!
//! Returns `Ok(())` if the kernel does not support Landlock (`ENOSYS`).

use std::ffi::CString;
use std::io;
use std::path::PathBuf;

// ---------------------------------------------------------------------------
// Landlock ABI constants
// ---------------------------------------------------------------------------

/// Flag for `landlock_create_ruleset` to query the ABI version instead of
/// creating a ruleset.
const LANDLOCK_CREATE_RULESET_VERSION: libc::c_long = 1 << 0;
/// Rule type: restrict access beneath a path (directory or file).
const LANDLOCK_RULE_PATH_BENEATH: libc::c_long = 1;

// ABI v1 (Linux 5.13) filesystem access rights.
const LANDLOCK_ACCESS_FS_EXECUTE: u64 = 1 << 0;
const LANDLOCK_ACCESS_FS_WRITE_FILE: u64 = 1 << 1;
const LANDLOCK_ACCESS_FS_READ_FILE: u64 = 1 << 2;
const LANDLOCK_ACCESS_FS_READ_DIR: u64 = 1 << 3;
const LANDLOCK_ACCESS_FS_REMOVE_DIR: u64 = 1 << 4;
const LANDLOCK_ACCESS_FS_REMOVE_FILE: u64 = 1 << 5;
const LANDLOCK_ACCESS_FS_MAKE_CHAR: u64 = 1 << 6;
const LANDLOCK_ACCESS_FS_MAKE_DIR: u64 = 1 << 7;
const LANDLOCK_ACCESS_FS_MAKE_REG: u64 = 1 << 8;
const LANDLOCK_ACCESS_FS_MAKE_SOCK: u64 = 1 << 9;
const LANDLOCK_ACCESS_FS_MAKE_FIFO: u64 = 1 << 10;
const LANDLOCK_ACCESS_FS_MAKE_BLOCK: u64 = 1 << 11;
const LANDLOCK_ACCESS_FS_MAKE_SYM: u64 = 1 << 12;
// ABI v2 (Linux 5.19).
const LANDLOCK_ACCESS_FS_REFER: u64 = 1 << 13;
// ABI v3 (Linux 6.2).
const LANDLOCK_ACCESS_FS_TRUNCATE: u64 = 1 << 14;
// ABI v4 (Linux 6.7).
const LANDLOCK_ACCESS_FS_IOCTL_DEV: u64 = 1 << 15;

/// Allow reading and executing files and listing directories; no modification.
pub const FS_ACCESS_RO: u64 =
    LANDLOCK_ACCESS_FS_EXECUTE | LANDLOCK_ACCESS_FS_READ_FILE | LANDLOCK_ACCESS_FS_READ_DIR;

/// Allow all filesystem operations (read + write + execute + create/remove/rename).
pub const FS_ACCESS_RW: u64 = LANDLOCK_ACCESS_FS_EXECUTE
    | LANDLOCK_ACCESS_FS_WRITE_FILE
    | LANDLOCK_ACCESS_FS_READ_FILE
    | LANDLOCK_ACCESS_FS_READ_DIR
    | LANDLOCK_ACCESS_FS_REMOVE_DIR
    | LANDLOCK_ACCESS_FS_REMOVE_FILE
    | LANDLOCK_ACCESS_FS_MAKE_CHAR
    | LANDLOCK_ACCESS_FS_MAKE_DIR
    | LANDLOCK_ACCESS_FS_MAKE_REG
    | LANDLOCK_ACCESS_FS_MAKE_SOCK
    | LANDLOCK_ACCESS_FS_MAKE_FIFO
    | LANDLOCK_ACCESS_FS_MAKE_BLOCK
    | LANDLOCK_ACCESS_FS_MAKE_SYM
    | LANDLOCK_ACCESS_FS_REFER
    | LANDLOCK_ACCESS_FS_TRUNCATE
    | LANDLOCK_ACCESS_FS_IOCTL_DEV;

// ---------------------------------------------------------------------------
// Kernel struct layouts (must match <linux/landlock.h>)
// ---------------------------------------------------------------------------

#[repr(C)]
struct LandlockRulesetAttr {
    handled_access_fs: u64,
}

/// path_beneath_attr must be packed — the kernel definition is `__attribute__((packed))`.
#[repr(C, packed)]
struct LandlockPathBeneathAttr {
    allowed_access: u64,
    parent_fd: i32,
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// A single Landlock filesystem rule: allow `access` rights beneath `path`.
#[derive(Clone, Debug)]
pub struct LandlockRule {
    /// Path to allow (resolved after chroot — e.g. `/etc`, `/usr`).
    pub path: PathBuf,
    /// Bitmask of allowed access rights.  Use [`FS_ACCESS_RO`] or [`FS_ACCESS_RW`],
    /// or construct a custom bitmask from the `LANDLOCK_ACCESS_FS_*` constants.
    pub access: u64,
}

/// Query the Landlock ABI version supported by the running kernel.
///
/// Returns `0` if Landlock is not available (kernel < 5.13 or not compiled in).
pub fn get_abi_version() -> u32 {
    let ret = unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            std::ptr::null::<LandlockRulesetAttr>(),
            0usize,
            LANDLOCK_CREATE_RULESET_VERSION,
        )
    };
    if ret < 0 {
        0
    } else {
        ret as u32
    }
}

/// Compute the bitmask of all filesystem access rights for the given ABI version.
///
/// Clamping `handled_access_fs` to the supported bits avoids EINVAL when
/// creating a ruleset on an older kernel with a flag it doesn't know.
fn fs_access_mask_for_abi(abi: u32) -> u64 {
    // ABI v1: bits 0–12 (13 rights).
    let mut mask: u64 = (1 << 13) - 1;
    if abi >= 2 {
        mask |= LANDLOCK_ACCESS_FS_REFER;
    }
    if abi >= 3 {
        mask |= LANDLOCK_ACCESS_FS_TRUNCATE;
    }
    if abi >= 4 {
        mask |= LANDLOCK_ACCESS_FS_IOCTL_DEV;
    }
    mask
}

/// Apply Landlock filesystem rules to the calling process.
///
/// After this call, the process (and its descendants) can only access paths
/// that have an explicit allow rule; all other filesystem access is denied.
///
/// # Requirements
/// - Must be called **after** chroot/pivot_root.
/// - Must be called **before** seccomp (syscalls 444–446 must not be blocked).
/// - The calling thread must have either `CAP_SYS_ADMIN` or `no_new_privs` set.
///
/// # Errors
/// Returns `Ok(())` if the kernel does not support Landlock (`ENOSYS` — silent
/// no-op).  Returns `Err` on any other failure (e.g. `EPERM` when neither
/// `CAP_SYS_ADMIN` nor `no_new_privs` is satisfied).
///
/// Paths that do not exist in the container filesystem are silently skipped
/// (the rule is vacuous — the process cannot access a path that doesn't exist).
pub fn apply_landlock(rules: &[LandlockRule]) -> io::Result<()> {
    if rules.is_empty() {
        return Ok(());
    }

    let abi = get_abi_version();
    if abi == 0 {
        return Ok(()); // Kernel < 5.13 or Landlock not compiled in.
    }

    // handled_access_fs: the set of rights this ruleset controls (deny by default).
    // We restrict all rights the running ABI version knows about.
    let handled_access = fs_access_mask_for_abi(abi);

    let attr = LandlockRulesetAttr {
        handled_access_fs: handled_access,
    };
    let ruleset_fd = unsafe {
        let ret = libc::syscall(
            libc::SYS_landlock_create_ruleset,
            &attr as *const LandlockRulesetAttr,
            std::mem::size_of::<LandlockRulesetAttr>() as libc::size_t,
            0i64,
        );
        if ret < 0 {
            let e = io::Error::last_os_error();
            if e.raw_os_error() == Some(libc::ENOSYS) {
                return Ok(()); // Race: ABI check passed but create failed.
            }
            return Err(e);
        }
        ret as i32
    };

    for rule in rules {
        let cpath = match CString::new(rule.path.to_string_lossy().as_ref()) {
            Ok(s) => s,
            Err(_) => continue, // Null byte in path — skip.
        };

        let path_fd = unsafe { libc::open(cpath.as_ptr(), libc::O_PATH | libc::O_CLOEXEC) };
        if path_fd < 0 {
            // Path doesn't exist in the container filesystem — skip silently.
            continue;
        }

        // Clamp allowed_access to what this ABI version supports.
        let allowed = rule.access & handled_access;
        let path_attr = LandlockPathBeneathAttr {
            allowed_access: allowed,
            parent_fd: path_fd,
        };

        unsafe {
            // Ignore individual rule errors — best-effort per-path.
            libc::syscall(
                libc::SYS_landlock_add_rule,
                ruleset_fd as libc::c_long,
                LANDLOCK_RULE_PATH_BENEATH,
                &path_attr as *const LandlockPathBeneathAttr,
                0i64,
            );
            libc::close(path_fd);
        }
    }

    // Restrict the calling thread (and all future descendants) to the ruleset.
    let ret = unsafe {
        libc::syscall(
            libc::SYS_landlock_restrict_self,
            ruleset_fd as libc::c_long,
            0i64,
        )
    };
    unsafe { libc::close(ruleset_fd) };
    if ret < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
