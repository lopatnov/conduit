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
        out = perf.render(dict(self.ENV, HW="evil\x00‮" + "x" * 1000))
        self.assertNotIn("\x00", out)
        self.assertNotIn("‮", out)
        self.assertLess(len(out), 2000)

    def test_a_regression_of_ten_percent_is_flagged(self):
        out = perf.render(dict(self.ENV, HEAD_RPS="700"))
        self.assertIn("⚠️", out)


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
