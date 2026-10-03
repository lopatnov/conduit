---
description: Produce a self-contained handoff summary for continuing this exact work in a brand-new session (new chat, new environment, or after this session is archived).
argument-hint: "(none — reads this session's own conversation state)"
---

# /handoff — continue this work elsewhere

The user is about to archive/close this session (or switch machines) and wants a summary they
can paste as the **first message of a brand-new session** to pick up exactly where this one left
off — without that new session re-deriving anything from scratch.

Write the summary **as a message addressed to that future session**, not as a report to the
user. Assume the reader has: the repo's `CLAUDE.md` and `.claude/` (read fresh, so don't restate
policy that's already written down there), but **zero memory of this conversation**.

## What to include

1. **One-paragraph goal** — what this whole session was working toward, in plain language.
2. **State right now** — exact branch names, PR numbers/URLs, commit SHAs, CI status as of the
   last check, and precisely what step is in-flight vs. finished vs. not-yet-started. If
   something is mid-flight (a subagent running, CI pending, a scheduled check-in armed), say so
   explicitly and what the next action is once it resolves.
3. **Environment quirks discovered this session** — anything that cost real time to figure out
   and would burn the same time again if rediscovered from scratch. Concretely: broken tooling
   and its workaround (e.g. "local `git push` fails with 'could not read Username' — use
   `mcp__github__push_files` instead, which goes through the already-authenticated API path, not
   local git credentials"), any tool-gap you hit and how you routed around it (or *didn't*, per
   the "don't route around a missing tool" rule — say what you reported instead), unusual repo
   state, or a wrong assumption you had to correct.
4. **Decisions made and why** — anything the user or you decided that isn't obvious from the
   code/PR alone (e.g. "left Cargo.lock un-synced because CI never uses `--locked`" — the
   reasoning, not just the fact, so the next session doesn't re-litigate it).
5. **Scheduled/background work still pending** — any `ScheduleWakeup`, `RemoteTrigger`, or
   cron job tied to *this* session. **This is the critical part**: a Routine bound to this
   session (`persistent_session_id` = this session's own ID, "self-bind" mode) will **not**
   deliver into the new session — it keeps firing into this one, which is about to go away or
   be archived. (There is no periodic-rotation command to defer to for this — see
   `.claude/rules/index.md` "Session rotation retired." A handoff like this one is the
   deliberate manual case, still fully supported; only the *scheduled* version was retired.)

   Call `RemoteTrigger action:"list"` fresh (don't guess from memory) and find every entry
   whose `persistent_session_id` matches this session's own ID. For each one, tell the user:
   its `id`, `name`, `cron_expression`, and that — because a trigger can't be created bound to
   a session that doesn't exist yet — the *old* session (this one) needs to create the
   replacement trigger itself (`RemoteTrigger action:"create"`, same `cron_expression` and
   `prompt`, `persistent_session_id` set to the *new* session's ID) once the user has created
   that new session and supplied its ID, then the old trigger should be disabled via
   `RemoteTrigger action:"update"` (there is no delete action — `enabled:false` is the
   available equivalent). Give the exact prompt text the new trigger should use. Two
   load-bearing details, confirmed the hard way in `.claude/logs/session-rotation.md`'s
   history: (a) the *old* session creates the trigger bound to the new session's ID, not the
   other way around; (b) do **not** spawn the replacement session via a `create_session`-style
   tool — a confirmed platform bug leaves a session created that way with zero GitHub/MCP tool
   access. The user must create the new session themselves and hand back its ID.
6. **Next concrete step** — the literal first thing the new session should do, not a vague
   "continue the migration." If it's "wait for CI on commit X then post a PR comment," say that.

## Before writing the summary — this is the moment to check `/fork` and `/recap`

> Added 2026-08-29: `/fork` and `/recap` are real built-in Claude Code commands (per
> official docs, via `claude-code-guide`) that may make parts of this procedure
> unnecessary — `/fork` might create a new session that *keeps* MCP/GitHub tool access
> (unlike `create_session`, confirmed broken — see item 5 above), and `/recap` might
> already produce what this command's own summary is manually built to produce. Neither
> has been verified yet, and both are client-invoked (the user types them, not the agent),
> so the only place to actually test them is a genuine handoff moment like this one —
> **not** a disconnected "please go try this" ask outside of any real need.

Since a real handoff is happening right now: **ask the user to try `/fork` and `/recap`
here before falling back to the manual procedure above.** Specifically —
- `/fork`: if it creates a session that already has GitHub/MCP tools, that may fix the
  underlying problem item 5 works around, and steps 2-6 could target the forked session
  directly instead of asking the user to create one from scratch.
- `/recap`: compare its output against what this command would otherwise produce by hand
  (items 1-6 above). If it already covers the same ground, say so.

Report back whatever actually happens (tool access present or not; recap content
sufficient or missing something) so this file and `.claude/rules/index.md` "Session
rotation retired" can be corrected with a real result instead of staying speculative. If
either doesn't pan out, fall back to the manual procedure in items 1-6 as normal — this
check shouldn't block or delay the handoff itself.

## Format

Plain text or light markdown, written in the second person to the future session ("You're
picking up..."), not a bulleted status report to the human. End with a short **"tell the user"**
line only if there's something time-sensitive the human specifically needs to do (e.g. delete
and recreate a Routine) — everything else in the summary is for the new session to act on, not
for the human to relay.

Keep it dense but complete — this replaces re-reading the whole transcript, so err on the side of
including a fact rather than omitting it for brevity. Don't pad with generic advice ("write clean
code") that's already in `CLAUDE.md`/`.claude/rules/`.
