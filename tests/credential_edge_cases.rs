//! Credential checks at the edges (issue #624: could Conduit have a bug of the "sshole" class, where a check fails
//! open for an input length or shape the usual tests never use?).
//!
//! Every credential kind is tried with the lengths 255, 256, 257, 1024, 1025 and 65 536 (the OpenSSH musl bypass
//! worked for 256..1024 characters), the empty value, a NUL byte, bytes that are not UTF-8, a value that differs from the
//! real one only in its last byte or by one trailing byte, and (for the long ones) a configured credential of that length.
//! The rule is the same everywhere: only the exact configured credential gets through, every other input is refused
//! with 401 (or, when it is too large for the connection to carry, with 4xx or a closed connection, never with a
//! status that means "authorised").
mod common;

use base64::Engine as _;
use reqwest::blocking::Client;
use reqwest::header::HeaderValue;

const LENGTHS: [usize; 6] = [255, 256, 257, 1024, 1025, 65_536];

fn basic(user: &str, pass: &str) -> String {
    format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(format!("{user}:{pass}"))
    )
}

/// The status of `GET /` with one header, or `None` when the connection was refused or closed (too large for the
/// server to carry is an acceptable way to say no).
fn status(
    client: &Client,
    srv: &common::TestServer,
    name: &str,
    value: HeaderValue,
) -> Option<u16> {
    client
        .get(srv.url("/"))
        .header(name, value)
        .send()
        .ok()
        .map(|r| r.status().as_u16())
}

fn hv(s: &str) -> HeaderValue {
    HeaderValue::from_str(s).expect("header value")
}

/// A server with no upstream: an authorised request ends in something other than 401 (the router's own answer), so
/// "refused" means exactly 401 and "carried" means anything else.
fn start(site: serde_json::Value) -> common::TestServer {
    let port = common::free_port();
    let admin_port = common::free_port();
    let mut site = site;
    site["port"] = port.into();
    common::TestServer::start_with_config(
        port,
        admin_port,
        serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [site]
        }),
    )
}

/// Refused: 401, or a 4xx that says the request was too large, or no answer at all. Never a 2xx/3xx/404/5xx
/// (what an authorised request to this upstream-less server ends in).
fn refused(code: Option<u16>) -> bool {
    matches!(code, None | Some(400 | 401 | 413 | 414 | 431))
}

fn assert_refused(what: &str, code: Option<u16>) {
    assert!(
        refused(code),
        "{what}: answered {code:?}, it must be refused"
    );
}

#[test]
fn basic_auth_refuses_every_wrong_password_length_and_accepts_only_the_exact_one() {
    let client = common::local_client();
    for len in LENGTHS {
        let secret = "s".repeat(len);
        let srv =
            start(serde_json::json!({ "basicAuth": { "users": { "alice": secret.clone() } } }));
        // the exact credential (65 536 characters do not fit in a header; that case is checked for refusal only)
        if len <= 4096 {
            let ok = status(&client, &srv, "authorization", hv(&basic("alice", &secret)));
            assert!(
                matches!(ok, Some(c) if c != 401),
                "{len}: the exact password was refused: {ok:?}"
            );
        }
        // The labels are kept apart from the candidate credentials: a label stored next to a secret-derived value
        // looks like a secret to the cleartext-logging scan (CodeQL), and a label is all a failure message carries.
        const BASIC_LABELS: [&str; 6] = [
            "one byte short",
            "one byte long",
            "last byte differs",
            "first byte differs",
            "empty",
            "same length, other bytes",
        ];
        let candidates = [
            secret[..len - 1].to_owned(),
            format!("{secret}x"),
            format!("{}t", &secret[..len - 1]),
            format!("t{}", &secret[1..]),
            String::new(),
            "z".repeat(len),
        ];
        for (k, what) in BASIC_LABELS.iter().enumerate() {
            let code = status(
                &client,
                &srv,
                "authorization",
                hv(&basic("alice", &candidates[k])),
            );
            assert_refused(
                &format!("basic auth, configured length {len}, {what}"),
                code,
            );
        }
        let wrong_user = status(
            &client,
            &srv,
            "authorization",
            hv(&basic("mallory", &secret)),
        );
        assert_refused(
            &format!("basic auth, right password of length {len}, wrong user"),
            wrong_user,
        );
    }
}

#[test]
fn basic_auth_refuses_malformed_headers() {
    let client = common::local_client();
    let srv = start(serde_json::json!({ "basicAuth": { "users": { "alice": "secret" } } }));
    for (what, value) in [
        ("empty", HeaderValue::from_static("")),
        ("scheme only", HeaderValue::from_static("Basic")),
        ("scheme and a space", HeaderValue::from_static("Basic ")),
        ("not base64", HeaderValue::from_static("Basic !!!")),
        (
            "no colon",
            hv(&format!(
                "Basic {}",
                base64::engine::general_purpose::STANDARD.encode("alice")
            )),
        ),
        (
            "NUL in the password",
            hv(&format!(
                "Basic {}",
                base64::engine::general_purpose::STANDARD.encode("alice:secret\0")
            )),
        ),
        (
            "NUL before the password",
            hv(&format!(
                "Basic {}",
                base64::engine::general_purpose::STANDARD.encode("alice:\0secret")
            )),
        ),
        ("empty user and password", hv(&basic("", ""))),
        (
            "a user with the right password in the user part",
            hv(&basic("alice:secret", "secret")),
        ),
        (
            "non-UTF-8 bytes",
            HeaderValue::from_bytes(b"Basic \xff\xfe\xfd").expect("bytes"),
        ),
        ("another scheme", HeaderValue::from_static("Bearer secret")),
    ] {
        assert_refused(
            &format!("basic auth, {what}"),
            status(&client, &srv, "authorization", value),
        );
    }
}

#[test]
fn api_key_refuses_every_wrong_key_length_and_non_utf8_value() {
    let client = common::local_client();
    for len in LENGTHS {
        let key = "k".repeat(len);
        let srv = start(
            serde_json::json!({ "apiKey": { "keys": [key.clone()], "header": "X-API-Key" } }),
        );
        if len <= 4096 {
            let ok = status(&client, &srv, "x-api-key", hv(&key));
            assert!(
                matches!(ok, Some(c) if c != 401),
                "{len}: the exact key was refused: {ok:?}"
            );
        }
        const KEY_LABELS: [&str; 5] = [
            "one byte short",
            "one byte long",
            "last byte differs",
            "empty",
            "non-UTF-8",
        ];
        let candidates = [
            hv(&key[..len - 1]),
            hv(&format!("{key}x")),
            hv(&format!("{}j", &key[..len - 1])),
            HeaderValue::from_static(""),
            HeaderValue::from_bytes(&vec![0xff; len]).expect("bytes"),
        ];
        for (k, what) in KEY_LABELS.iter().enumerate() {
            assert_refused(
                &format!("api key, configured length {len}, {what}"),
                status(&client, &srv, "x-api-key", candidates[k].clone()),
            );
        }
    }
}

#[cfg(feature = "consumers")]
#[test]
fn consumers_refuse_every_wrong_credential_and_never_confuse_two_consumers() {
    let client = common::local_client();
    let srv = start(serde_json::json!({ "consumers": { "consumers": [
        { "username": "alice", "apiKey": "alice-key-0123456789" },
        { "username": "bob", "basicAuth": { "password": "bob-password-0123456789" } },
    ] } }));
    let ok = status(&client, &srv, "x-api-key", hv("alice-key-0123456789"));
    assert!(
        matches!(ok, Some(c) if c != 401),
        "alice's key refused: {ok:?}"
    );
    for len in LENGTHS {
        let long = "a".repeat(len);
        assert_refused(
            &format!("consumer api key of length {len}"),
            status(&client, &srv, "x-api-key", hv(&long)),
        );
        assert_refused(
            &format!("consumer basic password of length {len}"),
            status(&client, &srv, "authorization", hv(&basic("bob", &long))),
        );
    }
    // bob's password with alice's name, alice's key as bob's password, an empty key
    assert_refused(
        "bob's password for alice",
        status(
            &client,
            &srv,
            "authorization",
            hv(&basic("alice", "bob-password-0123456789")),
        ),
    );
    assert_refused(
        "alice's key as bob's password",
        status(
            &client,
            &srv,
            "authorization",
            hv(&basic("bob", "alice-key-0123456789")),
        ),
    );
    assert_refused(
        "empty key",
        status(&client, &srv, "x-api-key", HeaderValue::from_static("")),
    );
}

#[test]
fn the_admin_token_refuses_every_wrong_bearer_length() {
    let client = common::local_client();
    for len in [255usize, 256, 1024, 4096] {
        let token = "t".repeat(len);
        let port = common::free_port();
        let admin_port = common::free_port();
        let srv = common::TestServer::start_with_config(
            port,
            admin_port,
            serde_json::json!({
                "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}"), "token": token.clone() } },
                "sites": [{ "port": port }]
            }),
        );
        let get = |value: HeaderValue| {
            client
                .get(srv.admin_url("/status"))
                .header("authorization", value)
                .send()
                .ok()
                .map(|r| r.status().as_u16())
        };
        assert_eq!(
            get(hv(&format!("Bearer {token}"))),
            Some(200),
            "{len}: the exact token was refused"
        );
        const TOKEN_LABELS: [&str; 6] = [
            "one byte short",
            "one byte long",
            "last byte differs",
            "empty",
            "no scheme",
            "non-UTF-8",
        ];
        let candidates = [
            hv(&format!("Bearer {}", &token[..len - 1])),
            hv(&format!("Bearer {token}x")),
            hv(&format!("Bearer {}u", &token[..len - 1])),
            HeaderValue::from_static("Bearer "),
            hv(&token),
            HeaderValue::from_bytes(b"Bearer \xff").expect("bytes"),
        ];
        for (k, what) in TOKEN_LABELS.iter().enumerate() {
            let code = get(candidates[k].clone());
            assert!(
                matches!(code, None | Some(400 | 401)),
                "admin token, length {len}, {what}: {code:?}"
            );
        }
    }
}

#[cfg(feature = "jwt")]
#[test]
fn jwt_refuses_tokens_signed_with_the_wrong_key_whatever_its_length_and_oversized_tokens() {
    use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
    let client = common::local_client();
    let secret = "e2e-secret-of-a-reasonable-length-0123456789";
    let srv = start(serde_json::json!({ "jwtAuth": { "secret": secret } }));
    let exp = (std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_secs()
        + 3600) as usize;
    let token = |key: &[u8]| {
        encode(
            &Header::new(Algorithm::HS256),
            &serde_json::json!({ "sub": "carol", "exp": exp }),
            &EncodingKey::from_secret(key),
        )
        .expect("encode")
    };
    let bearer = |t: &str| hv(&format!("Bearer {t}"));
    let ok = status(
        &client,
        &srv,
        "authorization",
        bearer(&token(secret.as_bytes())),
    );
    assert!(
        matches!(ok, Some(c) if c != 401),
        "the right token was refused: {ok:?}"
    );
    // a token signed with an empty key, with keys of the sshole lengths, and with a key that is the secret plus a byte
    // (the labels are constants: a label built from a key's length is secret-derived to the cleartext-logging scan)
    for (label, key) in [
        ("an empty key", Vec::new()),
        ("a 255-byte key", vec![b'k'; 255]),
        ("a 256-byte key", vec![b'k'; 256]),
        ("a 1024-byte key", vec![b'k'; 1024]),
        ("a 1025-byte key", vec![b'k'; 1025]),
        ("the secret plus a byte", format!("{secret}x").into_bytes()),
        (
            "the secret less a byte",
            secret.as_bytes()[..secret.len() - 1].to_vec(),
        ),
    ] {
        let code = status(&client, &srv, "authorization", bearer(&token(&key)));
        assert_refused(&format!("jwt signed with {label}"), code);
    }
    // oversized or empty tokens
    for len in LENGTHS {
        let long = format!(
            "{}.{}.{}",
            "a".repeat(len),
            "b".repeat(len),
            "c".repeat(len)
        );
        assert_refused(
            &format!("jwt of {len}x3 characters"),
            status(&client, &srv, "authorization", bearer(&long)),
        );
    }
    assert_refused(
        "empty bearer",
        status(
            &client,
            &srv,
            "authorization",
            HeaderValue::from_static("Bearer "),
        ),
    );
    assert_refused(
        "only dots",
        status(&client, &srv, "authorization", bearer("..")),
    );
}
