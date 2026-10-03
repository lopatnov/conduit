---
description: Before starting or resuming a handed-over task (spawn_task chip, /handoff summary, "continue" after a usage-limit reset), verify it is still needed and that you are on the right base branch.
argument-hint: "<file path, issue number or PR number the task is about>"
---

# /verify-handed-over-task — is this task still needed, and am I on the right tree?

> Added 2026-10-03 (from a `/retro`). A session was handed "split
> `crates/conduit-server/src/server/builder.rs` (431 lines)" — written right after PR #493
> merged into the migration branch. By the time the session got going (after two
> usage-limit interruptions) PR #495 had already done exactly that, merged 33 minutes after
> #493, with `builder.rs` down to 291 lines on origin. The session's worktree was also based
> on `main`, where `crates/` does not exist at all. Nothing was redone only because the
> session looked before editing; this command is that look, written down.

Run it before the **first edit** of a handed-over task, and again on **resuming** after any
gap long enough for the repo to move on (a usage-limit reset, a day). Arguments, if given:
`$ARGUMENTS`.

## 1. Fetch, then check the tree you actually have

```bash
git fetch origin
git cat-file -e HEAD:<path> && echo "present at HEAD"
MSYS_NO_PATHCONV=1 git cat-file -e origin/<target-branch>:<path> && echo "present on target"
```

`<target-branch>` is the branch the PR will go into — for #114 work that is
`claude/cargo-workspace-features-23qxfr`, not `main`. A worktree created for a session starts
on `main` unless told otherwise, and `main` has no `crates/`. If `HEAD` lacks the path but the
target has it, branch from the origin tip of the right branch (`git worktree add -b <task>
<short-path> origin/<target-branch>`) — don't plan against the wrong tree. A new branch made
that way tracks the target branch: push it with an explicit refspec
(`git push -u origin <task>:<task>`), never a bare `git push`.

## 2. Was it already done?

```bash
git log origin/<target-branch> --oneline -- <path>
gh pr list --repo lopatnov/conduit --state merged --search "<file or title words>"
gh pr view <n> --repo lopatnov/conduit --json baseRefName,headRefName,mergedAt,state
gh issue view <n> --repo lopatnov/conduit --json state,title
```

Match any branch name quoted in the task text to a PR (`headRefName`). Check that the
originating issue is still open and that no open PR against the target already covers it.

## 3. Is another session already on it?

`ListAgents` for peer sessions (an idle one may hold unpushed commits), and `git branch -vv`
— a leading `+` means the branch is checked out in another worktree. Report overlap to the
user; do not edit another session's branch.

## 4. Decide

- **Already done** → say so, give the PR link, and stop. Don't redo it to honour the
  assignment.
- **Partly done / overlapping** → report exactly what exists and what remains; confirm scope
  with the user before continuing.
- **Still needed** → continue, on a task branch cut from the target branch's origin tip.

## Git Bash on Windows pitfalls

- Any `<rev>:<path>` argument containing a branch name with slashes — not just `git show`,
  also `git cat-file -e`, `git log <rev> -- <path>`, anything of that shape — is mangled by
  MSYS path conversion (`ambiguous argument 'origin\claude\…;…'`). Prefix the command with
  `MSYS_NO_PATHCONV=1` (already applied to Step 1's `origin/<target-branch>:<path>` lookup
  above — don't drop it if you edit that command).
- Never pipe `git show` into `wc -l` unchecked: the error message counts as "1 line" (two
  failed lookups read as one-line files in the session that motivated this command).
