//! Every ```rhai block of `docs/rhai.md` must compile, so that an example in the guide cannot rot
//! (issue #566). A block that is deliberately not a whole script (an excerpt, a fragment that shows
//! one construct) is marked with an HTML comment on the line before the fence, invisible in the
//! rendered page:
//!
//! ```text
//! <!-- docs-check: skip (why this block is not a whole script) -->
//! ```
//!
//! The guide lives outside this crate, so when it is not there (the crate was unpacked on its own)
//! the test says so and passes.

mod common;
#[path = "common/fences.rs"]
mod fences;

#[test]
fn every_rhai_block_of_the_guide_compiles() {
    let Some(markdown) = common::guide("rhai.md") else {
        return;
    };
    let found = fences::blocks(&markdown, "rhai");
    assert!(
        found.len() >= 10,
        "the guide is expected to hold many rhai blocks, found {}",
        found.len()
    );
    let dir = tempfile::tempdir().unwrap();
    let mut failures = Vec::new();
    for block in found.iter().filter(|b| !b.skipped) {
        let file = dir.path().join(format!("block-{}.rhai", block.line));
        std::fs::write(&file, &block.text).unwrap();
        if let Err(why) = conduit_script_rhai::check_script(file.to_str().unwrap()) {
            failures.push(format!("docs/rhai.md:{} {why}", block.line));
        }
    }
    assert!(
        failures.is_empty(),
        "rhai blocks that do not compile:\n{}",
        failures.join("\n")
    );
}

#[test]
fn a_block_that_does_not_compile_is_reported_with_its_position() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("bad.rhai");
    std::fs::write(&file, "let x = ;\n").unwrap();
    let why = conduit_script_rhai::check_script(file.to_str().unwrap()).unwrap_err();
    assert!(
        why.contains("does not compile") && why.contains("line 1"),
        "{why}"
    );
}
