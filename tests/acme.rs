/// Integration tests for ACME / Auto-TLS (Phase 3.1).
///
/// These tests require a running Pebble ACME test server.  In CI the server is
/// provided by a Docker service; locally they can be skipped via the env-var
/// guard below.
///
/// Pebble configuration:
///   - ACME directory:  https://localhost:14000/dir
///   - Challenge validation is disabled (`PEBBLE_VA_ALWAYS_VALID=1`) so no
///     real HTTP-01 request is made.  Conduit still goes through the full
///     order/finalize flow — only the CA-side validation is short-circuited.
mod common;

use serial_test::serial;

/// Return `true` when Pebble is reachable on the well-known port.
///
/// Used as a guard so that ACME tests are skipped gracefully when Pebble is
/// not running (e.g., local dev without Docker).
fn pebble_available() -> bool {
    std::net::TcpStream::connect("127.0.0.1:14000")
        .map(|_| true)
        .unwrap_or(false)
}

/// Full ACME flow: obtain certificate from Pebble and serve HTTPS.
#[test]
#[serial]
fn acme_obtains_certificate_and_serves_https() {
    if !pebble_available() {
        eprintln!("SKIP: Pebble not reachable on 127.0.0.1:14000");
        return;
    }

    let port = common::free_port();
    let admin_port = common::free_port();
    let dir = tempfile::tempdir().expect("tempdir");
    let cert_dir = dir.path().join("certs");

    // Use Pebble staging directory.  domain must be a valid DNS label; Pebble
    // accepts any domain when PEBBLE_VA_ALWAYS_VALID=1 is set.
    let config = serde_json::json!({
        "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
        "sites": [{
            "host": "test.example.com",
            "port": port,
            "tls": {
                "acme": {
                    "email": "test@example.com",
                    "directory": "https://localhost:14000/dir",
                    "storage": cert_dir.to_str().unwrap()
                }
            },
            "healthCheck": true
        }]
    });

    // Write config to disk and start the server.
    let cfg_dir = tempfile::tempdir().expect("tempdir");
    let cfg_path = cfg_dir.path().join("conduit.json");
    std::fs::write(&cfg_path, config.to_string()).expect("write config");

    let binary = env!("CARGO_BIN_EXE_conduit");
    // Propagate RUST_LOG so Conduit emits tracing output that appears in CI
    // logs when --nocapture is used.  Default to "conduit=info" so the ACME
    // flow (including any error messages) is visible without drowning stdout.
    let rust_log = std::env::var("RUST_LOG").unwrap_or_else(|_| "conduit=info".to_owned());
    let mut child = std::process::Command::new(binary)
        .arg("--config")
        .arg(&cfg_path)
        .env("RUST_LOG", &rust_log)
        .spawn()
        .expect("spawn conduit");

    // Wait up to 60 seconds for the HTTPS server to become ready (ACME
    // negotiation can take a few seconds).
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let client = reqwest::blocking::Client::builder()
        .danger_accept_invalid_certs(true)
        .build()
        .expect("http client");

    let mut ready = false;
    while std::time::Instant::now() < deadline {
        match child.try_wait() {
            Ok(Some(status)) => panic!("server exited prematurely: {status}"),
            Ok(None) => {}
            Err(e) => panic!("could not check server: {e}"),
        }
        if client
            .get(format!("https://127.0.0.1:{port}/__health__"))
            .send()
            .map(|r| r.status().is_success())
            .unwrap_or(false)
        {
            ready = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }

    child.kill().ok();
    child.wait().ok();

    assert!(ready, "HTTPS server did not become ready within 60 seconds");

    // Verify the certificate was stored on disk.
    assert!(
        cert_dir.join("test.example.com.crt.pem").exists(),
        "cert file must be stored on disk"
    );
    assert!(
        cert_dir.join("test.example.com.key.pem").exists(),
        "key file must be stored on disk"
    );
}

/// ACME challenge handler: /.well-known/acme-challenge/<token> is served
/// without auth, rate-limiting, or other filters.
#[test]
#[serial]
fn acme_challenge_endpoint_is_accessible() {
    let port = common::free_port();
    let admin_port = common::free_port();

    // Start a plain-HTTP site (no ACME flow needed — we just verify the route).
    let srv = common::TestServer::start_with_config(
        port,
        admin_port,
        serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [{
                "port": port,
                "healthCheck": true
            }]
        }),
    );

    // The challenge store is empty — expect 404 but NOT 401/403/5xx.
    let resp = reqwest::blocking::get(srv.url("/.well-known/acme-challenge/test-token"))
        .expect("GET challenge");
    assert_eq!(
        resp.status().as_u16(),
        404,
        "missing challenge token must return 404, not auth/error"
    );
}
