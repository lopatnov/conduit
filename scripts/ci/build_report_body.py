#!/usr/bin/env python3
"""Build the body of a report comment from text produced by an earlier (untrusted) job.

The report jobs in ci.yml are split in two (#461): a `measure` job that runs repository code and holds no write
token, and a `comment` job that holds `pull-requests: write` and runs nothing from the pull request. The comment job
receives the measured text through a job output, so it treats it as data: this script puts the marker (and an
optional heading) in front, removes any marker the text itself carries, and cuts the result below the 65 536
character limit of a GitHub comment.

Usage: build_report_body.py OUT_FILE
Env:   MARKER (required)  REPORT (the text)  HEADING (optional, e.g. "## Footprint report")
"""
import os
import sys

LIMIT = 60000  # GitHub rejects comments over 65 536 characters


def build(marker, report, heading=""):
    text = report.replace(marker, "").strip()
    head = marker + "\n\n" + (heading + "\n\n" if heading else "")
    room = LIMIT - len(head)
    if len(text) > room:
        text = text[: room - 80].rstrip() + "\n\n_[report truncated to fit a GitHub comment]_"
    return head + text + "\n"


def main():
    marker = os.environ["MARKER"]
    if not marker.strip():
        sys.exit("MARKER must not be empty")
    body = build(marker, os.environ.get("REPORT", ""), os.environ.get("HEADING", ""))
    with open(sys.argv[1], "w", encoding="utf-8") as f:
        f.write(body)


if __name__ == "__main__":
    main()
