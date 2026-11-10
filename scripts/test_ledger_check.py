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
        found = mock.patch.object(lc, "find_gh", return_value="/x/gh")
        found.start()
        self.addCleanup(found.stop)
        err = subprocess.CalledProcessError(4, ["gh"], stderr="HTTP 401: bad credentials\n")
        with mock.patch.object(lc.subprocess, "run", side_effect=err):
            with self.assertRaises(lc.GhError) as cm:
                lc.gh_json(["pr", "view", "1"])
        self.assertIn("HTTP 401: bad credentials", str(cm.exception))
        self.assertNotIn("\n", str(cm.exception))
        self.assertFalse(cm.exception.missing)
        err404 = subprocess.CalledProcessError(1, ["gh"], stderr="gh: Not Found (HTTP 404)\n")
        with mock.patch.object(lc.subprocess, "run", side_effect=err404):
            with self.assertRaises(lc.GhError) as cm:
                lc.gh_json(["api", "x"])
        self.assertTrue(cm.exception.missing)
        with mock.patch.object(lc.subprocess, "run", side_effect=FileNotFoundError("gh")):
            with self.assertRaises(lc.GhError):
                lc.gh_json(["pr", "view", "1"])

    def test_main_exits_2_for_every_gh_failure_and_prints_one_line(self):
        def run(args):
            raise lc.GhError("gh pr view failed (4): HTTP 401: bad credentials")
        err = io.StringIO()
        with contextlib.redirect_stderr(err):
            code, _ = run_main(["--pr", "50", "--since", "2026-11-01"], run)
        self.assertEqual(code, 2)
        self.assertEqual(err.getvalue().count("\n"), 1)
        self.assertIn("bad credentials", err.getvalue())

    def test_a_lookup_that_fails_for_another_reason_than_404_exits_2(self):
        body = BODY.replace("(#10)", "(#10, #99)")

        def run(args):
            if args[0] == "api":
                raise lc.GhError("gh api failed (1): HTTP 401: bad credentials")
            return gh(body)(args)
        with contextlib.redirect_stderr(io.StringIO()):
            code, _ = run_main(["--pr", "50", "--since", "2026-11-01"], run)
        self.assertEqual(code, 2)

    def test_an_unexpected_reply_shape_exits_2_not_with_a_traceback(self):
        replies = {
            "pr": [1, 2],
            "pr-no-body": {"other": 1},
            "list": {"not": "a list"},
            "list-item": [{"number": "x", "title": 3}],
            "api": ["not", "a", "mapping"],
        }
        body = BODY.replace("(#10)", "(#10, #99)")
        for name, bad in replies.items():
            def run(args, bad=bad, name=name):
                if args[:2] == ["pr", "view"]:
                    return bad if name.startswith("pr") else {"body": body}
                if args[:2] == ["issue", "list"]:
                    return bad if name.startswith("list") else []
                return bad
            with contextlib.redirect_stderr(io.StringIO()):
                code, _ = run_main(["--pr", "50", "--since", "2026-11-01"], run)
            # a PR reply without a body is read as an empty description, everything else is a tool failure
            self.assertEqual(code, 1 if name == "pr-no-body" else 2, name)

    def test_an_impossible_date_is_refused_before_gh(self):
        def run(args):
            raise AssertionError("gh must not be called")
        for bad in ("2026-99-99", "2026-02-30", "0000-01-01"):
            code, _ = run_main(["--since", bad], run)
            self.assertEqual(code, 2, bad)

    def test_an_open_issue_in_the_table_but_not_in_the_ledger_is_a_gap(self):
        body = BODY.replace("(#10)", "(#10, #11, #99)")
        code, out = run_main(["--pr", "50", "--since", "2026-11-01"], gh(body, api={99: {"state": "open"}}))
        self.assertEqual(code, 1)
        self.assertIn("#99 is in the PR table, is still open", out)
        self.assertEqual(out.count("\n  - "), 1, out)  # and it is the only gap

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
        body = BODY.replace("(#10)", "(#10, #11, #99, #98)")

        def run(args):
            if args[0] == "api" and args[1].endswith("/99"):
                raise lc.GhError("gh api failed (1): HTTP 404", missing=True)
            return gh(body, api={98: {"state": "closed"}})(args)
        with contextlib.redirect_stderr(io.StringIO()):
            code, out = run_main(["--pr", "50", "--since", "2026-11-01"], run)
        self.assertEqual(code, 1)
        self.assertIn("#99 is in the PR table but GitHub has no such issue here", out)
        self.assertNotIn("#98 is in", out)
        self.assertEqual(out.count("\n  - "), 1, out)  # #99 is the only gap
        self.assertIn("do we do #11?", out)  # the to-do list is still printed

    def test_cross_repository_and_glued_references_are_not_issues_of_this_one(self):
        body = "## Found while here\n\n| other/repo#5, abc#6, (#7), #8: x, cloudflare/pingora#1117 |\n"
        self.assertEqual(lc.table_issues(body), {7, 8})
        self.assertEqual(lc.closed_by("Fixes other/repo#5"), set())

    def test_closing_keywords_in_linear_time(self):
        import time
        started = time.monotonic()
        # 150,000 blanks: the old quadratic pattern needs about a minute here, the linear one a few hundredths of a second
        for body in ("fix" + " " * 150000, "fix " + " " * 150000 + ":" + " " * 150000, ("fix " * 15000)):
            self.assertEqual(lc.closed_by(body), set())
        self.assertLess(time.monotonic() - started, 5.0)
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

    def test_closing_keywords(self):
        self.assertEqual(lc.closed_by("Closes #1\nfixed #2, resolves: #3\nSee #4"), {1, 2, 3})


class Hardening(unittest.TestCase):
    """Issue #697, from the security review of #686."""

    def setUp(self):
        from unittest import mock
        found = mock.patch.object(lc, "find_gh", return_value="/x/gh")
        found.start()
        self.addCleanup(found.stop)

    def test_gh_is_never_taken_from_the_working_directory(self):
        name = "gh.exe" if os.name == "nt" else "gh"
        # the real function (setUp replaced the module's), with the planted copy first and the real one last
        real = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(real)
        with tempfile.TemporaryDirectory() as work, tempfile.TemporaryDirectory() as good:
            for d in (work, good):
                path = os.path.join(d, name)
                with open(path, "w") as f:
                    f.write("x")
                os.chmod(path, 0o755)
            entries = os.pathsep.join(["", ".", work, good])
            self.assertEqual(real.find_gh(entries, cwd=work), os.path.abspath(os.path.join(good, name)))
            self.assertIsNone(real.find_gh(os.pathsep.join(["", ".", work]), cwd=work))
            self.assertIsNone(real.find_gh("", cwd=work))

    def test_gh_not_found_is_a_gh_error(self):
        from unittest import mock
        with mock.patch.object(lc, "find_gh", return_value=None):
            with self.assertRaises(lc.GhError) as cm:
                lc.gh_json(["pr", "view", "1"])
        self.assertIn("not found", str(cm.exception))

    def test_a_hung_gh_times_out_as_a_gh_error(self):
        import subprocess
        from unittest import mock
        with mock.patch.object(lc.subprocess, "run", side_effect=subprocess.TimeoutExpired(["gh"], lc.GH_TIMEOUT)) as run:
            with self.assertRaises(lc.GhError) as cm:
                lc.gh_json(["pr", "view", "1"])
        self.assertIn("no answer", str(cm.exception))
        # and the call is made with a timeout at all
        done = subprocess.CompletedProcess(["gh"], 0, stdout="{}")
        with mock.patch.object(lc.subprocess, "run", return_value=done) as run:
            lc.gh_json(["pr", "view", "1"])
        self.assertEqual(run.call_args.kwargs["timeout"], lc.GH_TIMEOUT)

    def test_a_deeply_nested_reply_is_a_gh_error_not_a_traceback(self):
        import subprocess
        from unittest import mock
        done = subprocess.CompletedProcess(["gh"], 0, stdout="[" * 200000)
        with mock.patch.object(lc.subprocess, "run", return_value=done):
            with self.assertRaises(lc.GhError):
                lc.gh_json(["pr", "view", "1"])

    def test_a_reference_glued_to_a_word_in_the_ledger_is_reported(self):
        ledger = ("| #11 | a | found in PR#50 | open |\n"
                  "| #12 | b | #40 | waits for cloudflare/pingora#1117 |\n"
                  "| #13 | c | #41 | done in PR #42 |\n")
        self.assertEqual(lc.glued_references(lc.parse_ledger(ledger)), [(11, "PR#50")])
        code, out = run_main(["--since", "2026-11-01"], gh(), ledger)
        self.assertEqual(code, 1)
        self.assertIn('#11: "PR#50" has no space', out)
        self.assertNotIn("#12:", out)
        self.assertNotIn("#13:", out)


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
