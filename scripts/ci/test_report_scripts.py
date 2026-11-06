#!/usr/bin/env python3
"""Unit tests for scripts/ci/{build_report_body,performance_report}.py and resolve_pr.sh (#461).

The comment jobs treat the measured text as untrusted data; these tests pin what they do with hostile input.

Run:  python3 scripts/ci/test_report_scripts.py
"""
import importlib.util
import os
import subprocess
import unittest

_here = os.path.dirname(os.path.abspath(__file__))


def load(name):
    spec = importlib.util.spec_from_file_location(name, os.path.join(_here, f"{name}.py"))
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


body = load("build_report_body")
perf = load("performance_report")
MARKER = "<!-- footprint-report -->"


class BuildBody(unittest.TestCase):
    def test_marker_and_heading_come_first(self):
        out = body.build(MARKER, "table", "## Footprint report")
        self.assertTrue(out.startswith(MARKER + "\n\n## Footprint report\n\ntable"))

    def test_a_marker_inside_the_report_is_removed(self):
        out = body.build(MARKER, f"x {MARKER} y")
        self.assertEqual(out.count(MARKER), 1)

    def test_oversized_reports_are_cut_below_the_comment_limit(self):
        out = body.build(MARKER, "a" * 200_000, "## H")
        self.assertLess(len(out), 65_536)
        self.assertIn("truncated", out)

    def test_a_small_report_is_untouched(self):
        self.assertIn("\nrow | 1\n", body.build(MARKER, "row | 1"))


class Performance(unittest.TestCase):
    ENV = {"HEAD_RPS": "1000", "HEAD_P50": "1.5", "HEAD_P99": "2.5", "HEAD_OK": "100",
           "BASE_RPS": "800", "BASE_P50": "1.6", "BASE_P99": "2.6", "BASE_OK": "100",
           "HW": "4 vCPU, 16 GB RAM"}

    def test_full_report_has_the_marker_and_a_delta(self):
        out = perf.render(self.ENV)
        self.assertTrue(out.startswith(perf.MARKER))
        self.assertIn("▲ +25.0%", out)
        self.assertIn("**Runner:** 4 vCPU, 16 GB RAM.", out)

    def test_missing_base_prints_head_only(self):
        env = {k: v for k, v in self.ENV.items() if not k.startswith("BASE_")}
        out = perf.render(env)
        self.assertIn("Metric | PR head", out)
        self.assertIn("nothing to diff against", out)

    def test_non_numeric_and_non_finite_values_are_dropped(self):
        env = dict(self.ENV, HEAD_RPS="<script>", BASE_RPS="nan")
        out = perf.render(env)
        self.assertIn("no usable measurement", out)
        self.assertNotIn("<script>", out)

    def test_runner_text_is_reduced_to_printable_ascii_and_cut(self):
        out = perf.render(dict(self.ENV, HW="evil\x00\u202e" + "x" * 1000))
        self.assertNotIn("\x00", out)
        self.assertNotIn("\u202e", out)
        self.assertLess(len(out), 2000)

    def test_a_regression_of_ten_percent_is_flagged(self):
        out = perf.render(dict(self.ENV, HEAD_RPS="700"))
        self.assertIn("⚠️", out)


class WorkerSettings(unittest.TestCase):
    """The report shows the default single worker thread and a four-worker run, and judges each difference against
    the scatter of the runs (the standard error of the mean, `*_SEM`, in percent of the mean)."""

    # One worker: 800 -> 1000 with noise sqrt(1.5^2 + 2.0^2) = 2.5%. Four workers: 3300 -> 3000 (-9.1%) with
    # noise sqrt(3.0^2 + 4.0^2) = 5.0%, so 2.5 times the noise (12.5%) is more than the drop.
    ENV = dict(
        Performance.ENV,
        HEAD_RPS_SEM="1.5", BASE_RPS_SEM="2.0",
        HEAD_W4_RPS="3000", HEAD_W4_P50="1.0", HEAD_W4_P99="2.0", HEAD_W4_OK="100", HEAD_W4_RPS_SEM="3.0",
        BASE_W4_RPS="3300", BASE_W4_P50="1.0", BASE_W4_P99="2.0", BASE_W4_OK="100", BASE_W4_RPS_SEM="4.0",
    )

    def test_both_worker_settings_are_shown_each_with_its_own_delta(self):
        out = perf.render(self.ENV)
        self.assertIn(perf.ONE_WORKER_TITLE, out)
        self.assertIn(perf.FOUR_WORKERS_TITLE, out)
        one, four = out.split(perf.FOUR_WORKERS_TITLE)
        self.assertIn("Requests/sec | 800 | 1000 | ▲ +25.0% (beyond noise, ±2.5%)", one)  # one worker
        self.assertIn("Requests/sec | 3300 | 3000 | ▼ -9.1% (within noise, ±5.0%)", four)  # four workers
        self.assertNotIn("3300", one)

    def test_a_drop_within_the_noise_is_not_flagged_however_large(self):
        out = perf.render(self.ENV)  # -9.1% at four workers, noise +-5.0%
        self.assertNotIn("⚠️", out)

    def test_an_adverse_difference_beyond_the_noise_is_flagged(self):
        out = perf.render(dict(self.ENV, HEAD_RPS="700"))  # 800 -> 700 = -12.5%, noise +-2.5%
        self.assertIn("▼ -12.5% ⚠️ (beyond noise, ±2.5%)", out)

    def test_a_small_difference_beyond_a_tiny_noise_is_reported_but_not_flagged(self):
        out = perf.render(dict(self.ENV, HEAD_RPS="790", HEAD_RPS_SEM="0.1", BASE_RPS_SEM="0.1"))  # -1.25%
        self.assertIn("▼ -1.2% (beyond noise, ±0.1%)", out)
        self.assertNotIn("⚠️", out)

    def test_a_gain_beyond_the_noise_is_not_flagged(self):
        out = perf.render(self.ENV)
        self.assertIn("▲ +25.0% (beyond noise", out)
        self.assertNotIn("⚠️", out)

    def test_without_scatter_numbers_a_difference_is_judged_by_its_size_alone(self):
        env = {k: v for k, v in self.ENV.items() if not k.endswith("_SEM")}
        out = perf.render(dict(env, HEAD_RPS="730"))  # -8.75%: under the 10% line
        self.assertNotIn("noise", out.split("### Four")[0].split("Metric")[1])
        self.assertNotIn("⚠️", out)
        self.assertIn("⚠️", perf.render(dict(env, HEAD_RPS="700")))  # -12.5%

    def test_a_scatter_above_the_mean_itself_is_not_believed(self):
        for big in ("150", "1e308", "100.5"):
            out = perf.render(dict(self.ENV, HEAD_RPS_SEM=big, HEAD_RPS="700"))
            row = next(line for line in out.splitlines() if line.startswith("Requests/sec"))
            self.assertIn("▼ -12.5% ⚠️", row)
            self.assertNotIn("noise", row, f"scatter {big!r} must not be used")
        out = perf.render(dict(self.ENV, HEAD_RPS_SEM="100", BASE_RPS_SEM="0", HEAD_RPS="700"))
        self.assertIn("within noise", next(line for line in out.splitlines() if line.startswith("Requests/sec")))

    def test_an_exact_tie_with_no_measured_scatter_is_within_noise(self):
        env = dict(self.ENV, HEAD_RPS="800", HEAD_RPS_SEM="0.0", BASE_RPS_SEM="0.0")
        row = next(line for line in perf.render(env).splitlines() if line.startswith("Requests/sec"))
        self.assertIn("(within noise, ±0.0%)", row)
        self.assertNotIn("⚠️", row)

    def test_hostile_scatter_numbers_fall_back_to_the_size_alone(self):
        for bad in ("nan", "inf", "-1", "<b>x</b>", ""):
            out = perf.render(dict(self.ENV, HEAD_RPS_SEM=bad, HEAD_RPS="700"))
            row = next(line for line in out.splitlines() if line.startswith("Requests/sec"))  # the one-worker table
            self.assertIn("▼ -12.5% ⚠️", row)
            self.assertNotIn("noise", row, f"scatter {bad!r} must not be used")
            self.assertNotIn("<b>", out)

    def test_without_four_worker_numbers_the_section_is_left_out(self):
        out = perf.render(Performance.ENV)
        self.assertNotIn(perf.FOUR_WORKERS_TITLE, out)
        self.assertIn(perf.ONE_WORKER_TITLE, out)

    def test_hostile_four_worker_values_leave_the_section_out_and_are_not_echoed(self):
        out = perf.render(dict(self.ENV, HEAD_W4_RPS="<script>alert(1)</script>"))
        self.assertNotIn("<script>", out)
        self.assertNotIn(perf.FOUR_WORKERS_TITLE, out)
        out = perf.render(dict(self.ENV, BASE_W4_P99="[x](http://evil.example)", HEAD_W4_RPS_SEM="inf"))
        self.assertNotIn("evil.example", out)
        self.assertIn("Requests/sec | 3300 | 3000", out)  # the rest of the table survives; the bad cells read n/a

    def test_a_regression_at_four_workers_is_flagged_on_its_own(self):
        out = perf.render(dict(self.ENV, HEAD_W4_RPS="2000"))  # 3300 -> 2000 = -39%, beyond the noise
        self.assertIn("⚠️", out.split(perf.FOUR_WORKERS_TITLE)[1])
        self.assertNotIn("⚠️", out.split(perf.FOUR_WORKERS_TITLE)[0])

    def test_without_a_base_each_table_is_head_only_and_the_missing_base_is_said_once(self):
        env = {k: v for k, v in self.ENV.items() if not k.startswith("BASE_")}
        out = perf.render(env)
        self.assertEqual(out.count("Metric | PR head"), 2)
        self.assertEqual(out.count("nothing to diff against"), 1)

    def test_the_four_worker_section_comes_before_the_runner_note(self):
        """The four-worker section comes before the runner note."""
        out = perf.render(self.ENV)
        self.assertLess(out.index(perf.FOUR_WORKERS_TITLE), out.index("**Runner:**"))

    def test_the_report_has_no_p50_row_because_it_only_restates_the_throughput(self):
        """The report has no p50 row because it only restates the throughput."""
        out = perf.render(self.ENV)
        self.assertNotIn("p50", out)
        self.assertIn("p99 latency", out)

    def test_the_success_rate_row_appears_only_when_a_request_failed(self):
        self.assertNotIn("Success rate", perf.render(self.ENV))  # every request succeeded on both sides
        self.assertIn("Success rate | 100.0% | 99.5%", perf.render(dict(self.ENV, HEAD_OK="99.5")))
        self.assertIn("Success rate | 98.0% | 100.0%", perf.render(dict(self.ENV, BASE_OK="98")))
        head_only = {k: v for k, v in self.ENV.items() if not k.startswith("BASE_")}
        self.assertNotIn("Success rate", perf.render(head_only))
        self.assertIn("Success rate | 99.0%", perf.render(dict(head_only, HEAD_OK="99")))

    def test_the_report_has_no_load_balancing_section_even_when_a_job_still_passes_one(self):
        """The report has no load-balancing section even when a job still passes one (LB, LB_W4)."""
        out = perf.render(dict(self.ENV, LB='{"head": {"even": {"p2c": {"rps": 1, "p99": 1}}}}', LB_W4="{}"))
        self.assertNotIn("Load balancing", out)
        self.assertNotIn("<details>", out)


class ResolvePr(unittest.TestCase):
    def run_script(self, **env):
        base = {"PATH": os.environ["PATH"], "EVENT_NAME": "pull_request"}
        base.update(env)
        out = subprocess.run(["bash", os.path.join(_here, "resolve_pr.sh")], env=base, capture_output=True,
                             text=True, check=True).stdout
        return dict(line.split("=", 1) for line in out.splitlines())

    def test_pull_request_values_pass_through_when_valid(self):
        got = self.run_script(EVENT_PR_NUMBER="546", EVENT_BASE_SHA="a" * 40)
        self.assertEqual(got, {"number": "546", "base_sha": "a" * 40})

    def test_a_non_numeric_number_is_dropped(self):
        got = self.run_script(EVENT_PR_NUMBER="12; rm -rf /", EVENT_BASE_SHA="a" * 40)
        self.assertEqual(got["number"], "")

    def test_a_malformed_base_sha_is_dropped(self):
        for bad in ("abc", "g" * 40, "A" * 40, ("a" * 40) + "\nx"):
            got = self.run_script(EVENT_PR_NUMBER="1", EVENT_BASE_SHA=bad)
            self.assertEqual(got["base_sha"], "", bad)


if __name__ == "__main__":
    unittest.main()
