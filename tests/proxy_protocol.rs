//! The PROXY protocol on HTTPS ports (`sites[].proxyProtocol`, issue #757): a load balancer's header names the real
//! client, Conduit uses that address for the IP filter and `X-Forwarded-For`, and a connection that does not open with a
//! header from a trusted address is closed before the TLS handshake.
//!
//! The tcp-site side (accept and send) is in `tests/tcp_proxy.rs`; the parser is covered by the unit tests of
//! `conduit_tcp::proxy_protocol`.
//!
//! The sites proxy to a local upstream, so the file needs the `proxy` feature (without it a request falls through to a 404).
#![cfg(feature = "proxy")]

mod common;

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::time::Duration;

use rustls::pki_types::{pem::PemObject, CertificateDer};
use serial_test::serial;

/// An HTTPS GET to `port` over a connection that starts with `preamble` (a PROXY protocol header, or nothing), the way a
/// load balancer opens one. Trusts only `cert_pem`. Returns the whole response, or the error that ended it.
fn https_get_after(port: u16, preamble: &[u8], cert_pem: &str) -> std::io::Result<String> {
    let mut tcp = TcpStream::connect(("127.0.0.1", port))?;
    tcp.set_read_timeout(Some(Duration::from_secs(10)))?;
    tcp.write_all(preamble)?;

    let mut roots = rustls::RootCertStore::empty();
    for cert in CertificateDer::pem_slice_iter(cert_pem.as_bytes()) {
        roots
            .add(cert.expect("a certificate"))
            .expect("trust the test certificate");
    }
    let config = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .expect("protocol versions")
    .with_root_certificates(roots)
    .with_no_client_auth();
    let conn =
        rustls::ClientConnection::new(Arc::new(config), "localhost".try_into().expect("name"))
            .map_err(std::io::Error::other)?;
    let mut tls = rustls::StreamOwned::new(conn, tcp);
    tls.write_all(b"GET /who HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")?;
    let mut out = Vec::new();
    match tls.read_to_end(&mut out) {
        Ok(_) => {}
        // the server closes without a close_notify: what arrived before it is the response
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof && !out.is_empty() => {}
        Err(e) => return Err(e),
    }
    Ok(String::from_utf8_lossy(&out).into_owned())
}

/// A PROXY protocol v1 header naming `client` (an IPv4 address) as the source.
fn v1(client: &str) -> Vec<u8> {
    format!("PROXY TCP4 {client} 192.0.2.1 51000 443\r\n").into_bytes()
}

/// The same as a v2 (binary) header.
fn v2(client: [u8; 4]) -> Vec<u8> {
    let mut h = b"\r\n\r\n\0\r\nQUIT\n".to_vec();
    h.extend_from_slice(&[0x21, 0x11, 0x00, 0x0c]);
    h.extend_from_slice(&client);
    h.extend_from_slice(&[192, 0, 2, 1]);
    h.extend_from_slice(&51000u16.to_be_bytes());
    h.extend_from_slice(&443u16.to_be_bytes());
    h
}

struct Setup {
    _server: common::TestServer,
    _cert_dir: tempfile::TempDir,
    cert_pem: String,
    /// HTTPS port that requires the header from 127.0.0.0/8 and only lets 203.0.113.0/24 in.
    pp_port: u16,
    /// HTTPS port with the same filter that requires the header only from 192.0.2.0/24 (not the test's peer).
    untrusting_port: u16,
    /// HTTPS port that does not read the header.
    plain_tls_port: u16,
}

fn setup() -> Setup {
    let (cert_dir, cert_path, key_path) = common::make_self_signed_cert();
    let cert_pem = std::fs::read_to_string(&cert_path).expect("cert");
    let (echo_port, _echo) = common::start_echo_upstream();
    let (port, admin_port) = (common::free_port(), common::free_port());
    let (pp_port, untrusting_port, plain_tls_port) = (
        common::free_port(),
        common::free_port(),
        common::free_port(),
    );
    let tls = serde_json::json!({ "cert": cert_path, "key": key_path });
    let upstream = format!("http://127.0.0.1:{echo_port}");
    let filter = serde_json::json!({ "allow": ["203.0.113.0/24"] });
    let server = common::TestServer::start_with_config(
        port,
        admin_port,
        serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [
                // the readiness probe of the test harness talks plain HTTP to this one
                { "port": port, "healthCheck": true },
                {
                    "port": pp_port, "tls": tls, "proxy": upstream, "ipFilter": filter,
                    "proxyProtocol": { "trustedSources": ["127.0.0.0/8"], "timeoutMs": 1000 }
                },
                {
                    "port": untrusting_port, "tls": tls, "proxy": upstream, "ipFilter": filter,
                    "proxyProtocol": { "trustedSources": ["192.0.2.0/24"] }
                },
                { "port": plain_tls_port, "tls": tls, "proxy": upstream }
            ]
        }),
    );
    Setup {
        _server: server,
        _cert_dir: cert_dir,
        cert_pem,
        pp_port,
        untrusting_port,
        plain_tls_port,
    }
}

#[test]
#[serial]
fn the_client_in_the_header_is_the_client_for_the_ip_filter_and_x_forwarded_for() {
    let s = setup();

    // v1 and v2: the filter lets 203.0.113.9 in although the peer of the socket is 127.0.0.1 ...
    for header in [v1("203.0.113.9"), v2([203, 0, 113, 9])] {
        let resp = https_get_after(s.pp_port, &header, &s.cert_pem).expect("response");
        assert!(resp.starts_with("HTTP/1.1 200"), "{resp}");
        assert!(
            resp.to_ascii_lowercase()
                .contains(r#""x-forwarded-for":"203.0.113.9""#),
            "the upstream sees the client of the header: {resp}"
        );
    }

    // ... and refuses 198.51.100.7, which the filter does not list
    let resp = https_get_after(s.pp_port, &v1("198.51.100.7"), &s.cert_pem).expect("response");
    assert!(resp.starts_with("HTTP/1.1 403"), "{resp}");

    // an IPv4-mapped IPv6 source is the IPv4 address it carries
    let mapped = b"PROXY TCP6 ::ffff:203.0.113.9 ::ffff:192.0.2.1 51000 443\r\n";
    let resp = https_get_after(s.pp_port, mapped, &s.cert_pem).expect("response");
    assert!(resp.starts_with("HTTP/1.1 200"), "{resp}");
}

#[test]
#[serial]
fn a_connection_without_a_header_or_from_an_untrusted_peer_gets_no_tls() {
    let s = setup();

    // trusted peer, but it goes straight to the TLS handshake: closed, not served
    let direct = https_get_after(s.pp_port, b"", &s.cert_pem);
    assert!(
        direct.is_err(),
        "a TLS hello is not a PROXY header: {direct:?}"
    );

    // a header that is not valid (a v1 line with a port out of range)
    let bad = https_get_after(
        s.pp_port,
        b"PROXY TCP4 203.0.113.9 192.0.2.1 99999 443\r\n",
        &s.cert_pem,
    );
    assert!(bad.is_err(), "{bad:?}");

    // a valid header from a peer that is not in trustedSources: the claimed address is not believed
    let spoof = https_get_after(s.untrusting_port, &v1("203.0.113.9"), &s.cert_pem);
    assert!(spoof.is_err(), "{spoof:?}");

    // a trusted peer that never finishes the header is closed at the deadline (1 s here), not held
    let mut slow = TcpStream::connect(("127.0.0.1", s.pp_port)).expect("connect");
    slow.set_read_timeout(Some(Duration::from_secs(8))).unwrap();
    slow.write_all(b"PROXY TCP4 203.0.").unwrap();
    let started = std::time::Instant::now();
    let mut buf = [0u8; 16];
    let n = slow.read(&mut buf).unwrap_or(0);
    assert_eq!(n, 0, "closed, nothing sent back");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "closed after about timeoutMs, not {:?}",
        started.elapsed()
    );

    // the port without proxyProtocol still serves a plain TLS client
    let ok = https_get_after(s.plain_tls_port, b"", &s.cert_pem).expect("served");
    assert!(ok.starts_with("HTTP/1.1 200"), "{ok}");
}
