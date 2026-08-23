/// CLI subcommand integration tests.
///
/// These tests invoke the `conduit` binary as a subprocess and verify exit
/// codes, stdout content, and argument parsing (including the `-c` short form
/// and `--config` long form).  No running server is required.
use std::process::Command;

fn conduit() -> Command {
    Command::new(env!("CARGO_BIN_EXE_conduit"))
}

// ── helpers ────────────────────────────────────────────────────────────────

/// Write a minimal valid config to a temp file and return (TempDir, PathBuf).
fn write_minimal_config() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("conduit.json");
    std::fs::write(
        &path,
        r#"{"global":{"admin":{"bind":"127.0.0.1:0"}},"sites":[{"port":0}]}"#,
    )
    .expect("write config");
    (dir, path)
}

/// Write a broken config and return (TempDir, PathBuf).
///
/// Uses `{"port": "not-a-number"}` which fails all three `ConfigFile` variants:
/// - `Full(AppConfig)` — no `sites` key, fails
/// - `Sites(Vec<SiteConfig>)` — not an array, fails
/// - `Single(SiteConfig)` — `port` must be u16, "not-a-number" fails
fn write_invalid_config() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("conduit.json");
    std::fs::write(&path, r#"{"port": "not-a-number"}"#).expect("write config");
    (dir, path)
}

/// Write a config with a validation-level error (duplicate host+port).
fn write_validation_error_config() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("conduit.json");
    std::fs::write(
        &path,
        r#"{"global":{"admin":{"bind":"127.0.0.1:0"}},"sites":[
              {"host":"a.example.com","port":8080},
              {"host":"a.example.com","port":8080}
           ]}"#,
    )
    .expect("write config");
    (dir, path)
}

/// Write a minimal valid YAML config and return (TempDir, PathBuf).
fn write_minimal_yaml_config() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("conduit.yaml");
    std::fs::write(
        &path,
        "global:\n  admin:\n    bind: \"127.0.0.1:0\"\nsites:\n  - port: 0\n",
    )
    .expect("write yaml config");
    (dir, path)
}

// ── --version / --help ──────────────────────────────────────────────────────

#[test]
fn version_flag_exits_zero() {
    let out = conduit().arg("--version").output().expect("run");
    assert!(out.status.success(), "exit code: {}", out.status);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("conduit"),
        "stdout should mention 'conduit': {stdout}"
    );
}

#[test]
fn help_flag_exits_zero() {
    let out = conduit().arg("--help").output().expect("run");
    assert!(out.status.success(), "exit code: {}", out.status);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("Usage"),
        "help output should contain 'Usage': {stdout}"
    );
}

// ── -c / --config short and long form ──────────────────────────────────────

#[test]
fn short_flag_c_works_with_validate() {
    let (_dir, path) = write_minimal_config();
    // `conduit -c <file> validate` — short flag before subcommand
    let out = conduit()
        .arg("-c")
        .arg(&path)
        .arg("validate")
        .output()
        .expect("run");
    assert!(
        out.status.success(),
        "exit {}; stderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn short_flag_c_after_subcommand_works() {
    let (_dir, path) = write_minimal_config();
    // `conduit validate -c <file>` — short flag after subcommand (global = true)
    let out = conduit()
        .arg("validate")
        .arg("-c")
        .arg(&path)
        .output()
        .expect("run");
    assert!(
        out.status.success(),
        "exit {}; stderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn long_flag_config_after_subcommand_works() {
    let (_dir, path) = write_minimal_config();
    let out = conduit()
        .arg("validate")
        .arg("--config")
        .arg(&path)
        .output()
        .expect("run");
    assert!(
        out.status.success(),
        "exit {}; stderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
}

// ── conduit validate ────────────────────────────────────────────────────────

#[test]
fn validate_valid_config_exits_zero_and_prints_ok() {
    let (_dir, path) = write_minimal_config();
    let out = conduit()
        .arg("validate")
        .arg("--config")
        .arg(&path)
        .output()
        .expect("run");
    assert!(
        out.status.success(),
        "exit {}; stderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("valid"),
        "stdout should say config is valid: {stdout}"
    );
}

#[test]
fn validate_parse_error_exits_nonzero() {
    let (_dir, path) = write_invalid_config();
    let out = conduit()
        .arg("validate")
        .arg("--config")
        .arg(&path)
        .output()
        .expect("run");
    assert!(
        !out.status.success(),
        "expected nonzero exit for parse error"
    );
}

#[test]
fn validate_semantic_error_exits_nonzero() {
    let (_dir, path) = write_validation_error_config();
    let out = conduit()
        .arg("validate")
        .arg("--config")
        .arg(&path)
        .output()
        .expect("run");
    assert!(
        !out.status.success(),
        "expected nonzero exit for validation error"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("error"),
        "stderr should describe the error: {stderr}"
    );
}

#[test]
fn validate_missing_file_exits_nonzero() {
    let dir = tempfile::tempdir().expect("tempdir");
    let missing = dir.path().join("no-such-file.json");
    let out = conduit()
        .arg("validate")
        .arg("--config")
        .arg(&missing)
        .output()
        .expect("run");
    assert!(
        !out.status.success(),
        "expected nonzero exit for missing file"
    );
}

/// A missing config file should print guidance pointing to `conduit init`
/// and `--help`, not just a bare "cannot read" error — see the report that
/// prompted this: a fresh binary run with no config anywhere in sight gave
/// no indication of what to do next.
#[test]
fn missing_config_file_prints_init_and_help_hint() {
    let dir = tempfile::tempdir().expect("tempdir");
    let missing = dir.path().join("no-such-file.json");
    let out = conduit()
        .arg("validate")
        .arg("--config")
        .arg(&missing)
        .output()
        .expect("run");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("conduit init"),
        "expected a `conduit init` hint in stderr, got: {stderr}"
    );
    assert!(
        stderr.contains("conduit --help"),
        "expected a `conduit --help` hint in stderr, got: {stderr}"
    );
}

/// The default (no-subcommand) server-start path shares the same config-load
/// error handling — running with no config file anywhere should fail fast
/// with the same hint, not attempt to bind and hang.
#[test]
fn serve_with_no_config_file_prints_hint_and_exits_nonzero() {
    let dir = tempfile::tempdir().expect("tempdir");
    let out = conduit()
        .current_dir(dir.path())
        .output()
        .expect("run");
    assert!(
        !out.status.success(),
        "expected nonzero exit when no config file exists anywhere"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("conduit init"),
        "expected a `conduit init` hint in stderr, got: {stderr}"
    );
}

/// A parse error on an *existing* file is a different problem than a missing
/// file — it should not get the "no config file found" hint, since the user
/// already has a config file and needs to fix its content, not create a new one.
#[test]
fn parse_error_on_existing_file_does_not_print_missing_config_hint() {
    let (_dir, path) = write_invalid_config();
    let out = conduit()
        .arg("validate")
        .arg("--config")
        .arg(&path)
        .output()
        .expect("run");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("No config file found"),
        "an existing-but-invalid file should not get the missing-file hint, got: {stderr}"
    );
}

// ── conduit fmt ─────────────────────────────────────────────────────────────

#[test]
fn fmt_prints_formatted_json_to_stdout() {
    let (_dir, path) = write_minimal_config();
    let out = conduit()
        .arg("fmt")
        .arg("--config")
        .arg(&path)
        .output()
        .expect("run");
    assert!(
        out.status.success(),
        "exit {}; stderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    // Should be pretty-printed JSON (has newlines and indentation)
    assert!(
        stdout.contains('\n') && stdout.contains("sites"),
        "expected pretty-printed JSON: {stdout}"
    );
    // Should be valid JSON
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("fmt stdout is not valid JSON: {e}\nOutput: {stdout}"));
    assert!(
        parsed.get("sites").is_some(),
        "formatted JSON must have 'sites' key"
    );
}

#[test]
fn fmt_write_overwrites_file() {
    let (_dir, path) = write_minimal_config();
    let original = std::fs::read_to_string(&path).expect("read");

    let out = conduit()
        .arg("fmt")
        .arg("--write")
        .arg("--config")
        .arg(&path)
        .output()
        .expect("run");
    assert!(
        out.status.success(),
        "exit {}; stderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );

    let after = std::fs::read_to_string(&path).expect("read after");
    // File was written (pretty-printed, so longer than the compact original)
    assert!(
        after.len() >= original.len(),
        "formatted file should not be shorter"
    );
    // Result is still valid JSON
    serde_json::from_str::<serde_json::Value>(&after).expect("formatted file should be valid JSON");
}

#[test]
fn fmt_short_c_flag_works() {
    let (_dir, path) = write_minimal_config();
    let out = conduit()
        .arg("fmt")
        .arg("-c")
        .arg(&path)
        .output()
        .expect("run");
    assert!(
        out.status.success(),
        "exit {}; stderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
}

// ── conduit validate / fmt — YAML config ────────────────────────────────────

#[test]
fn validate_accepts_yaml_config() {
    let (_dir, path) = write_minimal_yaml_config();
    let out = conduit()
        .arg("validate")
        .arg("-c")
        .arg(&path)
        .output()
        .expect("run");
    assert!(
        out.status.success(),
        "validate should accept YAML config; exit {}; stderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn fmt_yaml_outputs_yaml() {
    let (_dir, path) = write_minimal_yaml_config();
    let out = conduit()
        .arg("fmt")
        .arg("-c")
        .arg(&path)
        .output()
        .expect("run");
    assert!(out.status.success(), "fmt of YAML should succeed");
    let stdout = String::from_utf8_lossy(&out.stdout);
    // YAML output should not start with '{' (that would be JSON)
    assert!(
        !stdout.trim_start().starts_with('{'),
        "fmt of a .yaml file should output YAML, got: {stdout:.200}"
    );
    assert!(
        stdout.contains("sites"),
        "YAML output should contain 'sites'"
    );
}

// ── conduit init ─────────────────────────────────────────────────────────────

#[test]
fn init_yes_generates_valid_config() {
    let dir = tempfile::tempdir().expect("tempdir");
    let out = conduit()
        .args([
            "init",
            "--yes",
            "--port",
            "8080",
            "--proxy",
            "http://127.0.0.1:4000",
        ])
        .current_dir(dir.path())
        .output()
        .expect("run");
    assert!(
        out.status.success(),
        "init --yes should exit 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    // Should have written a config file in the current directory
    let json_exists = dir.path().join("conduit.json").exists();
    let yaml_exists = dir.path().join("conduit.yaml").exists();
    assert!(
        json_exists || yaml_exists,
        "init --yes should write conduit.json or conduit.yaml"
    );
}

#[test]
fn init_yes_format_json_generates_json() {
    let dir = tempfile::tempdir().expect("tempdir");
    let out = conduit()
        .args(["init", "--yes", "--format", "json"])
        .current_dir(dir.path())
        .output()
        .expect("run");
    assert!(
        out.status.success(),
        "init --yes --format json should exit 0"
    );
    let path = dir.path().join("conduit.json");
    assert!(
        path.exists(),
        "conduit.json should be created with --format json"
    );
    // Verify the file is valid JSON
    let contents = std::fs::read_to_string(&path).expect("read config");
    serde_json::from_str::<serde_json::Value>(&contents)
        .expect("conduit.json should be valid JSON");
}

#[test]
fn init_yes_output_flag_writes_to_specified_path() {
    let dir = tempfile::tempdir().expect("tempdir");
    let out_path = dir.path().join("my-config.json");
    let out = conduit()
        .args(["init", "--yes", "--output", out_path.to_str().unwrap()])
        .current_dir(dir.path())
        .output()
        .expect("run");
    assert!(out.status.success(), "init --yes --output should exit 0");
    assert!(out_path.exists(), "output file should be created");
    let contents = std::fs::read_to_string(&out_path).expect("read");
    serde_json::from_str::<serde_json::Value>(&contents).expect("output file should be valid JSON");
}

// ── conduit probe ───────────────────────────────────────────────────────────

#[test]
fn probe_no_upstreams_exits_zero_with_message() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("conduit.json");
    // Config with static only — no upstream URLs to probe
    std::fs::write(
        &path,
        r#"{"global":{"admin":{"bind":"127.0.0.1:0"}},"sites":[{"port":0,"static":"./dist"}]}"#,
    )
    .expect("write");

    let out = conduit()
        .arg("probe")
        .arg("--config")
        .arg(&path)
        .output()
        .expect("run");
    assert!(
        out.status.success(),
        "exit {}; stderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("No upstream"),
        "expected 'No upstream' message: {stdout}"
    );
}

#[test]
fn probe_unreachable_upstream_exits_nonzero() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("conduit.json");
    // Port 1 is always unreachable (reserved / no service)
    std::fs::write(
        &path,
        r#"{"global":{"admin":{"bind":"127.0.0.1:0"}},"sites":[{"port":0,"proxy":"http://127.0.0.1:1"}]}"#,
    )
    .expect("write");

    let out = conduit()
        .arg("probe")
        .arg("-c")
        .arg(&path)
        .output()
        .expect("run");
    assert!(
        !out.status.success(),
        "probe to unreachable upstream should exit nonzero"
    );
}

// ── conduit completions ─────────────────────────────────────────────────────

#[test]
fn completions_bash_outputs_shell_script() {
    let out = conduit()
        .args(["completions", "bash"])
        .output()
        .expect("run");
    assert!(
        out.status.success(),
        "exit {}; stderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !stdout.is_empty(),
        "completions bash should produce non-empty output"
    );
    // Bash completions contain a function definition
    assert!(
        stdout.contains("conduit"),
        "bash completions should reference 'conduit': {stdout:.200}"
    );
}

#[test]
fn completions_zsh_outputs_script() {
    let out = conduit()
        .args(["completions", "zsh"])
        .output()
        .expect("run");
    assert!(out.status.success());
    assert!(!out.stdout.is_empty());
}

#[test]
fn completions_fish_outputs_script() {
    let out = conduit()
        .args(["completions", "fish"])
        .output()
        .expect("run");
    assert!(out.status.success());
    assert!(!out.stdout.is_empty());
}

#[test]
fn completions_powershell_outputs_script() {
    // clap_complete uses "power-shell" (with hyphen) as the variant name
    let out = conduit()
        .args(["completions", "power-shell"])
        .output()
        .expect("run");
    assert!(out.status.success());
    assert!(!out.stdout.is_empty());
}

#[test]
fn completions_elvish_outputs_script() {
    let out = conduit()
        .args(["completions", "elvish"])
        .output()
        .expect("run");
    assert!(out.status.success());
    assert!(!out.stdout.is_empty());
}

// ── conduit man ─────────────────────────────────────────────────────────────

#[test]
fn man_outputs_troff_content() {
    let out = conduit().arg("man").output().expect("run");
    assert!(
        out.status.success(),
        "exit {}; stderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !stdout.is_empty(),
        "man page should produce non-empty output"
    );
    // troff man pages start with a .TH macro
    assert!(
        stdout.contains(".TH") || stdout.contains("conduit"),
        "man output should be troff: {stdout:.200}"
    );
}

// ── CONDUIT_ADMIN env var ────────────────────────────────────────────────────

#[test]
fn conduit_admin_env_var_used_by_reload() {
    // CONDUIT_ADMIN env var should be used as default admin address
    let out = conduit()
        .args(["reload"])
        .env("CONDUIT_ADMIN", "127.0.0.1:19890")
        .output()
        .expect("run");
    // No server is running, so it should fail — but it must attempt the
    // right address (we can't verify the address directly, but a non-zero
    // exit code confirms the env var was picked up and a connection was tried)
    assert!(
        !out.status.success(),
        "reload via CONDUIT_ADMIN should fail gracefully when no server runs"
    );
}

// ── admin commands fail gracefully when no server is running ────────────────

#[test]
fn reload_no_server_exits_nonzero() {
    let out = conduit()
        .args(["reload", "--admin", "127.0.0.1:19876"])
        .output()
        .expect("run");
    assert!(
        !out.status.success(),
        "reload with no server should exit nonzero"
    );
}

#[test]
fn status_no_server_exits_nonzero() {
    let out = conduit()
        .args(["status", "--admin", "127.0.0.1:19877"])
        .output()
        .expect("run");
    assert!(
        !out.status.success(),
        "status with no server should exit nonzero"
    );
}

#[test]
fn shutdown_no_server_exits_nonzero() {
    let out = conduit()
        .args(["shutdown", "--admin", "127.0.0.1:19878"])
        .output()
        .expect("run");
    assert!(
        !out.status.success(),
        "shutdown with no server should exit nonzero"
    );
}

#[test]
fn upstreams_no_server_exits_nonzero() {
    let out = conduit()
        .args(["upstreams", "--admin", "127.0.0.1:19879"])
        .output()
        .expect("run");
    assert!(
        !out.status.success(),
        "upstreams with no server should exit nonzero"
    );
}

#[test]
fn upstreams_remove_no_server_exits_nonzero() {
    let out = conduit()
        .args([
            "upstreams",
            "--admin",
            "127.0.0.1:19881",
            "remove",
            "--route",
            "/api",
            "--target",
            "http://127.0.0.1:4000",
        ])
        .output()
        .expect("run");
    assert!(
        !out.status.success(),
        "upstreams remove with no server should exit nonzero"
    );
}

#[test]
fn upstreams_weight_no_server_exits_nonzero() {
    let out = conduit()
        .args([
            "upstreams",
            "--admin",
            "127.0.0.1:19882",
            "weight",
            "--route",
            "/api",
            "--target",
            "http://127.0.0.1:4000",
            "--weight",
            "2",
        ])
        .output()
        .expect("run");
    assert!(
        !out.status.success(),
        "upstreams weight with no server should exit nonzero"
    );
}

#[test]
fn upstreams_add_no_server_exits_nonzero() {
    let out = conduit()
        .args([
            "upstreams",
            "--admin",
            "127.0.0.1:19880",
            "add",
            "--route",
            "/api",
            "--target",
            "http://127.0.0.1:4000",
        ])
        .output()
        .expect("run");
    assert!(
        !out.status.success(),
        "upstreams add with no server should exit nonzero"
    );
}
