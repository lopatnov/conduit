---
description: Read back over this session's conversation for process friction (repeated manual work, tool gaps, wrong assumptions, missing rules) and propose concrete edits to CLAUDE.md / .claude/ to fix it.
argument-hint: "(none — reviews this session's own history; optionally a hint like 'focus on the CodeQL findings' to narrow scope)"
---

# /retro — turn this session's friction into process fixes

This is a **retrospective on the conductor's own process**, not a code review of what was built.
The question it answers: *"what happened in this session that a better `CLAUDE.md`/`.claude/`
would have prevented or made faster?"*

## What to look for

Scan back over this session's conversation (and, if relevant, recent entries in `CLAUDE.md`'s
own session logs) for:

- **Repeated manual workarounds** — anything you had to figure out by trial and error that a
  documented rule/skill would have given you immediately (e.g. discovering `git push` doesn't
  authenticate in this environment and `mcp__github__push_files` is the workaround — that's
  exactly the kind of thing that should land in a rule so the *next* session doesn't rediscover
  it the expensive way).
- **Tool gaps** — a capability you needed and didn't have (no code-scanning-alert-dismiss tool,
  no `gh` CLI for subagents, etc.). Not all of these are fixable from inside a session, but the
  ones that recur are worth a documented note ("known gap: X, workaround: Y") so the next
  session doesn't waste time rediscovering the same dead end.
- **Wrong assumptions you had to correct** — anything you initially believed about the repo,
  its CI, its conventions, or its state that turned out to be false, and cost a turn or more to
  discover.
- **Rules that were unclear, contradictory, or silent on a real situation** — a point where you
  had to make a judgment call because `.claude/rules/*.md` or `CLAUDE.md` didn't actually cover
  the case that came up.
- **Things that worked well and are undocumented** — a technique, delegation pattern, or
  sequencing that worked smoothly but only lives in this transcript. Worth writing down so it's
  reusable, not just worth avoiding what went wrong.
- **Stale or inaccurate content already in `CLAUDE.md`/`.claude/`** — if something you read at
  the start of the session turned out to be wrong or outdated by the time you actually acted on
  it (a described mechanism that no longer matches the code, a backlog item marked done that
  isn't, an agent's tool grant listed as including something it doesn't).

If `$ARGUMENTS` gives a hint (e.g. "focus on the CodeQL findings" or "just the git-push issue"),
narrow the scan to that instead of doing a full-session sweep.

## What NOT to do

- Don't re-review the actual code changes made this session — that's `/code-review`'s job, not
  this command's.
- Don't propose process changes for one-off flukes (a single transient network error) — only
  patterns that would recur for the *next* session working this repo.
- Don't silently rewrite `CLAUDE.md`'s architectural-decision sections ("не пересматривать без
  обсуждения") — those need the user, not a retro.

## What to produce

For each finding: a short **before/after** — what's missing or wrong today, and the exact edit
(file + concrete text, not just "improve the docs section"). Group findings as:

1. **Apply now** — small, unambiguous fixes (a new bullet in `.claude/rules/index.md`, a
   corrected fact in `CLAUDE.md`, a note added to an agent's `.md` about a tool gap). Make these
   edits directly, the same way any other low-risk documentation fix would be made this session.
2. **Propose to the user** — anything that would change how autonomously the conductor operates,
   add/remove a subagent, or touch an architectural-decision section — describe the change and
   why, but don't apply it without confirmation.

End with a short summary: what was fixed directly, and what's waiting on the user.
