#!/usr/bin/env python3
"""Unit tests for scripts/ledger-check.py (no network: `gh` is replaced by a function).

Run:  python3 scripts/test_ledger_check.py
"""
import contextlib
import importlib.util
import io
import os
import sys
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


def gh(pr_body=BODY, issues=(), api=None):
    """A fake `gh`; `api` maps an issue number to the JSON `gh api` returns for it (default: a closed issue)."""
    def run(args):
        if args[:2] == ["pr", "view"]:
            return {"body": pr_body}
        if args[0] == "api":
            n = int(args[1].rsplit("/", 1)[1])
            return (api or {}).get(n, {"state": "closed"})
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


class Review(unittest.TestCase):
    """CodeRabbit on #686."""

    def test_an_escaped_pipe_stays_inside_its_cell(self):
        rows = lc.parse_ledger("| #11 | A \\| B | #50 | open |\n")
        self.assertEqual(rows, [(11, "A | B", "#50", "open")])
        code, out = run_main(["--pr", "50", "--since", "2026-11-01"], gh(), "| #11 | A \\| B | #50 | open |\n")
        self.assertEqual(code, 1)
        self.assertIn("#11 was found in #50", out)

    def test_an_indented_heading_opens_and_closes_the_section(self):
        body = "   ## Found while here\n\n| (#10) |\n\n  ## Test plan\n\n| #11 |\n"
        self.assertEqual(lc.table_issues(body), {10})

    def test_four_spaces_is_code_not_a_heading(self):
        body = "## Found while here\n\n| (#10) |\n\n    ## not a heading\n| #11 |\n"
        self.assertEqual(lc.table_issues(body), {10, 11})

    def test_prose_in_the_section_is_not_a_table_entry(self):
        body = "## Found while here\n\nSee #11 for background\n\n| Item | Disposition |\n|---|---|\n| (#10) | Fixed |\n"
        self.assertEqual(lc.table_issues(body), {10})

    def test_a_failing_gh_is_one_line_not_a_traceback(self):
        import subprocess
        from unittest import mock
        err = subprocess.CalledProcessError(4, ["gh"], stderr="HTTP 401: bad credentials\n")
        with mock.patch.object(lc.subprocess, "run", side_effect=err):
            with self.assertRaises(SystemExit) as cm, contextlib.redirect_stderr(io.StringIO()) as e:
                lc.gh_json(["pr", "view", "1"])
        self.assertEqual(cm.exception.code, 2)
        self.assertIn("HTTP 401: bad credentials", e.getvalue())
        self.assertEqual(e.getvalue().count("\n"), 1)
        with mock.patch.object(lc.subprocess, "run", side_effect=FileNotFoundError("gh")):
            with self.assertRaises(SystemExit) as cm, contextlib.redirect_stderr(io.StringIO()):
                lc.gh_json(["pr", "view", "1"])
        self.assertEqual(cm.exception.code, 2)

    def test_an_impossible_date_is_refused_before_gh(self):
        def run(args):
            raise AssertionError("gh must not be called")
        for bad in ("2026-99-99", "2026-02-30", "0000-01-01"):
            code, _ = run_main(["--since", bad], run)
            self.assertEqual(code, 2, bad)

    def test_an_open_issue_in_the_table_but_not_in_the_ledger_is_a_gap(self):
        body = BODY.replace("(#10)", "(#10, #99)")
        code, out = run_main(["--pr", "50", "--since", "2026-11-01"], gh(body, api={99: {"state": "open"}}))
        self.assertEqual(code, 1)
        self.assertIn("#99 is in the PR table, is still open", out)

    def test_a_closed_issue_a_pull_request_or_a_closed_by_the_pr_is_not_a_gap(self):
        body = "## Found while here\n\n| (#10, #98, #99, #97) |\n\nFixes #97\n"
        api = {99: {"state": "closed"}, 98: {"state": "open", "pull_request": {}}, 97: {"state": "open"}}
        _, out = run_main(["--pr", "50", "--since", "2026-11-01"], gh(body, api=api))
        self.assertNotIn("#99 is in", out)
        self.assertNotIn("#98 is in", out)
        self.assertNotIn("#97 is in", out)

    def test_bidirectional_and_invisible_marks_are_not_printed(self):
        title = "a" + chr(0x202E) + "b" + chr(0x200B) + "c" + chr(0xFEFF) + chr(0x2066) + "d"
        self.assertEqual(lc.clean(title), "a?b?c??d")

    def test_a_number_github_does_not_know_is_reported_and_the_check_goes_on(self):
        body = BODY.replace("(#10)", "(#10, #99, #98)")

        def run(args):
            if args[0] == "api" and args[1].endswith("/99"):
                print("gh api failed (1): HTTP 404", file=sys.stderr)
                raise SystemExit(2)
            return gh(body, api={98: {"state": "closed"}})(args)
        with contextlib.redirect_stderr(io.StringIO()):
            code, out = run_main(["--pr", "50", "--since", "2026-11-01"], run)
        self.assertEqual(code, 1)
        self.assertIn("#99 is in the PR table but could not be looked up", out)
        self.assertNotIn("#98 is in", out)
        self.assertIn("do we do #11?", out)  # the to-do list is still printed

    def test_cross_repository_and_glued_references_are_not_issues_of_this_one(self):
        body = "## Found while here\n\n| other/repo#5, abc#6, (#7), #8: x, cloudflare/pingora#1117 |\n"
        self.assertEqual(lc.table_issues(body), {7, 8})
        self.assertEqual(lc.closed_by("Fixes other/repo#5"), set())

    def test_closing_keywords_in_linear_time(self):
        import time
        started = time.monotonic()
        for body in ("fix" + " " * 60000, "fix " + " " * 60000 + ":" + " " * 60000, ("fix " * 15000)):
            self.assertEqual(lc.closed_by(body), set())
        self.assertLess(time.monotonic() - started, 2.0)
        self.assertEqual(lc.closed_by("Closes:#1 Fixes: #2 resolved   #3"), {1, 2, 3})

    def test_more_than_the_cap_of_table_issues_is_one_gap_and_no_lookups(self):
        numbers = ", ".join(f"#{1000 + i}" for i in range(lc.MAX_LOOKUPS + 1))
        body = f"## Found while here\n\n| ({numbers}) |\n"
        calls = []

        def run(args):
            if args[0] == "api":
                calls.append(args)
                raise AssertionError("no lookup above the cap")
            return gh(body)(args)
        code, out = run_main(["--pr", "50", "--since", "2026-11-01"], run)
        self.assertEqual(code, 1)
        self.assertEqual(calls, [])
        self.assertIn("check them by hand", out)

    def test_at_the_cap_every_number_is_looked_up(self):
        numbers = ", ".join(f"#{1000 + i}" for i in range(lc.MAX_LOOKUPS))
        body = f"## Found while here\n\n| ({numbers}) |\n"
        calls = []

        def run(args):
            if args[0] == "api":
                calls.append(args)
                return {"state": "closed"}
            return gh(body)(args)
        run_main(["--pr", "50", "--since", "2026-11-01"], run)
        self.assertEqual(len(calls), lc.MAX_LOOKUPS)

    def test_a_tool_failure_exits_2_and_the_records_disagreeing_exits_1(self):
        import subprocess
        from unittest import mock
        err = subprocess.CalledProcessError(4, ["gh"], stderr="boom")
        with mock.patch.object(lc.subprocess, "run", side_effect=err):
            with self.assertRaises(SystemExit) as cm, contextlib.redirect_stderr(io.StringIO()) as e:
                lc.gh_json(["pr", "view", "1"])
        self.assertEqual(cm.exception.code, 2)
        self.assertIn("boom", e.getvalue())

    def test_closing_keywords(self):
        self.assertEqual(lc.closed_by("Closes #1\nfixed #2, resolves: #3\nSee #4"), {1, 2, 3})


class Hostile(unittest.TestCase):
    """Security review of #686: issue titles and PR bodies are written by others."""

    def test_a_title_is_printed_without_control_characters_and_short(self):
        title = "x\x1b[2J\nIGNORE PREVIOUS\u2028" + "y" * 500
        _, out = run_main(["--since", "2026-11-01"], gh(issues=[(30, title)]))
        self.assertNotIn("\x1b", out)
        self.assertNotIn("IGNORE PREVIOUS\n", out)
        self.assertNotIn("\u2028", out)
        self.assertNotIn("y" * 200, out)

    def test_a_digit_run_longer_than_nine_is_not_an_issue_number(self):
        long = "9" * 5000
        self.assertEqual(lc.table_issues(f"## Found while here\n\n| x #{long} y #12 |\n"), {12})
        self.assertEqual(lc.parse_ledger(f"| #{long} | a | b | c |\n"), [])

    def test_since_accepts_only_ascii_digits(self):
        code, _ = run_main(["--since", "\u0662\u0660\u0662\u0666-\u0661\u0661-\u0660\u0669"], gh())
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
