#!/usr/bin/env bash
# Everything reviewers and bots said on a pull request, in one read.
#
#   scripts/pr-comments.sh PR [--since ISO8601] [--full] [--repo OWNER/REPO]
#
# Prints, for the PR's current head: its state, every check that is not green (pending ones are
# flagged — a bot that is still "in progress" has not spoken yet), then all three comment streams
# from every author — issue comments, reviews, inline review comments — newest last. GitHub's PR
# page hides most of this behind "resolved"/"outdated" folds, and a green check list says nothing
# about it; the merge rule in `.claude/commands/feature-workspace-cycle.md` (Step 7) is that every
# comment has been READ, so read this output to the end.
#
#   --since TS   only items created/submitted at or after TS (ISO 8601, e.g. 2026-09-26T19:00:00Z)
#   --full       print whole bodies (default: first 900 characters of each)
#   --repo R     OWNER/REPO (default: the current directory's repository)
#
# Needs an authenticated `gh`. Read-only.
set -uo pipefail

usage() { sed -n '2,17p' "$0" | sed 's/^# \{0,1\}//'; exit "${1:-2}"; }

PR=""; SINCE="0000-00-00T00:00:00Z"; LIMIT=900; REPO=""
while [ $# -gt 0 ]; do
  case "$1" in
    # `shift 2` with one argument left shifts nothing and the loop would see the option again for ever.
    --since) [ $# -ge 2 ] || { echo "--since needs a value" >&2; usage; }; SINCE="$2"; shift 2 ;;
    --full) LIMIT=1000000; shift ;;
    --repo) [ $# -ge 2 ] || { echo "--repo needs a value" >&2; usage; }; REPO="$2"; shift 2 ;;
    -h|--help) usage 0 ;;
    -*) echo "unknown option: $1" >&2; usage ;;
    *) [ -z "$PR" ] && PR="$1" || { echo "unexpected argument: $1" >&2; usage; }; shift ;;
  esac
done
[ -n "$PR" ] || usage
case "$PR" in *[!0-9]*) echo "PR must be a number, got: $PR" >&2; exit 2 ;; esac
# SINCE is interpolated into a jq program below; accept only characters an ISO timestamp has.
case "$SINCE" in *[!0-9TZ:.+-]*) echo "--since must look like 2026-09-26T19:00:00Z" >&2; exit 2 ;; esac

[ -n "$REPO" ] || REPO=$(gh repo view --json nameWithOwner -q .nameWithOwner) || exit 1

# Trim, drop carriage returns, keep the body on its own indented lines.
BODY='(.body // "") | gsub("\r"; "") | .[0:'"$LIMIT"'] | gsub("\n"; "\n    ")'

echo "== PR #$PR in $REPO =="
gh pr view "$PR" -R "$REPO" --json title,state,baseRefName,headRefOid,mergeStateStatus \
  --jq '"title:  " + .title, "state:  " + .state + "  merge: " + .mergeStateStatus + "  base: " + .baseRefName, "head:   " + .headRefOid'

echo
echo "== checks that are not green =="
# `gh pr checks` exits 8 while checks are pending and 1 when some failed, and prints its tab-separated table either
# way; anything without a table (auth or network error, bad repo, "no checks reported") is a real failure and must
# not be shown as a green list (a false "all passing" on a script meant to gate merges).
RAW=$(gh pr checks "$PR" -R "$REPO" 2>&1); RC=$?
if ! printf '%s\n' "$RAW" | grep -q "$(printf '\t')"; then
  echo "  !! could not read the checks (gh exit $RC): ${RAW:-no output}"
  CHECKS=""
else
  CHECKS=$(printf '%s\n' "$RAW" | awk -F'\t' 'NF > 1 && $2 != "pass" && $2 != "skipping" {print "  " $2 "\t" $1}')
  if [ -z "$CHECKS" ]; then echo "  (all passing)"; else printf '%s\n' "$CHECKS"; fi
fi
if printf '%s\n' "$CHECKS" | grep -qiE 'pending|queued|in_progress'; then
  echo "  !! something is still running — a bot that has not finished has not commented yet; run this again"
fi

section() { # section TITLE ENDPOINT JQ
  local title="$1" endpoint="$2" filter="$3" out
  out=$(gh api --paginate "$endpoint" --jq "$filter" 2>&1) || { echo "  (could not read $endpoint: $out)"; return; }
  echo
  echo "== $title =="
  if [ -z "$out" ]; then echo "  (none)"; else printf '%s\n' "$out"; fi
}

section "issue comments (since $SINCE)" "repos/$REPO/issues/$PR/comments?per_page=100" \
  '.[] | select(.created_at >= "'"$SINCE"'") | "-- " + .user.login + "  " + .created_at + "  #" + (.id|tostring) + "\n    " + ('"$BODY"')'

section "reviews (since $SINCE)" "repos/$REPO/pulls/$PR/reviews?per_page=100" \
  '.[] | select((.submitted_at // "") >= "'"$SINCE"'") | "-- " + .user.login + "  " + .state + "  " + (.submitted_at // "") + "  on " + (.commit_id[0:8]) + "\n    " + ('"$BODY"')'

section "inline review comments (since $SINCE)" "repos/$REPO/pulls/$PR/comments?per_page=100" \
  '.[] | select(.created_at >= "'"$SINCE"'") | "-- " + .user.login + "  " + .path + ":" + ((.line // .original_line // 0)|tostring) + "  " + .created_at + "  on " + (.commit_id[0:8]) + (if .in_reply_to_id then "  (reply to #" + (.in_reply_to_id|tostring) + ")" else "" end) + "\n    " + ('"$BODY"')'

echo
echo "== counts (all time) =="
printf '  issue comments: %s   reviews: %s   inline comments: %s\n' \
  "$(gh api --paginate "repos/$REPO/issues/$PR/comments?per_page=100" --jq '.[].id' | wc -l | tr -d ' ')" \
  "$(gh api --paginate "repos/$REPO/pulls/$PR/reviews?per_page=100" --jq '.[].id' | wc -l | tr -d ' ')" \
  "$(gh api --paginate "repos/$REPO/pulls/$PR/comments?per_page=100" --jq '.[].id' | wc -l | tr -d ' ')"
