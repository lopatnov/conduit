//! TLS termination on `tcp` sites (`sites[].tcp` with `sites[].tls`, issue #757): a real server, a real TLS client and a
//! plain upstream that sees only the decrypted bytes.
//!
//! What is shown: the TLS handshake happens in Conduit and the upstream gets plaintext; a client that does not speak TLS
//! is not relayed; `tls.clientAuth` keeps a client without a certificate from the upstream; and with `proxyProtocol` the
//! load balancer's header is read before the handshake, so the client address in it reaches the upstream.

mod common;

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::time::Duration;

use rustls::pki_types::{pem::PemObject, CertificateDer, PrivateKeyDer};
use serial_test::serial;
use tempfile::TempDir;

/// An upstream that answers with every byte it was sent, once it has seen the line `hi`.
fn spawn_echo_upstream() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind upstream");
    let addr = listener.local_addr().expect("local_addr");
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
            let mut got = Vec::new();
            let mut buf = [0u8; 256];
            while !got.ends_with(b"hi\n") {
                match stream.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => got.extend_from_slice(&buf[..n]),
                }
            }
            let _ = stream.write_all(&got);
        }
    });
    addr
}

/// A TLS client that trusts only `server_cert_pem` and presents `identity` (certificate and key, PEM) when it has one.
fn tls_config(server_cert_pem: &str, identity: Option<(&str, &str)>) -> Arc<rustls::ClientConfig> {
    let mut roots = rustls::RootCertStore::empty();
    for cert in CertificateDer::pem_slice_iter(server_cert_pem.as_bytes()) {
        roots
            .add(cert.expect("a certificate"))
            .expect("trust the test certificate");
    }
    let builder = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .expect("protocol versions")
    .with_root_certificates(roots);
    Arc::new(match identity {
        None => builder.with_no_client_auth(),
        Some((cert, key)) => builder
            .with_client_auth_cert(
                CertificateDer::pem_slice_iter(cert.as_bytes())
                    .collect::<Result<_, _>>()
                    .expect("client certificate"),
                PrivateKeyDer::from_pem_slice(key.as_bytes()).expect("client key"),
            )
            .expect("client identity"),
    })
}

/// Sends `preamble` as it is (a PROXY header, or nothing), then a TLS handshake and `hi\n` inside it. Returns what the
/// upstream sent back through the tunnel, or the error that ended the exchange.
fn exchange(
    port: u16,
    preamble: &[u8],
    config: Arc<rustls::ClientConfig>,
) -> std::io::Result<String> {
    // setting up the connection is not what is under test: a failure here must stop the test, not count as a refusal
    let mut tcp = TcpStream::connect(("127.0.0.1", port)).expect("connect to the listener");
    tcp.set_read_timeout(Some(Duration::from_secs(10)))?;
    tcp.write_all(preamble)?;
    let conn = rustls::ClientConnection::new(config, "localhost".try_into().expect("name"))
        .expect("a TLS client connection");
    let mut tls = rustls::StreamOwned::new(conn, tcp);
    tls.write_all(b"hi\n")?;
    let mut out = Vec::new();
    match tls.read_to_end(&mut out) {
        Ok(_) => {}
        // the server closes without a close_notify: what arrived before it is the reply
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof && !out.is_empty() => {}
        Err(e) => return Err(e),
    }
    Ok(String::from_utf8_lossy(&out).into_owned())
}

/// A refusal is the server closing the connection or breaking the handshake off: EOF, a reset or abort, or an alert. Our
/// own read timing out on a connection it left open proves nothing, so it fails the test, and so does a reply.
fn assert_refused(result: &std::io::Result<String>, what: &str) {
    use std::io::ErrorKind::{
        BrokenPipe, ConnectionAborted, ConnectionReset, InvalidData, UnexpectedEof,
    };
    match result {
        Ok(reply) => panic!("{what}: the connection was served: {reply:?}"),
        Err(e) => assert!(
            matches!(
                e.kind(),
                UnexpectedEof | ConnectionReset | ConnectionAborted | BrokenPipe | InvalidData
            ),
            "{what}: expected the server to close the connection, got {e:?}"
        ),
    }
}

struct Setup {
    _server: common::TestServer,
    _dir: TempDir,
    cert_pem: String,
}

/// A server with one `tcp` site on a fresh port that terminates TLS; `extra` is merged into the site.
fn start(extra: serde_json::Value, dir: TempDir, cert_path: &str, key_path: &str) -> (Setup, u16) {
    let upstream = spawn_echo_upstream();
    let (port, admin_port) = (common::free_port(), common::free_port());
    let mut site = serde_json::json!({
        "port": port,
        "tls": { "cert": cert_path, "key": key_path },
        "tcp": { "targets": [upstream.to_string()] }
    });
    for (key, value) in extra.as_object().expect("an object") {
        match (site.get_mut(key), value) {
            (Some(existing), serde_json::Value::Object(add)) if existing.is_object() => {
                existing.as_object_mut().unwrap().extend(add.clone());
            }
            _ => {
                site[key] = value.clone();
            }
        }
    }
    let server = common::TestServer::start_with_config(
        port,
        admin_port,
        serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [site]
        }),
    );
    let cert_pem = std::fs::read_to_string(cert_path).expect("cert");
    (
        Setup {
            _server: server,
            _dir: dir,
            cert_pem,
        },
        port,
    )
}

fn start_plain_tls(extra: serde_json::Value) -> (Setup, u16) {
    let (dir, cert, key) = common::make_self_signed_cert();
    start(extra, dir, &cert, &key)
}

#[test]
#[serial]
fn a_tcp_site_terminates_tls_and_the_upstream_sees_plaintext() {
    let (setup, port) = start_plain_tls(serde_json::json!({}));

    // the upstream echoes what it was sent: `hi\n` in the clear, not TLS records
    let reply = exchange(port, &[], tls_config(&setup.cert_pem, None)).expect("a TLS exchange");
    assert_eq!(reply, "hi\n");

    // a client that does not speak TLS is not relayed
    let mut plain = TcpStream::connect(("127.0.0.1", port)).expect("connect");
    plain
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    plain.write_all(b"hi\n").unwrap();
    let mut out = Vec::new();
    // EOF, or a reset because the server closed with our bytes unread; our own timeout would mean it was left open
    if let Err(e) = plain.read_to_end(&mut out) {
        assert!(
            matches!(
                e.kind(),
                std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::ConnectionAborted
            ),
            "expected EOF or a reset, got {e:?}"
        );
    }
    assert!(
        !out.windows(3).any(|w| w == b"hi\n"),
        "plaintext reached the upstream: {out:?}"
    );

    // the same listener still serves TLS afterwards
    let reply = exchange(port, &[], tls_config(&setup.cert_pem, None)).expect("a TLS exchange");
    assert_eq!(reply, "hi\n");
}

#[test]
#[serial]
fn client_auth_keeps_a_client_without_a_certificate_from_the_upstream() {
    let (dir, cert, key) = common::make_self_signed_cert();
    let (ca_cert, issuer) = common::new_ca();
    let ca_path = common::write_ca_pem(&dir, &ca_cert);
    let (client_cert, client_key) = common::new_client_cert(&issuer);
    let (client_cert_pem, client_key_pem) = (client_cert.pem(), client_key.serialize_pem());
    let (setup, port) = start(
        serde_json::json!({ "tls": { "clientAuth": { "ca": ca_path } } }),
        dir,
        &cert,
        &key,
    );

    // a client with a certificate of the CA gets through (so the refusals below are not a broken server)
    let ok = exchange(
        port,
        &[],
        tls_config(&setup.cert_pem, Some((&client_cert_pem, &client_key_pem))),
    )
    .expect("a client with a certificate");
    assert_eq!(ok, "hi\n");

    // one without a certificate does not
    assert_refused(
        &exchange(port, &[], tls_config(&setup.cert_pem, None)),
        "a client without a certificate",
    );

    // nor does one whose certificate another CA signed
    let (_other_ca, other_issuer) = common::new_ca();
    let (other_cert, other_key) = common::new_client_cert(&other_issuer);
    assert_refused(
        &exchange(
            port,
            &[],
            tls_config(
                &setup.cert_pem,
                Some((&other_cert.pem(), &other_key.serialize_pem())),
            ),
        ),
        "a client whose certificate another CA signed",
    );
}

#[test]
#[serial]
fn the_proxy_protocol_is_read_before_the_handshake_and_its_client_reaches_the_upstream() {
    let (setup, port) = start_plain_tls(serde_json::json!({
        "proxyProtocol": { "trustedSources": ["127.0.0.0/8"], "timeoutMs": 1000 },
        "tcp": { "sendProxyProtocol": "v1" }
    }));

    // header, then the handshake over the same connection: the upstream is told the client of the header
    let header = b"PROXY TCP4 203.0.113.9 192.0.2.1 51000 993\r\n";
    let reply = exchange(port, header, tls_config(&setup.cert_pem, None)).expect("an exchange");
    assert_eq!(
        reply,
        format!("PROXY TCP4 203.0.113.9 127.0.0.1 51000 {port}\r\nhi\n"),
        "the upstream gets a header naming the client of the header, then the decrypted payload"
    );

    // no header: the connection is closed before the handshake, so no TLS and nothing for the upstream
    assert_refused(
        &exchange(port, &[], tls_config(&setup.cert_pem, None)),
        "a connection without the header",
    );
}
