//! Build engine for creating OCI images from Remfiles (simplified Dockerfiles).
//!
//! The build process reads a Remfile, executes each instruction in sequence,
//! and produces an `ImageManifest` stored in the local image store.

use crate::container::{Command, Namespace, Stdio};
use crate::image::{self, ImageConfig, ImageManifest};
use crate::network::NetworkMode;
use std::io;
use std::path::Path;

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum BuildError {
    #[error("parse error at line {line}: {message}")]
    Parse { line: usize, message: String },

    #[error("FROM must be the first instruction")]
    MissingFrom,

    #[error("image '{0}' not found locally; run 'remora image pull {0}' first")]
    ImageNotFound(String),

    #[error("RUN command failed with exit code {0}")]
    RunFailed(i32),

    #[error("container error: {0}")]
    Container(#[from] crate::container::Error),

    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
}

// ---------------------------------------------------------------------------
// Instruction AST
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Instruction {
    From(String),
    Run(String),
    Copy { src: String, dest: String },
    Cmd(Vec<String>),
    Entrypoint(Vec<String>),
    Env { key: String, value: String },
    Workdir(String),
    Expose(u16),
    Label { key: String, value: String },
    User(String),
}

// ---------------------------------------------------------------------------
// Parser
// ---------------------------------------------------------------------------

/// Parse a Remfile into a list of instructions.
pub fn parse_remfile(content: &str) -> Result<Vec<Instruction>, BuildError> {
    let mut instructions = Vec::new();
    let mut lines = content.lines().enumerate().peekable();

    while let Some((line_num, raw_line)) = lines.next() {
        let line_num = line_num + 1; // 1-indexed for error messages
        let mut line = raw_line.trim().to_string();

        // Skip blank lines and comments.
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        // Handle continuation lines (trailing backslash).
        while line.ends_with('\\') {
            line.pop(); // remove backslash
            if let Some((_, next)) = lines.next() {
                line.push(' ');
                line.push_str(next.trim());
            }
        }

        let (keyword, rest) = split_instruction(&line);
        let rest = rest.trim();

        match keyword.to_ascii_uppercase().as_str() {
            "FROM" => {
                if rest.is_empty() {
                    return Err(BuildError::Parse {
                        line: line_num,
                        message: "FROM requires an image reference".to_string(),
                    });
                }
                instructions.push(Instruction::From(rest.to_string()));
            }
            "RUN" => {
                if rest.is_empty() {
                    return Err(BuildError::Parse {
                        line: line_num,
                        message: "RUN requires a command".to_string(),
                    });
                }
                instructions.push(Instruction::Run(rest.to_string()));
            }
            "COPY" => {
                let parts: Vec<&str> = rest.splitn(2, char::is_whitespace).collect();
                if parts.len() < 2 {
                    return Err(BuildError::Parse {
                        line: line_num,
                        message: "COPY requires <src> <dest>".to_string(),
                    });
                }
                instructions.push(Instruction::Copy {
                    src: parts[0].to_string(),
                    dest: parts[1].trim().to_string(),
                });
            }
            "CMD" => {
                let cmd = parse_cmd_value(rest).map_err(|msg| BuildError::Parse {
                    line: line_num,
                    message: msg,
                })?;
                instructions.push(Instruction::Cmd(cmd));
            }
            "ENV" => {
                let (key, value) = parse_env_value(rest).ok_or_else(|| BuildError::Parse {
                    line: line_num,
                    message: "ENV requires KEY=VALUE or KEY VALUE".to_string(),
                })?;
                instructions.push(Instruction::Env { key, value });
            }
            "WORKDIR" => {
                if rest.is_empty() {
                    return Err(BuildError::Parse {
                        line: line_num,
                        message: "WORKDIR requires a path".to_string(),
                    });
                }
                instructions.push(Instruction::Workdir(rest.to_string()));
            }
            "ENTRYPOINT" => {
                let ep = parse_cmd_value(rest).map_err(|msg| BuildError::Parse {
                    line: line_num,
                    message: msg,
                })?;
                instructions.push(Instruction::Entrypoint(ep));
            }
            "EXPOSE" => {
                let port: u16 = rest
                    .split('/')
                    .next()
                    .unwrap_or(rest)
                    .parse()
                    .map_err(|_| BuildError::Parse {
                        line: line_num,
                        message: format!("invalid port number: {}", rest),
                    })?;
                instructions.push(Instruction::Expose(port));
            }
            "LABEL" => {
                let (key, value) = parse_label_value(rest).ok_or_else(|| BuildError::Parse {
                    line: line_num,
                    message: "LABEL requires KEY=VALUE".to_string(),
                })?;
                instructions.push(Instruction::Label { key, value });
            }
            "USER" => {
                if rest.is_empty() {
                    return Err(BuildError::Parse {
                        line: line_num,
                        message: "USER requires a user spec (e.g. 1000 or 1000:1000)".to_string(),
                    });
                }
                instructions.push(Instruction::User(rest.to_string()));
            }
            other => {
                return Err(BuildError::Parse {
                    line: line_num,
                    message: format!("unknown instruction: {}", other),
                });
            }
        }
    }

    Ok(instructions)
}

/// Split a line into (keyword, rest).
fn split_instruction(line: &str) -> (&str, &str) {
    match line.split_once(char::is_whitespace) {
        Some((kw, rest)) => (kw, rest),
        None => (line, ""),
    }
}

/// Parse CMD value: supports JSON array `["a", "b"]` or shell form `a b c`.
fn parse_cmd_value(rest: &str) -> Result<Vec<String>, String> {
    let trimmed = rest.trim();
    if trimmed.starts_with('[') {
        // JSON array form: ["cmd", "arg1", "arg2"]
        let parsed: Vec<String> =
            serde_json::from_str(trimmed).map_err(|e| format!("invalid CMD JSON: {}", e))?;
        if parsed.is_empty() {
            return Err("CMD cannot be empty".to_string());
        }
        Ok(parsed)
    } else {
        // Shell form: wrap in /bin/sh -c
        if trimmed.is_empty() {
            return Err("CMD requires a command".to_string());
        }
        Ok(vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            trimmed.to_string(),
        ])
    }
}

/// Parse LABEL: supports `KEY=VALUE` or `KEY="quoted value"`.
fn parse_label_value(rest: &str) -> Option<(String, String)> {
    let trimmed = rest.trim();
    if trimmed.is_empty() {
        return None;
    }
    let (k, v) = trimmed.split_once('=')?;
    let k = k.trim();
    let v = v.trim();
    // Strip surrounding quotes if present.
    let v =
        if (v.starts_with('"') && v.ends_with('"')) || (v.starts_with('\'') && v.ends_with('\'')) {
            &v[1..v.len() - 1]
        } else {
            v
        };
    Some((k.to_string(), v.to_string()))
}

/// Parse ENV: supports `KEY=VALUE` or `KEY VALUE`.
fn parse_env_value(rest: &str) -> Option<(String, String)> {
    let trimmed = rest.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some((k, v)) = trimmed.split_once('=') {
        Some((k.to_string(), v.to_string()))
    } else if let Some((k, v)) = trimmed.split_once(char::is_whitespace) {
        Some((k.to_string(), v.trim().to_string()))
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Build execution
// ---------------------------------------------------------------------------

/// Execute a parsed Remfile and produce a tagged image.
///
/// `context_dir` is the directory context for COPY instructions.
/// `tag` is the image reference (e.g. `"myapp:latest"`).
/// `network_mode` is the network for RUN steps (bridge for root, pasta for rootless).
/// `use_cache` enables layer caching: if a RUN instruction's cache key matches
/// a previously built layer, that layer is reused without re-executing the command.
/// A cache miss invalidates all subsequent steps (same as Docker).
pub fn execute_build(
    instructions: &[Instruction],
    context_dir: &Path,
    tag: &str,
    network_mode: NetworkMode,
    use_cache: bool,
) -> Result<ImageManifest, BuildError> {
    if instructions.is_empty() {
        return Err(BuildError::MissingFrom);
    }

    // First instruction must be FROM.
    let base_ref = match &instructions[0] {
        Instruction::From(r) => r.clone(),
        _ => return Err(BuildError::MissingFrom),
    };

    // Load base image.
    let normalised = normalise_image_reference(&base_ref);
    let base_manifest =
        image::load_image(&normalised).map_err(|_| BuildError::ImageNotFound(base_ref.clone()))?;

    // Accumulated state.
    let mut layers: Vec<String> = base_manifest.layers.clone();
    let mut config = base_manifest.config.clone();
    let total = instructions.len();
    // Once a cache miss occurs, all subsequent steps run uncached.
    let mut cache_active = use_cache;

    for (idx, instr) in instructions.iter().enumerate() {
        let step = idx + 1;
        match instr {
            Instruction::From(ref r) => {
                eprintln!("Step {}/{}: FROM {}", step, total, r);
            }
            Instruction::Run(ref cmd_text) => {
                // Build cache: hash(parent_layer_digest + instruction) → cached layer.
                let cache_key = if cache_active {
                    Some(compute_cache_key(&layers, &format!("RUN {}", cmd_text)))
                } else {
                    None
                };
                if let Some(ref key) = cache_key {
                    if let Some(cached_digest) = cache_lookup(key) {
                        eprintln!("Step {}/{}: RUN {} (cached)", step, total, cmd_text);
                        layers.push(cached_digest);
                        continue;
                    }
                }
                // Cache miss — invalidate for all subsequent steps.
                cache_active = false;
                eprintln!("Step {}/{}: RUN {}", step, total, cmd_text);
                let new_digest = execute_run(cmd_text, &layers, &config, network_mode.clone())?;
                if let Some(ref digest) = new_digest {
                    if let Some(ref key) = cache_key {
                        cache_store(key, digest);
                    }
                    layers.push(digest.clone());
                }
            }
            Instruction::Copy { ref src, ref dest } => {
                // COPY always invalidates cache (context content may have changed).
                cache_active = false;
                eprintln!("Step {}/{}: COPY {} {}", step, total, src, dest);
                let digest = execute_copy(src, dest, context_dir)?;
                layers.push(digest);
            }
            Instruction::Cmd(ref args) => {
                eprintln!("Step {}/{}: CMD {:?}", step, total, args);
                config.cmd = args.clone();
            }
            Instruction::Env { ref key, ref value } => {
                eprintln!("Step {}/{}: ENV {}={}", step, total, key, value);
                // Remove any existing entry for this key, then add.
                config.env.retain(|e| !e.starts_with(&format!("{}=", key)));
                config.env.push(format!("{}={}", key, value));
            }
            Instruction::Workdir(ref path) => {
                eprintln!("Step {}/{}: WORKDIR {}", step, total, path);
                config.working_dir = path.clone();
            }
            Instruction::Entrypoint(ref args) => {
                eprintln!("Step {}/{}: ENTRYPOINT {:?}", step, total, args);
                config.entrypoint = args.clone();
            }
            Instruction::Expose(port) => {
                eprintln!("Step {}/{}: EXPOSE {}", step, total, port);
                // Metadata only — no layer created.
            }
            Instruction::Label { ref key, ref value } => {
                eprintln!("Step {}/{}: LABEL {}={}", step, total, key, value);
                config.labels.insert(key.clone(), value.clone());
            }
            Instruction::User(ref user) => {
                eprintln!("Step {}/{}: USER {}", step, total, user);
                config.user = user.clone();
            }
        }
    }

    // Compute a digest for the final manifest.
    let digest = compute_manifest_digest(&layers);

    // Append :latest if the tag has no version/digest, matching OCI convention.
    let reference = if !tag.contains(':') && !tag.contains('@') {
        format!("{}:latest", tag)
    } else {
        tag.to_string()
    };

    let manifest = ImageManifest {
        reference,
        digest,
        layers,
        config,
    };

    image::save_image(&manifest)?;

    Ok(manifest)
}

/// Execute a RUN instruction: spawn a container, wait, capture upper layer.
fn execute_run(
    cmd_text: &str,
    current_layers: &[String],
    config: &ImageConfig,
    network_mode: NetworkMode,
) -> Result<Option<String>, BuildError> {
    let layer_dirs = current_layers
        .iter()
        .rev()
        .map(|d| image::layer_dir(d))
        .collect::<Vec<_>>();

    // Note: with_image_layers sets Namespace::MOUNT internally, so we must
    // add UTS|IPC *before* it (with_namespaces does assignment, not |=).
    let mut cmd = Command::new("/bin/sh")
        .args(["-c", cmd_text])
        .with_namespaces(Namespace::UTS | Namespace::IPC)
        .with_image_layers(layer_dirs)
        .stdin(Stdio::Null)
        .stdout(Stdio::Inherit)
        .stderr(Stdio::Inherit);

    // Apply accumulated environment.
    for env_str in &config.env {
        if let Some((k, v)) = env_str.split_once('=') {
            cmd = cmd.env(k, v);
        }
    }

    // Apply accumulated workdir.
    if !config.working_dir.is_empty() {
        cmd = cmd.with_cwd(&config.working_dir);
    }

    // Apply network mode for package installs etc.
    // Bridge mode needs NAT (MASQUERADE) for outbound internet and DNS for
    // hostname resolution.  Pasta provides both natively.
    cmd = cmd.with_network(network_mode.clone());
    if network_mode.is_bridge() {
        cmd = cmd.with_nat().with_dns(&["8.8.8.8", "1.1.1.1"]);
    }

    let mut child = cmd.spawn()?;
    let (status, overlay_base) = child.wait_preserve_overlay()?;

    if !status.success() {
        // Clean up overlay base if present.
        if let Some(ref base) = overlay_base {
            let _ = std::fs::remove_dir_all(base);
        }
        return Err(BuildError::RunFailed(status.code().unwrap_or(1)));
    }

    // Check if upper dir has any content.
    let result = if let Some(ref base) = overlay_base {
        let upper = base.join("upper");
        if upper.is_dir() && dir_has_content(&upper)? {
            let digest = create_layer_from_dir(&upper)?;
            Some(digest)
        } else {
            None
        }
    } else {
        None
    };

    // Clean up overlay base dir now that we've captured the layer.
    if let Some(ref base) = overlay_base {
        let _ = std::fs::remove_dir_all(base);
    }

    Ok(result)
}

/// Execute a COPY instruction: create a layer from context files.
fn execute_copy(src: &str, dest: &str, context_dir: &Path) -> Result<String, BuildError> {
    let src_path = context_dir.join(src);
    if !src_path.exists() {
        return Err(BuildError::Io(io::Error::new(
            io::ErrorKind::NotFound,
            format!("COPY source not found: {}", src_path.display()),
        )));
    }

    // Prevent path traversal outside context dir.
    let canonical_src = src_path.canonicalize()?;
    let canonical_ctx = context_dir.canonicalize()?;
    if !canonical_src.starts_with(&canonical_ctx) {
        return Err(BuildError::Io(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "COPY source '{}' is outside build context",
                src_path.display()
            ),
        )));
    }

    let tmp = tempfile::tempdir()?;

    // Build the destination path structure inside temp dir.
    // Strip leading '/' from dest to make it relative.
    let relative_dest = dest.strip_prefix('/').unwrap_or(dest);
    let dest_in_tmp = tmp.path().join(relative_dest);

    if let Some(parent) = dest_in_tmp.parent() {
        std::fs::create_dir_all(parent)?;
    }

    if src_path.is_dir() {
        copy_dir_recursive(&src_path, &dest_in_tmp)?;
    } else {
        std::fs::copy(&src_path, &dest_in_tmp)?;
    }

    let digest = create_layer_from_dir(tmp.path())?;
    Ok(digest)
}

// ---------------------------------------------------------------------------
// Layer creation
// ---------------------------------------------------------------------------

/// Create a content-addressable layer from a directory's contents.
///
/// 1. Tar+gzip the directory contents to compute sha256 digest.
/// 2. If layer already exists (dedup), return early.
/// 3. Copy the directory contents to the layer store.
/// 4. Return the `sha256:<hex>` digest string.
pub fn create_layer_from_dir(source_dir: &Path) -> Result<String, io::Error> {
    use sha2::{Digest, Sha256};

    // Create a tar.gz in memory to compute the digest.
    // We walk the tree manually instead of using `append_dir_all` because the
    // overlay upper dir may contain absolute symlinks that only resolve inside
    // the container rootfs — following them on the host would fail with ENOENT.
    let mut tar_gz_bytes = Vec::new();
    {
        let gz_encoder =
            flate2::write::GzEncoder::new(&mut tar_gz_bytes, flate2::Compression::fast());
        let mut tar_builder = tar::Builder::new(gz_encoder);
        tar_builder.follow_symlinks(false);
        append_dir_all_no_follow(&mut tar_builder, Path::new("."), source_dir)?;
        let gz_encoder = tar_builder.into_inner()?;
        gz_encoder.finish()?;
    }

    let mut hasher = Sha256::new();
    hasher.update(&tar_gz_bytes);
    let hash = hasher.finalize();
    let hex = format!("{:x}", hash);
    let digest = format!("sha256:{}", hex);

    // Check if layer already exists (dedup).
    if image::layer_exists(&digest) {
        log::debug!("layer {} already exists, skipping", &hex[..12]);
        return Ok(digest);
    }

    // Copy directory contents to the layer store.
    let dest = image::layer_dir(&digest);
    std::fs::create_dir_all(&dest)?;
    copy_dir_recursive(source_dir, &dest)?;

    log::debug!("created layer {}", &hex[..12]);
    Ok(digest)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Walk a directory tree and append entries to a tar builder without following
/// symlinks.  Symlinks are stored as symlinks in the archive (preserving their
/// target path), which is critical for overlay upper dirs that contain absolute
/// symlinks into the container rootfs.
fn append_dir_all_no_follow<W: io::Write>(
    builder: &mut tar::Builder<W>,
    prefix: &Path,
    src: &Path,
) -> Result<(), io::Error> {
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let ft = entry.file_type()?; // does NOT follow symlinks
        let name = prefix.join(entry.file_name());
        let path = entry.path();

        if ft.is_dir() {
            builder.append_dir(&name, &path)?;
            append_dir_all_no_follow(builder, &name, &path)?;
        } else if ft.is_symlink() {
            let target = std::fs::read_link(&path)?;
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(tar::EntryType::Symlink);
            header.set_size(0);
            // Read symlink metadata for permissions/ownership.
            let meta = path.symlink_metadata()?;
            header.set_mode(std::os::unix::fs::MetadataExt::mode(&meta));
            header.set_uid(std::os::unix::fs::MetadataExt::uid(&meta) as u64);
            header.set_gid(std::os::unix::fs::MetadataExt::gid(&meta) as u64);
            header.set_mtime(std::os::unix::fs::MetadataExt::mtime(&meta) as u64);
            header.set_cksum();
            builder.append_link(&mut header, &name, &target)?;
        } else {
            // Regular file (or special file — best-effort).
            match builder.append_path_with_name(&path, &name) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => {
                    // Race condition or stale overlay entry — skip silently.
                    log::debug!("skipping vanished file: {}", path.display());
                }
                Err(e) => return Err(e),
            }
        }
    }
    Ok(())
}

/// Check if a directory contains any entries.
fn dir_has_content(dir: &Path) -> Result<bool, io::Error> {
    let mut entries = std::fs::read_dir(dir)?;
    Ok(entries.next().is_some())
}

/// Recursively copy a directory tree.
fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<(), io::Error> {
    if !dst.exists() {
        std::fs::create_dir_all(dst)?;
    }
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let dest_path = dst.join(entry.file_name());

        if file_type.is_dir() {
            copy_dir_recursive(&entry.path(), &dest_path)?;
        } else if file_type.is_symlink() {
            let target = std::fs::read_link(entry.path())?;
            // Remove existing symlink/file if present.
            let _ = std::fs::remove_file(&dest_path);
            std::os::unix::fs::symlink(target, &dest_path)?;
        } else {
            std::fs::copy(entry.path(), &dest_path)?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Build cache
// ---------------------------------------------------------------------------

/// Compute a cache key from the current layer stack and instruction text.
///
/// Key = sha256(last_layer_digest + "\n" + instruction_text).
/// Using only the top layer (not all layers) is sufficient because the top layer
/// digest already transitively depends on everything below it.
fn compute_cache_key(layers: &[String], instruction: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    if let Some(top) = layers.last() {
        hasher.update(top.as_bytes());
    }
    hasher.update(b"\n");
    hasher.update(instruction.as_bytes());
    let hash = hasher.finalize();
    format!("{:x}", hash)
}

/// Look up a cached layer digest by cache key.
fn cache_lookup(key: &str) -> Option<String> {
    let path = crate::paths::build_cache_dir().join(key);
    let digest = std::fs::read_to_string(&path).ok()?;
    let digest = digest.trim().to_string();
    // Verify the layer still exists on disk.
    if image::layer_exists(&digest) {
        Some(digest)
    } else {
        // Stale cache entry — layer was removed.
        let _ = std::fs::remove_file(&path);
        None
    }
}

/// Store a cache entry mapping key → layer digest.
fn cache_store(key: &str, digest: &str) {
    let dir = crate::paths::build_cache_dir();
    if std::fs::create_dir_all(&dir).is_ok() {
        let _ = std::fs::write(dir.join(key), digest);
    }
}

/// Compute a deterministic digest from the ordered layer list.
fn compute_manifest_digest(layers: &[String]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    for layer in layers {
        hasher.update(layer.as_bytes());
        hasher.update(b"\n");
    }
    let hash = hasher.finalize();
    format!("sha256:{:x}", hash)
}

/// Expand bare image names: "alpine" -> "docker.io/library/alpine:latest".
fn normalise_image_reference(reference: &str) -> String {
    let r = reference.to_string();
    let r = if !r.contains(':') && !r.contains('@') {
        format!("{}:latest", r)
    } else {
        r
    };
    if !r.contains('/') {
        format!("docker.io/library/{}", r)
    } else {
        r
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_simple_remfile() {
        let content = r#"
FROM alpine:latest
RUN apk add --no-cache curl
COPY index.html /var/www/index.html
ENV APP_PORT=8080
WORKDIR /var/www
CMD ["httpd", "-f", "-p", "8080"]
EXPOSE 8080
"#;
        let instructions = parse_remfile(content).unwrap();
        assert_eq!(instructions.len(), 7);
        assert_eq!(instructions[0], Instruction::From("alpine:latest".into()));
        assert_eq!(
            instructions[1],
            Instruction::Run("apk add --no-cache curl".into())
        );
        assert_eq!(
            instructions[2],
            Instruction::Copy {
                src: "index.html".into(),
                dest: "/var/www/index.html".into()
            }
        );
        assert_eq!(
            instructions[3],
            Instruction::Env {
                key: "APP_PORT".into(),
                value: "8080".into()
            }
        );
        assert_eq!(instructions[4], Instruction::Workdir("/var/www".into()));
        assert_eq!(
            instructions[5],
            Instruction::Cmd(vec![
                "httpd".into(),
                "-f".into(),
                "-p".into(),
                "8080".into()
            ])
        );
        assert_eq!(instructions[6], Instruction::Expose(8080));
    }

    #[test]
    fn test_parse_comments_and_blank_lines() {
        let content = r#"
# This is a comment
FROM alpine

# Another comment

RUN echo hello
"#;
        let instructions = parse_remfile(content).unwrap();
        assert_eq!(instructions.len(), 2);
    }

    #[test]
    fn test_parse_continuation_lines() {
        let content = "FROM alpine\nRUN apk add \\\n  curl \\\n  wget";
        let instructions = parse_remfile(content).unwrap();
        assert_eq!(instructions.len(), 2);
        assert_eq!(
            instructions[1],
            Instruction::Run("apk add  curl  wget".into())
        );
    }

    #[test]
    fn test_parse_cmd_shell_form() {
        let content = "FROM alpine\nCMD echo hello world";
        let instructions = parse_remfile(content).unwrap();
        assert_eq!(
            instructions[1],
            Instruction::Cmd(vec![
                "/bin/sh".into(),
                "-c".into(),
                "echo hello world".into()
            ])
        );
    }

    #[test]
    fn test_parse_cmd_json_form() {
        let content = r#"FROM alpine
CMD ["/bin/sh", "-c", "echo hello"]"#;
        let instructions = parse_remfile(content).unwrap();
        assert_eq!(
            instructions[1],
            Instruction::Cmd(vec!["/bin/sh".into(), "-c".into(), "echo hello".into()])
        );
    }

    #[test]
    fn test_parse_env_equals_form() {
        let content = "FROM alpine\nENV MY_VAR=hello_world";
        let instructions = parse_remfile(content).unwrap();
        assert_eq!(
            instructions[1],
            Instruction::Env {
                key: "MY_VAR".into(),
                value: "hello_world".into()
            }
        );
    }

    #[test]
    fn test_parse_env_space_form() {
        let content = "FROM alpine\nENV MY_VAR hello world";
        let instructions = parse_remfile(content).unwrap();
        assert_eq!(
            instructions[1],
            Instruction::Env {
                key: "MY_VAR".into(),
                value: "hello world".into()
            }
        );
    }

    #[test]
    fn test_parse_expose_with_protocol() {
        let content = "FROM alpine\nEXPOSE 8080/tcp";
        let instructions = parse_remfile(content).unwrap();
        assert_eq!(instructions[1], Instruction::Expose(8080));
    }

    #[test]
    fn test_parse_error_empty_from() {
        let content = "FROM";
        let err = parse_remfile(content).unwrap_err();
        assert!(err.to_string().contains("requires an image reference"));
    }

    #[test]
    fn test_parse_error_unknown_instruction() {
        let content = "FROM alpine\nFOOBAR something";
        let err = parse_remfile(content).unwrap_err();
        assert!(err.to_string().contains("unknown instruction"));
    }

    #[test]
    fn test_parse_error_copy_missing_dest() {
        let content = "FROM alpine\nCOPY onlysrc";
        let err = parse_remfile(content).unwrap_err();
        assert!(err.to_string().contains("COPY requires <src> <dest>"));
    }

    #[test]
    fn test_parse_case_insensitive() {
        let content = "from alpine\nrun echo hi\ncmd echo hello";
        let instructions = parse_remfile(content).unwrap();
        assert_eq!(instructions.len(), 3);
        assert_eq!(instructions[0], Instruction::From("alpine".into()));
    }

    #[test]
    fn test_normalise_image_reference() {
        assert_eq!(
            normalise_image_reference("alpine"),
            "docker.io/library/alpine:latest"
        );
        assert_eq!(
            normalise_image_reference("alpine:3.19"),
            "docker.io/library/alpine:3.19"
        );
        assert_eq!(
            normalise_image_reference("myregistry.io/myimage:v1"),
            "myregistry.io/myimage:v1"
        );
    }

    #[test]
    fn test_compute_manifest_digest_deterministic() {
        let layers = vec!["sha256:aaa".to_string(), "sha256:bbb".to_string()];
        let d1 = compute_manifest_digest(&layers);
        let d2 = compute_manifest_digest(&layers);
        assert_eq!(d1, d2);
        assert!(d1.starts_with("sha256:"));
    }

    #[test]
    fn test_parse_empty_file() {
        let content = "";
        let instructions = parse_remfile(content).unwrap();
        assert!(instructions.is_empty());
    }

    #[test]
    fn test_parse_entrypoint_json_form() {
        let content = r#"FROM alpine
ENTRYPOINT ["/usr/bin/python3", "-m", "http.server"]"#;
        let instructions = parse_remfile(content).unwrap();
        assert_eq!(
            instructions[1],
            Instruction::Entrypoint(vec![
                "/usr/bin/python3".into(),
                "-m".into(),
                "http.server".into()
            ])
        );
    }

    #[test]
    fn test_parse_entrypoint_shell_form() {
        let content = "FROM alpine\nENTRYPOINT /usr/bin/myapp";
        let instructions = parse_remfile(content).unwrap();
        assert_eq!(
            instructions[1],
            Instruction::Entrypoint(vec!["/bin/sh".into(), "-c".into(), "/usr/bin/myapp".into()])
        );
    }

    #[test]
    fn test_parse_label() {
        let content = "FROM alpine\nLABEL maintainer=\"John Doe\"";
        let instructions = parse_remfile(content).unwrap();
        assert_eq!(
            instructions[1],
            Instruction::Label {
                key: "maintainer".into(),
                value: "John Doe".into()
            }
        );
    }

    #[test]
    fn test_parse_label_unquoted() {
        let content = "FROM alpine\nLABEL version=1.0";
        let instructions = parse_remfile(content).unwrap();
        assert_eq!(
            instructions[1],
            Instruction::Label {
                key: "version".into(),
                value: "1.0".into()
            }
        );
    }

    #[test]
    fn test_parse_user() {
        let content = "FROM alpine\nUSER 1000:1000";
        let instructions = parse_remfile(content).unwrap();
        assert_eq!(instructions[1], Instruction::User("1000:1000".into()));
    }

    #[test]
    fn test_parse_user_name() {
        let content = "FROM alpine\nUSER nobody";
        let instructions = parse_remfile(content).unwrap();
        assert_eq!(instructions[1], Instruction::User("nobody".into()));
    }

    #[test]
    fn test_parse_error_empty_user() {
        let content = "FROM alpine\nUSER";
        let err = parse_remfile(content).unwrap_err();
        assert!(err.to_string().contains("USER requires"));
    }

    #[test]
    fn test_parse_error_empty_label() {
        let content = "FROM alpine\nLABEL";
        let err = parse_remfile(content).unwrap_err();
        assert!(err.to_string().contains("LABEL requires"));
    }

    #[test]
    fn test_cache_key_deterministic() {
        let layers = vec!["sha256:aaa".to_string()];
        let k1 = compute_cache_key(&layers, "RUN echo hello");
        let k2 = compute_cache_key(&layers, "RUN echo hello");
        assert_eq!(k1, k2);
    }

    #[test]
    fn test_cache_key_changes_with_instruction() {
        let layers = vec!["sha256:aaa".to_string()];
        let k1 = compute_cache_key(&layers, "RUN echo hello");
        let k2 = compute_cache_key(&layers, "RUN echo world");
        assert_ne!(k1, k2);
    }

    #[test]
    fn test_cache_key_changes_with_layers() {
        let l1 = vec!["sha256:aaa".to_string()];
        let l2 = vec!["sha256:bbb".to_string()];
        let k1 = compute_cache_key(&l1, "RUN echo hello");
        let k2 = compute_cache_key(&l2, "RUN echo hello");
        assert_ne!(k1, k2);
    }
}
