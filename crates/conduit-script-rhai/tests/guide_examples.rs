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

use std::path::Path;

/// One ```rhai block: its text, the 1-based line of its opening fence and whether it is marked as a fragment.
struct Block {
    text: String,
    line: usize,
    skipped: bool,
}

fn blocks(markdown: &str) -> Vec<Block> {
    let mut found = Vec::new();
    let mut lines = markdown.lines().enumerate().peekable();
    let mut previous = "";
    while let Some((index, line)) = lines.next() {
        if line.trim_start().starts_with("```rhai") {
            let mut text = String::new();
            for (_, body) in lines.by_ref() {
                if body.trim_start().starts_with("```") {
                    break;
                }
                text.push_str(body);
                text.push('\n');
            }
            found.push(Block {
                text,
                line: index + 1,
                skipped: previous.contains("docs-check: skip"),
            });
        }
        previous = line;
    }
    found
}

/// The guide, or `None` when this crate was unpacked on its own (no workspace above it). In the workspace checkout
/// a guide that cannot be read fails the test: a moved file must not make the check pass by checking nothing.
fn guide(name: &str) -> Option<String> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let read = std::fs::read_to_string(root.join("docs").join(name));
    let in_workspace =
        std::fs::read_to_string(root.join("Cargo.toml")).is_ok_and(|t| t.contains("[workspace]"));
    assert!(
        read.is_ok() || !in_workspace,
        "docs/{name} cannot be read in the workspace checkout"
    );
    read.ok()
}

#[test]
fn every_rhai_block_of_the_guide_compiles() {
    let Some(markdown) = guide("rhai.md") else {
        eprintln!("docs/rhai.md is not next to this crate: nothing to check");
        return;
    };
    let found = blocks(&markdown);
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
fn the_block_extractor_finds_blocks_and_honours_the_skip_marker() {
    let md = "text\n```rhai\nlet a = 1;\n```\n<!-- docs-check: skip (a fragment) -->\n```rhai\nlet b =\n```\n```yaml\nx: 1\n```\n";
    let found = blocks(md);
    assert_eq!(found.len(), 2);
    assert_eq!((found[0].line, found[0].skipped), (2, false));
    assert_eq!(found[0].text, "let a = 1;\n");
    assert_eq!((found[1].line, found[1].skipped), (6, true));
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
