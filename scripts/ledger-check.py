#!/usr/bin/env python3
"""Compare the ledger of noticed items, a PR's "Found while here" table and the fast-follow issues.

The three are records of the same thing (`.claude/rules/workflow.md`, "The ledger"); a session forgets to keep them in step.
Run it before `gh pr merge` and at the end of a task:

    python3 scripts/ledger-check.py --pr 683 --since 2026-11-01

It prints the open ledger items as a to-do ("do we do #N?") and exits 1 when
  * a ledger line that says it was found in this PR (its "Found in" column names `#<pr>`) is open and not in the PR table,
  * a ledger line marked done in this PR is not in the PR table,
  * an open `fast-follow` issue created since the given day is not in the ledger at all,
  * an open issue that is in the PR table but neither in the ledger nor closed by the PR (a deferral nobody recorded),
  * a ledger line whose "Found in" or "Status" cell has a reference glued to a word (`PR#50`), which is not read.
Without `--pr` only the last check and the to-do list run.  `gh` is called in one place (`gh_json`), so the tests need no network.
"""
import argparse
import datetime
import json
import os
import re
import shutil
import subprocess
import sys

LEDGER = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), ".claude", "logs", "open-items.md")
LIMIT = 200
# at most 9 digits: a longer run is not an issue number, and `int()` refuses very long ones
# not after a word character or `/`: `owner/other-repo#5` and `abc#5` are not issues of this repository
ISSUE = re.compile(r"(?<![\w/])#([0-9]{1,9})(?![0-9])")
# control characters, line separators, and the invisible/bidirectional marks that reorder or hide text
CONTROL = re.compile(r"[\x00-\x1f\x7f-\x9f\u061c\u200b-\u200f\u2028-\u202e\u2060-\u2069\ufeff]")
# a `|` that is not escaped as `\|`, and a heading line (up to three leading spaces, CommonMark)
UNESCAPED_PIPE = re.compile(r"(?<!\\)\|")
HEADING = re.compile(r" {0,3}#{1,6}(?:[ \t]|$)")
# GitHub closing keywords: the issues a PR closes are not "deferred" even though they are still open before the merge
# (the separator is `\s+` or `:\s*`, which start with different characters, so matching is linear on a long run of blanks)
CLOSES = re.compile(r"\b(?:close[sd]?|fix(?:e[sd])?|resolve[sd]?)(?:\s+|:\s*)#([0-9]{1,9})(?![0-9])", re.I)
# the most issues the PR table is checked against GitHub for: a PR description is written by anyone who can edit it
MAX_LOOKUPS = 50
# a reference glued to a word (`PR#50`) is not read by `ISSUE`; `owner/repo#5` (a reference to another repository) is fine
GLUED = re.compile(r"(?<![\w/])[A-Za-z]+#[0-9]{1,9}")
# the longest a single `gh` call may take; a hung `gh` must not hang the check
GH_TIMEOUT = 60


def find_gh(path_env=None, cwd=None):
    """The `gh` executable found on `PATH`, never in the current directory (Windows looks there first, so a `gh.exe` planted
    in a checkout would run): empty entries, `.` and the working directory are skipped. `None` when there is none."""
    cwd = os.path.realpath(cwd or os.getcwd())
    names = ["gh"]
    if os.name == "nt":
        names = ["gh" + ext.lower() for ext in os.environ.get("PATHEXT", ".EXE;.CMD;.BAT").split(";") if ext]
    for entry in (os.environ.get("PATH", "") if path_env is None else path_env).split(os.pathsep):
        if entry in ("", ".") or os.path.realpath(entry) == cwd:
            continue
        for name in names:
            candidate = os.path.join(entry, name)
            if os.path.isfile(candidate) and os.access(candidate, os.X_OK):
                return os.path.abspath(candidate)
    return None


def clean(text, limit=120):
    """Text of an issue (written by anyone who can edit it) made safe to print: no control characters, one line, short."""
    text = CONTROL.sub("?", text)
    return text if len(text) <= limit else text[: limit - 3] + "..."


def positive_int(value):
    number = int(value)
    if not 0 < number < 10**9:
        raise argparse.ArgumentTypeError("must be a positive issue or PR number")
    return number


def parse_ledger(text):
    """Rows of the ledger table as `(number, what, found_in, status)`; the header and the separator are skipped."""
    rows = []
    for line in text.splitlines():
        cells = [c.strip().replace("\\|", "|") for c in UNESCAPED_PIPE.split(line.strip().strip("|"))]
        if len(cells) < 4 or not re.fullmatch(r"#[0-9]{1,9}", cells[0]):
            continue
        # an escaped `|` stays inside its cell; the last cell is the status, the first three are fixed
        rows.append((int(cells[0][1:]), cells[1], cells[2], cells[-1]))
    return rows


def is_done(status):
    return status.lower().startswith("done")


def table_issues(body):
    """The issue numbers in the table rows of the "Found while here" section of a PR description (up to the next heading)."""
    found = set()
    inside = False
    for line in body.splitlines():
        if HEADING.match(line):
            inside = line.strip().lstrip("#").strip().lower().startswith("found while here")
            continue
        if inside and line.strip().startswith("|"):
            found.update(int(n) for n in ISSUE.findall(line))
    return found


def closed_by(body):
    """The issues a PR description closes with a GitHub keyword (`Closes #1`, `Fixes #2`)."""
    return {int(n) for n in CLOSES.findall(body)}


def glued_references(rows):
    """`(number, text)` for each reference glued to a word (`PR#50`) in the "Found in" or "Status" cell of a ledger line."""
    return [(number, m.group()) for number, _what, found_in, status in rows
            for m in GLUED.finditer(found_in + " " + status)]


def check(rows, pr, table, fast_follow):
    """The list of problems; `pr` is a number or None, `table` the issues in the PR table, `fast_follow` the recent issues."""
    problems = []
    in_ledger = {number for number, *_ in rows}
    for number, text in glued_references(rows):
        problems.append(f"#{number}: \"{clean(text, 40)}\" has no space before the number, so it is not read as a reference: write \"PR #N\"")
    if pr is not None:
        for number, _what, found_in, status in rows:
            if number in table:
                continue
            if not is_done(status) and pr in (int(n) for n in ISSUE.findall(found_in)):
                problems.append(f"#{number} was found in #{pr} and is open, but is not in its \"Found while here\" table")
            elif is_done(status) and pr in (int(n) for n in ISSUE.findall(status)):
                problems.append(f"#{number} is marked done in #{pr}, but is not in its \"Found while here\" table")
    for number in sorted(fast_follow):
        if number not in in_ledger:
            problems.append(f"#{number} ({clean(fast_follow[number])}) is an open fast-follow issue (created in the window) that is not in the ledger")
    return problems


class GhError(Exception):
    """A `gh` call that failed or answered with something unusable. `missing` is set for an HTTP 404 (no such issue)."""

    def __init__(self, message, missing=False):
        super().__init__(message)
        self.missing = missing


def gh_json(args):
    """Run `gh` and parse its JSON. A failure raises `GhError`; `main` turns it into one stderr line and exit status 2."""
    what = f"gh {' '.join(args[:2])}"
    exe = find_gh()
    if exe is None:
        raise GhError(f"{what} failed: gh was not found on PATH")
    try:
        done = subprocess.run([exe, *args], check=True, capture_output=True, text=True, encoding="utf-8",
                              timeout=GH_TIMEOUT)
        return json.loads(done.stdout)
    except subprocess.CalledProcessError as e:
        stderr = e.stderr or ""
        raise GhError(f"{what} failed ({e.returncode}): {clean(stderr, 300)}", missing="HTTP 404" in stderr) from None
    except subprocess.TimeoutExpired:
        raise GhError(f"{what} failed: no answer in {GH_TIMEOUT} s") from None
    except (OSError, ValueError, RecursionError) as e:  # RecursionError: a document nested tens of thousands of levels deep
        raise GhError(f"{what} failed: {clean(str(e), 300)}") from None


def as_mapping(reply, what):
    if not isinstance(reply, dict):
        raise GhError(f"{what} returned an unexpected reply")
    return reply


def untracked_open_issues(numbers, run=gh_json):
    """Of these numbers: `(open, unknown)`. `open` are open issues (not pull requests); `unknown` are numbers GitHub
    answers 404 for (a typo, a deleted issue): that is reported, it does not stop the check. Any other failure raises."""
    found, unknown = [], []
    for n in sorted(numbers):
        try:
            info = as_mapping(run(["api", f"repos/{{owner}}/{{repo}}/issues/{n}"]), "gh api")
        except GhError as e:
            if not e.missing:
                raise
            print(e, file=sys.stderr)
            unknown.append(n)
            continue
        if "pull_request" not in info and info.get("state") == "open":
            found.append(n)
    return found, unknown


def recent_fast_follow(since, run=gh_json):
    issues = run(["issue", "list", "--label", "fast-follow", "--state", "open", "--search", f"created:>={since}",
                  "--limit", str(LIMIT), "--json", "number,title"])
    if not isinstance(issues, list) or not all(
        isinstance(i, dict) and isinstance(i.get("number"), int) and isinstance(i.get("title"), str) for i in issues
    ):
        raise GhError("gh issue list returned an unexpected reply")
    return {i["number"]: i["title"] for i in issues}


def report(args, run, ledger_path):
    """The checks and the report; `GhError` from any `gh` call propagates."""
    with open(ledger_path, encoding="utf-8") as f:
        rows = parse_ledger(f.read())
    table, closes = set(), set()
    if args.pr is not None:
        reply = as_mapping(run(["pr", "view", str(args.pr), "--json", "body"]), "gh pr view")
        body = reply.get("body")
        body = body if isinstance(body, str) else ""
        table, closes = table_issues(body), closed_by(body)
    recent = recent_fast_follow(args.since, run)
    problems = check(rows, args.pr, table, recent)
    in_ledger = {r[0] for r in rows}
    untracked = table - in_ledger - closes
    if len(untracked) > MAX_LOOKUPS:
        problems.append(f"the PR table names {len(untracked)} issues that are not in the ledger or closed by the PR "
                        f"(more than {MAX_LOOKUPS}): check them by hand, they were not looked up")
    else:
        open_issues, unknown = untracked_open_issues(untracked, run)
        for n in open_issues:
            problems.append(f"#{n} is in the PR table, is still open, is not closed by the PR and is not in the ledger")
        for n in unknown:
            problems.append(f"#{n} is in the PR table but GitHub has no such issue here (HTTP 404): check the number")
    if len(recent) >= LIMIT:
        problems.append(f"{LIMIT} or more open fast-follow issues in the window: the list may be cut, narrow --since")
    open_rows = [r for r in rows if not is_done(r[3])]
    print(f"Open ledger items ({len(open_rows)}):")
    for number, what, _found, status in open_rows:
        print(f"  do we do #{number}? {clean(what)} [{clean(status)}]")
    if problems:
        print("\nGaps:")
        for p in problems:
            print(f"  - {p}")
        return 1
    print("\nThe ledger, the PR table and the fast-follow issues agree.")
    return 0


def main(argv=None, run=gh_json, ledger_path=LEDGER):
    """Exit status: 0 the records agree, 1 they do not, 2 the tool could not run (bad input, or `gh` failed)."""
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--pr", type=positive_int, help="the pull request about to be merged")
    default_since = (datetime.date.today() - datetime.timedelta(days=7)).isoformat()
    ap.add_argument("--since", default=default_since, help="issues created since this day (YYYY-MM-DD), default: 7 days ago")
    args = ap.parse_args(argv)
    try:
        if not re.fullmatch(r"[0-9]{4}-[0-9]{2}-[0-9]{2}", args.since):
            raise ValueError
        datetime.date(*(int(part) for part in args.since.split("-")))
    except ValueError:
        print("--since must be a valid date, YYYY-MM-DD", file=sys.stderr)
        return 2
    try:
        return report(args, run, ledger_path)
    except GhError as e:
        print(e, file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())
