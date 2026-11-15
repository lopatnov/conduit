#!/usr/bin/env python3
"""Unit tests for scripts/third_party_licenses.py: the SPDX rules, the policy verdicts, and the file it writes (issue #640).

Run:  python3 scripts/test_third_party_licenses.py
"""
import json
import os
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import third_party_licenses as tpl  # noqa: E402

POLICY = {
    "allow": ["MIT", "Apache-2.0", "ISC", "MPL-2.0", "Zlib"],
    "allow_exceptions": ["LLVM-exception"],
    "clarify": {},
}


def ok(expr, policy=POLICY):
    return tpl.allowed(tpl.parse(expr), policy)


class Spdx(unittest.TestCase):
    def test_a_single_allowed_licence_passes_and_an_unknown_one_fails(self):
        self.assertTrue(ok("MIT"))
        self.assertFalse(ok("GPL-3.0-only"))

    def test_or_needs_one_allowed_alternative(self):
        self.assertTrue(ok("MIT OR Apache-2.0"))
        self.assertTrue(ok("MIT OR Apache-2.0 OR LGPL-2.1-or-later"))
        self.assertFalse(ok("GPL-2.0-only OR LGPL-3.0-only"))

    def test_and_needs_every_part_allowed(self):
        self.assertTrue(ok("MIT AND ISC"))
        self.assertFalse(ok("MIT AND GPL-3.0-only"), "one copyleft part makes the whole expression fail")

    def test_parentheses_and_precedence(self):
        # AND binds tighter than OR: GPL-3.0-only OR (MIT AND ISC)
        self.assertTrue(ok("GPL-3.0-only OR MIT AND ISC"))
        self.assertFalse(ok("(GPL-3.0-only OR MIT) AND GPL-2.0-only"))
        self.assertTrue(ok("ISC AND (Apache-2.0 OR GPL-3.0-only)"))

    def test_the_legacy_slash_means_or(self):
        self.assertTrue(ok("MIT/Apache-2.0"))
        self.assertTrue(ok("Apache-2.0 / MIT"))
        self.assertTrue(ok("GPL-3.0-only/MIT"))

    def test_a_plus_suffix_is_the_licence_itself(self):
        self.assertTrue(ok("MPL-2.0+"))
        self.assertFalse(ok("GPL-3.0+"))

    def test_with_needs_the_exception_allowed_as_well(self):
        self.assertTrue(ok("Apache-2.0 WITH LLVM-exception"))
        self.assertFalse(ok("Apache-2.0 WITH Classpath-exception-2.0"))
        self.assertFalse(ok("GPL-2.0-only WITH LLVM-exception"))

    def test_malformed_expressions_are_errors(self):
        for bad in ["", "MIT OR", "(MIT", "MIT AND AND ISC", "MIT)", "WITH"]:
            with self.assertRaises(tpl.SpdxError, msg=bad):
                tpl.parse(bad)


def package(name, version="1.0.0", license=None, files=None, directory=None, license_file=None):
    """A `cargo metadata` package whose directory holds `files` (name -> text)."""
    root = directory or tempfile.mkdtemp()
    for fname, text in (files or {}).items():
        with open(os.path.join(root, fname), "w", encoding="utf-8", newline="") as f:
            f.write(text)
    return {
        "name": name, "version": version, "license": license, "license_file": license_file,
        "repository": f"https://example.org/{name}", "manifest_path": os.path.join(root, "Cargo.toml"),
    }


class Policy(unittest.TestCase):
    def test_a_copyleft_crate_is_reported_with_its_licence(self):
        crates = [package("fine", license="MIT"), package("bad", license="GPL-3.0-only")]
        found = tpl.violations(crates, POLICY)
        self.assertEqual(len(found), 1, found)
        self.assertIn("bad 1.0.0", found[0])
        self.assertIn("GPL-3.0-only", found[0])

    def test_a_crate_without_an_expression_fails_until_the_policy_clarifies_it(self):
        crates = [package("mystery", license=None, files={"LICENSE": "x"})]
        self.assertEqual(len(tpl.violations(crates, POLICY)), 1)
        clarified = dict(POLICY, clarify={"mystery": {"license": "MIT"}})
        self.assertEqual(tpl.violations(crates, clarified), [])

    def test_a_malformed_expression_is_a_violation_not_a_crash(self):
        found = tpl.violations([package("odd", license="MIT OR")], POLICY)
        self.assertEqual(len(found), 1, found)

    def test_the_real_policy_file_accepts_the_common_licences_and_refuses_gpl(self):
        policy = tpl.load_policy()
        for expr in ["MIT", "Apache-2.0 WITH LLVM-exception", "Unicode-3.0", "CDLA-Permissive-2.0", "ISC AND MIT"]:
            self.assertTrue(tpl.allowed(tpl.parse(expr), policy), expr)
        for expr in ["GPL-3.0-only", "AGPL-3.0-or-later", "LGPL-2.1-only", "SSPL-1.0", "CC-BY-NC-4.0"]:
            self.assertFalse(tpl.allowed(tpl.parse(expr), policy), expr)


class Rendering(unittest.TestCase):
    def test_every_crate_is_listed_and_each_distinct_text_is_written_once_with_its_crates(self):
        mit = "MIT License\n\nCopyright (c) A\n"
        crates = [
            package("one", license="MIT", files={"LICENSE-MIT": mit, "LICENSE-APACHE": "Apache text\n"}),
            package("two", license="MIT", files={"LICENSE": mit}),
            package("three", license="MIT", files={"LICENSE": "MIT License\n\nCopyright (c) B\n"}),
        ]
        text = tpl.render(crates, POLICY)
        for name in ("one 1.0.0", "two 1.0.0", "three 1.0.0"):
            self.assertIn(f"{name}  [MIT]", text)
        self.assertEqual(text.count("Copyright (c) A"), 1, "the same text is written once")
        self.assertIn("Applies to: one 1.0.0, two 1.0.0", text)
        self.assertEqual(text.count("Copyright (c) B"), 1, "a different copyright line is a different text")
        self.assertIn("Apache text", text)

    def test_crlf_and_a_trailing_newline_do_not_make_two_texts(self):
        crates = [
            package("a", license="MIT", files={"LICENSE": "Same text\r\nsecond line\r\n"}),
            package("b", license="MIT", files={"LICENSE": "Same text\nsecond line"}),
        ]
        self.assertEqual(tpl.render(crates, POLICY).count("second line"), 1)

    def test_notice_and_copying_files_are_picked_up_but_other_files_are_not(self):
        pkg = package("c", license="MIT", files={
            "NOTICE": "a notice\n", "COPYING.md": "copying text\n", "README.md": "readme\n", "licenses_list.rs": "code\n",
        })
        names = [n for n, _ in tpl.license_files(pkg)]
        self.assertEqual(names, ["COPYING.md", "NOTICE"])

    def test_a_crate_without_a_file_is_listed_with_the_standard_text(self):
        text = tpl.render([package("bare", license="MIT OR Apache-2.0")], POLICY)
        self.assertIn("CRATES WITHOUT A LICENCE FILE", text)
        self.assertIn("bare 1.0.0  [MIT OR Apache-2.0]", text)
        self.assertIn("Apache License", text, "the standard text of the first available licence is added")

    def test_the_output_is_deterministic(self):
        crates = [package("a", license="MIT", files={"LICENSE": "a\n"}), package("z", license="MIT", files={"LICENSE": "z\n"})]
        self.assertEqual(tpl.render(crates, POLICY), tpl.render(crates, POLICY))


class Technologies(unittest.TestCase):
    """The hand-written table of docs/technologies.md is held to Cargo.lock."""

    def check(self, table):
        path = os.path.join(tempfile.mkdtemp(), "technologies.md")
        with open(path, "w", encoding="utf-8") as f:
            f.write(table)
        crates = [package("tokio", license="MIT"), package("tokio", "0.9.0", license="Apache-2.0"),
                  package("serde", license="MIT OR Apache-2.0")]
        return tpl.technology_problems(path, crates, POLICY)

    def test_a_row_that_matches_the_lock_is_fine(self):
        table = "| Crate | Use | Licence |\n|---|---|---|\n| `tokio` | runtime | MIT |\n| `serde` | config | MIT OR Apache-2.0 |\n"
        self.assertEqual(self.check(table), [])

    def test_any_version_of_a_crate_may_carry_the_licence(self):
        self.assertEqual(self.check("| `tokio` | runtime | Apache-2.0 |\n"), [])

    def test_a_wrong_licence_and_an_unknown_crate_are_reported(self):
        found = self.check("| `tokio` | runtime | GPL-3.0-only |\n| `nothing` | x | MIT |\n")
        self.assertEqual(len(found), 2, found)
        self.assertIn("line 1", found[0])
        self.assertIn("`nothing`", found[1])

    def test_prose_and_a_missing_file_are_handled(self):
        self.assertEqual(self.check("Some prose, not a table row.\n"), [])
        self.assertEqual(len(tpl.technology_problems("/nonexistent/technologies.md", [], POLICY)), 1)


class StandardTexts(unittest.TestCase):
    def test_the_templates_do_not_name_a_copyright_holder(self):
        """They are printed for third-party crates: a holder's name there would be a wrong notice (it once carried Conduit's
        own author, copied from LICENSE)."""
        folder = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "ci", "license-texts")
        files = sorted(os.listdir(folder))
        self.assertTrue(files)
        for name in files:
            with open(os.path.join(folder, name), encoding="utf-8") as f:
                text = f.read()
            for line in text.splitlines():
                if line.startswith("Copyright "):
                    self.assertTrue("[" in line or "<" in line, f"{name}: a copyright line must be a placeholder: {line!r}")
            for word in ("lopatnov", "Oleksandr"):
                self.assertNotIn(word, text, f"{name} names {word}")


class Metadata(unittest.TestCase):
    def test_only_normal_dependencies_of_the_root_are_shipped(self):
        def pid(n):
            return f"{n} 1.0.0"
        packages = [
            {"id": pid(n), "name": n, "version": "1.0.0"} for n in
            ("lopatnov-conduit", "member", "normal", "transitive", "devonly", "buildonly")
        ]
        dep = lambda n, kind: {"pkg": pid(n), "dep_kinds": [{"kind": kind}]}  # noqa: E731
        nodes = [
            {"id": pid("lopatnov-conduit"), "deps": [dep("member", None), dep("normal", None), dep("devonly", "dev"),
                                                        dep("buildonly", "build")]},
            {"id": pid("member"), "deps": [dep("transitive", None)]},
            {"id": pid("normal"), "deps": []}, {"id": pid("transitive"), "deps": []},
            {"id": pid("devonly"), "deps": []}, {"id": pid("buildonly"), "deps": []},
        ]
        meta = {
            "packages": packages,
            "workspace_members": [pid("lopatnov-conduit"), pid("member")],
            "resolve": {"nodes": nodes},
        }
        names = [p["name"] for p in tpl.shipped_crates(meta)]
        self.assertEqual(names, ["normal", "transitive"], "workspace crates, dev and build dependencies are not shipped")


if __name__ == "__main__":
    unittest.main()
