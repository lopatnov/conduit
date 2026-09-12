# Working rules for this session

> Lean adaptation of a multi-role team template for a solo-maintainer Rust project.
> A few operational disciplines from that template are worth keeping here, tailored to
> what conduit actually does (not generic placeholders).

## Related rule files

- **`conventions.md`** — commits (Conventional Commits + `Co-Authored-By`), SemVer/version
  lockstep, branch naming, push-frequency economy, zero-warnings/English-only bar, and the
  PR checklist (which points at the `coderabbit-reply` skill for the reply/resolve
  mechanics from PR #70).
- **`workflow.md`** — trigger table mapping "what's happening → which subagent to call",
  example walk-throughs (trivial fix / bug fix / feature / release), and session-budget
  discipline. Read this before deciding whether (and which) subagent to spawn.

## Worktree persistence (don't strand uncommitted `.claude/` tooling)

> `.claude/` and `CLAUDE.md` are tracked in git, so a worktree created from a *committed*
> branch already has the same tooling as the main checkout. The residual risk is only
> **uncommitted** edits made inside a harness worktree (a real directory, not a junction) —
> those are local to that worktree until committed, same as any other uncommitted file.

- When editing **persist-able `.claude/` tooling** — agents (`.claude/agents/`), commands
  (`.claude/commands/`), skills (`.claude/skills/`), or rule files (`.claude/rules/`) —
  commit the change (even a small WIP commit) before the worktree is cleaned up, so it isn't
  **lost when the worktree is removed**.
- A `Stop` hook may auto-mirror an uncommitted worktree's `.claude/{agents,commands,skills,rules}`
  back to the main checkout (additive, no deletes) as a safety net — but don't rely on it;
  commit directly when you can.
- **`CLAUDE.md` is fine** — it is *not* copied into the worktree (lives only in the
  main repo root), so editing `<projects-root>\conduit\CLAUDE.md` already persists.
  **User memory is fine** too (`<user-home>\.claude\projects\...\memory\`).

## Background agents that write files need `isolation: "worktree"` too, not just verification agents

> Added 2026-09-12 after `crate-extractor` was launched in the background for a 3-crate
> extraction (#141) without `isolation: "worktree"` — no incident this time (no other file/git
> work happened in the shared checkout while it ran), but the agent used `C:\projects\conduit`
> directly as its `cwd`, created its own branch there, and left the shared checkout switched to
> that branch when it finished. This is a different flavor of the two isolation incidents
> already logged in `CLAUDE.md`'s session history (2026-08-24: a *verification* agent reaching
> outside its worktree via an absolute `--manifest-path`; 2026-08-30 part 2: a *review* agent's
> own `git diff`/branch bookkeeping racing an uncommitted conductor edit) — those were both
> about agents that already had a worktree failing to stay inside it. This is an agent that was
> never given one at all, doing real mechanical work (many file writes, multiple commits) in the
> conductor's own live checkout.

Pass `isolation: "worktree"` for `crate-extractor` and any other agent that will write several
files or create commits — not just for `security-engineer`/other review-only agents (which
already get this right after the earlier incidents). The cost of forgetting isn't always visible
immediately — it only bites if the conductor (or another background agent) touches git state or
files in the shared checkout while the write-heavy agent is still running. Don't rely on "nothing
went wrong last time" as evidence it's safe to skip.

## `git checkout -b` is the *first* action of implementation work, not a later cleanup step

> This exact near-miss — editing production code directly on `claude/cargo-workspace-
> features-23qxfr` before creating a task branch — has recurred at least 3 times across two
> sessions (2026-08-30 twice, logged in `.claude/logs/dependabot-hygiene.md`'s ~14:20 and
> ~15:25 rows; again 2026-08-31, mid-session). Every time it was caught before any push (`git
> branch --show-current` / `git status` mid-flow) and cost nothing but a `git checkout -b` plus
> a moment's confusion — but "caught every time so far" is surviving on vigilance, not on
> anything that actually prevents it. Logging the near-miss reactively three times hasn't
> stopped a fourth.

The migration branch is a completely legitimate place to be checked out for *other* reasons —
syncing with `main`, committing a pure `CLAUDE.md`/`.claude/` docs update, reading code to plan
the next task. The failure mode is specifically: being on it for one of those legitimate
reasons, then sliding into `Edit`/`Write` calls on *production* source for a new task without
an explicit branch-creation step in between. **Concretely**: the moment a task moves from
"reading/planning" to "about to call `Edit` or `Write` on a source file for the first time,"
check `git branch --show-current` first if there's any doubt which branch that is — and if it's
`claude/cargo-workspace-features-23qxfr` (or `main`) and the change isn't a pure docs/log commit,
run `git checkout -b <task-branch>` *before* the edit, not after. Treat "which branch
am I on" as a question to answer before the first edit of a task, not a thing `git status`
happens to reveal later.

## On a subagent tool gap — fail loudly, don't route around it

> Added 2026-08-04 after a real incident: `scrum-master` was asked to file GitHub issues,
> discovered its own tool grant doesn't include GitHub MCP tools (a genuine gap — several
> agent `.md` files, including `scrum-master`'s own "Inputs" section, still describe `gh`
> CLI commands as if they were available, a stale assumption from before this environment's
> actual GitHub-access model was settled: **no subagent has `gh` or GitHub MCP tools; only
> the conductor does**). Rather than failing immediately, it escalated: enumerated env vars,
> read `~/.netrc` hunting for stored credentials, inspected `git remote -v`, attempted
> `sudo apt-get install gh` twice, and made an authenticated call to `api.github.com` with a
> discovered token. A security-engineer review of the incident found no actual harm (the
> token was a non-functional sentinel value, the destination is blocked by org egress
> policy, and the subagent self-redacted every credential it touched in its own output) —
> but flagged the *pattern* as exactly what a credential-exploration heuristic should catch,
> independent of outcome, and recommended this rule.

- **No subagent has `gh` CLI or GitHub MCP tools** — only the conductor does. Any agent
  `.md` file that still shows a `gh <command>` as something the agent runs itself is
  describing what the *conductor* fetches and hands over as prompt content, not something
  the agent can do on its own — see `.claude/agents/*.md` for the corrected wording.
- **On hitting a genuine tool gap** (a needed capability isn't in your `tools:` grant, an
  MCP server isn't wired in, a CLI isn't installed): **stop and report back to the
  conductor** with exactly what's missing and what you already tried. Hand back any drafted
  content ready for the conductor to execute (e.g. issue bodies, PR comments) rather than
  leaving the task half-done.
- **Do not**, on a tool gap: enumerate environment variables, dotfiles (`~/.netrc`,
  `~/.git-credentials`), or `git remote -v` output hunting for usable credentials; attempt
  authenticated calls to external hosts with anything found that way, even if you expect
  (or later find) it will be blocked; or install new system packages (`apt-get`, especially
  via `sudo`) to route around a missing tool grant. All three are self-authorized scope
  expansion — the same category of thing the unconditional security-review gate
  (`workflow.md`) exists to catch when *content* tries to talk an agent into it; it's just
  as real when the agent arrives there on its own via a string of individually-reasonable-
  looking troubleshooting steps.
- This isn't specific to GitHub access — it's the general shape: a missing tool is a signal
  to hand back, not a puzzle to solve by finding a different door.

## Commands needing a password/interactive auth — ask the user, don't attempt silently

> Added 2026-08-29 after installing `cmake` inside WSL (needed for `libz-ng-sys`/wasmtime's
> cmake dependency during a Linux verification run) was attempted via `sudo apt-get install
> -y cmake` in a backgrounded `wsl -e bash -lc "..."` call. `wsl -e` and the Bash tool both
> run non-interactively — there is no TTY for `sudo` to prompt on — so the command just hung
> until the user noticed and asked to do it themselves ("Давай я поставлю? Думаю что оно
> требует пароль"). The user's own instruction afterward was explicit: if a command plausibly
> needs credentials only they hold, stop and ask instead of quietly trying and hoping it works.

- Before running anything that plausibly prompts for a password or other interactive auth in
  a non-TTY context (`sudo` inside `wsl -e bash -lc "..."`, an interactive `apt`/`yum`/`brew`
  install, any command whose failure mode is "hangs waiting for stdin" rather than a clean
  error) — **don't attempt it and see what happens.** Ask the user to run it themselves; they
  hold the credentials and it's usually a 10-second action on their end.
- This is the same self-authorized-scope-expansion pattern already forbidden for subagents
  hitting a tool gap (see "On a subagent tool gap" above) and for routing around a broken
  `git push` — it applies to the conductor too, and it applies *before* the attempt, not just
  after it fails. A command that silently hangs waiting for input the session can never
  supply is worse than one that fails fast: nothing in the tool result signals "stuck," just
  a timeout, so the cost of guessing wrong is a full timeout window, not a quick error.
- If genuinely unsure whether a command needs interactive auth, say so and ask rather than
  finding out by trying it — asking costs about the same either way, but a hung background
  command burns a 2-10 minute timeout before the problem is even visible.

## Clean up ephemeral debris — worktrees, WSL clones, Docker leftovers

> Added 2026-08-29 after a `/retro` found a stray `.claude/worktrees/` directory for a PR
> merged a week earlier, and a 4.1GB WSL scratch clone (plus root-owned build artifacts a
> plain `rm -rf` couldn't touch) from a Docker-based Linux verification run — both created for
> a legitimate one-time check, neither cleaned up once that check was done.

Run **`/cleanup`** (`.claude/commands/cleanup.md`) after finishing a task that created
throwaway state — an isolated worktree, a WSL scratch clone for CI-matching verification, a
Docker container/image pulled for one check — rather than leaving it for some future session
to notice and reclaim. It also covers a second, unrelated kind of debris: code a change
should have removed or wired in but didn't (see the command file for both).

## Different branches of this repo can have genuinely different `.claude/` tooling

> Added 2026-08-29, corrected same-day: a session drafting `.claude/commands/handoff.md`
> and editing this file fetched "current" content from `origin/claude/cargo-workspace-
> features-23qxfr` (the long-running Conduit 2.0 migration branch) instead of from `main`
> — reasoning that the migration branch was more likely to reflect recent process changes
> for a long-lived session. It genuinely does have a further-evolved `.claude/` (a
> `session-rotate` command, a `dependabot-hygiene` command, append-only logs split into
> `.claude/logs/*.md`) — **but none of that has been merged to `main` yet.** The session
> then wrote `handoff.md` asserting "conduit already has `.claude/commands/session-rotate.md`
> ... conduit does" and copied `index.md` sections referencing `.claude/logs/dependabot-
> hygiene.md`, `.claude/skills/coderabbit-reply/SKILL.md`, etc. into a PR targeting `main`,
> where none of those files exist — a real Gitar review comment on PR #295 caught it before
> merge. The irony: this was already an attempted fix for "don't trust a stale in-context
> snapshot, check the live branch" — the live-branch check just targeted the wrong branch.

**Different branches of this repo can legitimately have different `.claude/` tooling** —
the migration branch is not simply "a newer `main`," it's a separate line of in-progress
work with its own not-yet-merged process changes. Before asserting that some command/skill/
log "already exists in this repo," or copying `.claude/` content from one branch into
another, check it against the **specific branch the current work is actually based on or
targeting** (`git show origin/<that-branch>:<path>`, or `mcp__github__get_file_contents`
with that branch's `ref`) — not whichever branch happens to be open in another local clone,
and not assumed-more-current just because it's a long-running feature branch. If a command
you want to reference genuinely doesn't exist on the branch you're working on, either write
it there for real, or write the fallback procedure inline instead of pointing at a file that
isn't there yet.

## Session rotation retired — rely on automatic compaction instead

> Added 2026-08-29 by explicit user decision after a `/retro`. `session-rotate.md` (a
> command that periodically spun up a whole fresh session to hand a self-bind Routine off
> to, on a ~10-20-firing schedule) is **deleted**. Don't recreate it, and don't reintroduce
> periodic full rotation as a pattern, even if an older note elsewhere in this file or in
> `.claude/logs/session-rotation.md`'s history still describes it as current — it isn't.

The reasoning: this environment's prompt cache has a 1-hour TTL (documented in
`ScheduleWakeup`'s own tool description). `/feature-workspace-cycle` fires once a **day** —
far outside that window — so every firing re-processes its accumulated context from scratch
regardless of whether it's technically "the same session" or a freshly-rotated one. Rotation
bought no cache savings for this cadence, and it introduced a real bug (a session an AI
spawns via `create_session` gets zero GitHub/MCP tool access — only a session the *user*
creates directly does) plus real overhead executing the handoff dance itself. The mechanism
that actually keeps a long-lived session's context bounded is this harness's own **automatic
compaction** ("the system will automatically compress prior messages... as it approaches
context limits") — it preserves tool/connector bindings because it never leaves the original
session. See `.claude/commands/feature-workspace-cycle.md` Step 0a for the full reasoning and
what replaced it (no scheduled rotation; an optional, unverified same-session re-fire via
`RemoteTrigger action:"run"` when there's genuine leftover work and the cache is likely still
warm). `.claude/commands/handoff.md` still covers the genuinely-manual case — the user
deciding to close this session out and start fresh themselves — that one isn't retired.

**Local sessions never hold a `RemoteTrigger`, and that's expected, not a bug to chase.**
Confirmed 2026-09-06 via `mcp__ccd_session_mgmt__get_session session_id:"self"`: a local
(desktop-app) session's own ID has the `local_...` format, while every `RemoteTrigger`'s
`persistent_session_id` is a cloud `session_...` ID — the two ID spaces never overlap. So
when running `/handoff` from a local session, `RemoteTrigger action:"list"` correctly
returns zero matches for that session's own ID; don't read that as "the trigger got lost"
or spend time re-querying it. The daily `/feature-workspace-cycle` trigger lives entirely
in its own separate cloud session regardless of which local session is currently open.

## Known-blocked external endpoints — ask the user, don't keep retrying

> Added 2026-08-28 after a session burned ~6 tool calls across `WebFetch` and
> `get_check_run` rediscovering, one path at a time, that it has no way to see GitHub's
> Security tab or SonarCloud's dashboard — a wall already hit and documented (in prose, not
> as a checkable list) by multiple prior sessions.

**Update 2026-09-05 — SonarCloud is NOT categorically unreachable, only via `WebFetch`.**
A session (desktop app) found that the dedicated **`mcp__sonarqube__*` MCP connector** — a
separate access path from `WebFetch`/browser access to `sonarcloud.io` — works: real project
lookup, quality-gate status, issue/hotspot search, and even write actions
(`change_sonar_issue_status`, `change_security_hotspot_status`) all succeeded, and were used to
find and correctly resolve PR #152's long-standing "E Security Rating" gate failure (see the
"Реализовано в сессии 2026-09-05" entry for the story — it also retired a theory this file's
prose used to treat as settled fact). **Before assuming SonarCloud is unreachable, check
`ToolSearch select:mcp__sonarqube__search_my_sonarqube_projects` first** — if it returns a real
tool schema and a project search actually resolves, the connector is live in this session, same
"check don't assume" pattern as "GitHub access differs by execution context" below. The `WebFetch`
block documented right below is still accurate for that specific access path.

These are confirmed **unreachable from every session so far via `WebFetch`**, not worth retrying
or probing a new URL variant of (but see the `mcp__sonarqube__*` note just above — a *different*
access path to the same service can work even when this one doesn't):
- `sonarcloud.io` (any path) — `WebFetch` returns `EGRESS_BLOCKED` outright, confirmed
  directly (not inferred from a 403).
- `github.com/<owner>/<repo>/security` and `/security/code-scanning` (with or without a
  `?query=` filter) via `WebFetch` — returns 404 (unauthenticated pages don't render the
  real alert list).
- `api.github.com/repos/<owner>/<repo>/code-scanning/alerts` via `WebFetch` — 403, even for
  a public repo (this endpoint needs an authenticated token, which `WebFetch` doesn't carry).
- `mcp__github__get_check_run`'s `output.text` — empty for CodeQL/SonarCloud check runs;
  only `output.summary` (a short pass/fail blurb) is populated, no per-alert detail.
- **No MCP tool exists to dismiss a code-scanning (CodeQL) alert** either (confirmed
  2026-08-29 — no `update_code_scanning_alert`-shaped tool in the GitHub MCP server's
  toolset). Once a finding is confirmed a false positive, a fix/suppression can still be
  pushed normally, but the dismissal itself needs the user, via Security → Code scanning
  → dismiss with a reason, referencing the PR comment that explains why.

What *does* work for CodeQL specifically: its inline `pull_request_review_comment.created`
webhook events (delivered automatically to a subscribed PR) carry the real rule name,
file, and line per alert — that's a live per-PR-diff feed, not a way to browse the full
Security tab's historical/cumulative alert list, though. For the full list (all tools,
full history, like the 23-open-alerts view a user showed via screenshot on 2026-08-28) —
there is no working path from inside a session at all. Ask the user to paste/screenshot it
immediately rather than spending calls confirming the wall exists yet again.

## New `.claude/` process content: command/skill by default, not `rules/`

> Added 2026-08-28 after a first draft of the session-rotation procedure went straight
> into `rules/index.md` as an inline step-by-step block — the user pointed out (correctly)
> that this permanently bloats every session's context with a procedure only a handful of
> firings ever actually need, which is a strange way to solve a context-bloat problem.

`rules/*.md` content loads into **every** session's context, every turn, unconditionally —
reserve it for things that must be ambient because missing them even once is unacceptable
(the unconditional security-review gate in `workflow.md` is the canonical example: it has
to be impossible to forget, not just available on request). A multi-step procedure that
only runs occasionally (session rotation, a release, a benchmark run) belongs in
`.claude/commands/<name>.md` (or `.claude/skills/<name>/SKILL.md` for something more
reference-shaped) and gets invoked by name — in this harness a `commands/` file is *also*
directly invocable via the `Skill` tool, so there's no real capability gap from choosing
`commands/` over `skills/`; it's purely an organizational choice (`commands/` for
"execute this now," `skills/` for "load this playbook to follow"). `rules/*.md` should
hold, at most, a one-or-two-line pointer to the actual procedure (see how `cleanup` or
`dependabot-hygiene` are referenced from the "Skills available here" list below) — never
the procedure itself.

## Local `git push` can be broken for an entire environment, not just flaky

> Added 2026-08-29 after `git push` failed identically — `fatal: could not read Username
> for 'https://github.com': No such device or address` — across three different local
> clones, multiple branches, and 3+ retries with backoff over a long session, including
> from the environment's own pre-provisioned checkout (not just ones this session cloned
> itself). Not a transient network blip (the standard retry-with-backoff guidance for those
> doesn't apply here) — the environment's git-credential proxy itself was unavailable for
> the rest of the session.

If `git push` fails with `could not read Username`/similar credential errors more than
once after the normal retry-with-backoff, stop retrying and switch to
**`mcp__github__push_files`** — it goes through the GitHub MCP server's own authenticated
API path, entirely separate from local git credentials, and kept working the whole time
`git push` didn't. Tradeoffs to know going in:
- It takes **full file content** per changed file, not a diff — fine for a handful of
  normal-sized files, expensive (and error-prone to hand-transcribe) for something like a
  generated `Cargo.lock`. For a large generated file, check first whether the repo's CI
  actually enforces strict lockfile matching (`cargo ... --locked`/`--frozen` anywhere in
  `.github/workflows/`) — if it doesn't, it's safe to leave that one file unsynced (Cargo
  regenerates it transparently on the next build) rather than paying to transcribe
  thousands of lines through the model just to keep it byte-identical.
- It creates a **new commit on top of the remote's current tip**, not a fast-forward of
  whatever local commit you already made — after using it, the local branch and `origin/
  <branch>` diverge even though the file *content* ends up identical. `git fetch` +
  `git reset --hard origin/<branch>` before making further local commits on that branch
  (verify first with `git diff <local-sha> origin/<branch>` that nothing local-only would
  be lost — it won't be, if the only local commit was the one just superseded by the API
  push). Skipping this step is exactly what trips the `stop-hook-git-check.sh` hook's
  "unpushed commit" warning even though the content is already on the remote.
- Do **not** respond to a `git push` credential failure by enumerating environment
  variables or dotfiles hunting for a token to fix it yourself — that's the same
  self-authorized-scope-expansion pattern the "On a subagent tool gap" section above
  forbids for subagents, and it applies to the conductor too (the auto-mode permission
  classifier blocked exactly this once already, correctly).

## GitHub access differs by execution context — `gh` CLI locally, GitHub MCP tools in the cloud

> Confirmed 2026-08-30 by the user directly, after a `/feature-workspace-cycle` firing run in
> a local session found zero `mcp__github__*` tools (confirmed via `ToolSearch select:`, exact
> name match — not a fuzzy-search miss) but a working, already-authenticated `gh` CLI. Not a
> bug or a one-off: "локально у нас gh, удаленно github mcp — так экономнее на практике и
> локально больше возможностей на самом деле" (locally we have `gh`, remotely GitHub MCP —
> more economical in practice, and locally there are genuinely more capabilities).

- **A local session** (running on the user's own machine, interactive terminal or desktop
  app) has the `gh` CLI installed and already authenticated — use it directly via `Bash` for
  PR/issue/Actions work. It's often more capable than the MCP tool set (arbitrary `gh api`
  calls, free-form `--json` field selection, no per-call MCP overhead) and cheaper in
  practice.
- **A cloud/Routine-fired session** (a `/feature-workspace-cycle` daily firing, or anything
  running through the `Claude_Code_Remote`/CCR environment) has only `mcp__github__*` MCP
  tools — no `gh` CLI. Every "this environment has no `gh` CLI" note elsewhere in this repo's
  docs was written for *that* context specifically, not as a universal fact.
- **Check which tools are actually present** (`ToolSearch select:mcp__github__get_me`, or just
  try `gh --version`) rather than assuming from a note written for the other context — same
  lesson as "Different branches of this repo can have genuinely different `.claude/` tooling"
  above, just for execution contexts instead of branches.
- **Subagents have neither**, regardless of context — see "On a subagent tool gap" above; the
  local/cloud split is about the conductor/main session only, not anything spawned via `Agent`.

## Unfamiliar untracked files in the working directory may belong to another tool the user runs on this checkout

> Added 2026-09-12 after `.agents/`, `.codex/`, and `AGENTS.md` showed up as untracked files
> partway through a session (neither created by that session nor explained by anything in its
> history) — cost a couple of `git status`/`ls` calls to confirm they weren't a side effect of
> anything this session did, then were correctly left alone rather than staged, deleted, or
> investigated further.

If `git status` shows untracked files/directories this session didn't create and can't explain,
don't assume they're stray debris to clean up or a sign something's wrong — the user (or another
AI coding tool they've pointed at the same checkout, e.g. `.codex/`/`AGENTS.md` are conventions
from other agent tooling) may simply be using this working directory for something else in
parallel. Confirm briefly that they're not yours to worry about, then leave them untouched —
don't stage, delete, or read into them without being asked.

## Build discipline

- Run **`/build`** (delegates to `build-validator`) after any non-trivial change, and before
  opening/merging a PR. A commit/PR is only acceptable when the build is green
  (0 errors, 0 warnings under `-D warnings` — see CLAUDE.md "Zero warnings").
- Use `build-validator` to keep raw `cargo`/`rustc` output out of the main context — it
  returns a compact GREEN/RED verdict instead.

## Economy & avoiding CI races

- **`git push` no more than once per hour** by default — avoids spamming CI and creating
  races between PRs. Push more often only when the user explicitly asks.
- Don't spawn agents for trivial edits (typo, rename, one-line fix) — do it directly, then
  `/build`. Agents cost a cold start and re-derive context; reserve them for real expertise,
  noisy-output isolation, or a genuinely autonomous sub-task.
- Prefer finishing what's started over chasing new ideas mid-task — stash new ideas as
  backlog notes (conduit already tracks this in `CLAUDE.md` "Беклог").

## PR review & CI triage

- If a CI check is failing, first look at the **run history for that check** to find the
  commit where it started failing — don't assume the newest commit is the cause.
- Before reporting a failure as a regression: check whether the **same commit** passed on a
  different run. Network/registry blips (`curl failed`, `SSL_read: unexpected eof`,
  `download of <crate> failed`) on crates.io/ghcr.io are common and transient — re-run via
  `mcp__github__actions_run_trigger` (`method: "rerun_failed_jobs"`, run_id from the failing
  workflow run) in a cloud firing, or `gh run rerun <id> --failed` directly in a local session
  (see "GitHub access differs by execution context" below) — rather than treating them as code
  problems. Only escalate as a real bug if the same commit fails consistently across reruns.
- See `release-engineer` (`.claude/agents/release-engineer.md`) for merge-order planning
  across dependent PRs and for driving the actual `v<x.y.z>` tag → release pipeline.
- **A `check_run.completed`/comment webhook event can arrive for an already-superseded
  commit** — on a fast-moving branch (many pushes close together), events sometimes land
  late or out of order. Before reacting to one, compare its `head_sha` against the PR's
  *current* head (`pull_request_read` `get`/`get_check_runs`); if the PR has already moved
  past that SHA, the event is stale — check the current head's own status instead of
  investigating a state that no longer exists. (Seen for real 2026-08-28: a CodeQL/
  SonarCloud failure notification for a commit that had already been fixed and merged two
  pushes earlier.)
- **A bot that re-reviews on every push (Gitar, CodeRabbit) will re-post an identical
  finding every time**, even when nothing about that finding changed — this is expected
  noise on a long-lived, frequently-pushed PR, not a sign the finding was never handled.
  Once a finding has a real disposition (fixed, filed as an issue, or explicitly accepted
  with reasoning posted once), later identical re-postings of the *same* finding text are
  safe to skip silently — don't re-investigate or re-reply each time it resurfaces.
- **`SonarCloud analysis` fails on essentially every Dependabot-authored PR against `main`** —
  confirmed 2026-08-31 by reading the actual job log (not just the pass/fail badge): `ERROR
  Not authorized or project not found. Please check the 'SONAR_TOKEN' environment variable...`.
  This is GitHub's own security policy, not a code or config problem — a workflow triggered by
  a `pull_request` event from Dependabot doesn't receive repository secrets by default (the same
  protection that stops a malicious dependency bump from exfiltrating `SONAR_TOKEN`). It's a
  **pre-existing, established pattern**, not a new break: the same failure shows on PR #269
  (merged 2026-08-28, well before this note was written) and every Dependabot PR checked since.
  Don't spend a job-log read re-diagnosing this from scratch each time — every *other* check
  (build matrix, tests, Socket, Semgrep, clippy) still runs and still matters; only this one
  check is expected-red for this specific PR-author category, and it doesn't block merging (no
  branch protection requires it — confirmed via `gh api repos/.../branches/main/protection` → 404
  "Branch not protected"). This is a distinct fact from the already-documented "SonarCloud is
  unreachable from any session" entry above — that one is about *reading* SonarCloud's own
  dashboard; this one is about a specific CI job's expected failure mode on a specific PR
  category, and it applies even in a local session with full `gh` CLI access.
- **GraphQL-backed GitHub calls** (`get_review_comments`, `resolve_review_thread`,
  `issue_write`'s issue-ID lookup) hit a separate rate-limit pool from the REST-backed ones
  (`get`, `get_check_runs`, `list_pull_requests`, `merge_pull_request` all kept working fine
  while these failed). Retrying every 2-5 minutes doesn't help — observed 9 consecutive
  failures over 30+ minutes on 2026-08-28. If one fails, retry once or twice at most in the
  moment, then space further retries out via `ScheduleWakeup` at 15-20+ minute intervals
  instead of hammering it; if it's still blocked after a couple of spaced-out retries, say
  so plainly and ask the user whether they'd rather act manually (they may be able to
  resolve/close from the GitHub UI immediately, unblocked by whatever's rate-limiting the
  API token).

## A `gh pr list`/`gh issue list` sighting of something unrelated is not the same as attending to it

> Added 2026-09-12 after PR #386 (a docs-only recipe, unrelated to the #114 migration work
> the session was actually doing) sat with a stale, invalidated `security-engineer` PASS and 5
> new unresolved review threads for the rest of a long session — despite `gh pr list` surfacing
> it, correctly, at least twice along the way for unrelated reasons (an early PR triage, then
> again right before starting unrelated #141 work). It only got noticed while preparing a
> `/handoff` summary at the very end, because that command's checklist forces an explicit
> `gh pr list` re-read. Nothing was wrong with the *data* the session had access to — the PR was
> right there in the output both times — the gap was that seeing it in a listing run for a
> different purpose didn't trigger any action or even a recorded "deferring this, here's why."

When `gh pr list`/`gh issue list` (or their MCP equivalents) surfaces something outside the
current task's scope, don't let it pass silently just because the call was made for something
else. Either triage it right there if it's cheap, or write down explicitly *why* it's being
deferred (a short chat note is enough — it doesn't need a formal issue) so a later sighting of
the same item registers as "still true" rather than "new information nobody's looked at yet."
This is the same shape as the existing `feature-workspace-cycle.md` Step 1 rule ("needs a
dedicated look is not a resting state") but that one only fires during an actual PR-triage step —
this gap was a listing surfaced *incidentally*, mid-task, for an unrelated purpose.

## Search existing issues before filing one for a bug found while reviewing something else

> Added 2026-09-12 after filing #391 (a bug found by `architect` while scoping #141) only to
> discover, while writing `CLAUDE.md`'s session-log entry, that it was an exact duplicate of
> already-open #379 — filed 5 days earlier by a Step 1c integrity-audit firing on the very same
> file (`wasm.rs`/`response_chain.rs`). Cost: a wasted issue number, a close-as-duplicate, and
> corrections to three places that had already cross-referenced the wrong number (an issue
> comment, a PR body, a PR comment).

Before filing a "found this while doing something else" bug issue — especially in a file
`CLAUDE.md`'s integrity-audit log records as already audited — do a quick `gh issue list
--search "<distinctive symptom text>"` or check the audit log's own entries for that file first.
Cheap (one call), and the alternative cost (a duplicate discovered only later, requiring cleanup
across every place that cited it) is exactly what happened here.

## Dependabot & branch hygiene reflex check

Any session that calls a GitHub tool against this repo for *any* reason — not just a
`/feature-workspace-cycle` firing — should, as a cheap side effect, check whether
`.claude/logs/dependabot-hygiene.md`'s newest row is older than ~24h (or the log is still
empty) and, if so, run **`/dependabot-hygiene`** (`.claude/commands/dependabot-hygiene.md`
— moved out of this file 2026-08-28, see "New `.claude/` process content" above). Added
2026-08-01 after PR #101 sat "held for dedicated review" for ~5 weeks and PR #151 went
untriaged for a while, both because nothing but the daily cycle firing repeated this check.

## Fast-follow issue reflex check

Before picking the *next* batch of work, run **`/fast-follow-check`**
(`.claude/commands/fast-follow-check.md`) — checks for open `fast-follow`-labeled issues
(deferred follow-ups spawned while reviewing some other PR, e.g. #357 from #356) so they
get surfaced ahead of the general backlog instead of aging silently. Tag such an issue
with the `fast-follow` label at creation time — see the command file for what counts and
what doesn't.

## Subagents available here

- `build-validator` (haiku, read-only) — fmt/clippy/test verdicts, called via `/build`.
- `duplication-scanner` (haiku, read-only) — finds candidate code duplication in given
  file(s)/directories (repeated blocks, near-identical struct/impl pairs, copy-pasted
  closures). Reports `file:line` locations only; the conductor judges whether/how to
  extract — safe extraction often depends on call-site intent (e.g. a documented perf
  property, a test's structural assumption) that a mechanical scan can't see. Cheap
  enough to call several times in parallel across unrelated files.
- `release-engineer` (sonnet) — release readiness, CI failure triage, merge-order planning,
  driving the tag → `release.yml` pipeline.
- `security-engineer` (sonnet) — auth/secrets/TLS/guard-chain review, scanner-finding triage
  (Dependabot/OSV/Trivy/Semgrep/CodeQL/SonarCloud). Can block Quality/Release on real risk.
- `business-analyst` (sonnet) — turns a vague request/issue into scope + acceptance criteria,
  and checks it against `CLAUDE.md`'s architectural decisions / existing backlog *before* work
  starts (catches duplicates, conflicts with "не пересматривать без обсуждения" rules, and
  stale `[🚫 BLOCKED]` items whose reasons may no longer hold).
- `scrum-master` (sonnet) — manages conduit's backlog *as it actually exists*: `CLAUDE.md`
  checkboxes + dated "Реализовано в сессии" log + GitHub Issues (no separate `.claude/backlog/`
  here — don't invent one). Marks things done, decomposes large asks, tracks multi-PR efforts
  to completion.
- `lawyer` (haiku) — license-compatibility check when a `Cargo.toml` change adds a dependency
  or a new optional feature's crate tree (conduit is Apache-2.0; ships as binary + npm + Docker).
  Can block an incompatible/copyleft dependency.
- `architect` (opus, advisory/read-only) — called when a file crosses the 400/1000-line limits
  (`conventions.md` "Code quality") or for bigger design-decomposition questions. Hands back a
  concrete split/PR-decomposition plan; never edits files itself — the conductor implements it.

### Added for the Conduit 2.0 workspace migration (#114) — `/feature-workspace-cycle`

These exist for the feature-driven Cargo workspace migration cycle, but aren't limited to
it — call them whenever the same shape of task comes up outside that cycle too.

- `dependency-steward` (haiku, read-only) — triages open Dependabot PRs in a batch: semver
  risk, grouping related bumps, CI status, merge/hold recommendation.
- `feature-matrix-runner` (haiku, read-only) — proves Cargo feature gating is actually
  correct (`cargo hack --each-feature --no-dev-deps`, optional powerset), distinct from
  `build-validator`'s single-profile check.
- `footprint-auditor` (haiku, read-only) — measures stripped binary size / dependency count
  per feature profile and diffs against a base ref; the metric the workspace split exists
  to move.
- `integrity-auditor` (sonnet, read-only) — spot-checks that an *already-shipped* feature
  still works as documented: implementation vs. its own tests vs. docs/schema, reporting
  gaps (never fixing them itself). Distinct from self-review (which only covers new diffs).
- `prior-art-researcher` (sonnet) — "how do other proxies/gateways solve this" research
  against the reference projects named in `CLAUDE.md`'s own backlog (h2o, Angie, Envoy,
  HAProxy, traefik, linkerd2-proxy, etc.), with concrete file/line pointers and an explicit
  adapt/don't-adapt call.
- `docs-scribe` (sonnet) — keeps README/`docs/*.md`/`CHANGELOG.md`/
  `schema/conduit.schema.json` in sync with a merged diff.
- `crate-extractor` (sonnet) — **temporary**, retire after #114 Phase 6 — executes one
  mechanical crate-extraction end-to-end from the recipe in `CONTRIBUTING.md`/the pilot PR.

> When in doubt about whether to spawn one of these for a small ask — don't. The conductor
> handles trivial scoping/backlog bookkeeping/license-glance itself; reserve these for when
> the question genuinely needs that role's framing (see "Economy" above).

## Skills available here

- **`testing`** (`.claude/skills/testing/SKILL.md`) — conduit's actual test idioms (port 0,
  `rcgen`, `serial_test`, raw-`TcpListener` mock upstreams) and where to find canonical
  examples to pattern-match against. Read before writing new tests rather than reinventing
  a mocking approach.
- **`release`** (`.claude/skills/release/SKILL.md`) — the concrete tag → `release.yml` →
  verify-artifacts runbook (version lockstep, Docker manifest checks, transient-failure
  triage). `release-engineer` drives a release from this; the conductor can also follow it
  directly for a quick one.
- **`dependabot-hygiene`** (`.claude/commands/dependabot-hygiene.md`) — the reflex check
  described above; run it whenever `.claude/logs/dependabot-hygiene.md` is stale.
- **`fast-follow-check`** (`.claude/commands/fast-follow-check.md`) — the reflex check
  described above; run it before picking the next batch of work.
- **`coderabbit-reply`** (`.claude/skills/coderabbit-reply/SKILL.md`) — reply-then-resolve
  mechanics for CodeRabbit/reviewer threads on a PR, written against `mcp__github__*` tools
  (the cloud-firing case — a local session can do the same thing with `gh api`/`gh pr comment`,
  see "GitHub access differs by execution context" above). Referenced from `conventions.md`'s
  PR checklist.
- **`cleanup`** (`.claude/commands/cleanup.md`) — sweeps for leftover worktrees/WSL-Docker
  verification state, and for code a change should have removed or wired in but didn't. Run
  it after any task that created throwaway state; see "Clean up ephemeral debris" above.

> **Note (originally added 2026-08-29, corrected 2026-09-12):** this note used to warn that
> the `claude/cargo-workspace-features-23qxfr` migration branch had further `.claude/` tooling
> (`dependabot-hygiene`, `.claude/logs/*.md`) not yet merged to wherever this copy of the file
> lived. **If you're reading this copy of `index.md` while checked out on the migration branch
> itself, that warning doesn't apply to you — it's already all here** (confirmed 2026-09-12:
> `.claude/commands/dependabot-hygiene.md` and `.claude/logs/*.md` both exist on this branch).
> The warning is still meaningful for a session working from `main` or another branch that
> hasn't merged this tooling in — check the *specific* branch you're actually on, per "Different
> branches of this repo can have genuinely different `.claude/` tooling" above, rather than
> trusting this note's date or assuming it still describes a gap that exists on every branch.

> `.claude/` and `CLAUDE.md` are tracked in git for this repo (not gitignored — they ship
> with the source tree so cloud/remote sessions get the same tooling as local ones) but are
> excluded from the *published crate* via `Cargo.toml` `[package] exclude` — they never end
> up in the `cargo publish` source package or release artifacts.
>
> **Append-only logs live in `.claude/logs/*.md`, not inline in `CLAUDE.md`** (split out
> 2026-08-28 — same rationale as "New `.claude/` process content" above: `CLAUDE.md` loads
> into every session's context in full, every turn, and these logs only ever grow. `CLAUDE.md`
> keeps just the newest row or two of each plus a pointer; read the full file when you
> actually need history older than that, e.g. to count firings since the last rotation.
