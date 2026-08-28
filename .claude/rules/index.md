# Working rules for this session

> Lean adaptation of a multi-role team template for a solo-maintainer Rust project.
> A few operational disciplines from that template are worth keeping here, tailored to
> what conduit actually does (not generic placeholders).

## Related rule files

- **`conventions.md`** — commits (Conventional Commits + `Co-Authored-By`), SemVer/version
  lockstep, branch naming, push-frequency economy, zero-warnings/English-only bar, PR
  checklist, and the literal CodeRabbit reply/resolve pattern from PR #70.
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

## Known-blocked external endpoints — ask the user, don't keep retrying

> Added 2026-08-28 after a session burned ~6 tool calls across `WebFetch` and
> `get_check_run` rediscovering, one path at a time, that it has no way to see GitHub's
> Security tab or SonarCloud's dashboard — a wall already hit and documented (in prose, not
> as a checkable list) by multiple prior sessions.

These are confirmed **unreachable from every session so far**, not worth retrying or
probing a new URL variant of:
- `sonarcloud.io` (any path) — `WebFetch` returns `EGRESS_BLOCKED` outright, confirmed
  directly (not inferred from a 403).
- `github.com/<owner>/<repo>/security` and `/security/code-scanning` (with or without a
  `?query=` filter) via `WebFetch` — returns 404 (unauthenticated pages don't render the
  real alert list).
- `api.github.com/repos/<owner>/<repo>/code-scanning/alerts` via `WebFetch` — 403, even for
  a public repo (this endpoint needs an authenticated token, which `WebFetch` doesn't carry).
- `mcp__github__get_check_run`'s `output.text` — empty for CodeQL/SonarCloud check runs;
  only `output.summary` (a short pass/fail blurb) is populated, no per-alert detail.

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
hold, at most, a one-or-two-line pointer to the actual procedure (see how `session-rotate`
is referenced from the "Skills available here" list below) — never the procedure itself.

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
  `download of <crate> failed`) on crates.io/ghcr.io are common and transient — `gh run rerun
  <id> --failed` rather than treating them as code problems. Only escalate as a real bug if
  the same commit fails consistently across reruns.
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

## Dependabot & branch hygiene reflex check

> Added 2026-08-01 after PR #101 (kube 3→4.0.0) sat "held for dedicated review" for
> ~5 weeks and PR #151 (an all-actions Dependabot bump) went untriaged for a while — both
> because the only routine check was the daily `/feature-workspace-cycle` firing at 4AM
> (changed from hourly per the user's own request), and no other session touching this
> repo's GitHub state repeated the check in between. This closes that gap.

Any session that calls a GitHub tool against this repo for *any* reason — not just a
`/feature-workspace-cycle` firing — should, as a cheap side effect, check whether this
sweep has run in the last ~24h (see the log in `CLAUDE.md` "Dependabot & branch hygiene
log"). If the newest row is older than that (or the table is still empty):

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
- Log the result — even "nothing new" — as a new row in `CLAUDE.md`'s table, so the next
  session (or the next daily firing) can see it was already checked and skip.

This is a *cheap* reflex check (a couple of list calls), not a deep audit — skip it
outright if the log shows it ran within the last ~24h. A `/feature-workspace-cycle`
firing that completes its own Step 1 satisfies this for the day; it doesn't need to run
the check twice.

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
- **`session-rotate`** (`.claude/commands/session-rotate.md`) — hands a self-bind Routine
  session off to a fresh one once it's accumulated too much context (checked at
  `feature-workspace-cycle.md` Step 0a, roughly every 10-20 firings; also fine to invoke
  ad hoc if a session is visibly struggling before that). Logs each handoff in `CLAUDE.md`'s
  "Session rotation log".

> `.claude/` and `CLAUDE.md` are tracked in git for this repo (not gitignored — they ship
> with the source tree so cloud/remote sessions get the same tooling as local ones) but are
> excluded from the *published crate* via `Cargo.toml` `[package] exclude` — they never end
> up in the `cargo publish` source package or release artifacts.
