#![allow(unused_imports)]

mod cli;

use clap::{Parser, Subcommand};
use log::{error, info};

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
enum CliCommand {
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
enum RootfsCmd {
    /// Import a local directory as a named rootfs image
    Import {
        /// Name for the rootfs image
        name: String,
        /// Path to the rootfs directory
        path: String,
    },
    /// List available rootfs images
    Ls,
    /// Remove a rootfs image (removes the symlink, not the directory)
    Rm {
        /// Name of the rootfs image
        name: String,
    },
}

#[derive(Subcommand, Debug)]
enum VolumeCmd {
    /// Create a named volume
    Create {
        /// Volume name
        name: String,
    },
    /// List named volumes
    Ls,
    /// Remove a named volume and its contents
    Rm {
        /// Volume name
        name: String,
    },
}

#[derive(Subcommand, Debug)]
enum ImageCmd {
    /// Pull an image from an OCI registry
    Pull {
        /// Image reference (e.g. alpine, alpine:3.19, docker.io/library/alpine:latest)
        reference: String,
    },
    /// List locally stored images
    Ls,
    /// Remove a locally stored image
    Rm {
        /// Image reference
        reference: String,
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
        CliCommand::Run(args) => cli::run::cmd_run(*args),
        CliCommand::Exec(args) => cli::exec::cmd_exec(args),
        CliCommand::Ps { all } => cli::ps::cmd_ps(all),
        CliCommand::Stop { name } => cli::stop::cmd_stop(&name),
        CliCommand::Rm { name, force } => cli::rm::cmd_rm(&name, force),
        CliCommand::Logs { name, follow } => cli::logs::cmd_logs(&name, follow),

        // Rootfs
        CliCommand::Rootfs { cmd } => match cmd {
            RootfsCmd::Import { name, path } => cli::rootfs::cmd_rootfs_import(&name, &path),
            RootfsCmd::Ls => cli::rootfs::cmd_rootfs_ls(),
            RootfsCmd::Rm { name } => cli::rootfs::cmd_rootfs_rm(&name),
        },

        // Volume
        CliCommand::Volume { cmd } => match cmd {
            VolumeCmd::Create { name } => cli::volume::cmd_volume_create(&name),
            VolumeCmd::Ls => cli::volume::cmd_volume_ls(),
            VolumeCmd::Rm { name } => cli::volume::cmd_volume_rm(&name),
        },

        // Image
        CliCommand::Image { cmd } => match cmd {
            ImageCmd::Pull { reference } => cli::image::cmd_image_pull(&reference),
            ImageCmd::Ls => cli::image::cmd_image_ls(),
            ImageCmd::Rm { reference } => cli::image::cmd_image_rm(&reference),
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
