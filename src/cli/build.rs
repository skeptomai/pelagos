//! `pelagos build` — build an image from a Remfile.

use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, clap::Args)]
pub struct BuildArgs {
    /// Tag for the built image (e.g. myapp:latest)
    #[clap(long, short = 't')]
    pub tag: String,

    /// Path to Remfile (default: Remfile in context dir)
    #[clap(long, short = 'f')]
    pub file: Option<String>,

    /// Network mode for RUN steps: bridge (root) or pasta (rootless)
    #[clap(long, default_value = "auto")]
    pub network: String,

    /// Disable build cache (re-run all steps)
    #[clap(long)]
    pub no_cache: bool,

    /// DNS backend: builtin (default) or dnsmasq
    #[clap(long = "dns-backend", value_name = "BACKEND")]
    pub dns_backend: Option<String>,

    /// Set build-time variables (e.g. --build-arg VERSION=1.0)
    #[clap(long = "build-arg")]
    pub build_arg: Vec<String>,

    /// Build context directory (default: current directory)
    #[clap(default_value = ".")]
    pub context: String,
}

pub fn cmd_build(args: BuildArgs) -> Result<(), Box<dyn std::error::Error>> {
    use pelagos::build;
    use pelagos::network::NetworkMode;

    // Set DNS backend env var before any DNS calls so active_backend() picks it up.
    if let Some(ref backend) = args.dns_backend {
        // SAFETY: called early in single-threaded CLI startup, before spawning threads.
        unsafe { std::env::set_var("PELAGOS_DNS_BACKEND", backend) };
    }

    let context_dir = PathBuf::from(&args.context)
        .canonicalize()
        .map_err(|e| format!("cannot access build context '{}': {}", args.context, e))?;

    // Determine Remfile path.
    let remfile_path = if let Some(ref f) = args.file {
        PathBuf::from(f)
    } else {
        context_dir.join("Remfile")
    };

    if !remfile_path.is_file() {
        return Err(format!(
            "Remfile not found: {} (use -f to specify a different path)",
            remfile_path.display()
        )
        .into());
    }

    let content = std::fs::read_to_string(&remfile_path)?;
    let instructions = build::parse_remfile(&content)?;

    if instructions.is_empty() {
        return Err("Remfile is empty".into());
    }

    // Determine network mode.
    let network_mode = match args.network.as_str() {
        "bridge" => NetworkMode::Bridge,
        "pasta" => NetworkMode::Pasta,
        "none" => NetworkMode::Loopback,
        "auto" => {
            if pelagos::paths::is_rootless() {
                NetworkMode::Pasta
            } else {
                NetworkMode::Bridge
            }
        }
        name => {
            // Check if it's a named network.
            let config = pelagos::paths::network_config_dir(name).join("config.json");
            if config.exists() {
                NetworkMode::BridgeNamed(name.to_string())
            } else {
                return Err(format!(
                    "unknown network '{}' — use a mode (none, bridge, pasta, auto) \
                     or create it first: pelagos network create {} --subnet CIDR",
                    name, name
                )
                .into());
            }
        }
    };

    // Parse --build-arg KEY=VALUE pairs into a map.
    let mut build_args_map = HashMap::new();
    for arg in &args.build_arg {
        if let Some((k, v)) = arg.split_once('=') {
            build_args_map.insert(k.to_string(), v.to_string());
        } else {
            // Bare name with no value — use empty string (matches Docker behaviour).
            build_args_map.insert(arg.clone(), String::new());
        }
    }

    eprintln!("Building {} from {}", args.tag, remfile_path.display());

    let manifest = build::execute_build(
        &instructions,
        &context_dir,
        &args.tag,
        network_mode,
        !args.no_cache,
        &build_args_map,
        Some(&|reference| {
            super::image::cmd_image_pull(reference, None, None, false, false)
                .map_err(|e| e.to_string())
        }),
    )
    .map_err(|e| {
        // Propagate as-is, but add a setup hint when the layer store is not writable.
        let msg = e.to_string();
        if msg.contains("Permission denied") || msg.contains("os error 13") {
            format!(
                "{}\nhint: the pelagos layer store requires write access.\n\
                 Run 'sudo ./scripts/setup.sh' to fix permissions, or build with sudo.",
                msg
            )
        } else {
            msg
        }
    })?;

    eprintln!(
        "Successfully built {} ({} layers)",
        args.tag,
        manifest.layers.len()
    );

    Ok(())
}
