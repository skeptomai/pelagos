//! `remora image` — pull, list, and remove OCI images.

use remora::image::{
    self, extract_layer, layer_dirs, layer_exists, list_images, load_image, remove_image,
    save_image, ImageConfig, ImageManifest,
};
use std::io::Write;

/// Pull an image from an OCI registry (anonymous auth).
pub fn cmd_image_pull(reference: &str) -> Result<(), Box<dyn std::error::Error>> {
    // Normalise bare names: "alpine" → "docker.io/library/alpine:latest"
    let full_ref = normalise_reference(reference);
    println!("Pulling {}...", full_ref);

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(async {
        pull_image(&full_ref).await
    })
}

/// List all locally stored images.
pub fn cmd_image_ls() -> Result<(), Box<dyn std::error::Error>> {
    let images = list_images();
    if images.is_empty() {
        println!("No images found. Use 'remora image pull <name>' to pull one.");
        return Ok(());
    }
    println!("{:<30} {:<15} {}", "REFERENCE", "LAYERS", "DIGEST");
    for img in &images {
        let short_digest = if img.digest.len() > 19 {
            &img.digest[7..19]
        } else {
            &img.digest
        };
        println!("{:<30} {:<15} {}", img.reference, img.layers.len(), short_digest);
    }
    Ok(())
}

/// Remove a locally stored image (does not remove shared layers).
pub fn cmd_image_rm(reference: &str) -> Result<(), Box<dyn std::error::Error>> {
    let full_ref = normalise_reference(reference);
    remove_image(&full_ref)?;
    println!("Removed image: {}", full_ref);
    Ok(())
}

/// Expand bare image names to fully qualified references.
fn normalise_reference(reference: &str) -> String {
    let r = reference.to_string();
    // Add default tag if missing
    let r = if !r.contains(':') && !r.contains('@') {
        format!("{}:latest", r)
    } else {
        r
    };
    // Add docker.io/library/ prefix for bare names
    if !r.contains('/') {
        format!("docker.io/library/{}", r)
    } else {
        r
    }
}

async fn pull_image(reference: &str) -> Result<(), Box<dyn std::error::Error>> {
    use oci_client::client::ClientConfig;
    use oci_client::secrets::RegistryAuth;
    use oci_client::{Client, Reference as OciRef};

    let oci_ref: OciRef = reference.parse()
        .map_err(|e| format!("invalid image reference '{}': {:?}", reference, e))?;

    let client = Client::new(ClientConfig::default());
    let auth = RegistryAuth::Anonymous;

    // Pull manifest + config
    let (manifest, digest, config_json) = client
        .pull_manifest_and_config(&oci_ref, &auth)
        .await
        .map_err(|e| format!("failed to pull manifest: {}", e))?;

    println!("  Manifest: {} ({} layers)", &digest[..19.min(digest.len())], manifest.layers.len());

    // Parse the image config from the raw JSON.
    let config = parse_image_config(&config_json)?;

    // Pull each layer
    let mut cached = 0usize;
    let mut downloaded = 0usize;
    for (i, layer_desc) in manifest.layers.iter().enumerate() {
        let layer_digest = &layer_desc.digest;
        if layer_exists(layer_digest) {
            cached += 1;
            println!("  Layer {}/{}: {} (cached)", i + 1, manifest.layers.len(), &layer_digest[7..19.min(layer_digest.len())]);
            continue;
        }

        println!("  Layer {}/{}: {} (downloading...)", i + 1, manifest.layers.len(), &layer_digest[7..19.min(layer_digest.len())]);

        // Download to a tempfile, then extract.
        let mut tmp = tempfile::NamedTempFile::new()?;
        let mut blob_data: Vec<u8> = Vec::new();
        client
            .pull_blob(&oci_ref, layer_desc, &mut blob_data)
            .await
            .map_err(|e| format!("failed to pull layer {}: {}", layer_digest, e))?;
        tmp.write_all(&blob_data)?;
        tmp.flush()?;

        extract_layer(layer_digest, tmp.path())?;
        downloaded += 1;
    }

    // Collect layer digests in order (bottom to top, matching manifest order).
    let layer_digests: Vec<String> = manifest.layers.iter().map(|l| l.digest.clone()).collect();

    // Save image metadata.
    let img_manifest = ImageManifest {
        reference: reference.to_string(),
        digest,
        layers: layer_digests,
        config,
    };
    save_image(&img_manifest)?;

    println!("Done: {} layers downloaded, {} cached", downloaded, cached);
    Ok(())
}

/// Parse the OCI image config JSON to extract Env, Cmd, Entrypoint, WorkingDir, User.
fn parse_image_config(config_json: &str) -> Result<ImageConfig, Box<dyn std::error::Error>> {
    let value: serde_json::Value = serde_json::from_str(config_json)
        .map_err(|e| format!("invalid image config JSON: {}", e))?;

    let container_config = value.get("config").or_else(|| value.get("container_config"));

    let env = container_config
        .and_then(|c| c.get("Env"))
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_default();

    let cmd = container_config
        .and_then(|c| c.get("Cmd"))
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_default();

    let entrypoint = container_config
        .and_then(|c| c.get("Entrypoint"))
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
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

    Ok(ImageConfig {
        env,
        cmd,
        entrypoint,
        working_dir,
        user,
    })
}
