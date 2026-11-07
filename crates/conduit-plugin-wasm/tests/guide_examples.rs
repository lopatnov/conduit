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

/// A fence line: its character, its length and what follows it (CommonMark: three or more backticks or tildes).
fn fence(line: &str) -> Option<(char, usize, &str)> {
    let line = line.trim_start();
    let ch = line.chars().next().filter(|c| *c == '`' || *c == '~')?;
    let run = line.chars().take_while(|c| *c == ch).count();
    (run >= 3).then(|| (ch, run, &line[run..]))
}

/// The ```wat blocks of `markdown`. `open` is the fence being read (character, length) and, when it is a `wat`
/// block, the block; a fence of another language is tracked too, so that a line inside it is never taken for a
/// fence, and a fence closes only on the same character, at least as long, with nothing after it.
fn blocks(markdown: &str) -> Vec<Block> {
    let (mut found, mut skip_next) = (Vec::new(), false);
    let mut open: Option<(char, usize, Option<Block>)> = None;
    for (index, line) in markdown.lines().enumerate() {
        match (open.take(), fence(line)) {
            (Some((ch, len, block)), Some((c, run, rest)))
                if c == ch && run >= len && rest.trim().is_empty() =>
            {
                found.extend(block);
            }
            (Some((ch, len, mut block)), _) => {
                if let Some(b) = block.as_mut() {
                    b.text.push_str(line);
                    b.text.push('\n');
                }
                open = Some((ch, len, block));
            }
            (None, Some((ch, len, info))) => {
                let is_wat = info.split_whitespace().next() == Some("wat");
                let block = is_wat.then(|| Block {
                    text: String::new(),
                    line: index + 1,
                    skipped: skip_next,
                });
                open = Some((ch, len, block));
            }
            (None, None) => {}
        }
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

#[test]
fn fences_follow_commonmark_rules() {
    // A longer fence is not closed by a shorter run, a tilde fence is found, and a wat fence inside another
    // language's block is not a block.
    let md = [
        "````wat",
        "(module)",
        "```",
        "(still inside)",
        "````",
        "~~~wat",
        "(module)",
        "~~~",
        "```rust",
        "```wat",
        "not a block",
        "```",
    ]
    .join("\n");
    let found = blocks(&md);
    assert_eq!(
        found.len(),
        2,
        "{:?}",
        found.iter().map(|b| &b.text).collect::<Vec<_>>()
    );
    assert_eq!(found[0].text, "(module)\n```\n(still inside)\n");
    assert_eq!(found[1].text, "(module)\n");
}
