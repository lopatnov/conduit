# Working rules for this session

Compact since the 2026-10-03 instruction diet (#512): only what must be ambient. The dated incident stories behind each rule
are in `.claude/archive/rules-index-2026-10-03.md`. Companions: `conventions.md` (commits, versions, branches, PR checklist),
`workflow.md` (priorities, who to call, security gate, proportionate process).

## What belongs in `rules/` (and what does not)
`rules/*.md` loads into **every** turn. Keep here only what must be impossible to forget (the security gate is the model). A
procedure that runs occasionally is a `.claude/commands/<name>.md` or `.claude/skills/<name>/SKILL.md`, with a one-line pointer
here at most. Append-only logs live in `.claude/logs/*.md`; `CLAUDE.md` keeps two journal summaries. `.claude/` and `CLAUDE.md`
are tracked but excluded from the published crate (`[package] exclude`).

## Worktrees and background agents
- Commit edits to `.claude/{agents,commands,skills,rules}` before a worktree is removed (`CLAUDE.md` and user memory live outside
  worktrees and are safe).
- An agent that writes several files, creates commits, **or runs `git checkout`/`git pull`** — including "read-only" validators
  such as `build-validator` — gets `isolation: "worktree"`. Without it: `git status` first and commit pending work.
- After resuming an agent with `SendMessage`, run `git worktree list`; no entry for it means it is working in the shared checkout
  (a race with your own git work), whatever isolation it was spawned with.
- `git checkout -b <task-branch>` comes **before** the first `Edit`/`Write` on production source. The migration branch and `main`
  are fine to sit on for docs/log commits, reading and planning — not for a new task's code.
- After a task that created throwaway state (worktree, WSL clone, Docker image): run `/cleanup`.

## Subagent tool gaps, credentials, interactive auth
- **No subagent has `gh` or GitHub MCP tools; only the conductor does.** On a real tool gap an agent stops and reports what is
  missing and what it tried, with any drafted content (issue bodies, comments) ready to execute. It must not enumerate env vars,
  `~/.netrc`, `~/.git-credentials` or `git remote -v` for credentials, call external hosts with a found token, or install system
  packages to route around a gap. The conductor follows the same rule.
- Anything that plausibly needs a password or other interactive auth in a non-TTY shell (`sudo` inside `wsl -e bash -lc`,
  interactive `apt`/`brew`) is **not attempted** — ask the owner to run it; a silent hang costs a full timeout.

## GitHub access and its known walls
- Local session: `gh` is installed and authenticated — use it. Cloud/Routine session: only `mcp__github__*`. Check
  (`gh --version`, `ToolSearch select:mcp__github__get_me`) instead of trusting a note written for the other context.
- `git push` failing with `could not read Username` after retries: switch to `mcp__github__push_files` (full file content per
  file; it makes a new commit on the remote tip, so afterwards `git fetch` + `git reset --hard origin/<branch>` after checking
  nothing local-only is lost; leave a generated `Cargo.lock` unsynced if CI does not use `--locked`). Never hunt for a token.
- Walls: `sonarcloud.io` and the GitHub Security tab / code-scanning API are unreachable via `WebFetch`; the
  `mcp__sonarqube__*` connector **does** work (check it first); no tool can dismiss a CodeQL alert (the owner does it in the UI);
  for the full code-scanning list ask the owner to paste it. `mcp__github__get_check_run` has no per-alert detail.
- A local session never holds a `RemoteTrigger` (different id space) — not a bug. Session rotation is retired (automatic
  compaction bounds the context); don't recreate `session-rotate`. `/handoff` is for a manual restart.
- GraphQL-backed GitHub calls (`get_review_comments`, `resolve_review_thread`) have their own rate limit: retry once or twice,
  then space retries ≥15 min (`ScheduleWakeup`), then ask the owner.

## Branches and other tools' files
- Branches can carry different `.claude/` tooling. Before saying "this command/skill/log exists" or copying `.claude/` content
  between branches, check the **target** branch (`git show origin/<branch>:<path>`).
- Untracked or unfamiliar files may belong to another AI tool the owner runs: confirm they are not yours and leave them.
  `AGENTS.md` is a short **pointer** to `CLAUDE.md` and `.claude/` (owner, 2026-10-03: one source of truth, compatible with other
  tools, fewer files); never copy instructions into it or generate tool-specific mirrors — the old 133 KB copy, `.agents/` and
  `.codex/agents/` went stale and misled tools (#512). `.codex/hooks.json` (cargo fmt after an edit) stays.

## `.reference/<name>` — persistent source cache
Clone dependency and reference sources there on demand (gitignored), at the tag pinned in `Cargo.lock`
(`git clone --depth 1 --branch <tag>`); check `ls .reference/` first; update a stale clone in place; **`/cleanup` never removes
it**. Table of what is where: `.claude/archive/reference-sources.md`.

## Build discipline and economy
- `/build` after any non-trivial change and before a PR (zero warnings, `-D warnings`); `build-validator` keeps cargo output
  out of the context.
- Never patch source with a script that goes through a Bash heredoc if the text contains backslashes or quotes — the heredoc
  silently rewrites `\\` (it happened six times). Use `Edit`/`Write`, then look at the result.
- Push a ready branch at once — do not idle to satisfy an hourly interval (owner, 2026-09-26) — but do not push WIP
  repeatedly: every push is a ~25-minute CI run.
- No agents for trivial edits (a cold start re-derives context); finish what is started before chasing a new idea.

## PR review and CI triage
- A failing check: find the first failing commit in the run history. Same commit green on another run, or a network blip
  (`curl failed`, `SSL_read: unexpected eof`, `download of <crate> failed`): re-run (`gh run rerun <id> --failed`); escalate only
  if it fails consistently.
- A `check_run`/comment event can be for a superseded commit: compare its `head_sha` with the PR's current head first.
- Bots (Gitar, CodeRabbit) re-post identical findings on every push. Once a finding has a recorded disposition, skip identical
  re-postings.
- **Gitar's auto-apply (on for the tracking PR #152) commits to the migration branch unreviewed** and once broke it. `git fetch`
  and read `git log <last known tip>..origin/<branch>` before work and before every push; treat such a commit as unreviewed
  (security gate if it touches auth/TLS/validation) and revert it if it breaks the build. Turning auto-apply off is the owner's call.
- `SonarCloud analysis` fails on Dependabot-authored PRs (Dependabot `pull_request` runs get no secrets) — expected, not a
  regression, and not required by branch protection.
- A listing that surfaces something outside the current task (`gh pr list`) is not attended to just by being seen: triage it, or say
  in one line why it is deferred.
- **Before creating any issue** (bug or not), search existing issues — as its own call — read the result, then create. Never
  `search; create` in one command (#515 was filed as a duplicate of #258 that way). For a bug in a file the audit log lists, check
  `.claude/logs/integrity-audit.md` too.
- Reflex checks: `/dependabot-hygiene` when its log is >24 h old; `/fast-follow-check` before picking the next batch of work.

## Agents and skills
The harness lists every agent and skill with its description; the "who to call when" table is in `workflow.md`. Facts the
descriptions do not carry: `crate-steward` is the successor of the Phase-6 `crate-extractor` (new feature crates by the
`new-feature-crate` skill, #259; standalone promotion, #258); `prior-art-researcher` is the agent for the "Research before building"
step; `coderabbit-reply` documents the reply-then-resolve mechanics (MCP tools in the cloud, `gh api` locally);
`verify-handed-over-task` (`.claude/commands/verify-handed-over-task.md`) runs before the first edit of a task handed over from
another session (a `spawn_task` chip, a `/handoff` summary) and when resuming after a usage-limit reset — is it already done
(e.g. the `builder.rs` split merged as #495 before the session started), and does the worktree's base branch contain the file.
`/issue` (`.claude/commands/issue.md`) is the default command for ordinary work — one or more issues to one merged PR; it leans on
the `best-practices` skill (`.claude/skills/best-practices/SKILL.md`: re-audit shipped code, size a batch to its risk, one coherent branch, write a lesson down when found).
`feature-workspace-cycle` stays only as a template for another large multi-PR effort.
