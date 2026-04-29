use axum::{Json, http::StatusCode};
use serde_json::{json, Value};

pub async fn version() -> (StatusCode, Json<Value>) {
    let arch = if cfg!(target_arch = "aarch64") { "arm64" } else { "amd64" };
    (StatusCode::OK, Json(json!({
        "Version": "24.0.0",
        "ApiVersion": "1.41",
        "MinAPIVersion": "1.24",
        "Os": "linux",
        "Arch": arch,
        "KernelVersion": "6.1.0",
        "BuildTime": "2024-01-01T00:00:00.000000000+00:00",
        "GitCommit": "pelagos-dockerd",
        "GoVersion": "go1.21.0",
        "Components": []
    })))
}

pub async fn info() -> (StatusCode, Json<Value>) {
    let arch = if cfg!(target_arch = "aarch64") { "aarch64" } else { "x86_64" };
    (StatusCode::OK, Json(json!({
        "ID": "pelagos-dockerd",
        "Containers": 0,
        "ContainersRunning": 0,
        "ContainersPaused": 0,
        "ContainersStopped": 0,
        "Images": 0,
        "Driver": "overlay2",
        "MemoryLimit": true,
        "SwapLimit": true,
        "KernelMemory": false,
        "CpuCfsPeriod": true,
        "CpuCfsQuota": true,
        "CPUShares": true,
        "CPUSet": true,
        "IPv4Forwarding": true,
        "BridgeNfIptables": false,
        "BridgeNfIp6tables": false,
        "Debug": false,
        "OomKillDisable": false,
        "NGoroutines": 1,
        "NEventsListener": 0,
        "LoggingDriver": "json-file",
        "CgroupDriver": "cgroupfs",
        "CgroupVersion": "2",
        "DockerRootDir": "/var/lib/pelagos",
        "HttpProxy": "",
        "HttpsProxy": "",
        "NoProxy": "",
        "Name": "pelagos-dockerd",
        "ServerVersion": "24.0.0",
        "OperatingSystem": "Ubuntu 24.04",
        "OSType": "linux",
        "Architecture": arch,
        "NCPU": 1,
        "MemTotal": 0,
        "IndexServerAddress": "https://index.docker.io/v1/"
    })))
}
