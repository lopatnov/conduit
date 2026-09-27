#!/usr/bin/env bash
# Publishes every workspace crate (the root `lopatnov-conduit` plus all
# `lopatnov-conduit-*` members under crates/) to crates.io in dependency
# order, via native `cargo publish --workspace` (issue #114/#148 — see that
# issue's PR for why not cargo-workspaces/release-plz).
#
# Safe to re-run: before publishing, it checks crates.io for each package at
# the workspace's current version and skips (--exclude) anything already
# there. This matters because crates.io rate-limits *new* crate creation
# (roughly one new crate per ~10 minutes after an initial burst, last
# verified against crates.io's own docs at the time this script was
# written — reconfirm if publishing starts failing with 429s) — a first
# real 2.0.0 publish of ~33 crates can plausibly need more than one CI job
# run to finish, and this script makes a second run a no-op for whatever
# already landed rather than an error.
#
# Requires CARGO_REGISTRY_TOKEN in the environment (cargo reads it directly;
# not passed as a flag so it never appears in process listings/logs).
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

workspace_version=$(grep -m1 -E '^version *= *"[^"]+"' Cargo.toml | sed -E 's/^version *= *"([^"]+)"/\1/')
if [[ -z "$workspace_version" ]]; then
    echo "Could not read [workspace.package].version from Cargo.toml" >&2
    exit 1
fi
echo "Target version: $workspace_version"

# Root package name + every lopatnov-conduit-* workspace-dependency entry —
# same source scripts/check-workspace-versions.sh reads, so the two scripts
# can never disagree about which packages exist.
root_name=$(grep -m1 -E '^name *= *"[^"]+"' Cargo.toml | sed -E 's/^name *= *"([^"]+)"/\1/')
# Only the [workspace.dependencies] path-dependency declarations
# ("name = { path = ...") -- NOT the root [dependencies] section's
# "name.workspace = true" forwarding lines, which match the same
# ^lopatnov-conduit-<x> prefix but aren't a distinct package name to list.
member_names=$(grep -oE '^lopatnov-conduit-[a-z0-9-]+ *= *\{ *path' Cargo.toml \
    | sed -E 's/ *= *\{ *path//')
all_names="$root_name
$member_names"

exclude_args=()
already_published=0
to_publish=0
for name in $all_names; do
    # crates.io returns 200 for GET /api/v1/crates/<name>/<version> only if
    # that exact version exists, and 404 if the crate or the version is
    # unknown. Only 404 means "needs publishing" -- anything else (a 429
    # from crates.io's ~1 req/s API crawler policy, a 5xx, curl's own `000`
    # on a network error) is NOT the same as "unpublished": treating it that
    # way would skip --exclude-ing an already-published crate, and
    # `cargo publish --workspace` would then hard-fail with "crate already
    # exists" -- exactly the partial-rerun case this script exists to
    # handle safely. --retry/--max-time absorb transient failures; anything
    # that still isn't 200 or 404 after retrying aborts the whole run rather
    # than guessing (found by gitar-bot review on PR #498).
    status=$(curl -s --retry 3 --retry-all-errors --max-time 20 -o /dev/null -w "%{http_code}" \
        "https://crates.io/api/v1/crates/${name}/${workspace_version}" \
        -H "User-Agent: conduit-release-ci (https://github.com/lopatnov/conduit)")
    sleep 1
    if [[ "$status" == "200" ]]; then
        echo "  already published: $name @ $workspace_version — skipping"
        exclude_args+=("--exclude" "$name")
        already_published=$((already_published + 1))
    elif [[ "$status" == "404" ]]; then
        to_publish=$((to_publish + 1))
    else
        echo "crates.io returned HTTP $status for $name — aborting (not a 200/404, so publish state is unknown)" >&2
        exit 1
    fi
done

echo ""
echo "$already_published already published, $to_publish to publish."

if [[ "$to_publish" -eq 0 ]]; then
    echo "Nothing left to publish — every crate is already at $workspace_version."
    exit 0
fi

set -x
cargo publish --workspace --no-verify "${exclude_args[@]}"
