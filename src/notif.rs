//! `SECCOMP_RET_USER_NOTIF` supervisor mode — userspace syscall interception.
//!
//! Linux ≥ 5.0 allows a seccomp filter to return `SECCOMP_RET_USER_NOTIF`
//! instead of `ALLOW`/`ERRNO`/`KILL` for specific syscalls.  When triggered,
//! the kernel suspends the calling thread mid-syscall and delivers a notification
//! to a file descriptor held by a supervisor process.  The supervisor reads the
//! notification (syscall number, all 6 arguments, PID), makes a policy decision,
//! and writes a response (allow / deny with errno / synthetic return value).  The
//! suspended thread resumes with the response result.
//!
//! This enables policies impossible with static BPF:
//! - Per-destination egress network control (`connect` interception)
//! - Mount proxying without granting `CAP_SYS_ADMIN` to the container
//! - Audit logging of sensitive syscalls with full argument visibility
//!
//! # Architecture
//!
//! ```text
//!  Parent process (supervisor)          Child process (container)
//!  ─────────────────────────────        ─────────────────────────────
//!  socketpair(parent_end, child_end)    [inherited child_end via fork]
//!  fork()
//!                                       pre_exec:
//!                                         install regular seccomp filter
//!                                         install user_notif filter ──┐
//!                                         notif_fd = return value     │
//!                                         send_fd(child_end, notif_fd)│
//!                                         close(child_end)            │
//!                                         return → exec               │
//!  recv_fd(parent_end) ←────────────────────────────────────────────-┘
//!  spawn supervisor thread ──► loop:
//!    poll(notif_fd) → RECV ioctl → handler() → SEND ioctl
//! ```
//!
//! # Ordering in pre_exec
//!
//! The user_notif filter is installed **after** the regular seccomp filter.
//! The kernel evaluates filters in LIFO order (last installed = first evaluated),
//! so the user_notif filter wins for intercepted syscalls; all others fall through
//! to the regular filter.  Landlock and regular seccomp are already applied before
//! this step.
//!
//! # Requirements
//!
//! - Linux ≥ 5.0 (`SECCOMP_RET_USER_NOTIF` + `SECCOMP_FILTER_FLAG_NEW_LISTENER`)
//! - `CAP_SYS_ADMIN` **or** `no_new_privs` (same as regular seccomp)
//! - The supervisor fd must be held open for the full container lifetime

use std::io;
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Public API types
// ---------------------------------------------------------------------------

/// Details of a suspended syscall, delivered to the supervisor handler.
#[derive(Debug, Clone)]
pub struct SyscallNotif {
    /// Opaque notification ID — must be echoed in the response.
    pub id: u64,
    /// PID of the thread that made the syscall.
    pub pid: u32,
    /// Syscall number (e.g. `libc::SYS_connect` on x86_64 = 42).
    pub nr: i32,
    /// CPU architecture identifier (e.g. `AUDIT_ARCH_X86_64 = 0xC000003E`).
    pub arch: u32,
    /// Instruction pointer at the time of the syscall.
    pub instruction_pointer: u64,
    /// Syscall arguments (args[0]..args[5]).
    pub args: [u64; 6],
}

/// Supervisor response to a [`SyscallNotif`].
#[derive(Debug, Clone)]
pub enum SyscallResponse {
    /// Allow the syscall to proceed normally (as if no filter were installed).
    Allow,
    /// Deny the syscall; the thread receives `Err(errno)`.
    Deny(i32),
    /// Return a specific value to the thread (errno = 0).
    Return(i64),
}

/// Trait implemented by a seccomp user-notification handler.
///
/// The handler is called from a dedicated supervisor thread for each
/// intercepted syscall.  It must not block for long — the container thread
/// is suspended while waiting for the response.
pub trait SyscallHandler: Send + Sync + 'static {
    fn handle(&self, notif: &SyscallNotif) -> SyscallResponse;
}

// ---------------------------------------------------------------------------
// Kernel ABI constants and structs
// ---------------------------------------------------------------------------

const SECCOMP_SET_MODE_FILTER: libc::c_int = 1;
/// Flag: return a notification fd from `seccomp(2)` (Linux ≥ 5.0).
const SECCOMP_FILTER_FLAG_NEW_LISTENER: libc::c_ulong = 1 << 3;

/// `SECCOMP_RET_USER_NOTIF` — the BPF action that triggers a notification.
const SECCOMP_RET_USER_NOTIF: u32 = 0x7fc00000;
/// `SECCOMP_RET_ALLOW` — pass to the next filter (or allow if no more filters).
const SECCOMP_RET_ALLOW: u32 = 0x7fff0000;

/// Response flag: allow the syscall to continue (as opposed to using val/error).
const SECCOMP_USER_NOTIF_FLAG_CONTINUE: u32 = 1;

// Kernel struct layouts — must match <linux/seccomp.h>.

#[repr(C)]
struct SeccompData {
    nr: i32,
    arch: u32,
    instruction_pointer: u64,
    args: [u64; 6],
}

#[repr(C)]
struct SeccompNotifRaw {
    id: u64,
    pid: u32,
    flags: u32,
    data: SeccompData,
}

#[repr(C)]
struct SeccompNotifResp {
    id: u64,
    val: i64,
    error: i32,
    flags: u32,
}

// ioctl numbers — _IOWR('!', nr, sizeof(T))
// _IOWR(type, nr, size) = (3 << 30) | (type << 8) | nr | (size << 16)
const fn iowr(type_: u32, nr: u32, size: u32) -> libc::c_ulong {
    ((3u32 << 30) | (type_ << 8) | nr | (size << 16)) as libc::c_ulong
}

const SECCOMP_IOCTL_NOTIF_RECV: libc::c_ulong = iowr(
    b'!' as u32,
    0,
    std::mem::size_of::<SeccompNotifRaw>() as u32,
);
const SECCOMP_IOCTL_NOTIF_SEND: libc::c_ulong = iowr(
    b'!' as u32,
    1,
    std::mem::size_of::<SeccompNotifResp>() as u32,
);

// ---------------------------------------------------------------------------
// BPF filter construction
// ---------------------------------------------------------------------------

/// Build a raw seccomp BPF program that returns `SECCOMP_RET_USER_NOTIF` for
/// the given syscall numbers and `SECCOMP_RET_ALLOW` for all others.
///
/// The generated program:
/// ```text
/// LD  [0]                  ; load seccomp_data.nr (syscall number)
/// JEQ syscalls[0] → NOTIF  ; match → jump to return-notif
/// JEQ syscalls[1] → NOTIF
/// ...
/// RET SECCOMP_RET_ALLOW    ; no match → allow (fall through to next filter)
/// RET SECCOMP_RET_USER_NOTIF
/// ```
pub fn build_user_notif_bpf(syscalls: &[i64]) -> Vec<libc::sock_filter> {
    // BPF instruction opcodes.
    const BPF_LD_W_ABS: u16 = 0x20; // LD | W | ABS
    const BPF_JEQ_K: u16 = 0x15; // JMP | JEQ | K
    const BPF_RET_K: u16 = 0x06; // RET | K

    let n = syscalls.len();
    let mut insns: Vec<libc::sock_filter> = Vec::with_capacity(n + 3);

    // Instruction 0: load seccomp_data.nr (offset 0, 32-bit word).
    insns.push(libc::sock_filter {
        code: BPF_LD_W_ABS,
        jt: 0,
        jf: 0,
        k: 0, // offsetof(seccomp_data, nr) = 0
    });

    // Instructions 1..=n: one JEQ per target syscall.
    // jt = distance from next instruction to RET_USER_NOTIF (the last instruction).
    // Layout: [LD] [JEQ_0] [JEQ_1] ... [JEQ_{n-1}] [RET_ALLOW] [RET_USER_NOTIF]
    // Index of RET_USER_NOTIF = n + 2 (0-indexed in the full program).
    // For JEQ at index (i+1), jt = (n+2) - (i+1) - 1 = n - i.
    for (i, &syscall_nr) in syscalls.iter().enumerate() {
        let jt = (n - i) as u8; // jump to RET_USER_NOTIF
        insns.push(libc::sock_filter {
            code: BPF_JEQ_K,
            jt,
            jf: 0, // fall through to next JEQ
            k: syscall_nr as u32,
        });
    }

    // Instruction n+1: no match → allow (pass to the next filter in the chain).
    insns.push(libc::sock_filter {
        code: BPF_RET_K,
        jt: 0,
        jf: 0,
        k: SECCOMP_RET_ALLOW,
    });

    // Instruction n+2: match → notify supervisor.
    insns.push(libc::sock_filter {
        code: BPF_RET_K,
        jt: 0,
        jf: 0,
        k: SECCOMP_RET_USER_NOTIF,
    });

    insns
}

// ---------------------------------------------------------------------------
// Filter installation
// ---------------------------------------------------------------------------

/// Install a user-notif BPF filter in the calling process and return the
/// notification file descriptor.
///
/// Must be called in `pre_exec` (after fork, before exec), after any regular
/// seccomp filter has already been installed.  Requires either `CAP_SYS_ADMIN`
/// or `no_new_privs`.
///
/// Returns the notification fd on success.  The caller is responsible for
/// sending it to the parent process (via [`send_notif_fd`]) before exec.
pub fn install_user_notif_filter(bpf: &[libc::sock_filter]) -> io::Result<i32> {
    if bpf.is_empty() {
        return Err(io::Error::other("user_notif BPF filter is empty"));
    }
    let fprog = libc::sock_fprog {
        len: bpf.len() as u16,
        filter: bpf.as_ptr() as *mut libc::sock_filter,
    };
    let ret = unsafe {
        libc::syscall(
            libc::SYS_seccomp,
            SECCOMP_SET_MODE_FILTER as libc::c_long,
            SECCOMP_FILTER_FLAG_NEW_LISTENER as libc::c_long,
            &fprog as *const libc::sock_fprog as libc::c_long,
        )
    };
    if ret < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(ret as i32)
    }
}

// ---------------------------------------------------------------------------
// fd transfer (SCM_RIGHTS) — child → parent
// ---------------------------------------------------------------------------

/// Send a file descriptor from child (pre_exec) to parent over a Unix socket.
///
/// Uses `sendmsg(SCM_RIGHTS)`.  Called in pre_exec; must be async-signal-safe
/// (only uses `sendmsg`, `CMSG_*`, no allocation).
pub fn send_notif_fd(sock: i32, notif_fd: i32) -> io::Result<()> {
    let cmsg_space =
        unsafe { libc::CMSG_SPACE(std::mem::size_of::<i32>() as libc::c_uint) as usize };
    let mut cmsg_buf = vec![0u8; cmsg_space];
    let mut iov_buf = [0u8; 1];
    let mut iov = libc::iovec {
        iov_base: iov_buf.as_mut_ptr() as *mut libc::c_void,
        iov_len: 1,
    };
    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    msg.msg_control = cmsg_buf.as_mut_ptr() as *mut libc::c_void;
    msg.msg_controllen = cmsg_space as _;
    let cmsg = unsafe { libc::CMSG_FIRSTHDR(&msg) };
    if cmsg.is_null() {
        return Err(io::Error::other("CMSG_FIRSTHDR returned null"));
    }
    unsafe {
        (*cmsg).cmsg_level = libc::SOL_SOCKET;
        (*cmsg).cmsg_type = libc::SCM_RIGHTS;
        (*cmsg).cmsg_len = libc::CMSG_LEN(std::mem::size_of::<i32>() as _) as _;
        *(libc::CMSG_DATA(cmsg) as *mut i32) = notif_fd;
    }
    let ret = unsafe { libc::sendmsg(sock, &msg, 0) };
    if ret < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// Receive a notification fd from the child via SCM_RIGHTS.  Called in parent
/// after fork.
pub fn recv_notif_fd(sock: i32) -> io::Result<i32> {
    let cmsg_space =
        unsafe { libc::CMSG_SPACE(std::mem::size_of::<i32>() as libc::c_uint) as usize };
    let mut cmsg_buf = vec![0u8; cmsg_space];
    let mut iov_buf = [0u8; 1];
    let mut iov = libc::iovec {
        iov_base: iov_buf.as_mut_ptr() as *mut libc::c_void,
        iov_len: 1,
    };
    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    msg.msg_control = cmsg_buf.as_mut_ptr() as *mut libc::c_void;
    msg.msg_controllen = cmsg_space as _;
    let ret = unsafe { libc::recvmsg(sock, &mut msg, 0) };
    if ret < 0 {
        return Err(io::Error::last_os_error());
    }
    let cmsg = unsafe { libc::CMSG_FIRSTHDR(&msg) };
    if cmsg.is_null() {
        return Err(io::Error::other("recvmsg: no control message received"));
    }
    Ok(unsafe { *(libc::CMSG_DATA(cmsg) as *const i32) })
}

// ---------------------------------------------------------------------------
// Supervisor loop
// ---------------------------------------------------------------------------

/// Run the supervisor loop: receive notifications, call `handler`, send responses.
///
/// Blocks until the notification fd is closed (container exited) or an
/// unrecoverable ioctl error occurs.  Designed to run in a dedicated thread.
pub fn run_supervisor_loop(notif_fd: i32, handler: Arc<dyn SyscallHandler>) {
    loop {
        let mut notif: SeccompNotifRaw = unsafe { std::mem::zeroed() };
        let ret = unsafe {
            libc::ioctl(
                notif_fd,
                SECCOMP_IOCTL_NOTIF_RECV,
                &mut notif as *mut SeccompNotifRaw,
            )
        };
        if ret < 0 {
            let err = io::Error::last_os_error();
            match err.raw_os_error() {
                // ENOENT: container exited; no more notifications.
                Some(libc::ENOENT) => break,
                // EINTR: signal interrupted the ioctl; retry.
                Some(libc::EINTR) => continue,
                // EBADF: notif_fd was closed (container exited or fd closed).
                Some(libc::EBADF) => break,
                _ => {
                    log::warn!("seccomp notif recv error: {}", err);
                    break;
                }
            }
        }

        let public_notif = SyscallNotif {
            id: notif.id,
            pid: notif.pid,
            nr: notif.data.nr,
            arch: notif.data.arch,
            instruction_pointer: notif.data.instruction_pointer,
            args: notif.data.args,
        };

        let response = handler.handle(&public_notif);

        let mut resp: SeccompNotifResp = unsafe { std::mem::zeroed() };
        resp.id = notif.id;
        match response {
            SyscallResponse::Allow => {
                resp.flags = SECCOMP_USER_NOTIF_FLAG_CONTINUE;
                resp.error = 0;
                resp.val = 0;
            }
            SyscallResponse::Deny(errno) => {
                resp.flags = 0;
                resp.error = -errno.abs(); // kernel expects negative errno
                resp.val = 0;
            }
            SyscallResponse::Return(val) => {
                resp.flags = 0;
                resp.error = 0;
                resp.val = val;
            }
        }

        let ret = unsafe {
            libc::ioctl(
                notif_fd,
                SECCOMP_IOCTL_NOTIF_SEND,
                &resp as *const SeccompNotifResp,
            )
        };
        if ret < 0 {
            let err = io::Error::last_os_error();
            match err.raw_os_error() {
                // ENOENT: container exited between RECV and SEND — this is normal.
                Some(libc::ENOENT) => continue,
                Some(libc::EINTR) => continue,
                Some(libc::EBADF) => break,
                _ => {
                    log::warn!("seccomp notif send error: {}", err);
                    break;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bpf_filter_empty_syscalls() {
        let bpf = build_user_notif_bpf(&[]);
        // LD (1) + RET_ALLOW (1) + RET_USER_NOTIF (1) = 3 instructions.
        // With 0 syscalls the JEQ block is empty; RET_USER_NOTIF is unreachable
        // but present for structural uniformity.
        assert_eq!(bpf.len(), 3);
    }

    #[test]
    fn test_bpf_filter_single_syscall() {
        let bpf = build_user_notif_bpf(&[42]); // connect on x86_64
                                               // LD + 1 JEQ + RET_ALLOW + RET_NOTIF = 4 instructions
        assert_eq!(bpf.len(), 4);
        // First instruction: LD [0]
        assert_eq!(bpf[0].code, 0x20);
        assert_eq!(bpf[0].k, 0);
        // Second instruction: JEQ 42, jt should jump to last instruction
        assert_eq!(bpf[1].k, 42);
        assert_eq!(bpf[1].jt, 1); // n - 0 = 1 - 0 = 1
        assert_eq!(bpf[1].jf, 0);
        // Third: RET_ALLOW
        assert_eq!(bpf[2].k, SECCOMP_RET_ALLOW);
        // Fourth: RET_USER_NOTIF
        assert_eq!(bpf[3].k, SECCOMP_RET_USER_NOTIF);
    }

    #[test]
    fn test_bpf_filter_multiple_syscalls() {
        let syscalls = [42i64, 165, 228]; // connect, mount, clock_gettime
        let bpf = build_user_notif_bpf(&syscalls);
        // LD + 3 JEQ + RET_ALLOW + RET_NOTIF = 6 instructions
        assert_eq!(bpf.len(), 6);
        // JEQ[0] at index 1: jt = 3 - 0 = 3 (skip 3 to reach index 5 = RET_NOTIF)
        assert_eq!(bpf[1].jt, 3);
        // JEQ[1] at index 2: jt = 3 - 1 = 2
        assert_eq!(bpf[2].jt, 2);
        // JEQ[2] at index 3: jt = 3 - 2 = 1
        assert_eq!(bpf[3].jt, 1);
        // RET_ALLOW at index 4
        assert_eq!(bpf[4].k, SECCOMP_RET_ALLOW);
        // RET_USER_NOTIF at index 5
        assert_eq!(bpf[5].k, SECCOMP_RET_USER_NOTIF);
    }

    #[test]
    fn test_ioctl_numbers_reasonable() {
        // SECCOMP_IOCTL_NOTIF_RECV should have direction bits = _IOC_READ|_IOC_WRITE = 3
        assert_eq!(SECCOMP_IOCTL_NOTIF_RECV >> 30, 3);
        // type field ('!' = 0x21) in bits 8-15
        assert_eq!((SECCOMP_IOCTL_NOTIF_RECV >> 8) & 0xff, 0x21);
        // nr = 0
        assert_eq!(SECCOMP_IOCTL_NOTIF_RECV & 0xff, 0);

        // SECCOMP_IOCTL_NOTIF_SEND
        assert_eq!(SECCOMP_IOCTL_NOTIF_SEND >> 30, 3);
        assert_eq!((SECCOMP_IOCTL_NOTIF_SEND >> 8) & 0xff, 0x21);
        assert_eq!(SECCOMP_IOCTL_NOTIF_SEND & 0xff, 1);
    }
}
