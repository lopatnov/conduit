#!/usr/bin/env python3
"""Validate every configuration block in the documentation, so that an example cannot rot (issue #567).

    python3 scripts/check_doc_configs.py [--conduit "path/to/conduit"] [--jobs N] [FILE ...]

It finds the fenced ```yaml / ```yml / ```json blocks of README.md, npm/Readme.md and docs/*.md (or of the files
given), writes each to a temporary file and runs `conduit validate -c <file>` on it; every file of examples/ is
validated as it is. A block fails when the command exits non-zero, when it prints a warning that names a key
Conduit does not recognise (a typo in an example is a bug), or when its fence is never closed.

A block that is deliberately not a complete configuration (an excerpt, a "shorthand, then full form" pair that
repeats a top-level key, a Kubernetes manifest, an Admin API reply) is marked with an HTML comment on the line
before the fence, invisible in the rendered page:

    <!-- docs-check: skip (why this block is not a whole config) -->

Every unmarked block must validate. A marker that is not followed by a fence is an error, so a stale marker
cannot hide a block that moved. JSON blocks may have whole-line `//` comments, which are removed before the check.

Build the binary with `--features full`, or the examples of a feature that is not compiled in fail validation
("compiled without the feature"). Exit status: 0 all good, 1 something failed, 2 usage or setup error.
"""
import argparse
import concurrent.futures
import glob
import os
import re
import shlex
import subprocess
import sys
import tempfile

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
LANGS = {"yaml": ".yaml", "yml": ".yaml", "json": ".json"}
FENCE = re.compile(r"^(?P<indent>[ \t]*)(?P<fence>`{3,}|~{3,})(?P<info>.*)$")
MARKER = re.compile(r"^\s*<!--\s*docs-check:\s*skip\b(?:\s*\((?P<why>.*)\))?\s*-->\s*$")
# Warnings that make a block fail even though `conduit validate` exits 0 for them.
FATAL_WARNINGS = ("is not a recognized configuration key",)
TIMEOUT_SECS = 60


class Block:
    """One fenced block: where it is, what language, its text, and whether it is marked as a fragment."""

    def __init__(self, path, line, lang, text, skip_reason):
        """Remember where the block is, its language, its text and, if it is marked, why it is skipped."""
        self.path, self.line, self.lang, self.text, self.skip_reason = path, line, lang, text, skip_reason

    @property
    def skipped(self):
        """True when the block carries a skip marker."""
        return self.skip_reason is not None

    def where(self):
        """`file:line` of the block's opening fence, relative to the repository."""
        return f"{os.path.relpath(self.path, REPO)}:{self.line}"


def extract(path, text):
    """Return (blocks, problems) for one markdown file. Only yaml/yml/json fences are blocks."""
    blocks, problems = [], []
    lines = text.splitlines()
    open_fence = None  # (char, length, start line, lang, collected lines, skip reason)
    pending_marker = None  # (line number, reason) of a marker waiting for its fence
    blank_since_marker = 0
    for number, line in enumerate(lines, start=1):
        if open_fence is None:
            marker = MARKER.match(line)
            if marker:
                if pending_marker is not None:
                    problems.append(f"{os.path.relpath(path, REPO)}:{pending_marker[0]}: skip marker is not followed by a fence")
                pending_marker = (number, (marker.group("why") or "").strip() or "fragment")
                blank_since_marker = 0
                continue
            fence = FENCE.match(line)
            if fence:
                info = fence.group("info").strip()
                lang = info.split()[0].lower() if info else ""
                reason = None
                if pending_marker is not None:
                    reason = pending_marker[1]
                    pending_marker = None
                open_fence = (fence.group("fence")[0], len(fence.group("fence")), number, lang, [], reason)
                continue
            if pending_marker is not None:
                if line.strip() == "":
                    blank_since_marker += 1
                    if blank_since_marker > 1:
                        problems.append(f"{os.path.relpath(path, REPO)}:{pending_marker[0]}: skip marker is not followed by a fence")
                        pending_marker = None
                else:
                    problems.append(f"{os.path.relpath(path, REPO)}:{pending_marker[0]}: skip marker is not followed by a fence")
                    pending_marker = None
            continue
        char, length, start, lang, body, reason = open_fence
        # A fence of the same kind that opens a config block inside an open fence is almost surely a missing
        # closer: the block would be swallowed and never validated. (An inner fence of a markdown example is
        # shorter than its outer one, or uses the other fence character, so it does not trip this.)
        inner = FENCE.match(line)
        if inner and inner.group("fence")[0] == char and len(inner.group("fence")) >= length:
            inner_lang = inner.group("info").strip().split()[0].lower() if inner.group("info").strip() else ""
            if inner_lang in LANGS:
                problems.append(
                    f"{os.path.relpath(path, REPO)}:{number}: a {inner_lang} fence opens inside the fence opened at line "
                    f"{start}; is that one missing its closing fence?"
                )
        close = FENCE.match(line)
        if close and close.group("fence")[0] == char and len(close.group("fence")) >= length and not close.group("info").strip():
            if lang in LANGS:
                blocks.append(Block(path, start, lang, "\n".join(body) + "\n", reason))
            open_fence = None
        else:
            body.append(line)
    if open_fence is not None:
        problems.append(f"{os.path.relpath(path, REPO)}:{open_fence[2]}: the fence opened here is never closed")
    if pending_marker is not None:
        problems.append(f"{os.path.relpath(path, REPO)}:{pending_marker[0]}: skip marker is not followed by a fence")
    return blocks, problems


def strip_json_comments(text):
    """Remove whole-line `//` comments (the docs annotate JSON blocks that way)."""
    return "".join(line for line in text.splitlines(keepends=True) if not line.lstrip().startswith("//"))


def validate_file(conduit, path):
    """Run `conduit validate -c path`; return (ok, output_excerpt)."""
    try:
        done = subprocess.run(
            [*conduit, "validate", "-c", path],
            capture_output=True, text=True, timeout=TIMEOUT_SECS, encoding="utf-8", errors="replace",
        )
    except subprocess.TimeoutExpired:
        return False, f"timed out after {TIMEOUT_SECS} s"
    output = (done.stdout + done.stderr).strip()
    fatal = [l for l in output.splitlines() if l.lower().startswith("warning") and any(w in l for w in FATAL_WARNINGS)]
    if done.returncode != 0 or fatal:
        keep = fatal if done.returncode == 0 else output.splitlines()
        return False, "\n".join(keep[:6])
    return True, ""


def check_block(conduit, block, tmp):
    """Write one block to a temporary file and validate it; return (ok, output excerpt)."""
    suffix = LANGS[block.lang]
    text = strip_json_comments(block.text) if suffix == ".json" else block.text
    path = os.path.join(tmp, f"block-{id(block)}{suffix}")
    with open(path, "w", encoding="utf-8", newline="\n") as f:
        f.write(text)
    return validate_file(conduit, path)


def default_files():
    """README.md, npm/Readme.md and docs/*.md, those that exist."""
    files = [os.path.join(REPO, "README.md"), os.path.join(REPO, "npm", "Readme.md")]
    files += sorted(glob.glob(os.path.join(REPO, "docs", "*.md")))
    return [f for f in files if os.path.exists(f)]


def example_files():
    """Every yaml, yml and json file of examples/."""
    out = []
    for pattern in ("*.yaml", "*.yml", "*.json"):
        out += glob.glob(os.path.join(REPO, "examples", pattern))
    return sorted(out)


def run(conduit, files, jobs, with_examples):
    """Extract and validate everything; return (checked, skipped, failures, problems)."""
    problems, blocks = [], []
    for path in files:
        with open(path, encoding="utf-8") as f:
            found, bad = extract(path, f.read())
        blocks += found
        problems += bad
    failures = []
    checked = skipped = 0
    with tempfile.TemporaryDirectory() as tmp, concurrent.futures.ThreadPoolExecutor(max_workers=jobs) as pool:
        jobs_list = []
        for block in blocks:
            if block.skipped:
                skipped += 1
                continue
            jobs_list.append((block.where(), pool.submit(check_block, conduit, block, tmp)))
        if with_examples:
            for path in example_files():
                jobs_list.append((os.path.relpath(path, REPO), pool.submit(validate_file, conduit, path)))
        for where, future in jobs_list:
            checked += 1
            ok, detail = future.result()
            if not ok:
                failures.append((where, detail))
    return checked, skipped, failures, problems


def main(argv=None):
    """Command-line entry point: 0 all good, 1 something failed, 2 usage or setup error."""
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("files", nargs="*", help="markdown files to check (default: README.md, npm/Readme.md, docs/*.md, and examples/)")
    parser.add_argument("--conduit", default=os.environ.get("CONDUIT_BIN", "conduit"), help="the conduit binary (default: $CONDUIT_BIN or `conduit` on PATH)")
    parser.add_argument("--jobs", type=int, default=min(8, os.cpu_count() or 2), help="parallel validations")
    args = parser.parse_args(argv)
    # On Windows shlex keeps the quotes around a quoted path (posix=False), so remove them.
    conduit = [t.strip('"') for t in shlex.split(args.conduit, posix=os.name != "nt")]
    try:
        subprocess.run([*conduit, "--version"], capture_output=True, timeout=TIMEOUT_SECS, check=False)
    except (OSError, subprocess.TimeoutExpired) as e:
        print(f"cannot run {args.conduit!r}: {e}", file=sys.stderr)
        return 2
    files = [os.path.abspath(f) for f in args.files] or default_files()
    for path in files:
        try:
            with open(path, encoding="utf-8") as f:
                f.read(1)
        except (OSError, UnicodeDecodeError) as e:
            print(f"cannot read {path}: {e}", file=sys.stderr)
            return 2
    checked, skipped, failures, problems = run(conduit, files, max(1, args.jobs), with_examples=not args.files)
    for line in problems:
        print(f"PROBLEM  {line}")
    for where, detail in failures:
        print(f"FAIL     {where}")
        for line in detail.splitlines():
            print(f"           {line}")
    print(f"{checked} checked, {skipped} marked as fragments, {len(failures)} failed, {len(problems)} marker or fence problems")
    return 1 if failures or problems else 0


if __name__ == "__main__":
    sys.exit(main())
