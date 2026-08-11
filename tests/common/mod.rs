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
    // Accept both 200 (no auth) and 401 (auth required) as "admin is ready".
    // 401 means the admin server is up and responding, it just requires a token.
    reqwest::blocking::get(url)
        .map(|r| r.status().is_success() || r.status().as_u16() == 401)
        .unwrap_or(false)
}

/// Start a minimal HTTP echo server on an OS-assigned free port.
///
/// Responds to every request with a JSON body:
/// ```json
/// { "method": "GET", "path": "/...", "headers": { "x-foo": "bar", ... } }
/// ```
/// Binds to port 0 itself (rather than taking a pre-selected port from
/// [`free_port`]) so there's no TOCTOU gap between choosing a port and
/// actually binding it — the exact race `free_port`'s own doc comment
/// describes, which still occurs *across* test binaries since each binary's
/// atomic counter independently cycles the same 10000-19999 range.
///
/// Returns the bound port and a [`std::thread::JoinHandle`] that keeps the
/// server alive as long as the handle is in scope.  Drop it to shut down
/// the server.
pub fn start_echo_upstream() -> (u16, std::thread::JoinHandle<()>) {
    use std::io::{Read, Write};
    use std::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").expect("echo bind");
    let port = listener.local_addr().expect("echo local_addr").port();
    let handle = std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            // Read the HTTP request.
            let mut buf = vec![0u8; 4096];
            let n = stream.read(&mut buf).unwrap_or(0);
            let raw = String::from_utf8_lossy(&buf[..n]);

            // Parse method, path, and headers from the raw HTTP/1.1 request.
            let mut lines = raw.lines();
            let first = lines.next().unwrap_or("");
            let mut parts = first.split_whitespace();
            let method = parts.next().unwrap_or("GET").to_owned();
            let path = parts.next().unwrap_or("/").to_owned();

            let mut headers = serde_json::Map::new();
            for line in &mut lines {
                if line.is_empty() {
                    break;
                }
                if let Some((k, v)) = line.split_once(": ") {
                    headers.insert(
                        k.to_ascii_lowercase(),
                        serde_json::Value::String(v.to_owned()),
                    );
                }
            }

            let body = serde_json::json!({ "method": method, "path": path, "headers": headers });
            let body_str = body.to_string();
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body_str.len(),
                body_str
            );
            let _ = stream.write_all(response.as_bytes());
        }
    });
    (port, handle)
}

/// Return a free TCP port on 127.0.0.1.
///
/// Uses ports in the range 10000–19999, which is below the Linux/macOS OS
/// ephemeral port range (32768+).  Picking from the ephemeral range meant
/// that the OS could assign the same port to an outgoing connection in the
/// narrow window between `free_port()` releasing the socket and the Conduit
/// server binding it, causing flaky "Address already in use" failures.
///
/// An atomic counter gives each call within the same process a different
/// starting point, further reducing intra-binary conflicts.
pub fn free_port() -> u16 {
    use std::sync::atomic::{AtomicU16, Ordering};
    static NEXT: AtomicU16 = AtomicU16::new(10000);
    loop {
        // Wrap monotonically within 10000–19999.
        let p = NEXT.fetch_add(1, Ordering::Relaxed) % 10000 + 10000;
        if TcpListener::bind(format!("127.0.0.1:{p}")).is_ok() {
            return p;
        }
        // Port in use — skip it and try the next one.
    }
}
