# Ongoing Tasks

## Current task: Async TCP port proxy — multi-threaded runtime (2026-02-28)

### Context

Replace thread-per-connection TCP proxy with a tokio multi-threaded runtime.
All accept loops and relay tasks run as async tasks distributed across a small
worker thread pool (capped at `min(available_parallelism, 4)`). UDP unchanged.

Reference: `docs/WATCHER_PROCESS_MODEL.md` § "Port-forward proxy threads".

---

### Thread count change

| Scenario | Before | After |
|----------|--------|-------|
| 1 TCP port, idle | 1 thread | W worker threads (W = min(cpus, 4)) |
| 1 TCP port, N connections | 1 + 2N threads | W threads |
| 1 TCP + 1 UDP port, N TCP connections | 2 + 2N threads | W + 1 threads |

---

### Cargo.toml change

Add `rt-multi-thread` to tokio features:

```toml
tokio = { version = "1", features = ["rt", "rt-multi-thread", "net", "time", "io-util"] }
```

---

### `NetworkSetup` struct changes (`src/network.rs`)

Remove `proxy_stop: Option<Arc<AtomicBool>>`.
Add two separate fields:

```rust
/// Tokio multi-thread runtime owning all TCP proxy tasks. Dropped (shutdown_background)
/// during teardown to cancel accept loops and in-flight relay tasks.
proxy_tcp_runtime: Option<tokio::runtime::Runtime>,
/// Stop flag for UDP proxy threads (std threads, unchanged).
proxy_udp_stop: Option<Arc<AtomicBool>>,
```

`Runtime` doesn't impl `Debug`, so update the manual `Debug` impl for
`NetworkSetup` accordingly (replace `proxy_active` field with
`proxy_tcp_active` and `proxy_udp_active`).

---

### Functions removed from `src/network.rs`

| Removed | Reason |
|---------|--------|
| `start_tcp_proxy_listener` | Replaced by `tcp_accept_loop` async fn |
| `proxy_relay` | Replaced by `tcp_relay` async fn; also kills the raw-pointer `stop_flag` cast |
| `copy_until_done` | Replaced by `tokio::io::copy_bidirectional` |

---

### Functions added / changed in `src/network.rs`

#### `fn start_tcp_proxies_async` (new)

```rust
fn start_tcp_proxies_async(
    container_ip: Ipv4Addr,
    tcp_forwards: &[(u16, u16)],
) -> tokio::runtime::Runtime
```

Builds a `new_multi_thread()` runtime capped at `min(available_parallelism, 4)`
workers. Spawns one `tcp_accept_loop` task per port. Returns the `Runtime` —
the caller stores it; dropping it calls `shutdown_background`.

#### `async fn tcp_accept_loop` (new)

```rust
async fn tcp_accept_loop(host_port: u16, target: SocketAddr)
```

Binds `tokio::net::TcpListener`. Loops on `listener.accept().await`. For each
connection spawns a `tcp_relay` task. No stop flag — cancelled by runtime drop.

#### `async fn tcp_relay` (new)

```rust
async fn tcp_relay(mut client: tokio::net::TcpStream, target: SocketAddr)
```

Connects to `target` with a 5 s timeout. Calls
`tokio::io::copy_bidirectional(&mut client, &mut upstream)`.

#### `start_port_proxies` (changed)

Returns `(Option<tokio::runtime::Runtime>, Option<Arc<AtomicBool>>)` instead of
`Arc<AtomicBool>`. TCP ports → `start_tcp_proxies_async`. UDP ports → existing
`start_udp_proxy` threads unchanged.

---

### `setup_bridge_network` and `teardown_network` changes

`setup_bridge_network`: unpack tuple from `start_port_proxies`, store into
`proxy_tcp_runtime` and `proxy_udp_stop`.

`teardown_network`:
```rust
// TCP — drop runtime (shutdown_background cancels all tasks + worker threads)
if let Some(rt) = setup.proxy_tcp_runtime.take() {
    rt.shutdown_background();
}
// UDP — signal stop flag as before
if let Some(ref stop) = setup.proxy_udp_stop {
    stop.store(true, Ordering::Relaxed);
}
```

---

### New integration test: `test_port_proxy_concurrent_connections`

**Module:** `port_proxy`  **Requires:** root, alpine-rootfs.

Spawn container with port 19192→8080 running an HTTP-ish TCP server that echoes
back a unique token per connection. Open 5 simultaneous `TcpStream` connections
from the host. Write a unique payload on each; read response; assert each gets
its own payload back. Verifies the multi-threaded runtime schedules concurrent
connections correctly.

---

### Docs updates

- `docs/WATCHER_PROCESS_MODEL.md`: update TCP thread inventory (one-thread-per-
  connection → W async worker threads); update thread count formula.
- `docs/INTEGRATION_TESTS.md`: add entry for `test_port_proxy_concurrent_connections`.

---

### Implementation order

1. `Cargo.toml`: add `rt-multi-thread`
2. `src/network.rs`: add `tcp_accept_loop`, `tcp_relay`, `start_tcp_proxies_async`
3. `src/network.rs`: change `start_port_proxies` return type
4. `src/network.rs`: update `NetworkSetup`, its `Debug` impl, `setup_bridge_network`,
   `teardown_network`
5. `cargo test --lib` + integration tests — no regressions
6. Add `test_port_proxy_concurrent_connections`
7. Update docs
8. `cargo clippy -- -D warnings` + `cargo fmt`
9. Commit
