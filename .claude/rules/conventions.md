# Conventions — engineering conventions for conduit

> Lean, observed-from-reality version (not the generic template). Codifies what this repo
> actually does, so it stays consistent across sessions instead of drifting per-session.

## Commits — Conventional Commits

Format: `<type>(<scope>): <subject>` — matches the actual history (`feat:`, `fix:`, `chore:`,
`ci:`, `docs:`).

- **type:** `feat`, `fix`, `docs`, `style`, `refactor`, `perf`, `test`, `build`, `ci`, `chore`.
- **subject** — imperative mood, no trailing period, short. English only (CLAUDE.md "Language").
- Body explains *why*, not *what* — the diff already shows what changed.
- Always end with `Co-Authored-By: Claude Sonnet 4.6 <noreply@anthropic.com>` (per the global
  commit instructions) — heredoc the message, never amend unless explicitly asked.

## Versioning — SemVer, and where it lives

`MAJOR.MINOR.PATCH`. conduit ships three artifacts that must stay in lockstep on a version
bump — **all four** of these need updating together (see PR #71 for the canonical example):
- `Cargo.toml` (`version = "..."`) + `Cargo.lock` (`cargo update -p lopatnov-conduit --offline`)
- `npm/package.json` (`"version": "..."`)
- `docs/benchmarks.md`, `docs/cli.md`, `docs/deployment.md` — version strings in prose/examples

`release-engineer` drives this; confirm the *target* version with the user first — don't guess
whether something is patch/minor/major.

## Branches

- `main` — always green, always release-ready. **Never push directly to it** — branch + PR.
- Working branches: `feat/<short>`, `fix/<short>`, `chore/<short>`, `ci/<short>`, `docs/<short>`.
- One branch = one coherent change. Don't let unrelated fixes piggyback on a branch already
  open as a PR — open a new branch+PR instead (keeps merge order clean, see `release-engineer`).

## Push frequency & CI economy

- `git push` no more than once per hour by default — avoids spamming CI / creating races
  between PRs. More often only when the user explicitly asks (see `.claude/rules/index.md`).
- Before pushing a fix to an open PR, check whether the *same* failure is transient
  (network blip — see `release-engineer` "Transient vs real") before adding a new commit.

## Code quality

- Code matches its surroundings: same style, naming, comment density (`rustfmt` + `clippy`
  enforce most of this — see `.github/workflows/ci.yml` job `ci`).
- **Zero warnings** is the bar — both default and `--features full` builds (`-D warnings`).
- English only — code, comments, commit messages, CLI output, errors, logs, docs
  (CLAUDE.md "Language & Localization" — this overrides any default behavior).
- **File length**: soft limit 400 lines, hard limit 1000 lines per source file. Crossing
  400 is a signal to split into modules/helpers (see the `logging_phase.rs` /
  `request_phase.rs` phase-orchestrator pattern from PR #91/#92); a file must never reach
  1000 lines — split it before that point, not after. When a file crosses the limit, call
  the **`architect`** subagent (opus) for a concrete split plan before implementing it.

## PR checklist (gate before merge)

- [ ] `/build` green — fmt, clippy (`-D warnings`), tests (default + `full` if feature-gated).
- [ ] `gh pr checks <N>` — all CI jobs pass (or known-transient failures re-run and verified).
- [ ] CodeRabbit / reviewer threads addressed: reply with what changed (or why not), then
      resolve via `gh api graphql resolveReviewThread` — see PR #70 for the pattern (don't
      leave threads dangling; "Outside diff range" comments need a regular PR comment instead
      of an inline reply, since GitHub can't post inline on those).
- [ ] Version-string consistency checked if this is a release-shaped change (see "Versioning").
- [ ] Docs updated if behavior/config/features changed (`docs/configuration.md`, `building.md`,
      `cli.md`, `deployment.md` as relevant — and `schema/conduit.schema.json` if schema changed).
- [ ] `CLAUDE.md` backlog checkbox + session log updated if this closes a tracked item
      (see `scrum-master`).

## CodeRabbit reply pattern (recurring task — see PR #70)

```bash
# Reply to an inline comment
gh api repos/lopatnov/conduit/pulls/<PR>/comments/<comment_id>/replies -f body="..."

# Find the review-thread node id for that comment, then resolve it
gh api graphql -f query='query { repository(owner:"lopatnov", name:"conduit") {
  pullRequest(number: <PR>) { reviewThreads(first: 50) { nodes { id isResolved
  comments(first:1){nodes{databaseId}} } } } } }'
gh api graphql -f query='mutation { resolveReviewThread(input:{threadId:"<node_id>"})
  { thread { isResolved } } }'
```
For "Outside diff range" comments (can't be replied to inline — platform limitation),
post a regular `gh pr comment` addressing the points instead.

> One `git push` ≤ once/hour without explicit user request (see `.claude/rules/index.md` — economy & CI races).
