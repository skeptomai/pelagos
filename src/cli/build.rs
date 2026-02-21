//! `remora build` — build an image from a Remfile.

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

    /// Build context directory (default: current directory)
    #[clap(default_value = ".")]
    pub context: String,
}

pub fn cmd_build(args: BuildArgs) -> Result<(), Box<dyn std::error::Error>> {
    use remora::build;
    use remora::network::NetworkMode;

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
            if remora::paths::is_rootless() {
                NetworkMode::Pasta
            } else {
                NetworkMode::Bridge
            }
        }
        other => return Err(format!("unknown network mode: {}", other).into()),
    };

    eprintln!("Building {} from {}", args.tag, remfile_path.display());

    let manifest = build::execute_build(
        &instructions,
        &context_dir,
        &args.tag,
        network_mode,
        !args.no_cache,
    )?;

    eprintln!(
        "Successfully built {} ({} layers)",
        args.tag,
        manifest.layers.len()
    );

    Ok(())
}
