#!/usr/bin/env python3
"""Project health numbers for the journal's "Health" line (owner's retro, 2026-10-03).

Prints production Rust code (the same counter as scripts/check_file_length.py: no comments, blank lines or tests),
the number of workspace crates, and how many bytes of instructions every session loads (CLAUDE.md + .claude/rules).
Informational only: always exits 0.

    python scripts/health.py          # table
    python scripts/health.py --line   # one line for the journal entry
"""
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import check_file_length as cfl  # noqa: E402

ROOT = os.path.dirname(HERE)
# Bytes that are loaded into every session's context. Soft budget set in the 2026-10-03 retro (it was ~204 KB).
INSTRUCTION_BUDGET = 60_000


def production_code(root):
    files = cfl.source_files(root)
    total = 0
    for rel in files:
        with open(os.path.join(root, rel), encoding="utf-8") as f:
            total += cfl.count_source(f.read())
    return total, len(files)


def instruction_bytes(root):
    sizes = {"CLAUDE.md": os.path.getsize(os.path.join(root, "CLAUDE.md"))}
    rules = os.path.join(root, ".claude", "rules")
    if os.path.isdir(rules):
        for name in sorted(os.listdir(rules)):
            if name.endswith(".md"):
                sizes[f".claude/rules/{name}"] = os.path.getsize(os.path.join(rules, name))
    return sizes


def main(argv):
    code, files = production_code(ROOT)
    crates_dir = os.path.join(ROOT, "crates")
    crates = len([d for d in os.listdir(crates_dir) if os.path.isfile(os.path.join(crates_dir, d, "Cargo.toml"))]) if os.path.isdir(crates_dir) else 0
    sizes = instruction_bytes(ROOT)
    loaded = sum(sizes.values())
    flag = "OVER BUDGET" if loaded > INSTRUCTION_BUDGET else "ok"
    if "--line" in argv:
        print(f"production code {code} lines in {files} files, {crates} crates; "
              f"auto-loaded instructions {loaded // 1000} KB (budget {INSTRUCTION_BUDGET // 1000} KB, {flag})")
        return 0
    print(f"production Rust code : {code} lines in {files} files")
    print(f"workspace crates     : {crates}")
    for name, n in sizes.items():
        print(f"  {name:32s} {n:8d} bytes")
    print(f"auto-loaded instructions: {loaded} bytes (budget {INSTRUCTION_BUDGET}): {flag}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
