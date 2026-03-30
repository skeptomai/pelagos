//! `pelagos image` — pull, list, remove, push, login, logout for OCI images.

use pelagos::image::{
    self, blob_exists, extract_layer, layer_dirs, layer_exists, list_images, load_image,
    remove_image, save_image, HealthConfig, ImageConfig, ImageManifest,
};
use std::io::{Read as _, Write};

use super::auth::{parse_docker_config, remove_docker_config, resolve_auth, write_docker_config};

// ---------------------------------------------------------------------------
// Client config helper
// ---------------------------------------------------------------------------

fn oci_client_config(registry: &str, insecure: bool) -> oci_client::client::ClientConfig {
    use oci_client::client::{ClientConfig, ClientProtocol};
    // strip port to get just the host
    let host = registry.split(':').next().unwrap_or(registry);
    let auto_insecure = host == "localhost"
        || host == "127.0.0.1"
        || host == "::1"
        || host.starts_with("192.168.")
        || host.starts_with("10.")
        || host.starts_with("172.") && {
            host.split('.')
                .nth(1)
                .and_then(|s| s.parse::<u8>().ok())
                .map(|n| (16..=31).contains(&n))
                .unwrap_or(false)
        };
    if insecure || auto_insecure {
        ClientConfig {
            protocol: ClientProtocol::HttpsExcept(vec![registry.to_string()]),
            ..Default::default()
        }
    } else {
        ClientConfig::default()
    }
}

// ---------------------------------------------------------------------------
// Public commands
// ---------------------------------------------------------------------------

/// Pull an image from an OCI registry.
pub fn cmd_image_pull(
    reference: &str,
    username: Option<&str>,
    password: Option<&str>,
    password_stdin: bool,
    insecure: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let full_ref = normalise_reference(reference);
    println!("Pulling {}...", full_ref);

    let resolved_password = resolve_password(password, password_stdin)?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(async {
        pull_image(&full_ref, username, resolved_password.as_deref(), insecure).await
    })
}

/// List all locally stored images.
pub fn cmd_image_ls(json: bool) -> Result<(), Box<dyn std::error::Error>> {
    let images = list_images();

    if json {
        println!("{}", serde_json::to_string_pretty(&images)?);
        return Ok(());
    }

    if images.is_empty() {
        println!("No images found. Use 'pelagos image pull <name>' to pull one.");
        return Ok(());
    }
    println!("{:<30} {:<10} {:<6} DIGEST", "REFERENCE", "LAYERS", "TYPE");
    for img in &images {
        let short_digest = if img.digest.len() > 19 {
            &img.digest[7..19]
        } else {
            &img.digest
        };
        let type_tag = if img
            .layer_types
            .iter()
            .any(|t| t == "application/vnd.bytecodealliance.wasm.component.layer.v0+wasm")
        {
            "component"
        } else if img.is_wasm_image() {
            "wasm"
        } else {
            "linux"
        };
        println!(
            "{:<30} {:<10} {:<6} {}",
            img.reference,
            img.layers.len(),
            type_tag,
            short_digest
        );
    }
    Ok(())
}

/// Remove a locally stored image (does not remove shared layers).
///
/// Tries the local reference first (just adds `:latest` if no tag), so that
/// locally-built images like `monitoring-grafana` are found without the
/// `docker.io/library/` prefix that `normalise_reference` adds for pulls.
pub fn cmd_image_rm(reference: &str) -> Result<(), Box<dyn std::error::Error>> {
    let local_ref = add_default_tag(reference);
    match remove_image(&local_ref) {
        Ok(()) => {
            println!("Removed image: {}", local_ref);
            return Ok(());
        }
        // ErrorKind::Other is our custom "image not found" sentinel from remove_image().
        // Any other error (e.g. PermissionDenied) is a real failure — propagate it
        // immediately rather than masking it by trying the normalised reference.
        Err(e) if e.kind() != std::io::ErrorKind::Other => {
            return Err(hint_permission(e).into());
        }
        Err(_) => {} // "not found" — fall through and try the fully-qualified reference
    }
    let full_ref = normalise_reference(reference);
    remove_image(&full_ref).map_err(hint_permission)?;
    println!("Removed image: {}", full_ref);
    Ok(())
}

/// Append a setup hint to permission-denied errors on the image store.
fn hint_permission(e: std::io::Error) -> std::io::Error {
    if e.kind() == std::io::ErrorKind::PermissionDenied {
        std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!(
                "{}\nhint: run 'sudo ./scripts/setup.sh' to make the image store \
                 writable by the pelagos group, then add yourself: \
                 sudo usermod -aG pelagos $USER",
                e
            ),
        )
    } else {
        e
    }
}

/// Push a locally stored image to an OCI registry.
pub fn cmd_image_push(
    reference: &str,
    dest: Option<&str>,
    username: Option<&str>,
    password: Option<&str>,
    password_stdin: bool,
    insecure: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    // Resolve source reference (try local-first, then fully-normalised).
    let src_ref = resolve_local_reference(reference);

    // Resolve destination reference: default = source reference.
    let dest_ref = dest
        .map(normalise_reference)
        .unwrap_or_else(|| src_ref.clone());

    println!("Pushing {} → {}...", src_ref, dest_ref);

    let resolved_password = resolve_password(password, password_stdin)?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(async {
        push_image(
            &src_ref,
            &dest_ref,
            username,
            resolved_password.as_deref(),
            insecure,
        )
        .await
    })
}

/// Log in to an OCI registry (writes `~/.docker/config.json`).
pub fn cmd_image_login(
    registry: &str,
    username: Option<&str>,
    password_stdin: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let user = match username {
        Some(u) => u.to_string(),
        None => {
            eprint!("Username: ");
            std::io::stderr().flush()?;
            let mut s = String::new();
            std::io::stdin().read_line(&mut s)?;
            s.trim().to_string()
        }
    };

    let pass = if password_stdin {
        let mut s = String::new();
        std::io::stdin().read_to_string(&mut s)?;
        s.trim().to_string()
    } else {
        // Read password without echo (fallback to stdin if rpassword isn't available).
        eprint!("Password: ");
        std::io::stderr().flush()?;
        read_password_from_tty()?
    };

    write_docker_config(registry, &user, &pass)?;
    println!("Login Succeeded ({} as {})", registry, user);
    Ok(())
}

/// Log out of an OCI registry (removes entry from `~/.docker/config.json`).
pub fn cmd_image_logout(registry: &str) -> Result<(), Box<dyn std::error::Error>> {
    remove_docker_config(registry)?;
    println!("Removed login credentials for {}", registry);
    Ok(())
}

// ---------------------------------------------------------------------------
// Internal pull implementation
// ---------------------------------------------------------------------------

async fn pull_image(
    reference: &str,
    username: Option<&str>,
    password: Option<&str>,
    insecure: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    use oci_client::{Client, Reference as OciRef};

    // For pinned (immutable) tags, skip the network entirely if the image and
    // all its layers are already in the local store. Mutable tags like `latest`
    // must always fetch the manifest to detect upstream changes.
    if !is_mutable_tag(reference) {
        if let Ok(existing) = load_image(reference) {
            let all_cached = existing.layers.iter().all(|d| layer_exists(d));
            if all_cached {
                println!("Already present: {}", reference);
                return Ok(());
            }
        }
    }

    let oci_ref: OciRef = reference
        .parse()
        .map_err(|e| format!("invalid image reference '{}': {:?}", reference, e))?;

    let registry = oci_ref.resolve_registry();
    let auth = resolve_auth(registry, username, password);

    let client = Client::new(oci_client_config(registry, insecure));

    let (manifest, digest, config_json) = client
        .pull_manifest_and_config(&oci_ref, &auth)
        .await
        .map_err(|e| format!("failed to pull manifest: {}", e))?;

    println!(
        "  Manifest: {} ({} layers)",
        &digest[..19.min(digest.len())],
        manifest.layers.len()
    );

    let config = parse_image_config(&config_json)?;

    let mut cached = 0usize;
    let mut downloaded = 0usize;
    let mut layer_digests: Vec<String> = Vec::new();
    let mut layer_types: Vec<String> = Vec::new();

    for (i, layer_desc) in manifest.layers.iter().enumerate() {
        let layer_digest = &layer_desc.digest;
        let media_type = layer_desc.media_type.as_str();
        let is_wasm = pelagos::wasm::is_wasm_media_type(media_type);

        if layer_exists(layer_digest) {
            cached += 1;
            println!(
                "  Layer {}/{}: {} (cached{})",
                i + 1,
                manifest.layers.len(),
                &layer_digest[7..19.min(layer_digest.len())],
                if is_wasm { ", wasm" } else { "" }
            );
            layer_digests.push(layer_digest.clone());
            layer_types.push(media_type.to_string());
            continue;
        }

        println!(
            "  Layer {}/{}: {} (downloading{}...)",
            i + 1,
            manifest.layers.len(),
            &layer_digest[7..19.min(layer_digest.len())],
            if is_wasm { ", wasm" } else { "" }
        );

        let mut blob_data: Vec<u8> = Vec::new();
        client
            .pull_blob(&oci_ref, layer_desc, &mut blob_data)
            .await
            .map_err(|e| format!("failed to pull layer {}: {}", layer_digest, e))?;

        // Extract the layer for container use, branching on mediaType.
        // Write to a temp file first; the blob is not persisted — overlay mounts
        // use the unpacked layer directory directly, halving on-disk image cost.
        if !layer_exists(layer_digest) {
            let mut tmp = tempfile::NamedTempFile::new()?;
            tmp.write_all(&blob_data)?;
            tmp.flush()?;
            if is_wasm {
                image::extract_wasm_layer(layer_digest, tmp.path())?;
            } else {
                extract_layer(layer_digest, tmp.path())?;
            }
        }
        layer_digests.push(layer_digest.clone());
        layer_types.push(media_type.to_string());
        downloaded += 1;
    }

    let img_manifest = ImageManifest {
        reference: reference.to_string(),
        digest,
        layers: layer_digests,
        layer_types,
        config,
    };
    save_image(&img_manifest)?;

    // Persist the raw OCI config JSON so push can reconstruct the manifest.
    image::save_oci_config(reference, &config_json)?;

    println!("Done: {} layers downloaded, {} cached", downloaded, cached);
    Ok(())
}

// ---------------------------------------------------------------------------
// Internal push implementation
// ---------------------------------------------------------------------------

async fn push_image(
    src_ref: &str,
    dest_ref: &str,
    username: Option<&str>,
    password: Option<&str>,
    insecure: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    use oci_client::client::{Config, ImageLayer};
    use oci_client::{Client, Reference as OciRef};

    let dest_oci_ref: OciRef = dest_ref
        .parse()
        .map_err(|e| format!("invalid destination reference '{}': {:?}", dest_ref, e))?;

    let registry = dest_oci_ref.resolve_registry();
    let auth = resolve_auth(registry, username, password);

    let manifest =
        load_image(src_ref).map_err(|_| format!("image '{}' not found locally", src_ref))?;

    // Load raw OCI config JSON (saved during pull or build).
    let config_json = image::load_oci_config(src_ref)
        .map_err(|_| format!("OCI config not found for '{}' — re-pull or rebuild the image to populate the blob cache", src_ref))?;

    // Build ImageLayer list from the blob store.
    // Blobs are not retained after pull (freed immediately after layer unpack).
    // Only locally-built images keep their blobs for push.
    let mut layers = Vec::with_capacity(manifest.layers.len());
    for digest in &manifest.layers {
        if !blob_exists(digest) {
            return Err(format!(
                "blob not found for layer {} — pulled images do not retain blobs; \
                 re-pull the image to export it, or use `pelagos image push` for built images",
                &digest[..19.min(digest.len())]
            )
            .into());
        }
        let data = image::load_blob(digest)?;
        println!(
            "  Layer {}: {} bytes",
            &digest[7..19.min(digest.len())],
            data.len()
        );
        layers.push(ImageLayer::oci_v1_gzip(data, None));
    }

    let config = Config::oci_v1(config_json.into_bytes(), None);

    let client = Client::new(oci_client_config(registry, insecure));
    let response = client
        .push(&dest_oci_ref, &layers, config, &auth, None)
        .await
        .map_err(|e| format!("push failed: {}", e))?;

    println!("Pushed {}", dest_ref);
    println!("  manifest: {}", response.manifest_url);
    println!("  config:   {}", response.config_url);
    Ok(())
}

// ---------------------------------------------------------------------------
// Reference helpers
// ---------------------------------------------------------------------------

/// Add `:latest` tag if the reference has no tag or digest, but do not add
/// any registry prefix.  Used for local-image operations.
fn add_default_tag(reference: &str) -> String {
    if reference.contains(':') || reference.contains('@') {
        reference.to_string()
    } else {
        format!("{}:latest", reference)
    }
}

/// Returns true if the tag is mutable (latest, absent, or a non-version word).
///
/// Mutable tags must always hit the registry to detect upstream changes.
/// Pinned version tags (e.g. `3.21`, `1.2.3`) are immutable by convention
/// and can be skipped if the image is already present locally.
fn is_mutable_tag(reference: &str) -> bool {
    let tag = if let Some(at) = reference.rfind('@') {
        // digest-pinned reference — always immutable
        let _ = at;
        return false;
    } else if let Some(colon) = reference.rfind(':') {
        &reference[colon + 1..]
    } else {
        "latest"
    };
    // "latest" or any tag that doesn't start with a digit is treated as mutable.
    tag == "latest" || !tag.chars().next().is_some_and(|c| c.is_ascii_digit())
}

/// Expand bare image names to fully qualified references.
///
/// Delegates to [`pelagos::image::normalise_reference`]; see that function for
/// the full resolution rules including `PELAGOS_DEFAULT_REGISTRY` support.
pub fn normalise_reference(reference: &str) -> String {
    pelagos::image::normalise_reference(reference)
}

/// Resolve a reference for local-image operations: try the local (un-prefixed)
/// form first, then fall back to the fully-normalised form.
fn resolve_local_reference(reference: &str) -> String {
    let local = add_default_tag(reference);
    if load_image(&local).is_ok() {
        return local;
    }
    normalise_reference(reference)
}

// ---------------------------------------------------------------------------
// Password helpers
// ---------------------------------------------------------------------------

fn resolve_password(
    password: Option<&str>,
    password_stdin: bool,
) -> Result<Option<String>, Box<dyn std::error::Error>> {
    if let Some(p) = password {
        return Ok(Some(p.to_string()));
    }
    if password_stdin {
        let mut s = String::new();
        std::io::stdin().read_to_string(&mut s)?;
        return Ok(Some(s.trim().to_string()));
    }
    Ok(None)
}

/// Read a password from /dev/tty without echo, falling back to stdin.
fn read_password_from_tty() -> Result<String, Box<dyn std::error::Error>> {
    // Try /dev/tty for no-echo input.
    if let Ok(mut tty) = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")
    {
        use std::os::unix::io::AsRawFd;
        let fd = tty.as_raw_fd();

        // Save and disable echo.
        let mut termios: libc::termios = unsafe { std::mem::zeroed() };
        let saved = unsafe {
            let ok = libc::tcgetattr(fd, &mut termios) == 0;
            ok.then_some(termios)
        };
        if let Some(saved_termios) = saved {
            let mut raw = saved_termios;
            raw.c_lflag &= !(libc::ECHO | libc::ECHOE | libc::ECHOK | libc::ECHONL);
            unsafe { libc::tcsetattr(fd, libc::TCSANOW, &raw) };
        }

        let mut pass = String::new();
        let result = tty.read_to_string(&mut pass);

        // Restore terminal.
        if let Some(saved_termios) = saved {
            unsafe { libc::tcsetattr(fd, libc::TCSANOW, &saved_termios) };
        }
        eprintln!(); // newline after hidden input

        result?;
        return Ok(pass.trim().to_string());
    }

    // Fallback: just read from stdin (password will echo).
    let mut s = String::new();
    std::io::stdin().read_line(&mut s)?;
    Ok(s.trim().to_string())
}

// ---------------------------------------------------------------------------
// Config parsing (moved from pull; also used by push round-trip tests)
// ---------------------------------------------------------------------------

/// Parse an OCI `Healthcheck` config block into a [`HealthConfig`].
///
/// OCI nanosecond durations are converted to seconds (divide by 1_000_000_000).
/// Returns `None` when the block is absent or disabled (`["NONE"]`).
fn parse_oci_healthcheck(container_config: Option<&serde_json::Value>) -> Option<HealthConfig> {
    let hc = container_config?.get("Healthcheck")?;
    let test = hc.get("Test").and_then(|v| v.as_array())?;
    if test.is_empty() {
        return None;
    }
    let first = test[0].as_str().unwrap_or("");
    if first == "NONE" {
        return None;
    }
    let cmd: Vec<String> = match first {
        "CMD" => test[1..]
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect(),
        "CMD-SHELL" => {
            let shell_cmd = test.get(1).and_then(|v| v.as_str()).unwrap_or("");
            vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                shell_cmd.to_string(),
            ]
        }
        // Bare list (non-standard, treat as CMD)
        _ => test
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect(),
    };
    if cmd.is_empty() {
        return None;
    }
    let ns_to_secs = |field: &str| -> u64 {
        hc.get(field)
            .and_then(|v| v.as_u64())
            .map(|ns| ns / 1_000_000_000)
            .unwrap_or(0)
    };
    let interval_secs = {
        let v = ns_to_secs("Interval");
        if v == 0 {
            30
        } else {
            v
        }
    };
    let timeout_secs = {
        let v = ns_to_secs("Timeout");
        if v == 0 {
            10
        } else {
            v
        }
    };
    let start_period_secs = ns_to_secs("StartPeriod");
    let retries = hc
        .get("Retries")
        .and_then(|v| v.as_u64())
        .map(|n| n as u32)
        .unwrap_or(3);
    Some(HealthConfig {
        cmd,
        interval_secs,
        timeout_secs,
        start_period_secs,
        retries,
    })
}

/// Parse the OCI image config JSON to extract Env, Cmd, Entrypoint, WorkingDir, User.
pub(crate) fn parse_image_config(
    config_json: &str,
) -> Result<ImageConfig, Box<dyn std::error::Error>> {
    let value: serde_json::Value = serde_json::from_str(config_json)
        .map_err(|e| format!("invalid image config JSON: {}", e))?;

    let container_config = value
        .get("config")
        .or_else(|| value.get("container_config"));

    let env = container_config
        .and_then(|c| c.get("Env"))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    let cmd = container_config
        .and_then(|c| c.get("Cmd"))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    let entrypoint = container_config
        .and_then(|c| c.get("Entrypoint"))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    let working_dir = container_config
        .and_then(|c| c.get("WorkingDir"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let user = container_config
        .and_then(|c| c.get("User"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let labels = container_config
        .and_then(|c| c.get("Labels"))
        .and_then(|v| v.as_object())
        .map(|obj| {
            obj.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect()
        })
        .unwrap_or_default();

    let stop_signal = container_config
        .and_then(|c| c.get("StopSignal"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    // Parse OCI Healthcheck block.
    // OCI format: { "Test": ["CMD", "arg", ...], "Interval": ns, "Timeout": ns,
    //               "StartPeriod": ns, "Retries": n }
    // "Test" variants: ["NONE"], ["CMD", ...], ["CMD-SHELL", "shell_cmd"]
    let healthcheck = parse_oci_healthcheck(container_config);

    Ok(ImageConfig {
        env,
        cmd,
        entrypoint,
        working_dir,
        user,
        labels,
        stop_signal,
        healthcheck,
    })
}

// ---------------------------------------------------------------------------
// image tag
// ---------------------------------------------------------------------------

/// Assign a new local reference to an existing image without pulling.
pub fn cmd_image_tag(source: &str, target: &str) -> Result<(), Box<dyn std::error::Error>> {
    let src_ref = resolve_local_reference(source);
    let manifest =
        load_image(&src_ref).map_err(|_| format!("image '{}' not found locally", src_ref))?;

    let config_json = image::load_oci_config(&src_ref).map_err(|_| {
        format!(
            "OCI config not found for '{}' — re-pull or rebuild to populate the blob cache",
            src_ref
        )
    })?;

    let target_ref = add_default_tag(target);
    let new_manifest = ImageManifest {
        reference: target_ref.clone(),
        digest: manifest.digest,
        layers: manifest.layers,
        layer_types: manifest.layer_types,
        config: manifest.config,
    };
    save_image(&new_manifest)?;
    image::save_oci_config(&target_ref, &config_json)?;
    println!("{} → {}", src_ref, target_ref);
    Ok(())
}

// ---------------------------------------------------------------------------
// image save
// ---------------------------------------------------------------------------

/// Save a locally stored image to an OCI Image Layout tar archive.
///
/// Output goes to `output` (a file path) or stdout if `None`.
pub fn cmd_image_save(
    reference: &str,
    output: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    // Resolve reference the same way rm/push does: local form first.
    let src_ref = resolve_local_reference(reference);
    let manifest =
        load_image(&src_ref).map_err(|_| format!("image '{}' not found locally", src_ref))?;

    let config_json = image::load_oci_config(&src_ref).map_err(|_| {
        format!(
            "OCI config not found for '{}' — re-pull or rebuild to populate the blob cache",
            src_ref
        )
    })?;
    let config_bytes = config_json.into_bytes();

    // Collect layer blobs.
    let mut layer_blobs: Vec<(String, Vec<u8>)> = Vec::new();
    for digest in &manifest.layers {
        if !blob_exists(digest) {
            return Err(format!(
                "blob not found for layer {} — pulled images do not retain blobs; \
                 re-pull the image before pushing",
                &digest[..19.min(digest.len())]
            )
            .into());
        }
        layer_blobs.push((digest.clone(), image::load_blob(digest)?));
    }

    let tar_bytes = build_oci_tar(&src_ref, &config_bytes, &layer_blobs)?;

    if let Some(path) = output {
        std::fs::write(path, &tar_bytes).map_err(|e| format!("cannot write '{}': {}", path, e))?;
        println!(
            "Saved {} ({} layers) → {}",
            src_ref,
            layer_blobs.len(),
            path
        );
    } else {
        use std::io::Write as _;
        std::io::stdout().write_all(&tar_bytes)?;
    }

    Ok(())
}

/// Build an in-memory OCI Image Layout tar archive from raw bytes.
///
/// `layer_blobs` is `(digest, compressed_bytes)` ordered bottom-to-top.
/// Returns the raw tar bytes.
pub(crate) fn build_oci_tar(
    reference: &str,
    config_bytes: &[u8],
    layer_blobs: &[(String, Vec<u8>)],
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    use sha2::{Digest as _, Sha256};

    let config_digest = format!("sha256:{:x}", Sha256::digest(config_bytes));
    let config_hex = config_digest.strip_prefix("sha256:").unwrap();

    // Build OCI manifest JSON.
    let layer_descriptors: Vec<serde_json::Value> = layer_blobs
        .iter()
        .map(|(digest, data)| {
            serde_json::json!({
                "mediaType": "application/vnd.oci.image.layer.v1.tar+gzip",
                "digest": digest,
                "size": data.len()
            })
        })
        .collect();

    let oci_manifest = serde_json::json!({
        "schemaVersion": 2,
        "mediaType": "application/vnd.oci.image.manifest.v1+json",
        "config": {
            "mediaType": "application/vnd.oci.image.config.v1+json",
            "digest": config_digest,
            "size": config_bytes.len()
        },
        "layers": layer_descriptors
    });
    let manifest_bytes = serde_json::to_vec(&oci_manifest)?;
    let manifest_digest = format!("sha256:{:x}", Sha256::digest(&manifest_bytes));
    let manifest_hex = manifest_digest.strip_prefix("sha256:").unwrap();

    // Build index.json.
    let index = serde_json::json!({
        "schemaVersion": 2,
        "mediaType": "application/vnd.oci.image.index.v1+json",
        "manifests": [{
            "mediaType": "application/vnd.oci.image.manifest.v1+json",
            "digest": manifest_digest,
            "size": manifest_bytes.len(),
            "annotations": {
                "org.opencontainers.image.ref.name": reference
            }
        }]
    });
    let index_bytes = serde_json::to_vec(&index)?;
    let oci_layout_bytes = br#"{"imageLayoutVersion":"1.0.0"}"#;

    // Write all entries into an in-memory tar.
    let mut tar_buf: Vec<u8> = Vec::new();
    {
        let mut ar = tar::Builder::new(&mut tar_buf);

        let add =
            |ar: &mut tar::Builder<&mut Vec<u8>>, path: &str, data: &[u8]| -> std::io::Result<()> {
                let mut hdr = tar::Header::new_gnu();
                hdr.set_path(path)?;
                hdr.set_size(data.len() as u64);
                hdr.set_mode(0o644);
                hdr.set_cksum();
                ar.append(&hdr, data)
            };

        add(&mut ar, "oci-layout", oci_layout_bytes)?;
        add(&mut ar, "index.json", &index_bytes)?;
        add(
            &mut ar,
            &format!("blobs/sha256/{}", manifest_hex),
            &manifest_bytes,
        )?;
        add(
            &mut ar,
            &format!("blobs/sha256/{}", config_hex),
            config_bytes,
        )?;
        for (digest, data) in layer_blobs {
            let hex = digest.strip_prefix("sha256:").unwrap_or(digest.as_str());
            add(&mut ar, &format!("blobs/sha256/{}", hex), data)?;
        }
        ar.finish()?;
    }

    Ok(tar_buf)
}

// ---------------------------------------------------------------------------
// image load
// ---------------------------------------------------------------------------

/// Load an image from an OCI Image Layout tar archive.
///
/// Input comes from `input` (a file path) or stdin if `None`.
/// If `tag` is supplied it overrides the reference stored in the archive.
pub fn cmd_image_load(
    input: Option<&str>,
    tag: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    use std::collections::HashMap;
    use std::io::Read as _;

    // Read the entire tar into memory so we can do random-access by path.
    let tar_bytes: Vec<u8> = if let Some(path) = input {
        std::fs::read(path).map_err(|e| format!("cannot read '{}': {}", path, e))?
    } else {
        let mut buf = Vec::new();
        std::io::stdin().read_to_end(&mut buf)?;
        buf
    };

    // Extract all entries into a path → bytes map.
    let mut entries: HashMap<String, Vec<u8>> = HashMap::new();
    {
        let cursor = std::io::Cursor::new(&tar_bytes);
        let mut ar = tar::Archive::new(cursor);
        for entry in ar.entries()? {
            let mut entry = entry?;
            let path = entry.path()?.to_string_lossy().into_owned();
            let mut data = Vec::new();
            entry.read_to_end(&mut data)?;
            entries.insert(path, data);
        }
    }

    // Verify oci-layout.
    let layout_data = entries
        .get("oci-layout")
        .ok_or("missing 'oci-layout' — not a valid OCI Image Layout archive")?;
    let layout: serde_json::Value = serde_json::from_slice(layout_data)?;
    if layout.get("imageLayoutVersion").and_then(|v| v.as_str()) != Some("1.0.0") {
        return Err("unsupported OCI image layout version".into());
    }

    // Parse index.json.
    let index_data = entries
        .get("index.json")
        .ok_or("missing 'index.json' in archive")?;
    let index: serde_json::Value = serde_json::from_slice(index_data)?;
    let manifests = index
        .get("manifests")
        .and_then(|v| v.as_array())
        .ok_or("index.json: missing 'manifests' array")?;
    if manifests.is_empty() {
        return Err("index.json: empty manifests array".into());
    }

    // Load each image described in the index.
    for manifest_desc in manifests {
        let manifest_digest = manifest_desc
            .get("digest")
            .and_then(|v| v.as_str())
            .ok_or("manifest descriptor missing 'digest'")?;
        let ref_annotation = manifest_desc
            .pointer("/annotations/org.opencontainers.image.ref.name")
            .and_then(|v| v.as_str());

        let manifest_hex = manifest_digest
            .strip_prefix("sha256:")
            .unwrap_or(manifest_digest);
        let manifest_key = format!("blobs/sha256/{}", manifest_hex);
        let manifest_data = entries
            .get(&manifest_key)
            .ok_or_else(|| format!("missing blob: {}", manifest_key))?;
        let oci_manifest: serde_json::Value = serde_json::from_slice(manifest_data)?;

        // Config blob.
        let config_desc = oci_manifest
            .get("config")
            .ok_or("manifest: missing 'config'")?;
        let config_digest = config_desc
            .get("digest")
            .and_then(|v| v.as_str())
            .ok_or("config descriptor: missing 'digest'")?;
        let config_hex = config_digest
            .strip_prefix("sha256:")
            .unwrap_or(config_digest);
        let config_key = format!("blobs/sha256/{}", config_hex);
        let config_data = entries
            .get(&config_key)
            .ok_or_else(|| format!("missing blob: {}", config_key))?;
        let config_json =
            std::str::from_utf8(config_data).map_err(|_| "config blob is not valid UTF-8")?;

        // Layer blobs.
        let layer_descs = oci_manifest
            .get("layers")
            .and_then(|v| v.as_array())
            .ok_or("manifest: missing 'layers' array")?;

        let mut layer_digests: Vec<String> = Vec::new();
        for (i, layer_desc) in layer_descs.iter().enumerate() {
            let layer_digest = layer_desc
                .get("digest")
                .and_then(|v| v.as_str())
                .ok_or_else(|| format!("layer {}: missing 'digest'", i))?;
            let layer_hex = layer_digest.strip_prefix("sha256:").unwrap_or(layer_digest);
            let layer_key = format!("blobs/sha256/{}", layer_hex);
            let layer_data = entries
                .get(&layer_key)
                .ok_or_else(|| format!("missing blob: {}", layer_key))?;

            if !layer_exists(layer_digest) {
                let mut tmp = tempfile::NamedTempFile::new()?;
                tmp.write_all(layer_data)?;
                tmp.flush()?;
                extract_layer(layer_digest, tmp.path())?;
            }
            layer_digests.push(layer_digest.to_string());
        }

        // Determine reference.
        let reference = if let Some(t) = tag {
            t.to_string()
        } else if let Some(r) = ref_annotation {
            r.to_string()
        } else {
            manifest_digest.to_string()
        };

        let config = parse_image_config(config_json)?;
        let img_manifest = ImageManifest {
            reference: reference.clone(),
            digest: manifest_digest.to_string(),
            layers: layer_digests,
            layer_types: Vec::new(), // loaded archives have no Wasm layer type metadata
            config,
        };
        // save_image creates image_dir; save_oci_config must come after.
        save_image(&img_manifest)?;
        image::save_oci_config(&reference, config_json)?;
        println!("Loaded {}", reference);
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Verify that `is_mutable_tag` correctly classifies tags.
    ///
    /// Mutable: `latest`, absent tag (implied latest), non-numeric tags.
    /// Immutable: version tags starting with a digit (e.g. `3.21`, `1.2.3`),
    ///            digest references (sha256:...).
    #[test]
    fn test_is_mutable_tag() {
        // Mutable — must always hit the registry
        assert!(is_mutable_tag("alpine"), "bare name implies latest");
        assert!(is_mutable_tag("alpine:latest"), "explicit latest");
        assert!(is_mutable_tag("ubuntu:rolling"), "non-numeric tag");
        assert!(is_mutable_tag("nginx:stable"), "non-numeric tag");

        // Immutable — safe to skip network if present locally
        assert!(!is_mutable_tag("alpine:3.21"), "pinned version");
        assert!(!is_mutable_tag("alpine:3.21.3"), "pinned patch version");
        assert!(!is_mutable_tag("ubuntu:22.04"), "pinned version");
        assert!(
            !is_mutable_tag("alpine@sha256:abcdef1234"),
            "digest reference"
        );
    }

    /// Verify that `build_oci_tar` produces a valid OCI Image Layout tar.
    ///
    /// Calls the pure helper directly (no disk I/O, no root required) and asserts:
    ///   - `oci-layout` is present with imageLayoutVersion = "1.0.0"
    ///   - `index.json` contains the expected reference annotation
    ///   - `blobs/sha256/<hex>` entries exist for config, each layer, and manifest
    ///   - manifest JSON references the correct config and layer digests
    #[test]
    fn test_build_oci_tar() {
        use sha2::{Digest as _, Sha256};
        use std::collections::HashMap;
        use std::io::Read as _;

        let reference = "test-save:latest";

        // Fake config JSON.
        let config_bytes =
            br#"{"config":{"Env":["PATH=/usr/bin"],"Cmd":["/bin/sh"]},"rootfs":{"type":"layers","diff_ids":[]}}"#;
        let config_digest = format!("sha256:{:x}", Sha256::digest(config_bytes.as_ref()));

        // Two fake layer blobs.
        let layer1_bytes: Vec<u8> = vec![0u8, 1, 2, 3];
        let layer1_digest = format!("sha256:{:x}", Sha256::digest(&layer1_bytes));
        let layer2_bytes: Vec<u8> = vec![4u8, 5, 6, 7];
        let layer2_digest = format!("sha256:{:x}", Sha256::digest(&layer2_bytes));

        let layer_blobs = vec![
            (layer1_digest.clone(), layer1_bytes.clone()),
            (layer2_digest.clone(), layer2_bytes.clone()),
        ];

        let tar_bytes =
            build_oci_tar(reference, config_bytes.as_ref(), &layer_blobs).expect("build_oci_tar");

        // Extract into a map.
        let cursor = std::io::Cursor::new(&tar_bytes);
        let mut ar = tar::Archive::new(cursor);
        let mut found: HashMap<String, Vec<u8>> = HashMap::new();
        for entry in ar.entries().unwrap() {
            let mut entry = entry.unwrap();
            let path = entry.path().unwrap().to_string_lossy().into_owned();
            let mut data = Vec::new();
            entry.read_to_end(&mut data).unwrap();
            found.insert(path, data);
        }

        // oci-layout
        let layout: serde_json::Value =
            serde_json::from_slice(found.get("oci-layout").expect("oci-layout")).unwrap();
        assert_eq!(layout["imageLayoutVersion"].as_str().unwrap(), "1.0.0");

        // index.json reference annotation
        let index: serde_json::Value =
            serde_json::from_slice(found.get("index.json").expect("index.json")).unwrap();
        let ref_name = index
            .pointer("/manifests/0/annotations/org.opencontainers.image.ref.name")
            .and_then(|v| v.as_str())
            .expect("ref.name annotation");
        assert_eq!(ref_name, reference);

        // Config blob
        let config_hex = config_digest.strip_prefix("sha256:").unwrap();
        assert!(
            found.contains_key(&format!("blobs/sha256/{}", config_hex)),
            "config blob missing"
        );

        // Layer blobs
        for (digest, _) in &layer_blobs {
            let hex = digest.strip_prefix("sha256:").unwrap();
            assert!(
                found.contains_key(&format!("blobs/sha256/{}", hex)),
                "layer blob {} missing",
                hex
            );
        }

        // Manifest blob
        let manifest_digest = index
            .pointer("/manifests/0/digest")
            .and_then(|v| v.as_str())
            .expect("manifest digest in index.json");
        let manifest_hex = manifest_digest.strip_prefix("sha256:").unwrap();
        assert!(
            found.contains_key(&format!("blobs/sha256/{}", manifest_hex)),
            "manifest blob missing"
        );

        // Manifest content references correct config and both layers
        let manifest: serde_json::Value = serde_json::from_slice(
            found
                .get(&format!("blobs/sha256/{}", manifest_hex))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            manifest.pointer("/config/digest").and_then(|v| v.as_str()),
            Some(config_digest.as_str())
        );
        assert_eq!(
            manifest
                .pointer("/layers/0/digest")
                .and_then(|v| v.as_str()),
            Some(layer1_digest.as_str())
        );
        assert_eq!(
            manifest
                .pointer("/layers/1/digest")
                .and_then(|v| v.as_str()),
            Some(layer2_digest.as_str())
        );
    }

    #[test]
    fn test_parse_image_config_stop_signal() {
        // StopSignal present → propagated to ImageConfig.stop_signal.
        let json = r#"{"config":{"Env":["PATH=/usr/bin"],"StopSignal":"SIGQUIT"},"rootfs":{"type":"layers","diff_ids":[]}}"#;
        let cfg = parse_image_config(json).unwrap();
        assert_eq!(cfg.stop_signal, "SIGQUIT");

        // StopSignal absent → empty string (caller treats as SIGTERM).
        let json_no_sig =
            r#"{"config":{"Env":["PATH=/usr/bin"]},"rootfs":{"type":"layers","diff_ids":[]}}"#;
        let cfg2 = parse_image_config(json_no_sig).unwrap();
        assert_eq!(cfg2.stop_signal, "");
    }

    #[test]
    fn test_parse_signal() {
        use super::super::compose::parse_signal;
        assert_eq!(parse_signal("SIGTERM"), libc::SIGTERM);
        assert_eq!(parse_signal("sigterm"), libc::SIGTERM);
        assert_eq!(parse_signal(""), libc::SIGTERM);
        assert_eq!(parse_signal("SIGQUIT"), libc::SIGQUIT);
        assert_eq!(parse_signal("SIGINT"), libc::SIGINT);
        assert_eq!(parse_signal("15"), libc::SIGTERM);
        assert_eq!(parse_signal("3"), libc::SIGQUIT);
        assert_eq!(parse_signal("9"), libc::SIGKILL);
        // Unknown string falls back to SIGTERM.
        assert_eq!(parse_signal("SIGWEIRD"), libc::SIGTERM);
    }
}
