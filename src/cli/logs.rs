//! `remora logs` — print or follow a container's output.

use super::read_state;
use std::io::{self, Write};

pub fn cmd_logs(name: &str, follow: bool) -> Result<(), Box<dyn std::error::Error>> {
    let state = read_state(name)
        .map_err(|_| format!("no container named '{}'", name))?;

    let stdout_log = state.stdout_log.as_deref().unwrap_or("");
    let stderr_log = state.stderr_log.as_deref().unwrap_or("");

    if stdout_log.is_empty() && stderr_log.is_empty() {
        return Err(format!(
            "container '{}' has no log files (was it started with --detach?)",
            name
        ).into());
    }

    if follow {
        // Print what exists so far, then poll for new content.
        let mut stdout_pos = print_file(stdout_log)?;
        let mut stderr_pos = print_file(stderr_log)?;

        loop {
            std::thread::sleep(std::time::Duration::from_millis(200));
            stdout_pos = tail_file(stdout_log, stdout_pos)?;
            stderr_pos = tail_file(stderr_log, stderr_pos)?;

            // Stop following if the container has exited.
            if let Ok(s) = read_state(name) {
                if s.status == super::ContainerStatus::Exited {
                    // Drain any remaining output.
                    let _ = tail_file(stdout_log, stdout_pos);
                    let _ = tail_file(stderr_log, stderr_pos);
                    break;
                }
            }
        }
    } else {
        print_file(stdout_log)?;
        print_file(stderr_log)?;
    }

    Ok(())
}

/// Print entire file to stdout, return file size.
fn print_file(path: &str) -> io::Result<u64> {
    if path.is_empty() { return Ok(0); }
    match std::fs::read(path) {
        Ok(data) => {
            io::stdout().write_all(&data)?;
            Ok(data.len() as u64)
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(0),
        Err(e) => Err(e),
    }
}

/// Print new content from `path` since `offset`, return new offset.
fn tail_file(path: &str, offset: u64) -> io::Result<u64> {
    if path.is_empty() { return Ok(offset); }
    use std::io::{Read, Seek, SeekFrom};
    match std::fs::File::open(path) {
        Ok(mut f) => {
            let size = f.seek(SeekFrom::End(0))?;
            if size <= offset {
                return Ok(offset);
            }
            f.seek(SeekFrom::Start(offset))?;
            let mut buf = Vec::new();
            f.read_to_end(&mut buf)?;
            io::stdout().write_all(&buf)?;
            Ok(size)
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(offset),
        Err(e) => Err(e),
    }
}
