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

use std::path::Path;

struct Block {
    text: String,
    line: usize,
    skipped: bool,
}

/// The ```wat blocks of `markdown`, one pass: `open` holds the block being read.
fn blocks(markdown: &str) -> Vec<Block> {
    let (mut found, mut open, mut skip_next) = (Vec::new(), None::<Block>, false);
    for (index, line) in markdown.lines().enumerate() {
        let fence = line.trim_start().starts_with("```");
        open = match open.take() {
            Some(block) if fence => {
                found.push(block);
                None
            }
            Some(mut block) => {
                block.text.push_str(line);
                block.text.push('\n');
                Some(block)
            }
            None if line.trim_start().starts_with("```wat") => Some(Block {
                text: String::new(),
                line: index + 1,
                skipped: skip_next,
            }),
            None => None,
        };
        skip_next = line.contains("docs-check: skip");
    }
    found
}

#[test]
fn every_wat_block_of_the_guide_assembles_into_a_module_the_host_accepts() {
    let guide = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/wasm.md");
    let Ok(markdown) = std::fs::read_to_string(&guide) else {
        eprintln!("docs/wasm.md is not next to this crate: nothing to check");
        return;
    };
    let found = blocks(&markdown);
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

#[test]
fn the_block_extractor_finds_blocks_and_honours_the_skip_marker() {
    let md = "x\n```wat\n(module)\n```\n<!-- docs-check: skip (a fragment) -->\n```wat\n(func\n```\n```rust\nfn a() {}\n```\n";
    let found = blocks(md);
    assert_eq!(found.len(), 2);
    assert_eq!((found[0].line, found[0].skipped), (2, false));
    assert_eq!(found[0].text, "(module)\n");
    assert_eq!((found[1].line, found[1].skipped), (6, true));
}
