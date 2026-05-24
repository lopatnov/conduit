mod common;

use reqwest::blocking::Client;
use reqwest::redirect::Policy;

/// HTTP client that does NOT follow redirects — so we can inspect the 3xx.
fn no_follow() -> Client {
    Client::builder()
        .redirect(Policy::none())
        .build()
        .expect("client")
}

fn server_with_redirects() -> common::TestServer {
    let port = common::free_port();
    let admin_port = common::free_port();
    common::TestServer::start_with_config(
        port,
        admin_port,
        serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [{
                "port": port,
                "redirects": [
                    { "from": "/old",        "to": "/new",             "status": 301 },
                    { "from": "/temp",       "to": "/destination",     "status": 302 },
                    { "from": "/keep",       "to": "/kept",            "status": 307 },
                    { "from": "/perm",       "to": "/permanent",       "status": 308 },
                    { "from": "/blog/:slug", "to": "/posts/:slug",     "status": 301 },
                    { "from": "/u/:id/p/:n", "to": "/users/:id/posts/:n", "status": 301 },
                    { "from": "/ext",        "to": "https://example.com", "status": 302 }
                ],
                "healthCheck": true
            }]
        }),
    )
}

#[test]
fn redirect_301_exact() {
    let srv = server_with_redirects();
    let resp = no_follow().get(srv.url("/old")).send().expect("GET /old");
    assert_eq!(resp.status().as_u16(), 301);
    assert_eq!(resp.headers().get("location").unwrap(), "/new");
}

#[test]
fn redirect_302_exact() {
    let srv = server_with_redirects();
    let resp = no_follow().get(srv.url("/temp")).send().expect("GET /temp");
    assert_eq!(resp.status().as_u16(), 302);
    assert_eq!(resp.headers().get("location").unwrap(), "/destination");
}

#[test]
fn redirect_307_exact() {
    let srv = server_with_redirects();
    let resp = no_follow().get(srv.url("/keep")).send().expect("GET /keep");
    assert_eq!(resp.status().as_u16(), 307);
    assert_eq!(resp.headers().get("location").unwrap(), "/kept");
}

#[test]
fn redirect_308_exact() {
    let srv = server_with_redirects();
    let resp = no_follow().get(srv.url("/perm")).send().expect("GET /perm");
    assert_eq!(resp.status().as_u16(), 308);
    assert_eq!(resp.headers().get("location").unwrap(), "/permanent");
}

#[test]
fn no_redirect_for_unmatched_path() {
    let srv = server_with_redirects();
    // /__health__ is not in the redirects list — should return 200.
    let resp = reqwest::blocking::get(srv.url("/__health__")).expect("GET /__health__");
    assert_eq!(resp.status().as_u16(), 200);
}

#[test]
fn redirect_param_single_segment() {
    let srv = server_with_redirects();
    let resp = no_follow()
        .get(srv.url("/blog/hello-world"))
        .send()
        .expect("GET /blog/hello-world");
    assert_eq!(resp.status().as_u16(), 301);
    assert_eq!(
        resp.headers().get("location").unwrap(),
        "/posts/hello-world"
    );
}

#[test]
fn redirect_param_multiple_segments() {
    let srv = server_with_redirects();
    let resp = no_follow()
        .get(srv.url("/u/42/p/7"))
        .send()
        .expect("GET /u/42/p/7");
    assert_eq!(resp.status().as_u16(), 301);
    assert_eq!(resp.headers().get("location").unwrap(), "/users/42/posts/7");
}

#[test]
fn redirect_preserves_query_string() {
    let srv = server_with_redirects();
    let resp = no_follow()
        .get(srv.url("/old?foo=bar&baz=1"))
        .send()
        .expect("GET /old?foo=bar");
    assert_eq!(resp.status().as_u16(), 301);
    assert_eq!(
        resp.headers().get("location").unwrap(),
        "/new?foo=bar&baz=1"
    );
}

#[test]
fn redirect_param_with_query_string() {
    let srv = server_with_redirects();
    let resp = no_follow()
        .get(srv.url("/blog/my-post?ref=twitter"))
        .send()
        .expect("GET /blog/my-post?ref=twitter");
    assert_eq!(resp.status().as_u16(), 301);
    assert_eq!(
        resp.headers().get("location").unwrap(),
        "/posts/my-post?ref=twitter"
    );
}

#[test]
fn redirect_to_external_url() {
    let srv = server_with_redirects();
    let resp = no_follow().get(srv.url("/ext")).send().expect("GET /ext");
    assert_eq!(resp.status().as_u16(), 302);
    assert_eq!(
        resp.headers().get("location").unwrap(),
        "https://example.com"
    );
}
