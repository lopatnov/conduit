use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::Duration;

/// A running conduit server process managed for a test.
pub struct TestServer {
    child: Child,
    pub port: u16,
    pub admin_port: u16,
    /// Path to the config file — used by `rewrite_config` + `reload`.
    pub cfg_path: PathBuf,
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

        let mut server = TestServer {
            child,
            port,
            admin_port,
            cfg_path: cfg_path.clone(),
            _dir: dir,
        };
        server.wait_ready();
        server
    }

    /// Block until both the proxy and admin API respond or panic after timeout.
    ///
    /// Tries both `http://` and `https://` (with cert validation disabled) on
    /// the proxy port so TLS and non-TLS sites are handled transparently.
    ///
    /// Also polls `child.try_wait()` on every iteration so that a server that
    /// exits prematurely (e.g. due to a port-bind failure) is detected
    /// immediately rather than after the full 15-second deadline.
    fn wait_ready(&mut self) {
        let health_http = format!("http://127.0.0.1:{}/__health__", self.port);
        let health_https = format!("https://127.0.0.1:{}/__health__", self.port);
        let admin_url = format!("http://127.0.0.1:{}/status", self.admin_port);

        // Insecure client for self-signed certs used in TLS tests.
        let insecure = reqwest::blocking::Client::builder()
            .danger_accept_invalid_certs(true)
            .build()
            .expect("insecure client");

        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        let mut proxy_ok = false;
        let mut admin_ok = false;
        loop {
            // Detect premature server exit (e.g. failed port bind) before the
            // deadline so we get a clear message rather than a timeout or a
            // misleading "Connection refused" in the test body.
            match self.child.try_wait() {
                Ok(Some(status)) => panic!(
                    "conduit server exited prematurely with {status} \
                     (proxy_ok={proxy_ok}, admin_ok={admin_ok})"
                ),
                Ok(None) => {} // still running — continue polling
                Err(e) => panic!("could not check server liveness: {e}"),
            }

            if !proxy_ok && probe_proxy(&health_http, &health_https, &insecure) {
                proxy_ok = true;
            }
            if !admin_ok && probe_admin(&admin_url) {
                admin_ok = true;
            }
            if proxy_ok && admin_ok {
                return;
            }
            if std::time::Instant::now() >= deadline {
                panic!(
                    "server did not become ready within 30 seconds (proxy={proxy_ok}, admin={admin_ok})"
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

    /// Overwrite the config file on disk with a new JSON value.
    ///
    /// Call [`Self::reload`] afterwards to apply the change.
    pub fn rewrite_config(&self, config: serde_json::Value) {
        std::fs::write(&self.cfg_path, config.to_string()).expect("rewrite config");
    }

    /// POST to `POST /reload` and return the parsed JSON response.
    pub fn reload(&self) -> serde_json::Value {
        let client = reqwest::blocking::Client::new();
        client
            .post(self.admin_url("/reload"))
            .send()
            .expect("POST /reload")
            .json()
            .expect("JSON response from /reload")
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.child.kill().ok();
        self.child.wait().ok();
    }
}

/// Return `true` when an HTTP response status counts as "server ready".
///
/// Accepts 2xx, 401 (auth-protected health), and 403 (IP-filter tests that
/// deny 127.0.0.1).  Connection errors return `false` so polling continues.
fn is_ready_status(r: reqwest::blocking::Response) -> bool {
    let s = r.status().as_u16();
    r.status().is_success() || s == 401 || s == 403
}

/// Poll both the HTTP and HTTPS health endpoints once.
///
/// Returns `true` if either endpoint replies with a "ready" status.
fn probe_proxy(http: &str, https: &str, insecure: &reqwest::blocking::Client) -> bool {
    let http_ok = reqwest::blocking::get(http)
        .map(is_ready_status)
        .unwrap_or(false);
    let https_ok = insecure
        .get(https)
        .send()
        .map(is_ready_status)
        .unwrap_or(false);
    http_ok || https_ok
}

/// Poll the admin `/status` endpoint once.  Returns `true` on a 2xx response.
fn probe_admin(url: &str) -> bool {
    reqwest::blocking::get(url)
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

/// Bind to port 0, get the OS-assigned port, then release the socket.
pub fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .expect("bind port 0")
        .local_addr()
        .expect("local_addr")
        .port()
}
