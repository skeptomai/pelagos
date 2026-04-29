//! Bollard smoke test against a running pelagos-dockerd instance.
//!
//! Requires pelagos-dockerd listening at /var/run/pelagos-dockerd.sock.
//! Run with:
//!   PELAGOS_DOCKERD_SOCK=/var/run/pelagos-dockerd.sock cargo test --test dockerd_smoke

#[cfg(target_os = "linux")]
mod smoke {
    use bollard::Docker;
    use bollard::container::{CreateContainerOptions, Config, StartContainerOptions, RemoveContainerOptions};
    use bollard::models::HostConfig;

    fn connect() -> Docker {
        let sock = std::env::var("PELAGOS_DOCKERD_SOCK")
            .unwrap_or_else(|_| "/var/run/pelagos-dockerd.sock".to_string());
        Docker::connect_with_unix(&sock, 30, bollard::API_DEFAULT_VERSION).expect("connect")
    }

    #[tokio::test]
    async fn version_returns_api_version() {
        let docker = connect();
        let v = docker.version().await.expect("version()");
        let api = v.api_version.expect("ApiVersion missing");
        assert!(!api.is_empty(), "ApiVersion is empty");
        println!("ApiVersion: {}", api);
    }

    #[tokio::test]
    async fn list_containers_returns_vec() {
        let docker = connect();
        let containers = docker
            .list_containers(Some(bollard::container::ListContainersOptions::<String> {
                all: true,
                ..Default::default()
            }))
            .await
            .expect("list_containers()");
        println!("containers: {} found", containers.len());
    }

    #[tokio::test]
    async fn create_start_inspect_remove() {
        let docker = connect();
        let name = "bollard-smoke-test";

        // Clean up any leftover from previous run
        let _ = docker
            .remove_container(
                name,
                Some(RemoveContainerOptions { force: true, ..Default::default() }),
            )
            .await;

        // Create
        docker
            .create_container(
                Some(CreateContainerOptions { name, platform: None }),
                Config {
                    image: Some("alpine:latest"),
                    cmd: Some(vec!["sleep", "10"]),
                    host_config: Some(HostConfig {
                        network_mode: Some("bridge".to_string()),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            )
            .await
            .expect("create_container()");

        // Start
        docker
            .start_container(name, None::<StartContainerOptions<String>>)
            .await
            .expect("start_container()");

        // Inspect
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        let info = docker.inspect_container(name, None).await.expect("inspect_container()");
        let state = info.state.expect("State missing");
        assert_eq!(state.running, Some(true), "container not running");
        let ip = info
            .network_settings
            .expect("NetworkSettings missing")
            .networks
            .expect("Networks missing")
            .get("bridge")
            .and_then(|ep| ep.ip_address.clone())
            .unwrap_or_default();
        assert!(!ip.is_empty(), "bridge IP is empty");
        println!("bridge IP: {}", ip);

        // Remove (force stops + removes)
        docker
            .remove_container(
                name,
                Some(RemoveContainerOptions { force: true, ..Default::default() }),
            )
            .await
            .expect("remove_container()");
    }
}

#[cfg(not(target_os = "linux"))]
#[test]
fn smoke_tests_linux_only() {
    // No-op on non-Linux.
}
