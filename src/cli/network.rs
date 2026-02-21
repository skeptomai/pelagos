//! `remora network` — manage named networks.

use remora::network::{Ipv4Net, NetworkDef};

/// Validate a network name: alphanumeric + hyphen, no leading hyphen, ≤ 12 chars.
fn validate_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("network name cannot be empty".into());
    }
    if name.len() > 12 {
        return Err(format!(
            "network name '{}' too long ({} chars, max 12)",
            name,
            name.len()
        ));
    }
    if name.starts_with('-') {
        return Err(format!(
            "network name '{}' cannot start with a hyphen",
            name
        ));
    }
    if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return Err(format!(
            "network name '{}' contains invalid characters (only alphanumeric + hyphen allowed)",
            name
        ));
    }
    Ok(())
}

/// Compute the bridge interface name for a network.
fn bridge_name_for(name: &str) -> String {
    if name == "remora0" {
        "remora0".to_string()
    } else {
        format!("rm-{}", name)
    }
}

pub fn cmd_network_create(name: &str, subnet_cidr: &str) -> Result<(), Box<dyn std::error::Error>> {
    validate_name(name)?;

    let subnet = Ipv4Net::from_cidr(subnet_cidr)?;
    let bridge_name = bridge_name_for(name);

    // Bridge name must fit in IFNAMSIZ (15 chars).
    if bridge_name.len() > 15 {
        return Err(format!("bridge name '{}' exceeds 15 char kernel limit", bridge_name).into());
    }

    // Check for existing network with same name.
    let config_dir = remora::paths::network_config_dir(name);
    if config_dir.join("config.json").exists() {
        return Err(format!("network '{}' already exists", name).into());
    }

    // Check subnet overlap against all existing networks.
    let networks_dir = remora::paths::networks_config_dir();
    if networks_dir.exists() {
        if let Ok(entries) = std::fs::read_dir(&networks_dir) {
            for entry in entries.flatten() {
                let cfg_path = entry.path().join("config.json");
                if let Ok(data) = std::fs::read_to_string(&cfg_path) {
                    if let Ok(existing) = serde_json::from_str::<NetworkDef>(&data) {
                        if existing.subnet.overlaps(&subnet) {
                            return Err(format!(
                                "subnet {} overlaps with network '{}' ({})",
                                subnet_cidr,
                                existing.name,
                                existing.subnet.cidr_string()
                            )
                            .into());
                        }
                    }
                }
            }
        }
    }

    let gateway = subnet.gateway();
    let net = NetworkDef {
        name: name.to_string(),
        subnet,
        gateway,
        bridge_name,
    };
    net.save()?;

    println!("Created network '{}' ({})", name, subnet_cidr);
    Ok(())
}

pub fn cmd_network_ls(json: bool) -> Result<(), Box<dyn std::error::Error>> {
    // Bootstrap default network so it always appears.
    let _ = remora::network::bootstrap_default_network();

    let networks_dir = remora::paths::networks_config_dir();
    let entries = match std::fs::read_dir(&networks_dir) {
        Ok(e) => e,
        Err(_) => {
            if json {
                println!("[]");
            } else {
                println!("No networks found.");
            }
            return Ok(());
        }
    };

    let mut nets: Vec<NetworkDef> = Vec::new();
    for entry in entries.flatten() {
        let cfg_path = entry.path().join("config.json");
        if let Ok(data) = std::fs::read_to_string(&cfg_path) {
            if let Ok(net) = serde_json::from_str::<NetworkDef>(&data) {
                nets.push(net);
            }
        }
    }
    nets.sort_by(|a, b| a.name.cmp(&b.name));

    if json {
        println!("{}", serde_json::to_string_pretty(&nets)?);
        return Ok(());
    }

    if nets.is_empty() {
        println!("No networks. Use: remora network create <name> --subnet CIDR");
        return Ok(());
    }

    println!(
        "{:<15} {:<20} {:<15} {:<10}",
        "NAME", "SUBNET", "GATEWAY", "BRIDGE"
    );
    for net in &nets {
        println!(
            "{:<15} {:<20} {:<15} {:<10}",
            net.name,
            net.subnet.cidr_string(),
            net.gateway,
            net.bridge_name,
        );
    }
    Ok(())
}

pub fn cmd_network_rm(name: &str) -> Result<(), Box<dyn std::error::Error>> {
    if name == "remora0" {
        return Err("cannot remove the default network 'remora0'".into());
    }

    let config_dir = remora::paths::network_config_dir(name);
    if !config_dir.join("config.json").exists() {
        return Err(format!("network '{}' not found", name).into());
    }

    let net = NetworkDef::load(name)?;

    // Delete the bridge interface if it exists (non-fatal).
    let _ = std::process::Command::new("ip")
        .args(["link", "del", &net.bridge_name])
        .stderr(std::process::Stdio::null())
        .status();

    // Delete the nft table if it exists (non-fatal).
    let table = net.nft_table_name();
    let script = format!("delete table ip {}\n", table);
    let _ = std::process::Command::new("nft")
        .args(["-f", "-"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            child.stdin.as_mut().unwrap().write_all(script.as_bytes())?;
            child.wait()
        });

    // Remove config dir.
    std::fs::remove_dir_all(&config_dir)?;

    // Remove runtime dir if it exists.
    let runtime_dir = remora::paths::network_runtime_dir(name);
    let _ = std::fs::remove_dir_all(&runtime_dir);

    println!("Removed network '{}'", name);
    Ok(())
}

pub fn cmd_network_inspect(name: &str) -> Result<(), Box<dyn std::error::Error>> {
    let net = if name == "remora0" {
        remora::network::bootstrap_default_network()?
    } else {
        NetworkDef::load(name)?
    };

    #[derive(serde::Serialize)]
    struct NetworkInfo {
        name: String,
        subnet: String,
        gateway: String,
        bridge_name: String,
        nft_table: String,
    }

    let info = NetworkInfo {
        name: net.name.clone(),
        subnet: net.subnet.cidr_string(),
        gateway: net.gateway.to_string(),
        bridge_name: net.bridge_name.clone(),
        nft_table: net.nft_table_name(),
    };

    println!("{}", serde_json::to_string_pretty(&info)?);
    Ok(())
}
