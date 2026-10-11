#!/usr/bin/env python3
"""Tests for scripts/pr-comments.sh, run against a fake `gh`.

The fake `gh` answers `gh api` from canned JSON files, applies `--jq` with the real `jq` (so the script's own filters
run), and refuses what a cloud container refuses: every GraphQL-backed command. The script must work without them.

Run:  python3 scripts/test_pr_comments.py        (needs bash, git and jq)
"""
import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest

SCRIPT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "pr-comments.sh")
SHA = "0123456789abcdef0123456789abcdef01234567"
PR = 5
PULL = f"repos/o/r/pulls/{PR}"
CHECK_RUNS = f"repos/o/r/commits/{SHA}/check-runs"
STATUS = f"repos/o/r/commits/{SHA}/status"
ISSUE_COMMENTS = f"repos/o/r/issues/{PR}/comments"
REVIEWS = f"repos/o/r/pulls/{PR}/reviews"
INLINE_COMMENTS = f"repos/o/r/pulls/{PR}/comments"

# The fake `gh`: refuses GraphQL, serves `<fixtures>/<endpoint with / as __, no query>.json` through `jq -r <--jq filter>`
# (every call of the script has a filter), can be told to fail (`.fail`) or to print a warning and still succeed
# (`.warn`), and logs every call.
FAKE_GH = """#!{python}
import os
import subprocess
import sys


def read(path):
    with open(path, encoding="utf-8") as f:
        return f.read()


args = sys.argv[1:]
fixtures = os.environ["FAKE_GH_FIXTURES"]
with open(os.path.join(fixtures, "calls.log"), "a", encoding="utf-8") as log:
    log.write(" ".join(args) + "\\n")
if args[:1] != ["api"] or args[1:2] == ["graphql"]:
    sys.exit("GitHub GraphQL is not available from Claude Code sessions")
jq, endpoint, rest = None, None, args[1:]
while rest:
    if rest[0] == "--jq":
        jq, rest = rest[1], rest[2:]
    else:
        if rest[0] != "--paginate":
            endpoint = rest[0]
        rest = rest[1:]
if jq is None:
    sys.exit("fake gh: the script never calls gh api without --jq")
base = os.path.join(fixtures, endpoint.split("?")[0].replace("/", "__"))
if os.path.exists(base + ".fail"):
    sys.exit(read(base + ".fail"))
if not os.path.exists(base + ".json"):
    sys.exit("gh: Not Found (HTTP 404)")
if os.path.exists(base + ".warn"):
    print(read(base + ".warn"), file=sys.stderr)
sys.exit(subprocess.run(["jq", "-r", jq, base + ".json"]).returncode)
"""


def pull(**fields):
    """A pull request as the REST API returns it, with `fields` overriding the defaults."""
    return {"title": "A change", "state": "open", "merged": False, "mergeable_state": "clean",
            "base": {"ref": "main"}, "head": {"sha": SHA}, **fields}


def check(name, status="completed", conclusion="success"):
    """A check run; only a completed one has a conclusion."""
    return {"name": name, "status": status, "conclusion": conclusion if status == "completed" else None}


def runs(*checks):
    """The body of the check-runs endpoint."""
    return {"total_count": len(checks), "check_runs": list(checks)}


def statuses(*pairs):
    """The body of the combined-status endpoint, from (state, context) pairs."""
    return {"statuses": [{"state": state, "context": context} for state, context in pairs]}


def git(cwd, *args):
    """Git on a throwaway repository: no inherited GIT_* and no user or system configuration."""
    env = {k: v for k, v in os.environ.items() if not k.startswith("GIT_")}
    env["GIT_CONFIG_GLOBAL"] = os.devnull
    env["GIT_CONFIG_NOSYSTEM"] = "1"
    return subprocess.run(["git", "-C", cwd, *args], check=True, capture_output=True, text=True, env=env).stdout


class PrComments(unittest.TestCase):
    """Runs the script on a repository whose origin is github.com/o/r, with every endpoint it reads canned."""

    def setUp(self):
        """Build the fake `gh`, a work directory with an origin, and fixtures for a PR whose checks are all green."""
        tmp = tempfile.mkdtemp()
        self.addCleanup(shutil.rmtree, tmp, ignore_errors=True)
        self.bin, self.fixtures, self.work = (os.path.join(tmp, d) for d in ("bin", "fixtures", "work"))
        for d in (self.bin, self.fixtures, self.work):
            os.mkdir(d)
        gh = os.path.join(self.bin, "gh")
        with open(gh, "w", encoding="utf-8") as f:
            f.write(FAKE_GH.format(python=sys.executable))
        os.chmod(gh, 0o755)
        git(self.work, "init", "-q")
        git(self.work, "remote", "add", "origin", "https://github.com/o/r")
        self.put(PULL, pull())
        self.put(CHECK_RUNS, runs(check("fmt")))
        self.put(STATUS, statuses())
        for endpoint in (ISSUE_COMMENTS, REVIEWS, INLINE_COMMENTS):
            self.put(endpoint, [])

    def path(self, endpoint, suffix):
        """The fixture file that stands for `endpoint`."""
        return os.path.join(self.fixtures, endpoint.replace("/", "__") + suffix)

    def put(self, endpoint, data):
        """Serve `data` (as JSON) for `endpoint`."""
        with open(self.path(endpoint, ".json"), "w", encoding="utf-8") as f:
            json.dump(data, f)

    def put_text(self, endpoint, suffix, text):
        """Make `endpoint` fail with `text` (suffix `.fail`) or warn with it on stderr and succeed (`.warn`)."""
        with open(self.path(endpoint, suffix), "w", encoding="utf-8") as f:
            f.write(text)

    def calls(self):
        """The arguments of every `gh` call so far, one string per call (none if it was never run)."""
        try:
            with open(os.path.join(self.fixtures, "calls.log"), encoding="utf-8") as f:
                return f.read().splitlines()
        except FileNotFoundError:
            return []

    def run_script(self, *args, cwd=None):
        """Run the script with the fake `gh` first on PATH; its output is decoded as it is, carriage returns included."""
        env = {k: v for k, v in os.environ.items() if not k.startswith(("GIT_", "GH_", "GITHUB_"))}
        env["PATH"] = self.bin + os.pathsep + env["PATH"]
        env["FAKE_GH_FIXTURES"] = self.fixtures
        env["GIT_CONFIG_GLOBAL"] = os.devnull
        env["GIT_CONFIG_NOSYSTEM"] = "1"
        r = subprocess.run(["bash", SCRIPT, *args], cwd=cwd or self.work, env=env, capture_output=True, timeout=60)
        return subprocess.CompletedProcess(r.args, r.returncode, r.stdout.decode("utf-8"), r.stderr.decode("utf-8"))

    def checks_section(self):
        """The part of the report between the checks heading and the next section."""
        out = self.run_script(str(PR)).stdout
        return out.split("== checks that are not green ==\n")[1].split("\n\n== ")[0]

    def test_header_names_the_pull_request_and_its_head(self):
        """The header comes from one REST read of the pull request, and the checks are asked about that same commit."""
        r = self.run_script(str(PR))
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(r.stdout.splitlines()[:4], [
            "== PR #5 in o/r ==", "title:  A change", "state:  open  merge: clean  base: main", f"head:   {SHA}"])
        self.assertEqual(len([c for c in self.calls() if c.startswith(f"api {PULL} ")]), 1)
        self.assertIn(f"api --paginate {CHECK_RUNS}?per_page=100 --jq", " ".join(self.calls()))

    def test_a_merged_pull_request_says_so(self):
        """A closed pull request that was merged is not reported as merely closed."""
        self.put(PULL, pull(state="closed", merged=True, mergeable_state=None))
        self.assertIn("state:  closed (merged)  merge: unknown  base: main", self.run_script(str(PR)).stdout)

    def test_green_checks_are_all_passing(self):
        """Success, skipped and neutral runs and a successful legacy status leave nothing to list."""
        self.put(CHECK_RUNS, runs(check("fmt"), check("e2e", conclusion="skipped"), check("note", conclusion="neutral")))
        self.put(STATUS, statuses(("success", "legacy")))
        self.assertEqual(self.checks_section().strip(), "(all passing)")

    def test_checks_that_are_not_green_are_listed(self):
        """Every other conclusion, a run that has not finished and a failed or pending status are listed by state."""
        self.put(CHECK_RUNS, runs(
            check("fmt"), check("unit", conclusion="failure"), check("lint", conclusion="cancelled"),
            check("slow", conclusion="timed_out"), check("windows", status="in_progress"), check("e2e", status="queued")))
        self.put(STATUS, statuses(("error", "ci/legacy"), ("pending", "ci/other"), ("success", "ci/fine")))
        section = self.checks_section()
        for row in ("failure\tunit", "cancelled\tlint", "timed_out\tslow", "in_progress\twindows", "queued\te2e",
                    "error\tci/legacy", "pending\tci/other"):
            self.assertIn("  " + row, section)
        self.assertNotIn("fmt", section)
        self.assertNotIn("ci/fine", section)
        self.assertNotIn("all passing", section)
        self.assertIn("!! something is still running", section)

    def test_a_failure_alone_does_not_claim_that_something_is_running(self):
        """The warning that a bot has not spoken yet appears only while a check has not finished."""
        self.put(CHECK_RUNS, runs(check("unit", conclusion="failure")))
        section = self.checks_section()
        self.assertIn("  failure\tunit", section)
        self.assertNotIn("still running", section)

    def test_a_pull_request_that_cannot_be_read_ends_the_run(self):
        """A failed read of the pull request stops the script with gh's own message, before any check is asked for."""
        self.put_text(PULL, ".fail", "gh: Bad Gateway (HTTP 502)")
        r = self.run_script(str(PR))
        self.assertEqual(r.returncode, 1)
        self.assertIn("HTTP 502", r.stderr)
        self.assertIn("could not read the head commit", r.stderr)
        self.assertNotIn("checks that are not green", r.stdout)
        self.assertNotIn("check-runs", " ".join(self.calls()))
        self.assertEqual(self.run_script("6").returncode, 1)  # no such pull request: the fake answers 404

    def test_an_unreadable_comment_stream_is_reported_and_the_others_still_print(self):
        """A stream that cannot be read says so in its own section instead of printing the error as a comment."""
        self.put_text(ISSUE_COMMENTS, ".fail", "gh: Bad Gateway (HTTP 502)")
        self.put(REVIEWS, [{"user": {"login": "owner"}, "state": "APPROVED", "submitted_at": "2026-10-11T10:05:00Z",
                            "commit_id": SHA, "body": "ok"}])
        r = self.run_script(str(PR))
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn(f"(could not read {ISSUE_COMMENTS}?per_page=100: gh: Bad Gateway (HTTP 502)", r.stdout)
        self.assertIn("-- owner  APPROVED", r.stdout)

    def test_every_unfinished_state_flags_that_something_is_running(self):
        """Whichever word the API uses for a check that has not finished, the report says to run it again."""
        for state in ("queued", "in_progress", "waiting", "requested", "pending"):
            with self.subTest(check_run=state):
                self.put(CHECK_RUNS, runs(check("job", status=state)))
                self.put(STATUS, statuses())
                self.assertIn("still running", self.checks_section())
        self.put(CHECK_RUNS, runs(check("job")))
        self.put(STATUS, statuses(("pending", "ci/legacy")))
        self.assertIn("still running", self.checks_section())

    def test_a_check_named_after_a_state_is_not_an_unfinished_check(self):
        """Only the state column says that something is running, not a check that happens to be called "queued"."""
        self.put(CHECK_RUNS, runs(check("queued", conclusion="failure"), check("in_progress notes", conclusion="failure")))
        section = self.checks_section()
        self.assertIn("  failure\tqueued", section)
        self.assertNotIn("still running", section)

    def test_a_head_that_is_not_a_commit_id_stops_the_run(self):
        """The head is put into an API path, so anything but lowercase hex ends the run before a check is asked for."""
        for sha in ("", "NOT-A-SHA", "abc/../def", "0123 4567"):
            with self.subTest(sha=sha):
                self.put(PULL, pull(head={"sha": sha}))
                r = self.run_script(str(PR))
                self.assertEqual(r.returncode, 1)
                self.assertIn("could not read the head commit", r.stderr)
                self.assertNotIn("check-runs", " ".join(self.calls()))

    def test_unreadable_check_runs_are_never_all_passing(self):
        """A failed read of the check runs says so, with gh's own words, instead of showing a green list."""
        self.put_text(CHECK_RUNS, ".fail", "gh: Bad Gateway (HTTP 502)")
        section = self.checks_section()
        self.assertIn("could not read the checks", section)
        self.assertIn("HTTP 502", section)
        self.assertNotIn("all passing", section)

    def test_unreadable_legacy_statuses_are_never_all_passing(self):
        """The same for the legacy statuses, although the check runs were read fine."""
        self.put_text(STATUS, ".fail", "gh: Bad Gateway (HTTP 502)")
        section = self.checks_section()
        self.assertIn("could not read the checks", section)
        self.assertNotIn("all passing", section)

    def test_no_check_at_all_is_never_all_passing(self):
        """A head commit nothing has reported on yet is not a green one."""
        self.put(CHECK_RUNS, runs())
        section = self.checks_section()
        self.assertIn("no check reported for 01234567 yet", section)
        self.assertNotIn("all passing", section)

    def test_noise_from_gh_is_not_a_check(self):
        """A warning gh prints next to an empty answer is not a row, so it cannot turn "no check" into "all passing"."""
        self.put(CHECK_RUNS, runs())
        self.put_text(CHECK_RUNS, ".warn", "A new release of gh is available")
        section = self.checks_section()
        self.assertIn("no check reported", section)
        self.assertNotIn("all passing", section)

    def test_the_repository_comes_from_the_origin_when_graphql_is_refused(self):
        """`gh repo view` is refused, so the owner and name are read from whatever form the origin URL has."""
        for url in ("https://github.com/o/r", "https://github.com/o/r.git", "https://github.com/o/r/",
                    "git@github.com:o/r.git", "ssh://git@github.com/o/r.git", "http://local_proxy@127.0.0.1:41299/git/o/r"):
            with self.subTest(url=url):
                git(self.work, "remote", "set-url", "origin", url)
                r = self.run_script(str(PR))
                self.assertEqual(r.returncode, 0, r.stderr)
                self.assertEqual(r.stdout.splitlines()[0], "== PR #5 in o/r ==")

    def test_the_repo_option_wins_over_the_origin_and_needs_no_checkout(self):
        """`--repo` is used as given, even from a directory that is not a repository."""
        git(self.work, "remote", "set-url", "origin", "https://github.com/x/y")
        elsewhere = os.path.join(os.path.dirname(self.work), "elsewhere")
        os.mkdir(elsewhere)
        r = self.run_script(str(PR), "--repo", "o/r", cwd=elsewhere)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(r.stdout.splitlines()[0], "== PR #5 in o/r ==")

    def test_a_repo_that_is_not_owner_slash_name_is_refused_before_any_request(self):
        """The repository ends up in an API path, so anything but OWNER/REPO characters stops the script first."""
        for repo in ("o", "o/r/extra", "o/r?x=1", "o/r r", "/r", "o/"):
            with self.subTest(repo=repo):
                r = self.run_script(str(PR), "--repo", repo)
                self.assertEqual(r.returncode, 2)
                self.assertIn("pass --repo OWNER/REPO", r.stderr)
        self.assertFalse(any(f"pulls/{PR}" in call for call in self.calls()), self.calls())

    def test_an_unknown_repository_asks_for_the_repo_option(self):
        """Without an origin and with `gh repo view` refused there is nothing to go on, and the script says what to do."""
        git(self.work, "remote", "remove", "origin")
        r = self.run_script(str(PR))
        self.assertEqual(r.returncode, 2)
        self.assertIn("pass --repo OWNER/REPO", r.stderr)

    def test_the_comment_streams_are_printed_with_counts(self):
        """Issue comments, reviews and inline comments are printed newest last, and the counts cover all of them."""
        self.put(ISSUE_COMMENTS, [{"user": {"login": "rabbit[bot]"}, "created_at": "2026-10-11T10:00:00Z", "id": 11,
                                   "body": "line1\r\nline2"}])
        self.put(REVIEWS, [{"user": {"login": "owner"}, "state": "COMMENTED", "submitted_at": "2026-10-11T10:05:00Z",
                            "commit_id": SHA, "body": "looks fine"}])
        self.put(INLINE_COMMENTS, [{"user": {"login": "rabbit[bot]"}, "path": "src/x.rs", "line": 7,
                                    "created_at": "2026-10-11T10:06:00Z", "commit_id": SHA, "body": "nit",
                                    "in_reply_to_id": 9},
                                   {"user": {"login": "owner"}, "path": "src/y.rs", "line": None, "original_line": 3,
                                    "created_at": "2026-10-11T10:07:00Z", "commit_id": SHA, "body": "top-level"}])
        out = self.run_script(str(PR)).stdout
        self.assertNotIn("\r", out)
        for expected in ("-- rabbit[bot]  2026-10-11T10:00:00Z  #11\n    line1\n    line2",
                         "-- owner  COMMENTED  2026-10-11T10:05:00Z  on 01234567\n    looks fine",
                         "-- rabbit[bot]  src/x.rs:7  2026-10-11T10:06:00Z  on 01234567  (reply to #9)\n    nit",
                         "-- owner  src/y.rs:3  2026-10-11T10:07:00Z  on 01234567\n    top-level",
                         "issue comments: 1   reviews: 1   inline comments: 2"):
            self.assertIn(expected, out)
        later = self.run_script(str(PR), "--since", "2026-10-11T10:03:00Z").stdout
        self.assertNotIn("line1", later)
        self.assertIn("looks fine", later)
        self.assertIn("issue comments: 1   reviews: 1   inline comments: 2", later)

    def test_bodies_are_cut_unless_the_whole_text_is_asked_for(self):
        """The default shows the first 900 characters of a body, `--full` all of it."""
        self.put(ISSUE_COMMENTS, [{"user": {"login": "a"}, "created_at": "2026-10-11T10:00:00Z", "id": 1,
                                   "body": "x" * 2000}])
        self.assertIn("x" * 900, self.run_script(str(PR)).stdout)
        self.assertNotIn("x" * 901, self.run_script(str(PR)).stdout)
        self.assertIn("x" * 2000, self.run_script(str(PR), "--full").stdout)

    def test_bad_arguments_print_the_usage_and_stop(self):
        """A missing or malformed argument ends the run with status 2 before anything is requested."""
        for args in ([], ["abc"], ["5", "6"], ["5", "--bogus"], ["5", "--since"], ["5", "--repo"],
                     ["5", "--since", 'x";y']):
            with self.subTest(args=args):
                r = self.run_script(*args)
                self.assertEqual(r.returncode, 2)
        self.assertEqual(self.calls(), [])

    def test_help_prints_the_whole_header_comment(self):
        """`--help` prints the comment block at the top of the script, from its first line to its last."""
        r = self.run_script("--help")
        self.assertEqual(r.returncode, 0)
        lines = r.stdout.splitlines()
        self.assertTrue(lines[0].startswith("Everything reviewers and bots said"), lines[0])
        self.assertTrue(lines[-1].startswith("Needs an authenticated `gh`"), lines[-1])


def setUpModule():
    """The tests need real bash, git and jq; fail loudly rather than skip, so a CI runner without them is noticed."""
    missing = [tool for tool in ("bash", "git", "jq") if shutil.which(tool) is None]
    if missing:
        raise RuntimeError("test_pr_comments.py needs " + ", ".join(missing) + " on PATH")


if __name__ == "__main__":
    unittest.main()
