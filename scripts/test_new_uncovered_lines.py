#!/usr/bin/env python3
"""Unit tests for scripts/new_uncovered_lines.py, mostly on a real temporary repository.

Run:  python3 scripts/test_new_uncovered_lines.py
"""
import contextlib
import io
import os
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import new_uncovered_lines as nul  # noqa: E402

PROPERTIES = "\n".join([
    "# comment",
    "sonar.exclusions=target/**",
    "sonar.coverage.exclusions=\\",
    "  tests/**,\\",
    "  src/excluded.rs,\\",
    "  crates/*/src/glue.rs",
    "sonar.other=1",
]) + "\n"

# The repository: a base commit, then one commit that edits, adds and deletes files.
BASE = {
    "sonar-project.properties": PROPERTIES,
    "src/a.rs": "l1\nl2\nl3\nl4\nl5\nl6\nl7\n",
    "src/gone.rs": "gone\n",
    "src/untouched.rs": "u1\n",
    "src/lookalike.rs": "a\nb\nc\nd\ne\n",
    "src/nonl.rs": "x\ny",  # no newline at the end of the file
}
CHANGE = {
    "src/a.rs": "l1\nn2\nn3\nl3\nl4x\nl5\nl7\n",  # l2 became n2 and n3, l4 became l4x, l6 was dropped
    "src/gone.rs": None,
    "src/b.rs": "b1\nb2\nb3\nb4\nb5\n",
    "src/excluded.rs": "e1\ne2\n",
    "src/lookalike.rs": "a\n++ b/fake.rs\nc\nd\nE\n",  # a changed line that reads like a diff header
    "src/nonl.rs": "x\ny\nz\n",
    "README.md": "docs\n",
    "docs/ünï.md": "x\n",  # git C-quotes a name like this in a diff header unless told not to
}
ADDED = {
    "README.md": [1],
    "docs/ünï.md": [1],
    "src/a.rs": [2, 3, 5],
    "src/b.rs": [1, 2, 3, 4, 5],
    "src/excluded.rs": [1, 2],
    "src/lookalike.rs": [2, 5],
    "src/nonl.rs": [2, 3],
}
# The coverage report: line -> execution count, the `DA:` records of an lcov file.
LCOV = {
    "src/a.rs": {1: 1, 2: 1, 3: 0, 4: 1, 5: 2, 6: 1, 7: 1},
    "src/b.rs": {1: 0, 2: 4, 4: 1, 5: 1},  # line 3 is not executable
    "src/excluded.rs": {1: 0, 2: 0},
    "src/untouched.rs": {1: 0},
    "src/lookalike.rs": {2: 0, 5: 1},
    "src/nonl.rs": {2: 1, 3: 1},
}
REPORT = (
    "src/a.rs: 1 of 3 new lines uncovered: [3]\n"
    "src/b.rs: 1 of 4 new lines uncovered: [1]\n"
    "src/lookalike.rs: 1 of 2 new lines uncovered: [2]\n"
    "new code: 8/11 lines covered = 72.7%\n"
)


def git(cwd, *args):
    """Git on a throwaway repository: no inherited GIT_* and no user or system configuration (signing, hooks)."""
    env = {k: v for k, v in os.environ.items() if not k.startswith("GIT_")}
    env["GIT_CONFIG_GLOBAL"] = os.devnull
    env["GIT_CONFIG_NOSYSTEM"] = "1"
    return subprocess.run(
        ["git", "-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false",
         "-c", f"core.hooksPath={os.devnull}", "-C", cwd, *args],
        check=True, capture_output=True, text=True, encoding="utf-8", env=env).stdout


def write(path, text):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w", encoding="utf-8", newline="\n") as f:
        f.write(text)


def commit(root, files, message):
    for name, text in files.items():
        if text is None:
            os.remove(os.path.join(root, name))
        else:
            write(os.path.join(root, name), text)
    git(root, "add", "-A")
    git(root, "commit", "-q", "-m", message)


def write_lcov(root, records):
    lines = []
    for name, hits in records.items():
        lines += ["TN:", f"SF:{os.path.join(root, name)}", *(f"DA:{n},{c}" for n, c in hits.items()), "end_of_record"]
    path = os.path.join(root, "coverage.lcov")
    write(path, "\n".join(lines) + "\n")
    return path


class Repository(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.root = os.path.realpath(tmp.name)
        git(self.root, "init", "-q")
        commit(self.root, BASE, "base")
        self.base = git(self.root, "rev-parse", "HEAD").strip()
        commit(self.root, CHANGE, "change")

    def report(self, *args):
        """Run the command inside the repository; returns (stdout, stderr)."""
        out, err = io.StringIO(), io.StringIO()
        previous = os.getcwd()
        os.chdir(self.root)
        try:
            with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
                self.assertEqual(nul.main(list(args)), 0)
        finally:
            os.chdir(previous)
        return out.getvalue(), err.getvalue()

    def test_added_lines_are_the_new_side_of_every_hunk(self):
        """A hunk without a count, a pure deletion (`+6,0`), a new file, a deleted file, a changed line that reads like
        a `+++ b/...` header and a file without a final newline."""
        self.assertEqual(nul.added_lines(self.root, self.base), ADDED)

    def test_reports_the_uncovered_new_lines_and_the_percentage(self):
        out, err = self.report(write_lcov(self.root, LCOV), self.base)
        self.assertEqual(out, REPORT)
        self.assertEqual(err, "")

    def test_base_defaults_to_origin_main(self):
        git(self.root, "update-ref", "refs/remotes/origin/main", self.base)
        out, _ = self.report(write_lcov(self.root, LCOV))
        self.assertEqual(out, REPORT)

    def test_the_users_diff_settings_do_not_change_what_is_parsed(self):
        for key, value in (("diff.noprefix", "true"), ("color.diff", "always"), ("color.ui", "always"),
                           ("diff.external", "false")):
            git(self.root, "config", key, value)
        self.assertEqual(nul.added_lines(self.root, self.base), ADDED)

    def test_an_inherited_git_environment_does_not_redirect_git(self):
        with tempfile.TemporaryDirectory() as decoy:
            git(decoy, "init", "-q")
            with mock.patch.dict(os.environ, {"GIT_DIR": os.path.join(decoy, ".git"), "GIT_WORK_TREE": decoy}):
                self.assertEqual(nul.added_lines(self.root, self.base), ADDED)

    def test_uncommitted_edits_and_untracked_rust_files_are_part_of_the_diff(self):
        """cargo llvm-cov measures the working tree, so the diff has to be taken from the working tree too."""
        write(os.path.join(self.root, "src", "a.rs"), CHANGE["src/a.rs"] + "l8\n")  # unstaged
        write(os.path.join(self.root, "src", "untouched.rs"), "u1\nu2\n")  # staged
        git(self.root, "add", "src/untouched.rs")
        write(os.path.join(self.root, "src", "fresh.rs"), "f1\nf2\nf3")  # untracked, no final newline
        write(os.path.join(self.root, "notes.txt"), "not Rust\n")  # untracked, not compiled
        expected = dict(ADDED, **{"src/a.rs": [2, 3, 5, 8], "src/untouched.rs": [2], "src/fresh.rs": [1, 2, 3]})
        self.assertEqual(nul.added_lines(self.root, self.base), expected)

    def test_nothing_added_warns_and_reports_no_executable_lines(self):
        out, err = self.report(write_lcov(self.root, LCOV), "HEAD")
        self.assertEqual(out, "new code: no executable lines among the added lines\n")
        self.assertIn("no lines added between HEAD and the working tree", err)

    def test_added_rust_files_that_are_not_in_the_report_are_named(self):
        commit(self.root, {"src/absent.rs": "z1\n", "src/absent.txt": "z\n"}, "more")
        out, err = self.report(write_lcov(self.root, LCOV), self.base)
        self.assertEqual(out, REPORT)
        self.assertIn("src/absent.rs", err)
        self.assertNotIn("absent.txt", err)  # only Rust files can be in the report

    def test_a_report_that_measures_none_of_the_added_lines_is_not_full_coverage(self):
        out, err = self.report(write_lcov(self.root, {"src/untouched.rs": {1: 1}}), self.base)
        self.assertEqual(out, "new code: no executable lines among the added lines\n")
        for name in ("src/a.rs", "src/b.rs", "src/lookalike.rs", "src/nonl.rs"):
            self.assertIn(name, err)
        self.assertNotIn("excluded.rs", err)  # sonar.coverage.exclusions skips it anyway

    def test_an_unknown_base_stops_with_the_message_from_git(self):
        with self.assertRaises(SystemExit) as caught:
            nul.added_lines(self.root, "no-such-ref")
        self.assertIn("no-such-ref", str(caught.exception))


class DiffText(unittest.TestCase):
    """Parser cases that real diffs cannot be made to produce on demand: a canned diff instead of git's."""

    def added(self, diff):
        return nul.parse_diff(diff)

    def test_a_name_with_a_space_ends_in_a_tab(self):
        diff = "--- a/my file.rs\t\n+++ b/my file.rs\t\n@@ -1 +1,2 @@\n-a\n+b\n+c\n"
        self.assertEqual(self.added(diff), {"my file.rs": [1, 2]})

    def test_a_pure_deletion_adds_nothing(self):
        self.assertEqual(self.added("--- a/x.rs\n+++ b/x.rs\n@@ -3 +2,0 @@\n-gone\n"), {})

    def test_a_no_newline_marker_is_not_a_hunk_line(self):
        diff = ("--- a/x.rs\n+++ b/x.rs\n@@ -2 +2,2 @@\n-y\n\\ No newline at end of file\n+y\n+++ b/q.rs\n"
                "@@ -9 +10 @@\n-i\n+j\n")
        self.assertEqual(self.added(diff), {"x.rs": [2, 3, 10]})

    def test_a_header_that_cannot_be_parsed_stops_the_run(self):
        """git prints a name that contains a quote C-quoted; failing loudly beats dropping the file from the report."""
        diff = '--- "a/we\\"ird.rs"\n+++ "b/we\\"ird.rs"\n@@ -1 +1 @@\n-a\n+b\n'
        with self.assertRaises(SystemExit) as caught:
            self.added(diff)
        self.assertIn("cannot parse the diff header", str(caught.exception))


class Files(unittest.TestCase):
    def test_globs_follow_sonars_ant_patterns(self):
        cases = [
            ("tests/**", "tests/a.rs", True),
            ("tests/**", "tests/x/y.rs", True),
            ("tests/**", "src/tests/a.rs", False),  # patterns start at the project root
            ("src/main.rs", "src/main.rs", True),
            ("src/main.rs", "src/mainXrs", False),  # the dot is a dot
            ("crates/*/src/glue.rs", "crates/c/src/glue.rs", True),
            ("crates/*/src/glue.rs", "crates/a/b/src/glue.rs", False),  # `*` stays inside one segment
            ("**/gen.rs", "gen.rs", True),
            ("**/gen.rs", "a/b/gen.rs", True),
            ("**/gen.rs", "xgen.rs", False),
            ("src/?.rs", "src/a.rs", True),
            ("src/?.rs", "src/ab.rs", False),
            ("a?b.rs", "a/b.rs", False),  # `?` does not match a separator
            ("src/main.rs", "src/main.rs.bak", False),  # the whole path has to match
        ]
        for glob, path, expected in cases:
            with self.subTest(glob=glob, path=path):
                self.assertEqual(bool(nul.glob_regex(glob).match(path)), expected)

    def test_exclusions_come_from_the_coverage_key_with_its_continuation_lines(self):
        with tempfile.TemporaryDirectory() as d:
            path = os.path.join(d, "sonar-project.properties")
            write(path, PROPERTIES)
            patterns = nul.coverage_exclusions(path)
            missing = nul.coverage_exclusions(os.path.join(d, "absent.properties"))

        def excluded(p):
            return any(rx.match(p) for rx in patterns)

        self.assertTrue(excluded("tests/x.rs"))
        self.assertTrue(excluded("src/excluded.rs"))
        self.assertTrue(excluded("crates/z/src/glue.rs"))
        self.assertFalse(excluded("target/x.rs"))  # `sonar.exclusions` is another key
        self.assertFalse(excluded("src/a.rs"))
        self.assertEqual(missing, [])

    def test_lcov_records_resolve_against_the_repository_and_merge(self):
        with tempfile.TemporaryDirectory() as d:
            root = os.path.join(d, "repo")
            path = os.path.join(d, "x.lcov")
            write(path, "\n".join([
                "TN:",
                "SF:" + os.path.join(root, "src", "a.rs"), "DA:1,4", "DA:2,3,abc123", "LF:2", "end_of_record",
                "SF:src/a.rs", "DA:1,0", "DA:3,7", "end_of_record",  # a second record of the same file, path relative
                "SF:" + os.path.join(d, "elsewhere.rs"), "DA:5,1", "end_of_record",
            ]) + "\n")
            hits = nul.read_lcov(path, root)
        self.assertEqual(hits["src/a.rs"], {1: 4, 2: 3, 3: 7})  # a line run by either record counts as run
        self.assertEqual(hits["../elsewhere.rs"], {5: 1})


if __name__ == "__main__":
    unittest.main()
