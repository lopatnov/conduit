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
    """Join lines into markdown text that ends with a newline."""
    return "\n".join(parts) + "\n"


class Extract(unittest.TestCase):
    def blocks(self, text):
        """Extract the blocks and problems of a markdown text as if it were docs/x.md."""
        return cdc.extract(os.path.join(cdc.REPO, "docs", "x.md"), text)

    def test_only_yaml_yml_json_fences_are_blocks(self):
        """Only yaml yml json fences are blocks."""
        blocks, problems = self.blocks(md("```yaml", "port: 1", "```", "```bash", "ls", "```", "```json", "{}", "```", "```yml", "a: 1", "```"))
        self.assertEqual([b.lang for b in blocks], ["yaml", "json", "yml"])
        self.assertEqual(problems, [])

    def test_a_block_knows_its_line_and_text(self):
        """A block knows its line and text."""
        blocks, _ = self.blocks(md("text", "", "```yaml", "port: 1", "host: a", "```"))
        self.assertEqual(blocks[0].line, 3)
        self.assertEqual(blocks[0].text, "port: 1\nhost: a\n")

    def test_the_marker_marks_the_next_fence_only(self):
        """The marker marks the next fence only."""
        blocks, problems = self.blocks(md("<!-- docs-check: skip (excerpt) -->", "```yaml", "a: 1", "```", "```yaml", "b: 2", "```"))
        self.assertEqual([b.skipped for b in blocks], [True, False])
        self.assertEqual(blocks[0].skip_reason, "excerpt")
        self.assertEqual(problems, [])

    def test_the_reason_may_contain_parentheses(self):
        """The reason may contain parentheses."""
        blocks, problems = self.blocks(md("<!-- docs-check: skip (command output (JSON), not a config) -->", "```json", "{}", "```"))
        self.assertEqual(blocks[0].skip_reason, "command output (JSON), not a config")
        self.assertEqual(problems, [])

    def test_a_marker_may_be_separated_from_its_fence_by_one_blank_line(self):
        """A marker may be separated from its fence by one blank line."""
        blocks, problems = self.blocks(md("<!-- docs-check: skip -->", "", "```yaml", "a: 1", "```"))
        self.assertTrue(blocks[0].skipped)
        self.assertEqual(problems, [])

    def test_a_marker_with_nothing_after_it_is_a_problem(self):
        """A marker with nothing after it is a problem."""
        _, problems = self.blocks(md("<!-- docs-check: skip -->", "", "", "some prose", "```yaml", "a: 1", "```"))
        self.assertEqual(len(problems), 1)
        self.assertIn("not followed by a fence", problems[0])

    def test_a_marker_at_the_end_of_the_file_is_a_problem(self):
        """A marker at the end of the file is a problem."""
        _, problems = self.blocks(md("text", "<!-- docs-check: skip -->"))
        self.assertEqual(len(problems), 1)

    def test_two_markers_in_a_row_are_a_problem(self):
        """Two markers in a row are a problem."""
        _, problems = self.blocks(md("<!-- docs-check: skip -->", "<!-- docs-check: skip -->", "```yaml", "a: 1", "```"))
        self.assertEqual(len(problems), 1)

    def test_an_unclosed_fence_is_a_problem(self):
        """An unclosed fence is a problem."""
        blocks, problems = self.blocks(md("```yaml", "a: 1", "", "text goes on"))
        self.assertEqual(blocks, [])
        self.assertIn("never closed", problems[0])

    def test_a_longer_fence_is_closed_only_by_an_equal_or_longer_one(self):
        """A longer fence is closed only by an equal or longer one."""
        blocks, problems = self.blocks(md("````yaml", "a: 1", "```", "b: 2", "````"))
        self.assertEqual(blocks[0].text, "a: 1\n```\nb: 2\n")
        self.assertEqual(problems, [])

    def test_tilde_fences_work_and_are_not_closed_by_backticks(self):
        """Tilde fences work and are not closed by backticks."""
        blocks, _ = self.blocks(md("~~~yaml", "a: 1", "```", "~~~"))
        self.assertEqual(blocks[0].text, "a: 1\n```\n")

    def test_a_fence_inside_another_language_block_is_not_a_block(self):
        """A fence inside another language block is not a block."""
        blocks, _ = self.blocks(md("```markdown", "```yaml", "a: 1", "```", "```"))
        self.assertEqual(blocks, [])

    def test_a_config_fence_opening_inside_an_open_fence_is_a_problem(self):
        """A config fence opening inside an open fence is a problem."""
        _, problems = self.blocks(md("```bash", "echo hi", "```yaml", "a: 1", "```", "", "```yaml", "b: 2", "```"))
        self.assertTrue(any("opens inside the fence opened at line 1" in p for p in problems), problems)

    def test_an_inner_fence_of_a_markdown_example_is_not_a_problem(self):
        """An inner fence of a markdown example is not a problem."""
        _, problems = self.blocks(md("````markdown", "```yaml", "a: 1", "```", "````"))
        self.assertEqual(problems, [])
        _, problems = self.blocks(md("~~~markdown", "```yaml", "a: 1", "```", "~~~"))
        self.assertEqual(problems, [])

    def test_a_marker_inside_a_fence_is_just_text(self):
        """A marker inside a fence is just text."""
        blocks, problems = self.blocks(md("```yaml", "<!-- docs-check: skip -->", "a: 1", "```"))
        self.assertFalse(blocks[0].skipped)
        self.assertEqual(problems, [])

    def test_the_info_string_may_have_more_words(self):
        """The info string may have more words."""
        blocks, _ = self.blocks(md("```yaml title=conduit.yaml", "a: 1", "```"))
        self.assertEqual(blocks[0].lang, "yaml")

    def test_json_comment_lines_are_removed_but_a_slash_slash_in_a_value_stays(self):
        """Json comment lines are removed but a slash slash in a value stays."""
        text = '{\n  // a comment\n  "url": "http://a//b"\n}\n'
        self.assertEqual(cdc.strip_json_comments(text), '{\n  "url": "http://a//b"\n}\n')


class Run(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        """Write the stub `conduit` to a temporary directory once for the class."""
        cls.tmp = tempfile.TemporaryDirectory()
        cls.stub = os.path.join(cls.tmp.name, "stub.py")
        with open(cls.stub, "w", encoding="utf-8") as f:
            f.write(STUB)
        cls.conduit = [sys.executable, cls.stub]

    @classmethod
    def tearDownClass(cls):
        """Remove the temporary directory of the stub."""
        cls.tmp.cleanup()

    def check(self, markdown):
        """Validate a markdown text with the stub `conduit`; return what `run` returns."""
        path = os.path.join(self.tmp.name, "doc.md")
        with open(path, "w", encoding="utf-8") as f:
            f.write(markdown)
        return cdc.run(self.conduit, [path], jobs=2, with_examples=False)

    def test_good_blocks_pass(self):
        """Good blocks pass."""
        checked, skipped, failures, problems = self.check(md("```yaml", "port: 1", "```", "```json", '{ "port": 1 }', "```"))
        self.assertEqual((checked, skipped, failures, problems), (2, 0, [], []))

    def test_an_unknown_key_warning_fails_the_block(self):
        """An unknown key warning fails the block."""
        _, _, failures, _ = self.check(md("```yaml", "BADKEY: 1", "```"))
        self.assertEqual(len(failures), 1)
        self.assertIn("not a recognized configuration key", failures[0][1])

    def test_another_warning_does_not_fail_the_block(self):
        """Another warning does not fail the block."""
        _, _, failures, _ = self.check(md("```yaml", "OTHERWARN: 1", "```"))
        self.assertEqual(failures, [])

    def test_a_validation_error_fails_the_block(self):
        """A validation error fails the block."""
        _, _, failures, _ = self.check(md("```yaml", "BADVALUE: 1", "```"))
        self.assertEqual(len(failures), 1)
        self.assertIn("broken", failures[0][1])

    def test_a_marked_fragment_is_not_run(self):
        """A marked fragment is not run."""
        checked, skipped, failures, _ = self.check(md("<!-- docs-check: skip (excerpt) -->", "```yaml", "BADVALUE: 1", "```"))
        self.assertEqual((checked, skipped, failures), (0, 1, []))

    def test_json_comments_are_stripped_before_the_check(self):
        """Json comments are stripped before the check."""
        _, _, failures, _ = self.check(md("```json", "{", "  // BADVALUE would fail if it were not removed", '  "a": 1', "}", "```"))
        self.assertEqual(failures, [])

    def test_a_failure_names_the_file_and_line(self):
        """A failure names the file and line."""
        _, _, failures, _ = self.check(md("text", "```yaml", "BADVALUE: 1", "```"))
        self.assertTrue(failures[0][0].endswith(":2"), failures[0][0])


class Main(unittest.TestCase):
    def test_exit_codes(self):
        """Exit codes."""
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


class StubScripts(unittest.TestCase):
    """The script files a documented configuration names (issue #566): `conduit validate` reads and compiles them."""

    def test_a_relative_rhai_path_gets_an_empty_file_and_the_text_names_it(self):
        """A relative .rhai path gets an empty file under the directory and the text names that file."""
        with tempfile.TemporaryDirectory() as tmp:
            out = cdc.stub_scripts("path: ./scripts/auth.rhai\nother: check.rhai\n", tmp)
            first = os.path.join(tmp, "scripts", "auth.rhai").replace("\\", "/")
            second = os.path.join(tmp, "check.rhai").replace("\\", "/")
            self.assertEqual(out, f"path: {first}\nother: {second}\n")
            self.assertTrue(os.path.isfile(first) and os.path.getsize(first) == 0)
            self.assertTrue(os.path.isfile(second))

    def test_json_quoted_paths_are_found_too(self):
        """A path inside JSON quotes is found as well."""
        with tempfile.TemporaryDirectory() as tmp:
            out = cdc.stub_scripts('{ "path": "./scripts/a.rhai" }', tmp)
            self.assertNotIn("./scripts/a.rhai", out)
            self.assertTrue(os.path.isfile(os.path.join(tmp, "scripts", "a.rhai")))

    def test_absolute_paths_parent_paths_and_other_files_are_left_alone(self):
        """Absolute paths, paths with .. and files of other kinds are left alone and nothing is created for them."""
        with tempfile.TemporaryDirectory() as tmp:
            text = "a: /etc/conduit/x.rhai\nb: ../up/y.rhai\nc: ./plugin.wasm\nd: C:\\scripts\\z.rhai\n"
            self.assertEqual(cdc.stub_scripts(text, tmp), text)
            self.assertEqual(os.listdir(tmp), [])

    def test_a_block_that_names_a_script_is_validated_against_the_stub(self):
        """check_block hands the stub's path to the validator, not the one in the documentation."""
        with tempfile.TemporaryDirectory() as tmp:
            stub = os.path.join(tmp, "conduit_stub.py")
            with open(stub, "w", encoding="utf-8") as f:
                f.write(
                    "import sys\n"
                    "text = open(sys.argv[sys.argv.index('-c') + 1], encoding='utf-8').read()\n"
                    "path = [w for w in text.split() if w.endswith('.rhai')][0]\n"
                    "import os\n"
                    "sys.exit(0 if os.path.isfile(path) else 1)\n"
                )
            block = cdc.Block("docs/x.md", 1, "yaml", "path: ./scripts/auth.rhai\n", None)
            ok, out = cdc.check_block([sys.executable, stub], block, tmp)
            self.assertTrue(ok, out)


if __name__ == "__main__":
    unittest.main()
