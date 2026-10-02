#!/usr/bin/env bash
# Per-crate .text difference between Conduit --no-default-features and the smallest Pingora proxy (same profile). Run after size.sh.
set -uo pipefail
export CARGO_PROFILE_RELEASE_STRIP=none
cd ~/perf/size && cargo bloat --release --no-default-features -p lopatnov-conduit --crates -n 300 > ~/perf/bloat_conduit.txt 2>/dev/null
export CARGO_PROFILE_RELEASE_LTO=true CARGO_PROFILE_RELEASE_CODEGEN_UNITS=1
cd ~/perf/pp_fat && cargo bloat --release --crates -n 300 > ~/perf/bloat_pp.txt 2>/dev/null
python3 - <<'PY'
import re, os
def load(p):
    d, total = {}, 0
    for line in open(os.path.expanduser(p)):
        m = re.match(r"\s*[\d.]+%\s+[\d.]+%\s+([\d.]+)(KiB|MiB)\s+(\S+)", line)
        if m:
            v = float(m.group(1)) * (1024 if m.group(2) == "MiB" else 1)
            d[m.group(3)] = d.get(m.group(3), 0) + v
        t = re.search(r"([\d.]+)(KiB|MiB)\s+\.text section size", line)
        if t:
            total = float(t.group(1)) * (1024 if t.group(2) == "MiB" else 1)
    return d, total
c, ct = load("~/perf/bloat_conduit.txt")
p, pt = load("~/perf/bloat_pp.txt")
print(f".text: conduit {ct/1024:.2f} MiB, smallest pingora proxy {pt/1024:.2f} MiB, diff {(ct-pt)/1024:.2f} MiB")
own = sum(v for k, v in c.items() if k.startswith("conduit"))
print(f"conduit_* crates (ours): {own/1024:.2f} MiB")
rows = sorted(((c.get(k, 0) - p.get(k, 0), k, c.get(k, 0), p.get(k, 0)) for k in set(c) | set(p)), reverse=True)
print(f"{'crate':28s} {'conduit':>9s} {'pingora':>9s} {'diff':>9s}  (KiB)")
for d, k, a, b in rows[:28]:
    print(f"{k:28s} {a:9.0f} {b:9.0f} {d:9.0f}")
PY
