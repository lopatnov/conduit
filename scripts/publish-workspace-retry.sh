#!/usr/bin/env bash
# Runs scripts/publish-workspace.sh (from the checkout given as $1) until every
# workspace crate is on crates.io, sleeping through crates.io's new-crate rate
# limit instead of failing on it.
#
# crates.io allows a burst of about 5 new crates, then roughly one per 10
# minutes, and answers 429 with "try again after <RFC 2822 date>". The inner
# script is safe to re-run (it skips anything already published), so this loop
# only has to wait for the stated time and call it again. Any failure that is
# not a 429 is a real error and stops the loop.
#
# Requires CARGO_REGISTRY_TOKEN in the environment.
set -euo pipefail

src_dir=${1:?usage: publish-workspace-retry.sh <checkout-dir> [deadline-seconds]}
deadline_secs=${2:-20400} # 5h40m: stays under GitHub's 6 h job limit
inner="$src_dir/scripts/publish-workspace.sh"
[[ -x "$inner" ]] || { echo "missing or not executable: $inner" >&2; exit 1; }

start=$(date +%s)
log=$(mktemp)
trap 'rm -f "$log"' EXIT

while true; do
    if "$inner" 2>&1 | tee "$log"; then
        echo "All workspace crates are published."
        exit 0
    fi
    if ! grep -Eq "429 Too Many Requests|HTTP 429" "$log"; then
        echo "Publish failed for a reason other than the crates.io rate limit." >&2
        exit 1
    fi
    retry_at=$(grep -o "try again after [A-Za-z]*, [0-9]* [A-Za-z]* [0-9]* [0-9:]* GMT" "$log" \
        | tail -1 | sed 's/^try again after //' || true)
    now=$(date +%s)
    target=""
    if [[ -n "$retry_at" ]]; then
        target=$(date -u -d "$retry_at" +%s 2>/dev/null || true)
    fi
    # No parsable time (or one already past): the new-crate limit refills one
    # crate per ~10 minutes; any other 429 (API crawler, new-version bucket) is
    # much shorter, so retry sooner.
    fallback=90
    grep -q "new crates" "$log" && fallback=600
    if [[ -z "$target" || "$target" -le "$now" ]]; then
        target=$((now + fallback))
    fi
    wait=$((target - now + 15))
    if (( now - start + wait > deadline_secs )); then
        echo "Out of time budget; re-run this workflow to continue." >&2
        exit 1
    fi
    echo "Rate limited by crates.io; sleeping ${wait}s (until $(date -u -d "@$((now + wait))" '+%H:%M:%S') UTC)."
    sleep "$wait"
done
