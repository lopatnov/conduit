//! The API tables of `docs/rhai.md` must list exactly what the two engines register (issue #566), so a function
//! added, renamed or removed in `script.rs` without the guide cannot go unnoticed.
//!
//! A table is marked with an HTML comment on the line before it, invisible in the rendered page:
//!
//! ```text
//! <!-- docs-check: api request -->    (or `api response`: the phase the table describes)
//! ```
//!
//! The first cell of every row is `` `object.member` `` for a property or `` `object.member(...)` `` for a method.
//! The guide lives outside this crate, so when it is not there (the crate was unpacked on its own) the test says so
//! and passes.

use std::collections::BTreeSet;
use std::path::Path;

/// One member of an object a script can use: (object variable, member name, is a method).
type Member = (String, String, bool);

/// The object a registered Rust type is known as in each phase: (phase, variable).
fn variable_of(type_name: &str) -> Option<(&'static str, &'static str)> {
    match type_name {
        "Request" => Some(("request", "request")),
        "Response" => Some(("request", "response")),
        "ResponseBuilder" => Some(("response", "response")),
        "UpstreamView" => Some(("response", "upstream")),
        _ => None,
    }
}

/// Members from Rhai's own signature list: `get$path(_: &mut Request) -> string`, `header(_: &mut Request, _: string)`.
fn members(signatures: &[String], phase: &str) -> BTreeSet<Member> {
    let mut found = BTreeSet::new();
    for signature in signatures {
        let (name, rest) = signature
            .split_once('(')
            .expect("a signature has a parameter list");
        let Some(first) = rest.strip_prefix("_: &mut ") else {
            continue;
        };
        let type_name = first.split([',', ')']).next().unwrap_or_default();
        let Some((owner_phase, variable)) = variable_of(type_name) else {
            continue;
        };
        assert_eq!(
            owner_phase, phase,
            "{type_name} is registered on the {phase} engine"
        );
        let (member, is_method) = match name.split_once('$') {
            Some((_, property)) => (property, false), // `get$path` / `set$status`
            None => (name, true),
        };
        found.insert((variable.to_owned(), member.to_owned(), is_method));
    }
    found
}

/// The members the guide documents for `phase`, from the tables marked for it.
fn documented(markdown: &str, phase: &str) -> BTreeSet<Member> {
    let marker = format!("<!-- docs-check: api {phase} -->");
    let mut found = BTreeSet::new();
    let mut in_table = false;
    let mut header_rows_left = 0;
    for line in markdown.lines() {
        if line.trim() == marker {
            (in_table, header_rows_left) = (true, 2); // the column titles and the `---` line
            continue;
        }
        if !in_table {
            continue;
        }
        if !line.starts_with('|') {
            in_table = false;
            continue;
        }
        if header_rows_left > 0 {
            header_rows_left -= 1;
            continue;
        }
        let cell = line
            .trim_start_matches('|')
            .split('|')
            .next()
            .unwrap_or_default()
            .trim();
        let cell = cell.trim_matches('`');
        let (variable, member) = cell
            .split_once('.')
            .expect("the first cell is `object.member`");
        let is_method = member.contains('(');
        let name = member.split('(').next().unwrap_or_default();
        found.insert((variable.to_owned(), name.to_owned(), is_method));
    }
    found
}

fn difference(left: &BTreeSet<Member>, right: &BTreeSet<Member>) -> Vec<String> {
    left.difference(right)
        .map(|(variable, member, is_method)| {
            format!(
                "{variable}.{member}{}",
                if *is_method { "(...)" } else { "" }
            )
        })
        .collect()
}

#[test]
fn the_api_tables_of_the_guide_list_exactly_what_the_engines_register() {
    let guide = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/rhai.md");
    let Ok(markdown) = std::fs::read_to_string(&guide) else {
        eprintln!("docs/rhai.md is not next to this crate: nothing to check");
        return;
    };
    let (request_signatures, response_signatures) = crate::script::registered_signatures();
    let mut problems = Vec::new();
    for (phase, signatures) in [
        ("request", &request_signatures),
        ("response", &response_signatures),
    ] {
        let registered = members(signatures, phase);
        let documented = documented(&markdown, phase);
        assert!(
            !registered.is_empty() && !documented.is_empty(),
            "nothing found for the {phase} phase"
        );
        for missing in difference(&registered, &documented) {
            problems.push(format!(
                "{phase} phase: {missing} is registered but not in the guide"
            ));
        }
        for extra in difference(&documented, &registered) {
            problems.push(format!("{phase} phase: {extra} is in the guide but not registered (or is a property where it is a method, or the reverse)"));
        }
    }
    assert!(
        problems.is_empty(),
        "docs/rhai.md and the engines disagree:\n{}",
        problems.join("\n")
    );
}

#[test]
fn the_parsers_read_signatures_and_tables() {
    let signatures = vec![
        "get$path(_: &mut Request) -> string".to_owned(),
        "header(_: &mut Request, _: string) -> string".to_owned(),
        "set$status(_: &mut Response, _: i64)".to_owned(),
        "get$status(_: &mut Response) -> i64".to_owned(),
        "to_string(_: &mut Other) -> string".to_owned(),
    ];
    let found = members(&signatures, "request");
    let expected: BTreeSet<Member> = [
        ("request", "path", false),
        ("request", "header", true),
        ("response", "status", false),
    ]
    .into_iter()
    .map(|(v, m, k)| (v.to_owned(), m.to_owned(), k))
    .collect();
    assert_eq!(found, expected);

    let md = "<!-- docs-check: api request -->\n| Property | Type |\n| --- | --- |\n| `request.path` | `String` |\n| `request.header(\"Name\")` | `String` |\n\ntext\n| `response.status` | not in a marked table |\n";
    let found = documented(md, "request");
    let expected: BTreeSet<Member> = [("request", "path", false), ("request", "header", true)]
        .into_iter()
        .map(|(v, m, k)| (v.to_owned(), m.to_owned(), k))
        .collect();
    assert_eq!(found, expected);
}
