#!/usr/bin/env python3
"""Compare the ledger of noticed items, a PR's "Found while here" table and the fast-follow issues.

The three are records of the same thing (`.claude/rules/workflow.md`, "The ledger"); a session forgets to keep them in step.
Run it before `gh pr merge` and at the end of a task:

    python3 scripts/ledger-check.py --pr 683 --since 2026-11-01

It prints the open ledger items as a to-do ("do we do #N?") and exits 1 when
  * a ledger line that says it was found in this PR (its "Found in" column names `#<pr>`) is open and not in the PR table,
  * a ledger line marked done in this PR is not in the PR table,
  * an open `fast-follow` issue created since the given day is not in the ledger at all.
Without `--pr` only the last check and the to-do list run.  `gh` is called in one place (`gh_json`), so the tests need no network.
"""
import argparse
import datetime
import json
import os
import re
import subprocess
import sys

LEDGER = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), ".claude", "logs", "open-items.md")
LIMIT = 200
# at most 9 digits: a longer run is not an issue number, and `int()` refuses very long ones
ISSUE = re.compile(r"#([0-9]{1,9})(?![0-9])")
CONTROL = re.compile(r"[\x00-\x1f\x7f-\x9f\u2028\u2029]")


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
        cells = [c.strip() for c in line.strip().strip("|").split("|")]
        if len(cells) < 4 or not re.fullmatch(r"#[0-9]{1,9}", cells[0]):
            continue
        # a `|` inside a cell would shift the columns: the last cell is the status, the first three are fixed
        rows.append((int(cells[0][1:]), cells[1], cells[2], cells[-1]))
    return rows


def is_done(status):
    return status.lower().startswith("done")


def table_issues(body):
    """The issue numbers named in the "Found while here" section of a PR description (up to the next heading)."""
    found = set()
    inside = False
    for line in body.splitlines():
        if line.startswith("#"):
            inside = line.lstrip("# ").strip().lower().startswith("found while here")
            continue
        if inside:
            found.update(int(n) for n in ISSUE.findall(line))
    return found


def check(rows, pr, table, fast_follow):
    """The list of problems; `pr` is a number or None, `table` the issues in the PR table, `fast_follow` the recent issues."""
    problems = []
    in_ledger = {number for number, *_ in rows}
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


def gh_json(args):
    out = subprocess.run(["gh", *args], check=True, capture_output=True, text=True, encoding="utf-8").stdout
    return json.loads(out)


def recent_fast_follow(since, run=gh_json):
    issues = run(["issue", "list", "--label", "fast-follow", "--state", "open", "--search", f"created:>={since}",
                  "--limit", str(LIMIT), "--json", "number,title"])
    return {i["number"]: i["title"] for i in issues}


def main(argv=None, run=gh_json, ledger_path=LEDGER):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--pr", type=positive_int, help="the pull request about to be merged")
    default_since = (datetime.date.today() - datetime.timedelta(days=7)).isoformat()
    ap.add_argument("--since", default=default_since, help="issues created since this day (YYYY-MM-DD), default: 7 days ago")
    args = ap.parse_args(argv)
    if not re.fullmatch(r"[0-9]{4}-[0-9]{2}-[0-9]{2}", args.since):
        print("--since must be YYYY-MM-DD", file=sys.stderr)
        return 2
    with open(ledger_path, encoding="utf-8") as f:
        rows = parse_ledger(f.read())
    table = set()
    if args.pr is not None:
        table = table_issues(run(["pr", "view", str(args.pr), "--json", "body"])["body"] or "")
    recent = recent_fast_follow(args.since, run)
    problems = check(rows, args.pr, table, recent)
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


if __name__ == "__main__":
    sys.exit(main())
