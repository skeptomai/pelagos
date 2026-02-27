//! Registry authentication provider.
//!
//! Resolution order:
//! 1. CLI flags (`username` + `password`)
//! 2. Environment variables (`REMORA_REGISTRY_USER` + `REMORA_REGISTRY_PASS`)
//! 3. `~/.docker/config.json` `auths[registry].auth` (base64-decoded `user:pass`)
//! 4. `RegistryAuth::Anonymous`
//!
//! Credential helpers (`credHelpers`, `credsStore`) are not supported.
//! ECR users: `--password $(aws ecr get-login-password)`.

use oci_client::secrets::RegistryAuth;

/// Resolve the best available auth for `registry`.
///
/// `registry` is the bare hostname, e.g. `"ghcr.io"`, `"docker.io"`.
pub fn resolve_auth(
    registry: &str,
    username: Option<&str>,
    password: Option<&str>,
) -> RegistryAuth {
    // 1. CLI flags take priority.
    if let (Some(u), Some(p)) = (username, password) {
        return RegistryAuth::Basic(u.to_string(), p.to_string());
    }

    // 2. Environment variables.
    let env_user = std::env::var("REMORA_REGISTRY_USER").ok();
    let env_pass = std::env::var("REMORA_REGISTRY_PASS").ok();
    if let (Some(u), Some(p)) = (env_user.as_deref(), env_pass.as_deref()) {
        if !u.is_empty() && !p.is_empty() {
            return RegistryAuth::Basic(u.to_string(), p.to_string());
        }
    }

    // 3. ~/.docker/config.json
    if let Some((u, p)) = parse_docker_config(registry) {
        return RegistryAuth::Basic(u, p);
    }

    RegistryAuth::Anonymous
}

/// Parse `~/.docker/config.json` and return `(username, password)` for `registry`.
pub fn parse_docker_config(registry: &str) -> Option<(String, String)> {
    let config_path = docker_config_path()?;
    let data = std::fs::read_to_string(config_path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&data).ok()?;
    let auths = value.get("auths")?.as_object()?;

    for key in registry_keys(registry) {
        if let Some(entry) = auths.get(&key) {
            if let Some(auth_b64) = entry.get("auth").and_then(|v| v.as_str()) {
                if let Some((u, p)) = decode_auth(auth_b64) {
                    return Some((u, p));
                }
            }
        }
    }
    None
}

/// Write (or update) a `~/.docker/config.json` `auths` entry for `registry`.
pub fn write_docker_config(registry: &str, username: &str, password: &str) -> std::io::Result<()> {
    let config_path = docker_config_path().ok_or_else(|| {
        std::io::Error::other("cannot determine HOME directory for docker config")
    })?;

    let mut value: serde_json::Value = if config_path.exists() {
        let data = std::fs::read_to_string(&config_path)?;
        serde_json::from_str(&data).unwrap_or(serde_json::json!({}))
    } else {
        serde_json::json!({})
    };

    if !value.get("auths").map(|v| v.is_object()).unwrap_or(false) {
        value["auths"] = serde_json::json!({});
    }

    let auth_b64 = base64_encode(format!("{}:{}", username, password).as_bytes());
    value["auths"][registry] = serde_json::json!({ "auth": auth_b64 });

    if let Some(parent) = config_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json =
        serde_json::to_string_pretty(&value).map_err(|e| std::io::Error::other(e.to_string()))?;
    std::fs::write(&config_path, json)
}

/// Remove `registry` from `~/.docker/config.json` `auths`.
pub fn remove_docker_config(registry: &str) -> std::io::Result<()> {
    let config_path = docker_config_path().ok_or_else(|| {
        std::io::Error::other("cannot determine HOME directory for docker config")
    })?;

    if !config_path.exists() {
        return Err(std::io::Error::other(format!(
            "not logged in to {}",
            registry
        )));
    }

    let data = std::fs::read_to_string(&config_path)?;
    let mut value: serde_json::Value =
        serde_json::from_str(&data).map_err(|e| std::io::Error::other(e.to_string()))?;

    let removed = if let Some(auths) = value.get_mut("auths").and_then(|v| v.as_object_mut()) {
        let mut removed = false;
        for key in registry_keys(registry) {
            if auths.remove(&key).is_some() {
                removed = true;
            }
        }
        removed
    } else {
        false
    };

    if !removed {
        return Err(std::io::Error::other(format!(
            "not logged in to {}",
            registry
        )));
    }

    let json =
        serde_json::to_string_pretty(&value).map_err(|e| std::io::Error::other(e.to_string()))?;
    std::fs::write(&config_path, json)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn docker_config_path() -> Option<std::path::PathBuf> {
    let home = std::env::var("HOME").ok()?;
    Some(
        std::path::PathBuf::from(home)
            .join(".docker")
            .join("config.json"),
    )
}

/// Return candidate registry keys for `~/.docker/config.json` lookup.
///
/// Docker uses different canonical forms depending on history:
/// - `docker.io` → also try `"https://index.docker.io/v1/"`
/// - All others → exact hostname plus `"https://<host>/"` variant
fn registry_keys(registry: &str) -> Vec<String> {
    let bare = registry
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_end_matches('/');
    // docker.io and index.docker.io are the same registry.  oci-client's
    // resolve_registry() maps "docker.io" → "index.docker.io", but users
    // naturally run `remora image login docker.io`, so we need to search
    // both forms regardless of which one was presented.
    match bare {
        "docker.io" | "index.docker.io" => vec![
            "docker.io".to_string(),
            "index.docker.io".to_string(),
            "https://index.docker.io/v1/".to_string(),
        ],
        _ => vec![bare.to_string(), format!("https://{}/", bare)],
    }
}

/// Decode a base64-encoded `"user:password"` string.
fn decode_auth(b64: &str) -> Option<(String, String)> {
    let decoded = base64_decode(b64.trim())?;
    let s = String::from_utf8(decoded).ok()?;
    let (u, p) = s.split_once(':')?;
    Some((u.to_string(), p.to_string()))
}

pub(crate) fn base64_encode(data: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[((n >> 18) & 0x3f) as usize] as char);
        out.push(ALPHABET[((n >> 12) & 0x3f) as usize] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[((n >> 6) & 0x3f) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[(n & 0x3f) as usize] as char
        } else {
            '='
        });
    }
    out
}

fn base64_decode(b64: &str) -> Option<Vec<u8>> {
    fn char_to_val(c: u8) -> Option<u8> {
        match c {
            b'A'..=b'Z' => Some(c - b'A'),
            b'a'..=b'z' => Some(c - b'a' + 26),
            b'0'..=b'9' => Some(c - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            b'=' => Some(0),
            _ => None,
        }
    }
    let clean: Vec<u8> = b64.bytes().filter(|&b| !b" \t\r\n".contains(&b)).collect();
    if !clean.len().is_multiple_of(4) {
        return None;
    }
    let mut out = Vec::with_capacity(clean.len() / 4 * 3);
    for chunk in clean.chunks(4) {
        let v0 = char_to_val(chunk[0])?;
        let v1 = char_to_val(chunk[1])?;
        let v2 = char_to_val(chunk[2])?;
        let v3 = char_to_val(chunk[3])?;
        let n = ((v0 as u32) << 18) | ((v1 as u32) << 12) | ((v2 as u32) << 6) | (v3 as u32);
        out.push((n >> 16) as u8);
        if chunk[2] != b'=' {
            out.push((n >> 8) as u8);
        }
        if chunk[3] != b'=' {
            out.push(n as u8);
        }
    }
    Some(out)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_base64_roundtrip() {
        let data = b"user:password123";
        let encoded = base64_encode(data);
        let decoded = base64_decode(&encoded).expect("decode");
        assert_eq!(decoded, data);
    }

    #[test]
    fn test_decode_auth_basic() {
        let b64 = base64_encode(b"user:pass");
        let (u, p) = decode_auth(&b64).expect("decode_auth");
        assert_eq!(u, "user");
        assert_eq!(p, "pass");
    }

    #[test]
    fn test_decode_auth_password_with_colon() {
        // Password contains ':', should split on first colon only.
        let b64 = base64_encode(b"user:pa:ss");
        let (u, p) = decode_auth(&b64).expect("decode_auth");
        assert_eq!(u, "user");
        assert_eq!(p, "pa:ss");
    }

    #[test]
    fn test_parse_docker_config_synthetic() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let docker_dir = tmp.path().join(".docker");
        std::fs::create_dir_all(&docker_dir).unwrap();
        let auth_b64 = base64_encode(b"myuser:mypass");
        let config = serde_json::json!({
            "auths": { "ghcr.io": { "auth": auth_b64 } }
        });
        std::fs::write(
            docker_dir.join("config.json"),
            serde_json::to_string(&config).unwrap(),
        )
        .unwrap();

        // Temporarily override HOME.
        let old_home = std::env::var("HOME").ok();
        std::env::set_var("HOME", tmp.path());
        let result = parse_docker_config("ghcr.io");
        if let Some(h) = old_home {
            std::env::set_var("HOME", h);
        } else {
            std::env::remove_var("HOME");
        }

        let (u, p) = result.expect("should find creds");
        assert_eq!(u, "myuser");
        assert_eq!(p, "mypass");
    }

    #[test]
    fn test_resolve_auth_env() {
        std::env::set_var("REMORA_REGISTRY_USER", "envuser");
        std::env::set_var("REMORA_REGISTRY_PASS", "envpass");
        let auth = resolve_auth("example.com", None, None);
        std::env::remove_var("REMORA_REGISTRY_USER");
        std::env::remove_var("REMORA_REGISTRY_PASS");
        match auth {
            RegistryAuth::Basic(u, p) => {
                assert_eq!(u, "envuser");
                assert_eq!(p, "envpass");
            }
            other => panic!("expected Basic, got {:?}", other),
        }
    }

    #[test]
    fn test_resolve_auth_cli_priority() {
        std::env::set_var("REMORA_REGISTRY_USER", "envuser");
        std::env::set_var("REMORA_REGISTRY_PASS", "envpass");
        let auth = resolve_auth("example.com", Some("cliuser"), Some("clipass"));
        std::env::remove_var("REMORA_REGISTRY_USER");
        std::env::remove_var("REMORA_REGISTRY_PASS");
        match auth {
            RegistryAuth::Basic(u, p) => {
                assert_eq!(u, "cliuser");
                assert_eq!(p, "clipass");
            }
            other => panic!("expected Basic, got {:?}", other),
        }
    }

    #[test]
    fn test_resolve_auth_anonymous() {
        std::env::remove_var("REMORA_REGISTRY_USER");
        std::env::remove_var("REMORA_REGISTRY_PASS");
        let tmp = tempfile::tempdir().unwrap();
        let old_home = std::env::var("HOME").ok();
        std::env::set_var("HOME", tmp.path());
        let auth = resolve_auth("nobody.example", None, None);
        if let Some(h) = old_home {
            std::env::set_var("HOME", h);
        } else {
            std::env::remove_var("HOME");
        }
        assert!(
            matches!(auth, RegistryAuth::Anonymous),
            "expected Anonymous"
        );
    }

    #[test]
    fn test_registry_keys_docker_io() {
        let keys = registry_keys("docker.io");
        assert!(keys.contains(&"docker.io".to_string()));
        assert!(keys.contains(&"https://index.docker.io/v1/".to_string()));
    }

    #[test]
    fn test_registry_keys_other() {
        let keys = registry_keys("ghcr.io");
        assert!(keys.contains(&"ghcr.io".to_string()));
        // Exact hostname variant
        assert_eq!(keys[0], "ghcr.io");
    }
}
