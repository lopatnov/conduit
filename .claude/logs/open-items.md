# Ledger of noticed items (issues filed so that they are not forgotten)

Rule: `.claude/rules/workflow.md`, "The ledger". One line per issue: what, where it was found, status. At the end of a task each
open line is fixed or put to the owner as "do we do #N?"; the line is deleted (or marked done) when the issue is closed. The
list is also regenerable from GitHub: `gh issue list --label fast-follow`. **Before a merge and at the end of a task run
`python3 scripts/ledger-check.py --pr <N> --since <day the task began>`** (#685): it prints the open lines as the to-do list and
exits 1 when the ledger, the PR's "Found while here" table and the open `fast-follow` issues disagree.

| Issue | What | Found in | Status (2026-11-09) |
|---|---|---|---|
| #700 | upload: the 5 GiB request ceiling is hard-coded, undocumented and silently caps a larger per-site limit (decided with #696: the 32-bit overflow fix there is a patch on it) | owner's question during #696 | open: proposal on the issue, owner to confirm |
| #696 | 32-bit targets (i686 Linux/Windows, ARMv6/ARMv7) and the Android targets: probe build, then ship what builds (standard, gateway, static-server; `full` not required) | owner's question on rustc and targets, 2026-11-11 | owner said yes: start right after 2.3.0 |
| #697 | ledger-check hardening: Windows cwd lookup of `gh`, no subprocess timeout, `RecursionError`, glued `PR#N` | security review of #686 | open (was missing from #686's table; added there afterwards) |
| #677 | flaky server start in the integration tests | full test run for #678 | harness improved in PR #683 (stderr kept, one retry); open until the cause is named |
| #676 | serde_yaml is unmaintained (RUSTSEC-2024-0449) | OSV | PR #678 closed by the owner, waits for Pingora (cloudflare/pingora#1117); reopen then |
| #673 | RISC-V release build is not run under QEMU in CI | 2.2.1 | open |
| #671 | Windows ARM64 release | 2.2.0 | open, waits for a green windows-11-arm job |
| #670 | systemd unit: more hardening directives | .deb | open |
| #663 | Rhai guide test reads fences by the CommonMark rule | #662 | open |
| #661 | wasm/rhai: one file reader, compile once per reload | #566 | open |
| #659 | build the C, Go and Zig samples of the WASM guide in CI | #565 | open |
| #658 | flaky dns::resolve_socket_addr_hostname_respects_timeout | a docs PR | open |
| #684 | libyaml folds leading/trailing blanks on the first line of a block scalar or a continuation line of a multi-line quoted scalar: `interpolate_yaml` does not guard it | security review of #683 | open |
| #633 | split the production files over 400 code lines: `wasm.rs` (614), `filter.rs` (520), and since #683 `conduit-config-core/src/env.rs` (415, to go with #684) | code-length reports | open |
| #619 | Windows #600 follow-ups: placeholders, ReFS, symlink-converted dirs (needs those environments) | #600 | open, waits for the environments |
| #643 | groups: a dead group is not failed over; backup, retry and mirror not applied to a grouped route | balancer work (#631) | open (predates the ledger) |
| #644 | sticky: ignored on routes with groups without a warning; cookie has no Secure/Max-Age | balancer work (#631) | open (predates the ledger) |
| #645 | disk-cache is a marker feature: it gates no code | balancer work (#631) | open (predates the ledger) |
| #647 | perf: four upstreams give 10-20% less throughput than one, for every strategy | balancer work (#631) | open (predates the ledger) |
| #651 | half-open: the probe offer should be claimed by the pick that selects the upstream | balancer work (#631) | open (predates the ledger) |
| #653 | health checking fully configurable (`timeoutMs` done in #683; method, pass criteria, request-driven probing, per-upstream off, half-open timings remain) | balancer work (#631) | open (partly done) |
| #655 | SIGTERM: exit as soon as in-flight requests are done instead of sleeping the whole grace period | #489 | open (predates the ledger) |
