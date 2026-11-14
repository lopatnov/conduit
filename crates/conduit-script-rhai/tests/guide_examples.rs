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

/// One ```rhai block: its text, the 1-based line of its opening fence and whether it is marked as a fragment.
struct Block {
    text: String,
    line: usize,
    skipped: bool,
}

/// A fence line: its character, its length and what follows it (CommonMark: three or more backticks or tildes).
///
/// The same rule as `crates/conduit-plugin-wasm/tests/guide_examples.rs` (issue #663): two crates cannot share a
/// test file without breaking the standalone packaging of either, so the logic is written twice. Keep them equal.
fn fence(line: &str) -> Option<(char, usize, &str)> {
    let line = line.trim_start();
    let ch = line.chars().next().filter(|c| *c == '`' || *c == '~')?;
    let run = line.chars().take_while(|c| *c == ch).count();
    (run >= 3).then(|| (ch, run, &line[run..]))
}

/// The ```rhai blocks of `markdown`. `open` is the fence being read (character, length) and, when it is a `rhai`
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
                let is_rhai = info.split_whitespace().next() == Some("rhai");
                let block = is_rhai.then(|| Block {
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
fn every_rhai_block_of_the_guide_compiles() {
    let Some(markdown) = common::guide("rhai.md") else {
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
fn fences_follow_commonmark_rules() {
    // A longer fence is not closed by a shorter run, a tilde fence is found, and a rhai fence inside another
    // language's block is not a block (issue #663).
    let md = [
        "````rhai",
        "let a = 1;",
        "```",
        "(still inside)",
        "````",
        "~~~rhai",
        "let b = 2;",
        "~~~",
        "```yaml",
        "```rhai",
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
    assert_eq!(found[0].text, "let a = 1;\n```\n(still inside)\n");
    assert_eq!(found[1].text, "let b = 2;\n");
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
