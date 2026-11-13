#!/usr/bin/env bash
# Runs the Docker end-to-end tests: every scenario of this directory, or the named ones.
#
#   scripts/e2e/run.sh BINARY [SCENARIO ...]
#   CONDUIT_IMAGE=debian:13 scripts/e2e/run.sh BINARY h2-host
#
# BINARY is a conduit built with the features the scenarios need (`--features standard` is enough for the ones here).
# A scenario is a file `NAME.sh` in this directory that takes BINARY as its only argument; lib.sh describes the network
# it runs in. A failing scenario does not stop the others, and the exit status is non-zero when any failed.
set -uo pipefail
here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)

if [ "$#" -lt 1 ]; then
  echo "usage: $0 BINARY [SCENARIO ...]" >&2
  exit 2
fi
bin=$1
shift
scenarios=("$@")
if [ "${#scenarios[@]}" -eq 0 ]; then
  for f in "$here"/*.sh; do
    name=$(basename "$f" .sh)
    case "$name" in lib | run) continue ;; esac
    scenarios+=("$name")
  done
fi

failed=()
for name in "${scenarios[@]}"; do
  echo "=== $name"
  # a scenario is a file of this directory: a name with a path in it is not one
  if [[ ! "$name" =~ ^[A-Za-z0-9_-]+$ ]] || [ ! -f "$here/$name.sh" ]; then
    echo "no scenario named $name" >&2
    failed+=("$name")
    continue
  fi
  bash "$here/$name.sh" "$bin" || failed+=("$name")
done

if [ "${#failed[@]}" -ne 0 ]; then
  echo "failed: ${failed[*]}" >&2
  exit 1
fi
echo "all scenarios passed: ${scenarios[*]}"
