#!/usr/bin/env python3
"""Third-party licence notices for the Conduit binaries (issue #640).

    python3 scripts/third_party_licenses.py --write THIRD-PARTY-LICENSES.txt   # regenerate the file
    python3 scripts/third_party_licenses.py --check THIRD-PARTY-LICENSES.txt   # CI: policy + the file is current
    python3 scripts/third_party_licenses.py --policy                           # policy only, no file

What it does, from `Cargo.lock` through `cargo metadata` (no extra tool to install):

* takes every crate that can end up in a binary: the *normal* dependencies of the `lopatnov-conduit` package with
  `--features full,jemalloc` (the largest build, with the alternative allocator `full` leaves out; every smaller one is a subset), on every target platform, minus the
  workspace's own crates (those are Conduit, Apache-2.0, `LICENSE`);
* checks each crate's SPDX licence expression against the allow-list in `ci/license-policy.json`: an `OR` needs one
  allowed alternative, an `AND` needs all of them, `WITH <exception>` needs the exception to be allowed too. A crate
  that fails, or has no licence expression, fails the check unless the policy has an entry for it under `clarify` that names the exact
  string the crate declares today (`"declared"`, left out when it declares nothing);
* writes one file with the list of crates (name, version, licence, repository) and then the licence and notice files
  that each crate ships, every distinct text once with the crates it applies to. Licences such as MIT, BSD and ISC
  require the text and the copyright notice to travel with a binary distribution (Apache-2.0 section 4 asks for the
  same for NOTICE files).

A crate whose package carries no licence file is listed in its own section with its SPDX expression: the text of the
standard licence is then what the SPDX identifier names (the copyright line is the crate's repository).

`--check` fails when a crate is outside the policy, when a crate in the lock has a licence the policy does not know, and
when the file is not what `--write` would produce now (a changed `Cargo.lock` needs a regenerated file).
"""
import argparse
import hashlib
import json
import os
import re
import subprocess
import sys
import textwrap

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
POLICY = os.path.join(REPO, "ci", "license-policy.json")
ROOT_PACKAGE = "lopatnov-conduit"
FEATURES = "full,jemalloc"
LICENSE_FILE = re.compile(r"^(licen[cs]e|copying|notice|unlicense|copyright)([-_. ].*)?$", re.IGNORECASE)
MAX_LICENSE_BYTES = 200 * 1024


# ── SPDX expressions ────────────────────────────────────────────────────────────

class SpdxError(ValueError):
    """The expression is not valid SPDX."""


def tokenize(expr):
    """Split an SPDX expression into identifiers, operators and parentheses. The legacy `/` means OR (Cargo accepts it)."""
    expr = expr.replace("/", " OR ")
    return re.findall(r"\(|\)|[^\s()]+", expr)


def parse(expr):
    """Parse an SPDX expression into a tree: ('id', name), ('with', id, exception), ('and', [..]) or ('or', [..])."""
    tokens = tokenize(expr)
    pos = 0

    def peek():
        return tokens[pos] if pos < len(tokens) else None

    def take():
        nonlocal pos
        if pos >= len(tokens):
            raise SpdxError(f"unexpected end of {expr!r}")
        pos += 1
        return tokens[pos - 1]

    def atom():
        tok = take()
        if tok == "(":
            node = or_expr()
            if take() != ")":
                raise SpdxError(f"missing ) in {expr!r}")
            return node
        if tok in (")", "AND", "OR", "WITH"):
            raise SpdxError(f"unexpected {tok!r} in {expr!r}")
        ident = tok[:-1] if tok.endswith("+") else tok  # "MPL-2.0+" is "MPL-2.0 or later": the licence is MPL-2.0
        if peek() == "WITH":
            take()
            return ("with", ident, take())
        return ("id", ident)

    def and_expr():
        parts = [atom()]
        while peek() == "AND":
            take()
            parts.append(atom())
        return parts[0] if len(parts) == 1 else ("and", parts)

    def or_expr():
        parts = [and_expr()]
        while peek() == "OR":
            take()
            parts.append(and_expr())
        return parts[0] if len(parts) == 1 else ("or", parts)

    node = or_expr()
    if pos != len(tokens):
        raise SpdxError(f"trailing {tokens[pos]!r} in {expr!r}")
    return node


def allowed(node, policy):
    """True when the expression can be satisfied using only allowed licences (and exceptions)."""
    kind = node[0]
    if kind == "id":
        return node[1] in policy["allow"]
    if kind == "with":
        return node[1] in policy["allow"] and node[2] in policy["allow_exceptions"]
    if kind == "and":
        return all(allowed(n, policy) for n in node[1])
    return any(allowed(n, policy) for n in node[1])


# ── The crates of a build ───────────────────────────────────────────────────────

def load_metadata():
    """`cargo metadata` of the workspace with the `full` feature set, from Cargo.lock as it is."""
    out = subprocess.run(
        ["cargo", "metadata", "--format-version", "1", "--locked", "--features", FEATURES],
        cwd=REPO, check=True, capture_output=True, text=True,
    )
    return json.loads(out.stdout)


def shipped_crates(meta):
    """The third-party packages that are normal (not dev or build) dependencies of the root package, sorted."""
    packages = {p["id"]: p for p in meta["packages"]}
    nodes = {n["id"]: n for n in meta["resolve"]["nodes"]}
    workspace = set(meta["workspace_members"])
    root = next(i for i in workspace if packages[i]["name"] == ROOT_PACKAGE)
    seen, stack = set(), [root]
    while stack:
        pid = stack.pop()
        if pid in seen:
            continue
        seen.add(pid)
        for dep in nodes[pid]["deps"]:
            if any(k["kind"] is None for k in dep["dep_kinds"]):
                stack.append(dep["pkg"])
    crates = [packages[i] for i in seen if i not in workspace]
    return sorted(crates, key=lambda p: (p["name"], p["version"]))


def license_files(package, policy=None):
    """(file name, text) of the licence and notice files in the package's top directory, sorted by name.

    `policy["bundled_notices"][crate]` names more files, relative to the package, for a crate that bundles C sources
    under another licence than its own (libmimalloc-sys: Microsoft's mimalloc notice sits in `c_src/`; tikv-jemalloc-sys: jemalloc's COPYING sits in `jemalloc/`). The file must
    exist and stay inside the package: a stale entry fails the check instead of silently dropping a notice.
    """
    directory = os.path.dirname(package["manifest_path"])
    names = set()
    for name in os.listdir(directory):
        if LICENSE_FILE.match(name) and os.path.isfile(os.path.join(directory, name)):
            names.add(name)
    if package.get("license_file"):
        names.add(package["license_file"])
    for name in ((policy or {}).get("bundled_notices", {}).get(package["name"], [])):
        path = os.path.join(directory, name)
        inside = os.path.commonpath([os.path.realpath(directory), os.path.realpath(path)]) == os.path.realpath(directory)
        if not inside or not os.path.isfile(path):
            raise SystemExit(f"{package['name']}: ci/license-policy.json bundled_notices names {name}, which the package does not contain")
        names.add(name)
    found = []
    root = os.path.realpath(directory)
    for name in sorted(names):
        path = os.path.join(directory, name)
        # `license-file` comes from the crate's own Cargo.toml: an absolute path, `..` or a symlink must not make the
        # generator read (and publish) a file from outside the package
        if os.path.commonpath([root, os.path.realpath(path)]) != root:
            continue
        if not os.path.isfile(path) or os.path.getsize(path) > MAX_LICENSE_BYTES:
            continue
        with open(path, "rb") as f:
            text = f.read().decode("utf-8", errors="replace").replace("\r\n", "\n").strip("\n")
        if text.strip():
            found.append((os.path.basename(name), text))
    return found


def load_policy(path=POLICY):
    """The allow-list: `allow`, `allow_exceptions`, `clarify` (a per-crate licence for a crate whose own expression is missing or malformed, pinned to it) and `bundled_notices` (extra notice files inside a package)."""
    with open(path, encoding="utf-8") as f:
        policy = json.load(f)
    policy.setdefault("clarify", {})
    policy.setdefault("bundled_notices", {})
    return policy


def effective_license(package, policy):
    """The licence expression to judge: the crate's own, or the policy's clarification for it."""
    declared = package.get("license")
    clarified = policy["clarify"].get(package["name"])
    # a clarification is pinned to what the crate declares now (`declared`, absent when it declares nothing): when the
    # crate changes its licence the pin no longer matches, the clarification is ignored and the check fails again
    if clarified and clarified.get("declared") == declared:
        return clarified["license"]
    return declared


def violations(crates, policy):
    """One line per crate whose licence is missing, malformed or outside the allow-list."""
    bad = []
    for p in crates:
        expr = effective_license(p, policy)
        label = f"{p['name']} {p['version']}"
        if not expr:
            bad.append(f"{label}: no licence expression (add it under `clarify` in ci/license-policy.json after reading its licence file)")
            continue
        try:
            ok = allowed(parse(expr), policy)
        except SpdxError as e:
            bad.append(f"{label}: {e}")
            continue
        if not ok:
            bad.append(f"{label}: licence `{expr}` has no alternative inside the allow-list (ci/license-policy.json)")
    return bad


# ── The file ────────────────────────────────────────────────────────────────────

HEADER = """THIRD-PARTY LICENSES
====================

Conduit is licensed under the Apache License 2.0 (see LICENSE). The binaries of Conduit contain the open-source
crates listed below, each under its own licence. This file lists them with the licence and notice texts they ship, as
those licences require for a distribution in binary form.

It covers the largest build (`--features full`, every target platform); a smaller build (`standard`, `gateway`,
`static-server`, the default) contains a subset of these crates. It is generated from Cargo.lock by
scripts/third_party_licenses.py: do not edit it by hand (`python3 scripts/third_party_licenses.py --write
THIRD-PARTY-LICENSES.txt`).

Where a crate offers a choice (`MIT OR Apache-2.0`), Conduit uses it under either of the licences; the texts of all of
the licence files that the crate ships are included.
"""


def standard_text_id(expr):
    """The licence whose standard text goes with a crate that ships no licence file: the first of Apache-2.0, MIT and
    BSD-3-Clause that the expression names (None when it names none of them, e.g. only Zlib)."""
    ids = set(re.findall(r"[A-Za-z0-9.\-]+", expr.replace("/", " ")))
    for ident in ("Apache-2.0", "MIT", "BSD-3-Clause"):
        if ident in ids:
            return ident
    return None


def standard_text(ident):
    """The standard text of `ident` from ci/license-texts, or None."""
    if not ident:
        return None
    path = os.path.join(REPO, "ci", "license-texts", f"{ident}.txt")
    if not os.path.isfile(path):
        return None
    with open(path, encoding="utf-8") as f:
        return f.read().replace("\r\n", "\n").strip("\n")


def missing_standard_texts(crates, policy):
    """Crates that ship no licence file and whose licence has no standard text in ci/license-texts.

    Without this the notices would list such a crate with no licence text at all (a crate licensed only `Zlib` or
    `ISC`, say); the fix is a `<SPDX id>.txt` there and a line in `standard_text_id`.
    """
    problems = []
    for p in crates:
        if license_files(p, policy):
            continue
        expr = effective_license(p, policy) or "unknown"
        if not standard_text(standard_text_id(expr)):
            problems.append(
                f"{p['name']} {p['version']} [{expr}] ships no licence file and ci/license-texts has no standard text "
                "for its licence (add one and name it in standard_text_id)"
            )
    return problems


def render(crates, policy):
    """The text of the third-party licence file for `crates`."""
    notices = sorted(
        f"{p['name']} {p['version']}" for p in crates if any(n.lower().startswith("notice") for n, _ in license_files(p, policy))
    )
    note = (
        "NOTICE files (Apache-2.0 section 4(d)): the text of a crate's NOTICE file is reproduced below with its licence "
        "files, in this file, which is the documentation that travels with the binary. "
        + (f"Crates that ship one: {', '.join(notices)}." if notices else "No crate in this build ships a NOTICE file.")
    )
    out = [HEADER, "\n" + textwrap.fill(note, 118) + "\n", f"\nCRATES ({len(crates)})\n{'-' * 60}\n"]
    texts = {}  # sha256 -> (text, [crate labels], [file names])
    no_file = []
    for p in crates:
        expr = effective_license(p, policy) or "unknown"
        repo = p.get("repository") or p.get("homepage") or "(no repository given)"
        label = f"{p['name']} {p['version']}"
        out.append(f"{label}  [{expr}]  {repo}\n")
        files = license_files(p, policy)
        if not files:
            no_file.append((label, expr, repo))
        for name, text in files:
            key = hashlib.sha256(text.encode("utf-8")).hexdigest()
            entry = texts.setdefault(key, (text, [], set()))
            entry[1].append(label)
            entry[2].add(name)
    if no_file:
        out.append(f"\nCRATES WITHOUT A LICENCE FILE IN THEIR PACKAGE ({len(no_file)})\n{'-' * 60}\n")
        out.append(
            "The licence is the one the SPDX identifier names; the copyright holders are named in the repository. "
            "The standard text of that licence follows each group.\n"
        )
        groups = {}
        for label, expr, repo in no_file:
            groups.setdefault(standard_text_id(expr), []).append((label, expr, repo))
        for ident, members in sorted(groups.items(), key=lambda kv: str(kv[0])):
            out.append("\n")
            for label, expr, repo in members:
                out.append(f"{label}  [{expr}]  {repo}\n")
            text = standard_text(ident)
            if text:
                out.append(f"\nStandard {ident} text for the crates above:\n\n{text}\n")
    out.append(f"\nLICENCE AND NOTICE TEXTS ({len(texts)})\n{'-' * 60}\n")
    for _, (text, labels, names) in sorted(texts.items(), key=lambda kv: (sorted(kv[1][1])[0], kv[0])):
        out.append("\n" + "=" * 78 + "\n")
        out.append(f"Applies to: {', '.join(sorted(set(labels)))}\n")
        out.append(f"Files: {', '.join(sorted(names))}\n")
        out.append("=" * 78 + "\n\n")
        out.append(text + "\n")
    return "".join(out)


TECHNOLOGIES = os.path.join(REPO, "docs", "technologies.md")
TECH_ROW = re.compile(r"^\|\s*`([^`]+)`\s*\|.*\|\s*([^|]+?)\s*\|\s*$")


def technology_problems(path, crates, policy):
    """Rows of the hand-written docs/technologies.md whose crate or licence differs from what Cargo.lock says.

    A row starts with the crate name in backticks and ends with its SPDX expression; a crate that has several
    versions in the lock matches when any of them carries that expression.
    """
    expressions = {}
    for p in crates:
        expressions.setdefault(p["name"], set()).add(effective_license(p, policy))
    problems = []
    try:
        with open(path, encoding="utf-8") as f:
            lines = f.read().splitlines()
    except FileNotFoundError:
        return [f"{path} is missing"]
    for number, line in enumerate(lines, 1):
        row = TECH_ROW.match(line)
        if not row or row.group(1) == "Crate":
            continue
        name, licence = row.groups()
        if name not in expressions:
            problems.append(f"line {number}: `{name}` is not a dependency in any shipped build")
        elif licence not in expressions[name]:
            problems.append(f"line {number}: `{name}` is {' / '.join(sorted(map(str, expressions[name])))} in Cargo.lock, not {licence}")
    return problems


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    group = ap.add_mutually_exclusive_group(required=True)
    group.add_argument("--write", metavar="FILE", help="write the licence file")
    group.add_argument("--check", metavar="FILE", help="check the policy and that FILE is current")
    group.add_argument("--policy", action="store_true", help="check the policy only")
    args = ap.parse_args(argv)

    policy = load_policy()
    crates = shipped_crates(load_metadata())
    bad = violations(crates, policy) + missing_standard_texts(crates, policy)
    for line in bad:
        print(f"licence policy: {line}", file=sys.stderr)
    if bad:
        return 1
    print(f"licence policy: {len(crates)} third-party crates, all inside the allow-list")
    if args.policy:
        return 0
    text = render(crates, policy)
    if args.write:
        with open(args.write, "w", encoding="utf-8", newline="\n") as f:
            f.write(text)
        print(f"wrote {args.write} ({len(text)} bytes)")
        return 0
    try:
        with open(args.check, encoding="utf-8", newline="") as f:
            current = f.read()
    except FileNotFoundError:
        current = None
    if current != text:
        print(
            f"{args.check} is not what Cargo.lock produces now: regenerate it with\n"
            f"    python3 scripts/third_party_licenses.py --write {args.check}",
            file=sys.stderr,
        )
        return 1
    print(f"{args.check} is current")
    problems = technology_problems(TECHNOLOGIES, crates, policy)
    for line in problems:
        print(f"{os.path.relpath(TECHNOLOGIES, REPO)}: {line}", file=sys.stderr)
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
