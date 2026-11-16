#!/usr/bin/env bash
# Keep THIRD-PARTY-LICENSES.txt current on Dependabot's Cargo pull requests (issue #780).
#
#   scripts/ci/dependabot_licenses.sh <pr-number>...     # one or more PRs
#   scripts/ci/dependabot_licenses.sh                    # every open Dependabot PR on a dependabot/cargo/* branch
#
# A version bump changes Cargo.lock, so the `licenses` job of ci.yml (`third_party_licenses.py --check`) fails until
# the file is regenerated, and Dependabot cannot do that. For each PR this script:
#
#   1. checks that it is Dependabot's, from this repository, on a dependabot/cargo/* branch, and touches nothing but
#      Cargo.toml / Cargo.lock files (and the licence file itself): anything else is skipped, because the job runs with
#      a write token on code from the PR's branch;
#   2. brings the branch up to date with main when it is behind (the server-side "Update branch"), so that the file is
#      generated for the same Cargo.lock that CI tests;
#   3. runs the generator *from main* (scripts/third_party_licenses.py and ci/ of the trusted checkout, never the PR's
#      copy) on the branch's Cargo.lock; `--write` refuses, as `--check` does, a crate outside ci/license-policy.json, so
#      a bump that brings in a licence the policy does not allow is never papered over: the PR stays red;
#   4. commits the file to the branch when it changed, and starts ci.yml on the branch through `workflow_dispatch`
#      (a push made with GITHUB_TOKEN starts no `pull_request` run by design, so without this the new head has no CI).
#
# Nothing from the PR's branch is executed: `cargo fetch` and `cargo metadata` read Cargo.toml / Cargo.lock and run no
# build script, and the file check in step 1 keeps a `.cargo/config.toml` (which could name a `rustc` wrapper) out.
#
# Env: GH_TOKEN (contents: write, pull-requests: write, actions: write), GITHUB_REPOSITORY, TRUSTED (path of the main
# checkout, default: this repository), WORK (scratch dir, default: mktemp), DRY_RUN=1 (read and generate, push and
# dispatch nothing). Exit 1 when a PR could not be handled; the others are still processed.
set -euo pipefail

REPO="${GITHUB_REPOSITORY:?GITHUB_REPOSITORY is required (owner/name)}"
TRUSTED="${TRUSTED:-$(cd "$(dirname "$0")/../.." && pwd)}"
WORK="${WORK:-$(mktemp -d)}"
DRY_RUN="${DRY_RUN:-0}"
BOT_NAME="github-actions[bot]"
BOT_EMAIL="41898282+github-actions[bot]@users.noreply.github.com"
LICENSE_FILE="THIRD-PARTY-LICENSES.txt"

note() { echo "::notice title=Dependabot licences::$*"; }
fail() { echo "::error title=Dependabot licences::$*"; }

auth_b64() { printf 'x-access-token:%s' "${GH_TOKEN:?GH_TOKEN is required}" | base64 -w0; }

# Paths a Dependabot Cargo PR may touch for this job to act on it.
allowed_path() {
  [[ "$1" != *..* ]] || return 1
  [[ "$1" =~ ^([A-Za-z0-9._-]+/)*Cargo\.(toml|lock)$ ]] || [ "$1" = "$LICENSE_FILE" ]
}

handle_pr() {
  local n="$1" pr login state head_repo ref sha base
  [[ "$n" =~ ^[0-9]+$ ]] || { fail "not a PR number: $n"; return 1; }
  pr=$(gh api "repos/$REPO/pulls/$n")
  login=$(jq -r '.user.login' <<<"$pr")
  state=$(jq -r '.state' <<<"$pr")
  head_repo=$(jq -r '.head.repo.full_name' <<<"$pr")
  ref=$(jq -r '.head.ref' <<<"$pr")
  sha=$(jq -r '.head.sha' <<<"$pr")
  base=$(jq -r '.base.ref' <<<"$pr")
  if [ "$login" != "dependabot[bot]" ] || [ "$state" != "open" ] || [ "$head_repo" != "$REPO" ] \
     || [[ "$ref" != dependabot/cargo/* ]] || [ "$base" != "main" ]; then
    note "#$n skipped: not an open Dependabot Cargo PR into main from this repository"
    return 0
  fi
  [[ "$sha" =~ ^[0-9a-f]{40}$ ]] || { fail "#$n: unexpected head sha"; return 1; }

  local f
  while IFS= read -r f; do
    if ! allowed_path "$f"; then
      note "#$n skipped: it changes $f, not only Cargo.toml / Cargo.lock (regenerate the file by hand)"
      return 0
    fi
  done < <(gh api --paginate "repos/$REPO/pulls/$n/files" --jq '.[].filename')

  # 2. Up to date with main, so the file is made for the Cargo.lock CI tests (the merge result).
  local behind updated=0
  behind=$(gh api "repos/$REPO/compare/main...$sha" --jq '.behind_by')
  if [ "$behind" != "0" ]; then
    if [ "$DRY_RUN" = "1" ]; then
      note "#$n is $behind commit(s) behind main (dry run: not updating the branch)"
    elif gh api -X PUT "repos/$REPO/pulls/$n/update-branch" -f expected_head_sha="$sha" >/dev/null; then
      local _ new="$sha"
      for _ in $(seq 1 30); do
        new=$(gh api "repos/$REPO/pulls/$n" --jq '.head.sha')
        [ "$new" != "$sha" ] && break
        sleep 2
      done
      if [ "$new" = "$sha" ]; then fail "#$n: the branch update did not land within a minute"; return 1; fi
      sha="$new"
      updated=1
      note "#$n was $behind commit(s) behind main: branch updated to $sha"
    else
      fail "#$n is behind main and the branch could not be updated (a conflict, or main changed a workflow file," \
        "which GITHUB_TOKEN may not write): comment \"@dependabot rebase\" on the PR and this job runs again"
      return 1
    fi
  fi

  # 3. Generate with main's generator and policy on the branch's Cargo.lock.
  local dir="$WORK/pr-$n"
  rm -rf "$dir"
  git init -q "$dir"
  git -C "$dir" -c http.extraheader="AUTHORIZATION: basic $(auth_b64)" fetch -q --depth 1 \
    "https://github.com/$REPO.git" "$sha"
  git -C "$dir" checkout -q FETCH_HEAD
  cp "$TRUSTED/scripts/third_party_licenses.py" "$dir/scripts/third_party_licenses.py"
  rm -rf "$dir/ci" && cp -R "$TRUSTED/ci" "$dir/ci"
  (cd "$dir" && cargo fetch --locked -q)
  if ! python3 "$dir/scripts/third_party_licenses.py" --write "$dir/$LICENSE_FILE"; then
    fail "#$n: the licence policy rejects a crate in this bump; the file is not regenerated (see the log above)"
    return 1
  fi
  if git -C "$dir" diff --quiet -- "$LICENSE_FILE"; then
    note "#$n: $LICENSE_FILE is already current"
    if [ "$updated" = "1" ]; then dispatch_ci "$n" "$ref"; fi
    return 0
  fi

  # 4. Commit the file alone (the copied generator and policy stay out of the commit).
  git -C "$dir" -c user.name="$BOT_NAME" -c user.email="$BOT_EMAIL" commit -q -m \
    "chore(deps): regenerate $LICENSE_FILE" -m "Generated by scripts/ci/dependabot_licenses.sh for #$n." -- "$LICENSE_FILE"
  if [ "$DRY_RUN" = "1" ]; then
    note "#$n: $LICENSE_FILE changes (dry run: not pushed)"
    git -C "$dir" show --stat HEAD | tail -3
    return 0
  fi
  # --force-with-lease against the sha just used: a push by Dependabot in between makes this fail instead of being lost.
  git -C "$dir" -c http.extraheader="AUTHORIZATION: basic $(auth_b64)" push -q \
    --force-with-lease="refs/heads/$ref:$sha" "https://github.com/$REPO.git" "HEAD:refs/heads/$ref"
  note "#$n: $LICENSE_FILE regenerated and pushed to $ref"
  dispatch_ci "$n" "$ref"
}

dispatch_ci() {
  if [ "$DRY_RUN" = "1" ]; then return 0; fi
  gh workflow run ci.yml --repo "$REPO" --ref "$2"
  note "#$1: CI started on $2"
}

prs=("$@")
if [ "${#prs[@]}" -eq 0 ]; then
  mapfile -t prs < <(gh api --paginate "repos/$REPO/pulls?state=open&base=main&per_page=100" \
    --jq '.[] | select(.user.login == "dependabot[bot]" and (.head.ref | startswith("dependabot/cargo/"))) | .number')
fi
status=0
for n in "${prs[@]}"; do
  # A subshell outside any `||`, so that `set -e` holds inside handle_pr (bash ignores it in a function called from
  # `cmd || ...`) and one PR's failure stops that PR only.
  set +e
  (set -e; handle_pr "$n")
  rc=$?
  set -e
  [ "$rc" -eq 0 ] || status=1
done
exit "$status"
