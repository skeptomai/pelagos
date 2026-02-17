//! PTY (pseudoterminal) relay for interactive container sessions.
//!
//! When a container is spawned with `spawn_interactive()`, the parent process
//! runs a relay loop that forwards bytes between the user's terminal and the
//! container's PTY slave. This gives the container proper session isolation
//! while preserving a fully interactive terminal experience.

use std::io;
use std::os::fd::{AsRawFd, OwnedFd, RawFd};
use std::sync::atomic::{AtomicBool, Ordering};

use nix::poll::{poll, PollFd, PollFlags, PollTimeout};
use nix::sys::termios::{self, SetArg, Termios};

use crate::container::{Child, ExitStatus};

/// Set when the kernel delivers SIGWINCH (terminal resize).
static WINCH_RECEIVED: AtomicBool = AtomicBool::new(false);

/// RAII guard that restores terminal settings on drop.
///
/// On creation, saves the current termios and switches to raw mode.
/// On drop (including panic / early return), restores the saved settings.
/// This ensures the user's shell is never left in raw mode.
struct TerminalGuard {
    fd: RawFd,
    original: Termios,
}

impl TerminalGuard {
    fn enter_raw(fd: RawFd) -> io::Result<Self> {
        let borrowed = unsafe { std::os::fd::BorrowedFd::borrow_raw(fd) };
        let original = termios::tcgetattr(borrowed).map_err(io::Error::from)?;
        let mut raw = original.clone();
        termios::cfmakeraw(&mut raw);
        termios::tcsetattr(borrowed, SetArg::TCSANOW, &raw).map_err(io::Error::from)?;
        Ok(TerminalGuard { fd, original })
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let borrowed = unsafe { std::os::fd::BorrowedFd::borrow_raw(self.fd) };
        let _ = termios::tcsetattr(borrowed, SetArg::TCSANOW, &self.original);
    }
}

/// Handle returned by `Command::spawn_interactive()`.
///
/// Call `run()` to start the relay loop, which blocks until the container exits.
pub struct InteractiveSession {
    /// PTY master fd — Remora reads/writes this to communicate with the container.
    pub(crate) master: OwnedFd,
    /// The spawned container process.
    pub child: Child,
}

impl InteractiveSession {
    /// Run the PTY relay loop.
    ///
    /// - Puts the host terminal into raw mode
    /// - Forwards bytes between the user's stdin/stdout and the PTY master
    /// - Forwards window resize events (SIGWINCH) to the PTY
    /// - Restores the terminal when the container exits
    ///
    /// Blocks until the container process exits. Returns its exit status.
    pub fn run(mut self) -> io::Result<ExitStatus> {
        // If stdin isn't a terminal (e.g. CI, piped input), skip raw mode
        // and just relay bytes as-is.
        let stdin_is_tty = unsafe { libc::isatty(libc::STDIN_FILENO) } == 1;

        // Sync the PTY's window size with the current terminal before starting
        if stdin_is_tty {
            if let Ok(ws) = get_winsize(libc::STDOUT_FILENO) {
                let _ = set_winsize(self.master.as_raw_fd(), &ws);
            }
        }

        // Install SIGWINCH handler so resizes are forwarded to the container
        install_sigwinch_handler();

        // Enter raw mode (restored automatically by Drop)
        let _guard = if stdin_is_tty {
            Some(TerminalGuard::enter_raw(libc::STDIN_FILENO)?)
        } else {
            None
        };

        relay_loop(self.master.as_raw_fd())?;

        self.child.wait().map_err(|e| io::Error::other(e.to_string()))
    }
}

/// Core relay loop: forwards bytes between stdin/stdout and the PTY master.
fn relay_loop(master_fd: RawFd) -> io::Result<()> {
    let mut buf = [0u8; 4096];

    loop {
        // Handle any pending window resize before blocking on poll
        if WINCH_RECEIVED.swap(false, Ordering::Relaxed) {
            if let Ok(ws) = get_winsize(libc::STDOUT_FILENO) {
                let _ = set_winsize(master_fd, &ws);
            }
        }

        let stdin_bfd = unsafe { std::os::fd::BorrowedFd::borrow_raw(libc::STDIN_FILENO) };
        let master_bfd = unsafe { std::os::fd::BorrowedFd::borrow_raw(master_fd) };

        let mut fds = [
            PollFd::new(stdin_bfd, PollFlags::POLLIN),
            PollFd::new(master_bfd, PollFlags::POLLIN),
        ];

        // Short timeout so we can check WINCH_RECEIVED even if no I/O arrives
        // PollTimeout::from(100i16) = 100ms
        let timeout = PollTimeout::from(100u16);

        match poll(&mut fds, timeout) {
            Ok(0) => continue, // timeout — loop to check SIGWINCH flag
            Ok(_) => {}
            Err(nix::errno::Errno::EINTR) => continue, // interrupted by signal, loop
            Err(e) => return Err(io::Error::from(e)),
        }

        // stdin → master (user keystrokes go into the container)
        if let Some(revents) = fds[0].revents() {
            if revents.contains(PollFlags::POLLIN) {
                let n = unsafe {
                    libc::read(libc::STDIN_FILENO, buf.as_mut_ptr() as *mut _, buf.len())
                };
                if n > 0 {
                    unsafe {
                        libc::write(master_fd, buf.as_ptr() as *const _, n as usize);
                    }
                }
            }
        }

        // master → stdout (container output comes to the user's screen)
        if let Some(revents) = fds[1].revents() {
            if revents.contains(PollFlags::POLLIN) {
                let n = unsafe {
                    libc::read(master_fd, buf.as_mut_ptr() as *mut _, buf.len())
                };
                if n > 0 {
                    unsafe {
                        libc::write(libc::STDOUT_FILENO, buf.as_ptr() as *const _, n as usize);
                    }
                } else {
                    // n == 0 or n < 0: slave closed its end (container exited)
                    // On Linux, reading from a PTY master after all slaves close returns EIO
                    break;
                }
            }
            // POLLHUP: slave side closed — container exited
            if revents.contains(PollFlags::POLLHUP) {
                break;
            }
        }
    }

    Ok(())
}

/// Read the current terminal window size.
fn get_winsize(fd: RawFd) -> io::Result<libc::winsize> {
    let mut ws = libc::winsize {
        ws_row: 24,
        ws_col: 80,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let ret = unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, &mut ws) };
    if ret < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(ws)
}

/// Set the window size on a PTY master fd.
fn set_winsize(fd: RawFd, ws: &libc::winsize) -> io::Result<()> {
    let ret = unsafe { libc::ioctl(fd, libc::TIOCSWINSZ, ws) };
    if ret < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Install a SIGWINCH signal handler that sets the `WINCH_RECEIVED` flag.
///
/// The relay loop checks this flag each iteration and forwards the new
/// terminal size to the PTY master.
fn install_sigwinch_handler() {
    use nix::sys::signal::{signal, SigHandler, Signal};

    extern "C" fn sigwinch_handler(_: libc::c_int) {
        WINCH_RECEIVED.store(true, Ordering::Relaxed);
    }

    unsafe {
        let _ = signal(Signal::SIGWINCH, SigHandler::Handler(sigwinch_handler));
    }
}
