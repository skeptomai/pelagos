//! Docker Engine API server that delegates to the `pelagos` CLI.
//! Linux-only binary.

#[cfg(target_os = "linux")]
mod handlers;
#[cfg(target_os = "linux")]
mod pelagos_state;
#[cfg(target_os = "linux")]
mod state;
#[cfg(target_os = "linux")]
mod types;

fn main() {
    #[cfg(not(target_os = "linux"))]
    {
        eprintln!("pelagos-dockerd only runs on Linux");
        std::process::exit(1);
    }
    #[cfg(target_os = "linux")]
    linux_run();
}

#[cfg(target_os = "linux")]
#[derive(clap::Parser)]
#[clap(name = "pelagos-dockerd", about = "Docker Engine API shim for pelagos")]
struct Args {
    /// Unix socket path to listen on.
    #[clap(long, default_value = "/var/run/pelagos-dockerd.sock")]
    socket: String,
    /// Path to the pelagos binary.
    #[clap(long, default_value = "pelagos")]
    pelagos_bin: String,
}

#[cfg(target_os = "linux")]
fn linux_run() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    use clap::Parser;
    let args = Args::parse();

    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    if let Err(e) = rt.block_on(async_run(args)) {
        log::error!("{}", e);
        std::process::exit(1);
    }
}

#[cfg(target_os = "linux")]
async fn async_run(args: Args) -> Result<(), Box<dyn std::error::Error>> {
    use hyper::server::conn::http1;
    use hyper_util::rt::TokioIo;
    use hyper_util::service::TowerToHyperService;
    use std::os::unix::fs::PermissionsExt;
    use tokio::net::UnixListener;
    use tower::Service;

    if std::path::Path::new(&args.socket).exists() {
        std::fs::remove_file(&args.socket)?;
    }
    if let Some(parent) = std::path::Path::new(&args.socket).parent() {
        std::fs::create_dir_all(parent)?;
    }

    let app_state = state::AppState::new_with_bin(args.pelagos_bin.clone());
    let mut make_service = handlers::router(app_state).into_make_service();

    let listener = UnixListener::bind(&args.socket)?;
    std::fs::set_permissions(&args.socket, std::fs::Permissions::from_mode(0o660))?;

    log::info!("listening on {}", args.socket);

    loop {
        let (stream, _) = listener.accept().await?;
        let io = TokioIo::new(stream);
        let svc = make_service.call(()).await?;
        tokio::spawn(async move {
            let svc = TowerToHyperService::new(svc);
            if let Err(e) = http1::Builder::new()
                .serve_connection(io, svc)
                .with_upgrades()
                .await
            {
                log::debug!("connection error: {}", e);
            }
        });
    }
}
