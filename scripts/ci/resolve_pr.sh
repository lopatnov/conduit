#!/usr/bin/env bash
# Print `number=<PR number>` and `base_sha=<40-hex sha>` lines (either may be empty) for the report jobs in ci.yml,
# ready to be appended to "$GITHUB_OUTPUT".
#
# On a `pull_request` run both come from the event. On a `push` run (a branch that has a PR but whose CI runs through
# the push trigger) the open PR for this branch is looked up through the API. Both values are validated here -- a PR
# number must be digits only, a base sha 40 lowercase hex characters -- because they are later used in URLs and git
# commands; anything else becomes empty, which the callers treat as "no PR, nothing to post / nothing to diff".
#
# Required env: EVENT_NAME, REPO, REPO_OWNER, REF_NAME, GH_TOKEN (a token that can READ pull requests)
# Event env (empty outside pull_request): EVENT_PR_NUMBER, EVENT_BASE_SHA
# A failed lookup (rate limit, 5xx, no open PR) yields empty values and exit 0: these reports are informational.
set -euo pipefail

number=""
base=""
if [ "${EVENT_NAME:-}" = "pull_request" ]; then
  number="${EVENT_PR_NUMBER:-}"
  base="${EVENT_BASE_SHA:-}"
else
  pr_json=$(curl -sS -H "Authorization: Bearer ${GH_TOKEN:-}" -H "Accept: application/vnd.github+json" \
    "https://api.github.com/repos/${REPO}/pulls?head=${REPO_OWNER}:${REF_NAME}&state=open" || true)
  pair=$(printf '%s' "$pr_json" | python3 -c "
import json, sys
try:
    d = json.load(sys.stdin)
except ValueError:
    d = None
if isinstance(d, list) and d:
    print(d[0].get('number', ''), (d[0].get('base') or {}).get('sha', ''))
" 2>/dev/null || true)
  number="${pair%% *}"
  base="${pair#* }"
  [ "$base" = "$pair" ] && base=""
fi

case "$number" in '' | *[!0-9]*) number="" ;; esac
case "$base" in *[!0-9a-f]*) base="" ;; esac
[ "${#base}" -eq 40 ] || base=""

echo "number=$number"
echo "base_sha=$base"
