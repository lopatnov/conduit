---
description: (Re)measure build size and/or throughput for a feature set via the benchmark-runner subagent and report the numbers — without dumping long cargo/cross/wrk output into the main context.
argument-hint: "[feature-set: default|standard|full] [what: size|throughput|both]  e.g. 'standard size'"
---

# /benchmark — measure and report

Re-run conduit's documented benchmark methodology for a feature set and report the numbers.
Delegates the long, noisy build/load runs to a cheap subagent so the conductor's context stays
clean. `docs/benchmarks.md` carries no figures (owner, 2026-10-10: historical data removed), so
the results go to an issue or PR comment, never into that page.

## What to do
1. Call the **`benchmark-runner`** subagent (`.claude/agents/benchmark-runner.md`, haiku).
   Pass the scope from `$ARGUMENTS`:
   - **feature-set** — `default`, `standard`, or `full`. If omitted, default to `standard`
     (the one measured least recently).
   - **what** — `size` (build sizes), `throughput` (wrk latency/throughput), or `both`.
     If omitted, default to `size` (it's the cheaper, more often-runnable half).
2. The agent measures and edits no files; it returns the numbers with the environment they were
   measured in. It never changes the methodology, version strings, or `src/` code.

## Environment caveats (the agent handles these; know them so the report makes sense)
- **Linux musl size** needs `cross` + a running Docker daemon. `cross` is flaky on the
  Windows dev host (toolchain-install failures) — if it can't build locally, the accurate
  musl size comes from a `release.yml` artifact (`conduit-x86_64-unknown-linux-musl`)
  instead. The agent reports which path it used; never fabricate a number.
- **Windows MSVC size** builds natively (`cargo build --release --features <set>`).
- **wrk throughput** is Linux/macOS-only and needs conduit running on Linux for comparable
  numbers — it cannot run on the Windows host. The agent reports it as NOT RUN there.

## What to return
The agent's compact handoff (`BENCHMARK / ENVIRONMENT / RESULTS / NOTES`). Then the **conductor**
posts it as a comment on the issue at hand (#487 for general re-measurement), adding the Conduit
commit, `global.workers`, the load generator and the exact command next to each number. Nothing is
committed unless the *method* described in `docs/benchmarks.md` changed.
