#!/usr/bin/env python3
"""Format the performance report comment from the numbers the `performance` job measured (#461).

Runs in the write-token `comment` job, so every input is treated as data: numbers are parsed with float() and
dropped if they are not finite, and the runner description is reduced to printable ASCII and cut short.

Usage: performance_report.py OUT_FILE
Env:   HEAD_RPS HEAD_P50 HEAD_P99 HEAD_OK  BASE_RPS BASE_P50 BASE_P99 BASE_OK  (one worker thread; means of the runs)
       HEAD_RPS_SEM HEAD_P50_SEM HEAD_P99_SEM  BASE_RPS_SEM BASE_P50_SEM BASE_P99_SEM  (standard error of those
       means, in percent of the mean; without them a difference is judged by its size alone)
       HEAD_W4_* and BASE_W4_* with the same suffixes  (four worker threads; the section is left out without them)
       HEAD_LB_* and BASE_LB_* with the same suffixes  (round-robin over four upstreams, four worker threads; the
       section is left out without them)
       HW
"""
import math
import os
import re
import sys

MARKER = "<!-- performance-report -->"

INTRO = (
    "_Reverse-proxy passthrough, `oha -z 10s -c 50`; informational, not a merge gate. Each number is the mean of four "
    "runs per build, taken alternately (head, base, base, head, ...). The Δ column is judged against the run-to-run "
    "noise (2.5 standard errors of the difference; see `docs/benchmarks.md`): only a difference beyond the noise is a "
    "finding._"
)

ONE_WORKER_TITLE = "### One worker thread (the default: `global.workers` unset)"
FOUR_WORKERS_TITLE = "### Four worker threads (`global.workers: 4`)"
FOUR_WORKERS_NOTE = "_The load generator and the upstream share the runner's four vCPUs with the workers._"
LB_TITLE = "### Load balancing: `round-robin` over four upstreams, four worker threads"
LB_NOTE = (
    "_The same load through four upstreams instead of one, so the choice among them is on the path. Everything shares "
    "the runner's four vCPUs: read base against head, not these numbers against the tables above._"
)


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


def delta(head, base, key, higher_is_better):
    """The head-against-base difference of `key`, as text.

    With a scatter estimate for both sides (the repeated passthrough runs) the text says whether the difference is
    beyond the noise, and only an adverse difference beyond the noise is flagged. Without one (a single run), an
    adverse difference of FLAG_UNKNOWN_NOISE_AT percent or more is flagged."""
    if base[key] is None or base[key] == 0 or head[key] is None:
        return "—"
    pct = (head[key] - base[key]) / base[key] * 100
    arrow = "▲" if pct > 0 else ("▼" if pct < 0 else "—")
    good = (pct > 0) == higher_is_better
    sign = "+" if pct > 0 else ""
    n = noise(head, base, key)
    if n is None:
        flag = " ⚠️" if abs(pct) >= FLAG_UNKNOWN_NOISE_AT and not good else ""
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
        ]
        if head["ok"] is not None and head["ok"] < 100:
            lines.append(f"Success rate | {fmt(head, 'ok', '.1f', '%')}")
        if note_if_no_base:
            lines += ["", "_No base commit measured (couldn't resolve this PR's base SHA) — nothing to diff against._"]
        return lines
    lines = [
        "Metric | Base | PR head | Δ",
        "---|---|---|---",
        f"Requests/sec | {fmt(base, 'rps', '.0f')} | {fmt(head, 'rps', '.0f')} | {delta(head, base, 'rps', True)}",
        f"p50 latency | {fmt(base, 'p50', '.2f', ' ms')} | {fmt(head, 'p50', '.2f', ' ms')} | {delta(head, base, 'p50', False)}",
        f"p99 latency | {fmt(base, 'p99', '.2f', ' ms')} | {fmt(head, 'p99', '.2f', ' ms')} | {delta(head, base, 'p99', False)}",
    ]
    # Every request succeeds in a healthy run; a row for it is shown only when one side did not.
    if (head["ok"] is not None and head["ok"] < 100) or (base["ok"] is not None and base["ok"] < 100):
        lines.append(f"Success rate | {fmt(base, 'ok', '.1f', '%')} | {fmt(head, 'ok', '.1f', '%')} | {delta(head, base, 'ok', True)}")
    return lines


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
    head_lb, base_lb = metrics(env, "HEAD_LB"), metrics(env, "BASE_LB")
    if head_lb["rps"] is not None:  # absent when the balancer run failed or the job is older than this section
        lines += ["", LB_TITLE, "", LB_NOTE, ""] + passthrough_lines(head_lb, base_lb, False)

    if hw:
        if base["rps"] is not None:
            note = "Hardware varies between runs: read the Δ column (base and head ran back to back here), not absolute numbers."
        else:
            note = "Hardware varies between runs: absolute numbers are not comparable across PRs."
        lines += ["", f"**Runner:** {hw}. " + note]
    return "\n".join(lines) + "\n"


def main():
    with open(sys.argv[1], "w", encoding="utf-8") as f:
        f.write(render(os.environ))


if __name__ == "__main__":
    main()
