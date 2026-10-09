---
description: Implement one or more GitHub issues end-to-end on one branch and ship one PR — orient, pick/confirm scope, implement, self-review, get it green, update docs, pass the mandatory security gate, merge, close the issues, log a summary.
argument-hint: "<issue-number> [issue-number ...]  (omit to pick up unfinished work, or the next fast-follow/backlog item)"
---

# /issue — implement one or more issues as one PR

> Added 2026-10-03, replacing `/feature-workspace-cycle` as the everyday entry point
> (owner's request: "переименовать как-то типа /issue и можно назвать одно или несколько
> issue для одного PR" — rename it to something like `/issue`, able to name one or several
> issues for one PR). `feature-workspace-cycle.md` stays on disk as a battle-tested
> *template* for the shape of work it was built for — a large, multi-PR, phase-ordered
> migration with its own frozen integration branch — but for ordinary issue work, this is
> the command to run. The general habits either command leans on (early audits, secure
> coding, batch sizing, branch grouping, writing a lesson down immediately) live in
> `.claude/rules/best-practices.md`, not duplicated here.

Takes one or more issue numbers as arguments (`/issue 531`, `/issue 480 481` for two
issues that genuinely belong in one PR). With no arguments, look for unfinished work first
(Step 0), then fall back to `/fast-follow-check` and the Dependabot/branch-hygiene reflex
check (`.claude/rules/index.md`) before asking the user what to pick up.

## Step 0 — orient

- Check for work already in flight before picking anything new: an open PR from a prior
  `/issue` run still awaiting CI/review, or a branch with uncommitted/unpushed changes.
  Resume that first — "continue the unfinished thing" beats "start something new."
- If resuming a handed-over task (a `spawn_task` chip, a `/handoff` summary, or picking
  back up after a usage-limit reset), run **`verify-handed-over-task`** first: is it
  already done, and is the worktree on the right base branch.
- Read every named issue in full, plus its comments — don't start from the title alone.

## Step 1 — scope and branch

- **Confirm the issue(s) actually belong in one PR**, per `best-practices.md` §3/§4: one
  issue is the default; naming several only makes sense when they share a root cause, the
  same code path, or are genuinely small independent leaves of one theme. If the named
  issues don't clearly meet that bar, say so and propose splitting rather than forcing a
  single branch.
- Check `CLAUDE.md`'s architectural decisions and open backlog for conflicts or
  duplication before writing code — call **`business-analyst`** if the ask is vague or its
  scope against existing decisions isn't obvious.
- **Search for other open bugs in the files/functions this issue will touch** (best-practices.md
  §1) and say up front which you'll fix in the same PR as a separate "behaviour change" commit.
- Branch off the current tip of the target branch (`main`, unless the issue says otherwise
  or an integration branch is explicitly in play) — never commit directly to it. Name per
  `conventions.md` (`feat/`, `fix/`, `chore/`, `ci/`, `docs/` + short slug).
- A milestone-sized issue (spans many files or several config forms) gets a design pass
  first — call **`architect`** for a concrete plan, post it on the issue, and spot-check its
  headline numbers against the actual code before executing (plans go stale fast).

## Step 2 — implement

- Do the work yourself for anything needing judgment; delegate only for genuine expertise,
  noisy-output isolation, or a bounded mechanical sub-task (`workflow.md`'s trigger table —
  `crate-extractor` for a mechanical crate move, `prior-art-researcher` for "how do others
  solve this" input, etc.).
- Keep the branch to one coherent change. If the issue turns out to need slices, they're
  separate verifiable **commits** on this one branch, not separate PRs (best-practices.md §4).

## Step 3 — self-review and fix

- Review your own diff as if it were someone else's: correctness, scope creep, missed edge
  cases, anything that contradicts a recorded architectural decision in `CLAUDE.md`.
- Address any bot findings (CodeRabbit/Gitar/SonarCloud/Socket/Semgrep/CodeQL) that land
  before you loop back — fix genuine issues, explain rather than silently ignore or
  blindly comply with a false positive or stylistic opinion.

## Step 4 — get it green

- Call **`build-validator`** (`/build`, with `full` if the change touches feature-gated
  code) and, if the change touches `[features]` or crosses a crate boundary,
  **`feature-matrix-runner`**.
- Spawn anything that mutates git state (`git checkout`/`pull`) or writes files with
  `isolation: "worktree"` if you'll keep editing concurrently — see `.claude/rules/index.md`
  on background-agent isolation.

## Step 5 — docs

- Call **`docs-scribe`** if the diff changed config schema, CLI surface, Cargo features, or
  moved a module referenced by path in the docs. Grep every changed feature/identifier
  name across the docs by content, not by an expected keyword.
- Update `CLAUDE.md`'s backlog / `.claude/` tooling if this closes a tracked item.

## Step 6 — merge

- **"Reviewed" means every comment has been READ** — issue comments, inline review
  comments, and reviews, from every author (CodeRabbit, Gitar, Sonar, Semgrep, Socket, the
  user) — not that checks are green or the unresolved-thread count is 0. Give every finding
  a recorded disposition: fixed, deferred with an issue (`fast-follow` label), or rejected
  with the reason.
- **`security-engineer` sign-off before every merge, unconditionally** — no PR is "too
  small" or "too obviously safe" to skip this (`workflow.md` "Security review is
  unconditional"). One pass on the final head; re-run only the delta if a commit lands
  afterward. Post the verdict as an actual PR comment before merging.
- Merge once green and reviewed. Call **`release-engineer`** first if there's merge-order
  ambiguity with other open PRs on the same target branch.

## Step 7 — close out

- Close each named issue by hand with a comment giving the measured result (not just
  "done") — `Closes #N` only auto-closes on merge into the repo's *default* branch.
- Log a short summary wherever this session's state lives (a PR/issue comment, or
  `CLAUDE.md`'s session log for anything future sessions will need) — what changed, what's
  next.

## Escalation (stop and ask, don't guess)

A merge conflict needing a real judgment call, a design fork the issue doesn't resolve, or
a security-sensitive ambiguity → surface it plainly rather than picking silently and
moving on. Usage limits or a skipped run are fine — just resume at Step 0 next time.
