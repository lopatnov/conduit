---
description: Check for open `fast-follow` labeled issues and prioritize them ahead of the general backlog when picking the next batch of work.
argument-hint: "(none — reads GitHub issue state for lopatnov/conduit)"
---

# /fast-follow-check — pick up deferred fast-follows before the general backlog

> Added 2026-09-05 at the user's request, after issue #357 (a deliberate design-judgment
> follow-up spawned while reviewing PR #356) was filed with nothing but its own issue body
> linking it back to #356 — no mechanism made sure it would actually get picked up soon
> instead of drifting into the general backlog alongside everything else. The user's
> framing: when a task/PR spawns a follow-up issue, don't just leave a comment and hope a
> future session remembers it — make sure the *next* round of "what should we work on"
> checks for it first.

## What counts as a fast-follow

An issue filed **during work on some other task/PR**, describing something found or
deferred along the way that couldn't be folded into the PR that spawned it — usually
because it needs its own design decision (see the batch-sizing "1, always" tier in
`feature-workspace-cycle.md`), not because it's unrelated work. Issue #357 (deferred from
#356's review) is the canonical example.

**Not** a fast-follow: a general backlog item, a Dependabot PR, a routine research/RFC
issue, or anything filed independent of active PR work. Don't label those — the label
loses its signal value if it's applied broadly.

## When filing one

Tag it with the **`fast-follow`** label at creation time
(`gh issue create ... && gh issue edit <n> --add-label fast-follow`, or
`gh label create fast-follow --description "Deferred follow-up from a just-merged PR — check
before pulling from the general backlog" --color fbca04` first if the label doesn't exist
yet in this repo). The issue body should already link back to the PR/issue that spawned it
(this repo's existing convention — see #357's body for the pattern) — the label is what
makes it *discoverable* at the next batch-selection round, the body is what makes it
*understandable* once found.

## When picking the next batch of work

Before pulling from the general backlog (GitHub issue list, `CLAUDE.md` backlog
checkboxes, whatever's next in the agreed ordering), check:

```bash
gh issue list --repo lopatnov/conduit --label fast-follow --state open
```

If anything is open there, surface it to the user as a candidate for the *next* batch —
don't silently skip past it in favor of older backlog items just because it's small.
This doesn't mean force it into the current PR/branch (that's exactly the premature
bundling the label exists to avoid — a fast-follow was deferred because it needs its own
scope/design call, not because it's urgent). It means: don't let it silently age past its
natural pickup point either.

Once addressed (fixed, or explicitly re-deferred with a reason), the label naturally drops
out of the open-issue list when the issue closes — no separate log file to maintain here,
unlike `/dependabot-hygiene`'s append-only log (GitHub's own issue state already is the
log for this one).
