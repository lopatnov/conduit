#!/usr/bin/env python3
"""Report Rust files that exceed the project's code-length limits.

Counts *code lines* per file — comments, blank lines and tests are NOT counted — and compares them
with the limits in `.claude/rules/conventions.md` ("Code quality" -> "File length"):

  * soft limit 400   crossing it is a signal to split the file (call the `architect` agent)
  * hard limit 1000  production code must never reach it

What is excluded, and how:
  * `//` and `///` and `//!` comments, `/* ... */` comments (nested), blank lines;
  * items whose `cfg` requires a test build — `#[cfg(test)]`, `#[cfg(all(test, feature = "x"))]`, but not
    `any(test, ...)` or `not(test)` — and `#[test]` / `#[tokio::test]` functions, cut by brace matching
    (not by heuristics on indentation);
  * whole test files: anything under a `tests` directory, `tests.rs`, `*_tests.rs`.
The scanner is string-aware: `//` inside a string literal is not a comment, raw strings (`r#"..."#`),
byte strings, char literals such as `'"'` and lifetimes (`'a`) are handled, and braces inside
literals do not confuse the brace matching. Text inside a multi-line string literal counts as code.

Scope: `src/**/*.rs` and `crates/*/src/**/*.rs`.

Usage:
  python scripts/check_file_length.py                       # text report, exit 0
  python scripts/check_file_length.py --markdown out.md     # also write the PR-comment body
  python scripts/check_file_length.py --base origin/main    # mark files this branch touched, with before -> after
  python scripts/check_file_length.py --fail-on-hard        # exit 1 if any file is over the hard limit
"""
import argparse
import os
import re
import subprocess
import sys

SOFT_LIMIT = 400
HARD_LIMIT = 1000
MARKER = "<!-- code-length-report -->"

_CHAR_LIT = re.compile(r"'(?:\\u\{[0-9a-fA-F_]+\}|\\x[0-9a-fA-F]{2}|\\.|[^\\'])'")
_RAW_START = re.compile(r'(b|c)?r(#*)"')
_IDENT_CHAR = re.compile(r"[A-Za-z0-9_]")
_TEST_FN_ATTR = re.compile(r"^\s*#\[(?:test|tokio::test(?:\(.*\))?|async_std::test)\]\s*$")
_CFG_ATTR = re.compile(r"^\s*#\[cfg\((.*)\)\]\s*$")
_ANY_ATTR = re.compile(r"^\s*#\[.*\]\s*$")


def _split_args(s):
    """Split `a, b(c, d), e` on the top-level commas."""
    args, depth, cur = [], 0, []
    for ch in s:
        if ch == "(":
            depth += 1
        elif ch == ")":
            depth -= 1
        if ch == "," and depth == 0:
            args.append("".join(cur).strip())
            cur = []
        else:
            cur.append(ch)
    tail = "".join(cur).strip()
    if tail:
        args.append(tail)
    return args


def cfg_requires_test(expr):
    """True when the `cfg` predicate can only hold in a test build.

    `test` does; `all(..., test, ...)` does (any member requiring it is enough, nested `all` included).
    `any(test, feature = "x")` does NOT (it also holds without `test`), and neither does `not(test)`.
    """
    e = expr.strip()
    if e == "test":
        return True
    m = re.match(r"^all\((.*)\)$", e)
    if m:
        return any(cfg_requires_test(a) for a in _split_args(m.group(1)))
    return False


def is_test_attr(line):
    if _TEST_FN_ATTR.match(line):
        return True
    m = _CFG_ATTR.match(line)
    return bool(m) and cfg_requires_test(m.group(1))


def scan(src):
    """Return (code_view, mask_view).

    Both have exactly the same line structure as `src`, with comments removed. `code_view` keeps the
    contents of string/char literals (they count as code); `mask_view` blanks them out so that braces
    inside literals cannot confuse brace matching.
    """
    code, mask = [], []
    i, n = 0, len(src)

    def emit(text, masked=False):
        code.append(text)
        if masked:
            mask.append("".join(c if c == "\n" else " " for c in text))
        else:
            mask.append(text)

    while i < n:
        c = src[i]
        two = src[i:i + 2]
        if two == "//":
            while i < n and src[i] != "\n":
                i += 1
        elif two == "/*":
            depth, i = 1, i + 2
            while i < n and depth:
                if src[i:i + 2] == "/*":
                    depth, i = depth + 1, i + 2
                elif src[i:i + 2] == "*/":
                    depth, i = depth - 1, i + 2
                else:
                    if src[i] == "\n":
                        emit("\n")
                    i += 1
        elif c in "rbc" and (i == 0 or not _IDENT_CHAR.match(src[i - 1])) and _RAW_START.match(src, i):
            m = _RAW_START.match(src, i)
            close = '"' + m.group(2)
            j = src.find(close, m.end())
            j = n if j < 0 else j + len(close)
            emit(src[i:j], masked=True)
            i = j
        elif c == '"':
            j = i + 1
            while j < n and src[j] != '"':
                j += 2 if src[j] == "\\" else 1
            j = min(j + 1, n)
            emit(src[i:j], masked=True)
            i = j
        elif c == "'":
            m = _CHAR_LIT.match(src, i)
            if m:
                emit(m.group(0), masked=True)
                i = m.end()
            else:                       # a lifetime or loop label
                emit(c)
                i += 1
        else:
            emit(c)
            i += 1
    return "".join(code), "".join(mask)


def _find_item_end(mask_lines, start):
    """Line index of the last line of the item whose (first) attribute is at `start`."""
    depth, seen_brace = 0, False
    for k in range(start, len(mask_lines)):
        line = mask_lines[k]
        if k > start or not _ANY_ATTR.match(line):
            depth += line.count("{") - line.count("}")
            if "{" in line:
                seen_brace = True
            if seen_brace and depth <= 0:
                return k
            if not seen_brace and line.rstrip().endswith(";"):
                return k
    return len(mask_lines) - 1


def count_source(src):
    """Number of code lines in Rust source `src` (no comments, no blank lines, no tests)."""
    code_view, mask_view = scan(src)
    code_lines = code_view.split("\n")
    mask_lines = mask_view.split("\n")
    drop = set()
    k = 0
    while k < len(mask_lines):
        if is_test_attr(mask_lines[k]) and k not in drop:
            first = k
            while first > 0 and _ANY_ATTR.match(mask_lines[first - 1]):   # stacked attributes above
                first -= 1
            last = _find_item_end(mask_lines, k)
            drop.update(range(first, last + 1))
            k = last + 1
        else:
            k += 1
    return sum(1 for idx, ln in enumerate(code_lines) if idx not in drop and ln.strip())


def is_test_path(path):
    parts = path.replace("\\", "/").split("/")
    stem = os.path.splitext(parts[-1])[0]
    return "tests" in parts[:-1] or stem == "tests" or stem.endswith("_tests")


def source_files(root):
    found = []
    roots = [os.path.join(root, "src")]
    crates = os.path.join(root, "crates")
    if os.path.isdir(crates):
        roots += [os.path.join(crates, d, "src") for d in sorted(os.listdir(crates))]
    for base in roots:
        for dirpath, dirnames, filenames in os.walk(base):
            dirnames[:] = sorted(d for d in dirnames if d != "target")
            for fn in sorted(filenames):
                if fn.endswith(".rs"):
                    rel = os.path.relpath(os.path.join(dirpath, fn), root).replace("\\", "/")
                    if not is_test_path(rel):
                        found.append(rel)
    return sorted(found)


def git(root, *args):
    return subprocess.run(["git", "-C", root, *args], capture_output=True, text=True)


def changed_files(root, base):
    """Paths added/modified between `base` and HEAD (three-dot: since the merge base)."""
    r = git(root, "diff", "--name-only", "--diff-filter=AMR", f"{base}...HEAD")
    if r.returncode != 0:
        r = git(root, "diff", "--name-only", "--diff-filter=AMR", base, "HEAD")
    return {p.strip() for p in r.stdout.splitlines() if p.strip().endswith(".rs")}


def count_at(root, ref, path):
    r = git(root, "show", f"{ref}:{path}")
    return count_source(r.stdout) if r.returncode == 0 else None


def classify(n):
    if n > HARD_LIMIT:
        return "hard"
    if n > SOFT_LIMIT:
        return "soft"
    return "ok"


def build_report(root, base=None):
    counts = {}
    for p in source_files(root):
        with open(os.path.join(root, p), encoding="utf-8") as f:
            counts[p] = count_source(f.read())
    touched = changed_files(root, base) if base else set()
    before = {p: count_at(root, base, p) for p in touched if p in counts} if base else {}
    return counts, touched, before


def render_text(counts, touched, before):
    rows = sorted(((n, p) for p, n in counts.items() if n > SOFT_LIMIT), reverse=True)
    lines = [f"files scanned: {len(counts)}  (soft limit {SOFT_LIMIT}, hard limit {HARD_LIMIT})"]
    for n, p in rows:
        mark = "HARD" if n > HARD_LIMIT else "soft"
        extra = ""
        if p in touched:
            b = before.get(p)
            extra = f"   [touched: {b if b is not None else 'new'} -> {n}]"
        lines.append(f"  {mark}  {n:5d}  {p}{extra}")
    if not rows:
        lines.append("  no file exceeds the soft limit")
    return "\n".join(lines)


def render_markdown(counts, touched, before):
    over = sorted(((n, p) for p, n in counts.items() if n > SOFT_LIMIT), reverse=True)
    hard = [x for x in over if x[0] > HARD_LIMIT]
    soft = [x for x in over if x[0] <= HARD_LIMIT]
    touched_over = [x for x in over if x[1] in touched]
    grew = [x for x in touched_over
            if before.get(x[1]) is not None and x[0] > before[x[1]]]

    if not over:
        status = f"✅ No file exceeds the soft limit of {SOFT_LIMIT} code lines."
    else:
        bits = []
        if hard:
            bits.append(f"❌ **{len(hard)}** over the hard limit ({HARD_LIMIT})")
        if soft:
            bits.append(f"⚠️ **{len(soft)}** over the soft limit ({SOFT_LIMIT})")
        status = " · ".join(bits)
    out = [MARKER, "", "## 📏 Code length report", "",
           "Rust **code lines** per file (comments, blank lines and tests excluded) against the limits in "
           f"`.claude/rules/conventions.md`: soft **{SOFT_LIMIT}**, hard **{HARD_LIMIT}**. "
           "Informational — not a merge gate.", "",
           status, ""]
    if touched:
        if not touched_over:
            out += ["✅ No file touched by this PR is over a limit.", ""]
        else:
            worse = f" — **{len(grew)} of them grew**" if grew else ""
            out += [f"**Touched by this PR and over a limit: {len(touched_over)}**{worse}", ""]
    if over:
        out += ["| File | Code lines | Limit | This PR |", "|---|---:|---|---|"]
        for n, p in over:
            lim = f"❌ > {HARD_LIMIT}" if n > HARD_LIMIT else f"⚠️ > {SOFT_LIMIT}"
            if p in touched:
                b = before.get(p)
                this = "new file" if b is None else (f"{b} → {n} ({n - b:+d})" if n != b else f"{n} (unchanged)")
            else:
                this = "—"
            out.append(f"| `{p}` | {n} | {lim} | {this} |")
        out.append("")
    out += ["<sub>Crossing 400 is the signal to split a file (see the `architect` role in "
            "`.claude/rules/workflow.md`); production code must never reach 1000. "
            f"{len(counts)} files scanned by `scripts/check_file_length.py`.</sub>", ""]
    return "\n".join(out)


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--root", default=os.path.join(os.path.dirname(os.path.abspath(__file__)), ".."))
    ap.add_argument("--base", help="git ref to compare against; marks files this branch touched")
    ap.add_argument("--markdown", metavar="FILE", help="write the PR-comment body to FILE")
    ap.add_argument("--fail-on-hard", action="store_true", help="exit 1 if any file exceeds the hard limit")
    a = ap.parse_args(argv)
    root = os.path.abspath(a.root)

    counts, touched, before = build_report(root, a.base)
    print(render_text(counts, touched, before))
    if a.markdown:
        with open(a.markdown, "w", encoding="utf-8", newline="\n") as f:
            f.write(render_markdown(counts, touched, before))
    if a.fail_on_hard and any(n > HARD_LIMIT for n in counts.values()):
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
