//! A minimal ACME server for tests (port 0, plain HTTP, no JWS verification): enough of RFC 8555 to create an
//! account and to refuse an order, and of RFC 9773 to answer renewal information. It records what was sent to
//! `newOrder` and which certificate identifiers were asked about, so a test can assert on the wire.
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

/// What the mock CA does with a renewal-information request.
#[derive(Clone)]
pub(super) enum Ari {
    /// The directory has no `renewalInfo` entry at all.
    Absent,
    /// A suggested window (seconds since the Unix epoch).
    Window { start: i64, end: i64 },
    /// The endpoint answers 500.
    Broken,
}

#[derive(Default)]
pub(super) struct Seen {
    /// The decoded JSON payload of every `newOrder` request.
    pub orders: Vec<serde_json::Value>,
    /// The certificate identifier of every renewal-information request.
    pub ari_ids: Vec<String>,
    /// How many accounts were created.
    pub accounts: usize,
}

pub(super) struct MockCa {
    pub directory_url: String,
    pub seen: Arc<Mutex<Seen>>,
    /// While set, a request for the directory is accepted and never answered.
    pub hang_directory: Arc<AtomicBool>,
    /// While set, a `newOrder` request is accepted and never answered.
    pub hang_new_order: Arc<AtomicBool>,
    /// How many `newOrder` requests were accepted and left unanswered because of `hang_new_order`.
    pub hung_new_orders: Arc<AtomicUsize>,
    /// The `Retry-After` (seconds) of every renewal-information answer; 6 hours until a test sets it.
    pub retry_after_secs: Arc<AtomicU64>,
}

fn rfc3339(secs: i64) -> String {
    time::OffsetDateTime::from_unix_timestamp(secs)
        .unwrap()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap()
}

/// Decode the unpadded base64url payload of a flattened JWS body (`{"protected":..,"payload":..,"signature":..}`).
fn jws_payload(body: &str) -> Option<serde_json::Value> {
    let outer: serde_json::Value = serde_json::from_str(body).ok()?;
    let payload = outer.get("payload")?.as_str()?;
    let mut bits = 0u32;
    let mut nbits = 0;
    let mut bytes = Vec::new();
    for c in payload.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'-' => 62,
            b'_' => 63,
            _ => return None,
        };
        bits = (bits << 6) | u32::from(v);
        nbits += 6;
        if nbits >= 8 {
            nbits -= 8;
            bytes.push((bits >> nbits) as u8);
            bits &= (1 << nbits) - 1;
        }
    }
    serde_json::from_slice(&bytes).ok()
}

/// The mock CA's certificate and key (one per process) and the PEM file the ACME client is told to trust through
/// `CONDUIT_ACME_EXTRA_ROOT`. instant-acme speaks only HTTPS, so the mock does too.
struct Tls {
    config: Arc<rustls::ServerConfig>,
    _root: tempfile::TempPath,
}

fn tls() -> &'static Tls {
    static TLS: OnceLock<Tls> = OnceLock::new();
    TLS.get_or_init(|| {
        let key = rcgen::KeyPair::generate().unwrap();
        let params = rcgen::CertificateParams::new(vec!["127.0.0.1".to_owned()]).unwrap();
        let cert = params.self_signed(&key).unwrap();
        let mut root = tempfile::Builder::new()
            .prefix("conduit-mock-ca-root-")
            .suffix(".pem")
            .tempfile()
            .unwrap();
        std::io::Write::write_all(&mut root, cert.pem().as_bytes()).unwrap();
        let root = root.into_temp_path();
        std::env::set_var("CONDUIT_ACME_EXTRA_ROOT", &*root);
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let config = rustls::ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(
                vec![cert.der().clone()],
                rustls::pki_types::PrivatePkcs8KeyDer::from(key.serialize_der()).into(),
            )
            .unwrap();
        Tls {
            config: Arc::new(config),
            _root: root,
        }
    })
}

/// Read one HTTP/1.1 request (head and `Content-Length` body) from `stream`.
fn read_request(stream: &mut impl Read) -> Option<(String, String, String)> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let (head_end, content_length) = loop {
        let n = stream.read(&mut chunk).ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&buf[..pos]).to_ascii_lowercase();
            let len = head
                .lines()
                .find_map(|l| l.strip_prefix("content-length:"))
                .and_then(|v| v.trim().parse::<usize>().ok())
                .unwrap_or(0);
            break (pos + 4, len);
        }
    };
    while buf.len() < head_end + content_length {
        let n = stream.read(&mut chunk).ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
    let body = String::from_utf8_lossy(&buf[head_end..]).into_owned();
    let request_line = head.lines().next().unwrap_or_default();
    let mut parts = request_line.split(' ');
    let method = parts.next().unwrap_or_default().to_owned();
    let path = parts.next().unwrap_or_default().to_owned();
    Some((method, path, body))
}

fn answer(
    base: &str,
    ari: &Ari,
    seen: &Mutex<Seen>,
    retry_after_secs: u64,
    method: &str,
    path: &str,
    body: &str,
) -> String {
    let nonce = "Replay-Nonce: mock-nonce\r\n";
    let json = "Content-Type: application/json\r\n";
    let (status, headers, payload) = match (method, path) {
        ("GET", "/directory") => {
            let mut dir = serde_json::json!({
                "newNonce": format!("{base}/nonce"),
                "newAccount": format!("{base}/new-acct"),
                "newOrder": format!("{base}/new-order"),
                "revokeCert": format!("{base}/revoke"),
                "keyChange": format!("{base}/key-change"),
            });
            if !matches!(ari, Ari::Absent) {
                dir["renewalInfo"] = format!("{base}/ari").into();
            }
            ("200 OK", format!("{json}{nonce}"), dir.to_string())
        }
        ("HEAD" | "GET", "/nonce") => ("200 OK", nonce.to_owned(), String::new()),
        ("POST", "/new-acct") => {
            seen.lock().unwrap().accounts += 1;
            (
                "201 Created",
                format!("{json}{nonce}Location: {base}/acct/1\r\n"),
                r#"{"status":"valid"}"#.to_owned(),
            )
        }
        ("POST", "/new-order") => {
            if let Some(p) = jws_payload(body) {
                seen.lock().unwrap().orders.push(p);
            }
            (
                "400 Bad Request",
                format!("Content-Type: application/problem+json\r\n{nonce}"),
                r#"{"type":"urn:ietf:params:acme:error:malformed","detail":"the mock CA issues nothing and does not know the replaces certificate"}"#
                    .to_owned(),
            )
        }
        ("GET", p) if p.starts_with("/ari/") => {
            seen.lock()
                .unwrap()
                .ari_ids
                .push(p.trim_start_matches("/ari/").to_owned());
            match ari {
                Ari::Window { start, end } => (
                    "200 OK",
                    format!("{json}Retry-After: {retry_after_secs}\r\n"),
                    serde_json::json!({
                        "suggestedWindow": {"start": rfc3339(*start), "end": rfc3339(*end)}
                    })
                    .to_string(),
                ),
                _ => ("500 Internal Server Error", String::new(), String::new()),
            }
        }
        _ => ("404 Not Found", String::new(), String::new()),
    };
    format!(
        "HTTP/1.1 {status}\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n{}",
        payload.len(),
        if method == "HEAD" { "" } else { &payload }
    )
}

impl MockCa {
    /// Start the server on a thread of its own (it ends with the process). Synchronous inside, so a test needs no
    /// extra runtime machinery.
    pub(super) async fn start(ari: Ari) -> MockCa {
        let tls = tls();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("https://{}", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Seen::default()));
        let hang = Arc::new(AtomicBool::new(false));
        let hang_order = Arc::new(AtomicBool::new(false));
        let hung_orders = Arc::new(AtomicUsize::new(0));
        let hu = hung_orders.clone();
        let retry_after = Arc::new(AtomicU64::new(21_600));
        let (b, s, h, ho) = (base.clone(), seen.clone(), hang.clone(), hang_order.clone());
        let ra = retry_after.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { return };
                let (base, ari, seen, config) =
                    (b.clone(), ari.clone(), s.clone(), tls.config.clone());
                let (hang, hang_order, ra, hu) = (h.clone(), ho.clone(), ra.clone(), hu.clone());
                std::thread::spawn(move || {
                    let Ok(conn) = rustls::ServerConnection::new(config) else {
                        return;
                    };
                    let mut stream = rustls::StreamOwned::new(conn, stream);
                    let Some((method, path, body)) = read_request(&mut stream) else {
                        return;
                    };
                    let hang_this = (path == "/directory" && hang.load(Ordering::SeqCst))
                        || (path == "/new-order" && hang_order.load(Ordering::SeqCst));
                    if hang_this {
                        if path == "/new-order" {
                            hu.fetch_add(1, Ordering::SeqCst);
                        }
                        std::thread::sleep(std::time::Duration::from_secs(3600));
                        return;
                    }
                    let response = answer(
                        &base,
                        &ari,
                        &seen,
                        ra.load(Ordering::SeqCst),
                        &method,
                        &path,
                        &body,
                    );
                    let _ = stream.write_all(response.as_bytes());
                    stream.conn.send_close_notify();
                    let _ = stream.flush();
                });
            }
        });
        MockCa {
            directory_url: format!("{base}/directory"),
            seen,
            hang_directory: hang,
            hang_new_order: hang_order,
            hung_new_orders: hung_orders,
            retry_after_secs: retry_after,
        }
    }
}
