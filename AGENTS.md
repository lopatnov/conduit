# Conduit — instructions for any AI coding tool

There is **one source of truth**; this file only points at it. Read these before changing anything:

1. [`CLAUDE.md`](CLAUDE.md) — the project reference: architectural decisions (do not revisit without discussion), the request
   pipeline, open/blocked backlog, rules. It is written in Russian; the product (code, comments, commits, `docs/`, CLI output)
   is **English only**.
2. [`.claude/rules/`](.claude/rules/) — `workflow.md` (priorities, who to call, the security gate), `conventions.md` (commits,
   versions, branches, PR checklist), `index.md` (environment rules).
3. Roles, procedures and playbooks are plain Markdown and work in any tool — open the one that matches the task:
   [`.claude/agents/`](.claude/agents/) (roles: architect, security-engineer, release-engineer, …),
   [`.claude/commands/`](.claude/commands/) (procedures: build, cleanup, dependabot-hygiene, …),
   [`.claude/skills/`](.claude/skills/) (playbooks: testing, release, new-feature-crate, …).

Non-negotiables, in case you read nothing else:
- Priorities: **security, performance, usability, code best practices, RFC compliance** — in that order.
- Never push to `main`; branch + PR. One PR = one feature; small tasks are bundled, one commit each (see `.claude/rules/workflow.md`). Every PR gets a security review of its final head.
- Zero warnings (`cargo clippy -- -D warnings`), `cargo fmt`, tests for new behaviour; new features follow
  `.claude/skills/new-feature-crate/SKILL.md`.
- Do not copy these instructions into other files or generate tool-specific mirrors (the old `AGENTS.md`, `.agents/` and
  `.codex/agents/` copies went stale and misled tools); if a tool needs its own format, add a thin file that links to the paths above.
