//! Build engine for creating OCI images from Remfiles (simplified Dockerfiles).
//!
//! The build process reads a Remfile, executes each instruction in sequence,
//! and produces an `ImageManifest` stored in the local image store.

use crate::container::{Command, Namespace, Stdio};
use crate::image::{self, ImageConfig, ImageManifest};
use crate::network::NetworkMode;
use std::collections::{HashMap, HashSet};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

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

    #[error("URL download failed: {0}")]
    UrlDownload(String),
}

// ---------------------------------------------------------------------------
// Instruction AST
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Instruction {
    From {
        image: String,
        alias: Option<String>,
    },
    Run(String),
    Copy {
        src: String,
        dest: String,
        from_stage: Option<String>,
    },
    Cmd(Vec<String>),
    Entrypoint(Vec<String>),
    Env {
        key: String,
        value: String,
    },
    Workdir(String),
    Expose(u16),
    Label {
        key: String,
        value: String,
    },
    User(String),
    Arg {
        name: String,
        default: Option<String>,
    },
    Add {
        src: String,
        dest: String,
    },
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
                // Detect "FROM image AS alias" (case-insensitive AS).
                let (image, alias) = if let Some(pos) = rest.to_ascii_lowercase().find(" as ") {
                    let img = rest[..pos].trim().to_string();
                    let al = rest[pos + 4..].trim().to_string();
                    (img, Some(al))
                } else {
                    (rest.to_string(), None)
                };
                instructions.push(Instruction::From { image, alias });
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
                // Detect optional --from=<stage> prefix.
                let (from_stage, remaining) = if let Some(after_flag) = rest.strip_prefix("--from=")
                {
                    if let Some((stage, r)) = after_flag.split_once(char::is_whitespace) {
                        (Some(stage.to_string()), r.trim())
                    } else {
                        return Err(BuildError::Parse {
                            line: line_num,
                            message: "COPY --from=<stage> requires <src> <dest>".to_string(),
                        });
                    }
                } else {
                    (None, rest)
                };
                let parts: Vec<&str> = remaining.splitn(2, char::is_whitespace).collect();
                if parts.len() < 2 {
                    return Err(BuildError::Parse {
                        line: line_num,
                        message: "COPY requires <src> <dest>".to_string(),
                    });
                }
                instructions.push(Instruction::Copy {
                    src: parts[0].to_string(),
                    dest: parts[1].trim().to_string(),
                    from_stage,
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
            "ADD" => {
                let parts: Vec<&str> = rest.splitn(2, char::is_whitespace).collect();
                if parts.len() < 2 {
                    return Err(BuildError::Parse {
                        line: line_num,
                        message: "ADD requires <src> <dest>".to_string(),
                    });
                }
                instructions.push(Instruction::Add {
                    src: parts[0].to_string(),
                    dest: parts[1].trim().to_string(),
                });
            }
            "ARG" => {
                if rest.is_empty() {
                    return Err(BuildError::Parse {
                        line: line_num,
                        message: "ARG requires a variable name".to_string(),
                    });
                }
                let (name, default) = if let Some((n, v)) = rest.split_once('=') {
                    (n.to_string(), Some(v.to_string()))
                } else {
                    (rest.to_string(), None)
                };
                instructions.push(Instruction::Arg { name, default });
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
// Variable substitution (ARG / ENV)
// ---------------------------------------------------------------------------

/// Replace `$VAR`, `${VAR}`, and `$$` (literal `$`) in `text`.
/// Unknown variables expand to the empty string.
pub fn substitute_vars(text: &str, vars: &HashMap<String, String>) -> String {
    let bytes = text.as_bytes();
    let len = bytes.len();
    let mut out = String::with_capacity(len);
    let mut i = 0;
    while i < len {
        if bytes[i] == b'$' {
            if i + 1 < len && bytes[i + 1] == b'$' {
                // Escaped dollar: $$ → $
                out.push('$');
                i += 2;
            } else if i + 1 < len && bytes[i + 1] == b'{' {
                // ${VAR} form
                if let Some(close) = text[i + 2..].find('}') {
                    let name = &text[i + 2..i + 2 + close];
                    if let Some(val) = vars.get(name) {
                        out.push_str(val);
                    }
                    i = i + 2 + close + 1;
                } else {
                    // Unterminated ${...}, copy literally
                    out.push('$');
                    i += 1;
                }
            } else if i + 1 < len && (bytes[i + 1].is_ascii_alphanumeric() || bytes[i + 1] == b'_')
            {
                // $VAR form
                let start = i + 1;
                let mut end = start;
                while end < len && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_') {
                    end += 1;
                }
                let name = &text[start..end];
                if let Some(val) = vars.get(name) {
                    out.push_str(val);
                }
                i = end;
            } else {
                out.push('$');
                i += 1;
            }
        } else {
            out.push(bytes[i] as char);
            i += 1;
        }
    }
    out
}

/// Clone an instruction with all string fields substituted.
fn substitute_instruction(instr: &Instruction, vars: &HashMap<String, String>) -> Instruction {
    match instr {
        Instruction::From { image, alias } => Instruction::From {
            image: substitute_vars(image, vars),
            alias: alias.clone(),
        },
        Instruction::Run(cmd) => Instruction::Run(substitute_vars(cmd, vars)),
        Instruction::Copy {
            src,
            dest,
            from_stage,
        } => Instruction::Copy {
            src: substitute_vars(src, vars),
            dest: substitute_vars(dest, vars),
            from_stage: from_stage.clone(),
        },
        Instruction::Cmd(args) => {
            Instruction::Cmd(args.iter().map(|a| substitute_vars(a, vars)).collect())
        }
        Instruction::Entrypoint(args) => {
            Instruction::Entrypoint(args.iter().map(|a| substitute_vars(a, vars)).collect())
        }
        Instruction::Env { key, value } => Instruction::Env {
            key: substitute_vars(key, vars),
            value: substitute_vars(value, vars),
        },
        Instruction::Workdir(path) => Instruction::Workdir(substitute_vars(path, vars)),
        Instruction::Expose(port) => Instruction::Expose(*port),
        Instruction::Label { key, value } => Instruction::Label {
            key: substitute_vars(key, vars),
            value: substitute_vars(value, vars),
        },
        Instruction::User(user) => Instruction::User(substitute_vars(user, vars)),
        Instruction::Arg { name, default } => Instruction::Arg {
            name: name.clone(),
            default: default.as_ref().map(|d| substitute_vars(d, vars)),
        },
        Instruction::Add { src, dest } => Instruction::Add {
            src: substitute_vars(src, vars),
            dest: substitute_vars(dest, vars),
        },
    }
}

// ---------------------------------------------------------------------------
// Build execution
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Multi-stage build support
// ---------------------------------------------------------------------------

/// A single stage in a multi-stage build.
struct BuildStage {
    alias: Option<String>,
    instructions: Vec<Instruction>,
}

/// Split a flat instruction list into stages at FROM boundaries.
/// Each stage starts with a FROM instruction. Pre-FROM ARG instructions
/// are placed into the first stage.
fn split_into_stages(instructions: &[Instruction]) -> Vec<BuildStage> {
    let mut stages: Vec<BuildStage> = Vec::new();

    for instr in instructions {
        match instr {
            Instruction::From {
                image: _,
                ref alias,
            } => {
                stages.push(BuildStage {
                    alias: alias.clone(),
                    instructions: vec![instr.clone()],
                });
            }
            _ => {
                if stages.is_empty() {
                    // Pre-FROM instructions (ARGs) — create a virtual first stage.
                    stages.push(BuildStage {
                        alias: None,
                        instructions: vec![instr.clone()],
                    });
                } else {
                    stages.last_mut().unwrap().instructions.push(instr.clone());
                }
            }
        }
    }

    stages
}

/// Execute a single build stage, returning (layers, config).
#[allow(clippy::too_many_arguments)]
fn execute_stage(
    instructions: &[Instruction],
    context_dir: &Path,
    network_mode: NetworkMode,
    use_cache: bool,
    args_map: &mut HashMap<String, String>,
    sub_vars: &mut HashMap<String, String>,
    remignore: Option<&ignore::gitignore::Gitignore>,
    completed_stages: &HashMap<String, Vec<String>>,
) -> Result<(Vec<String>, ImageConfig), BuildError> {
    // Find the FROM instruction to load the base image.
    let from_idx = instructions
        .iter()
        .position(|i| matches!(i, Instruction::From { .. }));

    let (mut layers, mut config) = if let Some(idx) = from_idx {
        let from_instr = substitute_instruction(&instructions[idx], sub_vars);
        let base_ref = match &from_instr {
            Instruction::From { ref image, .. } => image.clone(),
            _ => unreachable!(),
        };

        let normalised = normalise_image_reference(&base_ref);
        let base_manifest = image::load_image(&normalised)
            .map_err(|_| BuildError::ImageNotFound(base_ref.clone()))?;

        (base_manifest.layers.clone(), base_manifest.config.clone())
    } else {
        // Stage without FROM (pre-FROM ARGs only).
        (Vec::new(), ImageConfig::default())
    };

    let total = instructions.len();
    let mut cache_active = use_cache;

    for (idx, raw_instr) in instructions.iter().enumerate() {
        let instr = substitute_instruction(raw_instr, sub_vars);
        let step = idx + 1;
        match &instr {
            Instruction::From { ref image, .. } => {
                eprintln!("Step {}/{}: FROM {}", step, total, image);
            }
            Instruction::Arg {
                ref name,
                ref default,
            } => {
                let value = args_map
                    .entry(name.clone())
                    .or_insert_with(|| default.clone().unwrap_or_default())
                    .clone();
                sub_vars.insert(name.clone(), value.clone());
                eprintln!("Step {}/{}: ARG {}={}", step, total, name, value);
            }
            Instruction::Run(ref cmd_text) => {
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
            Instruction::Copy {
                ref src,
                ref dest,
                ref from_stage,
            } => {
                cache_active = false;
                if let Some(ref stage_name) = from_stage {
                    eprintln!(
                        "Step {}/{}: COPY --from={} {} {}",
                        step, total, stage_name, src, dest
                    );
                    let stage_layers =
                        completed_stages
                            .get(stage_name)
                            .ok_or_else(|| BuildError::Parse {
                                line: 0,
                                message: format!("COPY --from={}: unknown stage", stage_name),
                            })?;
                    let digest =
                        execute_copy_from_stage(src, dest, stage_layers, &config.working_dir)?;
                    layers.push(digest);
                } else {
                    eprintln!("Step {}/{}: COPY {} {}", step, total, src, dest);
                    let digest =
                        execute_copy(src, dest, context_dir, remignore, &config.working_dir)?;
                    layers.push(digest);
                }
            }
            Instruction::Cmd(ref args) => {
                eprintln!("Step {}/{}: CMD {:?}", step, total, args);
                config.cmd = args.clone();
            }
            Instruction::Env { ref key, ref value } => {
                eprintln!("Step {}/{}: ENV {}={}", step, total, key, value);
                config.env.retain(|e| !e.starts_with(&format!("{}=", key)));
                config.env.push(format!("{}={}", key, value));
                sub_vars.entry(key.clone()).or_insert_with(|| value.clone());
            }
            Instruction::Workdir(ref path) => {
                eprintln!("Step {}/{}: WORKDIR {}", step, total, path);
                config.working_dir = path.clone();
                // Create the directory as a layer if it doesn't already exist in
                // any current layer (matches Docker behaviour: WORKDIR always
                // ensures the directory is present).
                let rel = path.trim_start_matches('/');
                let already_exists = layers
                    .iter()
                    .any(|d| image::layer_dir(d).join(rel).is_dir());
                if !already_exists {
                    let digest = execute_workdir(path)?;
                    layers.push(digest);
                }
            }
            Instruction::Entrypoint(ref args) => {
                eprintln!("Step {}/{}: ENTRYPOINT {:?}", step, total, args);
                config.entrypoint = args.clone();
            }
            Instruction::Expose(port) => {
                eprintln!("Step {}/{}: EXPOSE {}", step, total, port);
            }
            Instruction::Label { ref key, ref value } => {
                eprintln!("Step {}/{}: LABEL {}={}", step, total, key, value);
                config.labels.insert(key.clone(), value.clone());
            }
            Instruction::User(ref user) => {
                eprintln!("Step {}/{}: USER {}", step, total, user);
                config.user = user.clone();
            }
            Instruction::Add { ref src, ref dest } => {
                cache_active = false;
                eprintln!("Step {}/{}: ADD {} {}", step, total, src, dest);
                let digest = execute_add(src, dest, context_dir, remignore, &config.working_dir)?;
                layers.push(digest);
            }
        }
    }

    Ok((layers, config))
}

/// Copy a file from a previous stage's layers into a new layer.
///
/// Walks the stage's layer directories top-to-bottom (last layer = highest
/// priority) looking for the source path. This is a simplified approach that
/// does not handle overlayfs whiteouts — deleted files in upper layers may
/// still be visible from lower layers. Acceptable for typical COPY --from
/// use cases (copying build artifacts).
fn execute_copy_from_stage(
    src: &str,
    dest: &str,
    stage_layers: &[String],
    working_dir: &str,
) -> Result<String, BuildError> {
    let relative_src = src.strip_prefix('/').unwrap_or(src);

    // Walk layers top-to-bottom (last added = highest priority).
    for layer_digest in stage_layers.iter().rev() {
        let layer_dir = image::layer_dir(layer_digest);
        let candidate = layer_dir.join(relative_src);
        if candidate.exists() {
            let tmp = tempfile::tempdir()?;
            let src_basename = Path::new(src)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(src);
            let resolved = resolve_copy_dest(dest, src_basename, working_dir);
            let relative_dest = resolved.trim_start_matches('/');
            let dest_in_tmp = tmp.path().join(relative_dest);

            if let Some(parent) = dest_in_tmp.parent() {
                std::fs::create_dir_all(parent)?;
            }

            if candidate.is_dir() {
                copy_dir_recursive(&candidate, &dest_in_tmp)?;
            } else {
                std::fs::copy(&candidate, &dest_in_tmp)?;
            }

            return Ok(create_layer_from_dir(tmp.path())?);
        }
    }

    Err(BuildError::Io(io::Error::new(
        io::ErrorKind::NotFound,
        format!("COPY --from: source '{}' not found in stage layers", src),
    )))
}

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
    build_args: &HashMap<String, String>,
) -> Result<ImageManifest, BuildError> {
    if instructions.is_empty() {
        return Err(BuildError::MissingFrom);
    }

    // ARG substitution state: seeded from CLI --build-arg values.
    let mut args_map: HashMap<String, String> = build_args.clone();

    // Find the first non-ARG instruction — it must be FROM.
    // ARG instructions before FROM are allowed (Docker compat) and seed the
    // substitution context for the FROM line itself.
    let first_non_arg = instructions
        .iter()
        .position(|i| !matches!(i, Instruction::Arg { .. }))
        .ok_or(BuildError::MissingFrom)?;

    // Process pre-FROM ARGs.
    for instr in &instructions[..first_non_arg] {
        if let Instruction::Arg { name, default } = instr {
            args_map
                .entry(name.clone())
                .or_insert_with(|| default.clone().unwrap_or_default());
        }
    }

    // Build substitution context: ARG values override ENV on conflict.
    let mut sub_vars: HashMap<String, String> = HashMap::new();
    // (ENV vars will be added as we encounter them; ARG values are already in args_map)
    sub_vars.extend(args_map.iter().map(|(k, v)| (k.clone(), v.clone())));

    let from_instr = substitute_instruction(&instructions[first_non_arg], &sub_vars);
    let _base_ref = match &from_instr {
        Instruction::From { ref image, .. } => image.clone(),
        _ => return Err(BuildError::MissingFrom),
    };

    // Load .remignore patterns (if present) for COPY filtering.
    let remignore = load_remignore(context_dir);

    // Split into stages (multi-stage builds).
    let stages = split_into_stages(instructions);

    // Track completed stages for COPY --from resolution.
    let mut completed_stages: HashMap<String, Vec<String>> = HashMap::new();
    let mut final_layers = Vec::new();
    let mut final_config = ImageConfig::default();
    let num_stages = stages.len();

    for (stage_idx, stage) in stages.iter().enumerate() {
        let is_final = stage_idx == num_stages - 1;
        eprintln!(
            "==> Stage {} ({}){}",
            stage_idx,
            stage.alias.as_deref().unwrap_or("unnamed"),
            if is_final { " [final]" } else { "" }
        );

        let (layers, config) = execute_stage(
            &stage.instructions,
            context_dir,
            network_mode.clone(),
            use_cache,
            &mut args_map,
            &mut sub_vars,
            remignore.as_ref(),
            &completed_stages,
        )?;

        // Record this stage's layers for COPY --from.
        if let Some(ref alias) = stage.alias {
            completed_stages.insert(alias.clone(), layers.clone());
        }
        // Also track by stage index.
        completed_stages.insert(stage_idx.to_string(), layers.clone());

        if is_final {
            final_layers = layers;
            final_config = config;
        }
    }

    let layers = final_layers;
    let config = final_config;

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

    // Generate and persist the OCI config JSON so `remora image push` can use it.
    let oci_config_json = generate_oci_config_json(&manifest.config, &manifest.layers);
    if let Err(e) = image::save_oci_config(&manifest.reference, &oci_config_json) {
        log::warn!(
            "failed to save OCI config JSON for '{}': {}",
            manifest.reference,
            e
        );
    }

    Ok(manifest)
}

/// Generate a minimal OCI image config JSON for a built image.
///
/// Uses the diff_id sidecar files saved by `create_layer_from_dir`.  Falls
/// back to the compressed digest when the sidecar is absent (e.g. for layers
/// pulled from a registry, where the OCI config JSON is already saved verbatim
/// by `cmd_image_pull`).
fn generate_oci_config_json(config: &ImageConfig, layer_digests: &[String]) -> String {
    let diff_ids: Vec<String> = layer_digests
        .iter()
        .map(|d| image::load_blob_diffid(d).unwrap_or_else(|| d.clone()))
        .collect();

    serde_json::json!({
        "architecture": "amd64",
        "os": "linux",
        "config": {
            "Env": config.env,
            "Cmd": config.cmd,
            "Entrypoint": config.entrypoint,
            "WorkingDir": config.working_dir,
            "User": config.user,
            "Labels": config.labels,
        },
        "rootfs": {
            "type": "layers",
            "diff_ids": diff_ids,
        },
        "history": []
    })
    .to_string()
}

/// Execute a RUN instruction: spawn a container, wait, capture upper layer.
fn execute_run(
    cmd_text: &str,
    current_layers: &[String],
    config: &ImageConfig,
    network_mode: NetworkMode,
) -> Result<Option<String>, BuildError> {
    // Deduplicate layer paths — overlayfs returns EINVAL if lowerdir contains
    // duplicate entries, which can happen when a base image (e.g. Debian/Python)
    // has repeated layer digests (empty marker layers are common).
    let mut seen: HashSet<PathBuf> = HashSet::new();
    let layer_dirs: Vec<PathBuf> = current_layers
        .iter()
        .rev()
        .map(|d| image::layer_dir(d))
        .filter(|p| seen.insert(p.clone()))
        .collect();

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

/// Execute a WORKDIR instruction: create a layer that ensures the directory exists.
///
/// Docker's WORKDIR always creates the target directory if it is absent from
/// any existing layer.  We materialise it as a minimal layer so that
/// subsequent RUN steps can `chdir` into it after the chroot.
fn execute_workdir(path: &str) -> Result<String, BuildError> {
    let tmp = tempfile::tempdir().map_err(BuildError::Io)?;
    let rel = path.trim_start_matches('/');
    std::fs::create_dir_all(tmp.path().join(rel)).map_err(BuildError::Io)?;
    create_layer_from_dir(tmp.path()).map_err(BuildError::Io)
}

/// Resolve a COPY/ADD destination path following Docker semantics.
///
/// - Absolute paths are used as-is.
/// - Relative paths (including `.`) are resolved against `working_dir`.
/// - If the resolved path ends with `/` (directory destination), `src_basename`
///   is appended to produce the final file path.  Pass `""` for archive ADD where
///   the destination is always a directory and no filename suffix is needed.
fn resolve_copy_dest(dest: &str, src_basename: &str, working_dir: &str) -> String {
    let abs = if dest.starts_with('/') {
        dest.to_owned()
    } else {
        let wd = working_dir.trim_end_matches('/');
        let wd = if wd.is_empty() { "/" } else { wd };
        if dest == "." || dest == "./" {
            format!("{}/", wd)
        } else {
            format!("{}/{}", wd, dest)
        }
    };
    if !src_basename.is_empty() && abs.ends_with('/') {
        format!("{}{}", abs, src_basename)
    } else {
        abs
    }
}

/// Execute a COPY instruction: create a layer from context files.
/// When `remignore` is `Some`, directory copies skip matched entries.
fn execute_copy(
    src: &str,
    dest: &str,
    context_dir: &Path,
    remignore: Option<&ignore::gitignore::Gitignore>,
    working_dir: &str,
) -> Result<String, BuildError> {
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

    // Resolve destination: handle relative paths and directory destinations.
    let src_basename = Path::new(src)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(src);
    let resolved = resolve_copy_dest(dest, src_basename, working_dir);
    let relative_dest = resolved.trim_start_matches('/');
    let dest_in_tmp = tmp.path().join(relative_dest);

    if let Some(parent) = dest_in_tmp.parent() {
        std::fs::create_dir_all(parent)?;
    }

    if src_path.is_dir() {
        if let Some(gi) = remignore {
            copy_dir_filtered(&src_path, &dest_in_tmp, gi, &src_path)?;
        } else {
            copy_dir_recursive(&src_path, &dest_in_tmp)?;
        }
    } else {
        // For single files, check remignore before copying.
        if let Some(gi) = remignore {
            let rel = src_path.strip_prefix(context_dir).unwrap_or(&src_path);
            if gi.matched(rel, false).is_ignore() {
                log::debug!("remignore: skipping single file {}", rel.display());
                // Return an empty layer.
                let digest = create_layer_from_dir(tmp.path())?;
                return Ok(digest);
            }
        }
        std::fs::copy(&src_path, &dest_in_tmp)?;
    }

    let digest = create_layer_from_dir(tmp.path())?;
    Ok(digest)
}

// ---------------------------------------------------------------------------
// ADD instruction execution
// ---------------------------------------------------------------------------

/// Detect whether a source string looks like a URL.
fn is_url(src: &str) -> bool {
    src.starts_with("http://") || src.starts_with("https://")
}

/// Recognised archive extensions for ADD auto-extraction.
fn is_archive(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.ends_with(".tar")
        || lower.ends_with(".tar.gz")
        || lower.ends_with(".tgz")
        || lower.ends_with(".tar.bz2")
        || lower.ends_with(".tar.xz")
        || lower.ends_with(".txz")
}

/// Execute an ADD instruction: URL download, archive extraction, or plain copy.
fn execute_add(
    src: &str,
    dest: &str,
    context_dir: &Path,
    remignore: Option<&ignore::gitignore::Gitignore>,
    working_dir: &str,
) -> Result<String, BuildError> {
    if is_url(src) {
        execute_add_url(src, dest, working_dir)
    } else if is_archive(src) {
        execute_add_archive(src, dest, context_dir, remignore, working_dir)
    } else {
        // Fall through to normal COPY behaviour.
        execute_copy(src, dest, context_dir, remignore, working_dir)
    }
}

/// Download a URL and place it at `dest` inside a new layer.
fn execute_add_url(url: &str, dest: &str, working_dir: &str) -> Result<String, BuildError> {
    let response = ureq::get(url)
        .call()
        .map_err(|e| BuildError::UrlDownload(format!("{}: {}", url, e)))?;

    let tmp = tempfile::tempdir()?;
    // Use the URL's last path segment as the filename for directory destinations.
    let url_basename = url.rsplit('/').next().unwrap_or("file");
    let resolved = resolve_copy_dest(dest, url_basename, working_dir);
    let relative_dest = resolved.trim_start_matches('/');
    let dest_in_tmp = tmp.path().join(relative_dest);

    if let Some(parent) = dest_in_tmp.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let mut reader = response.into_reader();
    let mut file = std::fs::File::create(&dest_in_tmp)?;
    io::copy(&mut reader, &mut file)?;

    let digest = create_layer_from_dir(tmp.path())?;
    Ok(digest)
}

/// Extract a local archive into a layer at `dest`.
fn execute_add_archive(
    src: &str,
    dest: &str,
    context_dir: &Path,
    remignore: Option<&ignore::gitignore::Gitignore>,
    working_dir: &str,
) -> Result<String, BuildError> {
    let src_path = context_dir.join(src);
    if !src_path.exists() {
        return Err(BuildError::Io(io::Error::new(
            io::ErrorKind::NotFound,
            format!("ADD source not found: {}", src_path.display()),
        )));
    }

    // Path traversal check.
    let canonical_src = src_path.canonicalize()?;
    let canonical_ctx = context_dir.canonicalize()?;
    if !canonical_src.starts_with(&canonical_ctx) {
        return Err(BuildError::Io(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "ADD source '{}' is outside build context",
                src_path.display()
            ),
        )));
    }

    // Check .remignore on the archive file itself.
    if let Some(gi) = remignore {
        let rel = src_path.strip_prefix(context_dir).unwrap_or(&src_path);
        if gi.matched(rel, false).is_ignore() {
            log::debug!("remignore: skipping ADD archive {}", rel.display());
            let tmp = tempfile::tempdir()?;
            return Ok(create_layer_from_dir(tmp.path())?);
        }
    }

    let tmp = tempfile::tempdir()?;
    // Archives are always extracted INTO a directory; pass "" so no filename is appended.
    let resolved = resolve_copy_dest(dest, "", working_dir);
    let relative_dest = resolved.trim_start_matches('/');
    let dest_in_tmp = tmp.path().join(relative_dest);
    std::fs::create_dir_all(&dest_in_tmp)?;

    let file = std::fs::File::open(&src_path)?;
    let lower = src.to_ascii_lowercase();

    if lower.ends_with(".tar.gz") || lower.ends_with(".tgz") {
        let decoder = flate2::read::GzDecoder::new(file);
        tar::Archive::new(decoder).unpack(&dest_in_tmp)?;
    } else if lower.ends_with(".tar.bz2") {
        let decoder = bzip2::read::BzDecoder::new(file);
        tar::Archive::new(decoder).unpack(&dest_in_tmp)?;
    } else if lower.ends_with(".tar.xz") || lower.ends_with(".txz") {
        let decoder = xz2::read::XzDecoder::new(file);
        tar::Archive::new(decoder).unpack(&dest_in_tmp)?;
    } else {
        // Plain .tar
        tar::Archive::new(file).unpack(&dest_in_tmp)?;
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

    // Build the raw (uncompressed) tar first so we can compute the diff_id
    // (sha256 of the uncompressed tar stream, required for OCI config JSON).
    // We walk the tree manually instead of using `append_dir_all` because the
    // overlay upper dir may contain absolute symlinks that only resolve inside
    // the container rootfs — following them on the host would fail with ENOENT.
    let mut raw_tar_bytes = Vec::new();
    {
        let mut tar_builder = tar::Builder::new(&mut raw_tar_bytes);
        tar_builder.follow_symlinks(false);
        append_dir_all_no_follow(&mut tar_builder, Path::new("."), source_dir)?;
        tar_builder.into_inner()?;
    }
    let diff_id = format!("sha256:{:x}", Sha256::digest(&raw_tar_bytes));

    // Compress to get the canonical tar.gz blob.
    let mut tar_gz_bytes = Vec::new();
    {
        let mut gz = flate2::write::GzEncoder::new(&mut tar_gz_bytes, flate2::Compression::fast());
        gz.write_all(&raw_tar_bytes)?;
        gz.finish()?;
    }
    drop(raw_tar_bytes); // free memory early

    let hex = format!("{:x}", Sha256::digest(&tar_gz_bytes));
    let digest = format!("sha256:{}", hex);

    // Check if layer already exists (dedup).
    if image::layer_exists(&digest) {
        log::debug!("layer {} already exists, skipping", &hex[..12]);
        // Still persist blob/diffid if they were missing (e.g. old layer store).
        if !image::blob_exists(&digest) {
            image::save_blob(&digest, &tar_gz_bytes)?;
            image::save_blob_diffid(&digest, &diff_id)?;
        }
        return Ok(digest);
    }

    // Persist the raw blob for future `remora image push`.
    image::save_blob(&digest, &tar_gz_bytes)?;
    image::save_blob_diffid(&digest, &diff_id)?;

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
// .remignore support
// ---------------------------------------------------------------------------

/// Load `.remignore` patterns from the build context root, if the file exists.
fn load_remignore(context_dir: &Path) -> Option<ignore::gitignore::Gitignore> {
    let path = context_dir.join(".remignore");
    if !path.is_file() {
        return None;
    }
    let mut builder = ignore::gitignore::GitignoreBuilder::new(context_dir);
    builder.add(path);
    match builder.build() {
        Ok(gi) => Some(gi),
        Err(e) => {
            log::warn!("failed to parse .remignore: {}", e);
            None
        }
    }
}

/// Recursively copy a directory tree, skipping entries matched by the ignore
/// patterns. `src_root` is the top-level source directory used to compute
/// relative paths for pattern matching.
fn copy_dir_filtered(
    src: &Path,
    dst: &Path,
    gi: &ignore::gitignore::Gitignore,
    src_root: &Path,
) -> Result<(), io::Error> {
    if !dst.exists() {
        std::fs::create_dir_all(dst)?;
    }
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let path = entry.path();
        let dest_path = dst.join(entry.file_name());

        // Compute relative path from src_root for pattern matching.
        let rel = path.strip_prefix(src_root).unwrap_or(&path);
        if gi.matched(rel, file_type.is_dir()).is_ignore() {
            log::debug!("remignore: skipping {}", rel.display());
            continue;
        }

        if file_type.is_dir() {
            copy_dir_filtered(&path, &dest_path, gi, src_root)?;
        } else if file_type.is_symlink() {
            let target = std::fs::read_link(&path)?;
            let _ = std::fs::remove_file(&dest_path);
            std::os::unix::fs::symlink(target, &dest_path)?;
        } else {
            std::fs::copy(&path, &dest_path)?;
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
        assert_eq!(
            instructions[0],
            Instruction::From {
                image: "alpine:latest".into(),
                alias: None
            }
        );
        assert_eq!(
            instructions[1],
            Instruction::Run("apk add --no-cache curl".into())
        );
        assert_eq!(
            instructions[2],
            Instruction::Copy {
                src: "index.html".into(),
                dest: "/var/www/index.html".into(),
                from_stage: None,
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
        assert_eq!(
            instructions[0],
            Instruction::From {
                image: "alpine".into(),
                alias: None
            }
        );
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

    // -- ARG parsing tests --

    #[test]
    fn test_parse_arg_with_default() {
        let content = "FROM alpine\nARG VERSION=1.0";
        let instructions = parse_remfile(content).unwrap();
        assert_eq!(
            instructions[1],
            Instruction::Arg {
                name: "VERSION".into(),
                default: Some("1.0".into())
            }
        );
    }

    #[test]
    fn test_parse_arg_no_default() {
        let content = "FROM alpine\nARG MY_VAR";
        let instructions = parse_remfile(content).unwrap();
        assert_eq!(
            instructions[1],
            Instruction::Arg {
                name: "MY_VAR".into(),
                default: None
            }
        );
    }

    #[test]
    fn test_parse_arg_before_from() {
        let content = "ARG BASE=alpine\nFROM $BASE\nRUN echo hi";
        let instructions = parse_remfile(content).unwrap();
        assert_eq!(instructions.len(), 3);
        assert!(matches!(instructions[0], Instruction::Arg { .. }));
        assert!(matches!(instructions[1], Instruction::From { .. }));
    }

    #[test]
    fn test_parse_arg_error_empty() {
        let content = "FROM alpine\nARG";
        let err = parse_remfile(content).unwrap_err();
        assert!(err.to_string().contains("ARG requires"));
    }

    // -- ADD parsing tests --

    #[test]
    fn test_parse_add() {
        let content = "FROM alpine\nADD archive.tar.gz /opt/app";
        let instructions = parse_remfile(content).unwrap();
        assert_eq!(
            instructions[1],
            Instruction::Add {
                src: "archive.tar.gz".into(),
                dest: "/opt/app".into()
            }
        );
    }

    #[test]
    fn test_parse_add_error_missing_dest() {
        let content = "FROM alpine\nADD onlysrc";
        let err = parse_remfile(content).unwrap_err();
        assert!(err.to_string().contains("ADD requires <src> <dest>"));
    }

    #[test]
    fn test_is_archive() {
        assert!(is_archive("foo.tar"));
        assert!(is_archive("foo.tar.gz"));
        assert!(is_archive("foo.tgz"));
        assert!(is_archive("foo.tar.bz2"));
        assert!(is_archive("foo.tar.xz"));
        assert!(is_archive("foo.txz"));
        assert!(is_archive("FOO.TAR.GZ"));
        assert!(!is_archive("foo.zip"));
        assert!(!is_archive("foo.txt"));
    }

    #[test]
    fn test_is_url() {
        assert!(is_url("http://example.com/file.tar.gz"));
        assert!(is_url("https://example.com/file"));
        assert!(!is_url("local/path"));
        assert!(!is_url("ftp://example.com"));
    }

    // -- Variable substitution tests --

    #[test]
    fn test_substitute_vars_dollar() {
        let mut vars = HashMap::new();
        vars.insert("NAME".to_string(), "world".to_string());
        assert_eq!(substitute_vars("hello $NAME", &vars), "hello world");
    }

    #[test]
    fn test_substitute_vars_braces() {
        let mut vars = HashMap::new();
        vars.insert("VER".to_string(), "3.19".to_string());
        assert_eq!(substitute_vars("alpine:${VER}", &vars), "alpine:3.19");
    }

    #[test]
    fn test_substitute_vars_escape_dollar() {
        let vars = HashMap::new();
        assert_eq!(substitute_vars("cost is $$5", &vars), "cost is $5");
    }

    #[test]
    fn test_substitute_vars_unknown() {
        let vars = HashMap::new();
        assert_eq!(substitute_vars("hello $NOBODY", &vars), "hello ");
    }

    #[test]
    fn test_substitute_vars_mixed() {
        let mut vars = HashMap::new();
        vars.insert("A".to_string(), "1".to_string());
        vars.insert("B".to_string(), "2".to_string());
        assert_eq!(substitute_vars("$A-${B}-$$", &vars), "1-2-$");
    }

    // -- .remignore tests --

    #[test]
    fn test_copy_dir_filtered_excludes() {
        let tmp_src = tempfile::tempdir().unwrap();
        let tmp_dst = tempfile::tempdir().unwrap();

        // Create source files.
        std::fs::write(tmp_src.path().join("keep.txt"), "keep").unwrap();
        std::fs::write(tmp_src.path().join("skip.log"), "skip").unwrap();
        std::fs::create_dir(tmp_src.path().join("subdir")).unwrap();
        std::fs::write(tmp_src.path().join("subdir/data.txt"), "data").unwrap();
        std::fs::write(tmp_src.path().join("subdir/debug.log"), "debug").unwrap();

        // Build gitignore that excludes *.log.
        let mut builder = ignore::gitignore::GitignoreBuilder::new(tmp_src.path());
        builder.add_line(None, "*.log").unwrap();
        let gi = builder.build().unwrap();

        copy_dir_filtered(tmp_src.path(), tmp_dst.path(), &gi, tmp_src.path()).unwrap();

        assert!(tmp_dst.path().join("keep.txt").exists());
        assert!(!tmp_dst.path().join("skip.log").exists());
        assert!(tmp_dst.path().join("subdir/data.txt").exists());
        assert!(!tmp_dst.path().join("subdir/debug.log").exists());
    }

    #[test]
    fn test_remignore_negation_pattern() {
        let tmp_src = tempfile::tempdir().unwrap();
        let tmp_dst = tempfile::tempdir().unwrap();

        std::fs::write(tmp_src.path().join("a.log"), "a").unwrap();
        std::fs::write(tmp_src.path().join("important.log"), "keep").unwrap();
        std::fs::write(tmp_src.path().join("b.txt"), "b").unwrap();

        // Exclude *.log but negate important.log.
        let mut builder = ignore::gitignore::GitignoreBuilder::new(tmp_src.path());
        builder.add_line(None, "*.log").unwrap();
        builder.add_line(None, "!important.log").unwrap();
        let gi = builder.build().unwrap();

        copy_dir_filtered(tmp_src.path(), tmp_dst.path(), &gi, tmp_src.path()).unwrap();

        assert!(!tmp_dst.path().join("a.log").exists());
        assert!(tmp_dst.path().join("important.log").exists());
        assert!(tmp_dst.path().join("b.txt").exists());
    }

    // -- Multi-stage build tests --

    #[test]
    fn test_parse_from_with_alias() {
        let content = "FROM alpine:3.19 AS builder\nRUN echo hi";
        let instructions = parse_remfile(content).unwrap();
        assert_eq!(
            instructions[0],
            Instruction::From {
                image: "alpine:3.19".into(),
                alias: Some("builder".into())
            }
        );
    }

    #[test]
    fn test_parse_from_without_alias() {
        let content = "FROM alpine\nRUN echo hi";
        let instructions = parse_remfile(content).unwrap();
        assert_eq!(
            instructions[0],
            Instruction::From {
                image: "alpine".into(),
                alias: None
            }
        );
    }

    #[test]
    fn test_parse_from_as_case_insensitive() {
        let content = "FROM alpine as builder\nRUN echo hi";
        let instructions = parse_remfile(content).unwrap();
        assert_eq!(
            instructions[0],
            Instruction::From {
                image: "alpine".into(),
                alias: Some("builder".into())
            }
        );
    }

    #[test]
    fn test_parse_copy_from_stage() {
        let content = "FROM alpine\nCOPY --from=builder /app/bin /usr/bin/app";
        let instructions = parse_remfile(content).unwrap();
        assert_eq!(
            instructions[1],
            Instruction::Copy {
                src: "/app/bin".into(),
                dest: "/usr/bin/app".into(),
                from_stage: Some("builder".into()),
            }
        );
    }

    #[test]
    fn test_parse_copy_without_from() {
        let content = "FROM alpine\nCOPY src.txt /dest.txt";
        let instructions = parse_remfile(content).unwrap();
        assert_eq!(
            instructions[1],
            Instruction::Copy {
                src: "src.txt".into(),
                dest: "/dest.txt".into(),
                from_stage: None,
            }
        );
    }

    #[test]
    fn test_split_into_stages() {
        let content =
            "FROM alpine AS builder\nRUN echo build\nFROM alpine\nCOPY --from=builder /app /app";
        let instructions = parse_remfile(content).unwrap();
        let stages = split_into_stages(&instructions);
        assert_eq!(stages.len(), 2);
        assert_eq!(stages[0].alias, Some("builder".into()));
        assert_eq!(stages[0].instructions.len(), 2);
        assert_eq!(stages[1].alias, None);
        assert_eq!(stages[1].instructions.len(), 2);
    }

    #[test]
    fn test_split_single_stage() {
        let content = "FROM alpine\nRUN echo hi\nCOPY a b";
        let instructions = parse_remfile(content).unwrap();
        let stages = split_into_stages(&instructions);
        assert_eq!(stages.len(), 1);
        assert_eq!(stages[0].instructions.len(), 3);
    }
}
