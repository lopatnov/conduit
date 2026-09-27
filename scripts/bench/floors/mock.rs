// Minimal keep-alive HTTP/1.1 echo upstream, a stand-in for the Go server in
// .github/workflows/ci.yml's performance job:
//   body = {"status":"ok","ts":0}, Content-Type: application/json, port 4000.
// Every request gets the same fixed response; pipelined requests are handled.
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const BODY: &[u8] = br#"{"status":"ok","ts":0}"#;

fn find_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n").map(|p| p + 4)
}

#[tokio::main]
async fn main() {
    let listener = TcpListener::bind("127.0.0.1:4000").await.unwrap();
    let resp = {
        let mut r = Vec::new();
        r.extend_from_slice(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: ");
        r.extend_from_slice(BODY.len().to_string().as_bytes());
        r.extend_from_slice(b"\r\n\r\n");
        r.extend_from_slice(BODY);
        r
    };
    loop {
        let (mut sock, _) = match listener.accept().await {
            Ok(x) => x,
            Err(_) => continue,
        };
        let _ = sock.set_nodelay(true);
        let resp = resp.clone();
        tokio::spawn(async move {
            let mut buf = vec![0u8; 16 * 1024];
            let mut filled = 0usize;
            loop {
                let n = match sock.read(&mut buf[filled..]).await {
                    Ok(0) | Err(_) => return,
                    Ok(n) => n,
                };
                filled += n;
                let mut out = Vec::new();
                while let Some(end) = find_end(&buf[..filled]) {
                    out.extend_from_slice(&resp);
                    buf.copy_within(end..filled, 0);
                    filled -= end;
                }
                if !out.is_empty() && sock.write_all(&out).await.is_err() {
                    return;
                }
                if filled == buf.len() {
                    return; // request larger than the buffer: not expected here
                }
            }
        });
    }
}
