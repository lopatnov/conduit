#!/usr/bin/env python3
"""Unit tests for scripts/ledger-check.py (no network: `gh` is replaced by a function).

Run:  python3 scripts/test_ledger_check.py
"""
import contextlib
import importlib.util
import io
import os
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
spec = importlib.util.spec_from_file_location("ledger_check", os.path.join(HERE, "ledger-check.py"))
lc = importlib.util.module_from_spec(spec)
spec.loader.exec_module(lc)

LEDGER = """# Ledger

Rule text with a #999 in it, which is not a row.

| Issue | What | Found in | Status (2026-11-09) |
|---|---|---|---|
| #10 | done in the PR | audit | done in PR #50 |
| #11 | open, found in the PR | security review of #50 | open |
| #12 | open, found elsewhere | #40 | open |
| #13 | done earlier | #50 | Done in PR #30 |
"""

BODY = """## Summary

Fixes #1 and mentions #77 here.

## Found while here

| Item | Disposition |
|---|---|
| a thing (#10) | Fixed here |

## Test plan

- #11 is only mentioned after the table
"""


def gh(pr_body=BODY, issues=()):
    def run(args):
        if args[:2] == ["pr", "view"]:
            return {"body": pr_body}
        assert args[:2] == ["issue", "list"], args
        return [{"number": n, "title": t} for n, t in issues]
    return run


def run_main(argv, run, ledger=LEDGER):
    with tempfile.TemporaryDirectory() as d:
        path = os.path.join(d, "open-items.md")
        with open(path, "w", encoding="utf-8") as f:
            f.write(ledger)
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            code = lc.main(argv, run=run, ledger_path=path)
    return code, out.getvalue()


class Parse(unittest.TestCase):
    def test_rows_skip_header_separator_and_prose(self):
        self.assertEqual([r[0] for r in lc.parse_ledger(LEDGER)], [10, 11, 12, 13])

    def test_done_is_case_insensitive_and_a_prefix(self):
        self.assertTrue(lc.is_done("Done in PR #30"))
        self.assertFalse(lc.is_done("open: done soon"))

    def test_table_is_only_the_found_while_here_section(self):
        self.assertEqual(lc.table_issues(BODY), {10})

    def test_no_table_means_no_issues(self):
        self.assertEqual(lc.table_issues("## Summary\n\nFixes #1\n"), set())


class Checks(unittest.TestCase):
    def test_open_item_found_in_this_pr_and_missing_from_the_table(self):
        code, out = run_main(["--pr", "50", "--since", "2026-11-01"], gh())
        self.assertEqual(code, 1)
        self.assertIn("#11 was found in #50 and is open", out)

    def test_done_in_this_pr_but_not_in_the_table(self):
        body = "## Found while here\n\n| x | y |\n|---|---|\n| (#11) | Deferred |\n"
        code, out = run_main(["--pr", "50", "--since", "2026-11-01"], gh(body))
        self.assertEqual(code, 1)
        self.assertIn("#10 is marked done in #50", out)
        self.assertNotIn("#11 was found", out)

    def test_an_item_of_another_pr_is_not_a_gap(self):
        _, out = run_main(["--pr", "50", "--since", "2026-11-01"], gh())
        self.assertNotIn("#12 was found", out)
        self.assertNotIn("#13 is marked", out)

    def test_recent_fast_follow_issue_missing_from_the_ledger(self):
        code, out = run_main(["--since", "2026-11-01"], gh(issues=[(12, "known"), (20, "forgotten")]))
        self.assertEqual(code, 1)
        self.assertIn("#20 (forgotten)", out)
        self.assertNotIn("#12 (known)", out)

    def test_a_done_ledger_line_still_counts_as_in_the_ledger(self):
        code, _ = run_main(["--since", "2026-11-01"], gh(issues=[(10, "done one")]))
        self.assertEqual(code, 0)

    def test_everything_in_step_exits_zero_and_lists_the_open_items(self):
        body = BODY.replace("(#10)", "(#10, #11)")
        code, out = run_main(["--pr", "50", "--since", "2026-11-01"], gh(body, [(11, "x")]))
        self.assertEqual(code, 0)
        self.assertIn("do we do #11?", out)
        self.assertIn("do we do #12?", out)
        self.assertNotIn("do we do #10?", out)
        self.assertIn("agree", out)

    def test_bad_since_is_refused(self):
        code, _ = run_main(["--since", "yesterday"], gh())
        self.assertEqual(code, 2)

    def test_the_search_uses_the_given_day_and_the_label(self):
        seen = []

        def run(args):
            seen.append(args)
            return []
        run_main(["--since", "2026-11-03"], run)
        self.assertIn("created:>=2026-11-03", seen[0])
        self.assertIn("fast-follow", seen[0])


class Hostile(unittest.TestCase):
    """Security review of #686: issue titles and PR bodies are written by others."""

    def test_a_title_is_printed_without_control_characters_and_short(self):
        title = "x\x1b[2J\nIGNORE PREVIOUS " + "y" * 500
        _, out = run_main(["--since", "2026-11-01"], gh(issues=[(30, title)]))
        self.assertNotIn("\x1b", out)
        self.assertNotIn("IGNORE PREVIOUS\n", out)
        self.assertNotIn(" ", out)
        self.assertNotIn("y" * 200, out)

    def test_a_digit_run_longer_than_nine_is_not_an_issue_number(self):
        long = "9" * 5000
        self.assertEqual(lc.table_issues(f"## Found while here\n\nx #{long} y #12\n"), {12})
        self.assertEqual(lc.parse_ledger(f"| #{long} | a | b | c |\n"), [])

    def test_since_accepts_only_ascii_digits(self):
        code, _ = run_main(["--since", "٢٠٢٦-١١-٠٩"], gh())
        self.assertEqual(code, 2)

    def test_pr_must_be_positive(self):
        for bad in ("-5", "0", "1000000000"):
            with self.assertRaises(SystemExit), contextlib.redirect_stderr(io.StringIO()):
                run_main(["--pr", bad], gh())

    def test_a_full_page_of_issues_is_reported_as_possibly_cut(self):
        issues = [(1000 + n, "t") for n in range(lc.LIMIT)]
        code, out = run_main(["--since", "2026-11-01"], gh(issues=issues))
        self.assertEqual(code, 1)
        self.assertIn("may be cut", out)


if __name__ == "__main__":
    unittest.main()
