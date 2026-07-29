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

## Worktree persistence (don't strand `.claude/` tooling)

> A harness worktree (`C:\projects\conduit\.claude\worktrees\<name>`) has its **own
> physical copy** of `.claude/` — a real directory, not a junction. Since `.claude/`
> is gitignored, git does **not** sync it between the worktree and the main checkout.

- When creating or editing **persist-able `.claude/` tooling** — agents
  (`.claude/agents/`), commands (`.claude/commands/`), skills (`.claude/skills/`),
  or rule files (`.claude/rules/`) — write to (or copy into) the **main checkout**
  `C:\projects\conduit\.claude\...`, **not** only the worktree copy. Files left in
  the worktree copy are **lost when the worktree is cleaned up**.
- A `Stop` hook auto-mirrors the worktree's `.claude/{agents,commands,skills,rules}`
  back to main (additive, no deletes) as a safety net — but don't rely on it; write
  to main directly when you can.
- **`CLAUDE.md` is fine** — it is *not* copied into the worktree (lives only in the
  main repo root), so editing `C:\projects\conduit\CLAUDE.md` already persists.
  **User memory is fine** too (`C:\Users\Lopat\.claude\projects\...\memory\`).

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

## Subagents available here

- `build-validator` (haiku, read-only) — fmt/clippy/test verdicts, called via `/build`.
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

> All of `.claude/` and `CLAUDE.md` are gitignored for this repo (see `.gitignore`) —
> never `git add -f` them, never push them.
