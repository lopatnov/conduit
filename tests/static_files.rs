mod common;

use serial_test::serial;
use std::fs;

fn make_static_server(files: &[(&str, &str)]) -> (common::TestServer, tempfile::TempDir) {
    let static_dir = tempfile::tempdir().expect("tempdir");
    for (name, content) in files {
        fs::write(static_dir.path().join(name), content).expect("write file");
    }

    let port = common::free_port();
    let admin_port = common::free_port();
    let static_path = static_dir.path().to_str().expect("utf8 path").to_owned();

    let server = common::TestServer::start_with_config(
        port,
        admin_port,
        serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [{ "port": port, "static": static_path }]
        }),
    );
    (server, static_dir)
}

#[test]
#[serial]
fn static_200_serves_file() {
    let (server, _dir) = make_static_server(&[("hello.txt", "Hello, world!")]);
    let resp = reqwest::blocking::get(server.url("/hello.txt")).expect("GET");
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.text().unwrap(), "Hello, world!");
}

#[test]
#[serial]
fn static_content_type_html() {
    let (server, _dir) = make_static_server(&[("index.html", "<h1>hi</h1>")]);
    let resp = reqwest::blocking::get(server.url("/index.html")).expect("GET");
    assert_eq!(resp.status(), 200);
    let ct = resp
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap();
    assert!(ct.starts_with("text/html"), "unexpected content-type: {ct}");
}

#[test]
#[serial]
fn static_index_html_at_root() {
    let (server, _dir) = make_static_server(&[("index.html", "root index")]);
    let resp = reqwest::blocking::get(server.url("/")).expect("GET /");
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.text().unwrap(), "root index");
}

#[test]
#[serial]
fn static_404_for_missing_file() {
    let (server, _dir) = make_static_server(&[("exists.txt", "yes")]);
    let resp = reqwest::blocking::get(server.url("/nope.txt")).expect("GET");
    assert_eq!(resp.status(), 404);
}

#[test]
#[serial]
fn static_304_on_matching_etag() {
    let (server, _dir) = make_static_server(&[("data.txt", "content")]);

    // First request to get ETag
    let first = reqwest::blocking::get(server.url("/data.txt")).expect("GET");
    assert_eq!(first.status(), 200);
    let etag = first
        .headers()
        .get("etag")
        .expect("etag header")
        .to_str()
        .unwrap()
        .to_owned();

    // Second request with If-None-Match
    let client = reqwest::blocking::Client::new();
    let resp = client
        .get(server.url("/data.txt"))
        .header("if-none-match", &etag)
        .send()
        .expect("GET with If-None-Match");
    assert_eq!(resp.status(), 304);
}

#[test]
#[serial]
fn static_200_on_mismatched_etag() {
    let (server, _dir) = make_static_server(&[("data.txt", "content")]);
    let client = reqwest::blocking::Client::new();
    let resp = client
        .get(server.url("/data.txt"))
        .header("if-none-match", "\"wrong-etag\"")
        .send()
        .expect("GET");
    assert_eq!(resp.status(), 200);
}

#[test]
#[serial]
fn static_206_range_request() {
    let (server, _dir) = make_static_server(&[("range.txt", "0123456789")]);
    let client = reqwest::blocking::Client::new();
    let resp = client
        .get(server.url("/range.txt"))
        .header("range", "bytes=2-5")
        .send()
        .expect("GET with Range");
    assert_eq!(resp.status(), 206);
    let content_range = resp
        .headers()
        .get("content-range")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    assert_eq!(resp.text().unwrap(), "2345");
    assert_eq!(
        content_range, "bytes 2-5/10",
        "Content-Range header mismatch"
    );
}

#[test]
#[serial]
fn static_206_range_suffix() {
    let (server, _dir) = make_static_server(&[("range.txt", "0123456789")]);
    let client = reqwest::blocking::Client::new();
    let resp = client
        .get(server.url("/range.txt"))
        .header("range", "bytes=-3")
        .send()
        .expect("GET suffix range");
    assert_eq!(resp.status(), 206);
    assert_eq!(resp.text().unwrap(), "789");
}

/// Issue #402 (RFC 9110 §13.1.5): a current `If-Range` validator keeps the `206`, a stale one
/// turns the request into a plain `200` with the whole file.
#[test]
#[serial]
fn static_if_range_decides_between_206_and_200() {
    let (server, _dir) = make_static_server(&[("range.txt", "0123456789")]);
    let client = reqwest::blocking::Client::new();
    let etag = client
        .get(server.url("/range.txt"))
        .send()
        .expect("GET")
        .headers()
        .get("etag")
        .and_then(|v| v.to_str().ok())
        .expect("etag")
        .to_owned();

    let resp = client
        .get(server.url("/range.txt"))
        .header("range", "bytes=2-5")
        .header("if-range", &etag)
        .send()
        .expect("GET with current If-Range");
    assert_eq!(resp.status(), 206);
    assert_eq!(resp.text().unwrap(), "2345");

    let resp = client
        .get(server.url("/range.txt"))
        .header("range", "bytes=2-5")
        .header("if-range", "\"stale\"")
        .send()
        .expect("GET with stale If-Range");
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.text().unwrap(), "0123456789");
}

#[test]
#[serial]
fn static_416_invalid_range() {
    let (server, _dir) = make_static_server(&[("small.txt", "hi")]);
    let client = reqwest::blocking::Client::new();
    let resp = client
        .get(server.url("/small.txt"))
        .header("range", "bytes=100-200")
        .send()
        .expect("GET bad range");
    assert_eq!(resp.status(), 416);
}

#[test]
#[serial]
fn static_404_for_dotfile_default() {
    let (server, _dir) = make_static_server(&[(".hidden", "secret")]);
    let resp = reqwest::blocking::get(server.url("/.hidden")).expect("GET");
    assert_eq!(resp.status(), 404);
}

#[test]
#[serial]
fn static_head_returns_no_body() {
    let (server, _dir) = make_static_server(&[("file.txt", "body content")]);
    let client = reqwest::blocking::Client::new();
    let resp = client.head(server.url("/file.txt")).send().expect("HEAD");
    assert_eq!(resp.status(), 200);
    let body = resp.text().unwrap();
    assert!(body.is_empty(), "HEAD should return no body");
}

#[test]
#[serial]
fn static_etag_and_last_modified_present() {
    let (server, _dir) = make_static_server(&[("file.txt", "data")]);
    let resp = reqwest::blocking::get(server.url("/file.txt")).expect("GET");
    assert_eq!(resp.status(), 200);
    assert!(resp.headers().contains_key("etag"), "missing etag");
    assert!(
        resp.headers().contains_key("last-modified"),
        "missing last-modified"
    );
    assert!(
        resp.headers().contains_key("cache-control"),
        "missing cache-control"
    );
}

/// Issue #401: `staticOptions.etag` / `lastModified` really switch the headers off, and a request
/// that carries the (now meaningless) conditional headers is served in full.
fn validators_server(options: serde_json::Value) -> (common::TestServer, tempfile::TempDir) {
    let port = common::free_port();
    let admin_port = common::free_port();
    let dir = tempfile::tempdir().expect("tempdir");
    fs::write(dir.path().join("f.txt"), "data").unwrap();
    let static_path = dir.path().to_str().unwrap().to_owned();
    let server = common::TestServer::start_with_config(
        port,
        admin_port,
        serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [{ "port": port, "static": static_path, "staticOptions": options }]
        }),
    );
    (server, dir)
}

#[test]
#[serial]
fn static_etag_false_omits_etag_and_ignores_if_none_match() {
    let (server, _dir) = validators_server(serde_json::json!({ "etag": false }));
    let resp = reqwest::blocking::get(server.url("/f.txt")).expect("GET");
    assert_eq!(resp.status(), 200);
    assert!(!resp.headers().contains_key("etag"), "etag must be off");
    assert!(resp.headers().contains_key("last-modified"));

    let resp = reqwest::blocking::Client::new()
        .get(server.url("/f.txt"))
        .header("if-none-match", "*")
        .send()
        .expect("GET");
    assert_eq!(
        resp.status(),
        200,
        "If-None-Match is ignored without an ETag"
    );
}

#[test]
#[serial]
fn static_last_modified_false_omits_header_and_ignores_if_modified_since() {
    let (server, _dir) = validators_server(serde_json::json!({ "lastModified": false }));
    let resp = reqwest::blocking::get(server.url("/f.txt")).expect("GET");
    assert_eq!(resp.status(), 200);
    assert!(!resp.headers().contains_key("last-modified"));
    assert!(resp.headers().contains_key("etag"));

    let far_future = "Fri, 01 Jan 2100 00:00:00 GMT";
    let resp = reqwest::blocking::Client::new()
        .get(server.url("/f.txt"))
        .header("if-modified-since", far_future)
        .send()
        .expect("GET");
    assert_eq!(
        resp.status(),
        200,
        "If-Modified-Since is ignored without Last-Modified"
    );
}

// ── dotFiles policy tests ─────────────────────────────────────────────────

/// Helper: start a server with a specific dotFiles policy and a `.hidden` file.
fn dot_server(policy: &str) -> (common::TestServer, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::write(dir.path().join(".hidden"), "secret content").expect("write dotfile");
    fs::write(dir.path().join("normal.txt"), "public").expect("write normal");
    let port = common::free_port();
    let admin_port = common::free_port();
    let static_path = dir.path().to_str().expect("utf8").to_owned();
    let srv = common::TestServer::start_with_config(
        port,
        admin_port,
        serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [{
                "port": port,
                "static": static_path,
                "staticOptions": { "dotFiles": policy }
            }]
        }),
    );
    (srv, dir)
}

#[test]
#[serial]
fn dotfiles_allow_serves_hidden_file() {
    let (srv, _dir) = dot_server("allow");
    let resp = reqwest::blocking::get(srv.url("/.hidden")).expect("GET");
    assert_eq!(
        resp.status().as_u16(),
        200,
        "dotFiles:allow must serve the dotfile"
    );
    assert_eq!(resp.text().unwrap(), "secret content");
}

#[test]
#[serial]
fn dotfiles_deny_returns_403() {
    let (srv, _dir) = dot_server("deny");
    let resp = reqwest::blocking::get(srv.url("/.hidden")).expect("GET");
    assert_eq!(
        resp.status().as_u16(),
        403,
        "dotFiles:deny must return 403 Forbidden"
    );
}

#[test]
#[serial]
fn dotfiles_ignore_returns_404() {
    let (srv, _dir) = dot_server("ignore");
    let resp = reqwest::blocking::get(srv.url("/.hidden")).expect("GET");
    assert_eq!(
        resp.status().as_u16(),
        404,
        "dotFiles:ignore must treat dotfile as not found"
    );
}

#[test]
#[serial]
fn dotfiles_allow_does_not_affect_normal_files() {
    let (srv, _dir) = dot_server("allow");
    let resp = reqwest::blocking::get(srv.url("/normal.txt")).expect("GET");
    assert_eq!(resp.status().as_u16(), 200);
    assert_eq!(resp.text().unwrap(), "public");
}

/// dotFiles policy must check every path *segment*, not just the filename.
/// A path like /subdir/.hidden has a dotfile component one level deep.
#[test]
#[serial]
fn dotfiles_allow_serves_dotfile_in_subdirectory() {
    let dir = tempfile::tempdir().expect("tempdir");
    let sub = dir.path().join("subdir");
    fs::create_dir(&sub).expect("mkdir subdir");
    fs::write(sub.join(".env"), "SECRET=1").expect("write");
    let port = common::free_port();
    let admin_port = common::free_port();
    let static_path = dir.path().to_str().expect("utf8").to_owned();
    let srv = common::TestServer::start_with_config(
        port,
        admin_port,
        serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [{
                "port": port,
                "static": static_path,
                "staticOptions": { "dotFiles": "allow" }
            }]
        }),
    );
    let resp = reqwest::blocking::get(srv.url("/subdir/.env")).expect("GET");
    assert_eq!(
        resp.status().as_u16(),
        200,
        "dotFiles:allow must serve dotfiles inside subdirectories"
    );
    assert_eq!(resp.text().unwrap(), "SECRET=1");
}

#[test]
#[serial]
fn dotfiles_deny_blocks_dotfile_in_subdirectory() {
    let dir = tempfile::tempdir().expect("tempdir");
    let sub = dir.path().join("subdir");
    fs::create_dir(&sub).expect("mkdir subdir");
    fs::write(sub.join(".env"), "SECRET=1").expect("write");
    let port = common::free_port();
    let admin_port = common::free_port();
    let static_path = dir.path().to_str().expect("utf8").to_owned();
    let srv = common::TestServer::start_with_config(
        port,
        admin_port,
        serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [{
                "port": port,
                "static": static_path,
                "staticOptions": { "dotFiles": "deny" }
            }]
        }),
    );
    let resp = reqwest::blocking::get(srv.url("/subdir/.env")).expect("GET");
    assert_eq!(
        resp.status().as_u16(),
        403,
        "dotFiles:deny must block dotfiles inside subdirectories"
    );
}

/// A dot-prefixed *directory* component — e.g. /.dotdir/normal.txt — must
/// also be subject to the dotFiles policy, because every path segment is
/// checked by has_dotfile().
#[test]
#[serial]
fn dotfiles_deny_blocks_dot_directory_component() {
    let dir = tempfile::tempdir().expect("tempdir");
    let dotdir = dir.path().join(".secrets");
    fs::create_dir(&dotdir).expect("mkdir .secrets");
    fs::write(dotdir.join("config.txt"), "password=hunter2").expect("write");
    let port = common::free_port();
    let admin_port = common::free_port();
    let static_path = dir.path().to_str().expect("utf8").to_owned();
    let srv = common::TestServer::start_with_config(
        port,
        admin_port,
        serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [{
                "port": port,
                "static": static_path,
                "staticOptions": { "dotFiles": "deny" }
            }]
        }),
    );
    // /.secrets/config.txt — the directory segment starts with '.'
    let resp = reqwest::blocking::get(srv.url("/.secrets/config.txt")).expect("GET");
    assert_eq!(
        resp.status().as_u16(),
        403,
        "dotFiles:deny must block paths where a directory component starts with '.'"
    );
}

#[test]
#[serial]
fn static_cache_control_max_age() {
    let port = common::free_port();
    let admin_port = common::free_port();
    let dir = tempfile::tempdir().expect("tempdir");
    fs::write(dir.path().join("f.txt"), "x").unwrap();
    let static_path = dir.path().to_str().unwrap().to_owned();

    let server = common::TestServer::start_with_config(
        port,
        admin_port,
        serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [{
                "port": port,
                "static": static_path,
                "staticOptions": { "maxAge": "1h" }
            }]
        }),
    );

    let resp = reqwest::blocking::get(server.url("/f.txt")).expect("GET");
    assert_eq!(resp.status(), 200);
    let cc = resp
        .headers()
        .get("cache-control")
        .unwrap()
        .to_str()
        .unwrap();
    assert!(
        cc.contains("max-age=3600"),
        "expected max-age=3600, got: {cc}"
    );
}

/// A symlink to `target` at `link`; `None` when this machine cannot create one (a Windows account
/// without the privilege). CI can, so a missing privilege there is a failure, not a skip.
fn symlink_file(target: &std::path::Path, link: &std::path::Path) -> bool {
    #[cfg(unix)]
    let made = std::os::unix::fs::symlink(target, link);
    #[cfg(windows)]
    let made = std::os::windows::fs::symlink_file(target, link);
    match made {
        Ok(()) => true,
        Err(e) => {
            assert!(
                std::env::var_os("CI").is_none(),
                "CI must be able to create symlinks ({e})"
            );
            eprintln!("skipping: cannot create a symlink here ({e})");
            false
        }
    }
}

/// While a symlink to a longer file outside the root is swapped in and out at the served path, every
/// answer is either the complete inside file (`Content-Length` and body agree, and it is not the
/// outside file) or a plain `404`. This is an invariant check, not a proof of the open-before-headers
/// order (the window is too narrow to hit reliably; the unreadable-file test below is the
/// deterministic one for that).
#[test]
#[serial]
fn static_a_file_swapped_for_a_symlink_is_never_served_or_cut_off() {
    let inside = "inside-inside-inside";
    let outside_dir = tempfile::tempdir().expect("tempdir");
    let outside = outside_dir.path().join("secret.txt");
    fs::write(&outside, "OUTSIDE-OUTSIDE-OUTSIDE-OUTSIDE-OUTSIDE").unwrap();

    let (server, dir) = make_static_server(&[("f.txt", inside)]);
    let served = dir.path().join("f.txt");
    let parked = dir.path().join("parked.txt");
    if !symlink_file(&outside, &dir.path().join("probe.txt")) {
        return;
    }
    fs::remove_file(dir.path().join("probe.txt")).unwrap();

    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let swapper = {
        let (stop, served, parked, outside) = (stop.clone(), served, parked, outside);
        std::thread::spawn(move || {
            let mut swaps = 0u32;
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                if fs::rename(&served, &parked).is_ok() {
                    if symlink_file(&outside, &served) {
                        swaps += 1;
                        std::thread::yield_now();
                        let _ = fs::remove_file(&served);
                    }
                    let _ = fs::rename(&parked, &served);
                }
            }
            swaps
        })
    };

    let client = reqwest::blocking::Client::new();
    let (mut ok, mut missing) = (0u32, 0u32);
    for _ in 0..400 {
        let resp = match client.get(server.url("/f.txt")).send() {
            Ok(resp) => resp,
            Err(e) => panic!("the request itself failed: {e}"),
        };
        match resp.status().as_u16() {
            200 => {
                let declared = resp
                    .headers()
                    .get("content-length")
                    .map(|v| v.to_str().unwrap().parse::<usize>().unwrap());
                // A body cut off after its headers is an error here, not a short string.
                let body = resp.text().expect("a 200 must carry its whole body");
                assert_eq!(body, inside, "wrong bytes served");
                assert_eq!(declared, Some(body.len()), "Content-Length disagrees");
                ok += 1;
            }
            404 => missing += 1,
            other => panic!("unexpected status {other}"),
        }
    }
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    let swaps = swapper.join().unwrap();
    assert!(
        ok > 0,
        "no request ever got the file; the test proves nothing"
    );
    assert!(
        swaps > 0,
        "the file was never swapped; the test proves nothing"
    );
    eprintln!("200: {ok}, 404: {missing}, swaps: {swaps}");
}

/// Issue #618, deterministic: a file the server cannot read used to get `200` plus `Content-Length`
/// and then a body that was cut off when the open failed after the headers. The open now comes
/// first, so the answer is a `403` with its own small body. A mode-000 file is readable by root, so
/// the test skips where the account can still open it.
#[cfg(unix)]
#[test]
#[serial]
fn static_an_unreadable_file_is_403_before_any_header() {
    use std::os::unix::fs::PermissionsExt;
    let (server, dir) = make_static_server(&[("secret.txt", "classified bytes")]);
    let path = dir.path().join("secret.txt");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();
    if fs::File::open(&path).is_ok() {
        eprintln!("skipping: this account can read a mode-000 file (root)");
        return;
    }
    let resp = reqwest::blocking::get(server.url("/secret.txt")).expect("GET");
    assert_eq!(resp.status(), 403);
    assert_eq!(
        resp.text().expect("a 403 carries its whole body"),
        "Forbidden"
    );
    // HEAD opens the file too: the same answer, no body.
    let head = reqwest::blocking::Client::new()
        .head(server.url("/secret.txt"))
        .send()
        .expect("HEAD");
    assert_eq!(head.status(), 403);
}
