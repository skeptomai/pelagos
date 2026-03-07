//! Compose model: parse S-expression AST into typed structs, validate, and topo-sort.
//!
//! This module is pure validation — no I/O. It transforms the parsed S-expression
//! tree from [`crate::sexpr`] into a [`ComposeFile`] with typed service specs,
//! network specs, volume names, and dependency information.

use crate::sexpr::SExpr;
use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// A fully parsed and validated compose file.
#[derive(Debug, Clone)]
pub struct ComposeFile {
    pub networks: Vec<NetworkSpec>,
    pub volumes: Vec<String>,
    pub services: Vec<ServiceSpec>,
}

/// A network declaration with optional subnet.
#[derive(Debug, Clone)]
pub struct NetworkSpec {
    pub name: String,
    pub subnet: Option<String>,
}

/// A service declaration.
#[derive(Debug, Clone, Default)]
pub struct ServiceSpec {
    pub name: String,
    pub image: String,
    pub networks: Vec<String>,
    pub volumes: Vec<VolumeMount>,
    pub bind_mounts: Vec<BindMount>,
    pub tmpfs_mounts: Vec<String>,
    pub env: HashMap<String, String>,
    pub ports: Vec<PortMapping>,
    pub depends_on: Vec<Dependency>,
    pub memory: Option<String>,
    pub cpus: Option<String>,
    pub command: Option<Vec<String>>,
    pub workdir: Option<String>,
    pub user: Option<String>,
    /// Capabilities to add on top of the default set.
    /// Accepts bare names ("net-raw", "NET_RAW") or prefixed ("CAP_NET_RAW").
    pub cap_add: Vec<String>,
    /// Capabilities to remove from the default set, or "ALL" to start from empty.
    /// Accepts bare names, prefixed names, or the special value "ALL".
    pub cap_drop: Vec<String>,
}

/// A volume mount: `name:path` inside the container.
#[derive(Debug, Clone)]
pub struct VolumeMount {
    pub name: String,
    pub mount_path: String,
}

/// A bind mount: host path → container path, optionally read-only.
#[derive(Debug, Clone)]
pub struct BindMount {
    pub host_path: String,
    pub container_path: String,
    pub read_only: bool,
}

/// A port mapping: host_port → container_port.
#[derive(Debug, Clone)]
pub struct PortMapping {
    pub host: u16,
    pub container: u16,
}

/// A composable health-check expression used in `depends-on`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HealthCheck {
    /// TCP connect to the container's IP on the given port.
    Port(u16),
    /// HTTP GET to the given URL (host is replaced with the container IP at eval time).
    Http(String),
    /// Run a command inside the container's namespaces; passes if exit code is 0.
    Cmd(Vec<String>),
    /// All sub-checks must pass.
    And(Vec<HealthCheck>),
    /// Any sub-check must pass.
    Or(Vec<HealthCheck>),
    /// Wait until the dependency container's `state.json` reports `health == Healthy`.
    /// Used with `:condition service_healthy` in the compose S-expression.
    Healthy,
}

/// A dependency on another service with optional readiness check.
#[derive(Debug, Clone)]
pub struct Dependency {
    pub service: String,
    pub health_check: Option<HealthCheck>,
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Compose-specific errors.
#[derive(Debug, Clone)]
pub enum ComposeError {
    /// Error parsing the S-expression syntax.
    SyntaxError(String),
    /// Missing required field in a service or top-level declaration.
    MissingField(String),
    /// Invalid value for a field.
    InvalidValue(String),
    /// Dependency references a nonexistent service.
    UnknownDependency { service: String, depends_on: String },
    /// Network referenced by a service does not exist.
    UnknownNetwork { service: String, network: String },
    /// Volume referenced by a service does not exist.
    UnknownVolume { service: String, volume: String },
    /// Circular dependency detected.
    DependencyCycle(Vec<String>),
    /// Duplicate service/network/volume name.
    Duplicate(String),
}

impl fmt::Display for ComposeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ComposeError::SyntaxError(msg) => write!(f, "syntax error: {}", msg),
            ComposeError::MissingField(msg) => write!(f, "missing field: {}", msg),
            ComposeError::InvalidValue(msg) => write!(f, "invalid value: {}", msg),
            ComposeError::UnknownDependency {
                service,
                depends_on,
            } => {
                write!(
                    f,
                    "service '{}' depends on unknown service '{}'",
                    service, depends_on
                )
            }
            ComposeError::UnknownNetwork { service, network } => {
                write!(
                    f,
                    "service '{}' references unknown network '{}'",
                    service, network
                )
            }
            ComposeError::UnknownVolume { service, volume } => {
                write!(
                    f,
                    "service '{}' references unknown volume '{}'",
                    service, volume
                )
            }
            ComposeError::DependencyCycle(names) => {
                write!(f, "dependency cycle: {}", names.join(" -> "))
            }
            ComposeError::Duplicate(msg) => write!(f, "duplicate: {}", msg),
        }
    }
}

impl std::error::Error for ComposeError {}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

/// Parse a compose file from its text content.
///
/// This parses the S-expression syntax and transforms it into a [`ComposeFile`].
pub fn parse_compose(content: &str) -> Result<ComposeFile, ComposeError> {
    let ast = crate::sexpr::parse(content).map_err(|e| ComposeError::SyntaxError(e.to_string()))?;
    let items = ast
        .as_list()
        .ok_or_else(|| ComposeError::SyntaxError("top-level must be a list".into()))?;

    if items.is_empty() {
        return Err(ComposeError::SyntaxError("empty compose file".into()));
    }

    let head = items[0]
        .as_atom()
        .ok_or_else(|| ComposeError::SyntaxError("first element must be 'compose'".into()))?;
    if head != "compose" {
        return Err(ComposeError::SyntaxError(format!(
            "expected 'compose', got '{}'",
            head
        )));
    }

    let mut networks = Vec::new();
    let mut volumes = Vec::new();
    let mut services = Vec::new();

    for item in &items[1..] {
        let list = item
            .as_list()
            .ok_or_else(|| ComposeError::SyntaxError("expected a list declaration".into()))?;
        if list.is_empty() {
            continue;
        }
        let kind = list[0].as_atom().ok_or_else(|| {
            ComposeError::SyntaxError("declaration must start with an atom".into())
        })?;
        match kind {
            "network" => networks.push(parse_network_spec(&list[1..])?),
            "volume" => volumes.push(parse_volume_spec(&list[1..])?),
            "service" => services.push(parse_service_spec(&list[1..])?),
            other => {
                return Err(ComposeError::SyntaxError(format!(
                    "unknown declaration '{}'",
                    other
                )));
            }
        }
    }

    let compose = ComposeFile {
        networks,
        volumes,
        services,
    };
    validate(&compose)?;
    Ok(compose)
}

fn parse_network_spec(args: &[SExpr]) -> Result<NetworkSpec, ComposeError> {
    if args.is_empty() {
        return Err(ComposeError::MissingField("network name".into()));
    }
    let name = args[0]
        .as_atom()
        .ok_or_else(|| ComposeError::SyntaxError("network name must be an atom".into()))?
        .to_string();

    let mut subnet = None;
    for arg in &args[1..] {
        if let Some(list) = arg.as_list() {
            if list.len() >= 2 {
                if let Some(key) = list[0].as_atom() {
                    if key == "subnet" {
                        subnet = Some(
                            list[1]
                                .as_atom()
                                .ok_or_else(|| {
                                    ComposeError::InvalidValue("subnet must be a string".into())
                                })?
                                .to_string(),
                        );
                    }
                }
            }
        }
    }

    Ok(NetworkSpec { name, subnet })
}

fn parse_volume_spec(args: &[SExpr]) -> Result<String, ComposeError> {
    if args.is_empty() {
        return Err(ComposeError::MissingField("volume name".into()));
    }
    args[0]
        .as_atom()
        .ok_or_else(|| ComposeError::SyntaxError("volume name must be an atom".into()))
        .map(|s| s.to_string())
}

fn parse_service_spec(args: &[SExpr]) -> Result<ServiceSpec, ComposeError> {
    if args.is_empty() {
        return Err(ComposeError::MissingField("service name".into()));
    }
    let name = args[0]
        .as_atom()
        .ok_or_else(|| ComposeError::SyntaxError("service name must be an atom".into()))?
        .to_string();

    let mut spec = ServiceSpec {
        name: name.clone(),
        image: String::new(),
        networks: Vec::new(),
        volumes: Vec::new(),
        bind_mounts: Vec::new(),
        tmpfs_mounts: Vec::new(),
        env: HashMap::new(),
        ports: Vec::new(),
        depends_on: Vec::new(),
        memory: None,
        cpus: None,
        command: None,
        workdir: None,
        user: None,
        cap_add: Vec::new(),
        cap_drop: Vec::new(),
    };

    for arg in &args[1..] {
        let list = arg.as_list().ok_or_else(|| {
            ComposeError::SyntaxError(format!("service '{}': expected list field", name))
        })?;
        if list.is_empty() {
            continue;
        }
        let key = list[0].as_atom().ok_or_else(|| {
            ComposeError::SyntaxError(format!("service '{}': field name must be atom", name))
        })?;

        match key {
            "image" => {
                spec.image = require_atom(list, 1, &format!("service '{}' image", name))?;
            }
            "network" => {
                for item in &list[1..] {
                    let net = item.as_atom().ok_or_else(|| {
                        ComposeError::InvalidValue(format!(
                            "service '{}': network name must be atom",
                            name
                        ))
                    })?;
                    spec.networks.push(net.to_string());
                }
            }
            "volume" => {
                let vol_name = require_atom(list, 1, &format!("service '{}' volume name", name))?;
                let mount_path = require_atom(list, 2, &format!("service '{}' volume path", name))?;
                spec.volumes.push(VolumeMount {
                    name: vol_name,
                    mount_path,
                });
            }
            "env" => {
                let k = require_atom(list, 1, &format!("service '{}' env key", name))?;
                let v = require_atom(list, 2, &format!("service '{}' env value", name))?;
                spec.env.insert(k, v);
            }
            "port" => {
                let host = require_atom(list, 1, &format!("service '{}' port host", name))?;
                let container =
                    require_atom(list, 2, &format!("service '{}' port container", name))?;
                let host: u16 = host.parse().map_err(|e| {
                    ComposeError::InvalidValue(format!("service '{}' port host: {}", name, e))
                })?;
                let container: u16 = container.parse().map_err(|e| {
                    ComposeError::InvalidValue(format!("service '{}' port container: {}", name, e))
                })?;
                spec.ports.push(PortMapping { host, container });
            }
            "depends-on" => {
                for dep_item in &list[1..] {
                    spec.depends_on.push(parse_dependency(dep_item, &name)?);
                }
            }
            "memory" => {
                spec.memory = Some(require_atom(
                    list,
                    1,
                    &format!("service '{}' memory", name),
                )?);
            }
            "cpus" => {
                spec.cpus = Some(require_atom(list, 1, &format!("service '{}' cpus", name))?);
            }
            "command" => {
                let mut cmd = Vec::new();
                for item in &list[1..] {
                    let s = item.as_atom().ok_or_else(|| {
                        ComposeError::InvalidValue(format!(
                            "service '{}': command args must be atoms",
                            name
                        ))
                    })?;
                    cmd.push(s.to_string());
                }
                spec.command = Some(cmd);
            }
            "workdir" => {
                spec.workdir = Some(require_atom(
                    list,
                    1,
                    &format!("service '{}' workdir", name),
                )?);
            }
            "user" => {
                spec.user = Some(require_atom(list, 1, &format!("service '{}' user", name))?);
            }
            "bind-mount" => {
                let host_path =
                    require_atom(list, 1, &format!("service '{}' bind-mount host path", name))?;
                let container_path = require_atom(
                    list,
                    2,
                    &format!("service '{}' bind-mount container path", name),
                )?;
                let read_only = list[3..].iter().any(|e| e.as_atom() == Some(":ro"));
                spec.bind_mounts.push(BindMount {
                    host_path,
                    container_path,
                    read_only,
                });
            }
            "tmpfs" => {
                let path = require_atom(list, 1, &format!("service '{}' tmpfs path", name))?;
                spec.tmpfs_mounts.push(path);
            }
            "cap-add" => {
                for item in &list[1..] {
                    let cap = item.as_atom().ok_or_else(|| {
                        ComposeError::InvalidValue(format!(
                            "service '{}': cap-add values must be atoms",
                            name
                        ))
                    })?;
                    spec.cap_add.push(cap.to_string());
                }
            }
            "cap-drop" => {
                for item in &list[1..] {
                    let cap = item.as_atom().ok_or_else(|| {
                        ComposeError::InvalidValue(format!(
                            "service '{}': cap-drop values must be atoms",
                            name
                        ))
                    })?;
                    spec.cap_drop.push(cap.to_string());
                }
            }
            other => {
                return Err(ComposeError::SyntaxError(format!(
                    "service '{}': unknown field '{}'",
                    name, other
                )));
            }
        }
    }

    if spec.image.is_empty() {
        return Err(ComposeError::MissingField(format!(
            "service '{}' requires an (image ...) field",
            name
        )));
    }

    Ok(spec)
}

fn parse_dependency(expr: &SExpr, service_name: &str) -> Result<Dependency, ComposeError> {
    match expr {
        SExpr::Atom(name) | SExpr::Str(name) => Ok(Dependency {
            service: name.clone(),
            health_check: None,
        }),
        SExpr::List(items) => {
            if items.is_empty() {
                return Err(ComposeError::InvalidValue(format!(
                    "service '{}': empty depends-on entry",
                    service_name
                )));
            }
            let dep_name = items[0]
                .as_atom()
                .ok_or_else(|| {
                    ComposeError::InvalidValue(format!(
                        "service '{}': dependency name must be atom",
                        service_name
                    ))
                })?
                .to_string();

            let mut health_check = None;
            let mut i = 1;
            while i < items.len() {
                if let Some(kw) = items[i].as_atom() {
                    if kw == ":ready-port" && i + 1 < items.len() {
                        let port_str = items[i + 1].as_atom().ok_or_else(|| {
                            ComposeError::InvalidValue(format!(
                                "service '{}': :ready-port value must be atom",
                                service_name
                            ))
                        })?;
                        let port = port_str.parse::<u16>().map_err(|e| {
                            ComposeError::InvalidValue(format!(
                                "service '{}': :ready-port: {}",
                                service_name, e
                            ))
                        })?;
                        health_check = Some(HealthCheck::Port(port));
                        i += 2;
                        continue;
                    }
                    if kw == ":ready" && i + 1 < items.len() {
                        let check = parse_health_expr(&items[i + 1]).map_err(|e| {
                            ComposeError::InvalidValue(format!(
                                "service '{}': :ready: {}",
                                service_name, e
                            ))
                        })?;
                        health_check = Some(check);
                        i += 2;
                        continue;
                    }
                    if kw == ":condition" && i + 1 < items.len() {
                        let cond = items[i + 1].as_atom().ok_or_else(|| {
                            ComposeError::InvalidValue(format!(
                                "service '{}': :condition value must be an atom",
                                service_name
                            ))
                        })?;
                        match cond {
                            "service_healthy" => {
                                health_check = Some(HealthCheck::Healthy);
                            }
                            other => {
                                return Err(ComposeError::InvalidValue(format!(
                                    "service '{}': unknown :condition '{}' (supported: service_healthy)",
                                    service_name, other
                                )));
                            }
                        }
                        i += 2;
                        continue;
                    }
                }
                i += 1;
            }

            Ok(Dependency {
                service: dep_name,
                health_check,
            })
        }
        SExpr::DottedList(_, _) => Err(ComposeError::InvalidValue(format!(
            "service '{}': depends-on entry must be an atom or list",
            service_name
        ))),
    }
}

/// Parse a health-check S-expression into a [`HealthCheck`] value.
///
/// Supported forms:
/// - `(port N)` → `Port(N)`
/// - `(http "url")` → `Http(url)`
/// - `(cmd "str")` or `(cmd "exe" "arg" ...)` → `Cmd(argv)`
/// - `(and e1 e2 ...)` → `And(checks)`
/// - `(or  e1 e2 ...)` → `Or(checks)`
pub fn parse_health_expr(expr: &SExpr) -> Result<HealthCheck, String> {
    let list = expr
        .as_list()
        .ok_or_else(|| "health check must be a list, got atom".to_string())?;
    if list.is_empty() {
        return Err("empty health check expression".into());
    }
    let head = list[0]
        .as_atom()
        .ok_or_else(|| "health check type must be an atom".to_string())?;

    match head {
        "port" => {
            let s = list
                .get(1)
                .and_then(|e| e.as_atom())
                .ok_or_else(|| "port check requires a port number".to_string())?;
            let p: u16 = s.parse().map_err(|e| format!("invalid port: {}", e))?;
            Ok(HealthCheck::Port(p))
        }
        "http" => {
            let url = list
                .get(1)
                .and_then(|e| e.as_atom())
                .ok_or_else(|| "http check requires a URL string".to_string())?;
            Ok(HealthCheck::Http(url.to_string()))
        }
        "cmd" => {
            if list.len() < 2 {
                return Err("cmd check requires at least one argument".into());
            }
            // If a single string argument: split on whitespace.
            // If multiple atoms: treat as explicit argv.
            if list.len() == 2 {
                let s = list[1]
                    .as_atom()
                    .ok_or_else(|| "cmd argument must be a string".to_string())?;
                let argv: Vec<String> = s.split_whitespace().map(|w| w.to_string()).collect();
                if argv.is_empty() {
                    return Err("cmd check has empty command string".into());
                }
                Ok(HealthCheck::Cmd(argv))
            } else {
                let mut argv = Vec::new();
                for item in &list[1..] {
                    let s = item
                        .as_atom()
                        .ok_or_else(|| "cmd arguments must be atoms".to_string())?;
                    argv.push(s.to_string());
                }
                Ok(HealthCheck::Cmd(argv))
            }
        }
        "and" => {
            if list.len() < 2 {
                return Err("and check requires at least one sub-check".into());
            }
            let checks = list[1..]
                .iter()
                .map(parse_health_expr)
                .collect::<Result<Vec<_>, _>>()?;
            Ok(HealthCheck::And(checks))
        }
        "or" => {
            if list.len() < 2 {
                return Err("or check requires at least one sub-check".into());
            }
            let checks = list[1..]
                .iter()
                .map(parse_health_expr)
                .collect::<Result<Vec<_>, _>>()?;
            Ok(HealthCheck::Or(checks))
        }
        other => Err(format!("unknown health check type: '{}'", other)),
    }
}

fn require_atom(list: &[SExpr], index: usize, context: &str) -> Result<String, ComposeError> {
    list.get(index)
        .and_then(|e| e.as_atom())
        .map(|s| s.to_string())
        .ok_or_else(|| ComposeError::MissingField(context.into()))
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

/// Validate cross-references: dependencies, networks, volumes, and uniqueness.
pub fn validate(compose: &ComposeFile) -> Result<(), ComposeError> {
    let net_names: HashSet<&str> = compose.networks.iter().map(|n| n.name.as_str()).collect();
    let vol_names: HashSet<&str> = compose.volumes.iter().map(|v| v.as_str()).collect();
    let svc_names: HashSet<&str> = compose.services.iter().map(|s| s.name.as_str()).collect();

    // Check duplicate network names.
    {
        let mut seen = HashSet::new();
        for n in &compose.networks {
            if !seen.insert(&n.name) {
                return Err(ComposeError::Duplicate(format!("network '{}'", n.name)));
            }
        }
    }

    // Check duplicate volume names.
    {
        let mut seen = HashSet::new();
        for v in &compose.volumes {
            if !seen.insert(v.as_str()) {
                return Err(ComposeError::Duplicate(format!("volume '{}'", v)));
            }
        }
    }

    // Check duplicate service names.
    {
        let mut seen = HashSet::new();
        for s in &compose.services {
            if !seen.insert(&s.name) {
                return Err(ComposeError::Duplicate(format!("service '{}'", s.name)));
            }
        }
    }

    for svc in &compose.services {
        // Validate network references.
        for net in &svc.networks {
            if !net_names.contains(net.as_str()) {
                return Err(ComposeError::UnknownNetwork {
                    service: svc.name.clone(),
                    network: net.clone(),
                });
            }
        }
        // Validate volume references.
        for vol in &svc.volumes {
            if !vol_names.contains(vol.name.as_str()) {
                return Err(ComposeError::UnknownVolume {
                    service: svc.name.clone(),
                    volume: vol.name.clone(),
                });
            }
        }
        // Validate dependency references.
        for dep in &svc.depends_on {
            if !svc_names.contains(dep.service.as_str()) {
                return Err(ComposeError::UnknownDependency {
                    service: svc.name.clone(),
                    depends_on: dep.service.clone(),
                });
            }
        }
    }

    // Validate topo-sort (detects cycles).
    topo_sort(&compose.services)?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Topological sort (Kahn's algorithm)
// ---------------------------------------------------------------------------

/// Return service names in topological order (dependencies first).
///
/// Returns `DependencyCycle` if a cycle is detected.
pub fn topo_sort(services: &[ServiceSpec]) -> Result<Vec<String>, ComposeError> {
    let names: Vec<&str> = services.iter().map(|s| s.name.as_str()).collect();
    let name_to_idx: HashMap<&str, usize> =
        names.iter().enumerate().map(|(i, n)| (*n, i)).collect();

    let n = services.len();
    let mut in_degree = vec![0usize; n];
    let mut adj: Vec<Vec<usize>> = vec![Vec::new(); n];

    for (i, svc) in services.iter().enumerate() {
        for dep in &svc.depends_on {
            if let Some(&j) = name_to_idx.get(dep.service.as_str()) {
                adj[j].push(i); // j must come before i
                in_degree[i] += 1;
            }
        }
    }

    let mut queue: VecDeque<usize> = VecDeque::new();
    for (i, &deg) in in_degree.iter().enumerate() {
        if deg == 0 {
            queue.push_back(i);
        }
    }

    let mut order = Vec::with_capacity(n);
    while let Some(u) = queue.pop_front() {
        order.push(names[u].to_string());
        for &v in &adj[u] {
            in_degree[v] -= 1;
            if in_degree[v] == 0 {
                queue.push_back(v);
            }
        }
    }

    if order.len() != n {
        // Find a node still with in-degree > 0 to report in the cycle.
        let cycle_members: Vec<String> = in_degree
            .iter()
            .enumerate()
            .filter(|(_, &d)| d > 0)
            .map(|(i, _)| names[i].to_string())
            .collect();
        return Err(ComposeError::DependencyCycle(cycle_members));
    }

    Ok(order)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_minimal_compose() {
        let input = r#"
(compose
  (service app
    (image "alpine:latest")))
"#;
        let compose = parse_compose(input).unwrap();
        assert_eq!(compose.services.len(), 1);
        assert_eq!(compose.services[0].name, "app");
        assert_eq!(compose.services[0].image, "alpine:latest");
    }

    #[test]
    fn test_full_compose() {
        let input = r#"
(compose
  (network backend (subnet "10.88.1.0/24"))
  (network frontend (subnet "10.88.2.0/24"))
  (volume pgdata)

  (service db
    (image "postgres:16")
    (network backend)
    (volume pgdata "/var/lib/postgresql/data")
    (env POSTGRES_PASSWORD "secret")
    (port 5432 5432)
    (memory "512m"))

  (service api
    (image "my-api:latest")
    (network backend frontend)
    (depends-on (db :ready-port 5432))
    (env DATABASE_URL "postgres://db:5432/app")
    (port 8080 8080)
    (cpus "1.0"))

  (service web
    (image "my-web:latest")
    (network frontend)
    (depends-on (api :ready-port 8080))
    (port 80 3000)
    (command "/bin/sh" "-c" "nginx -g 'daemon off;'")))
"#;
        let compose = parse_compose(input).unwrap();
        assert_eq!(compose.networks.len(), 2);
        assert_eq!(compose.volumes.len(), 1);
        assert_eq!(compose.services.len(), 3);

        let db = &compose.services[0];
        assert_eq!(db.name, "db");
        assert_eq!(db.image, "postgres:16");
        assert_eq!(db.networks, vec!["backend"]);
        assert_eq!(db.volumes.len(), 1);
        assert_eq!(db.volumes[0].name, "pgdata");
        assert_eq!(db.volumes[0].mount_path, "/var/lib/postgresql/data");
        assert_eq!(db.env.get("POSTGRES_PASSWORD").unwrap(), "secret");
        assert_eq!(db.ports[0].host, 5432);
        assert_eq!(db.memory.as_deref(), Some("512m"));

        let api = &compose.services[1];
        assert_eq!(api.networks, vec!["backend", "frontend"]);
        assert_eq!(api.depends_on.len(), 1);
        assert_eq!(api.depends_on[0].service, "db");
        assert_eq!(
            api.depends_on[0].health_check,
            Some(HealthCheck::Port(5432))
        );

        let web = &compose.services[2];
        assert_eq!(
            web.command.as_ref().unwrap(),
            &["/bin/sh", "-c", "nginx -g 'daemon off;'"]
        );
    }

    #[test]
    fn test_topo_sort_ordering() {
        let input = r#"
(compose
  (service web
    (image "web")
    (depends-on api))
  (service api
    (image "api")
    (depends-on db))
  (service db
    (image "db")))
"#;
        let compose = parse_compose(input).unwrap();
        let order = topo_sort(&compose.services).unwrap();
        let db_pos = order.iter().position(|n| n == "db").unwrap();
        let api_pos = order.iter().position(|n| n == "api").unwrap();
        let web_pos = order.iter().position(|n| n == "web").unwrap();
        assert!(db_pos < api_pos, "db must come before api");
        assert!(api_pos < web_pos, "api must come before web");
    }

    #[test]
    fn test_cycle_detection() {
        let input = r#"
(compose
  (service a
    (image "a")
    (depends-on b))
  (service b
    (image "b")
    (depends-on a)))
"#;
        let err = parse_compose(input).unwrap_err();
        assert!(
            matches!(err, ComposeError::DependencyCycle(_)),
            "expected DependencyCycle, got: {}",
            err
        );
    }

    #[test]
    fn test_unknown_dependency() {
        let input = r#"
(compose
  (service a
    (image "a")
    (depends-on nonexistent)))
"#;
        let err = parse_compose(input).unwrap_err();
        assert!(
            matches!(err, ComposeError::UnknownDependency { .. }),
            "expected UnknownDependency, got: {}",
            err
        );
    }

    #[test]
    fn test_unknown_network() {
        let input = r#"
(compose
  (service a
    (image "a")
    (network missing)))
"#;
        let err = parse_compose(input).unwrap_err();
        assert!(
            matches!(err, ComposeError::UnknownNetwork { .. }),
            "expected UnknownNetwork, got: {}",
            err
        );
    }

    #[test]
    fn test_unknown_volume() {
        let input = r#"
(compose
  (service a
    (image "a")
    (volume missing "/data")))
"#;
        let err = parse_compose(input).unwrap_err();
        assert!(
            matches!(err, ComposeError::UnknownVolume { .. }),
            "expected UnknownVolume, got: {}",
            err
        );
    }

    #[test]
    fn test_missing_image() {
        let input = r#"
(compose
  (service a
    (network)))
"#;
        // The network field is empty (no args), and image is missing.
        let err = parse_compose(input);
        assert!(err.is_err());
    }

    #[test]
    fn test_duplicate_service() {
        let input = r#"
(compose
  (service a (image "x"))
  (service a (image "y")))
"#;
        let err = parse_compose(input).unwrap_err();
        assert!(
            matches!(err, ComposeError::Duplicate(_)),
            "expected Duplicate, got: {}",
            err
        );
    }

    #[test]
    fn test_parse_health_expr_port() {
        let input = "(compose (service db (image \"db\")) (service app (image \"app\") (depends-on (db :ready (port 5432)))))";
        let compose = parse_compose(input).unwrap();
        assert_eq!(
            compose.services[1].depends_on[0].health_check,
            Some(HealthCheck::Port(5432))
        );
    }

    #[test]
    fn test_parse_health_expr_http() {
        let input = "(compose (service db (image \"db\")) (service app (image \"app\") (depends-on (db :ready (http \"http://localhost:8080/healthz\")))))";
        let compose = parse_compose(input).unwrap();
        assert_eq!(
            compose.services[1].depends_on[0].health_check,
            Some(HealthCheck::Http("http://localhost:8080/healthz".into()))
        );
    }

    #[test]
    fn test_parse_health_expr_cmd_single() {
        let input = "(compose (service db (image \"db\")) (service app (image \"app\") (depends-on (db :ready (cmd \"pg_isready -U postgres\")))))";
        let compose = parse_compose(input).unwrap();
        assert_eq!(
            compose.services[1].depends_on[0].health_check,
            Some(HealthCheck::Cmd(vec![
                "pg_isready".into(),
                "-U".into(),
                "postgres".into()
            ]))
        );
    }

    #[test]
    fn test_parse_health_expr_cmd_multi() {
        let input = "(compose (service db (image \"db\")) (service app (image \"app\") (depends-on (db :ready (cmd \"pg_isready\" \"-U\" \"postgres\")))))";
        let compose = parse_compose(input).unwrap();
        assert_eq!(
            compose.services[1].depends_on[0].health_check,
            Some(HealthCheck::Cmd(vec![
                "pg_isready".into(),
                "-U".into(),
                "postgres".into()
            ]))
        );
    }

    #[test]
    fn test_parse_health_expr_and() {
        let input = "(compose (service db (image \"db\")) (service app (image \"app\") (depends-on (db :ready (and (port 5432) (cmd \"pg_isready\"))))))";
        let compose = parse_compose(input).unwrap();
        assert_eq!(
            compose.services[1].depends_on[0].health_check,
            Some(HealthCheck::And(vec![
                HealthCheck::Port(5432),
                HealthCheck::Cmd(vec!["pg_isready".into()])
            ]))
        );
    }

    #[test]
    fn test_parse_health_expr_or() {
        let input = "(compose (service db (image \"db\")) (service app (image \"app\") (depends-on (db :ready (or (port 8080) (http \"http://localhost:8080/health\"))))))";
        let compose = parse_compose(input).unwrap();
        assert_eq!(
            compose.services[1].depends_on[0].health_check,
            Some(HealthCheck::Or(vec![
                HealthCheck::Port(8080),
                HealthCheck::Http("http://localhost:8080/health".into())
            ]))
        );
    }

    #[test]
    fn test_ready_port_sugar_backward_compat() {
        // :ready-port N is sugar for :ready (port N)
        let input = "(compose (service db (image \"db\")) (service app (image \"app\") (depends-on (db :ready-port 5432))))";
        let compose = parse_compose(input).unwrap();
        assert_eq!(
            compose.services[1].depends_on[0].health_check,
            Some(HealthCheck::Port(5432))
        );
    }

    #[test]
    fn test_parse_health_expr_unknown_type() {
        use crate::sexpr::parse as sexpr_parse;
        let expr = sexpr_parse("(bogus 1234)").unwrap();
        let err = parse_health_expr(&expr);
        assert!(err.is_err());
        assert!(err.unwrap_err().contains("unknown health check type"));
    }

    #[test]
    fn test_dependency_simple_atom() {
        let input = r#"
(compose
  (service db (image "db"))
  (service app
    (image "app")
    (depends-on db)))
"#;
        let compose = parse_compose(input).unwrap();
        assert_eq!(compose.services[1].depends_on[0].service, "db");
        assert_eq!(compose.services[1].depends_on[0].health_check, None);
    }

    #[test]
    fn test_bind_mount_rw() {
        let input = r#"
(compose
  (service app
    (image "alpine:latest")
    (bind-mount "/host/data" "/data")))
"#;
        let compose = parse_compose(input).unwrap();
        let bm = &compose.services[0].bind_mounts;
        assert_eq!(bm.len(), 1);
        assert_eq!(bm[0].host_path, "/host/data");
        assert_eq!(bm[0].container_path, "/data");
        assert!(!bm[0].read_only);
    }

    #[test]
    fn test_bind_mount_ro() {
        let input = r#"
(compose
  (service app
    (image "alpine:latest")
    (bind-mount "/etc/config.yml" "/etc/app/config.yml" :ro)))
"#;
        let compose = parse_compose(input).unwrap();
        let bm = &compose.services[0].bind_mounts;
        assert_eq!(bm.len(), 1);
        assert_eq!(bm[0].host_path, "/etc/config.yml");
        assert_eq!(bm[0].container_path, "/etc/app/config.yml");
        assert!(bm[0].read_only);
    }

    #[test]
    fn test_bind_mount_multiple() {
        let input = r#"
(compose
  (service app
    (image "alpine:latest")
    (bind-mount "/cfg/a.yml" "/etc/a.yml" :ro)
    (bind-mount "/data" "/var/data")))
"#;
        let compose = parse_compose(input).unwrap();
        let bm = &compose.services[0].bind_mounts;
        assert_eq!(bm.len(), 2);
        assert!(bm[0].read_only);
        assert!(!bm[1].read_only);
    }

    #[test]
    fn test_bind_mount_missing_paths() {
        let input = r#"
(compose
  (service app
    (image "alpine:latest")
    (bind-mount "/host/only")))
"#;
        let err = parse_compose(input).unwrap_err();
        assert!(
            matches!(err, ComposeError::MissingField(_)),
            "expected MissingField, got: {}",
            err
        );
    }

    #[test]
    fn test_tmpfs_single() {
        let input = r#"
(compose
  (service app
    (image "alpine:latest")
    (tmpfs "/tmp")))
"#;
        let compose = parse_compose(input).unwrap();
        assert_eq!(compose.services[0].tmpfs_mounts, vec!["/tmp"]);
    }

    #[test]
    fn test_tmpfs_multiple() {
        let input = r#"
(compose
  (service app
    (image "alpine:latest")
    (tmpfs "/tmp")
    (tmpfs "/run")))
"#;
        let compose = parse_compose(input).unwrap();
        assert_eq!(compose.services[0].tmpfs_mounts, vec!["/tmp", "/run"]);
    }

    #[test]
    fn test_tmpfs_missing_path() {
        let input = r#"
(compose
  (service app
    (image "alpine:latest")
    (tmpfs)))
"#;
        let err = parse_compose(input).unwrap_err();
        assert!(
            matches!(err, ComposeError::MissingField(_)),
            "expected MissingField, got: {}",
            err
        );
    }

    #[test]
    fn test_service_user_and_workdir() {
        let input = r#"
(compose
  (service app
    (image "app")
    (user "1000:1000")
    (workdir "/app")))
"#;
        let compose = parse_compose(input).unwrap();
        assert_eq!(compose.services[0].user.as_deref(), Some("1000:1000"));
        assert_eq!(compose.services[0].workdir.as_deref(), Some("/app"));
    }

    #[test]
    fn test_network_without_subnet() {
        let input = r#"
(compose
  (network mynet)
  (service app
    (image "app")
    (network mynet)))
"#;
        let compose = parse_compose(input).unwrap();
        assert_eq!(compose.networks[0].name, "mynet");
        assert!(compose.networks[0].subnet.is_none());
    }

    #[test]
    fn test_example_compose_file() {
        let content = r#"
(compose
  (network frontend (subnet "10.88.1.0/24"))
  (network backend  (subnet "10.88.2.0/24"))
  (volume notes-data)
  (service redis
    (image "web-stack-redis:latest")
    (network backend)
    (memory "64m"))
  (service app
    (image "web-stack-app:latest")
    (network frontend backend)
    (depends-on (redis :ready-port 6379))
    (memory "128m"))
  (service proxy
    (image "web-stack-proxy:latest")
    (network frontend)
    (depends-on (app :ready-port 5000))
    (port 8080 80)
    (memory "32m")))
"#;
        let compose = parse_compose(&content).unwrap();
        assert_eq!(compose.networks.len(), 2);
        assert_eq!(compose.volumes, vec!["notes-data"]);
        assert_eq!(compose.services.len(), 3);

        // Topo order: redis → app → proxy
        let order = topo_sort(&compose.services).unwrap();
        let redis_pos = order.iter().position(|n| n == "redis").unwrap();
        let app_pos = order.iter().position(|n| n == "app").unwrap();
        let proxy_pos = order.iter().position(|n| n == "proxy").unwrap();
        assert!(redis_pos < app_pos);
        assert!(app_pos < proxy_pos);

        // App bridges both networks.
        let app = compose.services.iter().find(|s| s.name == "app").unwrap();
        assert_eq!(app.networks, vec!["frontend", "backend"]);
        assert_eq!(app.depends_on[0].service, "redis");
        assert_eq!(
            app.depends_on[0].health_check,
            Some(HealthCheck::Port(6379))
        );
    }
}
