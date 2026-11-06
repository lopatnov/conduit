#!/usr/bin/env python3
"""Format the performance report comment from the numbers the `performance` job measured (#461).

Runs in the write-token `comment` job, so every input is treated as data: numbers are parsed with float() and
dropped if they are not finite, and the runner description is reduced to printable ASCII and cut short.

Usage: performance_report.py OUT_FILE
Env:   HEAD_RPS HEAD_P50 HEAD_P99 HEAD_OK  BASE_RPS BASE_P50 BASE_P99 BASE_OK  (one worker thread; means of the runs)
       HEAD_RPS_SEM HEAD_P50_SEM HEAD_P99_SEM  BASE_RPS_SEM BASE_P50_SEM BASE_P99_SEM  (standard error of those
       means, in percent of the mean; without them a difference is judged by its size alone)
       HEAD_W4_* and BASE_W4_* with the same suffixes  (four worker threads; the section is left out without them)
       HW  LB

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
    "_Reverse-proxy passthrough, `default` feature profile, `oha -z 10s -c 50` — informational only, not a merge "
    "gate; single shared CI runner, treat as a trend signal, not a lab measurement (see #475 and "
    "`docs/benchmarks.md`). Each number is the mean of four runs per build, taken alternately (head, base, base, "
    "head, head, base, base, head) so that a slow start or a drifting runner hits both builds alike. One 10-second "
    "run on a shared runner scatters by several percent, so the Δ column says whether a difference is larger than "
    "the noise (2.5 times the standard error of the difference of the two means, from the scatter of the runs); "
    "only a difference beyond the noise is a finding._"
)

ONE_WORKER_TITLE = "### One worker thread (the default: `global.workers` unset)"
FOUR_WORKERS_TITLE = "### Four worker threads (`global.workers: 4`)"
FOUR_WORKERS_NOTE = (
    "_The runner has four vCPUs and the load generator and the upstream run on them too, so this is how the build "
    "behaves when its workers compete for the machine, not what four free cores would give._"
)


LB_STRATEGIES = ("round-robin", "least-conn", "p2c", "least-bandwidth")
LB_SCENARIOS = (
    ("even", "four equal upstreams: what a strategy costs when there is nothing to be clever about"),
    ("slow", "upstream 0 takes 8 ms per request, the other three answer at once: what a strategy does about a slow upstream"),
)
LB_INTRO = (
    "_The same load through **four local upstreams** with each load-balancing strategy, `oha -z 5s -c 50`, "
    "one worker thread, base and head measured back to back. Each cell is a single run that is not repeated, so "
    "a base-to-head difference is flagged only above 25%; the strategies compared with each other are the point "
    "of these tables. A strategy the base commit does not have shows `n/a` on the base side. The last column is "
    "each upstream's share of the requests on the head build._"
)

# A single 5-second run scatters by ten percent or more between base and head: flag only a large drop.
LB_FLAG_AT = 25.0
# The passthrough tables repeat each measurement, so they can tell noise from a change: an adverse difference
# beyond the noise is flagged from this size on.
FLAG_BEYOND_NOISE_AT = 3.0
FLAG_UNKNOWN_NOISE_AT = 10.0
# A difference counts as noise below this many standard errors of the difference. With four runs per build
# (about six degrees of freedom) the 95% critical value of Student's t is 2.45, so 2.5 keeps it honest.
NOISE_FACTOR = 2.5
# The largest standard error of a mean, in percent of the mean, that is believed (see `metrics`).
MAX_SEM = 100.0


def number(raw):
    try:
        v = float(raw)
    except (TypeError, ValueError, OverflowError):  # OverflowError: a 400-digit integer
        return None
    return v if math.isfinite(v) else None


def metrics(env, prefix):
    """The measured numbers of one build and worker setting. `<k>_sem` is the standard error of the mean of the
    runs, as a percentage of that mean: the scatter the Δ column is judged against."""
    keys = ("rps", "p50", "p99", "ok", "rps_sem", "p50_sem", "p99_sem")
    out = {k: number(env.get(f"{prefix}_{k.upper()}", "")) for k in keys}
    for k in ("rps_sem", "p50_sem", "p99_sem"):
        # A standard error above the mean itself says nothing, and an absurd one (1e308) would turn every row
        # into "within noise": treat it as absent, so the difference is judged by its size.
        if out[k] is not None and not 0 <= out[k] <= MAX_SEM:
            out[k] = None
    return out


def noise(head, base, key):
    """One standard error of (head - base), in percent, or None when either side has no scatter estimate."""
    h, b = head.get(f"{key}_sem"), base.get(f"{key}_sem")
    if h is None or b is None:  # `metrics` has already dropped negative and absurd values
        return None
    return math.hypot(h, b)


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
    except (ValueError, RecursionError):  # RecursionError: thousands of nested brackets
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
                rps = f"{b['rps']:.0f} → {h['rps']:.0f}, {delta(h, b, 'rps', True, LB_FLAG_AT)}"
                p99 = f"{b['p99']:.2f} → {h['p99']:.2f}, {delta(h, b, 'p99', False, LB_FLAG_AT)}"
            shares = " / ".join(f"{x:.0f}" for x in h["share"]) or "—"
            lines.append(f"{strategy} | {rps} | {p99} | {shares}")
    return lines


def delta(head, base, key, higher_is_better, flag_at=FLAG_UNKNOWN_NOISE_AT):
    """The head-against-base difference of `key`, as text.

    With a scatter estimate for both sides (the repeated passthrough runs) the text says whether the difference is
    beyond the noise, and only an adverse difference beyond the noise is flagged. Without one (a single run), an
    adverse difference of `flag_at` percent or more is flagged."""
    if base[key] is None or base[key] == 0 or head[key] is None:
        return "—"
    pct = (head[key] - base[key]) / base[key] * 100
    arrow = "▲" if pct > 0 else ("▼" if pct < 0 else "—")
    good = (pct > 0) == higher_is_better
    sign = "+" if pct > 0 else ""
    n = noise(head, base, key)
    if n is None:
        flag = " ⚠️" if abs(pct) >= flag_at and not good else ""
        return f"{arrow} {sign}{pct:.1f}%{flag}"
    if abs(pct) <= NOISE_FACTOR * n:  # `<=`: with no measured scatter at all, an exact tie is still noise
        return f"{arrow} {sign}{pct:.1f}% (within noise, ±{n:.1f}%)"
    flag = " ⚠️" if abs(pct) >= FLAG_BEYOND_NOISE_AT and not good else ""
    return f"{arrow} {sign}{pct:.1f}%{flag} (beyond noise, ±{n:.1f}%)"


def fmt(m, k, spec, unit=""):
    return "n/a" if m[k] is None else f"{m[k]:{spec}}{unit}"


def passthrough_lines(head, base, note_if_no_base):
    """The table for one worker setting: base against head, or head alone when there is no base measurement."""
    if base["rps"] is None:
        lines = [
            "Metric | PR head",
            "---|---",
            f"Requests/sec | {fmt(head, 'rps', '.0f')}",
            f"p50 latency | {fmt(head, 'p50', '.2f', ' ms')}",
            f"p99 latency | {fmt(head, 'p99', '.2f', ' ms')}",
            f"Success rate | {fmt(head, 'ok', '.1f', '%')}",
        ]
        if note_if_no_base:
            lines += ["", "_No base commit measured (couldn't resolve this PR's base SHA) — nothing to diff against._"]
        return lines
    return [
        "Metric | Base | PR head | Δ",
        "---|---|---|---",
        f"Requests/sec | {fmt(base, 'rps', '.0f')} | {fmt(head, 'rps', '.0f')} | {delta(head, base, 'rps', True)}",
        f"p50 latency | {fmt(base, 'p50', '.2f', ' ms')} | {fmt(head, 'p50', '.2f', ' ms')} | {delta(head, base, 'p50', False)}",
        f"p99 latency | {fmt(base, 'p99', '.2f', ' ms')} | {fmt(head, 'p99', '.2f', ' ms')} | {delta(head, base, 'p99', False)}",
        f"Success rate | {fmt(base, 'ok', '.1f', '%')} | {fmt(head, 'ok', '.1f', '%')} | {delta(head, base, 'ok', True)}",
    ]


def render(env):
    head, base = metrics(env, "HEAD"), metrics(env, "BASE")
    hw = re.sub(r"[^\x20-\x7e]", "", " ".join(env.get("HW", "").split()))[:300]
    lines = [MARKER, "", "## ⚡ Performance report", "", INTRO, ""]

    if head["rps"] is None:
        lines.append("_The head build produced no usable measurement; see the job log._")
        return "\n".join(lines) + "\n"

    lines += [ONE_WORKER_TITLE, ""] + passthrough_lines(head, base, True)
    head4, base4 = metrics(env, "HEAD_W4"), metrics(env, "BASE_W4")
    if head4["rps"] is not None:  # absent when the four-worker run failed or the job is older than this section
        lines += ["", FOUR_WORKERS_TITLE, "", FOUR_WORKERS_NOTE, ""] + passthrough_lines(head4, base4, False)

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
