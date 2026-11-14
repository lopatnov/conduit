//! The fenced code blocks of a Markdown guide, read by the CommonMark rule (issue #663). Shared by the guide tests of
//! this crate and of `lopatnov-conduit-plugin-wasm` (which includes this file through `#[path]` and leaves its test out
//! of its published package, so that an unpacked crate never meets the missing path): one reader, no copy.

/// One fenced block: its text, the 1-based line of its opening fence and whether it is marked as a fragment by an HTML
/// comment `<!-- docs-check: skip (why) -->` on the line before the fence.
pub struct Block {
    pub text: String,
    pub line: usize,
    pub skipped: bool,
}

/// A fence line: its character, its length and what follows it (three or more backticks or tildes).
fn fence(line: &str) -> Option<(char, usize, &str)> {
    let line = line.trim_start();
    let ch = line.chars().next().filter(|c| *c == '`' || *c == '~')?;
    let run = line.chars().take_while(|c| *c == ch).count();
    (run >= 3).then(|| (ch, run, &line[run..]))
}

/// The blocks of `markdown` whose info string starts with `lang`. `open` is the fence being read (character, length)
/// and, when it is a block of `lang`, the block; a fence of another language is tracked too, so that a line inside it
/// is never taken for a fence, and a fence closes only on the same character, at least as long, with nothing after it.
/// A fence that is still open at the end of the document runs to its end, as in CommonMark.
pub fn blocks(markdown: &str, lang: &str) -> Vec<Block> {
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
                let block = (info.split_whitespace().next() == Some(lang)).then(|| Block {
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
    if let Some((_, _, block)) = open {
        found.extend(block);
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_extractor_finds_blocks_of_its_language_and_honours_the_skip_marker() {
        let md = "x\n```demo\nlet a = 1;\n```\n<!-- docs-check: skip (a fragment) -->\n```demo\nlet b =\n```\n```yaml\nx: 1\n```\n";
        let found = blocks(md, "demo");
        assert_eq!(found.len(), 2);
        assert_eq!((found[0].line, found[0].skipped), (2, false));
        assert_eq!(found[0].text, "let a = 1;\n");
        assert_eq!((found[1].line, found[1].skipped), (6, true));
    }

    #[test]
    fn fences_follow_commonmark_rules() {
        // A longer fence is not closed by a shorter run, a tilde fence is found, a block of the language inside
        // another language's block is not a block, and a fence left open runs to the end of the document.
        let md = [
            "````demo",
            "let a = 1;",
            "```",
            "(still inside)",
            "````",
            "~~~demo",
            "let b = 2;",
            "~~~",
            "```yaml",
            "```demo",
            "not a block",
            "```",
            "```demo",
            "let c = 3;",
        ]
        .join("\n");
        let found = blocks(&md, "demo");
        let texts: Vec<&str> = found.iter().map(|b| b.text.as_str()).collect();
        assert_eq!(
            texts,
            [
                "let a = 1;\n```\n(still inside)\n",
                "let b = 2;\n",
                "let c = 3;\n"
            ]
        );
    }
}
