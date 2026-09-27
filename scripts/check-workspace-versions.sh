#!/usr/bin/env bash
# Fails if any lopatnov-conduit-* entry in [workspace.dependencies] carries a
# `version` that doesn't match [workspace.package].version. A real multi-crate
# `cargo publish --workspace` (issue #114/#148) resolves each inter-crate
# dependency through that literal version string, not through the path — a
# forgotten bump on any one of the ~32 lines silently pins that crate's
# dependents to a stale published version (or, if the stale version was never
# published, breaks the publish outright). Cargo itself won't catch this:
# `path = "..."` still resolves locally regardless of what `version` says.
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

workspace_version=$(grep -m1 -E '^version *= *"[^"]+"' Cargo.toml | sed -E 's/^version *= *"([^"]+)"/\1/')
if [[ -z "$workspace_version" ]]; then
    echo "Could not read [workspace.package].version from Cargo.toml" >&2
    exit 1
fi

mismatches=0
while IFS= read -r line; do
    name=$(echo "$line" | sed -E 's/^(lopatnov-conduit-[a-z0-9-]+) *=.*/\1/')
    line_version=$(echo "$line" | grep -oE 'version *= *"[^"]+"' | sed -E 's/version *= *"([^"]+)"/\1/')
    if [[ "$line_version" != "$workspace_version" ]]; then
        echo "MISMATCH: $name declares version = \"$line_version\", workspace is \"$workspace_version\""
        mismatches=$((mismatches + 1))
    fi
done < <(grep -E '^lopatnov-conduit-[a-z0-9-]+ *= *\{ *path' Cargo.toml)

member_count=$(grep -cE '^lopatnov-conduit-[a-z0-9-]+ *= *\{ *path' Cargo.toml)
crate_dir_count=$(find crates -mindepth 1 -maxdepth 1 -type d | wc -l | tr -d ' ')
if [[ "$member_count" -ne "$crate_dir_count" ]]; then
    echo "MISMATCH: [workspace.dependencies] lists $member_count lopatnov-conduit-* path entries, but crates/ has $crate_dir_count directories — a crate was added/removed without updating the other"
    mismatches=$((mismatches + 1))
fi

if [[ "$mismatches" -gt 0 ]]; then
    echo ""
    echo "$mismatches problem(s) found. Fix Cargo.toml's [workspace.dependencies] block before publishing." >&2
    exit 1
fi

echo "OK: $member_count member crates all pinned at workspace version $workspace_version"
