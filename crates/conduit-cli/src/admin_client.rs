use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::process;
use std::time::Duration;

// ── Upstream command helpers ───────────────────────────────────────────────

/// Build the JSON body for `/upstreams/add`, `/remove`, and `/weight`.
pub fn upstream_json(route: &str, target: &str, weight: Option<u32>, site: Option<&str>) -> String {
    let mut obj = serde_json::json!({ "route": route, "target": target });
    if let Some(w) = weight {
        obj["weight"] = serde_json::json!(w);
    }
    if let Some(s) = site {
        obj["site"] = serde_json::json!(s);
    }
    obj.to_string()
}

// ── Admin API helpers ──────────────────────────────────────────────────────

pub fn resolve_admin(flag: Option<&str>) -> String {
    flag.map(ToOwned::to_owned)
        .or_else(|| std::env::var("CONDUIT_ADMIN").ok())
        .unwrap_or_else(|| "127.0.0.1:2019".to_owned())
}

pub fn admin_get(path: &str, addr: &str) {
    match http_get(path, addr) {
        Ok(body) => println!("{body}"),
        Err(e) => {
            eprintln!("error: {e}");
            process::exit(1);
        }
    }
}

pub fn admin_post(path: &str, addr: &str) {
    match http_post(path, addr) {
        Ok(body) => println!("{body}"),
        Err(e) => {
            eprintln!("error: {e}");
            process::exit(1);
        }
    }
}

/// POST to `path` on the admin API with a JSON body.
pub fn admin_post_json(path: &str, addr: &str, json_body: &str) {
    match http_post_json(path, addr, json_body) {
        Ok(body) => println!("{body}"),
        Err(e) => {
            eprintln!("error: {e}");
            process::exit(1);
        }
    }
}

pub(crate) fn http_get(path: &str, addr: &str) -> anyhow::Result<String> {
    http_request("GET", path, addr, None)
}

fn http_post(path: &str, addr: &str) -> anyhow::Result<String> {
    http_request("POST", path, addr, None)
}

fn http_post_json(path: &str, addr: &str, json_body: &str) -> anyhow::Result<String> {
    http_request("POST", path, addr, Some(json_body))
}

/// Send a blocking HTTP/1.1 request to the Admin API over a plain TCP
/// stream and return the response body. `json_body` of `None` sends no
/// body (`Content-Length: 0` for `POST`, no body headers at all for `GET`,
/// matching prior per-method behavior); `Some` sends it as
/// `application/json`.
/// The environment variable the CLI reads the Admin API bearer token from (`global.admin.token` on the server).
const TOKEN_ENV: &str = "CONDUIT_ADMIN_TOKEN";

fn http_request(
    method: &str,
    path: &str,
    addr: &str,
    json_body: Option<&str>,
) -> anyhow::Result<String> {
    let token = std::env::var(TOKEN_ENV).ok().filter(|t| !t.is_empty());
    http_request_with(method, path, addr, json_body, token.as_deref())
}

/// A token is written into a header line: a carriage return or line feed in it would let it add headers.
fn check_token(token: &str) -> anyhow::Result<()> {
    if token.contains(['\r', '\n']) {
        anyhow::bail!("{TOKEN_ENV} must not contain a line break");
    }
    Ok(())
}

/// The status code of an HTTP/1.x response, from its first line.
fn status_code(response: &str) -> Option<u16> {
    let line = response.lines().next()?;
    let mut parts = line.split_whitespace();
    parts.next()?.starts_with("HTTP/").then_some(())?;
    parts.next()?.parse().ok()
}

/// One request to the Admin API. Sends `Authorization: Bearer <token>` when a token is given, and turns a reply
/// that is not 2xx (a rejected reload, a wrong token, an unknown route) into an error, so that the command exits
/// with status 1 instead of printing the error body and reporting success.
fn http_request_with(
    method: &str,
    path: &str,
    addr: &str,
    json_body: Option<&str>,
    token: Option<&str>,
) -> anyhow::Result<String> {
    if let Some(t) = token {
        check_token(t)?;
    }
    let sock = addr
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| anyhow::anyhow!("cannot resolve {addr}"))?;
    let mut stream = TcpStream::connect_timeout(&sock, Duration::from_secs(5))?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;

    let mut request = format!("{method} /{path} HTTP/1.1\r\nHost: {addr}\r\n");
    match json_body {
        Some(body) => {
            request.push_str("Content-Type: application/json\r\n");
            request.push_str(&format!("Content-Length: {}\r\n", body.len()));
        }
        None if method == "POST" => request.push_str("Content-Length: 0\r\n"),
        None => {}
    }
    if let Some(t) = token {
        request.push_str(&format!("Authorization: Bearer {t}\r\n"));
    }
    request.push_str("Connection: close\r\n\r\n");
    if let Some(body) = json_body {
        request.push_str(body);
    }

    stream.write_all(request.as_bytes())?;
    stream.flush()?;
    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    let body = extract_body(&response);
    match status_code(&response) {
        Some(code) if (200..300).contains(&code) => Ok(body),
        Some(code) => anyhow::bail!("Admin API answered HTTP {code}: {}", body.trim()),
        None => anyhow::bail!("Admin API sent a reply that is not HTTP"),
    }
}

fn extract_body(response: &str) -> String {
    response
        .find("\r\n\r\n")
        .map(|pos| response[pos + 4..].to_owned())
        .unwrap_or_else(|| response.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    /// A one-request server: reads the request, answers with `reply`, and hands the request text back.
    fn serve_once(reply: &'static str) -> (String, std::thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = vec![0u8; 8192];
            let n = stream.read(&mut buf).unwrap();
            stream.write_all(reply.as_bytes()).unwrap();
            String::from_utf8_lossy(&buf[..n]).into_owned()
        });
        (addr, handle)
    }

    const OK: &str = "HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}";
    const REJECTED: &str = "HTTP/1.1 400 Bad Request\r\n\r\n{\"status\":\"error\",\"message\":\"cold fields changed\"}";
    const UNAUTHORIZED: &str = "HTTP/1.1 401 Unauthorized\r\n\r\n{\"error\":\"unauthorized\"}";

    #[test]
    fn a_2xx_reply_returns_its_body() {
        let (addr, server) = serve_once(OK);
        assert_eq!(
            http_request_with("POST", "reload", &addr, None, None).unwrap(),
            "{}"
        );
        server.join().unwrap();
    }

    #[test]
    fn a_rejected_reload_is_an_error_that_carries_the_message() {
        let (addr, server) = serve_once(REJECTED);
        let err = http_request_with("POST", "reload", &addr, None, None)
            .unwrap_err()
            .to_string();
        server.join().unwrap();
        assert!(
            err.contains("HTTP 400") && err.contains("cold fields changed"),
            "{err}"
        );
    }

    #[test]
    fn a_401_is_an_error() {
        let (addr, server) = serve_once(UNAUTHORIZED);
        let err = http_request_with("GET", "status", &addr, None, None)
            .unwrap_err()
            .to_string();
        server.join().unwrap();
        assert!(err.contains("HTTP 401"), "{err}");
    }

    #[test]
    fn the_token_is_sent_as_a_bearer_header_and_only_when_given() {
        let (addr, server) = serve_once(OK);
        http_request_with("GET", "status", &addr, None, Some("s3cret")).unwrap();
        let sent = server.join().unwrap();
        assert!(sent.contains("Authorization: Bearer s3cret\r\n"), "{sent}");

        let (addr, server) = serve_once(OK);
        http_request_with("GET", "status", &addr, None, None).unwrap();
        let sent = server.join().unwrap();
        assert!(
            !sent.to_ascii_lowercase().contains("authorization"),
            "{sent}"
        );
    }

    #[test]
    fn a_token_with_a_line_break_is_refused_before_anything_is_sent() {
        for bad in ["a\r\nX-Evil: 1", "a\nb", "a\rb"] {
            // port 9 (discard) is never reached: the token is checked first
            let err = http_request_with("GET", "status", "127.0.0.1:9", None, Some(bad))
                .unwrap_err()
                .to_string();
            assert!(err.contains("line break"), "{bad:?}: {err}");
        }
    }

    #[test]
    fn a_redirect_is_an_error_not_a_success() {
        let (addr, server) = serve_once(
            "HTTP/1.1 302 Found
Location: /elsewhere

",
        );
        let err = http_request_with("GET", "status", &addr, None, None)
            .unwrap_err()
            .to_string();
        server.join().unwrap();
        assert!(err.contains("HTTP 302"), "{err}");
    }

    #[test]
    fn a_reply_that_is_not_http_is_an_error() {
        let (addr, server) = serve_once("garbage without a status line");
        let err = http_request_with("GET", "status", &addr, None, None)
            .unwrap_err()
            .to_string();
        server.join().unwrap();
        assert!(err.contains("not HTTP"), "{err}");
    }

    #[test]
    fn status_code_reads_the_first_line_only() {
        assert_eq!(status_code("HTTP/1.1 204 No Content\r\n\r\n"), Some(204));
        assert_eq!(
            status_code("HTTP/1.0 503 x\r\n\r\nHTTP/1.1 200 OK"),
            Some(503)
        );
        assert_eq!(status_code("200 OK"), None);
        assert_eq!(status_code(""), None);
    }
}
