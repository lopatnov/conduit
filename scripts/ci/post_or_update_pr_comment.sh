#!/usr/bin/env bash
# Post or update this job's own report comment on a PR, tolerating long-lived PRs
# with many existing comments (e.g. the #114 migration tracking PR #152).
#
# Finds OUR earlier comment by scanning up to 5 pages of 100 comments, accepting
# only one authored by github-actions[bot] that carries $MARKER — a user comment
# that merely quotes the marker text is never picked up (and never PATCHed over).
# An API error body (rate limit, 5xx) is treated as "no page", not as a crash.
#
# Originally this lookup was duplicated three times (footprint/performance/code-length
# jobs in ci.yml), and two of the three copies only checked page 1 with no author
# filter — which silently stopped updating the report once the bot's own earlier
# comment scrolled past page 1 on a long-running PR, and would also try (and fail,
# since GITHUB_TOKEN can't edit another user's comment) to PATCH a user comment that
# happened to quote the marker. See issue filed from gitar-bot's review of PR #152.
#
# Usage: post_or_update_pr_comment.sh <body-file>
# Required env: GH_TOKEN, REPO, PR_NUMBER, MARKER
# On a failed POST/PATCH (read-only token on a fork run, a rate limit), this warns
# via a GitHub Actions ::warning:: annotation rather than failing the job — these
# reports are informational, not a merge gate.
set -euo pipefail

body_file="$1"
script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

existing_id=""
for page in 1 2 3 4 5; do
  curl -sS -H "Authorization: Bearer $GH_TOKEN" -H "Accept: application/vnd.github+json" \
    "https://api.github.com/repos/$REPO/issues/$PR_NUMBER/comments?per_page=100&page=$page" \
    > /tmp/comments-page.json || echo '[]' > /tmp/comments-page.json
  result=$(python3 "$script_dir/find_bot_comment.py" < /tmp/comments-page.json)
  existing_id=${result%%|*}
  if [ -n "$existing_id" ] || [ "${result##*|}" != "1" ]; then
    break
  fi
done

payload=$(python3 -c "
import json
with open('$body_file', encoding='utf-8') as f:
    print(json.dumps({'body': f.read()}))
")

if [ -n "$existing_id" ]; then
  code=$(curl -sS -o /dev/null -w '%{http_code}' -X PATCH -H "Authorization: Bearer $GH_TOKEN" -H "Accept: application/vnd.github+json" \
    "https://api.github.com/repos/$REPO/issues/comments/$existing_id" -d "$payload") || code=000
else
  code=$(curl -sS -o /dev/null -w '%{http_code}' -X POST -H "Authorization: Bearer $GH_TOKEN" -H "Accept: application/vnd.github+json" \
    "https://api.github.com/repos/$REPO/issues/$PR_NUMBER/comments" -d "$payload") || code=000
fi
case "$code" in
  2??) ;;
  *) echo "::warning::The report comment (marker $MARKER) was not posted (HTTP $code)." ;;
esac
