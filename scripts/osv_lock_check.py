"""Query OSV for every crates.io package in a Cargo.lock and list the ones with advisories.

A quick local answer to "what would osv-scanner say about this lock, and what did a dependency change
fix?" without the scanner or a CI run. Compare two locks by giving both (for example the lock on `main` from
`git show main:Cargo.lock > /tmp/main.lock` and the working tree's).

    python scripts/osv_lock_check.py Cargo.lock [more.lock ...]

It sends package names and versions to https://api.osv.dev (nothing else) and prints, per lock, the number
of crates.io packages scanned, the packages with advisories and their RUSTSEC ids. "Unmaintained" notices
are advisories too; read the ids. Packages from any other source (an alternative registry, `sparse+`, `git+`)
are not looked up, because OSV would answer for the crates.io package of the same name: they are counted and
listed as NOT SCANNED. Needs Python 3.11+ (tomllib). Read-only.
"""
import json
import sys
import tomllib
import urllib.request

CRATES_IO = "registry+https://github.com/rust-lang/crates.io-index"
BATCH = 500


def query_batch(queries):
    body = json.dumps({"queries": queries}).encode()
    req = urllib.request.Request("https://api.osv.dev/v1/querybatch", data=body, headers={"Content-Type": "application/json"})
    return json.load(urllib.request.urlopen(req, timeout=120))["results"]


def advisories(pkgs):
    """Map (name, version) -> sorted advisory ids, following OSV's page tokens until none is left."""
    found = {}
    pending = [({"package": {"name": n, "ecosystem": "crates.io"}, "version": v}, (n, v)) for n, v in pkgs]
    while pending:
        retry = []
        for i in range(0, len(pending), BATCH):
            chunk = pending[i:i + BATCH]
            results = query_batch([q for q, _ in chunk])
            for (query, key), result in zip(chunk, results, strict=True):
                ids = found.setdefault(key, set())
                ids.update(x["id"] for x in result.get("vulns", []))
                token = result.get("next_page_token")
                if token:
                    retry.append(({**query, "page_token": token}, key))
        pending = retry
    return {k: sorted(v) for k, v in found.items() if v}


for path in sys.argv[1:]:
    with open(path, "rb") as f:
        lock = tomllib.load(f)
    scanned = [(p["name"], p["version"]) for p in lock["package"] if p.get("source") == CRATES_IO]
    # Every package that has a source and is not on crates.io: another registry (registry+ or sparse+) or git+.
    # Workspace members have no source and are ours, not dependencies.
    other = [(p["name"], p["version"], p["source"]) for p in lock["package"] if p.get("source") and p["source"] != CRATES_IO]
    found = advisories(scanned)
    total = sum(len(v) for v in found.values())
    print(f"{path}: {len(scanned)} crates.io packages, {len(found)} with advisories, {total} advisory hits")
    for (n, v), ids in sorted(found.items()):
        print(f"  {n} {v}: {len(ids)}  {', '.join(i for i in ids if i.startswith('RUSTSEC'))}")
    if other:
        print(f"  NOT SCANNED ({len(other)} from another registry or a git source):")
        for n, v, src in sorted(other):
            print(f"    {n} {v} ({src})")
