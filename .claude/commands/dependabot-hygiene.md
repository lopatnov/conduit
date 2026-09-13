---
description: Cheap Dependabot PR triage + orphan-branch sweep for lopatnov/conduit, run whenever the log shows it hasn't happened in ~24h. Extracted from rules/index.md so the procedure only loads when actually invoked, not on every turn of every session.
argument-hint: "(none — reads GitHub state and this repo's own hygiene log)"
---

# /dependabot-hygiene — Dependabot & branch hygiene reflex check

> Extracted 2026-08-28 from `.claude/rules/index.md` "Dependabot & branch hygiene reflex
> check" (see that file's "New `.claude/` process content: command/skill by default, not
> `rules/`" note for why) — content unchanged, just relocated so it isn't ambient in every
> session's context.
>
> Originally added 2026-08-01 after PR #101 (kube 3→4.0.0) sat "held for dedicated review"
> for ~5 weeks and PR #151 (an all-actions Dependabot bump) went untriaged for a while —
> both because the only routine check was the daily `/feature-workspace-cycle` firing, and
> no other session touching this repo's GitHub state repeated the check in between.

Any session that calls a GitHub tool against this repo for *any* reason — not just a
`/feature-workspace-cycle` firing — should, as a cheap side effect, check whether this
sweep has run in the last ~24h (see `.claude/logs/dependabot-hygiene.md`). If the newest
row is older than that (or the log is still empty):

- List open Dependabot PRs and triage/merge/hold each by the usual bar (green, clean, no
  unaddressed finding) — same as `/feature-workspace-cycle` Step 1.
- List all branches and cross-reference against PRs in every state. A branch with **no
  PR at all** is a genuine orphan worth a one-line flag to the user (could be real
  unfinished work, not touched further without asking). A branch whose PR is merged or
  closed is just leftover clutter — note it, don't spend effort chasing deletion: `git
  push --delete` is blocked from inside a Claude Code session by this environment's git
  proxy (a 403 on every attempt so far), so cleanup has to happen from the user's own
  machine or by enabling the setting below.
- **Recommend the user enable "Automatically delete head branches"** (repo Settings →
  General → Pull Requests section) if it isn't already — this is the permanent fix for
  branch clutter after merges and doesn't depend on any session remembering to check.
  There's no API tool available here to flip it directly; it has to be the user's own
  action.
- Log the result — even "nothing new" — as a new row in `.claude/logs/dependabot-hygiene.md`,
  so the next session (or the next daily firing) can see it was already checked and skip.

This is a *cheap* reflex check (a couple of list calls), not a deep audit — skip it
outright if the log shows it ran within the last ~24h. A `/feature-workspace-cycle`
firing that completes its own Step 1 satisfies this for the day; it doesn't need to run
the check twice.
