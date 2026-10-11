# Conventions — engineering conventions for conduit

Observed-from-reality, not a generic template. Compacted in the 2026-10-03 diet (#512).

## Commits — Conventional Commits
`<type>(<scope>): <subject>` — types `feat fix docs style refactor perf test build ci chore`; imperative, no trailing period,
short, English only. The body says *why*. End with the `Co-Authored-By:` trailer from the **current** system-reminder's
attribution instructions (never a model name copied from an old commit or from this file — it changes mid-project); pass the
message by heredoc; never amend unless asked.

## Versioning — SemVer, and where it lives
A bump touches, together: `[workspace.package].version` in the root `Cargo.toml` (every member inherits it) **and every
`lopatnov-conduit-*` `version = "…"` string in `[workspace.dependencies]`** (~32 literals; `cargo publish --workspace` resolves
inter-crate deps through them and they do not follow the workspace version — `./scripts/check-workspace-versions.sh`, CI job
`workspace-publish-dryrun`, fails on drift; run it before opening the PR), `Cargo.lock`
(`cargo update -p lopatnov-conduit --offline`), `npm/package.json`, and the version strings in `docs/cli.md` and
`docs/deployment.md`. No per-crate versioning yet (#258). `release-engineer` drives it; confirm the target
version with the owner, never guess patch/minor/major.

## Branches
`main` is always green and release-ready: never push to it, branch + PR. Names: `feat/ fix/ chore/ ci/ docs/<short>`. One branch =
one coherent change; unrelated fixes get their own branch.

## Push and CI economy
Push a ready branch at once, but don't push WIP repeatedly — every push is a ~25-minute CI run (owner's rule, `rules/index.md`).
Before adding a commit to fix a red check, find out whether the same failure is transient (`release-engineer` "Transient vs real").

## Code quality
- Match the surroundings (style, naming, comment density); **zero warnings** (`-D warnings`) on default and `--features full`;
  English only for code, comments, commits, CLI output, errors, logs, `docs/` (`CLAUDE.md` and `.claude/**` are the owner's own
  notes and stay Russian).
- **File length: soft 400, hard 1000 lines of *production* code — tests are exempt.** Crossing 400 means split (phase-orchestrator
  pattern, PR #91/#92) and ask `architect` for the plan first; never reach 1000. Measure with `scripts/check_file_length.py`
  (code lines: no comments, blanks or tests — every `#[cfg(test)]`/test-only-`cfg` item, `#[test]` fn, and whole `tests/`,
  `tests.rs`, `*_tests.rs` files are excluded), not `wc -l`. CI job `code-length` posts the report on the PR; it is informational.
  A large test module is not by itself a reason to split (a `foo/tests.rs` split is fine as organisation).

## PR checklist (gate before merge)
- [ ] `security-engineer` sign-off recorded — **mandatory on every PR, unconditional** (`workflow.md`); one pass on the final
      head, a delta pass after any later commit; the verdict is posted as a PR comment.
- [ ] `/build` green: fmt, clippy `-D warnings`, tests (default and `full` if feature-gated).
- [ ] All CI jobs pass (`gh pr checks`; known-transient failures re-run with `gh run rerun --failed`, or
      `mcp__github__actions_run_trigger` in a cloud firing).
- [ ] **Every comment READ** — issue comments, inline comments, reviews, all authors incl. the owner's and the bots' on #152
      (`scripts/pr-comments.sh <pr>`) — each finding with a recorded disposition; green checks plus zero open threads is not the
      same thing. Reply/resolve mechanics: the `coderabbit-reply` skill ("Outside diff range" comments need a regular PR comment).
- [ ] The PR description has the **"Found while here"** table (every oddity noticed in touched code, with its disposition; a deferred one links its follow-up issue).
- [ ] Version strings consistent if the change is release-shaped; docs updated if behaviour, config or features changed
      (`docs/configuration.md`, `building.md`, `cli.md`, `deployment.md`, `schema/conduit.schema.json`).
- [ ] Journal summary in `CLAUDE.md` (with its "Здоровье" line) and the issue comment written; closed issues closed by hand with the
      measured result (a PR into the migration branch does not auto-close them).
- [ ] A new regression test for a hash/modulo/ring/rotation-index bug: its negative control was verified by re-reading the
      *patched source* (a `cargo fmt` reflow or an imprecise replace can make a scripted edit silently no-op), and the fixture makes
      buggy and correct behaviour genuinely disagree (`.claude/skills/testing/SKILL.md`; it bit this repo four times, #372/#373).
