#!/usr/bin/env bash
# Local verification chain for a refactor/extraction PR — the checks that a `cargo test` alone does not give
# (feature matrix, before/after comparisons, the golden output of config validation). Run it once on the final
# head; it writes a summary to target/verify-local/summary.txt and exits non-zero if any step FAILs.
#
#   scripts/verify-local.sh [--base REV] [--quick] [--skip STEP,STEP] [--moved-to PKG[:STRIP]] [--also-pkg PKG]
#
#   --base REV          revision the before/after comparisons use (default: the merge-base of HEAD with the
#                       migration branch on origin, else with origin/main)
#   --quick             only leak, deps, lists, clippy (skip tests, goldens, hack)
#   --skip STEPS        comma list of: leak deps lists clippy tests goldens hack
#   --moved-to PKG[:S]  tests were moved out of the root package into workspace package PKG: the head list is taken over root + PKG, so a test that
#                       disappears from the root list must appear in PKG's list (after removing the prefix S,
#                       e.g. `config::`, from its name) — otherwise it is a FAIL
#   --also-pkg PKG      also run PKG next to the root package in the `--features full` / `static-server` test runs
#
# Steps: leak (CI's ci-no-proxy dependency-leak check), deps (unique crates of `cargo tree -e normal` before/after,
# per profile), lists (`cargo test -- --list` before/after on default/standard/full), clippy (-D warnings on 8
# profiles and `--workspace --tests`), tests (workspace / full / static-server), goldens (the validate golden
# tests in 10 named sets + every single feature + a depth-2 powerset), hack (`cargo hack --workspace --each-feature`
# and a depth-2 powerset over the interacting features).
#
# Do not `git add`/commit while the hack step runs: cargo-hack rewrites Cargo.toml files until it exits.
# The baseline (--base) is built in a temporary worktree under target/verify-local/ and cached per revision.
set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
ROOT="$PWD"
OUT="$ROOT/target/verify-local"
SUMMARY="$OUT/summary.txt"
mkdir -p "$OUT"

BASE=""
SKIP=","
MOVED_TO=""
STRIP=""
ALSO_PKG=""
while [ $# -gt 0 ]; do
  case "$1" in
    --base) BASE="$2"; shift 2 ;;
    --quick) SKIP=",tests,goldens,hack,"; shift ;;
    --skip) SKIP=",$2,"; shift 2 ;;
    --moved-to) MOVED_TO="${2%%:*}"; [ "$2" != "${2#*:}" ] && STRIP="${2#*:}"; shift 2 ;;
    --also-pkg) ALSO_PKG="$2"; shift 2 ;;
    -h|--help) sed -n '2,25p' "$0"; exit 0 ;;
    *) echo "unknown option: $1" >&2; exit 2 ;;
  esac
done
if [ -z "$BASE" ]; then
  BASE="$(git merge-base HEAD origin/claude/cargo-workspace-features-23qxfr 2>/dev/null || git merge-base HEAD origin/main)"
fi
BASE_SHA="$(git rev-parse "$BASE")"
HEAD_SHA="$(git rev-parse HEAD)"
skipped() { case "$SKIP" in *",$1,"*) return 0 ;; esac; return 1; }

FAILS=0
: > "$SUMMARY"
say() { echo "$*" | tee -a "$SUMMARY"; }
result() { # result LABEL PASS|FAIL DETAIL
  say "$2  $1${3:+ — $3}"
  [ "$2" = FAIL ] && FAILS=$((FAILS + 1))
}
DIRTY_BEFORE=$(git status --porcelain | wc -l)
say "verify-local: HEAD=$HEAD_SHA base=$BASE_SHA dirty=$DIRTY_BEFORE started=$(date -u +%H:%M:%S)"

# Normal-dependency crate set of a checkout, one line per unique crate (paths and versions of workspace crates stripped).
crates_of() { # crates_of DIR PROFILE-ARGS...
  local dir="$1"; shift
  (cd "$dir" && CARGO_TARGET_DIR="$OUT/target-base" cargo tree -p lopatnov-conduit "$@" -e normal --prefix none 2>/dev/null | sed 's/ (.*//' | sort -u)
}

# ---- 1. leak -------------------------------------------------------------------------------------------------
if ! skipped leak; then
  leak_ok=1
  for features in static static-server upload tcp; do
    crates=$(cargo tree -p lopatnov-conduit --no-default-features --features "$features" -e normal --prefix none | sed 's/ v.*//' | sort -u)
    if ! printf '%s\n' "$crates" | grep -x 'lopatnov-conduit' >/dev/null; then leak_ok=0; say "  no usable crate list for $features"; continue; fi
    leaked=$(printf '%s\n' "$crates" | grep -xE 'hmac|sha2|reqwest|tower-http|url|idna|icu_.*' || [ "$?" -eq 1 ])
    if [ -n "$leaked" ]; then leak_ok=0; say "  $features leaked: $leaked"; fi
  done
  result "leak check (static, static-server, upload, tcp)" "$([ $leak_ok = 1 ] && echo PASS || echo FAIL)"
fi

# ---- baseline worktree (deps, lists) ------------------------------------------------------------------------------
# The baseline is built with its OWN target directory. Cargo hashes a workspace member's artifacts by its path relative to the
# workspace root, so two checkouts of this repo sharing one target directory collide: the head build then silently reuses the
# baseline's stale rlibs ("cannot find `scheme` in `conduit_config_core`" with the new source sitting right there).
BASE_DIR="$OUT/base-${BASE_SHA:0:12}"
need_base=0
skipped deps || need_base=1
skipped lists || need_base=1
if [ "$need_base" = 1 ] && [ ! -d "$BASE_DIR/.git" ] && [ ! -f "$BASE_DIR/.git" ]; then
  git worktree add --detach "$BASE_DIR" "$BASE_SHA" >/dev/null 2>&1 || { say "cannot create the baseline worktree at $BASE_DIR"; exit 2; }
fi

# ---- 2. deps -------------------------------------------------------------------------------------------------
if ! skipped deps; then
  for spec in "default:" "none:--no-default-features" "full:--features full" "forward-auth:--no-default-features --features forward-auth"; do
    name="${spec%%:*}"; args="${spec#*:}"
    # shellcheck disable=SC2086
    before=$(crates_of "$BASE_DIR" $args); after=$(crates_of "$ROOT" $args)
    only_after=$(comm -13 <(printf '%s\n' "$before") <(printf '%s\n' "$after") | tr '\n' ' ')
    only_before=$(comm -23 <(printf '%s\n' "$before") <(printf '%s\n' "$after") | tr '\n' ' ')
    if [ -z "$only_after$only_before" ]; then
      result "root dependency set [$name]" PASS "identical, $(printf '%s\n' "$after" | wc -l) crates"
    else
      # a new workspace crate is expected when the PR adds one; anything third-party is not
      third_party=$(printf '%s\n' $only_after $only_before | grep -v '^lopatnov-conduit' | tr '\n' ' ')
      result "root dependency set [$name]" "$([ -z "$third_party" ] && echo PASS || echo FAIL)" "added: ${only_after:-none} removed: ${only_before:-none}"
    fi
  done
fi

# ---- 3. lists ------------------------------------------------------------------------------------------------
list_tests() { # list_tests DIR OUTFILE PROFILE-ARGS...
  local dir="$1" out="$2"; shift 2
  (cd "$dir" && CARGO_TARGET_DIR="$OUT/target-base" cargo test "$@" -- --list </dev/null 2>/dev/null | grep -E ': test$' | sort) > "$out"
}
if ! skipped lists; then
  # With --moved-to the head list is taken over BOTH packages in the same profile (the moved tests only exist under the
  # features the root forwards to the other package, so the other package's default-feature list would miss most of them).
  head_pkgs=""
  [ -n "$MOVED_TO" ] && head_pkgs="-p lopatnov-conduit -p $MOVED_TO"
  for spec in "default:" "standard:--features standard" "full:--features full"; do
    name="${spec%%:*}"; args="${spec#*:}"
    cache="$OUT/list-${BASE_SHA:0:12}-$name.txt"
    # shellcheck disable=SC2086
    [ -s "$cache" ] || list_tests "$BASE_DIR" "$cache" $args
    # shellcheck disable=SC2086
    list_tests "$ROOT" "$OUT/list-head-$name.txt" $head_pkgs $args
    removed=$(comm -23 "$cache" "$OUT/list-head-$name.txt"); added=$(comm -13 "$cache" "$OUT/list-head-$name.txt")
    nrem=$(printf '%s' "$removed" | grep -c . || true); nadd=$(printf '%s' "$added" | grep -c . || true)
    uncovered="$removed"
    if [ -n "$STRIP" ] && [ -n "$removed" ]; then
      # a test that was renamed by the move: found again once STRIP is removed from its old name
      uncovered=$(printf '%s\n' "$removed" | while IFS= read -r line; do
        n="${line#"$STRIP"}"
        grep -qxF "$n" "$OUT/list-head-$name.txt" || echo "$line"
      done)
    fi
    if [ -z "$uncovered" ]; then
      result "test list [$name]" PASS "$(wc -l < "$cache") -> $(wc -l < "$OUT/list-head-$name.txt"): $nrem missing from the root-only list${MOVED_TO:+ (none missing across root + $MOVED_TO)}, $nadd added"
    else
      result "test list [$name]" FAIL "$(printf '%s\n' "$uncovered" | grep -c .) test(s) disappeared: $(printf '%s\n' "$uncovered" | head -3 | tr '\n' ' ')"
    fi
  done
fi

# ---- 4. clippy -----------------------------------------------------------------------------------------------
if ! skipped clippy; then
  run_clippy() { # run_clippy LABEL ARGS...
    local label="$1"; shift
    if cargo clippy "$@" -- -D warnings >"$OUT/clippy-$label.log" 2>&1 </dev/null; then result "clippy [$label]" PASS
    else result "clippy [$label]" FAIL "see $OUT/clippy-$label.log"; fi
  }
  run_clippy default --all-targets
  run_clippy no-default --all-targets --no-default-features
  run_clippy static-server --all-targets --no-default-features --features static-server
  run_clippy gateway --all-targets --no-default-features --features gateway
  run_clippy cache --all-targets --no-default-features --features cache
  run_clippy static-server-upload --all-targets --no-default-features --features static-server,upload
  run_clippy full --all-targets --features full
  run_clippy consumers-only --all-targets --no-default-features --features consumers
  run_clippy workspace-tests --workspace --tests
fi

# ---- 5. tests ------------------------------------------------------------------------------------------------
if ! skipped tests; then
  run_tests() { # run_tests LABEL ARGS...
    local label="$1"; shift
    cargo test "$@" >"$OUT/test-$label.log" 2>&1 </dev/null; local rc=$?
    local totals; totals=$(grep -E '^test result' "$OUT/test-$label.log" | awk '{p+=$4; f+=$6} END {print p" passed, "f" failed"}')
    result "cargo test [$label]" "$([ $rc = 0 ] && echo PASS || echo FAIL)" "$totals"
  }
  pkgs=""; [ -n "$ALSO_PKG" ] && pkgs="-p lopatnov-conduit -p $ALSO_PKG"
  run_tests workspace --workspace
  # shellcheck disable=SC2086
  run_tests full $pkgs --features full
  # shellcheck disable=SC2086
  run_tests static-server $pkgs --no-default-features --features static-server
fi

# ---- 6. goldens ----------------------------------------------------------------------------------------------
if ! skipped goldens; then
  golden() { # golden LABEL ARGS...
    local label="$1"; shift
    if cargo test -p lopatnov-conduit --lib "$@" -- golden >"$OUT/golden-$label.log" 2>&1 </dev/null; then result "golden [$label]" PASS
    else result "golden [$label]" FAIL "see $OUT/golden-$label.log"; fi
  }
  golden default
  golden no-default --no-default-features
  golden consumers --no-default-features --features consumers
  golden jwt --no-default-features --features jwt
  golden standard --features standard
  golden full --features full
  golden static-server --no-default-features --features static-server
  golden gateway --no-default-features --features gateway
  golden redis-cache --no-default-features --features proxy,redis,cache
  golden consumers-jwt --no-default-features --features consumers,jwt
  FEATS=proxy,compression,static,hotreload,jwt,consumers,forward-auth,rhai,wasm,tcp,upload,redis,cache,acme,fault-injection,otlp
  if cargo hack test -p lopatnov-conduit --lib --each-feature --keep-going --include-features "$FEATS" -- golden >"$OUT/golden-each.log" 2>&1 </dev/null
  then result "golden [each feature]" PASS "$(grep -c '^info: running' "$OUT/golden-each.log") combinations"
  else result "golden [each feature]" FAIL "see $OUT/golden-each.log"; fi
  if cargo hack test -p lopatnov-conduit --lib --feature-powerset --depth 2 --keep-going --include-features proxy,cache,redis,consumers,jwt,forward-auth,tcp,static -- golden >"$OUT/golden-depth2.log" 2>&1 </dev/null
  then result "golden [depth-2 powerset]" PASS "$(grep -c '^info: running' "$OUT/golden-depth2.log") combinations"
  else result "golden [depth-2 powerset]" FAIL "see $OUT/golden-depth2.log"; fi
fi

# ---- 7. hack (rewrites manifests while it runs: no git add / commit until it exits) ------------------------------
if ! skipped hack; then
  if cargo hack check --workspace --each-feature --no-dev-deps >"$OUT/hack-each.log" 2>&1 </dev/null
  then result "cargo hack --workspace --each-feature" PASS "$(grep -c '^info: running' "$OUT/hack-each.log") combinations"
  else result "cargo hack --workspace --each-feature" FAIL "see $OUT/hack-each.log"; fi
  if cargo hack check -p lopatnov-conduit --feature-powerset --depth 2 --no-dev-deps --include-features forward-auth,proxy,cache,jwt,consumers >"$OUT/hack-powerset.log" 2>&1 </dev/null
  then result "cargo hack depth-2 powerset (forward-auth, proxy, cache, jwt, consumers)" PASS "$(grep -c '^info: running' "$OUT/hack-powerset.log") combinations"
  else result "cargo hack depth-2 powerset" FAIL "see $OUT/hack-powerset.log"; fi
  after=$(git status --porcelain | wc -l)
  result "worktree unchanged by hack" "$([ "$after" = "$DIRTY_BEFORE" ] && echo PASS || echo FAIL)" "$after changed paths, $DIRTY_BEFORE before"
fi

say "verify-local: done $(date -u +%H:%M:%S), $FAILS FAIL"
[ "$FAILS" = 0 ]
