//! Every ```wat block of `docs/wasm.md` must assemble into a module the host accepts, so that a sample in the
//! guide cannot rot (issue #565). A block that is deliberately not a whole module is marked with an HTML comment
//! on the line before the fence, invisible in the rendered page:
//!
//! ```text
//! <!-- docs-check: skip (why this block is not a whole module) -->
//! ```
//!
//! The Rust plugins of the guide are built by `scripts/check_doc_wasm.py`. The guide lives outside this crate, so
//! when it is not there (the crate was unpacked on its own) the test says so and passes.

// The fence reader is the one of the Rhai guide test (issue #663). This test is left out of the published package
// (`exclude` in Cargo.toml): the path below only exists in the workspace checkout.
#[path = "../../conduit-script-rhai/tests/common/fences.rs"]
mod fences;

/// The guide, or `None` when this crate was unpacked on its own. In the workspace checkout an unreadable guide
/// fails the test: a moved file must not make it pass by checking nothing.
fn guide() -> Option<String> {
    let manifest = env!("CARGO_MANIFEST_DIR");
    let text = std::fs::read_to_string(format!("{manifest}/../../docs/wasm.md"));
    match std::fs::read_to_string(format!("{manifest}/../../Cargo.toml")) {
        Ok(toml) if text.is_err() && toml.contains("[workspace]") => {
            panic!("docs/wasm.md cannot be read")
        }
        _ => {
            if text.is_err() {
                eprintln!("docs/wasm.md is not next to this crate: nothing to check");
            }
            text.ok()
        }
    }
}

#[test]
fn every_wat_block_of_the_guide_assembles_into_a_module_the_host_accepts() {
    let Some(markdown) = guide() else { return };
    let found = fences::blocks(&markdown, "wat");
    assert!(
        !found.is_empty(),
        "the guide is expected to hold a wat block"
    );
    let dir = tempfile::tempdir().unwrap();
    let mut failures = Vec::new();
    for block in found.iter().filter(|b| !b.skipped) {
        let binary = match wat::parse_str(&block.text) {
            Ok(binary) => binary,
            Err(e) => {
                failures.push(format!(
                    "docs/wasm.md:{} does not assemble: {e}",
                    block.line
                ));
                continue;
            }
        };
        let file = dir.path().join(format!("block-{}.wasm", block.line));
        std::fs::write(&file, binary).unwrap();
        if let Err(why) = conduit_plugin_wasm::check_module(file.to_str().unwrap()) {
            failures.push(format!("docs/wasm.md:{} {why}", block.line));
        }
    }
    assert!(
        failures.is_empty(),
        "wat blocks that do not work:\n{}",
        failures.join("\n")
    );
}
