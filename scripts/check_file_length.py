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

Known limits (all err in a way that is reported or conservative):
  * A test attribute is recognised when it stands on its own line(s) — rustfmt-wrapped multi-line attributes
    are joined — and applies to an item (`mod`, `fn`, `use`, `impl`, ...). One on a struct field, enum variant
    or match arm would drop the lines that follow it up to the next `;` or matching `}` — the script prints a
    warning to stderr for that case. An attribute on the same line as its item, or `#![cfg(test)]`, is not
    recognised, so that code is counted (an over-count).
  * `--base` marks the files the branch touched and shows before -> after for them; a rename found by git
    (`-M`) is compared with the file's OLD path.
  * It is a simple scanner, not a Rust parser. It is linear in the input (measured: 2 MB in 0.4 s, 8 MB /
    221 000 lines in 1.6 s), including on malformed input: an attribute is joined over at most 50 lines, so a
    stray unclosed `#[` cannot make it slower (a test pins this).

Scope: `src/**/*.rs` and `crates/*/src/**/*.rs`. An unreadable file is skipped with a warning; bytes that are
not UTF-8 are replaced, not fatal.

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
MAX_ATTR_LINES = 50      # longest attribute (in lines) that is joined and classified
MARKER = "<!-- code-length-report -->"

_CHAR_LIT = re.compile(r"'(?:\\u\{[0-9a-fA-F_]+\}|\\x[0-9a-fA-F]{2}|\\.|[^\\'])'")
_RAW_START = re.compile(r'(b|c)?r(#*)"')
_IDENT_CHAR = re.compile(r"[A-Za-z0-9_]")
_TEST_FN_ATTR = re.compile(r"^\s*#\[(?:test|tokio::test(?:\(.*\))?|async_std::test)\]\s*$")
_CFG_ATTR = re.compile(r"^\s*#\[cfg\((.*)\)\]\s*$")
_ATTR_START = re.compile(r"^\s*#\[")
_ITEM_START = re.compile(
    r"^\s*(?:pub(?:\([^)]*\))?\s+)?(?:default\s+)?(?:async\s+)?(?:unsafe\s+)?"
    r"(?:(?:mod|fn|use|impl|struct|enum|const|static|type|trait|extern)\b|macro_rules!)"
)


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
    if not m:
        return False
    try:
        return cfg_requires_test(m.group(1))
    except RecursionError:        # an absurdly nested predicate: not one we can reason about
        return False


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


def _attribute_spans(mask_lines):
    """Map every line that belongs to an attribute `#[...]` to its (first, last) line.

    rustfmt wraps long attributes over several lines, e.g. `#[cfg(all(\\n    test,\\n    feature = "x"\\n))]`,
    so an attribute is joined until its brackets balance. An attribute followed by its item on the same
    line (`#[derive(Debug)] struct S;`) is not a span: that line is ordinary code.
    """
    # bracket balance of every line, computed once (linear), so joining never re-counts a growing buffer
    delta = [ln.count("[") - ln.count("]") for ln in mask_lines]
    spans, i = {}, 0
    while i < len(mask_lines):
        if _ATTR_START.match(mask_lines[i]):
            j, balance = i, delta[i]
            # An attribute that has not closed within MAX_ATTR_LINES lines is not treated as one (unparseable
            # input such as a stray `#[` must not make the scan super-linear); its lines are counted as code.
            while balance > 0 and j + 1 < len(mask_lines) and j - i < MAX_ATTR_LINES:
                j += 1
                balance += delta[j]
            if balance == 0 and mask_lines[j].rstrip().endswith("]"):
                for k in range(i, j + 1):
                    spans[k] = (i, j)
                i = j + 1
                continue
        i += 1
    return spans


def _find_item_end(mask_lines, start, attr_lines):
    """Line index of the last line of the item whose attribute (block) starts at `start`."""
    depth, seen_brace = 0, False
    for k in range(start, len(mask_lines)):
        if k in attr_lines:
            continue
        line = mask_lines[k]
        depth += line.count("{") - line.count("}")
        if "{" in line:
            seen_brace = True
        if seen_brace and depth <= 0:
            return k
        if not seen_brace and line.rstrip().endswith(";"):
            return k
    return len(mask_lines) - 1


def count_source(src, warn=None):
    """Number of code lines in Rust source `src` (no comments, no blank lines, no tests).

    `warn(line_number, message)` is called for a test attribute that is not followed by an item.
    """
    code_view, mask_view = scan(src)
    code_lines = code_view.split("\n")
    mask_lines = mask_view.split("\n")
    spans = _attribute_spans(mask_lines)
    drop = set()
    k = 0
    while k < len(mask_lines):
        span = spans.get(k)
        if not span or span[0] != k or k in drop:
            k += 1
            continue
        attr_first, attr_last = span
        text = re.sub(r"\s+", " ", " ".join(mask_lines[i].strip() for i in range(attr_first, attr_last + 1)))
        if not is_test_attr(text):
            k = attr_last + 1
            continue
        if warn is not None:
            j = attr_last + 1
            while j < len(mask_lines) and (not mask_lines[j].strip() or j in spans):
                j += 1
            if j < len(mask_lines) and not _ITEM_START.match(mask_lines[j]):
                warn(attr_first + 1, "test attribute is not followed by an item; the lines after it up to the "
                                     "next `;` or matching `}` are excluded from the count")
        first = attr_first
        while first > 0 and (first - 1) in spans:            # stacked attributes above
            first = spans[first - 1][0]
        last = _find_item_end(mask_lines, attr_last, spans)
        drop.update(range(first, last + 1))
        k = last + 1
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
    """Run git in `root` (argv list, no shell). Output is decoded as UTF-8 (never the locale encoding) and the
    GIT_* environment is dropped, so a stray GIT_DIR/GIT_WORK_TREE cannot redirect it to another repository."""
    env = {k: v for k, v in os.environ.items() if not k.startswith("GIT_")}
    return subprocess.run(["git", "-C", root, *args], capture_output=True, text=True,
                          encoding="utf-8", errors="replace", env=env)


def valid_ref(ref):
    """A git ref/SHA that cannot be mistaken for an option (`--output=...`) or carry control characters."""
    return bool(ref) and not ref.startswith("-") and not any(c in ref for c in "\r\n\0")


def md_path(p):
    """Keep a repo path inert inside a markdown code span or table cell."""
    return re.sub(r"[`|\r\n]", "_", p)


def changed_files(root, base):
    """`{path: path at base}` for the `.rs` files added/modified/renamed between `base` and HEAD.

    Uses the three-dot form (changes since the merge base). The value is None for an added file, the same path
    for a modified one and the OLD path for a rename, so a renamed file can be compared with what it was.
    Returns None when git cannot produce the diff (bad ref, not a repository, base commit missing).
    """
    if not valid_ref(base):
        return None
    # -z: NUL-separated and never C-quoted, so a path with non-ASCII characters, a tab or a quote is exact.
    args = ("diff", "-z", "--name-status", "-M", "--diff-filter=AMR")
    r = git(root, *args, f"{base}...HEAD")
    if r.returncode != 0:
        r = git(root, *args, base, "HEAD")
    if r.returncode != 0:
        return None
    changed = {}
    tokens = r.stdout.split("\0")
    i = 0
    while i < len(tokens):
        status = tokens[i][:1]
        if status in ("R", "C") and i + 2 < len(tokens):        # R100 <NUL> old <NUL> new
            old, new, i = tokens[i + 1], tokens[i + 2], i + 3
        elif status in ("A", "M") and i + 1 < len(tokens):      # M <NUL> path
            old, new, i = (None if status == "A" else tokens[i + 1]), tokens[i + 1], i + 2
        else:
            i += 1
            continue
        if new.endswith(".rs"):
            changed[new] = old
    return changed


def count_at(root, ref, path):
    """Code lines of `path` as of `ref`, or None (with a warning on stderr) when git cannot show it."""
    r = git(root, "show", f"{ref}:{path}")
    if r.returncode != 0:
        print(f"warning: git show {ref}:{path} failed: {r.stderr.strip()}", file=sys.stderr)
        return None
    return count_source(r.stdout)


def classify(n):
    if n > HARD_LIMIT:
        return "hard"
    if n > SOFT_LIMIT:
        return "soft"
    return "ok"


def build_report(root, base=None):
    counts = {}
    for p in source_files(root):
        try:
            with open(os.path.join(root, p), encoding="utf-8", errors="replace") as f:
                text = f.read()
        except OSError as e:          # e.g. a dangling symlink named *.rs
            print(f"warning: skipping {p}: {e}", file=sys.stderr)
            continue
        counts[p] = count_source(
            text, lambda ln, msg, p=p: print(f"warning: {p}:{ln}: {msg}", file=sys.stderr))
    touched, before, note = set(), {}, None
    if base:
        changed = changed_files(root, base)
        if changed is None:
            note = f"could not diff against `{md_path(base)}`"
            print(f"warning: {note}; touched files are not marked", file=sys.stderr)
        else:
            touched = set(changed)
            for p, old in changed.items():
                if p in counts:
                    before[p] = count_at(root, base, old) if old else None    # None = a new file
    return counts, touched, before, note


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


def render_markdown(counts, touched, before, note=None):
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
    if note:
        out += [f"⚠️ Which files this PR touched could not be determined ({note}), so none are marked below.", ""]
    if touched:
        if not touched_over:
            out += ["✅ No file touched by this PR is over a limit.", ""]
        else:
            worse = f" — **{len(grew)} of them grew**" if grew else ""
            out += [f"**Touched by this PR and over a limit: {len(touched_over)}**{worse}", ""]
        present = [(counts[p], p) for p in touched if p in counts]
        if present:
            top_n, top_p = max(present)
            out += [f"{len(present)} Rust files touched by this PR (tests excluded); the largest is "
                    f"`{md_path(top_p)}` at **{top_n}** code lines.", ""]
    if over:
        out += ["| File | Code lines | Limit | This PR |", "|---|---:|---|---|"]
        for n, p in over:
            lim = f"❌ > {HARD_LIMIT}" if n > HARD_LIMIT else f"⚠️ > {SOFT_LIMIT}"
            if p in touched:
                b = before.get(p)
                this = "new file" if b is None else (f"{b} → {n} ({n - b:+d})" if n != b else f"{n} (unchanged)")
            else:
                this = "—"
            out.append(f"| `{md_path(p)}` | {n} | {lim} | {this} |")
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
    if a.base is not None and not valid_ref(a.base):
        ap.error("--base must be a git ref or SHA (not empty, not starting with '-')")
    root = os.path.abspath(a.root)

    counts, touched, before, note = build_report(root, a.base)
    print(render_text(counts, touched, before))
    if a.markdown:
        with open(a.markdown, "w", encoding="utf-8", newline="\n") as f:
            f.write(render_markdown(counts, touched, before, note))
    if a.fail_on_hard and any(n > HARD_LIMIT for n in counts.values()):
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
