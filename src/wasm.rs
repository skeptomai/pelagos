//! Wasm/WASI runtime integration.
//!
//! Detects WebAssembly binaries by magic bytes (`\0asm`) and dispatches
//! execution to an installed runtime (wasmtime or WasmEdge) via subprocess.
//!
//! # Example
//!
//! ```no_run
//! use pelagos::wasm::{WasmRuntime, WasiConfig, spawn_wasm};
//! use std::path::Path;
//! use std::process;
//!
//! let wasi = WasiConfig {
//!     runtime: WasmRuntime::Auto,
//!     env: vec![("KEY".into(), "val".into())],
//!     preopened_dirs: vec![("/data".into(), "/data".into())],
//! };
//! let child = spawn_wasm(
//!     Path::new("/app/module.wasm"),
//!     &[],
//!     &wasi,
//!     process::Stdio::inherit(),
//!     process::Stdio::inherit(),
//!     process::Stdio::inherit(),
//! ).expect("spawn wasm");
//! ```

use std::io;
use std::path::{Path, PathBuf};

/// WebAssembly module magic bytes: `\0asm` (0x00 0x61 0x73 0x6D).
const WASM_MAGIC: [u8; 4] = [0x00, 0x61, 0x73, 0x6D];

/// OCI layer media types that carry a raw WebAssembly module blob (not a tarball).
pub const WASM_LAYER_MEDIA_TYPES: &[&str] = &[
    "application/vnd.bytecodealliance.wasm.component.layer.v0+wasm",
    "application/vnd.wasm.content.layer.v1+wasm",
    "application/wasm",
];

/// Returns `true` if `media_type` is a recognised Wasm OCI layer type.
pub fn is_wasm_media_type(media_type: &str) -> bool {
    WASM_LAYER_MEDIA_TYPES.contains(&media_type)
}

/// Preferred Wasm runtime backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WasmRuntime {
    /// Use wasmtime (Bytecode Alliance reference implementation).
    Wasmtime,
    /// Use WasmEdge (CNCF project, strong WASI preview 2 support).
    WasmEdge,
    /// Auto-detect: try wasmtime first, then WasmEdge.
    #[default]
    Auto,
}

/// WASI configuration for a Wasm container.
#[derive(Debug, Clone, Default)]
pub struct WasiConfig {
    /// Preferred runtime backend.
    pub runtime: WasmRuntime,
    /// WASI environment variables (supplement to the process environment).
    pub env: Vec<(String, String)>,
    /// Host→guest directory mappings to preopen for WASI filesystem access.
    ///
    /// Each entry is `(host_path, guest_path)`.  For identity mappings
    /// (host and guest are the same path) set both to the same value.
    pub preopened_dirs: Vec<(PathBuf, PathBuf)>,
}

/// Returns `true` if the file at `path` begins with WebAssembly magic bytes.
///
/// Returns `false` (not an error) when the file is missing, too short, or
/// cannot be read.
pub fn is_wasm_binary(path: &Path) -> io::Result<bool> {
    use std::io::Read;
    let mut f = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e),
    };
    let mut magic = [0u8; 4];
    match f.read_exact(&mut magic) {
        Ok(()) => Ok(magic == WASM_MAGIC),
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => Ok(false),
        Err(e) => Err(e),
    }
}

/// Find an installed Wasm runtime binary in PATH.
///
/// Returns `(WasmRuntime, PathBuf)` for the first runtime found, or `None`
/// if neither wasmtime nor wasmedge is installed.
///
/// Preference order: `Auto`/`Wasmtime` → wasmtime first; `WasmEdge` → WasmEdge first.
pub fn find_wasm_runtime(preferred: WasmRuntime) -> Option<(WasmRuntime, PathBuf)> {
    let candidates: &[(&str, WasmRuntime)] = match preferred {
        WasmRuntime::WasmEdge => &[
            ("wasmedge", WasmRuntime::WasmEdge),
            ("wasmtime", WasmRuntime::Wasmtime),
        ],
        _ => &[
            ("wasmtime", WasmRuntime::Wasmtime),
            ("wasmedge", WasmRuntime::WasmEdge),
        ],
    };
    for (name, rt) in candidates {
        if let Some(path) = find_in_path(name) {
            return Some((*rt, path));
        }
    }
    None
}

fn find_in_path(name: &str) -> Option<PathBuf> {
    let path_env = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_env) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Spawn a WebAssembly module through an installed Wasm runtime subprocess.
///
/// `program` — path to the `.wasm` file on the host filesystem.
/// `extra_args` — forwarded verbatim to the Wasm module as WASI argv[1..].
///
/// # Errors
///
/// Returns `Err` if no runtime is found in PATH or if the subprocess fails to
/// start.
pub fn spawn_wasm(
    program: &Path,
    extra_args: &[std::ffi::OsString],
    wasi: &WasiConfig,
    stdin: std::process::Stdio,
    stdout: std::process::Stdio,
    stderr: std::process::Stdio,
) -> io::Result<std::process::Child> {
    let (rt, runtime_bin) = find_wasm_runtime(wasi.runtime).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "no Wasm runtime found in PATH — install wasmtime or wasmedge",
        )
    })?;

    log::info!(
        "spawning Wasm module '{}' via {:?} ({})",
        program.display(),
        rt,
        runtime_bin.display()
    );

    let mut cmd = match rt {
        WasmRuntime::Wasmtime => build_wasmtime_cmd(&runtime_bin, program, extra_args, wasi),
        WasmRuntime::WasmEdge => build_wasmedge_cmd(&runtime_bin, program, extra_args, wasi),
        WasmRuntime::Auto => unreachable!("Auto resolved to a concrete runtime above"),
    };

    cmd.stdin(stdin).stdout(stdout).stderr(stderr).spawn()
}

fn build_wasmtime_cmd(
    runtime: &Path,
    wasm: &Path,
    extra_args: &[std::ffi::OsString],
    wasi: &WasiConfig,
) -> std::process::Command {
    let mut cmd = std::process::Command::new(runtime);
    cmd.arg("run");
    // wasmtime >= 14: --dir host::guest
    for (host, guest) in &wasi.preopened_dirs {
        cmd.arg("--dir")
            .arg(format!("{}::{}", host.display(), guest.display()));
    }
    for (k, v) in &wasi.env {
        cmd.arg("--env").arg(format!("{k}={v}"));
    }
    cmd.arg("--").arg(wasm);
    cmd.args(extra_args);
    cmd
}

fn build_wasmedge_cmd(
    runtime: &Path,
    wasm: &Path,
    extra_args: &[std::ffi::OsString],
    wasi: &WasiConfig,
) -> std::process::Command {
    let mut cmd = std::process::Command::new(runtime);
    // wasmedge: --dir host:guest (single colon)
    for (host, guest) in &wasi.preopened_dirs {
        cmd.arg("--dir")
            .arg(format!("{}:{}", host.display(), guest.display()));
    }
    for (k, v) in &wasi.env {
        cmd.arg("--env").arg(format!("{k}={v}"));
    }
    cmd.arg(wasm);
    cmd.args(extra_args);
    cmd
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn test_is_wasm_binary_magic_bytes() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        // Wasm module header: magic + version 1
        tmp.write_all(&[0x00, 0x61, 0x73, 0x6D, 0x01, 0x00, 0x00, 0x00])
            .unwrap();
        tmp.flush().unwrap();
        assert!(is_wasm_binary(tmp.path()).unwrap());
    }

    #[test]
    fn test_is_wasm_binary_elf_is_false() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        tmp.write_all(b"\x7fELF\x02\x01\x01\x00").unwrap();
        tmp.flush().unwrap();
        assert!(!is_wasm_binary(tmp.path()).unwrap());
    }

    #[test]
    fn test_is_wasm_binary_too_short() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        tmp.write_all(b"\x00\x61").unwrap();
        tmp.flush().unwrap();
        assert!(!is_wasm_binary(tmp.path()).unwrap());
    }

    #[test]
    fn test_is_wasm_binary_empty_file() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        assert!(!is_wasm_binary(tmp.path()).unwrap());
    }

    #[test]
    fn test_is_wasm_binary_missing_path() {
        // Missing file → Ok(false), not an error.
        let result = is_wasm_binary(Path::new("/tmp/__pelagos_nonexistent_abc123.wasm"));
        assert!(result.is_ok());
        assert!(!result.unwrap());
    }

    #[test]
    fn test_is_wasm_media_type_known_types() {
        assert!(is_wasm_media_type(
            "application/vnd.bytecodealliance.wasm.component.layer.v0+wasm"
        ));
        assert!(is_wasm_media_type(
            "application/vnd.wasm.content.layer.v1+wasm"
        ));
        assert!(is_wasm_media_type("application/wasm"));
    }

    #[test]
    fn test_is_wasm_media_type_standard_layer_is_false() {
        assert!(!is_wasm_media_type(
            "application/vnd.oci.image.layer.v1.tar+gzip"
        ));
        assert!(!is_wasm_media_type(
            "application/vnd.docker.image.rootfs.diff.tar.gzip"
        ));
        assert!(!is_wasm_media_type(""));
    }

    #[test]
    fn test_find_wasm_runtime_does_not_panic() {
        // Verify no panic regardless of whether runtimes are installed.
        let _ = find_wasm_runtime(WasmRuntime::Auto);
        let _ = find_wasm_runtime(WasmRuntime::Wasmtime);
        let _ = find_wasm_runtime(WasmRuntime::WasmEdge);
    }
}
