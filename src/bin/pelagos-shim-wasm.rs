//! containerd-shim-pelagos-wasm-v1 — Wasm shim for containerd.
//!
//! Implements the containerd shim v2 protocol (ttrpc) so that containerd can
//! use pelagos as a first-class Wasm container runtime.
//!
//! # Installation
//!
//! Install (or symlink) this binary as `containerd-shim-pelagos-wasm-v1` in
//! PATH and configure containerd with:
//!
//! ```toml
//! [plugins."io.containerd.grpc.v1.cri".containerd.runtimes.wasm]
//!   runtime_type = "io.containerd.pelagos.wasm.v1"
//! ```
//!
//! # Lifecycle
//!
//! | shim call        | action                                              |
//! |------------------|-----------------------------------------------------|
//! | `create`         | Parse OCI bundle `config.json`, record state        |
//! | `start`          | `spawn_wasm()` the module, track PID                |
//! | `state`          | Inspect child liveness, return OCI state JSON       |
//! | `kill`           | Forward signal to Wasm runtime subprocess           |
//! | `wait`           | Block until subprocess exits, return exit status    |
//! | `delete`         | Remove state dir, return last known exit status     |
//! | `shutdown`       | Exit the shim process                               |

use std::sync::{Arc, Mutex};

use containerd_shim as shim;
use log::{debug, info, warn};
use shim::{
    api, synchronous::publisher::RemotePublisher, Config, DeleteResponse, ExitSignal, Flags,
    TtrpcContext, TtrpcResult,
};

// ─── State ──────────────────────────────────────────────────────────────────

/// Per-container state tracked by the shim.
#[derive(Default)]
struct WasmState {
    /// OCI bundle path (contains `config.json` and rootfs).
    bundle: std::path::PathBuf,
    /// Spawned Wasm runtime subprocess, if started.
    child: Option<std::process::Child>,
    /// Exit code after the child has terminated.
    exit_code: Option<i32>,
}

// ─── Service ────────────────────────────────────────────────────────────────

#[derive(Clone)]
struct WasmShim {
    exit: Arc<ExitSignal>,
    state: Arc<Mutex<WasmState>>,
}

impl shim::Shim for WasmShim {
    type T = WasmShim;

    fn new(_runtime_id: &str, _args: &Flags, _config: &mut Config) -> Self {
        WasmShim {
            exit: Arc::new(ExitSignal::default()),
            state: Arc::new(Mutex::new(WasmState::default())),
        }
    }

    fn start_shim(&mut self, opts: shim::StartOpts) -> Result<String, shim::Error> {
        // Spawn a new shim process and return its ttrpc socket address.
        let grouping = opts.id.clone();
        let (_child_id, address) = shim::spawn(opts, &grouping, Vec::new())?;
        Ok(address)
    }

    fn delete_shim(&mut self) -> Result<DeleteResponse, shim::Error> {
        Ok(DeleteResponse::new())
    }

    fn wait(&mut self) {
        self.exit.wait();
    }

    fn create_task_service(&self, _publisher: RemotePublisher) -> Self::T {
        self.clone()
    }
}

// ─── Task (shim v2 RPC handlers) ────────────────────────────────────────────

impl shim::Task for WasmShim {
    /// Create — record the bundle path. The Wasm module is not spawned yet.
    fn create(
        &self,
        _ctx: &TtrpcContext,
        req: api::CreateTaskRequest,
    ) -> TtrpcResult<api::CreateTaskResponse> {
        info!("create: id={} bundle={}", req.id, req.bundle);
        let bundle = std::path::PathBuf::from(&req.bundle);

        let mut s = self.state.lock().unwrap();
        s.bundle = bundle;
        s.child = None;
        s.exit_code = None;

        Ok(api::CreateTaskResponse {
            pid: 0,
            ..Default::default()
        })
    }

    /// Start — spawn the Wasm module via the system Wasm runtime.
    fn start(
        &self,
        _ctx: &TtrpcContext,
        req: api::StartRequest,
    ) -> TtrpcResult<api::StartResponse> {
        info!("start: id={}", req.id);
        let mut s = self.state.lock().unwrap();

        let config_path = s.bundle.join("config.json");
        let config_json = std::fs::read_to_string(&config_path)
            .map_err(|e| shim::Error::Other(format!("cannot read config.json: {}", e)))?;

        let (wasm_path, extra_args, wasi_env) = parse_oci_config(&config_json)?;

        let rootfs = s.bundle.join("rootfs");
        let wasi = pelagos::wasm::WasiConfig {
            runtime: pelagos::wasm::WasmRuntime::Auto,
            env: wasi_env,
            preopened_dirs: vec![(rootfs.clone(), rootfs)],
        };

        let child = pelagos::wasm::spawn_wasm(
            &wasm_path,
            &extra_args,
            &wasi,
            std::process::Stdio::inherit(),
            std::process::Stdio::inherit(),
            std::process::Stdio::inherit(),
        )
        .map_err(|e| shim::Error::Other(format!("spawn_wasm failed: {}", e)))?;

        let pid = child.id();
        s.child = Some(child);

        Ok(api::StartResponse {
            pid,
            ..Default::default()
        })
    }

    /// State — check child liveness and return current OCI state.
    fn state(
        &self,
        _ctx: &TtrpcContext,
        req: api::StateRequest,
    ) -> TtrpcResult<api::StateResponse> {
        debug!("state: id={}", req.id);
        let mut s = self.state.lock().unwrap();

        // Poll for exit without blocking.
        if let Some(ref mut child) = s.child {
            if let Ok(Some(status)) = child.try_wait() {
                s.exit_code = Some(status.code().unwrap_or(-1));
            }
        }

        let (status, pid, exit_code) = match &s.child {
            None => (api::Status::UNKNOWN, 0u32, 0i32),
            Some(child) => {
                let pid = child.id();
                if let Some(code) = s.exit_code {
                    (api::Status::STOPPED, pid, code)
                } else {
                    (api::Status::RUNNING, pid, 0)
                }
            }
        };

        Ok(api::StateResponse {
            id: req.id,
            bundle: s.bundle.to_string_lossy().into_owned(),
            pid,
            status: status.into(),
            exit_status: exit_code as u32,
            ..Default::default()
        })
    }

    /// Kill — forward a signal to the Wasm runtime subprocess.
    fn kill(&self, _ctx: &TtrpcContext, req: api::KillRequest) -> TtrpcResult<api::Empty> {
        info!("kill: id={} signal={}", req.id, req.signal);
        let s = self.state.lock().unwrap();
        if let Some(ref child) = s.child {
            let pid = nix::unistd::Pid::from_raw(child.id() as i32);
            let sig = nix::sys::signal::Signal::try_from(req.signal as i32)
                .unwrap_or(nix::sys::signal::Signal::SIGTERM);
            let _ = nix::sys::signal::kill(pid, sig);
        }
        Ok(api::Empty::default())
    }

    /// Wait — block until the child exits, then return its exit status.
    fn wait(&self, _ctx: &TtrpcContext, req: api::WaitRequest) -> TtrpcResult<api::WaitResponse> {
        info!("wait: id={}", req.id);
        let exit_code = {
            let mut s = self.state.lock().unwrap();
            if let Some(ref mut child) = s.child {
                match child.wait() {
                    Ok(status) => status.code().unwrap_or(-1),
                    Err(e) => {
                        warn!("wait failed: {}", e);
                        -1
                    }
                }
            } else {
                s.exit_code.unwrap_or(0)
            }
        };

        {
            let mut s = self.state.lock().unwrap();
            s.exit_code = Some(exit_code);
        }

        Ok(api::WaitResponse {
            exit_status: exit_code as u32,
            ..Default::default()
        })
    }

    /// Delete — clean up state; return last exit status.
    fn delete(
        &self,
        _ctx: &TtrpcContext,
        req: api::DeleteRequest,
    ) -> TtrpcResult<api::DeleteResponse> {
        info!("delete: id={}", req.id);
        let mut s = self.state.lock().unwrap();

        let exit_code = s.exit_code.unwrap_or(0);
        let pid = s.child.as_ref().map(|c| c.id()).unwrap_or(0);

        // Drop the child handle.
        s.child = None;

        Ok(api::DeleteResponse {
            exit_status: exit_code as u32,
            pid,
            ..Default::default()
        })
    }

    /// Connect — return shim version info.
    fn connect(
        &self,
        _ctx: &TtrpcContext,
        _req: api::ConnectRequest,
    ) -> TtrpcResult<api::ConnectResponse> {
        Ok(api::ConnectResponse {
            version: env!("CARGO_PKG_VERSION").to_string(),
            shim_pid: std::process::id(),
            task_pid: {
                let s = self.state.lock().unwrap();
                s.child.as_ref().map(|c| c.id()).unwrap_or(0)
            },
            ..Default::default()
        })
    }

    /// Shutdown — signal the shim process to exit cleanly.
    fn shutdown(&self, _ctx: &TtrpcContext, _req: api::ShutdownRequest) -> TtrpcResult<api::Empty> {
        info!("shutdown");
        self.exit.signal();
        Ok(api::Empty::default())
    }
}

// ─── OCI config.json parsing ─────────────────────────────────────────────────

/// Parsed fields from an OCI bundle `config.json`.
type OciParsed = (
    std::path::PathBuf,      // wasm binary path
    Vec<std::ffi::OsString>, // extra argv
    Vec<(String, String)>,   // WASI env
);

/// Parse an OCI `config.json` bundle config to extract the Wasm executable,
/// extra argv, and WASI environment variables.
fn parse_oci_config(json: &str) -> Result<OciParsed, shim::Error> {
    let v: serde_json::Value =
        serde_json::from_str(json).map_err(|e| shim::Error::Other(e.to_string()))?;

    let args = v
        .pointer("/process/args")
        .and_then(|a| a.as_array())
        .cloned()
        .unwrap_or_default();

    if args.is_empty() {
        return Err(shim::Error::Other(
            "config.json: process.args is empty".to_string(),
        ));
    }

    let wasm_path = std::path::PathBuf::from(
        args[0]
            .as_str()
            .ok_or_else(|| shim::Error::Other("process.args[0] is not a string".to_string()))?,
    );

    let extra_args: Vec<std::ffi::OsString> = args[1..]
        .iter()
        .filter_map(|v| v.as_str().map(std::ffi::OsString::from))
        .collect();

    let env: Vec<(String, String)> = v
        .pointer("/process/env")
        .and_then(|e| e.as_array())
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|v| {
            let s = v.as_str()?;
            let (k, val) = s.split_once('=')?;
            Some((k.to_string(), val.to_string()))
        })
        .collect();

    Ok((wasm_path, extra_args, env))
}

// ─── Entry point ─────────────────────────────────────────────────────────────

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    shim::run::<WasmShim>("io.containerd.pelagos.wasm.v1", None)
}
