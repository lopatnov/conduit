---
description: Rotate a long-running self-bind session to a fresh one — create the new session, repoint its Routine at it, retire the old binding, and log the handoff in CLAUDE.md.
argument-hint: "(none — operates on the calling session's own Routine binding)"
---

# /session-rotate — hand a self-bind Routine off to a fresh session

Call this when a session bound to a recurring Routine in self-bind mode has accumulated
enough context that it's worth starting clean — typically every ~10-20 firings (see
`.claude/commands/feature-workspace-cycle.md` Step 0a, which checks that threshold and
invokes this). Can also be run ad hoc if a session is visibly struggling (rate limits,
noticeably slower/costlier turns) well before the usual threshold.

> Added 2026-08-28 after this repo's `/feature-workspace-cycle` session — alive
> continuously since 2026-07-29, never rotated — hit the account's session-wide model
> rate limit mid-firing while triaging a security-alert batch. Kept as its own
> command (not inlined into `feature-workspace-cycle.md`, and not a `rules/` entry)
> so the procedure only loads into context when actually needed, rather than on every
> single turn of every session.

## ⚠️ Known platform limitation — do not use `create_session` for the replacement

> Confirmed by the user 2026-08-28, first hit by the very first rotation this
> procedure ever ran: a session spawned via `mcp__Claude_Code_Remote__create_session`
> (i.e. one Claude session creating another) comes up with **no GitHub API/MCP tool
> access at all** — no `gh` CLI, no `mcp__github__*` tools, no GitHub entry in
> `ListConnectors`. Only unauthenticated `WebFetch` on public pages and plain
> `git clone`/`push` still work. A session the *user* creates directly does not have
> this problem. Root cause unknown (the user's own words: "это твой баг" — a platform
> bug, not something fixable from inside this repo). Since essentially every step of
> `feature-workspace-cycle.md` (Steps 1, 2, 6-9) depends on GitHub MCP tools, a
> replacement session spawned this way cannot actually run the cycle it was rotated
> in to continue — it can orient and report, nothing more.
>
> **Until this is fixed upstream: do not execute Step 1 below (`create_session`) at
> all.** Instead, stop and ask the user to create the new session themselves (however
> they normally start a session against this repo) and give you its session ID. Only
> proceed to Steps 2-5 once you have a session ID the user actually created. If a
> rotation was already forced by something urgent (e.g. a rate-limit hit) before the
> user could be consulted, the interim session should still orient and report back
> per its handoff prompt, but must flag this exact limitation instead of silently
> proceeding as if the cycle can resume normally — see the 2026-08-28 row in
> `CLAUDE.md`'s "Session rotation log" for the precedent.

## Preconditions

- Make sure `CLAUDE.md` is actually up to date before rotating — the whole point of the
  handoff is that the new session can pick up full context *from `CLAUDE.md` alone*. If
  the current firing's own work isn't logged yet, log it first (this is usually already
  true if the calling command's own "log the summary" step ran before this).

## Steps

1. ~~`mcp__Claude_Code_Remote__create_session`~~ — **do not use this**, per the known
   limitation above. Instead, ask the user to create the new session themselves and
   supply its session ID. Once you have that ID, tell it (or make sure its own first
   read of `CLAUDE.md` will tell it) to read `CLAUDE.md` in full — especially the
   newest "Реализовано в сессии" entries and the "Session rotation log" table — to
   pick up context, and then to wait for its next scheduled firing rather than
   starting Step 1-9 work immediately (let the Routine's own next tick, now pointed
   at it, drive that normally).
2. **`mcp__Claude_Code_Remote__list_triggers`** — find the Routine currently bound to
   *this* session, and read its exact `cron_expression` and `prompt` verbatim (don't
   reconstruct these from memory — copy them from the tool result).
3. **`mcp__Claude_Code_Remote__create_trigger`** — same `cron_expression` and the same
   `prompt` text the old Routine fires, with `persistent_session_id` set to the new
   session's ID from step 1 (mode 2: "fire into a specific other session" — not
   self-bind, since the caller here is the *old* session creating a trigger on behalf of
   someone else).
4. **`mcp__Claude_Code_Remote__delete_trigger`** on the *old* trigger_id from step 2.
   Once this succeeds, this session stops receiving future firings.
5. **Append one row** to `CLAUDE.md`'s "Session rotation log" table: date, old session
   ID, new session ID, approximate firing count since the last rotation (or since the
   Routine was first created, if the table was empty), and a one-line reason (e.g.
   "scheduled rotation at ~15 firings" or "rate limit hit mid-firing"). Commit and push
   this alongside whatever other `CLAUDE.md` updates are pending.
6. **Stop.** Don't continue with whatever work the calling command was in the middle of
   — the new session's next firing picks that up fresh from `CLAUDE.md`. If the current
   firing had uncommitted/unpushed work in progress, commit and push it first (a rotation
   should never strand in-flight work in an unreachable session).
