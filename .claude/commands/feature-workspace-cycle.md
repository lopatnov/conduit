---
description: One autonomous iteration of the Conduit 2.0 workspace-migration cycle (#114) — Dependabot triage, a periodic integrity audit of already-shipped code/tests/docs, pick up or decompose a sub-issue, implement it on a branch, self-review, get it green, update docs, merge, log a summary, and check whether #114's goal is fully met.
argument-hint: "(none — reads state from GitHub: open PRs, #114's open sub-issues, and this session's own history)"
---

# /feature-workspace-cycle — one iteration

This command is fired **once per hour by a Routine bound to this same session**
(self-bind mode), so it continues the actual conversation rather than starting
cold each time. Before doing anything else: **look at what you were already
doing.** If the previous turn left a branch half-finished, a PR open awaiting
CI, or a task mid-flight, continue that — don't start something new just
because a new hour ticked over. "Continue the unfinished thing" beats "pick a
fresh task" every time this fires.

Target: fully realize #114 (feature-driven Cargo workspace — one crate per
feature, zero unrelated code/deps compiled in for any feature combination).
Work in **small, mergeable steps**: one sub-issue (or one clear slice of one)
per iteration, not the whole epic at once. Everything here should fit in
roughly a 5-hour wall-clock budget per firing — if a step is clearly going to
blow through that, stop cleanly at a safe checkpoint (branch pushed, PR
opened/updated, state recorded) rather than pushing on past it.

Model assignment (already encoded in each agent's frontmatter — don't override):
**haiku** for mechanical/high-volume work (`dependency-steward`,
`feature-matrix-runner`, `footprint-auditor`, `build-validator`,
`benchmark-runner`); **opus** for architecture/large-file decomposition
(`architect`); **sonnet** for everything else, including `integrity-auditor`,
`prior-art-researcher`, and this conductor loop itself.

## Step 0 — orient (conductor, no agent)
- Check this session's own recent history first: is there an open PR from a
  prior firing still awaiting CI/review? A branch with uncommitted or
  unpushed work? Resume that before looking for new work.
- Skim the most recent "Реализовано в сессии" entries in `CLAUDE.md` and the
  latest summary comments on #114's sub-issues (step 9 below) — a previous
  iteration's summary may directly tell you what to do next.

## Step 1 — PR triage (Dependabot + the user's own PRs)
- Call **`dependency-steward`** to list open Dependabot PRs, classify semver
  risk, group related bumps, and check their CI.
- Also list the user's own open, non-draft PRs against `main` (`lopatnov`-
  authored, not this migration's own PRs against the 2.0 branch — those are
  Step 7's job). For each: check CI (`get_check_runs`), check for unresolved
  review threads/findings from CodeRabbit/Qodo/Gitar/SonarCloud/Socket/
  Semgrep/CodeQL, and check `mergeable_state`.
- Act on both directly and by the same bar: merge
  (`mcp__github__merge_pull_request`, squash, matching this repo's commit-
  title convention) whatever is green, clean, and has no unaddressed real
  finding. Leave a comment explaining why not for anything held back — never
  leave a ready-looking PR sitting unmerged with no recorded reason, and never
  merge over a finding you haven't actually judged.
- If a PR's CI is red, fix the break when it's small and in scope, or say why
  not. The build must stay green after every merge — re-run `/build` if a
  merge could plausibly interact with in-flight work on the 2.0 branch.
- If merges to `main` accumulate to something worth shipping (several fixes,
  a security fix like #111, etc.), it's fine to flag that a release looks due
  — but only **cut** one (`release-engineer`, `.claude/skills/release/
  SKILL.md`) when the user has confirmed the target version, per that skill's
  own rule. If a release *is* cut during this step, don't stop at pushing the
  tag: watch `release.yml` (`mcp__github__actions_list`/`actions_get`/
  `get_job_logs` — this environment has no `gh` CLI) through to completion
  and verify the actual artifacts (GitHub Release binaries, both Docker
  manifest variants, npm package version) per the skill's Step 3/4 — a tag
  push that triggers a workflow which then fails partway is not a shipped
  release.

## Step 1c — integrity & completeness audit (periodic, not every firing)
Conduit has a large body of already-shipped code, features, and docs that
mostly hasn't been re-verified since it landed — the migration work in Steps
2-8 only reviews *new* diffs, which leaves a blind spot. This step closes it.
Not free (it's a real reasoning pass), so use judgment on cadence: run it
roughly **once every 4-6 firings**, or whenever Step 0 found nothing
unfinished and Step 1 found nothing to triage — don't skip it indefinitely,
but don't burn every firing on it either.
- Pick one feature/module/config area not audited recently (check `CLAUDE.md`
  for the oldest "Реализовано" entry, or a module whose test file hasn't
  changed in a long `git log`) and call **`integrity-auditor`** on it. The
  goal stated plainly: **at minimum, confirm nothing is silently broken or
  lost; ideally, leave it better than found.**
- For each gap it reports:
  - `docs-drift` or a small, obvious, low-risk fix (a missing doc line, an
    absent test for an existing code path with no behavior ambiguity) → fix
    it directly, own branch off `main`, through the same self-review → green
    → merge mechanics as Steps 4-7 (this is *not* #114 work, so the branch
    targets `main`, not the migration branch).
  - A real behavioral bug, or anything needing design judgment → file a
    GitHub issue with the specifics (`scrum-master`) rather than
    stealth-fixing it inline; it becomes normal backlog, picked up like any
    other issue (by a future firing of this cycle, or by the user directly).
  - If the auditor flags that a gap looks like "missing a known-good
    pattern" rather than just a missing test/doc line, loop in
    **`prior-art-researcher`** before deciding the fix — see the note below.
- Log what you audited and found (even "no gaps") so a future firing doesn't
  re-audit the same area needlessly — a short note in the relevant `CLAUDE.md`
  section or a comment is enough.

## Step 2 — pick the next task (skip if Step 0 found unfinished work)
- Look at #114's open sub-issues (`mcp__github__issue_read` /
  `list_issues` filtered to sub-issues of #114). Pick the next one in
  phase order (Phase 0 → 6) unless a dependency isn't merged yet.
- If the task genuinely needs "how do others solve this" input before you can
  implement it, call **`prior-art-researcher`** first and fold its
  recommendation into the approach. This isn't only for brand-new work: if
  you (or `integrity-auditor` in Step 1c) notice an existing piece of code
  solving a problem clumsily where a well-established pattern from the
  reference projects (`tower`'s typed middleware stack, linkerd2-proxy's
  `ReplayBody`, h2o/Angie/Envoy/HAProxy's algorithms — see `CLAUDE.md`'s own
  research backlog for the full list) would clearly do it better, treat that
  as worth raising too — via `prior-art-researcher` for the "what do others
  do" brief, then `architect` if it's a real structural change. Don't
  gold-plate: only worth it when the improvement is clear and low-risk, not
  as a pretext to rewrite something that already works.
- If the sub-issue is still too big to finish in one iteration (rare — most
  were already sized small during the original decomposition), decompose it
  further yourself or via **`scrum-master`**, file the pieces as GitHub
  issues linked as sub-issues of the parent, and pick the first piece.

## Step 3 — implement
- Create a branch off the current tip of the 2.0 migration branch
  (`claude/cargo-workspace-features-23qxfr`), never off `main` directly for
  #114 work.
- For a mechanical "extract conduit-X" sub-issue, delegate to
  **`crate-extractor`** with the sub-issue's spec. For a seam refactor or
  anything needing judgment, implement it yourself.
- Every PR into the 2.0 branch bumps the workspace minor version
  (2.1.0, 2.2.0, ...) per the user's standing decision — bump
  `[workspace.package] version` as part of the same commit.
- Open the PR against `claude/cargo-workspace-features-23qxfr` (draft is
  fine), and `subscribe_pr_activity` on it.

## Step 4 — self-review and fix
- Review your own diff as if it were someone else's: correctness, scope
  creep, missed edge cases, anything that contradicts `CLAUDE.md`'s recorded
  architectural decisions.
- Fix what you find. If another bot (CodeRabbit/Qodo/Gitar/SonarCloud/Socket/
  Semgrep/CodeQL) has already commented by the time you loop back to this PR,
  address genuinely valid findings — including ones outside the sub-issue's
  narrow scope, if they're real bugs — but don't "fix" a stylistic opinion or
  a false positive; say why you're not acting on it instead of silently
  ignoring or blindly complying.

## Step 5 — get it green
- Call **`build-validator`** (`/build`, with `full` if the change touches
  feature-gated code) and **`feature-matrix-runner`** (mandatory for any PR
  that touches `[features]` or moves code across a crate boundary — which is
  every extraction PR in this migration).
- Call **`footprint-auditor`** on extraction PRs specifically, to confirm the
  split actually shrank the target profile's dependency count/binary size —
  that's the number #114 exists to move.
- Do not proceed to Step 6 until both are GREEN.

## Step 6 — docs and CI/CD
- Call **`docs-scribe`** if the diff changed config schema, CLI surface,
  Cargo features, or moved a module referenced by path in the docs.
- Update `.github/workflows/*.yml` yourself if the crate split changes what
  needs building/testing (new workspace member, new feature combination worth
  covering in `ci-features`).

## Step 7 — merge
- Once green and reviewed, merge the PR into
  `claude/cargo-workspace-features-23qxfr` (call **`release-engineer`** first
  if there's any merge-order ambiguity with other open PRs on that branch).

## Step 8 — log the summary
- Comment on the sub-issue (and #114 if it's a phase-completing step) with a
  short summary: what changed, the footprint delta if measured, what's next.
  This is what Step 0 of the *next* firing reads to pick up context.

## Step 9 — check the finish line
- After logging the summary, check: does #114 have any open sub-issues left?
  Does any part of the codebase still compile in code unrelated to an
  active feature for any tested combination?
- If #114 is fully, elegantly, and concisely done — every planned crate
  extracted, `proxy` itself optional, `cargo-workspaces` publishing wired up,
  docs/CI in sync — say so explicitly in a closing comment on #114, close it,
  and note in your final output that the routine driving this command should
  be disabled (don't disable it yourself — that's the user's Routine to stop,
  flag it clearly).
- Otherwise: end the turn normally. The Routine fires again next hour and
  Step 0 will pick up from here.

## Escalation (stop and ask, don't guess)
- A merge conflict or design question genuinely needs the user's judgment
  (not just "which crate boundary" — `architect` handles that) → surface it
  plainly in your final message rather than picking silently and moving on.
- Usage limits or a skipped firing are expected and fine — just resume at
  Step 0 next time.
