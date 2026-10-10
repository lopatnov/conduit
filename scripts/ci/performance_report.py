#!/usr/bin/env python3
"""Format the performance report comment from the numbers the `performance` job measured (#461).

Runs in the write-token `comment` job, so every input is treated as data: numbers are parsed with float() and
dropped if they are not finite, and the runner description is reduced to printable ASCII and cut short.

Usage: performance_report.py OUT_FILE
Env:   HEAD_RPS HEAD_P50 HEAD_P99 HEAD_OK  BASE_RPS BASE_P50 BASE_P99 BASE_OK  HW
"""
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


def number(raw):
    try:
        v = float(raw)
    except (TypeError, ValueError):
        return None
    return v if math.isfinite(v) else None


def metrics(env, prefix):
    return {k: number(env.get(f"{prefix}_{k.upper()}", "")) for k in ("rps", "p50", "p99", "ok")}


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
    return "\n".join(lines) + "\n"


def main():
    with open(sys.argv[1], "w", encoding="utf-8") as f:
        f.write(render(os.environ))


if __name__ == "__main__":
    main()
