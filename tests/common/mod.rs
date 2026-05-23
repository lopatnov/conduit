use std::net::TcpListener;
use std::process::{Child, Command};
use std::time::Duration;

/// A running conduit server process managed for a test.
pub struct TestServer {
    child: Child,
    pub port: u16,
    pub admin_port: u16,
    _dir: tempfile::TempDir,
}

impl TestServer {
    /// Start conduit with a minimal config on OS-assigned ports.
    pub fn start_minimal() -> Self {
        let port = free_port();
        let admin_port = free_port();
        // Use the full AppConfig form so that `global.admin.bind` is honoured.
        // The single-site shorthand parses as SiteConfig and ignores `global`.
        Self::start_with_config(
            port,
            admin_port,
            serde_json::json!({
                "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
                "sites": [{ "port": port }]
            }),
        )
    }

    pub fn start_with_config(port: u16, admin_port: u16, config: serde_json::Value) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg_path = dir.path().join("conduit.json");
        std::fs::write(&cfg_path, config.to_string()).expect("write config");

        let binary = env!("CARGO_BIN_EXE_conduit");
        let child = Command::new(binary)
            .arg("--config")
            .arg(&cfg_path)
            .spawn()
            .expect("spawn conduit");

        let server = TestServer {
            child,
            port,
            admin_port,
            _dir: dir,
        };
        server.wait_ready();
        server
    }

    /// Block until both the proxy and admin API respond or panic after timeout.
    ///
    /// Tries both `http://` and `https://` (with cert validation disabled) on
    /// the proxy port so TLS and non-TLS sites are handled transparently.
    fn wait_ready(&self) {
        let health_http = format!("http://127.0.0.1:{}/__health__", self.port);
        let health_https = format!("https://127.0.0.1:{}/__health__", self.port);
        let admin_url = format!("http://127.0.0.1:{}/status", self.admin_port);

        // Insecure client for self-signed certs used in TLS tests.
        let insecure = reqwest::blocking::Client::builder()
            .danger_accept_invalid_certs(true)
            .build()
            .expect("insecure client");

        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        let mut proxy_ok = false;
        let mut admin_ok = false;
        loop {
            if !proxy_ok {
                // Accept any HTTP response (including 4xx/5xx) — the port is listening.
                if reqwest::blocking::get(&health_http).is_ok()
                    || insecure.get(&health_https).send().is_ok()
                {
                    proxy_ok = true;
                }
            }
            if !admin_ok {
                if let Ok(r) = reqwest::blocking::get(&admin_url) {
                    if r.status().is_success() {
                        admin_ok = true;
                    }
                }
            }
            if proxy_ok && admin_ok {
                return;
            }
            if std::time::Instant::now() >= deadline {
                panic!(
                    "server did not become ready within 15 seconds (proxy={proxy_ok}, admin={admin_ok})"
                );
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    pub fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.port)
    }

    pub fn admin_url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.admin_port)
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.child.kill().ok();
        self.child.wait().ok();
    }
}

/// Bind to port 0, get the OS-assigned port, then release the socket.
pub fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .expect("bind port 0")
        .local_addr()
        .expect("local_addr")
        .port()
}
