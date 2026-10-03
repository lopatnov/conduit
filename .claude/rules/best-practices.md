# Development best practices (repo-wide, not migration-specific)

> Extracted 2026-10-03 at the owner's explicit request from
> `.claude/commands/feature-workspace-cycle.md`, which had accumulated a genuinely useful
> set of habits (catch errors early, secure-by-default, batch work sensibly, group issues
> into coherent branches, write a lesson down the moment it's learned) while scoped
> narrowly to the Conduit 2.0 migration (#114). #114 shipped and merged into `main`
> 2026-10-03 (PR #152) — these practices outlive that one epic and apply to any work in
> this repo, migration or not. `feature-workspace-cycle.md` itself stays as a concrete,
> battle-tested *template* for running a similar large multi-PR effort again (its Step
> numbers are still a good skeleton), but the general rules below are the canonical source
> now — don't re-derive or fork them per-epic.

## 1. Catch errors early — audit shipped code, not just new diffs

A normal review (self-review, `security-engineer`, CI) only ever looks at the *new* diff.
That leaves a blind spot: code that shipped working, was never touched again, and quietly
drifted from its own docs/tests, or was wrong from the start and nobody re-checked. Over
the #114 migration this blind spot produced real, valuable findings purely from periodic
re-checks of already-shipped code: #164, #163, #216–#220, #157, #158, #185, #189–#191,
#232, #247, #248, and more — see `.claude/logs/integrity-audit.md` for the full list.

- **Periodically re-verify a feature/module that hasn't been looked at in a while** —
  implementation vs. its own tests vs. docs/schema claims — not only when touching it for
  a new task. Call `integrity-auditor` for this; it's read-only and reports gaps, it
  doesn't fix them. Pick a target by checking `.claude/logs/integrity-audit.md`'s own
  entries first (oldest audited wins), or `CLAUDE.md`'s oldest "Реализовано" backlog entry.
- **Route findings by risk and ambiguity, not by label.** Low-risk and unambiguous (a
  missing doc line, an absent test for an existing, uncontroversial code path) → fix it
  directly, small PR. A real behavioral bug, or anything needing design judgment → file a
  GitHub issue with specifics rather than stealth-fixing it inline; it can still be picked
  up as the next piece of work (see §3 below), it's just not silently folded into whatever
  else is in flight.
- **A performance anomaly is a bug the moment it's measured** — a benchmark that contradicts
  the docs, a surprisingly low throughput number, a regression. File the issue in the same
  turn with the numbers and their source, even if full root-causing has to wait for a free
  machine. A chat remark or a "deal with it later" note is lost on the next compaction;
  waiting to write it up loses exactly the context that makes the number meaningful.
- **Bugs found in code a task is already touching ride along in the same PR**, not a
  separate deferred issue. Before starting a task, search for open bugs in the
  files/functions it moves or changes; tell the reader up front which you'll fix in the
  same PR, then land each as its own clearly-marked "behaviour change" commit (with
  updated tests) *before* the mandatory security pass, so one review covers it. A bug
  found living inside code about to be moved and filed as a separate issue instead of
  fixed in the same PR is the thing to avoid (it happened once — #447 inside #316 — and
  cost an extra PR + review cycle for no reason).

## 2. Secure coding is unconditional, not a judgment call

Already a hard rule, not new here — see `workflow.md`'s "Security review is unconditional"
and the priority ordering **1. Security, 2. Performance, 3. Usability** (owner, 2026-09-27).
Restated because it's the backbone the other practices here build on:

- `security-engineer` sign-off is required before *every* merge, full stop — no PR is
  "too small" or "too obviously safe" to skip it, because that judgment call is exactly
  what a manipulated PR/comment/commit message would try to exploit.
- A performance optimization never weakens a safety check by default — the fast/unsafe
  path is opt-in and must declare itself; the slow/safe path is what happens if a flag is
  forgotten (`ResponseFilter::may_block`, #475, is the concrete precedent: a filter is
  assumed able to block unless it explicitly says otherwise).
- A PR that changes a hot path states, in its own description, what it does to security
  and availability — not just throughput.

## 3. Batch processing of tasks — size the batch to the risk, not to convenience

One issue is one PR is one security-review pass (owner's rule, 2026-09-26, see
`workflow.md` "Proportionate process") — splitting one issue into several PRs for
process's own sake multiplies CI time, review rounds, and merge-conflict surface for no
real gain; bundle genuinely independent slices of *different* issues instead, when they
share a theme. Pick a tier before picking which items go in a batch:

- **1 — always solo**: an open design question, a security-sensitive surface, something
  needing `architect`/`business-analyst` judgment, or anything not confident enough to
  trust without a dedicated look. When in doubt, default to solo — unwinding a batched
  item that turns out to need real judgment costs more than reviewing one extra PR up
  front would have.
- **2 — a related pair**: only when the two share a root cause or the same code
  path/function, so reviewing either alone wouldn't make sense.
- **3–5 — small independent leaves, one theme**: each needs real code + a test, no design
  ambiguity, no overlapping code paths between items. Group by crate/theme so the PR still
  reads as one coherent change.
- **5–10 — mechanical/trivial sweep**: every item is a near-one-liner, verifiable by
  reading the diff alone, fully independent, and — no exception — none touch a
  security-sensitive surface (auth/secrets/TLS/rate-limit/CORS/guard-chain/IP-filter).
  Keep the total diff small enough that review stays a real check, not a rubber stamp.
- **A milestone-sized task gets a design pass and a plan posted on its issue before any
  code** — not one big uninterrupted attempt. Slices of that one task are commits on one
  branch, not separate PRs (see §4). Spot-check the plan's headline numbers against the
  actual code before executing it — a plan is a snapshot, and sizes/counts go stale fast
  as the codebase moves; the direction usually survives, the exact numbers often don't.

## 4. Group related work into one coherent branch — don't split for its own sake

- **One branch = one coherent change.** Don't let an unrelated fix piggyback on a branch
  already open as a PR (`conventions.md`) — but conversely, don't fragment one coherent
  piece of work into many PRs just because it has multiple steps. A milestone task's
  slices are separate, individually-verifiable **commits** on one branch, each compiling
  and each with its own verification evidence; the PR opens once, on the final head, and
  review goes commit-by-commit.
- **Proof scales with risk, not with slice count.** A pure mechanical move (cut by line
  range, behavior pinned by golden tests and a `-- --list` identity check) needs one
  verifier pass and a small set of mutation controls — not a generator-and-verifier pair
  per slice. New logic (a gate switch, a new check, a new guard) needs the full set:
  verifier, negative and polarity controls, pin tests.
- **Run the full local verification chain once, on the final head** (leak check,
  dependency sets, `-- --list` identity, the clippy matrix, tests, goldens across feature
  sets, `cargo hack`) — not once per commit. `scripts/verify-local.sh` is this chain for
  conduit; use it rather than re-inventing a bespoke check sequence per task.
- **Budget discipline**: if a task reaches a second security-review round, or isn't merged
  after roughly 2 hours of work, stop and ask rather than continuing by the rulebook. A
  question to the person deciding should be short, self-contained, carry the needed
  context, and lead with a recommendation — no internal jargon/labels unless spelled out
  in the same sentence.

## 5. Self-improvement — write the lesson down the moment it's found, while context still understands it

The single biggest compounding habit in this repo's own history: `.claude/rules/index.md`
is almost entirely made of entries written *immediately* after a near-miss or mistake was
caught, each with a concrete trigger, what went wrong, and the corrected rule — not written
up later from memory after the detail had faded. That immediacy is the point: a mistake
understood right now, in full context, produces a precise rule; the same mistake
recollected a week later produces a vague one, or doesn't get written down at all.

- **The moment something surprising, wrong, or a genuine near-miss is found — during
  *any* task, not just a dedicated retro — write it down before moving on.** A one-off bug
  in the task at hand doesn't need this (that's just the bug fix); this is for *process*
  lessons: a tool behaved unexpectedly, an assumption about the environment was wrong, a
  convention got violated by accident and was caught just in time, a subagent hit a gap.
- **Where it goes**: a standing operational rule that should bind every future session →
  `.claude/rules/index.md` (or `workflow.md`/`conventions.md` if it fits their specific
  scope) as a new dated section, following the existing entries' shape (a short "why this
  was added" quote block, then the concrete rule). A one-off procedure that only runs
  occasionally → a new or updated file under `.claude/commands/` or `.claude/skills/`, not
  inline in a `rules/*.md` file that loads every turn (see index.md's own "New `.claude/`
  process content" rule). Something true of *this specific project's state* right now
  (a deadline, who's doing what, a decision made this week) → memory (team or private, per
  the memory-type rules already in force), not `.claude/rules/`.
- **Don't wait for a `/retro` to capture a lesson that's already fully understood in the
  moment** — `/retro` exists for sweeping up what wasn't caught inline, not as the only
  place lessons are allowed to land. Capturing it immediately also means the exact
  triggering detail (the command that hung, the file that raced, the exact error text) is
  still in context to quote — it usually isn't, a day later.
- **A rule that turns out to be wrong or stale gets corrected or removed, not left to
  rot.** Several entries in `index.md` are themselves corrections of an earlier, now-wrong
  note (e.g. the OCSP-stapling/TLS-cipher blockers that were re-verified against a newer
  Pingora source and found still blocked for a *different* reason than originally
  recorded). Treat a standing rule as a living claim about current reality, not a
  permanent fact — re-check it when new evidence contradicts it, and say so when updating.

## On the RAG / local-AI / web-search gap

Flagged by the owner (2026-10-03) as a real limitation worth tracking, not yet solved:
this workflow does no live web search, doesn't learn from the project's own accumulated
history beyond what fits in `CLAUDE.md`/`.claude/logs/*.md`/memory, and doesn't use any
local/free model running on the owner's own hardware. That's a separate, larger piece of
work (a RAG index over the repo's issues/logs, a local model for cheap/offline triage) —
tracked as a backlog item for a dedicated workflow-improvements effort, not solved by this
document. Don't claim this gap is closed until that effort actually ships something.
