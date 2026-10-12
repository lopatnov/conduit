#!/usr/bin/env python3
"""List the new lines a branch added that an lcov report says were never executed.

SonarCloud's "Coverage on New Code" gate counts these lines (the `cargo llvm-cov --lib` report of
.github/workflows/sonar.yml, minus the files in `sonar.coverage.exclusions`), so this shows what to
cover when the gate fails and sonarcloud.io is not reachable (a cloud container's proxy blocks it).

Usage, inside the repository on the PR branch:
  scripts/new_uncovered_lines.py coverage.lcov [base_ref]     # base_ref defaults to origin/main

The added lines are those of the working tree against the merge base with base_ref, the tree cargo
llvm-cov measured: committed, staged and unstaged edits, and every line of an untracked Rust file.
Producing coverage.lcov and the caveats: .claude/commands/sonar-gate.md.
"""
import argparse
import os
import pathlib
import re
import subprocess
import sys

HUNK = re.compile(r"@@ -\d+(?:,(\d+))? \+(\d+)(?:,(\d+))? @@")
GLOB_TOKEN = re.compile(r"\*\*/|\*\*|\*|\?|.", re.S)
GLOB_KINDS = {"**/": "dirs", "**": "any", "*": "star", "?": "one"}


def git(root, *args):
    """Run git in `root` and return its stdout, decoded as UTF-8. The GIT_* environment is dropped, so a stray
    GIT_DIR/GIT_WORK_TREE cannot point it at another repository."""
    env = {k: v for k, v in os.environ.items() if not k.startswith("GIT_")}
    try:
        out = subprocess.run(["git", "-C", root, *args], capture_output=True, check=True, env=env)
    except subprocess.CalledProcessError as e:
        sys.exit(e.stderr.decode("utf-8", errors="replace").strip() or f"git {args[0]} failed")
    return out.stdout.decode("utf-8", errors="replace")


def read_lcov(path, root):
    """{repo-relative file: {line: execution count}} from the `SF:` / `DA:` records."""
    hits, cur = {}, None
    with open(path, encoding="utf-8") as f:
        for raw in f:
            line = raw.strip()
            if line.startswith("SF:"):
                rel = os.path.relpath(os.path.join(root, line[3:]), root)
                cur = hits.setdefault(pathlib.PurePath(rel).as_posix(), {})
            elif line.startswith("DA:") and cur is not None:
                number, count = line[3:].split(",")[:2]
                cur[int(number)] = cur.get(int(number), 0) + int(count)
    return hits


def count(group):
    """A hunk header leaves out a line count of 1."""
    return 1 if group is None else int(group)


def parse_diff(diff):
    """{file: [added line numbers]} from the text of `git diff -U0`."""
    added, path, body = {}, None, 0
    for line in diff.split("\n"):
        if body:  # inside a hunk, so a line that merely looks like a header is content
            if not line.startswith("\\"):  # "\ No newline at end of file" is not a hunk line
                body -= 1
            continue
        if line.startswith("+++ "):
            if line == "+++ /dev/null":  # deleted file
                path = None
            elif line.startswith("+++ b/"):
                path = line[6:].rstrip("\t")  # git ends a name that contains a space with a tab
            else:
                sys.exit(f"cannot parse the diff header {line!r}")
            continue
        m = HUNK.match(line)
        if m:
            old, start, new = count(m.group(1)), int(m.group(2)), count(m.group(3))
            body = old + new
            if path and new:
                added.setdefault(path, []).extend(range(start, start + new))
    return added


def added_lines(root, base):
    """{file: [added line numbers]} of the working tree against the merge base of `base` and HEAD."""
    fork = git(root, "merge-base", base, "HEAD").strip()
    added = parse_diff(git(root, "-c", "core.quotePath=false", "diff", "--no-color", "--no-ext-diff",
                           "--src-prefix=a/", "--dst-prefix=b/", "-U0", fork))
    for name in git(root, "ls-files", "--others", "--exclude-standard", "-z").split("\0"):
        full = os.path.join(root, name)
        # git diff leaves out an untracked file, but the build compiled it. A FIFO or a device is not
        # source, and opening a FIFO would block.
        if name.endswith(".rs") and os.path.isfile(full):
            with open(full, "rb") as f:
                added[name] = list(range(1, sum(1 for _ in f) + 1))  # a last line without newline counts
    return added


def glob_tokens(glob):
    """An Ant-style pattern as (kind, text) tokens: `**/` is "dirs" (zero or more directories), `**` "any"
    (any characters), `*` "star" (any characters within one path segment), `?` "one" (one such character);
    every other character is a "lit"."""
    return [(GLOB_KINDS.get(token, "lit"), token) for token in GLOB_TOKEN.findall(glob)]


def glob_match(tokens, path):
    """True when the whole of `path` matches the tokens of `glob_tokens`. The table of (token, position)
    costs len(tokens) * len(path); a backtracking regex takes exponential time on a pattern such as
    `*a*a*a*a*b`, and the pattern comes from a file in the repository."""
    n = len(path)
    rest = [i == n for i in range(n + 1)]  # rest[i]: the tokens after this one match path[i:]
    for kind, text in reversed(tokens):
        here, slash = [False] * (n + 1), False
        for i in range(n, -1, -1):
            if kind == "lit":
                here[i] = i < n and path[i] == text and rest[i + 1]
            elif kind == "one":
                here[i] = i < n and path[i] != "/" and rest[i + 1]
            elif kind == "star":
                here[i] = rest[i] or (i < n and path[i] != "/" and here[i + 1])
            elif kind == "any":
                here[i] = rest[i] or (i < n and here[i + 1])
            else:  # dirs: nothing, or any text up to and including a "/"
                slash = slash or (i < n and path[i] == "/" and rest[i + 1])
                here[i] = rest[i] or slash
        rest = here
    return rest[0]


def coverage_exclusions(properties):
    """Token lists (see `glob_tokens`) for the `sonar.coverage.exclusions` globs of a sonar-project.properties file."""
    try:
        text = pathlib.Path(properties).read_text(encoding="utf-8")
    except OSError:
        return []
    text = re.sub(r"\\\n[ \t]*", "", text)  # a trailing backslash continues the line
    m = re.search(r"^sonar\.coverage\.exclusions\s*=(.*)$", text, re.M)
    return [glob_tokens(g.strip()) for g in m.group(1).split(",") if g.strip()] if m else []


def shown(path):
    """A file name for the terminal. A name with a control or format character in it (an escape sequence, a
    bidi override) is printed escaped, so a file name cannot drive the terminal."""
    return path if path.isprintable() else ascii(path)


def main(argv=None):
    """Print the uncovered new lines per file, then the share of covered executable lines."""
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("lcov", help="coverage report from cargo llvm-cov --lcov")
    ap.add_argument("base", nargs="?", default="origin/main", help="ref the branch is compared with")
    args = ap.parse_args(argv)
    root = git(".", "rev-parse", "--show-toplevel").strip()
    hits = read_lcov(args.lcov, root)
    excluded = coverage_exclusions(os.path.join(root, "sonar-project.properties"))
    added = added_lines(root, args.base)
    if not added:
        print(f"warning: no lines added between {args.base} and the working tree", file=sys.stderr)
    total = missed = 0
    unmeasured = []
    for path, lines in added.items():
        if any(glob_match(tokens, path) for tokens in excluded):
            continue
        if path not in hits:
            if path.endswith(".rs"):
                unmeasured.append(shown(path))
            continue
        executable = [n for n in lines if n in hits[path]]
        uncovered = [n for n in executable if hits[path][n] == 0]
        total += len(executable)
        missed += len(uncovered)
        if uncovered:
            print(f"{shown(path)}: {len(uncovered)} of {len(executable)} new lines uncovered: {uncovered}")
    if unmeasured:
        print("warning: not in the coverage report (not compiled in that run, or ignored by it): "
              + ", ".join(unmeasured), file=sys.stderr)
    if total:
        print(f"new code: {total - missed}/{total} lines covered = {100.0 * (total - missed) / total:.1f}%")
    else:
        print("new code: no executable lines among the added lines")
    return 0


if __name__ == "__main__":
    sys.exit(main())
