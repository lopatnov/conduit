mod common;

use serial_test::serial;

#[test]
#[serial]
fn health_returns_200() {
    let server = common::TestServer::start_minimal();
    let resp = reqwest::blocking::get(server.url("/__health__")).expect("GET /__health__");
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().expect("JSON body");
    assert_eq!(body["status"], "ok");
}

#[test]
#[serial]
fn unknown_path_returns_404() {
    let server = common::TestServer::start_minimal();
    let resp = reqwest::blocking::get(server.url("/does-not-exist")).expect("GET /does-not-exist");
    assert_eq!(resp.status(), 404);
}

#[test]
#[serial]
fn admin_status_returns_running() {
    let server = common::TestServer::start_minimal();
    let resp = reqwest::blocking::get(server.admin_url("/status")).expect("GET /status");
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().expect("JSON body");
    assert_eq!(body["status"], "running");
}
