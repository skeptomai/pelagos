//! Subordinate UID/GID mapping support for rootless containers.
//!
//! On Linux, unprivileged processes can only write a single-line uid_map mapping
//! their own UID. Multi-range mappings require `CAP_SETUID`/`CAP_SETGID`, provided
//! by the `newuidmap`/`newgidmap` helpers (from the `shadow-utils` / `uidmap` package).
//!
//! This module parses `/etc/subuid` and `/etc/subgid` to discover the subordinate
//! ranges allocated to the current user, and invokes the helpers to write the maps.

use crate::container::{GidMap, UidMap};
use std::io;
use std::path::Path;

/// A subordinate ID range from `/etc/subuid` or `/etc/subgid`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubIdRange {
    /// First host UID/GID in the range.
    pub start: u32,
    /// Number of consecutive IDs.
    pub count: u32,
}

/// Parse `/etc/subuid` or `/etc/subgid` for the given username or numeric UID.
///
/// Each line has the format `<name_or_uid>:<start>:<count>`.
/// Returns all ranges matching either the username string or the UID as a string.
pub fn parse_subid_file(path: &Path, username: &str, uid: u32) -> io::Result<Vec<SubIdRange>> {
    let contents = std::fs::read_to_string(path)?;
    Ok(parse_subid_contents(&contents, username, uid))
}

/// Parse subid file contents (testable without filesystem).
fn parse_subid_contents(contents: &str, username: &str, uid: u32) -> Vec<SubIdRange> {
    let uid_str = uid.to_string();
    let mut ranges = Vec::new();

    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let parts: Vec<&str> = line.splitn(3, ':').collect();
        if parts.len() != 3 {
            continue;
        }
        let name = parts[0];
        if name != username && name != uid_str {
            continue;
        }
        if let (Ok(start), Ok(count)) = (parts[1].parse::<u32>(), parts[2].parse::<u32>()) {
            ranges.push(SubIdRange { start, count });
        }
    }

    ranges
}

/// Check if `newuidmap` is available on PATH.
pub fn has_newuidmap() -> bool {
    which("newuidmap")
}

/// Check if `newgidmap` is available on PATH.
pub fn has_newgidmap() -> bool {
    which("newgidmap")
}

fn which(name: &str) -> bool {
    std::process::Command::new("which")
        .arg(name)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Get the current username and primary GID by looking up the real UID in `/etc/passwd`.
///
/// Returns `(username, primary_gid)`.
pub fn current_user_info() -> io::Result<(String, u32)> {
    let uid = unsafe { libc::getuid() };
    let contents = std::fs::read_to_string("/etc/passwd")?;
    for line in contents.lines() {
        let parts: Vec<&str> = line.splitn(7, ':').collect();
        if parts.len() >= 4 {
            if let Ok(file_uid) = parts[2].parse::<u32>() {
                if file_uid == uid {
                    let pw_gid = parts[3].parse::<u32>().unwrap_or(uid);
                    return Ok((parts[0].to_string(), pw_gid));
                }
            }
        }
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        format!("no /etc/passwd entry for uid {}", uid),
    ))
}

/// Get the current username by looking up the real UID in `/etc/passwd`.
pub fn current_username() -> io::Result<String> {
    current_user_info().map(|(name, _)| name)
}

/// Returns `true` when `newuidmap`/`newgidmap` will accept the current process.
///
/// Both helpers check that the target process's effective GID matches the primary
/// GID from `/etc/passwd`.  This fails when the caller is running inside a
/// `newgrp <group>` shell — the effective GID is the new group, not `pw_gid`.
/// In that case we must fall back to single-UID direct mapping.
pub fn newuidmap_will_work() -> bool {
    let Ok((_, pw_gid)) = current_user_info() else {
        return false;
    };
    let egid = unsafe { libc::getegid() };
    egid == pw_gid
}

/// Run `newuidmap <pid> <inside> <outside> <count> ...` to write UID maps.
pub fn apply_uid_map(pid: u32, maps: &[UidMap]) -> io::Result<()> {
    let mut args: Vec<String> = vec![pid.to_string()];
    for m in maps {
        args.push(m.inside.to_string());
        args.push(m.outside.to_string());
        args.push(m.count.to_string());
    }

    let status = std::process::Command::new("newuidmap")
        .args(&args)
        .status()
        .map_err(|e| io::Error::new(e.kind(), format!("newuidmap: {}", e)))?;

    if !status.success() {
        return Err(io::Error::other(format!(
            "newuidmap exited with {}",
            status
        )));
    }
    Ok(())
}

/// Run `newgidmap <pid> <inside> <outside> <count> ...` to write GID maps.
pub fn apply_gid_map(pid: u32, maps: &[GidMap]) -> io::Result<()> {
    let mut args: Vec<String> = vec![pid.to_string()];
    for m in maps {
        args.push(m.inside.to_string());
        args.push(m.outside.to_string());
        args.push(m.count.to_string());
    }

    let status = std::process::Command::new("newgidmap")
        .args(&args)
        .status()
        .map_err(|e| io::Error::new(e.kind(), format!("newgidmap: {}", e)))?;

    if !status.success() {
        return Err(io::Error::other(format!(
            "newgidmap exited with {}",
            status
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_subid_single_range() {
        let contents = "cb:100000:65536\n";
        let ranges = parse_subid_contents(contents, "cb", 1000);
        assert_eq!(
            ranges,
            vec![SubIdRange {
                start: 100000,
                count: 65536
            }]
        );
    }

    #[test]
    fn test_parse_subid_multiple_ranges() {
        let contents = "cb:100000:65536\ncb:200000:1000\n";
        let ranges = parse_subid_contents(contents, "cb", 1000);
        assert_eq!(ranges.len(), 2);
        assert_eq!(
            ranges[0],
            SubIdRange {
                start: 100000,
                count: 65536
            }
        );
        assert_eq!(
            ranges[1],
            SubIdRange {
                start: 200000,
                count: 1000
            }
        );
    }

    #[test]
    fn test_parse_subid_numeric_uid() {
        let contents = "1000:100000:65536\n";
        let ranges = parse_subid_contents(contents, "cb", 1000);
        assert_eq!(
            ranges,
            vec![SubIdRange {
                start: 100000,
                count: 65536
            }]
        );
    }

    #[test]
    fn test_parse_subid_no_match() {
        let contents = "alice:100000:65536\nbob:200000:65536\n";
        let ranges = parse_subid_contents(contents, "cb", 1000);
        assert!(ranges.is_empty());
    }

    #[test]
    fn test_parse_subid_comments_blanks() {
        let contents = "# comment\n\ncb:100000:65536\n  \nbad_line\n";
        let ranges = parse_subid_contents(contents, "cb", 1000);
        assert_eq!(
            ranges,
            vec![SubIdRange {
                start: 100000,
                count: 65536
            }]
        );
    }

    #[test]
    fn test_current_username() {
        // This test runs on the real system.
        if let Ok(name) = current_username() {
            assert!(!name.is_empty(), "username should not be empty");
        }
    }

    #[test]
    fn test_has_newuidmap() {
        // Just verify it returns a bool without panicking.
        let _ = has_newuidmap();
    }
}
