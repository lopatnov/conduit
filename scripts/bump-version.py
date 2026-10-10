#!/usr/bin/env python3
"""Bump the Conduit version everywhere it lives, in lockstep.

Usage:
  scripts/bump-version.py major|minor|patch|X.Y.Z[-pre] [--dry-run] [--no-lock]

Touches, together (see .claude/rules/conventions.md "Versioning"):
  * Cargo.toml: [workspace.package].version and every `lopatnov-conduit-*` `version = "..."`
    in [workspace.dependencies] (cargo publish resolves inter-crate deps through those literals)
  * Cargo.lock (`cargo update --workspace --offline`, skipped with --no-lock)
  * npm/package.json
  * docs/benchmarks.md, docs/cli.md, docs/deployment.md: the old full version, and the
    `:MAJOR.MINOR` Docker tag aliases in docs/deployment.md

It does not touch CHANGELOG.md (move [Unreleased] by hand), commit, tag or push. It ends by running
scripts/check-workspace-versions.sh. With --dry-run nothing is written; the planned edits are printed.
"""
import argparse
import json
import os
import re
import subprocess
import sys

ROOT = os.path.normpath(os.path.join(os.path.dirname(os.path.abspath(__file__)), ".."))
_NUM = r"(0|[1-9]\d*)"
_PRE = r"(?:0|[1-9]\d*|\d*[A-Za-z-][0-9A-Za-z-]*)"  # SemVer 2.0.0 item 9: no leading zeroes, no empty identifier
SEMVER = re.compile(rf"^{_NUM}\.{_NUM}\.{_NUM}(-{_PRE}(?:\.{_PRE})*)?$")
DOC_FILES = ["docs/benchmarks.md", "docs/cli.md", "docs/deployment.md"]


def parse(version):
    m = SEMVER.match(version)
    if not m:
        raise ValueError(f"not a SemVer version: {version!r}")
    return int(m[1]), int(m[2]), int(m[3]), m[4] or ""


def next_version(current, bump):
    """`bump` is major|minor|patch or an explicit version. A keyword bump drops any pre-release tag."""
    major, minor, patch, _ = parse(current)
    if bump == "major":
        return f"{major + 1}.0.0"
    if bump == "minor":
        return f"{major}.{minor + 1}.0"
    if bump == "patch":
        return f"{major}.{minor}.{patch + 1}"
    parse(bump)
    if bump == current:
        raise ValueError(f"already at {current}")
    return bump


def current_version(cargo_toml):
    """[workspace.package].version: the first `version = "..."` line, as check-workspace-versions.sh reads it."""
    m = re.search(r'^version\s*=\s*"([^"]+)"', cargo_toml, re.M)
    if not m:
        raise ValueError("no [workspace.package].version in Cargo.toml")
    return m[1]


def bump_cargo_toml(text, old, new):
    """Returns (new_text, number of lopatnov-conduit-* dependency literals changed)."""
    text, n_pkg = re.subn(r'^(version\s*=\s*)"' + re.escape(old) + '"', rf'\1"{new}"', text, count=1, flags=re.M)
    if n_pkg != 1:
        raise ValueError(f"[workspace.package].version is not {old}")
    dep = re.compile(r'^(lopatnov-conduit-[a-z0-9-]+\s*=\s*\{[^}\n]*?version\s*=\s*)"' + re.escape(old) + '"', re.M)
    text, n_deps = dep.subn(rf'\1"{new}"', text)
    return text, n_deps


def bump_package_json(text, old, new):
    data = json.loads(text)
    if data.get("version") != old:
        raise ValueError(f"npm/package.json is at {data.get('version')!r}, expected {old}")
    out, n = re.subn(r'^(\s*"version"\s*:\s*)"' + re.escape(old) + '"', rf'\1"{new}"', text, count=1, flags=re.M)
    if n != 1:
        raise ValueError("could not find the top-level version line in npm/package.json")
    return out


def bump_doc(text, old, new, with_tag_aliases=False):
    """Replace the old full version; optionally also the `:MAJOR.MINOR` image tag aliases (`:2.0`, `:2.0-full`).

    The aliases move only for a stable target: the release workflow publishes `{{major}}.{{minor}}` tags
    for stable versions only, so an rc bump must not point the docs at tags that do not exist."""
    text = re.sub(r"(?<![\d.])" + re.escape(old) + r"(?!\d|\.\d)", new, text)
    if with_tag_aliases and not parse(new)[3]:
        # Whatever alias is in the docs now (it stays on the previous stable line after an rc bump), not one derived from `old`.
        n_major, n_minor = parse(new)[:2]
        text = re.sub(r"(?<=`):\d+\.\d+(?=(?:-full)?`)", f":{n_major}.{n_minor}", text)
    return text


def read(path):
    with open(os.path.join(ROOT, path), encoding="utf-8", newline="") as f:
        return f.read()


def write(path, text):
    with open(os.path.join(ROOT, path), "w", encoding="utf-8", newline="") as f:
        f.write(text)


def plan(bump):
    """All file edits as {path: new_text}, plus a report of what changed."""
    cargo = read("Cargo.toml")
    old = current_version(cargo)
    new = next_version(old, bump)
    edits, report = {}, [f"{old} -> {new}"]
    edits["Cargo.toml"], n_deps = bump_cargo_toml(cargo, old, new)
    report.append(f"Cargo.toml: [workspace.package] + {n_deps} lopatnov-conduit-* dependency versions")
    edits["npm/package.json"] = bump_package_json(read("npm/package.json"), old, new)
    report.append("npm/package.json: version")
    for path in DOC_FILES:
        text = read(path)
        out = bump_doc(text, old, new, with_tag_aliases=path.endswith("deployment.md"))
        if out != text:
            edits[path] = out
            report.append(f"{path}: version strings")
        else:
            report.append(f"{path}: nothing to change")
    return old, new, edits, report


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("bump", help="major | minor | patch | an explicit X.Y.Z[-pre]")
    ap.add_argument("--dry-run", action="store_true", help="print the plan, write nothing")
    ap.add_argument("--no-lock", action="store_true", help="skip the Cargo.lock refresh")
    args = ap.parse_args(argv)

    try:
        old, new, edits, report = plan(args.bump)
    except (ValueError, OSError) as e:
        print(f"error: {e}", file=sys.stderr)
        return 2
    print("\n".join(report))
    if args.dry_run:
        print("dry run: nothing written")
        return 0
    for path, text in edits.items():
        write(path, text)
    if not args.no_lock:
        subprocess.run(["cargo", "update", "--workspace", "--offline"], cwd=ROOT, check=True)
        print("Cargo.lock: refreshed")
    subprocess.run(["bash", "scripts/check-workspace-versions.sh"], cwd=ROOT, check=True)
    print(f"done: {old} -> {new}. Move CHANGELOG [Unreleased] by hand, then follow .claude/skills/release/SKILL.md.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
