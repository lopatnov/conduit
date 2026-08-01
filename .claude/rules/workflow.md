# Workflow — when to call which subagent

> Lean version for a solo-maintainer project with 6 specialist subagents (not the full
> multi-role gate system from generic templates — that's disproportionate here: there's no
> separate UI/product/design track, and the conductor + user fill the BA/PM functions for
> almost everything).

## Who's the conductor

**The main Claude session is the conductor.** Subagents don't replace it — they're called for
a scoped task and **report back to the conductor**, never to each other. Deep agent→agent
chains are expensive and lose context — avoid them.

## Trigger table — event → who to call

| Event | Call |
|---|---|
| Request/issue is vague, or might duplicate/conflict with `CLAUDE.md` decisions or backlog | `business-analyst` |
| New idea surfaces mid-task; need to mark something done; multi-PR effort needs tracking | `scrum-master` |
| Need a compact fmt/clippy/test verdict without flooding context | `build-validator` (via `/build`) |
| Touches auth/secrets/TLS/guard-chain/rate-limit/CORS, or a scanner finding needs triage | `security-engineer` |
| New/changed Cargo dependency, especially behind a `--features` flag | `lawyer` |
| PR readiness, CI failure triage, merge-order across PRs, cutting a release (`v<x.y.z>` tag) | `release-engineer` |
| A file crosses the 400-line soft limit (or sits at/near the 1000-line hard limit), or a bigger architecture/design question needs a concrete decomposition plan | `architect` (opus, advisory only — see note below) |

## When NOT to call an agent (economy)

- **The conductor handles trivial things directly**: typo fixes, one-line changes, answering
  "how does X work" from `CLAUDE.md`/code, routine `gh` queries. Spawning an agent costs a
  cold start that re-derives context from scratch.
- Call a specialist only for (a) genuine domain expertise, (b) isolating noisy output
  (compiler dumps, long logs), or (c) a bounded autonomous sub-task.
- Don't chain agent→agent. Return to the conductor; it decides the next step.

## Example walk-throughs

**A. Trivial fix** (typo, rename, one-liner): conductor does it directly → `/build` → done.

**B. Bug fix**: reproduce/localize (conductor) → fix it → `/build` (`build-validator`) →
update `CLAUDE.md`/changelog if user-visible → `scrum-master` marks it done.

**C. New feature from a GitHub issue** (e.g. #65): `business-analyst` scopes it against
`CLAUDE.md` decisions/backlog (catches things like "the issue's proposed feature list doesn't
compile against current `Cargo.toml`") → conductor implements → `lawyer` if new deps appear →
`/build` → `security-engineer` if auth/secrets/TLS touched → docs updated → PR opened →
`release-engineer` for merge-order/CI triage → `scrum-master` checks it off.

**D. Cutting a release**: `release-engineer` audits open PRs + merge order → merges land →
version-string consistency check → tag `v<x.y.z>` → push → monitor `release.yml` → verify
artifacts (Docker images, GitHub Release, npm). See `.claude/skills/release/SKILL.md`.

## Note on the `architect` role

`architect` (opus, `.claude/agents/architect.md`) is advisory-only: it reads a file and hands
back a concrete module-split plan (or a PR-decomposition plan for bigger design questions) —
it never edits files. The conductor implements the plan and runs `/build`. Architectural
*decisions* (recorded in `CLAUDE.md` "Архитектурные решения") are still made by the user —
`architect` proposes structure within those decisions and flags conflicts with them rather
than overriding them.

## Definition of Done (general)

A task is done when: the change matches the agreed approach, `/build` is green, tests cover
the behavior, docs/changelog are current if user-facing, and `CLAUDE.md`/issues reflect the
new state (`scrum-master`).

## Session budget discipline

- Context is bounded (~200K). Split big tasks before starting (see `.claude/rules/index.md`).
- Delegate noisy/voluminous work to subagents (`build-validator` instead of raw compiler dumps).
- Finish what's started before chasing new ideas — park new ideas in the `CLAUDE.md` backlog.
- If you see a real risk of running out of budget mid-task: stop, record state clearly
  (for the next session), leave a recommendation — don't push through and lose context.
