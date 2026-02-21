#![allow(unused_imports)]

mod cli;

pub(crate) use clap::{Parser, Subcommand};
pub(crate) use log::error;
pub(crate) use std::fmt;

// ---------------------------------------------------------------------------
// Output format enum (shared by all list commands)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
enum OutputFormat {
    Table = 0,
    Json = 1,
}

impl std::str::FromStr for OutputFormat {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "table" => Ok(OutputFormat::Table),
            "json" => Ok(OutputFormat::Json),
            other => Err(format!(
                "unknown format '{}': expected 'table' or 'json'",
                other
            )),
        }
    }
}

impl std::fmt::Display for OutputFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OutputFormat::Table => {
                write!(f, "table")
            }
            OutputFormat::Json => {
                write!(f, "json")
            }
        }
    }
}

// ---------------------------------------------------------------------------
// CLI structure
// ---------------------------------------------------------------------------

#[derive(Parser, Debug)]
#[clap(
    author,
    version,
    about = "Remora container runtime",
    long_about = None,
)]
struct Cli {
    #[clap(subcommand)]
    command: CliCommand,
}

#[derive(Subcommand, Debug)]
pub(crate) enum CliCommand {
    // ── Container lifecycle ────────────────────────────────────────────────
    /// Create and start a container
    Run(Box<cli::run::RunArgs>),

    /// Run a command in a running container
    Exec(cli::exec::ExecArgs),

    /// List containers
    Ps {
        /// Show all containers (default: only running)
        #[clap(long, short = 'a')]
        all: bool,
        /// Output format: table or json
        #[clap(long, default_value = "table")]
        format: OutputFormat,
    },

    /// Send SIGTERM to a running container
    Stop {
        /// Container name
        name: String,
    },

    /// Remove a container
    Rm {
        /// Container name
        name: String,
        /// Kill and remove even if running
        #[clap(long, short = 'f')]
        force: bool,
    },

    /// Print container logs
    Logs {
        /// Container name
        name: String,
        /// Follow log output
        #[clap(long, short = 'f')]
        follow: bool,
    },

    // ── Image build ─────────────────────────────────────────────────────
    /// Build an image from a Remfile
    Build(cli::build::BuildArgs),

    // ── Rootfs management ─────────────────────────────────────────────────
    /// Manage the rootfs image store
    Rootfs {
        #[clap(subcommand)]
        cmd: RootfsCmd,
    },

    // ── Volume management ─────────────────────────────────────────────────
    /// Manage named volumes
    Volume {
        #[clap(subcommand)]
        cmd: VolumeCmd,
    },

    // ── Image management ─────────────────────────────────────────────────
    /// Manage OCI images
    Image {
        #[clap(subcommand)]
        cmd: ImageCmd,
    },

    // ── Network management ──────────────────────────────────────────────
    /// Manage named networks
    Network {
        #[clap(subcommand)]
        cmd: NetworkCmd,
    },

    // ── Container management ─────────────────────────────────────────────
    /// Manage containers
    Container {
        #[clap(subcommand)]
        cmd: ContainerCmd,
    },

    // ── OCI lifecycle (unchanged) ─────────────────────────────────────────
    /// OCI lifecycle: create a container (machine interface)
    Create {
        id: String,
        bundle: std::path::PathBuf,
    },
    /// OCI lifecycle: start a created container
    Start { id: String },
    /// OCI lifecycle: print container state as JSON
    State { id: String },
    /// OCI lifecycle: send a signal to a container
    Kill {
        id: String,
        #[clap(default_value = "SIGTERM")]
        signal: String,
    },
    /// OCI lifecycle: delete a stopped container's state directory
    Delete { id: String },
}

#[derive(Subcommand, Debug)]
pub(crate) enum ContainerCmd {
    /// List containers
    Ls {
        /// Show all containers (default: only running)
        #[clap(long, short = 'a')]
        all: bool,
        /// Output format: table or json
        #[clap(long, default_value = "table")]
        format: OutputFormat,
    },
    /// Show detailed information about a container (JSON)
    Inspect {
        /// Container name
        name: String,
    },
    /// Send SIGTERM to a running container
    Stop {
        /// Container name
        name: String,
    },
    /// Remove a container
    Rm {
        /// Container name
        name: String,
        /// Kill and remove even if running
        #[clap(long, short = 'f')]
        force: bool,
    },
    /// Print container logs
    Logs {
        /// Container name
        name: String,
        /// Follow log output
        #[clap(long, short = 'f')]
        follow: bool,
    },
}

#[derive(Subcommand, Debug)]
pub(crate) enum RootfsCmd {
    /// Import a local directory as a named rootfs image
    Import {
        /// Name for the rootfs image
        name: String,
        /// Path to the rootfs directory
        path: String,
    },
    /// List available rootfs images
    Ls {
        /// Output format: table or json
        #[clap(long, default_value = "table")]
        format: OutputFormat,
    },
    /// Remove a rootfs image (removes the symlink, not the directory)
    Rm {
        /// Name of the rootfs image
        name: String,
    },
}

#[derive(Subcommand, Debug)]
pub(crate) enum VolumeCmd {
    /// Create a named volume
    Create {
        /// Volume name
        name: String,
    },
    /// List named volumes
    Ls {
        /// Output format: table or json
        #[clap(long, default_value = "table")]
        format: OutputFormat,
    },
    /// Remove a named volume and its contents
    Rm {
        /// Volume name
        name: String,
    },
}

#[derive(Subcommand, Debug)]
pub(crate) enum ImageCmd {
    /// Pull an image from an OCI registry
    Pull {
        /// Image reference (e.g. alpine, alpine:3.19, docker.io/library/alpine:latest)
        reference: String,
    },
    /// List locally stored images
    Ls {
        /// Output format: table or json
        #[clap(long, default_value = "table")]
        format: OutputFormat,
    },
    /// Remove a locally stored image
    Rm {
        /// Image reference
        reference: String,
    },
}

#[derive(Subcommand, Debug)]
pub(crate) enum NetworkCmd {
    /// Create a named network with a subnet
    Create {
        /// Network name (alphanumeric + hyphen, max 12 chars)
        name: String,
        /// Subnet in CIDR notation (e.g. 10.88.1.0/24)
        #[clap(long)]
        subnet: String,
    },
    /// List networks
    Ls {
        /// Output format: table or json
        #[clap(long, default_value = "table")]
        format: OutputFormat,
    },
    /// Remove a network
    Rm {
        /// Network name
        name: String,
    },
    /// Show detailed information about a network (JSON)
    Inspect {
        /// Network name
        name: String,
    },
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

fn main() {
    env_logger::init();

    let cli = Cli::parse();

    let result: Result<(), Box<dyn std::error::Error>> = match cli.command {
        // Container lifecycle
        CliCommand::Build(args) => cli::build::cmd_build(args),
        CliCommand::Run(args) => cli::run::cmd_run(*args),
        CliCommand::Exec(args) => cli::exec::cmd_exec(args),
        CliCommand::Ps { all, format } => cli::ps::cmd_ps(all, format == OutputFormat::Json),
        CliCommand::Stop { name } => cli::stop::cmd_stop(&name),
        CliCommand::Rm { name, force } => cli::rm::cmd_rm(&name, force),
        CliCommand::Logs { name, follow } => cli::logs::cmd_logs(&name, follow),

        // Container (noun subcommand)
        CliCommand::Container { cmd } => match cmd {
            ContainerCmd::Ls { all, format } => cli::ps::cmd_ps(all, format == OutputFormat::Json),
            ContainerCmd::Inspect { name } => cli::ps::cmd_inspect(&name),
            ContainerCmd::Stop { name } => cli::stop::cmd_stop(&name),
            ContainerCmd::Rm { name, force } => cli::rm::cmd_rm(&name, force),
            ContainerCmd::Logs { name, follow } => cli::logs::cmd_logs(&name, follow),
        },

        // Rootfs
        CliCommand::Rootfs { cmd } => match cmd {
            RootfsCmd::Import { name, path } => cli::rootfs::cmd_rootfs_import(&name, &path),
            RootfsCmd::Ls { format } => cli::rootfs::cmd_rootfs_ls(format == OutputFormat::Json),
            RootfsCmd::Rm { name } => cli::rootfs::cmd_rootfs_rm(&name),
        },

        // Volume
        CliCommand::Volume { cmd } => match cmd {
            VolumeCmd::Create { name } => cli::volume::cmd_volume_create(&name),
            VolumeCmd::Ls { format } => cli::volume::cmd_volume_ls(format == OutputFormat::Json),
            VolumeCmd::Rm { name } => cli::volume::cmd_volume_rm(&name),
        },

        // Image
        CliCommand::Image { cmd } => match cmd {
            ImageCmd::Pull { reference } => cli::image::cmd_image_pull(&reference),
            ImageCmd::Ls { format } => cli::image::cmd_image_ls(format == OutputFormat::Json),
            ImageCmd::Rm { reference } => cli::image::cmd_image_rm(&reference),
        },

        // Network
        CliCommand::Network { cmd } => match cmd {
            NetworkCmd::Create { name, subnet } => cli::network::cmd_network_create(&name, &subnet),
            NetworkCmd::Ls { format } => cli::network::cmd_network_ls(format == OutputFormat::Json),
            NetworkCmd::Rm { name } => cli::network::cmd_network_rm(&name),
            NetworkCmd::Inspect { name } => cli::network::cmd_network_inspect(&name),
        },

        // OCI lifecycle
        CliCommand::Create { id, bundle } => {
            remora::oci::cmd_create(&id, &bundle).map_err(|e| e.to_string().into())
        }
        CliCommand::Start { id } => remora::oci::cmd_start(&id).map_err(|e| e.to_string().into()),
        CliCommand::State { id } => remora::oci::cmd_state(&id).map_err(|e| e.to_string().into()),
        CliCommand::Kill { id, signal } => {
            remora::oci::cmd_kill(&id, &signal).map_err(|e| e.to_string().into())
        }
        CliCommand::Delete { id } => remora::oci::cmd_delete(&id).map_err(|e| e.to_string().into()),
    };

    if let Err(e) = result {
        eprintln!("remora: error: {}", e);
        std::process::exit(1);
    }
}
