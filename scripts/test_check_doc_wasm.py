#!/usr/bin/env python3
"""Tests of scripts/check_doc_wasm.py: the block extractor, the one part that needs no toolchain."""
import os
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import check_doc_wasm as cdw  # noqa: E402

PATH = os.path.join(cdw.REPO, "docs", "x.md")


class Extract(unittest.TestCase):
    """What counts as a plugin block, and what is reported as a problem."""

    def test_a_marked_rust_block_is_a_plugin(self):
        """The marker names the plugin; the block is its whole source."""
        text = "intro\n<!-- docs-check: plugin first_one -->\n```rust\nfn a() {}\n```\n"
        plugins, problems = cdw.extract(PATH, text)
        self.assertEqual(plugins, [("first_one", "fn a() {}\n", 3)])
        self.assertEqual(problems, [])

    def test_unmarked_blocks_are_ignored(self):
        """A rust block without a marker is a fragment and is not built."""
        plugins, problems = cdw.extract(PATH, "```rust\nfn a() {}\n```\n")
        self.assertEqual((plugins, problems), ([], []))

    def test_a_marker_not_followed_by_a_fence_is_a_problem(self):
        """A stale marker cannot hide a block that moved."""
        _, problems = cdw.extract(PATH, "<!-- docs-check: plugin a -->\ntext\n```rust\nx\n```\n")
        self.assertEqual(len(problems), 1)
        self.assertIn("not followed by a fence", problems[0])

    def test_a_marker_before_a_block_of_another_language_is_a_problem(self):
        """Only rust sources are built."""
        plugins, problems = cdw.extract(PATH, "<!-- docs-check: plugin a -->\n```yaml\nx: 1\n```\n")
        self.assertEqual(plugins, [])
        self.assertIn("not rust", problems[0])

    def test_a_marker_at_the_end_of_the_file_is_a_problem(self):
        """A marker with nothing after it is reported."""
        _, problems = cdw.extract(PATH, "<!-- docs-check: plugin a -->\n")
        self.assertEqual(len(problems), 1)

    def test_an_unclosed_fence_is_a_problem(self):
        """A fence that never closes would swallow the rest of the page."""
        _, problems = cdw.extract(PATH, "<!-- docs-check: plugin a -->\n```rust\nx\n")
        self.assertIn("never closed", problems[0])

    def test_two_plugins_with_one_name_are_a_problem(self):
        """The build directory is named after the plugin, so a name may be used once."""
        text = "<!-- docs-check: plugin a -->\n```rust\nx\n```\n<!-- docs-check: plugin a -->\n```rust\ny\n```\n"
        plugins, problems = cdw.extract(PATH, text)
        self.assertEqual(len(plugins), 2)
        self.assertIn("also defined", problems[0])


class TheGuide(unittest.TestCase):
    """The real guide must have plugins to build, otherwise the check would pass by checking nothing."""

    def test_the_guide_has_marked_plugins_and_no_marker_problems(self):
        """docs/wasm.md holds at least three marked plugins and its markers are well formed."""
        path = os.path.join(cdw.REPO, "docs", "wasm.md")
        with open(path, encoding="utf-8") as f:
            plugins, problems = cdw.extract(path, f.read())
        self.assertEqual(problems, [])
        self.assertGreaterEqual(len(plugins), 3)


if __name__ == "__main__":
    unittest.main()
