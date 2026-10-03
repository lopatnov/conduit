#!/usr/bin/env python3
"""Find this repo's own earlier PR report comment by marker, across multiple pages.

Shared by the footprint/performance/code-length CI report jobs (see ci.yml) so a
long-lived tracking PR (many comments, e.g. #152) doesn't make page-1-only lookups
miss the bot's own earlier comment and start duplicating it on every push.

Reads one already-fetched comments page (a JSON array, or an error body) from stdin,
and prints "<comment_id>|<is_full_page>" — comment_id is empty if no match on this
page. Only a comment authored by github-actions[bot] that contains the marker counts
as a match, so a user comment that merely quotes the marker text is never picked up
(and never PATCHed over).
"""
import json
import os
import sys

marker = os.environ["MARKER"]

try:
    data = json.load(sys.stdin)
except ValueError:
    data = None

found = ""
if isinstance(data, list):
    for c in data:
        if marker in (c.get("body") or "") and (c.get("user") or {}).get("login") == "github-actions[bot]":
            found = str(c["id"])
            break

full_page = "1" if isinstance(data, list) and len(data) == 100 else "0"
print(f"{found}|{full_page}")
