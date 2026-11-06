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

/// The Admin API address: the flag, else `CONDUIT_ADMIN`, else `127.0.0.1:2019`.
pub fn resolve_admin(flag: Option<&str>) -> String {
    flag.map(ToOwned::to_owned)
        .or_else(|| std::env::var("CONDUIT_ADMIN").ok())
        .unwrap_or_else(|| "127.0.0.1:2019".to_owned())
}

/// GET `path` from the Admin API and print the body; on failure print the error and exit 1.
pub fn admin_get(path: &str, addr: &str) {
    match http_get(path, addr) {
        Ok(body) => println!("{body}"),
        Err(e) => {
            eprintln!("error: {e}");
            process::exit(1);
        }
    }
}

/// POST `path` (no body) to the Admin API and print the reply; on failure print the error and exit 1.
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

/// GET `path` from the Admin API at `addr` and return the body of a 2xx reply.
pub(crate) fn http_get(path: &str, addr: &str) -> anyhow::Result<String> {
    http_request("GET", path, addr, None)
}

/// POST `path` with no body to the Admin API at `addr` and return the body of a 2xx reply.
fn http_post(path: &str, addr: &str) -> anyhow::Result<String> {
    http_request("POST", path, addr, None)
}

/// POST `json_body` to `path` on the Admin API at `addr` and return the body of a 2xx reply.
fn http_post_json(path: &str, addr: &str, json_body: &str) -> anyhow::Result<String> {
    http_request("POST", path, addr, Some(json_body))
}

/// The environment variable the CLI reads the Admin API bearer token from (`global.admin.token` on the server).
const TOKEN_ENV: &str = "CONDUIT_ADMIN_TOKEN";

/// Send a blocking HTTP/1.1 request to the Admin API over a plain TCP
/// stream and return the response body. `json_body` of `None` sends no
/// body (`Content-Length: 0` for `POST`, no body headers at all for `GET`,
/// matching prior per-method behavior); `Some` sends it as
/// `application/json`. The token of `CONDUIT_ADMIN_TOKEN` is sent when set. The request is HTTP/1.0 with
/// `Connection: close`, so the reply is neither chunked nor preceded by interim `1xx` responses and the body is
/// everything after the headers.
fn http_request(
    method: &str,
    path: &str,
    addr: &str,
    json_body: Option<&str>,
) -> anyhow::Result<String> {
    let token = token_from_env(std::env::var(TOKEN_ENV))?;
    http_request_with(method, path, addr, json_body, token.as_deref())
}

/// The token from the environment: `None` when the variable is unset or empty, an error when it is not valid
/// Unicode (leaving the header out would turn a wrong setup into a bare 401 with no hint of the cause).
fn token_from_env(value: Result<String, std::env::VarError>) -> anyhow::Result<Option<String>> {
    match value {
        Ok(token) if token.is_empty() => Ok(None),
        Ok(token) => Ok(Some(token)),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(std::env::VarError::NotUnicode(_)) => anyhow::bail!("{TOKEN_ENV} is not valid Unicode"),
    }
}

/// A token is written into a header line: a carriage return or line feed in it would let it add headers.
fn check_token(token: &str) -> anyhow::Result<()> {
    if token.contains(['\r', '\n']) {
        anyhow::bail!("{TOKEN_ENV} must not contain a line break");
    }
    Ok(())
}

/// The warning for a token that is about to travel in clear text to an address that is not loopback.
fn token_warning(ip: std::net::IpAddr, addr: &str) -> Option<String> {
    (!ip.is_loopback()).then(|| {
        format!("warning: sending {TOKEN_ENV} over plain HTTP to {addr}, which is not a loopback address")
    })
}

/// The text of a server reply made safe to print in a terminal: control characters other than a line
/// break or a tab (escape sequences among them) become `?`.
fn printable(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c.is_control() && c != '\n' && c != '\t' {
                '?'
            } else {
                c
            }
        })
        .collect()
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
    if let Some(warning) = token.and_then(|_| token_warning(sock.ip(), addr)) {
        eprintln!("{warning}");
    }
    let mut stream = TcpStream::connect_timeout(&sock, Duration::from_secs(5))?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;

    let mut request = format!("{method} /{path} HTTP/1.0\r\nHost: {addr}\r\n");
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
        Some(code) => anyhow::bail!("Admin API answered HTTP {code}: {}", printable(body.trim())),
        None => anyhow::bail!("Admin API sent a reply that is not HTTP"),
    }
}

/// Everything after the blank line that ends the headers, or the whole text when there is none.
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

    /// A 2xx reply returns its body.
    #[test]
    fn a_2xx_reply_returns_its_body() {
        let (addr, server) = serve_once(OK);
        assert_eq!(
            http_request_with("POST", "reload", &addr, None, None).unwrap(),
            "{}"
        );
        server.join().unwrap();
    }

    /// A rejected reload is an error that carries the message.
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

    /// A 401 is an error.
    #[test]
    fn a_401_is_an_error() {
        let (addr, server) = serve_once(UNAUTHORIZED);
        let err = http_request_with("GET", "status", &addr, None, None)
            .unwrap_err()
            .to_string();
        server.join().unwrap();
        assert!(err.contains("HTTP 401"), "{err}");
    }

    /// The token is sent as a bearer header and only when given.
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

    /// A token with a line break is refused before anything is sent.
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

    /// A redirect is an error not a success.
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

    /// A reply that is not http is an error.
    #[test]
    fn a_reply_that_is_not_http_is_an_error() {
        let (addr, server) = serve_once("garbage without a status line");
        let err = http_request_with("GET", "status", &addr, None, None)
            .unwrap_err()
            .to_string();
        server.join().unwrap();
        assert!(err.contains("not HTTP"), "{err}");
    }

    /// The token warning appears only for an address that is not loopback.
    #[test]
    fn the_token_warning_appears_only_for_an_address_that_is_not_loopback() {
        use std::net::IpAddr;
        assert!(token_warning("127.0.0.1".parse::<IpAddr>().unwrap(), "127.0.0.1:2019").is_none());
        assert!(token_warning("::1".parse::<IpAddr>().unwrap(), "[::1]:2019").is_none());
        let w = token_warning("10.0.0.5".parse::<IpAddr>().unwrap(), "10.0.0.5:2019").unwrap();
        assert!(
            w.contains("10.0.0.5:2019") && w.contains("not a loopback"),
            "{w}"
        );
        assert!(!w.contains("s3cret"));
    }

    /// The error text of a server carries no control characters.
    #[test]
    fn the_error_text_of_a_server_carries_no_control_characters() {
        assert_eq!(printable("ok\tline\nnext"), "ok\tline\nnext");
        assert_eq!(printable("\u{1b}[31mred\u{1b}[0m\u{7}"), "?[31mred?[0m?");
        let (addr, server) = serve_once("HTTP/1.1 500 Internal Server Error\r\n\r\n\u{1b}[2Jboom");
        let err = http_request_with("GET", "status", &addr, None, None)
            .unwrap_err()
            .to_string();
        server.join().unwrap();
        assert!(
            !err.contains('\u{1b}') && err.contains("?[2Jboom"),
            "{err:?}"
        );
    }

    /// The request is http 1 0 so the reply cannot be chunked.
    #[test]
    fn the_request_is_http_1_0_so_the_reply_cannot_be_chunked() {
        let (addr, server) = serve_once(OK);
        http_request_with("POST", "reload", &addr, None, None).unwrap();
        let sent = server.join().unwrap();
        assert!(sent.starts_with("POST /reload HTTP/1.0\r\n"), "{sent}");
        assert!(sent.contains("Connection: close\r\n"), "{sent}");
    }

    /// The token from the environment is none when unset or empty and an error when not unicode.
    #[test]
    fn the_token_from_the_environment_is_none_when_unset_or_empty_and_an_error_when_not_unicode() {
        use std::env::VarError;
        assert_eq!(token_from_env(Err(VarError::NotPresent)).unwrap(), None);
        assert_eq!(token_from_env(Ok(String::new())).unwrap(), None);
        assert_eq!(
            token_from_env(Ok("s3cret".into())).unwrap().as_deref(),
            Some("s3cret")
        );
        let err =
            token_from_env(Err(VarError::NotUnicode(std::ffi::OsString::from("x")))).unwrap_err();
        assert!(err.to_string().contains("not valid Unicode"), "{err}");
    }

    /// Status code reads the first line only.
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
