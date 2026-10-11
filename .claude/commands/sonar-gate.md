---
description: Find the uncovered lines behind a failing SonarCloud quality gate on a PR
---

# SonarCloud gate investigation

The gate that usually fails is "Coverage on New Code" (80%). SonarCloud counts the lines a PR added that the `cargo llvm-cov --lib` run of `.github/workflows/sonar.yml` did not execute, except files listed in `sonar.coverage.exclusions` (`sonar-project.properties`). Only `--lib` unit tests count: a line that only `tests/*.rs` reach is uncovered for the gate.

## Where sonarcloud.io is reachable

The project is public, so a local session can list the uncovered new lines without the web UI (replace `<n>` with the PR number):

```bash
curl 'https://sonarcloud.io/api/measures/component_tree?component=lopatnov_conduit&pullRequest=<n>&metricKeys=new_uncovered_lines,new_coverage&strategy=leaves&ps=100'
curl 'https://sonarcloud.io/api/sources/lines?key=<fileKey>&pullRequest=<n>'   # per-line coverage of one file
```

## A cloud container (the proxy blocks the host)

`CONNECT` 403, `ENOTFOUND`; checked 2026-10-11. Use the `mcp__sonarqube__*` connector if it is present. Otherwise build the report yourself: CI computes the coverage with one command, so the same command run locally gives the same lines.

```bash
rm -rf target   # an older build can fill the disk allowance; the instrumented build alone needs about 16 GB
CARGO_INCREMENTAL=0 cargo llvm-cov --lib --workspace --features full --lcov \
  --output-path target/coverage.lcov --ignore-filename-regex '(tests/|build\.rs)'
git fetch origin main
python3 scripts/new_uncovered_lines.py target/coverage.lcov origin/main
```

The script prints, per file, the added lines the report says never ran, then `new code: covered/executable lines = N%`. It takes the added lines from `git diff -U0 origin/main...HEAD` and skips files that are not in the report (not compiled in this run, for example code for another OS) and files `sonar.coverage.exclusions` matches. On #592 it listed 39 uncovered lines in four files where SonarCloud listed 38 in three; the extra line was in `redirect.rs`, which `sonar.coverage.exclusions` skips and the script now skips too.

- **Disk and time** (measured on #592, 4 vCPUs): `target/` ends at about 16 GB with `CARGO_INCREMENTAL=0`; with incremental compilation on it reached 26 GB and exceeded the container's writable-disk allowance. Plan for more than 15 minutes: run it in the background and poll.
- **Keep the CI flags.** Dropping `--features full` silently changes which code is measured. The parent directory of `--output-path` must already exist, or `cargo llvm-cov` fails only after the whole run (checked 2026-10-11); `target/` does, and also keeps the file out of `git status` (a `coverage.lcov` in the repository root is not ignored).
- **Tools:** `cargo llvm-cov --version` must work (`cargo install cargo-llvm-cov --locked` and `rustup component add llvm-tools-preview`; CI installs both).
- `-p <crate>` limits the run to one crate's unit tests, so a line only another crate's tests reach shows as uncovered; confirm with the full run before concluding.
- The gate on the PR stays the arbiter; the script is a guide to which lines to cover.

If neither route is available, ask the owner for the list. Do not merge with a red gate without the owner's explicit yes.

If the coverage figure does not move after new tests, the uncovered lines are elsewhere (for example Unix-only or server wiring code that unit tests do not reach); look at the list before adding more tests.
