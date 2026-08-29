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
5. **Scheduled/background work still pending** — any `ScheduleWakeup`, `send_later`, or Routine
   (`list_triggers`) tied to *this* session. **This is the critical part**: a Routine bound to
   this session (`persistent_session_id` = this session's own ID, "self-bind" mode) will **not**
   deliver into the new session — it keeps firing into this one, which is about to go away or be
   archived. For each such Routine, tell the user explicitly:
   - Its `trig_id`, name, and cron schedule (call `list_triggers` to get these fresh — don't
     guess from memory).
   - That it must be deleted (`delete_trigger`) and recreated from the *new* session once that
     session exists (a new session calling `create_trigger` with the same cron/prompt binds to
     itself automatically) — this session cannot create a trigger bound to a session that doesn't
     exist yet, so this step has to happen from the new session, not this one.
   - The exact prompt text the new trigger should use (usually the same slash command/prompt the
     old one used, updated for anything that changed this session).
6. **Next concrete step** — the literal first thing the new session should do, not a vague
   "continue the migration." If it's "wait for CI on commit X then post a PR comment," say that.

## Format

Plain text or light markdown, written in the second person to the future session ("You're
picking up..."), not a bulleted status report to the human. End with a short **"tell the user"**
line only if there's something time-sensitive the human specifically needs to do (e.g. delete
and recreate a Routine) — everything else in the summary is for the new session to act on, not
for the human to relay.

Keep it dense but complete — this replaces re-reading the whole transcript, so err on the side of
including a fact rather than omitting it for brevity. Don't pad with generic advice ("write clean
code") that's already in `CLAUDE.md`/`.claude/rules/`.
