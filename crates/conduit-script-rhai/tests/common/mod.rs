//! Shared by the guide tests of this crate: `tests/guide_examples.rs` (as a module) and `src/api_doc_tests.rs`
//! (through `#[path]`).

use std::path::Path;

/// The guide `docs/<name>`, or `None` when this crate was unpacked on its own (no workspace above it). In the
/// workspace checkout a guide that cannot be read fails the test: a moved file must not make the check pass by
/// checking nothing.
pub fn guide(name: &str) -> Option<String> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let read = std::fs::read_to_string(root.join("docs").join(name));
    let in_workspace =
        std::fs::read_to_string(root.join("Cargo.toml")).is_ok_and(|t| t.contains("[workspace]"));
    assert!(
        read.is_ok() || !in_workspace,
        "docs/{name} cannot be read in the workspace checkout"
    );
    if read.is_err() {
        eprintln!("docs/{name} is not next to this crate: nothing to check");
    }
    read.ok()
}
