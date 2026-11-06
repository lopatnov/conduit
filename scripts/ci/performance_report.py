#!/usr/bin/env python3
"""Format the performance report comment from the numbers the `performance` job measured (#461).

Runs in the write-token `comment` job, so every input is treated as data: numbers are parsed with float() and
dropped if they are not finite, and the runner description is reduced to printable ASCII and cut short.

Usage: performance_report.py OUT_FILE
Env:   HEAD_RPS HEAD_P50 HEAD_P99 HEAD_OK  BASE_RPS BASE_P50 BASE_P99 BASE_OK  HW  LB

`LB` is the JSON of scripts/ci/bench_lb.sh for both builds, {"head": {...}, "base": {...}}, each with the
scenarios `even` and `slow` and per strategy {"rps", "p50", "p99", "ok", "share": [..]} or null (issue #629).
It is parsed defensively: only known scenario and strategy names are read, every number goes through
float() and is dropped if it is not finite, and anything malformed leaves the section out.
"""
import json
import math
import os
import re
import sys

MARKER = "<!-- performance-report -->"

INTRO = (
    "_Reverse-proxy passthrough, `default` feature profile, **one worker thread (the default; "
    "`global.workers` unset)**, `oha -z 10s -c 50` — informational only, not a merge gate; single shared CI "
    "runner, treat as a trend signal, not a lab measurement (see #475 and `docs/benchmarks.md`)._"
)


LB_STRATEGIES = ("round-robin", "least-conn", "p2c", "least-bandwidth")
LB_SCENARIOS = (
    ("even", "four equal upstreams: what a strategy costs when there is nothing to be clever about"),
    ("slow", "upstream 0 takes 8 ms per request, the other three answer at once: what a strategy does about a slow upstream"),
)
LB_INTRO = (
    "_The same load through **four local upstreams** with each load-balancing strategy, `oha -z 5s -c 50`, "
    "one worker thread, base and head measured back to back. A strategy the base commit does not have shows "
    "`n/a` on the base side. The last column is each upstream's share of the requests on the head build._"
)


def number(raw):
    try:
        v = float(raw)
    except (TypeError, ValueError):
        return None
    return v if math.isfinite(v) else None


def metrics(env, prefix):
    return {k: number(env.get(f"{prefix}_{k.upper()}", "")) for k in ("rps", "p50", "p99", "ok")}


def lb_cell(raw):
    """One {"rps", "p99", "share"} cell, or None for anything that is not a usable measurement."""
    if not isinstance(raw, dict):
        return None
    cell = {k: number(raw.get(k)) for k in ("rps", "p50", "p99")}
    if cell["rps"] is None or cell["p99"] is None:
        return None
    share = raw.get("share")
    shares = []
    if isinstance(share, list):
        for x in share[:8]:
            v = number(x)
            if v is None or not 0 <= v <= 100:
                shares = []
                break
            shares.append(v)
    cell["share"] = shares
    return cell


def parse_lb(raw):
    """{"head"|"base": {scenario: {strategy: cell | None}}} from the job output; {} if it is unusable."""
    try:
        data = json.loads(raw) if isinstance(raw, str) and raw.strip() else {}
    except ValueError:
        return {}
    if not isinstance(data, dict):
        return {}
    out = {}
    for side in ("head", "base"):
        scenarios = data.get(side)
        out[side] = {}
        if not isinstance(scenarios, dict):
            continue
        for name, _ in LB_SCENARIOS:
            cells = scenarios.get(name)
            if isinstance(cells, dict):
                out[side][name] = {s: lb_cell(cells.get(s)) for s in LB_STRATEGIES}
    return out


def render_lb(lb):
    """The load-balancing tables, or [] when the head build has no usable cell."""
    head = lb.get("head", {})
    if not any(c for cells in head.values() for c in cells.values()):
        return []
    base = lb.get("base", {})
    lines = ["", "### Load balancing across four upstreams", "", LB_INTRO]
    for name, what in LB_SCENARIOS:
        if name not in head:
            continue
        lines += ["", f"**{name}** — {what}", "", "Strategy | Requests/sec (base → head, Δ) | p99 ms (base → head, Δ) | Share per upstream %", "---|---|---|---"]
        for strategy in LB_STRATEGIES:
            h = head[name].get(strategy)
            b = base.get(name, {}).get(strategy)
            if h is None:
                lines.append(f"{strategy} | — | — | —")
                continue
            if b is None:
                rps, p99 = f"n/a → {h['rps']:.0f}", f"n/a → {h['p99']:.2f}"
            else:
                rps = f"{b['rps']:.0f} → {h['rps']:.0f}, {delta(h, b, 'rps', True)}"
                p99 = f"{b['p99']:.2f} → {h['p99']:.2f}, {delta(h, b, 'p99', False)}"
            shares = " / ".join(f"{x:.0f}" for x in h["share"]) or "—"
            lines.append(f"{strategy} | {rps} | {p99} | {shares}")
    return lines


def delta(head, base, key, higher_is_better):
    if base[key] is None or base[key] == 0 or head[key] is None:
        return "—"
    pct = (head[key] - base[key]) / base[key] * 100
    arrow = "▲" if pct > 0 else ("▼" if pct < 0 else "—")
    good = (pct > 0) == higher_is_better
    sign = "+" if pct > 0 else ""
    flag = " ⚠️" if abs(pct) >= 10 and not good else ""
    return f"{arrow} {sign}{pct:.1f}%{flag}"


def render(env):
    head, base = metrics(env, "HEAD"), metrics(env, "BASE")
    hw = re.sub(r"[^\x20-\x7e]", "", " ".join(env.get("HW", "").split()))[:300]
    lines = [MARKER, "", "## ⚡ Performance report", "", INTRO, ""]

    if head["rps"] is None:
        lines.append("_The head build produced no usable measurement; see the job log._")
        return "\n".join(lines) + "\n"

    def fmt(m, k, spec, unit=""):
        return "n/a" if m[k] is None else f"{m[k]:{spec}}{unit}"

    if base["rps"] is None:
        lines += [
            "Metric | PR head",
            "---|---",
            f"Requests/sec | {fmt(head, 'rps', '.0f')}",
            f"p50 latency | {fmt(head, 'p50', '.2f', ' ms')}",
            f"p99 latency | {fmt(head, 'p99', '.2f', ' ms')}",
            f"Success rate | {fmt(head, 'ok', '.1f', '%')}",
            "",
            "_No base commit measured (couldn't resolve this PR's base SHA) — nothing to diff against._",
        ]
    else:
        lines += [
            "Metric | Base | PR head | Δ",
            "---|---|---|---",
            f"Requests/sec | {fmt(base, 'rps', '.0f')} | {fmt(head, 'rps', '.0f')} | {delta(head, base, 'rps', True)}",
            f"p50 latency | {fmt(base, 'p50', '.2f', ' ms')} | {fmt(head, 'p50', '.2f', ' ms')} | {delta(head, base, 'p50', False)}",
            f"p99 latency | {fmt(base, 'p99', '.2f', ' ms')} | {fmt(head, 'p99', '.2f', ' ms')} | {delta(head, base, 'p99', False)}",
            f"Success rate | {fmt(base, 'ok', '.1f', '%')} | {fmt(head, 'ok', '.1f', '%')} | {delta(head, base, 'ok', True)}",
        ]

    if hw:
        if base["rps"] is not None:
            note = (
                "GitHub-hosted runner hardware varies between runs, so read the Δ column (base and head are "
                "measured back-to-back on this same machine) rather than comparing absolute numbers across PRs."
            )
        else:
            note = "GitHub-hosted runner hardware varies between runs, so absolute numbers are not comparable across PRs."
        lines += ["", f"**Runner:** {hw}. " + note]
    lines += render_lb(parse_lb(env.get("LB", "")))
    return "\n".join(lines) + "\n"


def main():
    with open(sys.argv[1], "w", encoding="utf-8") as f:
        f.write(render(os.environ))


if __name__ == "__main__":
    main()
