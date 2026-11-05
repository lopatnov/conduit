"""Query OSV for every registry package in a Cargo.lock and list the ones with advisories.

A quick local answer to "what would osv-scanner say about this lock, and what did a dependency change
fix?" without the scanner or a CI run. Compare two locks by giving both (for example the lock on `main` from
`git show main:Cargo.lock > /tmp/main.lock` and the working tree's).

    python scripts/osv_lock_check.py Cargo.lock [more.lock ...]

It sends package names and versions to https://api.osv.dev (nothing else) and prints, per lock, the number
of registry packages scanned, the packages with advisories and their RUSTSEC ids. "Unmaintained" notices
are advisories too; read the ids. Needs Python 3.11+ (tomllib). Read-only.
"""
import json
import sys
import tomllib
import urllib.request

for path in sys.argv[1:]:
    with open(path, "rb") as f:
        lock = tomllib.load(f)
    pkgs = [(p["name"], p["version"]) for p in lock["package"] if str(p.get("source", "")).startswith("registry+")]
    found = {}
    for i in range(0, len(pkgs), 500):
        chunk = pkgs[i:i + 500]
        body = json.dumps({"queries": [{"package": {"name": n, "ecosystem": "crates.io"}, "version": v} for n, v in chunk]}).encode()
        req = urllib.request.Request("https://api.osv.dev/v1/querybatch", data=body, headers={"Content-Type": "application/json"})
        res = json.load(urllib.request.urlopen(req, timeout=120))
        for (n, v), r in zip(chunk, res["results"]):
            ids = sorted({x["id"] for x in r.get("vulns", [])})
            if ids:
                found[(n, v)] = ids
    total = sum(len(v) for v in found.values())
    print(f"{path}: {len(pkgs)} registry packages, {len(found)} with advisories, {total} advisory hits")
    for (n, v), ids in sorted(found.items()):
        print(f"  {n} {v}: {len(ids)}  {', '.join(i for i in ids if i.startswith('RUSTSEC'))[:120]}")
