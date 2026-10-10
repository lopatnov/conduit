#!/usr/bin/env python3
"""Prove that a code-move commit only moved code (#479).

Every extraction PR (#145, #146, #222, #316 ...) needs the same proof: the items that left the old files
are byte-identical to the items in the new files, apart from a short, printed allow-list of edits. This
script is that proof.

  scripts/verify-move.py OLD_REV --from OLD_FILE... --to NEW_FILE... [--rename A=B]... [--allow REGEX]...

  OLD_REV        revision the old files are read from (`git show OLD_REV:FILE`)
  --from FILES   the files the code was cut out of, as they were at OLD_REV
  --to FILES     the files the code now lives in, read from the working tree
  --rename A=B   substitute A with B in the OLD text before comparing (a path moved, a module renamed)
  --allow REGEX  ignore every item whose key matches (a deliberately added or dropped item)

Both sides are split into top-level items (leading comments and attributes included; `mod name { .. }`
bodies are recursed into, so `mod tests` items get the key `tests::name`) with a string- and comment-aware
scanner. Items are matched by key (kind and name, plus the `#[cfg(..)]` attributes, so two cfg variants of
one function stay apart) across ALL given files: where an item moved to is not part of the proof.

Normalisations applied to both sides before the byte comparison, and printed:
  * visibility prefixes `pub`, `pub(crate)`, `pub(super)`, `pub(in ..)` are removed;
  * common indentation is removed and trailing whitespace is stripped (an item moved into or out of a
    `mod` changes its indentation);
  * `use` items are not compared at all: imports change with the new location (their count is printed).

Exit status: 0 only if nothing was lost, nothing was added and nothing differs after the normalisations
(and the --rename / --allow escapes). Semantic equivalence (types resolve the same, feature gates are
preserved) stays with the compiler, clippy and the golden tests.

Known limits: a simple scanner, not a Rust parser. Items produced by macro invocations at the top level
(`foo! { .. }`) are keyed by their text; `macro_rules!` bodies are kept whole.
"""
import argparse
import difflib
import re
import subprocess
import sys
import textwrap

KINDS = ("fn", "struct", "enum", "trait", "type", "const", "static", "mod", "union", "use", "impl", "macro_rules!")
# Items that end only at a top-level `;` (a `}` inside them does not end the item).
SEMI_ITEMS = ("use", "const", "static", "type", "extern crate")
VIS_RE = re.compile(r"\bpub(?:\s*\([^)]*\))?\s+")
HEAD_RE = re.compile(
    r"^(?:pub(?:\s*\([^)]*\))?\s+)?(?:default\s+)?(?:unsafe\s+|async\s+|const\s+|extern\s+(?:\"[^\"]*\"\s+)?)*"
    r"(fn|struct|enum|trait|type|const|static|mod|union|use|impl|macro_rules!|extern crate)(?![A-Za-z0-9_])\s*"
)


def _skip_string(s, i):
    """`s[i]` is the opening quote of a normal string; return the index after the closing one."""
    i += 1
    while i < len(s):
        if s[i] == "\\":
            i += 2
        elif s[i] == '"':
            return i + 1
        else:
            i += 1
    return i


def _raw_string_end(s, i):
    """If a raw string (`r"`, `r#"`, `br#"` ...) starts at `i`, return the index after it, else None."""
    j = i
    if s[j] == "b":
        j += 1
    if j >= len(s) or s[j] != "r":
        return None
    j += 1
    hashes = 0
    while j < len(s) and s[j] == "#":
        hashes += 1
        j += 1
    if j >= len(s) or s[j] != '"':
        return None
    end = s.find('"' + "#" * hashes, j + 1)
    return len(s) if end < 0 else end + 1 + hashes


def scan_code(s):
    """Yield (index, char) for every character that is real code: not in a comment, string or char literal."""
    i, n = 0, len(s)
    while i < n:
        c = s[i]
        if c == "/" and s.startswith("//", i):
            j = s.find("\n", i)
            i = n if j < 0 else j
        elif c == "/" and s.startswith("/*", i):
            depth, i = 1, i + 2
            while i < n and depth:
                if s.startswith("/*", i):
                    depth, i = depth + 1, i + 2
                elif s.startswith("*/", i):
                    depth, i = depth - 1, i + 2
                else:
                    i += 1
        elif c in "rb" and (i == 0 or not (s[i - 1].isalnum() or s[i - 1] == "_")) and _raw_string_end(s, i):
            i = _raw_string_end(s, i)
        elif c == "b" and s.startswith('b"', i):
            i = _skip_string(s, i + 1)
        elif c == '"':
            i = _skip_string(s, i)
        elif c == "'":
            # char literal ('a', '\n', '\u{1F600}') versus lifetime ('a, 'static)
            m = re.match(r"'(?:\\(?:u\{[0-9a-fA-F_]+\}|x[0-9a-fA-F]{2}|.)|[^\\'])'", s[i:i + 14])
            if m:
                i += m.end()
            else:
                yield i, c
                i += 1
        else:
            yield i, c
            i += 1


def split_items(text):
    """Split `text` into top-level item chunks (comments and attributes stay with the item that follows)."""
    chunks, start = [], 0
    depth = 0  # (), [], {} nesting together
    brace = 0
    head = None
    for i, c in scan_code(text):
        if c in "([{":
            depth += 1
            brace += c == "{"
        elif c in ")]}":
            depth -= 1
            if c == "}":
                brace -= 1
                if depth == 0 and brace == 0 and head is not None and head not in SEMI_ITEMS:
                    chunks.append(text[start:i + 1])
                    start, head = i + 1, None
                    continue
        elif c == ";" and depth == 0:
            chunks.append(text[start:i + 1])
            start, head = i + 1, None
            continue
        if head is None and depth == 0 and not c.isspace() and c not in "#!":
            head = _item_kind(text[start:]) or "?"
    tail = text[start:]
    if _item_kind(tail) is not None:
        chunks.append(tail)
    return [c.strip("\n") for c in chunks if c.strip()]


def _strip_leading(chunk):
    """The chunk without its leading comments and attributes."""
    out, i, n = [], 0, len(chunk)
    while i < n:
        if chunk[i].isspace():
            i += 1
        elif chunk.startswith("//", i):
            j = chunk.find("\n", i)
            i = n if j < 0 else j
        elif chunk.startswith("/*", i):
            j = chunk.find("*/", i)
            i = n if j < 0 else j + 2
        elif chunk.startswith("#", i):
            j = chunk.find("[", i)
            if j < 0:
                break
            depth, k = 0, j
            for idx, ch in scan_code(chunk[j:]):
                if ch == "[":
                    depth += 1
                elif ch == "]":
                    depth -= 1
                    if depth == 0:
                        k = j + idx + 1
                        break
            i = k
        else:
            break
    return chunk[i:]


def _item_kind(chunk):
    m = HEAD_RE.match(_strip_leading(chunk))
    return m.group(1) if m else None


def _cfg_attrs(chunk):
    return tuple(re.findall(r"#\[cfg\((?:[^\[\]]|\[[^\]]*\])*\)\]", _leading(chunk)))


def _leading(chunk):
    body = _strip_leading(chunk)
    return chunk[:len(chunk) - len(body)]


def item_key(chunk):
    body = _strip_leading(chunk)
    m = HEAD_RE.match(body)
    if not m:
        return ("other", " ".join(body.split())[:80]), None
    kind, rest = m.group(1), body[m.end():]
    if kind == "impl":
        header = re.split(r"\{", rest, 1)[0]
        name = " ".join(header.split())
    elif kind in ("use", "extern crate"):
        name = " ".join(rest.split()).rstrip(";")
    else:
        nm = re.match(r"[A-Za-z_][A-Za-z0-9_]*", rest)
        name = nm.group(0) if nm else rest[:40]
    cfg = " ".join(" ".join(_cfg_attrs(chunk)).split())
    return (kind, name, cfg), (rest if kind == "mod" else None)


def collect(text, prefix=""):
    """Map key -> list of normalised-ready item texts, recursing into inline `mod name { .. }` bodies."""
    items = {}
    for chunk in split_items(text):
        key, mod_rest = item_key(chunk)
        if key[0] == "mod" and mod_rest is not None and "{" in mod_rest and not mod_rest.lstrip().startswith(";"):
            name = key[1]
            body = chunk[chunk.index("{", chunk.index(name)) + 1:chunk.rindex("}")]
            for k, v in collect(textwrap.dedent(body), f"{prefix}{name}::").items():
                items.setdefault(k, []).extend(v)
            continue
        full = (key[0], prefix + key[1], *key[2:])
        items.setdefault(full, []).append(chunk)
    return items


def normalise(chunk, renames=()):
    for a, b in renames:
        chunk = chunk.replace(a, b)
    chunk = VIS_RE.sub("", chunk)
    lines = chunk.split("\n")
    while lines and not lines[0].strip():
        lines.pop(0)
    if not lines:
        return ""
    # An item is indented as a whole: drop the first line's indentation from every line.
    k = len(lines[0]) - len(lines[0].lstrip())
    out = [lines[0].strip()]
    for line in lines[1:]:
        lead = len(line) - len(line.lstrip())
        out.append(line[min(k, lead):].rstrip())
    return "\n".join(out).strip("\n")


def label(key):
    kind, name = key[0], key[1]
    cfg = f"  {key[2]}" if len(key) > 2 and key[2] else ""
    return f"{kind} {name}{cfg}"


def compare(old_text, new_text, renames=(), allow=()):
    old, new = collect(old_text), collect(new_text)
    allow_res = [re.compile(a) for a in allow]

    def skipped(k):
        return k[0] in ("use", "extern crate") or any(r.search(label(k)) for r in allow_res)

    lost = sorted((k for k in old if k not in new and not skipped(k)), key=label)
    added = sorted((k for k in new if k not in old and not skipped(k)), key=label)
    changed = []
    for k in sorted(set(old) & set(new), key=label):
        if skipped(k):
            continue
        o = [normalise(c, renames) for c in old[k]]
        n = [normalise(c) for c in new[k]]
        # Repeated keys (several `impl X` blocks) are matched as multisets of texts.
        for text in list(o):
            if text in n:
                n.remove(text)
                o.remove(text)
        for ot, nt in zip(o, n):
            changed.append((k, ot, nt))
        for ot in o[len(n):]:
            changed.append((k, ot, ""))
        for nt in n[len(o):]:
            changed.append((k, "", nt))
    uses = sum(len(v) for k, v in old.items() if k[0] in ("use", "extern crate"))
    return lost, added, changed, uses


def git_show(rev, path):
    return subprocess.run(["git", "show", f"{rev}:{path}"], check=True, capture_output=True, text=True).stdout


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("old_rev")
    ap.add_argument("--from", dest="src", nargs="+", required=True, metavar="OLD_FILE")
    ap.add_argument("--to", dest="dst", nargs="+", required=True, metavar="NEW_FILE")
    ap.add_argument("--rename", action="append", default=[], metavar="A=B")
    ap.add_argument("--allow", action="append", default=[], metavar="REGEX")
    args = ap.parse_args(argv)
    renames = []
    for r in args.rename:
        if "=" not in r:
            ap.error(f"--rename wants A=B, got {r!r}")
        renames.append(tuple(r.split("=", 1)))

    try:
        old_text = "\n\n".join(git_show(args.old_rev, p) for p in args.src)
    except subprocess.CalledProcessError as e:
        print(f"cannot read an old file: {e.stderr.strip() or e}", file=sys.stderr)
        return 2
    new_text = "\n\n".join(open(p, encoding="utf-8").read() for p in args.dst)
    lost, added, changed, uses = compare(old_text, new_text, renames, args.allow)

    print("normalisations: visibility prefixes removed; common indentation and trailing whitespace ignored; "
          f"`use` items not compared ({uses} in the old files)")
    if renames:
        print("renames applied to the old text: " + ", ".join(f"{a} -> {b}" for a, b in renames))
    if args.allow:
        print("items ignored by --allow: " + ", ".join(args.allow))
    for title, keys in (("LOST (only in the old files)", lost), ("ADDED (only in the new files)", added)):
        if keys:
            print(f"\n{title}:")
            for k in keys:
                print(f"  - {label(k)}")
    for k, ot, nt in changed:
        print(f"\nCHANGED: {label(k)}")
        diff = difflib.unified_diff(ot.split("\n"), nt.split("\n"), "old", "new", lineterm="")
        print("\n".join(diff))
    problems = len(lost) + len(added) + len(changed)
    if problems:
        print(f"\nFAIL: {len(lost)} lost, {len(added)} added, {len(changed)} changed")
        return 1
    print("\nOK: every item moved byte-identically (after the normalisations above)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
