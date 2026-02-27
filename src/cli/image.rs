//! `remora image` — pull, list, remove, push, login, logout for OCI images.

use remora::image::{
    self, blob_exists, extract_layer, layer_dirs, layer_exists, list_images, load_image,
    remove_image, save_image, ImageConfig, ImageManifest,
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
        println!("No images found. Use 'remora image pull <name>' to pull one.");
        return Ok(());
    }
    println!("{:<30} {:<15} DIGEST", "REFERENCE", "LAYERS");
    for img in &images {
        let short_digest = if img.digest.len() > 19 {
            &img.digest[7..19]
        } else {
            &img.digest
        };
        println!(
            "{:<30} {:<15} {}",
            img.reference,
            img.layers.len(),
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
    if remove_image(&local_ref).is_ok() {
        println!("Removed image: {}", local_ref);
        return Ok(());
    }
    let full_ref = normalise_reference(reference);
    remove_image(&full_ref)?;
    println!("Removed image: {}", full_ref);
    Ok(())
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
    for (i, layer_desc) in manifest.layers.iter().enumerate() {
        let layer_digest = &layer_desc.digest;
        if layer_exists(layer_digest) && blob_exists(layer_digest) {
            cached += 1;
            println!(
                "  Layer {}/{}: {} (cached)",
                i + 1,
                manifest.layers.len(),
                &layer_digest[7..19.min(layer_digest.len())]
            );
            continue;
        }

        println!(
            "  Layer {}/{}: {} (downloading...)",
            i + 1,
            manifest.layers.len(),
            &layer_digest[7..19.min(layer_digest.len())]
        );

        let mut blob_data: Vec<u8> = Vec::new();
        client
            .pull_blob(&oci_ref, layer_desc, &mut blob_data)
            .await
            .map_err(|e| format!("failed to pull layer {}: {}", layer_digest, e))?;

        // Persist the raw blob for future push operations.
        image::save_blob(layer_digest, &blob_data)?;

        // Extract the layer for container use.
        if !layer_exists(layer_digest) {
            let mut tmp = tempfile::NamedTempFile::new()?;
            tmp.write_all(&blob_data)?;
            tmp.flush()?;
            extract_layer(layer_digest, tmp.path())?;
        }
        downloaded += 1;
    }

    let layer_digests: Vec<String> = manifest.layers.iter().map(|l| l.digest.clone()).collect();

    let img_manifest = ImageManifest {
        reference: reference.to_string(),
        digest,
        layers: layer_digests,
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
    let mut layers = Vec::with_capacity(manifest.layers.len());
    for digest in &manifest.layers {
        if !blob_exists(digest) {
            return Err(format!(
                "blob not found for layer {} — re-pull or rebuild the image to populate the blob cache",
                &digest[..19.min(digest.len())]
            ).into());
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

/// Expand bare image names to fully qualified references.
pub fn normalise_reference(reference: &str) -> String {
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

    Ok(ImageConfig {
        env,
        cmd,
        entrypoint,
        working_dir,
        user,
        labels,
    })
}
