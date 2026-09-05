---
name: coderabbit-reply
description: Reply to and resolve CodeRabbit/reviewer inline comment threads on a conduit PR via the GitHub MCP tools, and handle "Outside diff range" comments that can't be replied to inline. Load this whenever the PR checklist's review-thread item comes up.
---

# CodeRabbit reply pattern — reply, then resolve

> Extracted 2026-08-28 from `.claude/rules/conventions.md` (see "New `.claude/` process
> content: command/skill by default, not `rules/`" in `rules/index.md`) — this is a
> mechanical recipe needed only while actively working PR review threads, not something
> every session's context should carry on every turn. Originally written for PR #70.
> **Corrected the same day**: the original recipe used `gh api`/`gh pr comment` — at the
> time this was written from a cloud/Routine-fired session, which has no `gh` CLI at all
> (only the GitHub MCP tools, `mcp__github__*`). The steps below use the MCP tools, which
> work in both contexts. **2026-08-30 addendum**: confirmed the split is context-dependent,
> not universal — a *local* session has `gh` CLI too (often more capable, cheaper in
> practice — see `.claude/rules/index.md` "GitHub access differs by execution context"), so
> the original `gh api`/`gh pr comment` recipe wasn't wrong, just scoped to the wrong
> context. Either approach is fine in a local session; the MCP steps below are the one that
> works everywhere, so they stay the default here.

## Reply to an inline review comment

```
mcp__github__add_reply_to_pull_request_comment(
  owner: "lopatnov", repo: "conduit", pullNumber: <PR>,
  commentId: <numeric id from the #discussion_r... anchor, NOT the PRRT_... thread node id>,
  body: "..."
)
```

## Resolve the thread

Resolving needs the thread's **GraphQL node ID** (`PRRT_...`), which is different from
the comment's numeric ID used above. Get it from:

```
mcp__github__pull_request_read(method: "get_review_comments", owner: "lopatnov",
  repo: "conduit", pullNumber: <PR>)
```

This returns review threads with `isResolved`/`isOutdated`/`isCollapsed` plus their
comments — find the thread containing the comment you just replied to, then:

```
mcp__github__resolve_review_thread(owner: "lopatnov", repo: "conduit", threadId: "<PRRT_...>")
```

(`mcp__github__unresolve_review_thread` is the inverse, same shape — for reopening one.)

## Local-session equivalent (`gh api`)

A local session (see `.claude/rules/index.md` "GitHub access differs by execution
context") can do the same two steps with `gh api` directly instead of the MCP tools:

```bash
# Reply (numeric comment id from #discussion_r..., same as above)
gh api repos/lopatnov/conduit/pulls/<PR>/comments \
  -f body="..." -F in_reply_to=<numeric comment id>

# Resolve (GraphQL node id, PRRT_..., from the query below)
gh api graphql -f query='
mutation {
  resolveReviewThread(input: {threadId: "<PRRT_...>"}) {
    thread { isResolved }
  }
}'

# Find thread node ids + isResolved state for a PR
gh api graphql -f query='
query {
  repository(owner: "lopatnov", name: "conduit") {
    pullRequest(number: <PR>) {
      reviewThreads(first: 20) {
        nodes { id isResolved comments(first: 1) { nodes { body } } }
      }
    }
  }
}'
```

## Resolving isn't optional here — it can be a hard merge gate

> Found the hard way on PR #361 (2026-09-05): `gh pr merge` failed with "the base branch
> policy prohibits the merge" even though every CI check was green and `gh api repos/
> .../branches/main/protection` returned 404 ("not protected"). The actual gate is a
> **repository ruleset** — a separate, newer GitHub mechanism from classic branch
> protection — checked via `gh api repos/lopatnov/conduit/rules/branches/main`, which can
> set `required_review_thread_resolution: true` independent of any required-approving-
> review-count setting (this repo's is 0). Replying to a thread is *not* the same as
> resolving it for this purpose — only the GraphQL `resolveReviewThread` mutation
> (or the MCP `mcp__github__resolve_review_thread` equivalent) clears the gate.

Before assuming a PR with all-green CI is actually mergeable, check
`gh pr view <PR> --json mergeable,mergeStateStatus` — `"mergeStateStatus":"BLOCKED"` with
`"mergeable":"MERGEABLE"` means something like this is the culprit, not a real merge
conflict. Resolve every thread you've addressed (see "Don't leave threads dangling"
below) before attempting the merge, not just reply to it — this applies whether the
finding was fixed, or declined with reasoning.

## "Outside diff range" comments

GitHub can't anchor an inline reply to these (a platform limitation, not a tool
limitation) — post a regular PR-level comment addressing the points instead:

```
mcp__github__add_issue_comment(owner: "lopatnov", repo: "conduit",
  issue_number: <PR number — PRs share the issue-comment endpoint>, body: "...")
```

## Don't leave threads dangling

Every CodeRabbit/Qodo/Gitar finding on a PR gets a disposition before merge: reply with
what changed (or why not) and resolve, or explicitly say why it's not being acted on. See
`.claude/rules/conventions.md` "PR checklist" for where this fits in the merge gate, and
`.claude/rules/index.md` "PR review & CI triage" for the caveat that a bot re-posting an
*identical* finding on every push is expected noise, not a sign the earlier disposition
didn't take — skip re-replying to a verbatim repeat once it already has one.
