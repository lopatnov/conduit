use std::env;

/// Replace `$VAR_NAME` patterns in config text with environment variable values.
/// If the variable is not set, the original `$VAR_NAME` is kept unchanged.
/// Interpolation is done at the raw text level before JSON parsing.
pub fn interpolate(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();

    while let Some(c) = chars.next() {
        if c != '$' {
            result.push(c);
            continue;
        }

        // Must start with letter or underscore (env var naming convention)
        match chars.peek() {
            Some(&first) if first.is_ascii_alphabetic() || first == '_' => {}
            _ => {
                result.push('$');
                continue;
            }
        }

        let mut var_name = String::new();
        while let Some(&next) = chars.peek() {
            if next.is_ascii_alphanumeric() || next == '_' {
                var_name.push(next);
                chars.next();
            } else {
                break;
            }
        }

        match env::var(&var_name) {
            Ok(val) => {
                // JSON-escape the value so that special characters (quotes,
                // backslashes, control chars) cannot break the JSON structure
                // or inject unexpected keys/values.
                // serde_json::to_string produces `"escaped"` — strip the quotes
                // since the $VAR placeholder is already inside a JSON string.
                let escaped = serde_json::to_string(&val)
                    .map(|s| s[1..s.len() - 1].to_owned())
                    .unwrap_or(val);
                result.push_str(&escaped);
            }
            Err(_) => {
                result.push('$');
                result.push_str(&var_name);
            }
        }
    }

    result
}

/// The names of the environment variables that `text` refers to (`$NAME`) and that are set, in order of first
/// appearance: the references [`interpolate`] and [`interpolate_yaml`] would replace by their values.
pub fn resolved_references(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut names: Vec<String> = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '$' {
            if let Some((name, end)) = reference_at(&chars, i) {
                if env::var(&name).is_ok() && !names.contains(&name) {
                    names.push(name);
                }
                i = end;
                continue;
            }
        }
        i += 1;
    }
    names
}

/// `$NAME` starting at `chars[at]`: the name and the index after it.
fn reference_at(chars: &[char], at: usize) -> Option<(String, usize)> {
    let first = *chars.get(at + 1)?;
    if !(first.is_ascii_alphabetic() || first == '_') {
        return None;
    }
    let mut end = at + 1;
    while end < chars.len() && (chars[end].is_ascii_alphanumeric() || chars[end] == '_') {
        end += 1;
    }
    Some((chars[at + 1..end].iter().collect(), end))
}

/// Where a `$NAME` stands in a YAML document, which decides how its value must be written.
#[derive(Clone, Copy, PartialEq)]
enum Ctx {
    /// An unquoted value (or key).
    Plain,
    /// Inside `"..."`.
    Double,
    /// Inside `'...'`.
    Single,
    /// After a `#` comment mark.
    Comment,
    /// The content lines of a `|` or `>` block scalar.
    Block,
}

/// Replace `$VAR_NAME` in YAML text with environment variable values, writing each value so that it reads back
/// **unchanged** and can never add or change a key (issue #680).
///
/// [`interpolate`] escapes a value as a JSON string, which is right inside `"..."` and wrong anywhere else in a
/// YAML file: a backslash or a quote in a plain value came out doubled or escaped. This function looks at where the
/// reference stands:
/// - in `"..."`: JSON escapes (YAML double-quoted scalars use the same ones);
/// - in `'...'`: a quote is doubled (`''`); a control character is refused, there is no way to write it;
/// - in a plain value: written as it is when that reads back unchanged, otherwise refused, with the advice to put the
///   reference in double quotes. Refused are a control character or a line break of any kind (LF, CR, NEL,
///   U+2028, U+2029: libyaml reads them all as line ends), a value that starts or ends with a space, one that starts
///   with a YAML indicator (`*`, `&`, `!`, `|`, `>`, `%`, `@`, a quote, a bracket, `#`, `- `, `? `, `: `), one that
///   contains `: ` or ` #` or starts with `#` after a blank, an empty value or `null`/`~` as the whole value (a secret
///   that reads as "not set"), and, inside `[...]` or `{...}`, a comma, a bracket or a leading `?`;
/// - in a block scalar: as it is, except a line break (it would end the block);
/// - in a comment: not replaced.
///
/// The position is found with a small scanner (quotes, comments, flow brackets, block scalar headers). When it
/// is wrong the worst outcome is an error or JSON escaping (what [`interpolate`] always did), not a changed key.
/// An unset variable keeps its `$NAME`, as in [`interpolate`].
pub fn interpolate_yaml(text: &str) -> Result<String, String> {
    let mut out = String::with_capacity(text.len());
    let mut ctx = Ctx::Plain;
    let mut flow = 0usize;
    let mut block_indent = 0usize;
    for line in text.split_inclusive('\n') {
        let chars: Vec<char> = line.chars().collect();
        if ctx == Ctx::Block {
            let blank = line.trim().is_empty();
            let indent = chars.iter().take_while(|c| **c == ' ').count();
            if !blank && indent <= block_indent {
                ctx = Ctx::Plain;
            }
        }
        if ctx == Ctx::Comment {
            ctx = Ctx::Plain;
        }
        let started_plain = ctx == Ctx::Plain;
        scan_line(&chars, &mut ctx, &mut flow, &mut out)?;
        if ctx == Ctx::Comment {
            ctx = Ctx::Plain;
        }
        if started_plain && ctx == Ctx::Plain && is_block_header(line) {
            ctx = Ctx::Block;
            block_indent = chars.iter().take_while(|c| **c == ' ').count();
        }
    }
    Ok(out)
}

/// Whether a (complete) line ends in a block scalar header: `key: |`, `- >-`, `key: |2+ # comment`.
fn is_block_header(line: &str) -> bool {
    let code = match line.find(" #") {
        Some(i) => &line[..i],
        None => line,
    };
    let code = code.trim_end();
    let Some(token_start) = code.rfind(|c: char| c.is_whitespace()).map(|i| i + 1) else {
        return false;
    };
    let token = &code[token_start..];
    let header = token.starts_with('|') || token.starts_with('>');
    header
        && token.len() <= 3
        && token[1..]
            .chars()
            .all(|c| c.is_ascii_digit() || c == '+' || c == '-')
        && matches!(
            code[..token_start].trim_end().chars().last(),
            Some(':' | '-')
        )
}

/// Scan one line, appending it (references replaced) to `out`.
fn scan_line(
    chars: &[char],
    ctx: &mut Ctx,
    flow: &mut usize,
    out: &mut String,
) -> Result<(), String> {
    // the last character of the line that is not a blank, and whether a blank came after it
    let mut prev: Option<char> = None;
    let mut blank_after = true;
    // the last blank-separated token before the current one was an anchor (`&a`) or a tag (`!!str`): a quote that
    // follows it opens a quoted scalar
    let mut token_is_property = false;
    let mut last_token_was_property = false;
    let mut in_token = false;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match *ctx {
            Ctx::Comment => {
                out.push(c);
                i += 1;
                continue;
            }
            Ctx::Double => {
                if c == '\\' && i + 1 < chars.len() {
                    out.push(c);
                    out.push(chars[i + 1]);
                    i += 2;
                    continue;
                }
                if c == '"' {
                    *ctx = Ctx::Plain;
                    prev = Some('"');
                    blank_after = false;
                    out.push(c);
                    i += 1;
                    continue;
                }
            }
            Ctx::Single => {
                if c == '\'' {
                    if chars.get(i + 1) == Some(&'\'') {
                        out.push_str("''");
                        i += 2;
                        continue;
                    }
                    *ctx = Ctx::Plain;
                    prev = Some('\'');
                    blank_after = false;
                    out.push(c);
                    i += 1;
                    continue;
                }
            }
            Ctx::Plain | Ctx::Block => {}
        }

        if c == '$' {
            if let Some((name, end)) = reference_at(chars, i) {
                if let Ok(value) = env::var(&name) {
                    let next = chars.get(end).copied();
                    let at_start = matches!(
                        (prev, blank_after),
                        (None, _) | (Some(':' | '-' | '?'), true) | (Some('[' | '{' | ','), _)
                    );
                    // a blank right before the reference: a value that starts with `#` would turn into a comment
                    let after_blank = blank_after && prev.is_some();
                    let place = Place {
                        at_start,
                        after_blank,
                        in_flow: *flow > 0,
                        next,
                    };
                    let written = write_value(&name, &value, *ctx, place)?;
                    out.push_str(&written);
                    prev = value.chars().last().or(prev);
                    blank_after = false;
                    i = end;
                    continue;
                }
                // unset: the reference stays as written
                out.extend(chars[i..end].iter());
                prev = Some('x');
                blank_after = false;
                i = end;
                continue;
            }
        }

        if *ctx == Ctx::Plain {
            match c {
                '#' if prev.is_none() || blank_after => {
                    *ctx = Ctx::Comment;
                    out.push(c);
                    i += 1;
                    continue;
                }
                '"' | '\''
                    if starts_scalar(prev, blank_after)
                        || (blank_after && last_token_was_property) =>
                {
                    *ctx = if c == '"' { Ctx::Double } else { Ctx::Single };
                    out.push(c);
                    i += 1;
                    continue;
                }
                '[' | '{' => *flow += 1,
                ']' | '}' => *flow = flow.saturating_sub(1),
                _ => {}
            }
        }
        if c.is_whitespace() {
            if in_token {
                last_token_was_property = token_is_property;
                in_token = false;
            }
            blank_after = true;
        } else {
            if !in_token {
                token_is_property = c == '&' || c == '!';
                in_token = true;
            }
            prev = Some(c);
            blank_after = false;
        }
        out.push(c);
        i += 1;
    }
    Ok(())
}

/// Whether a quote at this point opens a quoted scalar (and not an apostrophe inside a plain one).
fn starts_scalar(prev: Option<char>, blank_after: bool) -> bool {
    match prev {
        None => true,
        Some(':' | '-' | '?') => blank_after,
        Some('[' | '{' | ',') => true,
        Some(_) => false,
    }
}

/// Where in a plain scalar a reference stands.
#[derive(Clone, Copy)]
struct Place {
    /// It starts the scalar.
    at_start: bool,
    /// A blank comes right before it (after other text), so a `#` at the start of the value would open a comment.
    after_blank: bool,
    /// It is inside `[ ]` or `{ }`.
    in_flow: bool,
    /// The character after it.
    next: Option<char>,
}

/// A character that libyaml (the reader of this build) takes for the end of a line: LF, CR, NEL (U+0085) and the
/// Unicode line and paragraph separators (U+2028, U+2029). `char::is_control` knows NEL and not the other two.
fn is_line_break(c: char) -> bool {
    matches!(c, '\n' | '\r' | '\u{85}' | '\u{2028}' | '\u{2029}')
}

/// A character that must not be written as it is anywhere but in a double-quoted scalar: a control character or a
/// line break of any kind.
fn is_unwritable(c: char) -> bool {
    c.is_control() || is_line_break(c)
}

/// `value` as the inside of a YAML double-quoted scalar: the JSON escapes, and `\uXXXX` for the characters JSON leaves
/// alone and YAML folds or reads as a line break (DEL and C1 controls such as NEL, U+2028, U+2029).
fn double_quoted(value: &str) -> String {
    let json = serde_json::to_string(value).unwrap_or_else(|_| format!("\"{value}\""));
    let mut out = String::with_capacity(json.len());
    for c in json[1..json.len() - 1].chars() {
        if is_unwritable(c) {
            out.push_str(&format!("\\u{:04X}", c as u32));
        } else {
            out.push(c);
        }
    }
    out
}

/// The text to put in place of `$name` (whose value is `value`) in context `ctx`.
fn write_value(name: &str, value: &str, ctx: Ctx, place: Place) -> Result<String, String> {
    match ctx {
        Ctx::Comment => Ok(format!("${name}")),
        Ctx::Double => Ok(double_quoted(value)),
        Ctx::Single => {
            if value.chars().any(is_unwritable) {
                return Err(refusal(
                    name,
                    "a control character such as a line break",
                    "single-quoted",
                ));
            }
            Ok(value.replace('\'', "''"))
        }
        Ctx::Block => {
            if value.chars().any(is_line_break) {
                return Err(refusal(name, "a line break", "block scalar"));
            }
            Ok(value.to_owned())
        }
        Ctx::Plain => match plain_problem(value, place) {
            Some(why) => Err(refusal(name, why, "plain")),
            None => Ok(value.to_owned()),
        },
    }
}

fn refusal(name: &str, why: &str, place: &str) -> String {
    format!(
        "the environment variable ${name} contains {why}, which cannot be written into a {place} YAML value \
         without changing it; put the reference in double quotes (\"${name}\") instead"
    )
}

/// What stops `value` from being written as it is into a plain YAML scalar, `None` if nothing does.
fn plain_problem(value: &str, place: Place) -> Option<&'static str> {
    let Place {
        at_start,
        after_blank,
        in_flow,
        next,
    } = place;
    if value.chars().any(is_unwritable) {
        return Some("a line break or another control character");
    }
    // the value is the last thing of the scalar: a line end, a blank or comment, or (in a list) a separator
    let at_end = matches!(next, None | Some('\n' | '\r' | ' ' | '#'))
        || (in_flow && matches!(next, Some(',' | ']' | '}')));
    // a scalar that is the whole value and reads as null (or is empty) means "not set": for a secret that is a
    // silent fail-open, so it must be said in quotes
    if at_start && at_end && (value.is_empty() || matches!(value, "null" | "Null" | "NULL" | "~")) {
        return Some("an empty value, or one that YAML reads as null");
    }
    if (at_start && value.starts_with(char::is_whitespace))
        || (at_end && value.ends_with(char::is_whitespace))
    {
        return Some("a space at its start or end");
    }
    if value.contains(": ") || (at_end && value.ends_with(':')) {
        return Some("a colon followed by a space");
    }
    if value.contains(" #") || (after_blank && value.starts_with('#')) {
        return Some("a space followed by #");
    }
    if at_start {
        let first = value.chars().next();
        if matches!(
            first,
            Some(
                '&' | '*'
                    | '!'
                    | '|'
                    | '>'
                    | '%'
                    | '@'
                    | '`'
                    | '\''
                    | '"'
                    | '#'
                    | '['
                    | ']'
                    | '{'
                    | '}'
                    | ','
            )
        ) {
            return Some("a character at its start that YAML reads as an indicator");
        }
        if matches!(value, "-" | "?" | ":") || value.starts_with("- ") || value.starts_with("? ") {
            return Some("an indicator at its start");
        }
        // inside a list `[?x]` is a mapping with the key x (libyaml), not the text `?x`
        if in_flow && value.starts_with('?') {
            return Some("a question mark at its start inside a [ ] or { } list");
        }
    }
    if in_flow && value.contains([',', '[', ']', '{', '}']) {
        return Some("a comma or a bracket inside a [ ] or { } list");
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    #[test]
    #[serial]
    fn replaces_known_var() {
        std::env::set_var("CONDUIT_TEST_TOKEN", "secret");
        let out = interpolate(r#"{"token": "$CONDUIT_TEST_TOKEN"}"#);
        assert_eq!(out, r#"{"token": "secret"}"#);
    }

    #[test]
    #[serial]
    fn keeps_unknown_var() {
        let out = interpolate(r#"{"token": "$CONDUIT_NONEXISTENT_XYZ_987"}"#);
        assert_eq!(out, r#"{"token": "$CONDUIT_NONEXISTENT_XYZ_987"}"#);
    }

    #[test]
    #[serial]
    fn ignores_bare_dollar() {
        let out = interpolate("price: $42");
        assert_eq!(out, "price: $42");
    }

    #[test]
    #[serial]
    fn ignores_double_dollar() {
        let out = interpolate("$$VAR");
        assert_eq!(out, "$$VAR");
    }

    #[test]
    #[serial]
    fn escapes_quotes_in_value() {
        std::env::set_var("CONDUIT_TEST_QUOTE", r#"say "hello""#);
        let out = interpolate(r#"{"msg": "$CONDUIT_TEST_QUOTE"}"#);
        // The double-quotes inside the value must be escaped so the JSON stays valid.
        assert_eq!(out, r#"{"msg": "say \"hello\""}"#);
    }

    #[test]
    #[serial]
    fn escapes_backslash_in_value() {
        std::env::set_var("CONDUIT_TEST_BACKSLASH", r"C:\path\to\file");
        let out = interpolate(r#"{"path": "$CONDUIT_TEST_BACKSLASH"}"#);
        assert_eq!(out, r#"{"path": "C:\\path\\to\\file"}"#);
    }

    #[test]
    #[serial]
    fn var_at_end_of_string() {
        let out = interpolate("value=$CONDUIT_NONEXISTENT_END");
        assert_eq!(out, "value=$CONDUIT_NONEXISTENT_END");
    }

    #[test]
    #[serial]
    fn var_followed_by_special_char() {
        let out = interpolate("$CONDUIT_NONEXISTENT_SPEC.");
        assert_eq!(out, "$CONDUIT_NONEXISTENT_SPEC.");
    }

    #[test]
    #[serial]
    fn no_substitution_when_empty() {
        let out = interpolate("");
        assert_eq!(out, "");
    }

    #[test]
    #[serial]
    fn dollar_at_end() {
        let out = interpolate("trailing$");
        assert_eq!(out, "trailing$");
    }

    #[test]
    #[serial]
    fn multiple_vars_in_one_string() {
        std::env::set_var("CONDUIT_TEST_A", "hello");
        std::env::set_var("CONDUIT_TEST_B", "world");
        let out = interpolate("$CONDUIT_TEST_A $CONDUIT_TEST_B");
        assert_eq!(out, "hello world");
    }
}

/// Issue #680: values in YAML files arrive unchanged, in every place a reference can stand.
#[cfg(test)]
mod yaml_tests {
    use super::*;
    use serial_test::serial;

    fn with_var<T>(name: &str, value: &str, f: impl FnOnce() -> T) -> T {
        std::env::set_var(name, value);
        let out = f();
        std::env::remove_var(name);
        out
    }

    #[test]
    #[serial]
    fn a_plain_value_keeps_its_backslash_and_quote() {
        let out = with_var("CONDUIT_YAML_T1", r#"a\b"c"#, || {
            interpolate_yaml("host: $CONDUIT_YAML_T1\n")
        });
        assert_eq!(out.unwrap(), "host: a\\b\"c\n");
    }

    #[test]
    #[serial]
    fn a_double_quoted_value_is_json_escaped() {
        let out = with_var("CONDUIT_YAML_T2", "a\\b\"c\nd", || {
            interpolate_yaml("key: \"$CONDUIT_YAML_T2\"\n")
        });
        assert_eq!(out.unwrap(), "key: \"a\\\\b\\\"c\\nd\"\n");
    }

    #[test]
    #[serial]
    fn a_single_quoted_value_doubles_its_quote_and_keeps_backslashes() {
        let out = with_var("CONDUIT_YAML_T3", "it's a\\b", || {
            interpolate_yaml("key: '$CONDUIT_YAML_T3'\n")
        });
        assert_eq!(out.unwrap(), "key: 'it''s a\\b'\n");
        let err = with_var("CONDUIT_YAML_T3", "x\ny", || {
            interpolate_yaml("key: '$CONDUIT_YAML_T3'\n")
        });
        assert!(err.unwrap_err().contains("CONDUIT_YAML_T3"));
    }

    #[test]
    #[serial]
    fn a_plain_value_that_would_not_read_back_is_refused_and_the_message_names_only_the_variable() {
        for (value, why) in [
            ("x: y", "colon"),
            ("x #y", "#"),
            ("x\ninjected: 1", "line break"),
            ("*alias", "indicator"),
            ("&anchor", "indicator"),
            ("!tag", "indicator"),
            ("- item", "indicator"),
            ("|", "indicator"),
            (" lead", "space"),
            ("trail ", "space"),
            ("'q", "indicator"),
        ] {
            let err = with_var("CONDUIT_YAML_T4", value, || {
                interpolate_yaml("key: $CONDUIT_YAML_T4\n")
            })
            .unwrap_err();
            assert!(
                err.contains("CONDUIT_YAML_T4") && err.contains(why),
                "{value:?}: {err}"
            );
            assert!(
                !err.contains(value),
                "the message must not carry the value: {err}"
            );
            assert!(err.contains("double quotes"), "{err}");
        }
    }

    /// Gitar on #683: after text and a blank, a value that starts with `#` would turn into a comment and the rest of
    /// the value would be lost without an error.
    #[test]
    #[serial]
    fn a_value_starting_with_a_hash_after_a_blank_is_refused() {
        let err = with_var("CONDUIT_YAML_T13", "#x", || {
            interpolate_yaml("note: a $CONDUIT_YAML_T13\n")
        })
        .unwrap_err();
        assert!(err.contains("CONDUIT_YAML_T13"), "{err}");
        // glued to the text there is no comment, so it is fine; and it is fine in quotes
        let glued = with_var("CONDUIT_YAML_T13", "#x", || {
            interpolate_yaml("note: a$CONDUIT_YAML_T13\n")
        });
        assert_eq!(glued.unwrap(), "note: a#x\n");
        let quoted = with_var("CONDUIT_YAML_T13", "#x", || {
            interpolate_yaml("note: \"a $CONDUIT_YAML_T13\"\n")
        });
        assert_eq!(quoted.unwrap(), "note: \"a #x\"\n");
    }

    #[test]
    #[serial]
    fn the_same_values_are_fine_in_double_quotes() {
        for value in [
            "x: y",
            "x #y",
            "x\ninjected: 1",
            "*alias",
            " lead",
            "trail ",
        ] {
            let out = with_var("CONDUIT_YAML_T5", value, || {
                interpolate_yaml("key: \"$CONDUIT_YAML_T5\"\n")
            });
            assert!(out.is_ok(), "{value:?}");
        }
    }

    #[test]
    #[serial]
    fn an_indicator_in_the_middle_of_a_plain_value_is_fine() {
        // only the start is special: `a*b`, `a&b`, `http://x/?q=1#frag`, `a-b`
        for value in [
            "a*b",
            "a&b",
            "http://h/p?q=1#frag",
            "a-b",
            "p@ss",
            "50%",
            "a!b",
            "a|b",
            "a>b",
        ] {
            let out = with_var("CONDUIT_YAML_T6", value, || {
                interpolate_yaml("key: x$CONDUIT_YAML_T6\n")
            });
            assert_eq!(out.unwrap(), format!("key: x{value}\n"), "{value:?}");
        }
    }

    #[test]
    #[serial]
    fn inside_a_flow_list_a_comma_or_bracket_is_refused() {
        let ok = with_var("CONDUIT_YAML_T7", "plain", || {
            interpolate_yaml("keys: [$CONDUIT_YAML_T7, b]\n")
        });
        assert_eq!(ok.unwrap(), "keys: [plain, b]\n");
        for value in ["a,b", "a]", "{a"] {
            let err = with_var("CONDUIT_YAML_T7", value, || {
                interpolate_yaml("keys: [$CONDUIT_YAML_T7, b]\n")
            });
            assert!(err.is_err(), "{value:?}");
        }
        // the same commas are fine in a block value
        let ok = with_var("CONDUIT_YAML_T7", "a,b", || {
            interpolate_yaml("keys: $CONDUIT_YAML_T7\n")
        });
        assert_eq!(ok.unwrap(), "keys: a,b\n");
    }

    #[test]
    #[serial]
    fn a_reference_in_a_comment_is_left_alone() {
        let out = with_var("CONDUIT_YAML_T8", "v", || {
            interpolate_yaml(
                "# uses $CONDUIT_YAML_T8\nkey: $CONDUIT_YAML_T8 # and $CONDUIT_YAML_T8\n",
            )
        });
        assert_eq!(
            out.unwrap(),
            "# uses $CONDUIT_YAML_T8\nkey: v # and $CONDUIT_YAML_T8\n"
        );
    }

    #[test]
    #[serial]
    fn an_apostrophe_in_a_plain_value_does_not_open_a_quote() {
        // `it's` is a plain scalar: the reference after it is plain too (a backslash stays single)
        let out = with_var("CONDUIT_YAML_T9", "a\\b", || {
            interpolate_yaml("note: it's $CONDUIT_YAML_T9\n")
        });
        assert_eq!(out.unwrap(), "note: it's a\\b\n");
    }

    #[test]
    #[serial]
    fn a_quoted_scalar_over_several_lines_stays_quoted() {
        let out = with_var("CONDUIT_YAML_T10", "q\"x", || {
            interpolate_yaml("key: \"first line\n  $CONDUIT_YAML_T10 end\"\nnext: 1\n")
        });
        assert_eq!(out.unwrap(), "key: \"first line\n  q\\\"x end\"\nnext: 1\n");
    }

    #[test]
    #[serial]
    fn a_block_scalar_takes_the_value_as_it_is_unless_it_has_a_line_break() {
        let text = "script: |\n  echo $CONDUIT_YAML_T11\nafter: $CONDUIT_YAML_T11\n";
        let ok = with_var("CONDUIT_YAML_T11", "a\\b'c\"d", || interpolate_yaml(text));
        assert_eq!(
            ok.unwrap(),
            "script: |\n  echo a\\b'c\"d\nafter: a\\b'c\"d\n"
        );
        let err = with_var("CONDUIT_YAML_T11", "a\nb: 1", || interpolate_yaml(text));
        assert!(err.is_err());
    }

    #[test]
    #[serial]
    fn json_looking_yaml_uses_the_quotes() {
        let out = with_var("CONDUIT_YAML_T12", "a\"b", || {
            interpolate_yaml("{\"k\": \"$CONDUIT_YAML_T12\"}")
        });
        assert_eq!(out.unwrap(), "{\"k\": \"a\\\"b\"}");
    }

    #[test]
    #[serial]
    fn an_unset_reference_and_a_bare_dollar_stay() {
        let out =
            interpolate_yaml("a: $CONDUIT_YAML_UNSET\nb: $\nc: $$x\nd: \"$CONDUIT_YAML_UNSET\"\n");
        assert_eq!(
            out.unwrap(),
            "a: $CONDUIT_YAML_UNSET\nb: $\nc: $$x\nd: \"$CONDUIT_YAML_UNSET\"\n"
        );
    }

    /// The property that matters: for whatever the value, either the YAML reads back the value, or the
    /// interpolation refuses; it never yields a different value or an extra key.
    #[test]
    #[serial]
    fn whatever_the_value_it_reads_back_unchanged_or_is_refused() {
        let values = [
            "plain",
            "a b",
            "a\\b",
            "a\"b",
            "a'b",
            "a: b",
            "a #b",
            "#x",
            "*x",
            "&x",
            "!x",
            "|x",
            ">x",
            "%x",
            "@x",
            "- x",
            "? x",
            "[x]",
            "{x}",
            "a,b",
            " x",
            "x ",
            "x\ny: 1",
            "x\r\ny",
            "tab\there",
            "ünï",
            "",
            "0x1F",
            "null",
            "true",
            "a: b: c",
            "-",
            "?",
            ":",
            "---",
            "...",
            "x:",
            ":x",
            "a\\nb",
            "$HOME",
            "${X}",
            // the security review of #683: the line breaks libyaml knows besides LF and CR, a question mark in a
            // list, the spellings of null, a `#` after a blank, and escapes that a double-quoted scalar would read
            "\u{2028}",
            "\u{2029}",
            "\u{85}",
            "a\u{2028}evil:\u{2028} x",
            "b\u{2028}?",
            "\r`#\t\u{85}",
            "\u{2028} >",
            "?evil",
            "?",
            "~",
            "Null",
            "NULL",
            "# c",
            "\\a'.1>",
            "\\b",
            "\\x41",
            "\\u0041",
            "\u{7f}",
        ];
        // (text, label, what stands before the reference inside the scalar, and after it)
        let templates: [(&str, &str, &str, &str); 10] = [
            ("k: $V\nz: 1\n", "plain", "", ""),
            ("k: a $V\nz: 1\n", "plain after a blank", "a ", ""),
            ("k: a$V\nz: 1\n", "plain after text", "a", ""),
            ("k: a $V b\nz: 1\n", "plain in the middle", "a ", " b"),
            ("k: \"$V\"\nz: 1\n", "double", "", ""),
            ("k: &a \"x$V\"\nz: 1\n", "double after an anchor", "x", ""),
            ("k: !!str \"x$V\"\nz: 1\n", "double after a tag", "x", ""),
            ("k: '$V'\nz: 1\n", "single", "", ""),
            ("k: [$V, 2]\nz: 1\n", "flow", "", ""),
            ("k: [a, $V]\nz: 1\n", "flow second", "", ""),
        ];
        for value in values {
            for (template, label, before, after) in templates {
                let text = template.replace("$V", "$CONDUIT_YAML_PROP");
                let result = with_var("CONDUIT_YAML_PROP", value, || interpolate_yaml(&text));
                let Ok(out) = result else { continue };
                // the keys of the result are exactly `k` and `z`
                let doc: serde_json::Value = match serde_yaml_check(&out) {
                    Some(d) => d,
                    None => continue, // YAML that does not parse is an error at load, never a changed value
                };
                let keys: Vec<&String> = doc
                    .as_object()
                    .map(|m| m.keys().collect())
                    .unwrap_or_default();
                assert_eq!(keys, ["k", "z"], "{label} {value:?} -> {out:?}");
                let read = match &doc["k"] {
                    serde_json::Value::Array(items) => {
                        let item = if label == "flow second" { 1 } else { 0 };
                        items.get(item).cloned().unwrap_or_default()
                    }
                    other => other.clone(),
                };
                // a plain scalar may be read as a number, a bool or null; when it is a string it is the value
                if let Some(s) = read.as_str() {
                    // an unquoted scalar loses blanks at its end, whatever put them there: an empty value after
                    // `a ` gives `a` (the value itself never starts or ends with a blank here: those are refused)
                    let expected = format!("{before}{value}{after}");
                    let expected = if label.starts_with("plain") {
                        expected.trim_end().to_owned()
                    } else {
                        expected
                    };
                    assert_eq!(s, expected, "{label} {value:?} -> {out:?}");
                } else {
                    // only an unquoted value may become a number or a bool (`0x1F`, `true`), and never a null (an
                    // empty or `null` secret would mean "not set"), a mapping or a list (`[?x]`)
                    assert!(
                        (label.starts_with("plain") || label.starts_with("flow"))
                            && (read.is_number() || read.is_boolean()),
                        "{label} {value:?} gave {read:?}"
                    );
                }
            }
        }
    }

    /// A parse with the reader the loader uses, as a JSON value.
    fn serde_yaml_check(text: &str) -> Option<serde_json::Value> {
        serde_yaml::from_str(text).ok()
    }

    #[test]
    #[serial]
    fn resolved_references_lists_the_set_variables_once() {
        std::env::set_var("CONDUIT_YAML_R1", "x");
        let names = resolved_references(
            "a: $CONDUIT_YAML_R1\nb: \"$CONDUIT_YAML_R1\" $CONDUIT_YAML_UNSET2 $ $$",
        );
        std::env::remove_var("CONDUIT_YAML_R1");
        assert_eq!(names, ["CONDUIT_YAML_R1"]);
        assert!(resolved_references("no references here").is_empty());
    }
}
