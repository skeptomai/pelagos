//! Pelagos daemon/CLI configuration.
//!
//! Config file locations:
//! - Rootless: `$XDG_CONFIG_HOME/pelagos/config.toml`
//!   (default `~/.config/pelagos/config.toml`)
//! - Root: `/etc/pelagos/config.toml`
//!
//! A missing or unparseable file is silently ignored — built-in defaults
//! are always used as the fallback so the file is fully optional.
//!
//! # Example
//! ```toml
//! [network]
//! # Subnet assigned to the default pelagos0 bridge on first bootstrap.
//! # Has no effect once pelagos0 already exists.
//! default_subnet = "10.88.0.0/24"
//!
//! # Pool from which /24 blocks are carved when `pelagos network create`
//! # is called without an explicit --subnet.
//! auto_alloc_pool = "10.99.0.0/16"
//!
//! # DNS servers injected into bridge-networked containers when --dns is
//! # not passed.  Default is Cloudflare dual-stack (IPv4 + IPv6).
//! # Set to [] to disable auto-injection.
//! # Override at runtime: PELAGOS_DEFAULT_DNS=9.9.9.9,2620:fe::fe
//! default_dns = ["1.1.1.1", "2606:4700:4700::1111"]
//! ```

use serde::Deserialize;

use crate::network::Ipv4Net;

/// Default DNS servers injected into bridge-networked containers when no
/// `--dns` flag is passed.  Cloudflare dual-stack: works for both IPv4 and
/// IPv6 hosts.
const DEFAULT_DNS: &[&str] = &["1.1.1.1", "2606:4700:4700::1111"];

// ── Top-level config ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, Default)]
pub struct PelagosConfig {
    #[serde(default)]
    pub network: NetworkConfig,
}

impl PelagosConfig {
    /// Load config from the platform-appropriate path.
    ///
    /// Returns built-in defaults if the file does not exist or cannot be
    /// parsed — config is always optional.
    pub fn load() -> Self {
        Self::load_from(&crate::paths::config_file())
    }

    /// Load config from an explicit path. Useful for tests and overrides.
    pub fn load_from(path: &std::path::Path) -> Self {
        let data = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(_) => return Self::default(),
        };
        match toml::from_str::<Self>(&data) {
            Ok(cfg) => cfg,
            Err(e) => {
                log::warn!(
                    "config: failed to parse {}: {} — using defaults",
                    path.display(),
                    e
                );
                Self::default()
            }
        }
    }
}

// ── [network] section ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
pub struct NetworkConfig {
    /// Subnet for the `pelagos0` bridge on **first** bootstrap.
    ///
    /// Has no effect once the network has already been created and persisted
    /// to disk — use `pelagos network rm pelagos0` then restart to change it.
    #[serde(default = "NetworkConfig::default_subnet_str")]
    pub default_subnet: String,

    /// Pool from which /24 blocks are carved for named networks created
    /// without an explicit `--subnet`.  Must be a /16 or larger.
    #[serde(default = "NetworkConfig::default_alloc_pool_str")]
    pub auto_alloc_pool: String,

    /// DNS servers injected into bridge-networked containers when `--dns` is
    /// not passed explicitly.  Overridable via `PELAGOS_DEFAULT_DNS`
    /// (comma-separated, env takes precedence over config file).
    ///
    /// Set to an empty list to disable auto-injection entirely:
    /// `default_dns = []`
    #[serde(default = "NetworkConfig::default_dns_list")]
    pub default_dns: Vec<String>,
}

impl Default for NetworkConfig {
    fn default() -> Self {
        Self {
            default_subnet: Self::default_subnet_str(),
            auto_alloc_pool: Self::default_alloc_pool_str(),
            default_dns: Self::default_dns_list(),
        }
    }
}

impl NetworkConfig {
    fn default_subnet_str() -> String {
        "172.19.0.0/24".to_string()
    }

    fn default_alloc_pool_str() -> String {
        "10.99.0.0/16".to_string()
    }

    fn default_dns_list() -> Vec<String> {
        DEFAULT_DNS.iter().map(|s| s.to_string()).collect()
    }

    /// Resolve the effective default DNS list, applying the `PELAGOS_DEFAULT_DNS`
    /// environment variable override if set.  Returns an empty vec if the env
    /// var is set to an empty string (opt-out).
    pub fn effective_default_dns(&self) -> Vec<String> {
        if let Ok(val) = std::env::var("PELAGOS_DEFAULT_DNS") {
            if val.is_empty() {
                return vec![];
            }
            return val
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
        }
        self.default_dns.clone()
    }

    /// Parse `default_subnet` as an [`Ipv4Net`], falling back to the
    /// built-in default on error.
    pub fn default_subnet_parsed(&self) -> Ipv4Net {
        Ipv4Net::from_cidr(&self.default_subnet).unwrap_or_else(|e| {
            log::warn!(
                "config: invalid default_subnet '{}': {} — using 172.19.0.0/24",
                self.default_subnet,
                e
            );
            Ipv4Net::from_cidr("172.19.0.0/24").unwrap()
        })
    }

    /// Parse `auto_alloc_pool` as an [`Ipv4Net`], falling back to the
    /// built-in default on error.
    pub fn auto_alloc_pool_parsed(&self) -> Ipv4Net {
        Ipv4Net::from_cidr(&self.auto_alloc_pool).unwrap_or_else(|e| {
            log::warn!(
                "config: invalid auto_alloc_pool '{}': {} — using 10.99.0.0/16",
                self.auto_alloc_pool,
                e
            );
            Ipv4Net::from_cidr("10.99.0.0/16").unwrap()
        })
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config_values() {
        let cfg = PelagosConfig::default();
        assert_eq!(cfg.network.default_subnet, "172.19.0.0/24");
        assert_eq!(cfg.network.auto_alloc_pool, "10.99.0.0/16");
        assert_eq!(
            cfg.network.default_dns,
            vec!["1.1.1.1", "2606:4700:4700::1111"]
        );
    }

    #[test]
    fn test_parse_full_config() {
        let toml = r#"
[network]
default_subnet = "10.88.0.0/24"
auto_alloc_pool = "10.200.0.0/16"
"#;
        let cfg: PelagosConfig = toml::from_str(toml).unwrap();
        assert_eq!(cfg.network.default_subnet, "10.88.0.0/24");
        assert_eq!(cfg.network.auto_alloc_pool, "10.200.0.0/16");
    }

    #[test]
    fn test_parse_partial_config_uses_defaults() {
        let toml = "[network]\nauto_alloc_pool = \"10.200.0.0/16\"\n";
        let cfg: PelagosConfig = toml::from_str(toml).unwrap();
        assert_eq!(cfg.network.default_subnet, "172.19.0.0/24");
        assert_eq!(cfg.network.auto_alloc_pool, "10.200.0.0/16");
    }

    #[test]
    fn test_parse_empty_config_uses_defaults() {
        let cfg: PelagosConfig = toml::from_str("").unwrap();
        assert_eq!(cfg.network.default_subnet, "172.19.0.0/24");
        assert_eq!(cfg.network.auto_alloc_pool, "10.99.0.0/16");
    }

    #[test]
    fn test_default_subnet_parsed() {
        let cfg = NetworkConfig::default();
        let net = cfg.default_subnet_parsed();
        assert_eq!(net.addr.to_string(), "172.19.0.0");
        assert_eq!(net.prefix_len, 24);
    }

    #[test]
    fn test_auto_alloc_pool_parsed() {
        let cfg = NetworkConfig::default();
        let pool = cfg.auto_alloc_pool_parsed();
        assert_eq!(pool.addr.to_string(), "10.99.0.0");
        assert_eq!(pool.prefix_len, 16);
    }

    #[test]
    fn test_invalid_subnet_falls_back_to_default() {
        let cfg = NetworkConfig {
            default_subnet: "not-a-cidr".to_string(),
            auto_alloc_pool: "also-bad".to_string(),
            default_dns: vec![],
        };
        let net = cfg.default_subnet_parsed();
        assert_eq!(net.addr.to_string(), "172.19.0.0");
        let pool = cfg.auto_alloc_pool_parsed();
        assert_eq!(pool.addr.to_string(), "10.99.0.0");
    }

    #[test]
    fn test_default_dns_in_config_file() {
        let toml = r#"
[network]
default_dns = ["9.9.9.9", "2620:fe::fe"]
"#;
        let cfg: PelagosConfig = toml::from_str(toml).unwrap();
        assert_eq!(cfg.network.default_dns, vec!["9.9.9.9", "2620:fe::fe"]);
    }

    #[test]
    fn test_empty_default_dns_disables_injection() {
        let toml = "[network]\ndefault_dns = []\n";
        let cfg: PelagosConfig = toml::from_str(toml).unwrap();
        assert!(cfg.network.effective_default_dns().is_empty());
    }

    #[test]
    fn test_effective_default_dns_env_override() {
        // Temporarily set the env var; restore on exit.
        std::env::set_var("PELAGOS_DEFAULT_DNS", "8.8.8.8,8.8.4.4");
        let cfg = NetworkConfig::default();
        let dns = cfg.effective_default_dns();
        std::env::remove_var("PELAGOS_DEFAULT_DNS");
        assert_eq!(dns, vec!["8.8.8.8", "8.8.4.4"]);
    }

    #[test]
    fn test_effective_default_dns_env_empty_opt_out() {
        std::env::set_var("PELAGOS_DEFAULT_DNS", "");
        let cfg = NetworkConfig::default();
        let dns = cfg.effective_default_dns();
        std::env::remove_var("PELAGOS_DEFAULT_DNS");
        assert!(dns.is_empty());
    }

    #[test]
    fn test_load_missing_file_returns_defaults() {
        // Pass a path that does not exist — load_from must return defaults.
        let tmp = tempfile::tempdir().unwrap();
        let absent = tmp.path().join("pelagos/config.toml");
        let cfg = PelagosConfig::load_from(&absent);
        assert_eq!(cfg.network.default_subnet, "172.19.0.0/24");
        assert_eq!(cfg.network.auto_alloc_pool, "10.99.0.0/16");
    }

    #[test]
    fn test_load_from_xdg_config_home() {
        // Write a config file and load it directly via load_from — no env mutation.
        let tmp = tempfile::tempdir().unwrap();
        let cfg_dir = tmp.path().join("pelagos");
        std::fs::create_dir_all(&cfg_dir).unwrap();
        let cfg_path = cfg_dir.join("config.toml");
        std::fs::write(
            &cfg_path,
            "[network]\ndefault_subnet = \"10.77.0.0/24\"\nauto_alloc_pool = \"10.77.0.0/16\"\n",
        )
        .unwrap();
        let cfg = PelagosConfig::load_from(&cfg_path);
        assert_eq!(cfg.network.default_subnet, "10.77.0.0/24");
        assert_eq!(cfg.network.auto_alloc_pool, "10.77.0.0/16");
    }
}
