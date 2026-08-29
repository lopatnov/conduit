---
description: Sweep for leftover debris — worktrees, WSL/Docker verification state, and code left over from a change (dead code, or new code never wired in) — and clean it up.
argument-hint: "(optional: a specific area to focus on — 'worktrees', 'wsl', or a file/PR to check for dead code)"
---

# /cleanup — reclaim disk space and code debris left over from work in this repo

> Added 2026-08-29 after a `/retro` found ~4.1GB of never-cleaned WSL/Docker verification
> state and a stray merged-PR worktree that had sat untouched for about a week. Neither was
> wrong in the moment it was created — both are legitimate throwaway state for a one-time
> check — the gap was nobody going back to remove them once the check was done.

Run this after any task that created throwaway state (an isolated worktree, a WSL scratch
clone, a Docker image/container pulled for one verification run), or periodically as a sweep
alongside the Dependabot hygiene check. Two independent passes — run whichever applies.

## Pass 1 — physical debris

1. **Stray worktrees**: `git worktree list`. For each entry that isn't the main checkout,
   check its branch's PR state (`mcp__github__list_pull_requests` filtered by head branch, or
   `search_pull_requests`). If the PR is merged or closed, the worktree has no further
   purpose — `git worktree remove <path>` (add `--force` only if it reports uncommitted
   changes *and* you've confirmed those changes are already captured elsewhere, e.g. in the
   merged PR), then `git branch -D <branch>` for the now-redundant local ref. If a worktree's
   branch has an *open* PR or no PR at all, leave it — it may be in-progress work, not debris.
2. **WSL scratch clones** (see the `wsl-docker-linux-verification` project memory): a clone
   made under `~/verify` or similar for one-off CI-matching verification has no reason to
   persist once the check is done. Remove it the same session, not "next time someone
   notices."
   **Root-owned files from a Docker bind mount**: if the clone was built inside a
   `docker run` container (this repo's Linux-verification recipe does exactly this), the
   container's writes (target/, build artifacts) are owned by root inside the container — a
   plain `rm -rf` from the WSL user account fails with `Permission denied` on every such file.
   **Do not reach for `sudo`** (see "Commands needing a password/interactive auth" in
   `.claude/rules/index.md`) — Docker itself already runs as root, so a second throwaway
   container can delete what the first one wrote, with no host password needed:
   ```bash
   wsl -e bash -lc "docker run --rm -v <parent-dir>:/verify busybox sh -c 'rm -rf /verify/<subpath>'"
   ```
   Mount the *parent* of the directory you're deleting, not the directory itself — removing
   the bind-mount point itself fails with "Device or resource busy."
3. **Docker images/containers**: `docker ps -a` / `docker images` / `docker system df` (via
   `wsl -e bash -lc "..."`). Only touch what this session's own work created — this machine
   runs other unrelated projects' containers too (things like `mssql`/`qdrant` show up in
   `docker ps -a` and are not conduit's to clean up). A verification image like `rust:latest`
   is a legitimate reusable cache across sessions (removing it just means re-pulling ~600MB
   next time) — leave it unless the user says otherwise; "takes multiple GB" alone isn't a
   reason to remove a cache that's meant to be reused.
4. **Don't touch**: the main checkout's own `target/` directory. It's gitignored, expected,
   and expensive to rebuild — large is normal there, not debris.

## Pass 2 — code debris left over from a change

`cargo build`/`clippy -D warnings` (this repo's own zero-warnings bar) already catches unused
*private* items — but it does **not** catch two shapes of leftover code that have bitten this
repo before (see `CLAUDE.md`'s integrity-audit log, e.g. issue #218: `RequestCtx.
failed_upstream_attempts` sat as write-only state, undetected until a dedicated audit found it
well after the PR that introduced it had already merged):

- **Old code a refactor should have removed but didn't** — a function/struct/module whose
  last real caller was just deleted or rewritten, but the definition is still there (often
  because it's `pub` and so generates no dead-code warning, or because a test still calls it
  directly even though production code no longer does).
- **New code that's never actually wired in** — a config field that parses and validates but
  nothing reads at request time; a new guard/filter/handler defined but never registered in
  its chain (`FilterChain`/`ResponseFilterChain`/`build_handler()` — see `CLAUDE.md`
  "Архитектурные решения" #20-23 for where registration is supposed to happen); a helper
  written for a code path that ended up going a different way.

Before opening a PR (as part of normal self-review, not a separate subagent — this is quick
enough to do directly): for each new or changed `pub` item, and for each new config field,
grep for where it's actually used beyond its own definition/tests. If nothing calls it, either
wire it in or don't ship it. If a refactor removed the last caller of something, delete the
something — don't leave it as a courtesy in case it's needed again (that's what git history
is for).
