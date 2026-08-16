#!/usr/bin/env bash
# Fails if any Layer-1 feature crate under crates/ references SiteConfig or
# AppConfig directly. Those types are owned by Layer 2 (conduit-config) and
# consumed at the top by conduit-runtime/conduit-admin-core/conduit-server/
# conduit-cli — a Layer-1 feature crate reaching for them directly recreates
# the config<->feature circular dependency the workspace split exists to
# avoid. See issue #114 (Phase 1.6, #125): catching this now is cheap;
# discovering it mid-Phase-5 (after ~15 feature crates already exist) is not.
#
# No-ops cleanly today (crates/ has no member crates yet) — this establishes
# the guardrail ahead of Phase 2+ extractions, per #125's own scope.
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

# Layer 0/2/3 crates: allowed to reference SiteConfig/AppConfig (they own it,
# or need the full config at the top of the stack).
ALLOWED_CRATES=(
  conduit-core
  conduit-config-core
  conduit-config
  conduit-runtime
  conduit-admin-core
  conduit-server
  conduit-cli
)

is_allowed() {
  local name="$1" allowed
  for allowed in "${ALLOWED_CRATES[@]}"; do
    [[ "$name" == "$allowed" ]] && return 0
  done
  return 1
}

violations=0
checked=0

shopt -s nullglob
for manifest in crates/*/Cargo.toml; do
  crate_dir="$(dirname "$manifest")"
  crate_name="$(grep -m1 -E '^name\s*=' "$manifest" | sed -E 's/^name\s*=\s*"([^"]+)".*/\1/')"

  is_allowed "$crate_name" && continue
  [[ -d "$crate_dir/src" ]] || continue

  checked=$((checked + 1))
  hits="$(grep -rn -E '\b(SiteConfig|AppConfig)\b' "$crate_dir/src" || true)"
  if [[ -n "$hits" ]]; then
    echo "::error::$crate_name (Layer-1 feature crate) references SiteConfig/AppConfig directly:"
    echo "$hits"
    violations=$((violations + 1))
  fi
done

if [[ "$violations" -gt 0 ]]; then
  echo ""
  echo "Layer-1 feature crates must not depend on SiteConfig/AppConfig directly" >&2
  echo "(issue #114/#125). Give the crate its own narrow config struct owned by" >&2
  echo "conduit-config (Layer 2) instead of reaching into the full site config." >&2
  exit 1
fi

echo "No Layer-1 SiteConfig/AppConfig references found (checked $checked crate(s))."
