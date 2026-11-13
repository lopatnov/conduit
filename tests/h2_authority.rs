//! The request host on HTTP/2 and HTTP/1.1 (PR #703, issue #704).
//!
//! A browser on HTTP/2 sends no `host` header: the authority rides in `:authority`. These tests send real requests
//! over TLS (h2 is negotiated through ALPN) and over plain HTTP/1.1 to a running Conduit with a site whose `host` is
//! `app.test`, and look at what the upstream (the echo server of `tests/common`) received. The names are the ones
//! RFC 6761 reserves for tests (`.test`) and the address is the loopback: nothing here depends on the Internet or on a
//! name that someone owns.
//!
//! What they pin, so that a change in Conduit or in Pingora cannot lose it unnoticed:
//!   - an allowed host over h2 is served (the bug of PR #703: every browser got `400 host not allowed`), a host that is
//!     not allowed is refused, on h2 and on HTTP/1.1 alike;
//!   - the upstream gets a `Host` and an `X-Forwarded-Host` that carry the authority the client asked for;
//!   - Host header injection: a `host` that disagrees with `:authority`, an absolute-form target that disagrees with
//!     `Host`, two `Host` headers and a `Host` with a control character are all refused before they reach the upstream
//!     (this is Pingora's check, RFC 9112 §3.2 and RFC 9113 §8.3.1, which Conduit relies on when it prefers `host`);
//!   - header spoofing: a client's own `X-Forwarded-Host` never reaches the upstream.

mod common;

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

use serial_test::serial;
use tempfile::TempDir;

const SITE_HOST: &str = "app.test";
const OTHER_HOST: &str = "evil.test";
/// A second name the site accepts (`allowedHosts`), so that a request that mixes it with `SITE_HOST` is refused because
/// the two disagree, and not because one of them is not allowed.
const ALSO_ALLOWED_HOST: &str = "other.test";

/// A self-signed certificate for `localhost`; the clients below accept any certificate, so the name is of no
/// importance.
fn make_self_signed_cert() -> (TempDir, String, String) {
    use rcgen::{generate_simple_self_signed, CertifiedKey};

    let CertifiedKey { cert, signing_key } =
        generate_simple_self_signed(vec!["localhost".to_string()]).expect("rcgen cert generation");
    let dir = tempfile::tempdir().expect("tempdir");
    let cert_path = dir.path().join("cert.pem");
    let key_path = dir.path().join("key.pem");
    std::fs::write(&cert_path, cert.pem()).expect("write cert.pem");
    std::fs::write(&key_path, signing_key.serialize_pem()).expect("write key.pem");
    (
        dir,
        cert_path.to_string_lossy().into_owned(),
        key_path.to_string_lossy().into_owned(),
    )
}

/// A running Conduit with the same site on a TLS port (h2 and HTTP/1.1) and on a plain port (HTTP/1.1 only), proxying
/// to the echo upstream.
struct Fixture {
    _server: common::TestServer,
    _cert_dir: TempDir,
    tls_port: u16,
    plain_port: u16,
}

impl Fixture {
    fn start() -> Self {
        let (cert_dir, cert_path, key_path) = make_self_signed_cert();
        let (upstream_port, _upstream) = common::start_echo_upstream();
        let tls_port = common::free_port();
        let plain_port = common::free_port();
        let admin_port = common::free_port();
        let upstream = format!("http://127.0.0.1:{upstream_port}");
        let server = common::TestServer::start_with_config(
            tls_port,
            admin_port,
            serde_json::json!({
                "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
                "sites": [
                    {
                        "port": tls_port,
                        "host": SITE_HOST,
                        "securityHeaders": { "allowedHosts": [SITE_HOST, ALSO_ALLOWED_HOST] },
                        "tls": { "cert": cert_path, "key": key_path },
                        // without `http2` the server does not offer h2 in ALPN
                        "http2": { "maxConcurrentStreams": 100 },
                        "proxy": upstream
                    },
                    {
                        "port": plain_port,
                        "host": SITE_HOST,
                        "securityHeaders": { "allowedHosts": [SITE_HOST, ALSO_ALLOWED_HOST] },
                        "proxy": upstream
                    }
                ]
            }),
        );
        Self {
            _server: server,
            _cert_dir: cert_dir,
            tls_port,
            plain_port,
        }
    }

    /// A client for `host`, which it resolves to the loopback, so that the URL (and with it `:authority`) names the
    /// test host and not an address.
    fn client(&self, host: &str, http1_only: bool) -> reqwest::Client {
        let addr: SocketAddr = ([127, 0, 0, 1], self.tls_port).into();
        let builder = reqwest::Client::builder()
            .danger_accept_invalid_certs(true)
            .resolve(host, addr)
            .timeout(Duration::from_secs(10));
        let builder = if http1_only {
            builder.http1_only()
        } else {
            builder
        };
        builder.build().expect("client")
    }

    fn url(&self, host: &str) -> String {
        format!("https://{host}:{}/", self.tls_port)
    }

    /// What Conduit sent to the upstream, as the echo server reports it: the lower-cased header names to values.
    fn upstream_headers(body: &serde_json::Value) -> &serde_json::Map<String, serde_json::Value> {
        body["headers"].as_object().expect("headers object")
    }

    /// Send `raw` over plain HTTP/1.1 to the plain port, read everything the server answers, return it as text.
    fn raw(&self, raw: &[u8]) -> String {
        let mut stream =
            TcpStream::connect(("127.0.0.1", self.plain_port)).expect("connect to the plain port");
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .expect("read timeout");
        stream.write_all(raw).expect("write the request");
        let mut answer = Vec::new();
        let _ = stream.read_to_end(&mut answer);
        String::from_utf8_lossy(&answer).into_owned()
    }
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Runtime::new().expect("tokio runtime")
}

/// The bug of PR #703: an allowed host, asked for over HTTP/2, is served, and the upstream sees it.
#[test]
#[serial]
fn h2_request_for_the_site_host_is_served_and_the_upstream_sees_the_authority() {
    let f = Fixture::start();
    let authority = format!("{SITE_HOST}:{}", f.tls_port);
    let (status, version, body) = runtime().block_on(async {
        let resp = f
            .client(SITE_HOST, false)
            .get(f.url(SITE_HOST))
            .send()
            .await
            .expect("h2 GET");
        let status = resp.status().as_u16();
        let version = resp.version();
        let body: serde_json::Value = resp.json().await.expect("json body");
        (status, version, body)
    });
    assert_eq!(
        version,
        reqwest::Version::HTTP_2,
        "the test must run over h2"
    );
    assert_eq!(status, 200, "an allowed host over h2 must be served");
    let headers = Fixture::upstream_headers(&body);
    assert_eq!(
        headers.get("host").and_then(|v| v.as_str()),
        Some(authority.as_str()),
        "the upstream needs a Host (Pingora sets it from the authority): {body}"
    );
    assert_eq!(
        headers.get("x-forwarded-host").and_then(|v| v.as_str()),
        Some(authority.as_str()),
        "{body}"
    );
}

/// The same request over HTTP/1.1 behaves the same way.
#[test]
#[serial]
fn http1_request_for_the_site_host_is_served_and_the_upstream_sees_the_host() {
    let f = Fixture::start();
    let authority = format!("{SITE_HOST}:{}", f.tls_port);
    let (status, version, body) = runtime().block_on(async {
        let resp = f
            .client(SITE_HOST, true)
            .get(f.url(SITE_HOST))
            .send()
            .await
            .expect("HTTP/1.1 GET");
        let status = resp.status().as_u16();
        let version = resp.version();
        let body: serde_json::Value = resp.json().await.expect("json body");
        (status, version, body)
    });
    assert_eq!(version, reqwest::Version::HTTP_11);
    assert_eq!(status, 200);
    let headers = Fixture::upstream_headers(&body);
    assert_eq!(
        headers.get("host").and_then(|v| v.as_str()),
        Some(authority.as_str())
    );
    assert_eq!(
        headers.get("x-forwarded-host").and_then(|v| v.as_str()),
        Some(authority.as_str())
    );
}

/// A host that is not the site's is refused over h2 and over HTTP/1.1, and the upstream is not reached.
#[test]
#[serial]
fn a_host_that_is_not_allowed_is_refused_on_h2_and_on_http1() {
    let f = Fixture::start();
    for http1_only in [false, true] {
        let (status, version) = runtime().block_on(async {
            let resp = f
                .client(OTHER_HOST, http1_only)
                .get(f.url(OTHER_HOST))
                .send()
                .await
                .expect("GET");
            (resp.status().as_u16(), resp.version())
        });
        assert_eq!(
            version,
            if http1_only {
                reqwest::Version::HTTP_11
            } else {
                reqwest::Version::HTTP_2
            }
        );
        assert_eq!(status, 400, "http1_only={http1_only}");
    }
}

/// Host header injection over h2: a `host` header that disagrees with `:authority` must not get anywhere. Pingora
/// refuses the request (RFC 9113 §8.3.1); Conduit prefers `host`, so it relies on this. Both names are allowed by the
/// site, so only the disagreement can be the reason for the refusal; and the request would be served (200) if the
/// client did not send both, so a pass shows that it did.
#[test]
#[serial]
fn h2_request_with_a_host_header_that_disagrees_with_the_authority_is_refused() {
    let f = Fixture::start();
    for (authority_host, header_host) in [
        (SITE_HOST, ALSO_ALLOWED_HOST),
        (ALSO_ALLOWED_HOST, SITE_HOST),
    ] {
        let (status, version) = runtime().block_on(async {
            let resp = f
                .client(authority_host, false)
                .get(f.url(authority_host))
                .header(reqwest::header::HOST, header_host)
                .send()
                .await;
            let r = resp.expect("the refusal is a 400 response, not a transport error");
            (r.status().as_u16(), r.version())
        });
        assert_eq!(version, reqwest::Version::HTTP_2);
        assert_eq!(
            status, 400,
            ":authority {authority_host} with host {header_host} must be refused"
        );
    }
}

/// Host header injection over HTTP/1.1, with bytes a normal client does not send. Each is refused with 400 and none
/// reaches the upstream (the echo server would have answered 200).
#[test]
#[serial]
fn ambiguous_http1_hosts_are_refused() {
    let f = Fixture::start();
    let port = f.plain_port;
    let cases: [(&str, String); 4] = [
        (
            "an absolute-form target that disagrees with Host (RFC 9112 §3.2.2)",
            format!(
                "GET http://{OTHER_HOST}:{port}/ HTTP/1.1\r\nHost: {SITE_HOST}:{port}\r\nConnection: close\r\n\r\n"
            ),
        ),
        (
            "two Host headers (RFC 9112 §3.2)",
            format!(
                "GET / HTTP/1.1\r\nHost: {SITE_HOST}:{port}\r\nHost: {OTHER_HOST}\r\nConnection: close\r\n\r\n"
            ),
        ),
        (
            "a Host with userinfo",
            format!("GET / HTTP/1.1\r\nHost: user@{SITE_HOST}:{port}\r\nConnection: close\r\n\r\n"),
        ),
        (
            "a Host with a control character",
            format!("GET / HTTP/1.1\r\nHost: {SITE_HOST}\x01:{port}\r\nConnection: close\r\n\r\n"),
        ),
    ];
    for (what, request) in cases {
        let answer = f.raw(request.as_bytes());
        assert!(
            answer.starts_with("HTTP/1.1 400"),
            "{what} must be refused with 400; the answer was: {answer}"
        );
        assert!(
            !answer.contains("\"headers\""),
            "{what} must not reach the upstream (the echo body is in the answer): {answer}"
        );
    }
}

/// Header spoofing: the client's own `X-Forwarded-Host` is replaced, never passed on, and so cannot make the upstream
/// build a URL for another host.
#[test]
#[serial]
fn a_clients_x_forwarded_host_does_not_reach_the_upstream() {
    let f = Fixture::start();
    let port = f.plain_port;
    let answer = f.raw(
        format!(
            "GET / HTTP/1.1\r\nHost: {SITE_HOST}:{port}\r\nX-Forwarded-Host: {OTHER_HOST}\r\nConnection: close\r\n\r\n"
        )
        .as_bytes(),
    );
    assert!(answer.starts_with("HTTP/1.1 200"), "{answer}");
    let body = answer.split("\r\n\r\n").nth(1).expect("a response body");
    let body: serde_json::Value = serde_json::from_str(body).expect("json body");
    let headers = Fixture::upstream_headers(&body);
    assert_eq!(
        headers.get("x-forwarded-host").and_then(|v| v.as_str()),
        Some(format!("{SITE_HOST}:{port}").as_str()),
        "the upstream must see the host Conduit saw, not the client's claim: {body}"
    );
    assert!(
        !body.to_string().contains(OTHER_HOST),
        "nothing of the spoofed host may reach the upstream: {body}"
    );
}

/// The control of the test above: the second name on its own is served, so the refusal there is the disagreement.
#[test]
#[serial]
fn h2_request_for_the_second_allowed_host_is_served() {
    let f = Fixture::start();
    let status = runtime().block_on(async {
        f.client(ALSO_ALLOWED_HOST, false)
            .get(f.url(ALSO_ALLOWED_HOST))
            .send()
            .await
            .expect("h2 GET")
            .status()
            .as_u16()
    });
    assert_eq!(status, 200);
}

/// An authority-form `CONNECT` is not served: Pingora answers 405 and nothing reaches the upstream. With the h2
/// `:authority` fallback a CONNECT carries a host that `allowedHosts` accepts, so this is what keeps it out.
#[test]
#[serial]
fn connect_is_not_served() {
    let f = Fixture::start();
    let port = f.plain_port;
    let answer = f.raw(
        format!(
            "CONNECT {SITE_HOST}:{port} HTTP/1.1
Host: {SITE_HOST}:{port}
Connection: close

"
        )
        .as_bytes(),
    );
    assert!(answer.starts_with("HTTP/1.1 405"), "{answer}");
    assert!(!answer.contains("\"headers\""), "{answer}");
}
