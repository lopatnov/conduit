mod common;

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::time::Duration;

use serial_test::serial;

// ── Mock WebSocket upstream ───────────────────────────────────────────────────

/// Spawn a minimal WebSocket server in a background thread.
///
/// The server performs the HTTP upgrade handshake and then echoes each
/// WebSocket frame back to the caller (unmasked, as a server).
///
/// Returns the local address for the caller to proxy to.
fn spawn_ws_upstream() -> std::net::SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ws upstream");
    let addr = listener.local_addr().expect("local_addr");

    std::thread::spawn(move || {
        if let Ok((stream, _)) = listener.accept() {
            handle_ws_connection(stream);
        }
    });

    addr
}

/// Handle one WebSocket connection: perform the HTTP upgrade handshake and
/// run the echo loop until the peer sends a close frame or disconnects.
fn handle_ws_connection(stream: std::net::TcpStream) {
    let mut reader = BufReader::new(stream.try_clone().expect("clone"));
    let mut writer = stream;

    // Read HTTP request headers until the blank line.
    let mut headers = Vec::new();
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).is_err() || line == "\r\n" || line.is_empty() {
            break;
        }
        headers.push(line);
    }

    // Send 101 Switching Protocols with the computed Sec-WebSocket-Accept.
    let key = extract_ws_key(&headers);
    send_ws_101(&mut writer, &key);

    // Echo loop.
    let mut raw = reader.into_inner();
    raw.set_read_timeout(Some(Duration::from_secs(5))).ok();
    ws_echo_loop(&mut raw, &mut writer);
}

/// Extract the `Sec-WebSocket-Key` value from the request headers.
fn extract_ws_key(headers: &[String]) -> String {
    headers
        .iter()
        .find(|h| h.to_ascii_lowercase().starts_with("sec-websocket-key:"))
        .and_then(|h| h.split_once(':'))
        .map(|(_, v)| v.trim().to_owned())
        .unwrap_or_default()
}

/// Send an `HTTP/1.1 101 Switching Protocols` response with the correct
/// `Sec-WebSocket-Accept` header computed from `key`.
///
/// Computation per RFC 6455 §1.3:
/// `base64( SHA-1( key + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11" ) )`
fn send_ws_101(writer: &mut std::net::TcpStream, key: &str) {
    let combined = format!("{key}258EAFA5-E914-47DA-95CA-C5AB0DC85B11");
    let accept = base64_encode(&sha1(combined.as_bytes()));
    let resp = format!(
        "HTTP/1.1 101 Switching Protocols\r\n\
         Connection: Upgrade\r\n\
         Upgrade: websocket\r\n\
         Sec-WebSocket-Accept: {accept}\r\n\
         \r\n"
    );
    writer.write_all(resp.as_bytes()).ok();
}

/// Echo loop: read WebSocket frames from `raw` and write them back to `writer`
/// (server never masks outgoing frames per RFC 6455).
fn ws_echo_loop(raw: &mut std::net::TcpStream, writer: &mut std::net::TcpStream) {
    let mut buf = [0u8; 4096];
    while let Some((fin_opcode, payload)) = read_ws_frame(raw, &mut buf) {
        writer.write_all(&[fin_opcode, payload.len() as u8]).ok();
        writer.write_all(&payload).ok();
        if fin_opcode & 0x0f == 0x8 {
            // Connection close opcode — stop echoing.
            break;
        }
    }
}

/// Read one WebSocket frame from `raw`.
///
/// Returns `Some((fin_opcode, unmasked_payload))` on success, `None` on any
/// read error or a truncated frame.
fn read_ws_frame(raw: &mut impl std::io::Read, buf: &mut [u8; 4096]) -> Option<(u8, Vec<u8>)> {
    // Frame header: at least 2 bytes.
    let n = read_exact_timeout(raw, &mut buf[..2]).ok()?;
    if n < 2 {
        return None;
    }

    let fin_opcode = buf[0];
    let mask_len = buf[1];
    let masked = (mask_len & 0x80) != 0;
    let payload_len = (mask_len & 0x7f) as usize;

    // Optional 4-byte masking key.
    let mask_key = if masked {
        read_exact_timeout(raw, &mut buf[2..6]).ok()?;
        [buf[2], buf[3], buf[4], buf[5]]
    } else {
        [0u8; 4]
    };

    // Payload.
    let mut payload = vec![0u8; payload_len];
    if payload_len > 0 {
        read_exact_timeout(raw, &mut payload).ok()?;
    }

    // Unmask in-place.
    if masked {
        for (i, b) in payload.iter_mut().enumerate() {
            *b ^= mask_key[i % 4];
        }
    }

    Some((fin_opcode, payload))
}

// ── Utility: read exactly N bytes with a hard deadline ───────────────────────

fn read_exact_timeout(stream: &mut impl std::io::Read, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut total = 0;
    while total < buf.len() {
        match stream.read(&mut buf[total..]) {
            Ok(0) => break,
            Ok(n) => total += n,
            Err(e) => return Err(e),
        }
    }
    Ok(total)
}

// ── Minimal SHA-1 (FIPS 180-4) ────────────────────────────────────────────────
//
// Only used in tests — not production code.

fn sha1(data: &[u8]) -> [u8; 20] {
    const K: [u32; 4] = [0x5a827999, 0x6ed9eba1, 0x8f1bbcdc, 0xca62c1d6];

    let mut h: [u32; 5] = [0x67452301, 0xefcdab89, 0x98badcfe, 0x10325476, 0xc3d2e1f0];

    // Pre-processing: pad the message.
    let bit_len = (data.len() as u64) * 8;
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());

    // Process each 512-bit block.
    for block in msg.as_chunks::<64>().0 {
        let mut w = [0u32; 80];
        for (i, chunk) in block.as_chunks::<4>().0.iter().enumerate() {
            w[i] = u32::from_be_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }

        let [mut a, mut b, mut c, mut d, mut e] = h;

        for (i, &w_i) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | (!b & d), K[0]),
                20..=39 => (b ^ c ^ d, K[1]),
                40..=59 => ((b & c) | (b & d) | (c & d), K[2]),
                _ => (b ^ c ^ d, K[3]),
            };
            let temp = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(w_i);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = temp;
        }

        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
    }

    let mut out = [0u8; 20];
    for (i, &word) in h.iter().enumerate() {
        out[i * 4..(i + 1) * 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}

/// Standard base64 encoding (RFC 4648).
fn base64_encode(data: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[(triple >> 18) as usize] as char);
        out.push(TABLE[((triple >> 12) & 0x3f) as usize] as char);
        if chunk.len() > 1 {
            out.push(TABLE[((triple >> 6) & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(TABLE[(triple & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

// ── WebSocket client helpers ──────────────────────────────────────────────────

/// Send a WebSocket upgrade request to `addr/path` and return the raw response
/// headers (terminated by `\r\n\r\n`).
fn ws_connect(addr: &str, path: &str, key: &str) -> (std::net::TcpStream, String) {
    let mut stream = std::net::TcpStream::connect(addr).expect("TCP connect to conduit");
    let request = format!(
        "GET {path} HTTP/1.1\r\n\
         Host: {addr}\r\n\
         Connection: Upgrade\r\n\
         Upgrade: websocket\r\n\
         Sec-WebSocket-Version: 13\r\n\
         Sec-WebSocket-Key: {key}\r\n\
         \r\n"
    );
    stream
        .write_all(request.as_bytes())
        .expect("write upgrade request");
    stream.set_read_timeout(Some(Duration::from_secs(10))).ok();

    // Read until we find the end of headers.
    let mut buf = [0u8; 4096];
    let mut response = String::new();
    loop {
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => response.push_str(&String::from_utf8_lossy(&buf[..n])),
            Err(_) => break,
        }
        if response.contains("\r\n\r\n") {
            break;
        }
    }

    (stream, response)
}

/// Send a masked WebSocket text frame to `stream`.
fn ws_send_text(stream: &mut std::net::TcpStream, text: &str) {
    let payload = text.as_bytes();
    // Mask key (4 bytes).
    let mask: [u8; 4] = [0x37, 0xfa, 0x21, 0x3d];
    let mut frame = vec![
        0x81,                       // FIN + text opcode
        0x80 | payload.len() as u8, // MASK bit + length
        mask[0],
        mask[1],
        mask[2],
        mask[3],
    ];
    for (i, &b) in payload.iter().enumerate() {
        frame.push(b ^ mask[i % 4]);
    }
    stream.write_all(&frame).expect("write WS frame");
}

/// Read a single (unmasked server) WebSocket text frame from `stream`.
fn ws_recv_text(stream: &mut std::net::TcpStream) -> String {
    let mut header = [0u8; 2];
    read_exact_timeout(stream, &mut header).expect("read WS frame header");
    let payload_len = (header[1] & 0x7f) as usize;
    let mut payload = vec![0u8; payload_len];
    if payload_len > 0 {
        read_exact_timeout(stream, &mut payload).expect("read WS payload");
    }
    String::from_utf8_lossy(&payload).into_owned()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

/// The proxy forwards a WebSocket upgrade request and returns 101.
#[test]
#[serial]
fn websocket_upgrade_returns_101() {
    let upstream_addr = spawn_ws_upstream();
    let port = common::free_port();
    let admin_port = common::free_port();

    let _srv = common::TestServer::start_with_config(
        port,
        admin_port,
        serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [{
                "port": port,
                "proxy": { "/": { "targets": [format!("http://{upstream_addr}")], "websocket": true } }
            }]
        }),
    );

    let (_, response) = ws_connect(
        &format!("127.0.0.1:{port}"),
        "/",
        "dGhlIHNhbXBsZSBub25jZQ==", // RFC 6455 test key
    );

    assert!(
        response.contains("101"),
        "proxy must return 101 Switching Protocols; got: {response}"
    );
    assert!(
        response.to_ascii_lowercase().contains("upgrade: websocket"),
        "101 response must include 'Upgrade: websocket'; got: {response}"
    );
    assert!(
        response
            .to_ascii_lowercase()
            .contains("connection: upgrade"),
        "101 response must include 'Connection: Upgrade'; got: {response}"
    );
}

/// The proxy correctly computes Sec-WebSocket-Accept, allowing a proper
/// WebSocket handshake to complete (RFC 6455 §1.3 known test vector).
#[test]
#[serial]
fn websocket_accept_header_is_correct() {
    let upstream_addr = spawn_ws_upstream();
    let port = common::free_port();
    let admin_port = common::free_port();

    let _srv = common::TestServer::start_with_config(
        port,
        admin_port,
        serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [{
                "port": port,
                "proxy": { "/": { "targets": [format!("http://{upstream_addr}")], "websocket": true } }
            }]
        }),
    );

    // RFC 6455 §1.3 known test vector:
    //   key    = "dGhlIHNhbXBsZSBub25jZQ=="
    //   accept = "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
    let (_, response) = ws_connect(
        &format!("127.0.0.1:{port}"),
        "/",
        "dGhlIHNhbXBsZSBub25jZQ==",
    );

    assert!(
        response.contains("s3pPLMBiTxaQ9kYGzzhZRbK+xOo="),
        "Sec-WebSocket-Accept must match RFC 6455 test vector; got: {response}"
    );
}

/// After upgrade, the proxy tunnels WebSocket data bidirectionally.
/// Sends a text frame through Conduit → upstream → back, verifying echo.
#[test]
#[serial]
fn websocket_data_is_tunneled_after_upgrade() {
    let upstream_addr = spawn_ws_upstream();
    let port = common::free_port();
    let admin_port = common::free_port();

    let _srv = common::TestServer::start_with_config(
        port,
        admin_port,
        serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [{
                "port": port,
                "proxy": { "/": { "targets": [format!("http://{upstream_addr}")], "websocket": true } }
            }]
        }),
    );

    let (mut stream, response) = ws_connect(
        &format!("127.0.0.1:{port}"),
        "/",
        "dGhlIHNhbXBsZSBub25jZQ==",
    );
    assert!(
        response.contains("101"),
        "upgrade must succeed; got: {response}"
    );

    // Send a text message and expect the same payload echoed back.
    ws_send_text(&mut stream, "hello websocket");
    let echo = ws_recv_text(&mut stream);
    assert_eq!(
        echo, "hello websocket",
        "proxy must tunnel WebSocket frames bidirectionally"
    );
}

// ── SHA-1 self-test (RFC 6455 known vector) ───────────────────────────────────

#[test]
fn sha1_rfc6455_test_vector() {
    // SHA-1("dGhlIHNhbXBsZSBub25jZQ==258EAFA5-E914-47DA-95CA-C5AB0DC85B11")
    // == b3 7a 4f 2c c0 62 4f 16 90 f6 46 06 cf 38 59 45 b2 be c4 ea
    let input = b"dGhlIHNhbXBsZSBub25jZQ==258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
    let expected = [
        0xb3, 0x7a, 0x4f, 0x2c, 0xc0, 0x62, 0x4f, 0x16, 0x90, 0xf6, 0x46, 0x06, 0xcf, 0x38, 0x59,
        0x45, 0xb2, 0xbe, 0xc4, 0xea,
    ];
    assert_eq!(
        sha1(input),
        expected,
        "SHA-1 must match RFC 6455 test vector"
    );
    let b64 = base64_encode(&expected);
    assert_eq!(b64, "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
}
