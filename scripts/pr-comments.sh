#!/usr/bin/env bash
# Everything reviewers and bots said on a pull request, in one read.
#
#   scripts/pr-comments.sh PR [--since ISO8601] [--full] [--repo OWNER/REPO]
#
# Prints, for the PR's current head: its state, every check that is not green (pending ones are
# flagged — a bot that is still "in progress" has not spoken yet), then all three comment streams
# from every author — issue comments, reviews, inline review comments — newest last. GitHub's PR
# page hides most of this behind "resolved"/"outdated" folds, and a green check list says nothing
# about it; the merge rule in `.claude/commands/issue.md` (Step 6) is that every
# comment has been READ, so read this output to the end.
#
#   --since TS   only items created/submitted at or after TS (ISO 8601, e.g. 2026-09-26T19:00:00Z)
#   --full       print whole bodies (default: first 900 characters of each)
#   --repo R     OWNER/REPO (default: the current directory's repository)
#
# Needs an authenticated `gh` (REST only: works where GraphQL is refused, e.g. a cloud container). Read-only.
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

# `gh repo view`, `gh pr view` and `gh pr checks` use GraphQL, which a cloud container refuses ("GitHub GraphQL is not
# available from Claude Code sessions"); the repository falls back to the origin URL and the rest is read over REST.
if [ -z "$REPO" ]; then
  REPO=$(gh repo view --json nameWithOwner -q .nameWithOwner 2>/dev/null) \
    || REPO=$(git remote get-url origin 2>/dev/null | sed -E 's#/+$##; s#\.git$##; s#^.*[:/]([^/:]+/[^/]+)$#\1#')
fi
[[ $REPO =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]] \
  || { echo "cannot tell the repository from gh or from the origin URL; pass --repo OWNER/REPO" >&2; exit 2; }

# Trim, drop carriage returns, keep the body on its own indented lines.
BODY='(.body // "") | gsub("\r"; "") | .[0:'"$LIMIT"'] | gsub("\n"; "\n    ")'

echo "== PR #$PR in $REPO =="
# One read gives the header and the head commit the checks below are asked about, so the two cannot name different commits.
# A failed read leaves no head commit, and so does an answer that is not a commit id: either way the run stops here.
HEADER=$(gh api "repos/$REPO/pulls/$PR" \
  --jq '"title:  " + .title, "state:  " + .state + (if .merged then " (merged)" else "" end) + "  merge: " + (.mergeable_state // "unknown") + "  base: " + .base.ref, "head:   " + .head.sha')
HEAD=$(printf '%s\n' "$HEADER" | sed -n 's/^head:   //p')
case "$HEAD" in ""|*[!0-9a-f]*) echo "could not read the head commit of PR #$PR" >&2; exit 1 ;; esac
printf '%s\n' "$HEADER"

echo
echo "== checks that are not green =="
# The head commit's check runs (Actions jobs, apps) and its legacy commit statuses, as "state<TAB>name" rows. A read that
# fails (auth or network error, bad repo) or finds no check at all is a real failure and must not be shown as a green
# list (a false "all passing" on a script meant to gate merges).
RUNS=$(gh api --paginate "repos/$REPO/commits/$HEAD/check-runs?per_page=100" \
  --jq '.check_runs[] | [(if .status == "completed" then .conclusion else .status end), .name] | @tsv' 2>&1); RC1=$?
STATUSES=$(gh api --paginate "repos/$REPO/commits/$HEAD/status?per_page=100" \
  --jq '.statuses[] | [.state, .context] | @tsv' 2>&1); RC2=$?
ROWS=$(printf '%s\n%s\n' "$RUNS" "$STATUSES" | awk -F'\t' 'NF > 1')  # drops anything gh printed that is not a row
if [ "$RC1" -ne 0 ] || [ "$RC2" -ne 0 ]; then
  echo "  !! could not read the checks (gh exit $RC1/$RC2): $RUNS $STATUSES"
  CHECKS=""
elif [ -z "$ROWS" ]; then
  echo "  !! no check reported for ${HEAD:0:8} yet"
  CHECKS=""
else
  CHECKS=$(printf '%s\n' "$ROWS" | awk -F'\t' '$1 != "success" && $1 != "skipped" && $1 != "neutral" {print "  " $1 "\t" $2}')
  if [ -z "$CHECKS" ]; then echo "  (all passing)"; else printf '%s\n' "$CHECKS"; fi
fi
if printf '%s\n' "$CHECKS" | cut -f1 | grep -qE '^ +(pending|queued|in_progress|waiting|requested)$'; then
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
