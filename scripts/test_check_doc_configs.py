#!/usr/bin/env python3
"""Tests for scripts/check_doc_configs.py (issue #567).

A stub stands in for `conduit`: `validate -c FILE` prints a warning about an unknown key when the file contains
BADKEY, an error and exit 1 when it contains BADVALUE, and `Config is valid` otherwise.

Run:  python3 scripts/test_check_doc_configs.py
"""
import importlib.util
import os
import sys
import tempfile
import unittest

_here = os.path.dirname(os.path.abspath(__file__))
spec = importlib.util.spec_from_file_location("check_doc_configs", os.path.join(_here, "check_doc_configs.py"))
cdc = importlib.util.module_from_spec(spec)
spec.loader.exec_module(cdc)

STUB = """\
import sys
if sys.argv[1:2] == ["--version"]:
    print("conduit stub"); sys.exit(0)
path = sys.argv[sys.argv.index("-c") + 1]
text = open(path, encoding="utf-8").read()
if "BADKEY" in text:
    print("warning: sites[0].BADKEY is not a recognized configuration key and will be ignored - check for a typo.", file=sys.stderr)
    print("Config is valid - 1 site, 0 routes.")
elif "OTHERWARN" in text:
    print("warning: something else, not about a key", file=sys.stderr)
    print("Config is valid - 1 site, 0 routes.")
elif "BADVALUE" in text:
    print("error at sites[0].x: broken", file=sys.stderr); sys.exit(1)
else:
    print("Config is valid - 1 site, 0 routes.")
"""


def md(*parts):
    return "\n".join(parts) + "\n"


class Extract(unittest.TestCase):
    def blocks(self, text):
        return cdc.extract(os.path.join(cdc.REPO, "docs", "x.md"), text)

    def test_only_yaml_yml_json_fences_are_blocks(self):
        blocks, problems = self.blocks(md("```yaml", "port: 1", "```", "```bash", "ls", "```", "```json", "{}", "```", "```yml", "a: 1", "```"))
        self.assertEqual([b.lang for b in blocks], ["yaml", "json", "yml"])
        self.assertEqual(problems, [])

    def test_a_block_knows_its_line_and_text(self):
        blocks, _ = self.blocks(md("text", "", "```yaml", "port: 1", "host: a", "```"))
        self.assertEqual(blocks[0].line, 3)
        self.assertEqual(blocks[0].text, "port: 1\nhost: a\n")

    def test_the_marker_marks_the_next_fence_only(self):
        blocks, problems = self.blocks(md("<!-- docs-check: skip (excerpt) -->", "```yaml", "a: 1", "```", "```yaml", "b: 2", "```"))
        self.assertEqual([b.skipped for b in blocks], [True, False])
        self.assertEqual(blocks[0].skip_reason, "excerpt")
        self.assertEqual(problems, [])

    def test_the_reason_may_contain_parentheses(self):
        blocks, problems = self.blocks(md("<!-- docs-check: skip (command output (JSON), not a config) -->", "```json", "{}", "```"))
        self.assertEqual(blocks[0].skip_reason, "command output (JSON), not a config")
        self.assertEqual(problems, [])

    def test_a_marker_may_be_separated_from_its_fence_by_one_blank_line(self):
        blocks, problems = self.blocks(md("<!-- docs-check: skip -->", "", "```yaml", "a: 1", "```"))
        self.assertTrue(blocks[0].skipped)
        self.assertEqual(problems, [])

    def test_a_marker_with_nothing_after_it_is_a_problem(self):
        _, problems = self.blocks(md("<!-- docs-check: skip -->", "", "", "some prose", "```yaml", "a: 1", "```"))
        self.assertEqual(len(problems), 1)
        self.assertIn("not followed by a fence", problems[0])

    def test_a_marker_at_the_end_of_the_file_is_a_problem(self):
        _, problems = self.blocks(md("text", "<!-- docs-check: skip -->"))
        self.assertEqual(len(problems), 1)

    def test_two_markers_in_a_row_are_a_problem(self):
        _, problems = self.blocks(md("<!-- docs-check: skip -->", "<!-- docs-check: skip -->", "```yaml", "a: 1", "```"))
        self.assertEqual(len(problems), 1)

    def test_an_unclosed_fence_is_a_problem(self):
        blocks, problems = self.blocks(md("```yaml", "a: 1", "", "text goes on"))
        self.assertEqual(blocks, [])
        self.assertIn("never closed", problems[0])

    def test_a_longer_fence_is_closed_only_by_an_equal_or_longer_one(self):
        blocks, problems = self.blocks(md("````yaml", "a: 1", "```", "b: 2", "````"))
        self.assertEqual(blocks[0].text, "a: 1\n```\nb: 2\n")
        self.assertEqual(problems, [])

    def test_tilde_fences_work_and_are_not_closed_by_backticks(self):
        blocks, _ = self.blocks(md("~~~yaml", "a: 1", "```", "~~~"))
        self.assertEqual(blocks[0].text, "a: 1\n```\n")

    def test_a_fence_inside_another_language_block_is_not_a_block(self):
        blocks, _ = self.blocks(md("```markdown", "```yaml", "a: 1", "```", "```"))
        self.assertEqual(blocks, [])

    def test_a_config_fence_opening_inside_an_open_fence_is_a_problem(self):
        _, problems = self.blocks(md("```bash", "echo hi", "```yaml", "a: 1", "```", "", "```yaml", "b: 2", "```"))
        self.assertTrue(any("opens inside the fence opened at line 1" in p for p in problems), problems)

    def test_an_inner_fence_of_a_markdown_example_is_not_a_problem(self):
        _, problems = self.blocks(md("````markdown", "```yaml", "a: 1", "```", "````"))
        self.assertEqual(problems, [])
        _, problems = self.blocks(md("~~~markdown", "```yaml", "a: 1", "```", "~~~"))
        self.assertEqual(problems, [])

    def test_a_marker_inside_a_fence_is_just_text(self):
        blocks, problems = self.blocks(md("```yaml", "<!-- docs-check: skip -->", "a: 1", "```"))
        self.assertFalse(blocks[0].skipped)
        self.assertEqual(problems, [])

    def test_the_info_string_may_have_more_words(self):
        blocks, _ = self.blocks(md("```yaml title=conduit.yaml", "a: 1", "```"))
        self.assertEqual(blocks[0].lang, "yaml")

    def test_json_comment_lines_are_removed_but_a_slash_slash_in_a_value_stays(self):
        text = '{\n  // a comment\n  "url": "http://a//b"\n}\n'
        self.assertEqual(cdc.strip_json_comments(text), '{\n  "url": "http://a//b"\n}\n')


class Run(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.tmp = tempfile.TemporaryDirectory()
        cls.stub = os.path.join(cls.tmp.name, "stub.py")
        with open(cls.stub, "w", encoding="utf-8") as f:
            f.write(STUB)
        cls.conduit = [sys.executable, cls.stub]

    @classmethod
    def tearDownClass(cls):
        cls.tmp.cleanup()

    def check(self, markdown):
        path = os.path.join(self.tmp.name, "doc.md")
        with open(path, "w", encoding="utf-8") as f:
            f.write(markdown)
        return cdc.run(self.conduit, [path], jobs=2, with_examples=False)

    def test_good_blocks_pass(self):
        checked, skipped, failures, problems = self.check(md("```yaml", "port: 1", "```", "```json", '{ "port": 1 }', "```"))
        self.assertEqual((checked, skipped, failures, problems), (2, 0, [], []))

    def test_an_unknown_key_warning_fails_the_block(self):
        _, _, failures, _ = self.check(md("```yaml", "BADKEY: 1", "```"))
        self.assertEqual(len(failures), 1)
        self.assertIn("not a recognized configuration key", failures[0][1])

    def test_another_warning_does_not_fail_the_block(self):
        _, _, failures, _ = self.check(md("```yaml", "OTHERWARN: 1", "```"))
        self.assertEqual(failures, [])

    def test_a_validation_error_fails_the_block(self):
        _, _, failures, _ = self.check(md("```yaml", "BADVALUE: 1", "```"))
        self.assertEqual(len(failures), 1)
        self.assertIn("broken", failures[0][1])

    def test_a_marked_fragment_is_not_run(self):
        checked, skipped, failures, _ = self.check(md("<!-- docs-check: skip (excerpt) -->", "```yaml", "BADVALUE: 1", "```"))
        self.assertEqual((checked, skipped, failures), (0, 1, []))

    def test_json_comments_are_stripped_before_the_check(self):
        _, _, failures, _ = self.check(md("```json", "{", "  // BADVALUE would fail if it were not removed", '  "a": 1', "}", "```"))
        self.assertEqual(failures, [])

    def test_a_failure_names_the_file_and_line(self):
        _, _, failures, _ = self.check(md("text", "```yaml", "BADVALUE: 1", "```"))
        self.assertTrue(failures[0][0].endswith(":2"), failures[0][0])


class Main(unittest.TestCase):
    def test_exit_codes(self):
        with tempfile.TemporaryDirectory() as tmp:
            stub = os.path.join(tmp, "stub.py")
            with open(stub, "w", encoding="utf-8") as f:
                f.write(STUB)
            good, bad = os.path.join(tmp, "good.md"), os.path.join(tmp, "bad.md")
            with open(good, "w", encoding="utf-8") as f:
                f.write(md("```yaml", "port: 1", "```"))
            with open(bad, "w", encoding="utf-8") as f:
                f.write(md("```yaml", "BADVALUE: 1", "```"))
            cmd = f'"{sys.executable}" "{stub}"'
            self.assertEqual(cdc.main(["--conduit", cmd, good]), 0)
            self.assertEqual(cdc.main(["--conduit", cmd, bad]), 1)
            self.assertEqual(cdc.main(["--conduit", os.path.join(tmp, "nonexistent"), good]), 2)
            self.assertEqual(cdc.main(["--conduit", cmd, os.path.join(tmp, "missing.md")]), 2)


if __name__ == "__main__":
    unittest.main()
