---
description: (Re)measure build size and/or throughput for a feature set via the benchmark-runner subagent and update docs/benchmarks.md — without dumping long cargo/cross/wrk output into the main context.
argument-hint: "[feature-set: default|standard|full] [what: size|throughput|both]  e.g. 'standard size'"
---

# /benchmark — update docs/benchmarks.md

Re-run conduit's documented benchmark methodology for a feature set and refresh the
numbers in `docs/benchmarks.md`. Delegates the long, noisy build/load runs to a cheap
subagent so the conductor's context stays clean.

## What to do
1. Call the **`benchmark-runner`** subagent (`.claude/agents/benchmark-runner.md`, haiku).
   Pass the scope from `$ARGUMENTS`:
   - **feature-set** — `default`, `standard`, or `full`. If omitted, default to `standard`
     (the row most often stale — see the `CLAUDE.md` backlog).
   - **what** — `size` (build sizes), `throughput` (wrk latency/throughput), or `both`.
     If omitted, default to `size` (it's the cheaper, more often-runnable half).
2. The agent measures and edits **only** `docs/benchmarks.md` (the matching table cells),
   following the methodology already documented there — it never changes the methodology,
   version strings, or `src/` code.

## Environment caveats (the agent handles these; know them so the report makes sense)
- **Linux musl size** needs `cross` + a running Docker daemon. `cross` is flaky on the
  Windows dev host (toolchain-install failures) — if it can't build locally, the accurate
  musl size comes from a `release.yml` artifact (`conduit-x86_64-unknown-linux-musl`)
  instead. The agent reports which path it used; never fabricate a number.
- **Windows MSVC size** builds natively (`cargo build --release --features <set>`).
- **wrk throughput** is Linux/macOS-only and needs conduit running on Linux for comparable
  numbers — it cannot run on the Windows host. The agent reports it as NOT RUN there.

## What to return
The agent's compact handoff (`BENCHMARK / ENVIRONMENT / RESULTS / DOCS / NOTES`). Then the
**conductor** handles git — the agent does not commit. If `docs/benchmarks.md` changed,
stage it and open a `docs:`-scoped commit/PR (note: `docs/benchmarks.md` also carries
version strings — leave those untouched unless this is a coordinated version bump).

> Estimated values in the Build Sizes table are marked with a `¹` footnote. Replacing one
> with a real measurement should also drop its `¹` marker.
