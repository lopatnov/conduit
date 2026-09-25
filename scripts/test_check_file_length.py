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

    def test_markdown_names_the_largest_touched_file(self):
        counts = {"a.rs": 10, "b.rs": 120, "c.rs": 900}
        md = cfl.render_markdown(counts, {"a.rs", "b.rs", "tests_only.rs"}, {})
        # tests_only.rs is not in `counts` (a test file), so only two touched files are counted
        self.assertIn("2 Rust files touched by this PR (tests excluded); the largest is `b.rs` at **120**", md)
        self.assertNotIn("touched by this PR;", cfl.render_markdown(counts, set(), {}))

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


class Hardening(unittest.TestCase):
    """Cases found by the security review of the script (PR #460)."""

    def test_valid_ref_rejects_option_lookalikes_and_control_characters(self):
        for bad in ("", "-x", "--output=/tmp/x", "--output=PATH", "a\nb", "a\rb", "a\0b"):
            self.assertFalse(cfl.valid_ref(bad), repr(bad))
        for good in ("origin/main", "abc1234", "HEAD~3", "feature/x-y", "0456654d7b0b"):
            self.assertTrue(cfl.valid_ref(good), good)

    def test_base_that_looks_like_a_git_option_is_refused(self):
        # `--base=--output=PATH` is one argv element for argparse, and git would read it as an option.
        with tempfile.TemporaryDirectory() as d:
            os.makedirs(os.path.join(d, "src"))
            with self.assertRaises(SystemExit) as cm:
                cfl.main(["--root", d, "--base=--output=" + os.path.join(d, "PWNED")])
            self.assertEqual(cm.exception.code, 2)
            self.assertFalse(os.path.exists(os.path.join(d, "PWNED")))
            self.assertIsNone(cfl.changed_files(d, "--output=" + os.path.join(d, "PWNED")))

    def test_changed_files_is_none_when_git_cannot_diff(self):
        with tempfile.TemporaryDirectory() as d:          # not a git repository
            self.assertIsNone(cfl.changed_files(d, "origin/main"))

    def test_report_says_so_when_touched_files_cannot_be_determined(self):
        with tempfile.TemporaryDirectory() as d:
            os.makedirs(os.path.join(d, "src"))
            write(os.path.join(d, "src", "a.rs"), "let x = 1;\n")
            out = os.path.join(d, "report.md")
            self.assertEqual(cfl.main(["--root", d, "--base", "origin/main", "--markdown", out]), 0)
            with open(out, encoding="utf-8") as f:
                md = f.read()
        self.assertIn("could not be determined", md)
        self.assertIn("could not diff against `origin/main`", md)

    def test_non_utf8_source_does_not_crash(self):
        with tempfile.TemporaryDirectory() as d:
            os.makedirs(os.path.join(d, "src"))
            with open(os.path.join(d, "src", "bad.rs"), "wb") as f:
                f.write(b"let a = 1;\n\xff\xfe let b = 2;\n")
            self.assertEqual(cfl.main(["--root", d]), 0)

    def test_unreadable_file_is_skipped_with_a_warning(self):
        import builtins
        from unittest import mock
        real_open = builtins.open

        def fake_open(path, *a, **kw):
            if str(path).endswith("unreadable.rs"):
                raise PermissionError("denied")
            return real_open(path, *a, **kw)

        with tempfile.TemporaryDirectory() as d:
            os.makedirs(os.path.join(d, "src"))
            write(os.path.join(d, "src", "ok.rs"), "let a = 1;\n")
            write(os.path.join(d, "src", "unreadable.rs"), "let b = 2;\n")
            with mock.patch("builtins.open", fake_open):
                counts, _, _, _ = cfl.build_report(d)
        self.assertEqual(list(counts), ["src/ok.rs"])

    def test_absurdly_nested_cfg_does_not_raise(self):
        line = "#[cfg(" + "all(" * 5000 + "test" + ")" * 5000 + ")]"
        self.assertFalse(cfl.is_test_attr(line))

    def test_warning_for_a_test_attribute_on_a_non_item(self):
        seen = []
        cfl.count_source("struct S {\n    #[cfg(test)]\n    field: u8,\n    other: u8,\n}\n",
                         lambda ln, msg: seen.append(ln))
        self.assertEqual(seen, [2])

    def test_no_warning_for_a_test_attribute_on_an_item(self):
        seen = []
        src = ("#[cfg(test)]\nmod tests {\n    fn t() {}\n}\n"
               "#[cfg(test)]\nuse a::b;\n"
               "#[cfg(all(test, feature = \"x\"))]\npub(crate) async fn f() {}\n"
               "#[test]\n#[should_panic]\nfn t2() {}\n")
        cfl.count_source(src, lambda ln, msg: seen.append((ln, msg)))
        self.assertEqual(seen, [])

    def test_paths_are_inert_in_the_markdown_comment(self):
        self.assertEqual(cfl.md_path("src/a`b|c\nd.rs"), "src/a_b_c_d.rs")
        md = cfl.render_markdown({"src/we`ird|name.rs": 500}, {"src/we`ird|name.rs"}, {})
        self.assertNotIn("we`ird", md)
        self.assertNotIn("we|ird", md)
        self.assertIn("`src/we_ird_name.rs`", md)


class MultiLineAttributes(unittest.TestCase):
    """rustfmt wraps long attributes; the repo already has 12 multi-line ones (none about `test` yet)."""

    def test_wrapped_cfg_all_test_module_is_excluded(self):
        src = (
            "fn prod() {}\n"
            "#[cfg(all(\n"
            "    test,\n"
            '    feature = "proxy"\n'
            "))]\n"
            "mod tests {\n"
            "    fn helper() {}\n"
            "}\n"
            "fn after() {}\n"
        )
        self.assertEqual(n(src), 2)

    def test_wrapped_cfg_any_test_is_production_code(self):
        src = '#[cfg(any(\n    test,\n    feature = "testing"\n))]\nfn p() {\n    x();\n}\n'
        self.assertEqual(n(src), 7)

    def test_wrapped_test_attribute_on_a_fn(self):
        src = 'fn prod() {}\n#[tokio::test(\n    flavor = "multi_thread"\n)]\nasync fn t() {\n    x();\n}\nfn after() {}\n'
        self.assertEqual(n(src), 2)

    def test_stacked_wrapped_attributes_above_the_test_attribute_go_with_it(self):
        src = (
            "fn prod() {}\n"
            "#[serial(\n    db\n)]\n"
            "#[cfg(all(\n    test,\n    unix\n))]\n"
            "#[allow(dead_code)]\n"
            "fn t() {\n    x();\n}\n"
            "fn after() {}\n"
        )
        self.assertEqual(n(src), 2)

    def test_a_wrapped_non_test_attribute_is_counted_as_code(self):
        src = '#[serde(\n    rename = "x",\n    skip_serializing_if = "Option::is_none"\n)]\npub field: Option<u8>,\n'
        self.assertEqual(n(src), 5)

    def test_attribute_with_its_item_on_the_same_line_is_code(self):
        self.assertEqual(n("#[derive(Debug)] struct S;\n#[cfg(test)]\nmod t {}\n"), 1)

    def test_wrapped_test_attribute_on_a_non_item_still_warns(self):
        seen = []
        cfl.count_source("struct S {\n    #[cfg(all(\n        test,\n        unix\n    ))]\n    f: u8,\n}\n",
                         lambda ln, msg: seen.append(ln))
        self.assertEqual(seen, [2])


def _git(cwd, *args):
    import subprocess
    subprocess.run(["git", "-c", "user.name=t", "-c", "user.email=t@t", "-C", cwd, *args],
                   check=True, capture_output=True, text=True)


class BaseComparison(unittest.TestCase):
    """`--base`: which files a branch touched and what they were before, on a real temporary repository."""

    def _repo(self, d):
        _git(d, "init", "-q")
        os.makedirs(os.path.join(d, "src"))
        body = "".join(f"fn f{i}() {{ let x = {i}; }}\n" for i in range(50))
        write(os.path.join(d, "src", "old_name.rs"), body)
        write(os.path.join(d, "src", "keep.rs"), "fn k() {}\n")
        _git(d, "add", "-A")
        _git(d, "commit", "-q", "-m", "base")
        return body

    def test_rename_added_and_modified_files(self):
        import subprocess
        with tempfile.TemporaryDirectory() as d:
            body = self._repo(d)
            base = subprocess.run(["git", "-C", d, "rev-parse", "HEAD"], capture_output=True, text=True).stdout.strip()
            os.rename(os.path.join(d, "src", "old_name.rs"), os.path.join(d, "src", "new_name.rs"))
            write(os.path.join(d, "src", "new_name.rs"), body + "fn extra() {}\n")     # renamed AND grown by 1
            write(os.path.join(d, "src", "added.rs"), "fn a() {}\n")
            write(os.path.join(d, "src", "keep.rs"), "fn k() {}\nfn k2() {}\n")
            _git(d, "add", "-A")
            _git(d, "commit", "-q", "-m", "work")

            changed = cfl.changed_files(d, base)
            self.assertEqual(changed, {"src/new_name.rs": "src/old_name.rs", "src/added.rs": None,
                                       "src/keep.rs": "src/keep.rs"})

            counts, touched, before, note = cfl.build_report(d, base)
            self.assertIsNone(note)
            self.assertEqual(touched, {"src/new_name.rs", "src/added.rs", "src/keep.rs"})
            self.assertEqual(before["src/new_name.rs"], 50)         # what the file was under its OLD name
            self.assertEqual(counts["src/new_name.rs"], 51)
            self.assertIsNone(before["src/added.rs"])               # a genuinely new file
            self.assertEqual(before["src/keep.rs"], 1)

    def test_count_at_reports_a_failed_git_show_on_stderr(self):
        import contextlib
        import io
        with tempfile.TemporaryDirectory() as d:
            self._repo(d)
            err = io.StringIO()
            with contextlib.redirect_stderr(err):
                self.assertIsNone(cfl.count_at(d, "HEAD", "src/does_not_exist.rs"))
        self.assertIn("git show HEAD:src/does_not_exist.rs failed", err.getvalue())


class WorkspaceLayout(unittest.TestCase):
    def test_every_workspace_member_lives_where_the_script_looks(self):
        """The script scans `src/` and `crates/*/src/`. A workspace member anywhere else would be invisible to
        the report, so adding one must fail here until `source_files` learns about it."""
        import re
        cargo = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "Cargo.toml")
        with open(cargo, encoding="utf-8") as f:
            text = f.read()
        section = re.search(r"^\[workspace\]\s*$(.*?)(?=^\[)", text, re.S | re.M)
        self.assertIsNotNone(section, "no [workspace] section found in Cargo.toml")
        members = re.search(r"members\s*=\s*\[(.*?)\]", section.group(1), re.S)
        self.assertIsNotNone(members, "no `members` in [workspace]")
        entries = re.findall(r'"([^"]+)"', members.group(1))
        self.assertTrue(entries)
        for e in entries:
            self.assertEqual(e, "crates/*", f"workspace member {e!r} is outside the layout check_file_length.py scans")


if __name__ == "__main__":
    unittest.main(verbosity=2)
