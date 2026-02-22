use std::io::{Read, Write};
use std::net::TcpListener;
use std::time::{SystemTime, UNIX_EPOCH};

fn main() {
    let listener = TcpListener::bind("0.0.0.0:8080").expect("failed to bind to 0.0.0.0:8080");
    eprintln!("listening on 0.0.0.0:8080");

    for stream in listener.incoming() {
        let mut stream = match stream {
            Ok(s) => s,
            Err(e) => {
                eprintln!("accept error: {e}");
                continue;
            }
        };

        let mut buf = [0u8; 1024];
        let _ = stream.read(&mut buf);

        let hostname = std::fs::read_to_string("/etc/hostname").unwrap_or_else(|_| "unknown".into());
        let hostname = hostname.trim();
        let version = env!("CARGO_PKG_VERSION");
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        let body = format!(
            r#"{{"hostname":"{}","version":"{}","timestamp":{}}}"#,
            hostname, version, timestamp
        );
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        let _ = stream.write_all(response.as_bytes());
    }
}
