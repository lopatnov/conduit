# Ledger of noticed items (issues filed so that they are not forgotten)

Rule: `.claude/rules/workflow.md`, "The ledger". One line per issue: what, where it was found, status. At the end of a task each
open line is fixed or put to the owner as "do we do #N?"; the line is deleted (or marked done) when the issue is closed. The
list is also regenerable from GitHub: `gh issue list --label fast-follow`.

| Issue | What | Found in | Status (2026-11-09) |
|---|---|---|---|
| #652 | healthCheck.timeoutMs | docs audit (#568) | done in PR #683 |
| #660 | wasm `phase` has no effect: warn | WASM guide (#565) | done in PR #683 |
| #681 | mistyped keys under `global` | checking #582 | done in PR #683 |
| #682 | mistyped keys in nested objects | #681 | done in PR #683 |
| #679 | `conduit fmt --write` writes resolved secrets | security review of #678 | done in PR #683 |
| #680 | YAML interpolation escapes values as JSON | CodeRabbit on #678 | done in PR #683 |
| #586 | diagnostics re-interpolate | security review (batch of #570) | done in PR #683 |
| #587 | no guard for the CRD deny-list | security review (#580) | done in PR #683 |
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
