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
    # that exact version exists (404 for an unknown crate or unknown
    # version alike — either way, nothing to skip).
    status=$(curl -s -o /dev/null -w "%{http_code}" \
        "https://crates.io/api/v1/crates/${name}/${workspace_version}" \
        -H "User-Agent: conduit-release-ci (https://github.com/lopatnov/conduit)")
    if [[ "$status" == "200" ]]; then
        echo "  already published: $name @ $workspace_version — skipping"
        exclude_args+=("--exclude" "$name")
        already_published=$((already_published + 1))
    else
        to_publish=$((to_publish + 1))
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
