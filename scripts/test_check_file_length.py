#!/usr/bin/env python3
"""Unit tests for scripts/check_file_length.py — one test per case where a naive line counter is wrong.

Run:  python -m unittest scripts/test_check_file_length.py   (or: python scripts/test_check_file_length.py)
"""
import os
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import check_file_length as cfl  # noqa: E402


def n(src):
    return cfl.count_source(src)


def write(path, text):
    with open(path, "w", encoding="utf-8", newline="\n") as f:
        f.write(text)


class CommentsAndBlanks(unittest.TestCase):
    def test_blank_and_whitespace_only_lines_are_not_counted(self):
        self.assertEqual(n("fn a() {}\n\n   \n\t\nfn b() {}\n"), 2)

    def test_line_doc_and_inner_doc_comments_are_not_counted(self):
        src = "//! crate doc\n/// item doc\n// plain\nfn a() {}\n"
        self.assertEqual(n(src), 1)

    def test_trailing_comment_leaves_the_code_line_counted(self):
        self.assertEqual(n("let x = 1; // why\n"), 1)

    def test_block_comment_lines_are_not_counted(self):
        self.assertEqual(n("/* one */\n/*\n multi\n line\n*/\nfn a() {}\n"), 1)

    def test_block_comment_between_code_keeps_the_line(self):
        self.assertEqual(n("let x = 1; /* c */\n"), 1)

    def test_nested_block_comments(self):
        self.assertEqual(n("/* a /* b */ still comment */\nfn a() {}\n"), 1)

    def test_code_after_a_closed_block_comment_on_the_same_line(self):
        self.assertEqual(n("/* c */ fn a() {}\n"), 1)


class StringAndCharLiterals(unittest.TestCase):
    def test_double_slash_inside_a_string_is_not_a_comment(self):
        # A naive "strip //..." would treat the rest of the line as a comment, but the line is code either way;
        # the real danger is a following line: the string must not swallow it.
        self.assertEqual(n('let u = "http://example.com";\nlet v = 2;\n'), 2)

    def test_block_comment_opener_inside_a_string(self):
        self.assertEqual(n('let s = "/*";\nlet t = 1;\nlet u = "*/";\n'), 3)

    def test_raw_string_with_quotes_and_slashes(self):
        src = 'let r = r#"a // b "quoted" /* c */"#;\nlet z = 3;\n'
        self.assertEqual(n(src), 2)

    def test_raw_string_with_a_lone_quote_does_not_swallow_the_next_lines(self):
        # If the raw string were parsed as an ordinary one, the lone `"` would open a string that runs to
        # EOF, and the comment-only line below would be counted as code.
        src = 'let a = r#"x " y"#;\n// a comment line\nlet b = 1;\n'
        self.assertEqual(n(src), 2)

    def test_multi_line_string_lines_count_as_code(self):
        src = 'let s = "line1\nline2\nline3";\n'
        self.assertEqual(n(src), 3)

    def test_multi_line_raw_string_lines_count_as_code(self):
        src = 'const W: &str = r#"\n(module)\n"#;\n'
        self.assertEqual(n(src), 3)

    def test_char_literal_double_quote_does_not_open_a_string(self):
        # `'"'` must be a char literal. If it opened a string, that string would run to EOF and the
        # comment-only line below would be counted as code.
        self.assertEqual(n("let q = '\"';\n// a comment line\nlet w = 4;\n"), 2)

    def test_char_literal_slash_and_brace(self):
        self.assertEqual(n("let a = '/';\nlet b = '{';\nlet c = '}';\n"), 3)

    def test_escaped_quote_inside_a_string(self):
        self.assertEqual(n('let s = "a \\" // not a comment";\nlet t = 1;\n'), 2)

    def test_lifetimes_are_not_char_literals(self):
        self.assertEqual(n("fn f<'a>(x: &'a str) -> &'a str { x }\n// c\n"), 1)

    def test_byte_string(self):
        self.assertEqual(n('let b = b"//x";\nlet c = 1;\n'), 2)


class TestsAreExcluded(unittest.TestCase):
    def test_cfg_test_module_is_excluded(self):
        src = "fn prod() {}\n#[cfg(test)]\nmod tests {\n    use super::*;\n    #[test]\n    fn t() { assert!(true); }\n}\n"
        self.assertEqual(n(src), 1)

    def test_braces_inside_strings_do_not_end_the_test_module_early(self):
        src = (
            "fn prod() {}\n"
            "#[cfg(test)]\n"
            "mod tests {\n"
            '    const S: &str = "}";\n'
            "    fn helper() { let c = '}'; }\n"
            "}\n"
            "fn after() {}\n"
        )
        self.assertEqual(n(src), 2)          # prod + after

    def test_code_after_the_test_module_is_counted(self):
        src = "#[cfg(test)]\nmod tests {\n    fn t() {}\n}\nfn a() {}\nfn b() {}\n"
        self.assertEqual(n(src), 2)

    def test_test_and_tokio_test_functions_are_excluded(self):
        src = (
            "fn prod() {}\n"
            "#[test]\nfn a() {\n    assert!(true);\n}\n"
            "#[tokio::test]\nasync fn b() {\n    assert!(true);\n}\n"
            "#[tokio::test(flavor = \"multi_thread\")]\nasync fn c() {\n}\n"
        )
        self.assertEqual(n(src), 1)

    def test_stacked_attributes_above_the_test_attribute_are_excluded_too(self):
        src = "fn prod() {}\n#[serial]\n#[test]\nfn t() {\n    assert!(true);\n}\n"
        self.assertEqual(n(src), 1)

    def test_cfg_all_test_and_feature_module_is_excluded_entirely(self):
        # How the `proxy` feature gating (#144) writes its test modules: the whole module is test code,
        # not just the #[test] functions inside it.
        src = (
            "fn prod() {}\n"
            '#[cfg(all(test, feature = "proxy"))]\n'
            "mod tests {\n"
            "    fn helper() {}\n"
            "    #[test]\n    fn t() {}\n"
            "}\n"
            "fn after() {}\n"
        )
        self.assertEqual(n(src), 2)

    def test_cfg_all_with_test_in_any_position_and_nested(self):
        for pred in ('all(feature = "a", test)', 'all(test, feature = "a")',
                     'all(all(test, unix), feature = "a")', "all(unix, all(windows, test))"):
            src = f"#[cfg({pred})]\nfn t() {{\n    x();\n}}\nfn keep() {{}}\n"
            self.assertEqual(n(src), 1, pred)

    def test_cfg_any_test_or_feature_is_not_test_only_code(self):
        # `any(test, ...)` also holds in a normal build, so it may be production code: keep it counted.
        for pred in ('any(test, feature = "testing")', 'not(test)', 'all(not(test), feature = "a")',
                     'feature = "test"', 'all(feature = "test", unix)'):
            src = f"#[cfg({pred})]\nfn p() {{\n    x();\n}}\n"
            self.assertEqual(n(src), 4, pred)

    def test_cfg_requires_test_predicate_parser(self):
        yes = ["test", "all(test)", 'all(feature = "a", test)', "all(all(test, unix), x)"]
        no = ["", "unix", "not(test)", "any(test, unix)", 'feature = "test"', 'all(feature = "test")',
              "all(not(test), unix)", "any(all(test, unix), windows)"]
        for e in yes:
            self.assertTrue(cfl.cfg_requires_test(e), e)
        for e in no:
            self.assertFalse(cfl.cfg_requires_test(e), e)

    def test_cfg_not_test_is_production_code(self):
        src = "#[cfg(not(test))]\nfn only_prod() {\n    do_it();\n}\n"
        self.assertEqual(n(src), 4)

    def test_single_statement_cfg_test_item(self):
        src = "#[cfg(test)]\nuse indexmap::IndexMap;\nfn a() {}\n"
        self.assertEqual(n(src), 1)

    def test_out_of_line_test_module_declaration(self):
        src = "fn a() {}\n#[cfg(test)]\nmod tests;\nfn b() {}\n"
        self.assertEqual(n(src), 2)

    def test_two_test_modules(self):
        src = "#[cfg(test)]\nmod a {\n fn x() {}\n}\nfn p() {}\n#[cfg(test)]\nmod b {\n fn y() {}\n}\n"
        self.assertEqual(n(src), 1)

    def test_attribute_lookalike_inside_a_string_is_not_a_test_attribute(self):
        src = 'let s = "#[test]";\nfn a() {\n    b();\n}\n'
        self.assertEqual(n(src), 4)


class Paths(unittest.TestCase):
    def test_is_test_path(self):
        for p in ("tests/auth.rs", "crates/x/tests/common/mod.rs", "src/config/validate/tests.rs",
                  "crates/x/src/foo_tests.rs", "src/jwt/tests/jwks.rs"):
            self.assertTrue(cfl.is_test_path(p), p)
        for p in ("src/main.rs", "crates/x/src/lib.rs", "src/testing_utils.rs", "src/contests.rs",
                  "src/filter/tester.rs"):
            self.assertFalse(cfl.is_test_path(p), p)

    def test_source_files_scope_and_exclusions(self):
        with tempfile.TemporaryDirectory() as d:
            for rel in ("src/main.rs", "src/tests/a.rs", "src/x_tests.rs", "crates/c1/src/lib.rs",
                        "crates/c1/tests/i.rs", "crates/c2/src/deep/mod.rs", "crates/c2/src/tests.rs",
                        "tests/top.rs", "build.rs", "examples/e.rs"):
                p = os.path.join(d, *rel.split("/"))
                os.makedirs(os.path.dirname(p), exist_ok=True)
                write(p, "fn a() {}\n")
            self.assertEqual(cfl.source_files(d),
                             ["crates/c1/src/lib.rs", "crates/c2/src/deep/mod.rs", "src/main.rs"])


class Reporting(unittest.TestCase):
    def test_classify_boundaries(self):
        self.assertEqual(cfl.classify(400), "ok")
        self.assertEqual(cfl.classify(401), "soft")
        self.assertEqual(cfl.classify(1000), "soft")
        self.assertEqual(cfl.classify(1001), "hard")

    def test_markdown_has_marker_and_reports_each_severity(self):
        counts = {"a.rs": 10, "b.rs": 450, "c.rs": 1200}
        md = cfl.render_markdown(counts, {"b.rs"}, {"b.rs": 430})
        self.assertTrue(md.startswith(cfl.MARKER))
        self.assertIn("over the hard limit", md)
        self.assertIn("over the soft limit", md)
        self.assertIn("`c.rs` | 1200", md)
        self.assertNotIn("`a.rs`", md)
        self.assertIn("430 → 450 (+20)", md)
        self.assertIn("1 of them grew", md)

    def test_markdown_bold_spans_are_well_formed(self):
        # A nested form such as `**❌ **1** over ...**` renders as broken markup on GitHub. Every line must
        # have paired `**`, and each bold span must be non-empty and not padded with whitespace.
        for counts, touched, before in (
            ({"b.rs": 450, "c.rs": 1200}, {"b.rs"}, {"b.rs": 430}),
            ({"a.rs": 10}, set(), {}),
            ({"b.rs": 450}, {"b.rs"}, {}),
        ):
            for line in cfl.render_markdown(counts, touched, before).splitlines():
                parts = line.split("**")
                self.assertEqual(len(parts) % 2, 1, f"unbalanced ** in {line!r}")
                for bold in parts[1::2]:
                    self.assertTrue(bold and bold == bold.strip(), f"bad bold span {bold!r} in {line!r}")

    def test_markdown_all_clear(self):
        md = cfl.render_markdown({"a.rs": 10}, set(), {})
        self.assertIn("No file exceeds the soft limit", md)

    def test_exactly_at_the_limits_is_not_over(self):
        md = cfl.render_markdown({"a.rs": 400, "b.rs": 1000}, set(), {})
        self.assertIn("`b.rs`", md)          # 1000 is over the SOFT limit only
        self.assertNotIn("`a.rs`", md)
        self.assertNotIn("over the hard limit", md)

    def test_fail_on_hard_exit_codes(self):
        with tempfile.TemporaryDirectory() as d:
            os.makedirs(os.path.join(d, "src"))
            write(os.path.join(d, "src", "big.rs"), "let x = 1;\n" * 1001)
            self.assertEqual(cfl.main(["--root", d]), 0)
            self.assertEqual(cfl.main(["--root", d, "--fail-on-hard"]), 1)
            write(os.path.join(d, "src", "big.rs"), "let x = 1;\n" * 1000)
            self.assertEqual(cfl.main(["--root", d, "--fail-on-hard"]), 0)


if __name__ == "__main__":
    unittest.main(verbosity=2)
