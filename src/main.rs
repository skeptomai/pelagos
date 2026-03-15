#![allow(unused_imports)]

mod cli;

pub(crate) use clap::{Parser, Subcommand};
pub(crate) use log::error;
pub(crate) use std::fmt;
use std::{fmt::Display, str::FromStr};

// ---------------------------------------------------------------------------
// Output format enum (shared by all list commands)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
enum OutputFormat {
    Table = 0,
    Json = 1,
}

impl FromStr for OutputFormat {
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

impl Display for OutputFormat {
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
    about = "Pelagos container runtime",
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
        /// Shorthand for --format json; takes precedence if both are given
        #[clap(long)]
        json: bool,
        /// Filter containers (e.g. label=env=staging, label=managed, status=running)
        #[clap(long = "filter")]
        filter: Vec<String>,
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

    // ── Compose ──────────────────────────────────────────────────────────
    /// Multi-service orchestration
    Compose {
        #[clap(subcommand)]
        cmd: cli::compose::ComposeCmd,
    },

    // ── Container management ─────────────────────────────────────────────
    /// Manage containers
    Container {
        #[clap(subcommand)]
        cmd: ContainerCmd,
    },

    // ── Cleanup ────────────────────────────────────────────────────────────
    /// Remove stale network namespaces, overlay dirs, and temp dirs from dead containers
    Cleanup,

    // ── OCI lifecycle ─────────────────────────────────────────────────────
    /// OCI lifecycle: create a container (machine interface)
    Create {
        /// Container ID
        id: String,
        /// Path to the OCI bundle directory (overrides positional bundle arg)
        #[clap(long, short = 'b')]
        bundle: Option<std::path::PathBuf>,
        /// Path to a Unix socket for console I/O (when process.terminal is true)
        #[clap(long)]
        console_socket: Option<std::path::PathBuf>,
        /// Write the container PID to this file after create
        #[clap(long)]
        pid_file: Option<std::path::PathBuf>,
        /// Bundle path as a positional arg (deprecated; use --bundle)
        #[clap(hide = true)]
        bundle_positional: Option<std::path::PathBuf>,
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
    Delete {
        id: String,
        /// Force-delete even if container is still running (kills it first)
        #[clap(long)]
        force: bool,
    },
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
        /// Shorthand for --format json; takes precedence if both are given
        #[clap(long)]
        json: bool,
        /// Filter containers (e.g. label=env=staging, label=managed, status=running)
        #[clap(long = "filter")]
        filter: Vec<String>,
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
        /// Shorthand for --format json; takes precedence if both are given
        #[clap(long)]
        json: bool,
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
        /// Registry username
        #[clap(long, short = 'u')]
        username: Option<String>,
        /// Registry password
        #[clap(long)]
        password: Option<String>,
        /// Read password from stdin
        #[clap(long)]
        password_stdin: bool,
        /// Allow insecure (HTTP) registries
        #[clap(long)]
        insecure: bool,
    },
    /// List locally stored images
    Ls {
        /// Output format: table or json
        #[clap(long, default_value = "table")]
        format: OutputFormat,
        /// Shorthand for --format json; takes precedence if both are given
        #[clap(long)]
        json: bool,
    },
    /// Remove a locally stored image
    Rm {
        /// Image reference
        reference: String,
    },
    /// Push a locally stored image to an OCI registry
    Push {
        /// Image reference (local)
        reference: String,
        /// Push to a different destination (default: same reference)
        #[clap(long)]
        dest: Option<String>,
        /// Registry username
        #[clap(long, short = 'u')]
        username: Option<String>,
        /// Registry password
        #[clap(long)]
        password: Option<String>,
        /// Read password from stdin
        #[clap(long)]
        password_stdin: bool,
        /// Allow insecure (HTTP) registries
        #[clap(long)]
        insecure: bool,
    },
    /// Log in to an OCI registry (writes ~/.docker/config.json)
    Login {
        /// Registry hostname (e.g. ghcr.io, docker.io)
        registry: String,
        /// Registry username
        #[clap(long, short = 'u')]
        username: Option<String>,
        /// Read password from stdin
        #[clap(long)]
        password_stdin: bool,
    },
    /// Log out of an OCI registry
    Logout {
        /// Registry hostname
        registry: String,
    },
    /// Tag a locally stored image with a new reference
    Tag {
        /// Source image reference
        source: String,
        /// New reference to assign
        target: String,
    },
    /// Save a locally stored image to an OCI Image Layout tar archive
    Save {
        /// Image reference (e.g. alpine:latest)
        reference: String,
        /// Output file path (default: stdout)
        #[clap(long, short = 'o')]
        output: Option<String>,
    },
    /// Load an image from an OCI Image Layout tar archive
    Load {
        /// Input file path (default: stdin)
        #[clap(long, short = 'i')]
        input: Option<String>,
        /// Tag to apply to the loaded image (overrides annotation in archive)
        #[clap(long)]
        tag: Option<String>,
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
        /// Shorthand for --format json; takes precedence if both are given
        #[clap(long)]
        json: bool,
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
        CliCommand::Ps {
            all,
            format,
            json,
            filter,
        } => cli::ps::cmd_ps(all, json || format == OutputFormat::Json, &filter),
        CliCommand::Stop { name } => cli::stop::cmd_stop(&name),
        CliCommand::Rm { name, force } => cli::rm::cmd_rm(&name, force),
        CliCommand::Logs { name, follow } => cli::logs::cmd_logs(&name, follow),

        // Container (noun subcommand)
        CliCommand::Container { cmd } => match cmd {
            ContainerCmd::Ls {
                all,
                format,
                json,
                filter,
            } => cli::ps::cmd_ps(all, json || format == OutputFormat::Json, &filter),
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
            VolumeCmd::Ls { format, json } => {
                cli::volume::cmd_volume_ls(json || format == OutputFormat::Json)
            }
            VolumeCmd::Rm { name } => cli::volume::cmd_volume_rm(&name),
        },

        // Image
        CliCommand::Image { cmd } => match cmd {
            ImageCmd::Pull {
                reference,
                username,
                password,
                password_stdin,
                insecure,
            } => cli::image::cmd_image_pull(
                &reference,
                username.as_deref(),
                password.as_deref(),
                password_stdin,
                insecure,
            ),
            ImageCmd::Ls { format, json } => {
                cli::image::cmd_image_ls(json || format == OutputFormat::Json)
            }
            ImageCmd::Rm { reference } => cli::image::cmd_image_rm(&reference),
            ImageCmd::Push {
                reference,
                dest,
                username,
                password,
                password_stdin,
                insecure,
            } => cli::image::cmd_image_push(
                &reference,
                dest.as_deref(),
                username.as_deref(),
                password.as_deref(),
                password_stdin,
                insecure,
            ),
            ImageCmd::Login {
                registry,
                username,
                password_stdin,
            } => cli::image::cmd_image_login(&registry, username.as_deref(), password_stdin),
            ImageCmd::Logout { registry } => cli::image::cmd_image_logout(&registry),
            ImageCmd::Tag { source, target } => cli::image::cmd_image_tag(&source, &target),
            ImageCmd::Save { reference, output } => {
                cli::image::cmd_image_save(&reference, output.as_deref())
            }
            ImageCmd::Load { input, tag } => {
                cli::image::cmd_image_load(input.as_deref(), tag.as_deref())
            }
        },

        // Network
        CliCommand::Network { cmd } => match cmd {
            NetworkCmd::Create { name, subnet } => cli::network::cmd_network_create(&name, &subnet),
            NetworkCmd::Ls { format, json } => {
                cli::network::cmd_network_ls(json || format == OutputFormat::Json)
            }
            NetworkCmd::Rm { name } => cli::network::cmd_network_rm(&name),
            NetworkCmd::Inspect { name } => cli::network::cmd_network_inspect(&name),
        },

        // Compose
        CliCommand::Compose { cmd } => cli::compose::cmd_compose(cmd),

        // Cleanup
        CliCommand::Cleanup => cli::cleanup::cmd_cleanup(),

        // OCI lifecycle
        CliCommand::Create {
            id,
            bundle,
            bundle_positional,
            console_socket,
            pid_file,
        } => match bundle.or(bundle_positional) {
            None => Err("pelagos create: --bundle <path> is required".into()),
            Some(bundle_path) => pelagos::oci::cmd_create(
                &id,
                &bundle_path,
                console_socket.as_deref(),
                pid_file.as_deref(),
            )
            .map_err(|e| e.to_string().into()),
        },
        CliCommand::Start { id } => {
            // Dispatch: pelagos container restart takes priority over OCI lifecycle.
            // A pelagos container state lives at /run/pelagos/containers/<name>/state.json;
            // an OCI container state lives at /run/pelagos/<id>/state.json (different dir).
            if cli::container_state_exists(&id) {
                cli::start::cmd_start(&id)
            } else {
                pelagos::oci::cmd_start(&id).map_err(|e| e.to_string().into())
            }
        }
        CliCommand::State { id } => pelagos::oci::cmd_state(&id).map_err(|e| e.to_string().into()),
        CliCommand::Kill { id, signal } => {
            pelagos::oci::cmd_kill(&id, &signal).map_err(|e| e.to_string().into())
        }
        CliCommand::Delete { id, force } => {
            if force {
                pelagos::oci::cmd_delete_force(&id).map_err(|e| e.to_string().into())
            } else {
                pelagos::oci::cmd_delete(&id).map_err(|e| e.to_string().into())
            }
        }
    };

    let code = match result {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("pelagos: error: {}", e);
            1
        }
    };
    std::process::exit(code);
}
