---
name: release
description: Step-by-step playbook for cutting a conduit release (v<x.y.z> tag → release.yml pipeline → verify artifacts). Use when the user wants to ship a version, or release-engineer needs a concrete checklist to drive from.
---

# Release — cutting a conduit version

> Formalizes the runbook already given to the user for v1.1.1. `release-engineer` drives this;
> the conductor can also walk through it directly for a quick release. Always confirm the
> *target* version with the user first — don't infer patch/minor/major from the diff alone.

## Pre-flight — is it actually ready?

1. **Check open PRs that should land first.** `gh pr list` — confirm merge order for any
   PRs this release depends on (e.g. v1.1.1 depended on #70 → #71 → #72 landing in that order:
   CI fixes → version bump → docs).
2. **Confirm every required PR is green.** `gh pr checks <N>` for each — all required checks
   pass, CodeRabbit/reviewer threads resolved (see `conventions.md` PR checklist).
3. **`/build full`** on `main` after merges land — fmt, clippy `-D warnings`, tests
   (default + `--features full`) all green.

## Step 1 — version consistency (4-artifact lockstep)

All of these must show the **same** target version (see `conventions.md` "Versioning",
canonical example PR #71):

- `Cargo.toml` → `version = "x.y.z"`
- `Cargo.lock` → matching entry (regenerate with `cargo update -p lopatnov-conduit --offline`
  if it drifted)
- `npm/package.json` → `"version": "x.y.z"`
- `docs/benchmarks.md`, `docs/cli.md`, `docs/deployment.md` → version strings in prose/examples
- **Since the workspace migration (#114/#148): every `lopatnov-conduit-*` entry's own
  `version = "..."` string in `[workspace.dependencies]` (~32 lines) must match
  `[workspace.package].version` too — run `./scripts/check-workspace-versions.sh` (also
  the CI job `workspace-publish-dryrun`) rather than grepping these by hand.

```bash
# quick grep across the lockstep set
grep -n "version" Cargo.toml npm/package.json | head -5
grep -rn "1\.1\.[0-9]" docs/*.md   # adjust pattern to the current/target version
./scripts/check-workspace-versions.sh
```

If anything is out of step, fix it on its own small PR (or as part of the version-bump PR,
as #71 did) — **before** tagging, not after.

## Step 2 — tag and push

```bash
git checkout main && git pull
git tag v<x.y.z>
git push origin v<x.y.z>
```

This triggers `.github/workflows/release.yml`. **Never force-push a tag** that's already
triggered a release — if the tag is wrong, talk to the user before doing anything destructive.

## Step 3 — monitor the pipeline

```bash
gh run list --workflow=release.yml --limit 1
gh run watch <run-id>          # or: gh run view <run-id> --log-failed   if something fails
```

Watch specifically for the Docker jobs — `docker-scan` / `docker-full-scan` (Trivy) and the
manifest-assembly steps are the most failure-prone part of this pipeline (see the actual
bugs PR #70 fixed: `MANIFEST_UNKNOWN` double `-full-full` tag, wrong `trivy-version` input
name, missing `v` prefix on the Trivy version pin — all three real regressions caught here).

## Step 4 — verify artifacts

- **GitHub Release**: `gh release view v<x.y.z>` — release notes present, binaries attached.
- **Docker manifests**: pull and check both standard and `-full` variants exist for the
  expected platforms:
  ```bash
  docker buildx imagetools inspect ghcr.io/lopatnov/conduit:<x.y.z>
  docker buildx imagetools inspect ghcr.io/lopatnov/conduit:<x.y.z>-full
  ```
- **npm package** (if published as part of this release): `npm view @lopatnov/conduit version`
  matches the tag.
- **crates.io: since the workspace migration (#114/#148), one release publishes ~33
  packages, not just `lopatnov-conduit`.** The `publish-crates` job
  (`scripts/publish-workspace.sh`) can take hours the first time due to crates.io's
  new-crate rate limit — it's designed to be safely re-run (it skips anything already at
  the target version), so a run that times out or gets interrupted is not itself a
  failure; re-trigger the job rather than re-tagging. Spot-check a few member crates
  landed, not just the root: `curl -s https://crates.io/api/v1/crates/lopatnov-conduit-core
  | jq .crate.newest_version`.

## Step 5 — close the loop

- `scrum-master`: append a "Реализовано в сессии <date>" entry to `CLAUDE.md`'s "Журнал сессий"
  section (rotating the oldest to `.claude/logs/session-log.md`, per `scrum-master.md`) if the release
  itself is worth logging (usually the *features* in it are logged when they land, not the
  release act — but note the version bump).
- Close/comment any GitHub issues this release resolves.
- If any step above surfaced a CI/pipeline bug (like #70's three), that's its own `fix:` PR —
  don't let it block the current release once a workaround/rerun gets it through, but don't
  forget it either (`scrum-master` parks it).

## Transient vs. real failure during release

Same heuristic as routine CI triage (see `release-engineer`, `.claude/rules/index.md`): a `curl
failed: SSL_read: unexpected eof` or `download of <crate> failed` on crates.io/ghcr.io is
usually a network blip — `gh run rerun <id> --failed`. Only escalate as a real regression if
the *same commit/tag* fails the *same way* across multiple reruns.

## Push frequency note

Tagging and pushing a release tag is **not** subject to the "≤1 push/hour" guidance — that
rule is about avoiding noisy commit-churn on branches/PRs mid-development. A release tag is a
deliberate, infrequent, user-confirmed act.
