# TTY/PTY Support — Deep Dive

## "But We've Been Starting Shells Already..."

This is true! The CLI (`src/main.rs`) already launches interactive shells:

```rust
Command::new(to_run)
    .stdin(Stdio::Inherit)   // ← inherits your terminal
    .stdout(Stdio::Inherit)
    .stderr(Stdio::Inherit)
    .with_chroot(curdir)
    .spawn()
```

With `Stdio::Inherit`, the container process gets FD 0/1/2 pointing directly at
your real terminal. `isatty()` returns `true`, ash shows a prompt, and you can
type `ls`, run commands, etc. This works today.

So **the interactive shell is already functional**. What PTY adds is not the
ability to run an interactive shell — it's about running it *properly*.

---

## The Three Ways to Run a Shell

### Mode 1: Non-interactive (tests/examples)

```
Remora ──fork──► ash -c "echo hello"
                  │
                  └─► runs command, prints output, exits
```

- Explicit command passed via `-c`
- `stdin` is `/dev/null` or a pipe
- Ash knows it's not interactive: `isatty(0)` returns `false`

### Mode 2: Inherited terminal (current CLI — works today)

```
Your terminal (FD 0/1/2)
       │
       └──► forked into container process directly
             ash sees isatty(0) == true
             ash shows prompt, works interactively
```

- The container process literally shares your terminal's file descriptors
- No intermediary — your keystrokes go directly to the container, its output
  comes directly to your screen
- Works fine for basic use
- **This is what you were using before**

### Mode 3: PTY (what we'd be building)

```
Your terminal ──► PTY master (Remora relay) ──► PTY slave ──► ash
Your screen   ◄── PTY master (Remora relay) ◄── PTY slave ◄── ash
```

- The container gets its own *synthetic* terminal (the PTY slave)
- Remora sits in between, forwarding bytes
- ash still sees `isatty(0) == true`

---

## What PTY Adds Over Inherited Terminal

Since the inherited terminal approach already works interactively, it's worth
being precise about what a PTY actually buys us.

### 1. Process isolation / session management

With inherited terminal, the container process is in the **same session** as
Remora. That means:

- `Ctrl+C` sends `SIGINT` to the entire process group, which may include Remora
  itself and other unrelated processes
- The container can interfere with the terminal state of the parent session
- If the container crashes and corrupts terminal settings, your whole shell is affected

With a PTY, the container gets its own session (`setsid()`). Signals go only to
processes in the container's session.

### 2. Detached / background containers

With inherited terminal, the container is permanently tied to your current
terminal session. You can't:
- Start a container and come back to it later (`docker attach`)
- Run a container on a remote machine and connect to it
- Reconnect if your SSH session drops

With a PTY master fd, the container can outlive your connection. You can
close and reopen the master fd to detach/reattach.

### 3. Window resize

With inherited terminal, `SIGWINCH` goes to the container directly and terminal
size changes work automatically (they already share the same terminal).

With a PTY, Remora must forward resize events explicitly. This is extra work
that the inherited approach gets for free.

### 4. Logging / auditing / multiplexing

With a PTY relay in the middle, Remora can:
- Record all input/output (audit trail)
- Multiplex one container to multiple viewers
- Inject input programmatically

With inherited terminal, bytes flow directly — no opportunity to intercept.

### Summary

| Capability | Inherited terminal | PTY |
|------------|-------------------|-----|
| Interactive shell | ✅ works today | ✅ |
| Colors, readline | ✅ works today | ✅ |
| vim, htop, etc. | ✅ works today | ✅ |
| Session isolation | ❌ | ✅ |
| Detach/reattach | ❌ | ✅ |
| Audit logging | ❌ | ✅ |
| Remote containers | ❌ | ✅ |

**The inherited terminal approach is already good enough for local development.**
PTY becomes important for production container runtimes that need proper
isolation, remote access, and lifecycle management.

---

## What a PTY Actually Is

A PTY (pseudoterminal) is a **kernel-managed pair of file descriptors** that
simulate a hardware serial terminal. It has two ends:

```
┌─────────────────────────────────────────────────────┐
│                    Linux Kernel                     │
│                                                     │
│   PTY Master fd          PTY Slave fd               │
│   (held by Remora)       (given to container)       │
│        │                       │                    │
│        └───────────────────────┘                    │
│           bidirectional byte pipe                   │
│           + terminal discipline (line editing,      │
│             signal generation, echo, etc.)          │
└─────────────────────────────────────────────────────┘
```

**PTY master**: Remora holds this. It's the "outside" of the terminal.
Everything written to master appears as input to the slave. Everything the
slave writes appears as output from master.

**PTY slave**: The container process's stdin/stdout/stderr are all pointed at
this. From the container's perspective, it thinks it's talking to a real
hardware terminal. `isatty()` returns `true`.

**Terminal discipline**: The kernel layer between master and slave. It handles:
- Echo (typing `a` makes `a` appear on screen)
- Line buffering (your edits stay local until you press Enter)
- Signal generation (`Ctrl+C` → `SIGINT`, `Ctrl+Z` → `SIGTSTP`)
- Special characters (`Ctrl+D` → EOF, `Ctrl+\` → `SIGQUIT`)

---

## The Relay Loop

With a PTY, Remora becomes a **relay** between your actual terminal and the
container's PTY:

```
Your terminal (raw mode)
       │  ▲
       │  │  raw bytes
       ▼  │
  Remora relay loop          ← poll() on two fds simultaneously
       │  ▲
       │  │  raw bytes
       ▼  │
   PTY master fd
       │  ▲
       │  │  (kernel PTY discipline)
       ▼  │
   PTY slave fd
       │  ▲
       │  │
       ▼  │
  ash (inside container)
```

The relay loop does exactly two things:
1. **stdin → master**: bytes you type go to the container
2. **master → stdout**: bytes the container produces come to your screen

### Why Raw Mode on the Host Terminal

By default your terminal is in "cooked" mode — it buffers input until you press
Enter, handles backspace locally, etc. That's fine for normal use.

But with a PTY relay, you want every keypress to go to the container
*immediately and unmodified*. The PTY slave's terminal discipline (inside the
kernel) handles echo and line editing for the container. If your host terminal
also does this, you get double-echo and broken behavior.

So Remora must:
1. Put the host terminal into raw mode before the relay starts
2. Restore it to cooked mode when the container exits

If Remora crashes without restoring the terminal, your shell is left in raw
mode — no echo, broken Enter key. You'd need to type `reset` blind to recover.

---

## Window Resize (SIGWINCH)

When you resize your terminal window, the kernel sends `SIGWINCH`
(window change) to the foreground process. Remora must:

1. Catch `SIGWINCH`
2. Query the new terminal size with `TIOCGWINSZ` ioctl
3. Forward the new size to the PTY master with `TIOCSWINSZ` ioctl

Without this, programs like `vim` and `htop` don't redraw when you resize the
window.

---

## Signal Handling

With the current **inherited terminal** approach, signals work — but they're
broad. Because the container process is in the same session as Remora and your
shell, `Ctrl+C` sends `SIGINT` to the entire foreground process group, which
includes Remora itself. For simple cases this is fine. For complex cases (nested
process groups, job control inside the container), it breaks down.

With a PTY, `Ctrl+C` goes to the PTY slave's terminal discipline, which
generates `SIGINT` to the **foreground process group of the container's own
session** — precisely scoped, with no bleed into the parent session. Job control
(`Ctrl+Z`, `bg`, `fg`) works correctly within the container's own session.

---

## Implementation Plan

### What needs to be built

**1. PTY allocation**
```rust
// openpty() from libc — allocates master/slave pair
let (master_fd, slave_fd) = openpty(None, None)?;
```

**2. Child setup** (in pre_exec, before exec)
```rust
// Create a new session (detach from parent's controlling terminal)
setsid();
// Make the slave the controlling terminal of this session
ioctl(slave_fd, TIOCSCTTY, 0);
// Point stdin/stdout/stderr at the slave
dup2(slave_fd, 0);  // stdin
dup2(slave_fd, 1);  // stdout
dup2(slave_fd, 2);  // stderr
close(slave_fd);    // no longer need original fd
close(master_fd);   // child doesn't use master
```

**3. Parent: put host terminal in raw mode**
```rust
let original_termios = tcgetattr(stdin)?;
let mut raw = original_termios.clone();
cfmakeraw(&mut raw);
tcsetattr(stdin, TCSANOW, &raw)?;
// Restore on exit (critical!)
```

**4. Parent: relay loop**
```rust
loop {
    poll([master_fd, stdin], -1);
    if stdin readable  { copy stdin  → master_fd }
    if master readable { copy master → stdout    }
    if child exited    { break }
}
```

**5. SIGWINCH forwarding**
```rust
signal(SIGWINCH, handler);
// In handler:
let size = tcgetwinsize(stdout)?;
tcsetwinsize(master_fd, &size)?;
```

**6. Cleanup**
```rust
tcsetattr(stdin, TCSANOW, &original_termios)?;  // restore terminal
```

### API Design (Proposed)

```rust
// Current (non-interactive):
Command::new("/bin/ash")
    .stdin(Stdio::Null)
    .spawn()?

// With PTY (interactive):
Command::new("/bin/ash")
    .with_pty(true)   // ← new method
    .spawn()?
// Remora runs the relay loop internally, blocks until container exits
```

Or a separate method:

```rust
Command::new("/bin/ash")
    .with_pty(true)
    .spawn_interactive()?  // blocks, runs relay loop, restores terminal
```

---

## What PTY Implementation Adds to Remora

We already have interactive shells via `Stdio::Inherit`. PTY gives us proper
terminal semantics:

```bash
# These already work via Stdio::Inherit:
sudo remora -r alpine-rootfs -e /bin/ash -u 0 -g 0
# → interactive ash shell, ls works, colors work

# PTY adds proper isolation — the container gets its own session:
# → Ctrl+C only kills processes inside the container
# → terminal corruption inside can't affect your outer shell
# → groundwork for detach/reattach in the future
```

More concretely, PTY is what makes it safe and correct to run TUI programs
inside the container when you care about signal isolation:

```bash
# These work today but signals bleed across session boundaries:
# vim, htop, python REPL, etc.

# With PTY, they work with proper session isolation.
```

---

## Related Remora Docs

- `CLAUDE.md` — development guidelines
- `READONLY_ROOTFS.md` — read-only rootfs implementation
- `SECCOMP_DEEP_DIVE.md` — syscall filtering
- `ROADMAP.md` — overall development plan
