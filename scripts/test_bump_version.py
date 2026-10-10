#!/usr/bin/env python3
"""Unit tests for scripts/bump-version.py.

Run:  python3 scripts/test_bump_version.py
"""
import importlib.util
import json
import os
import unittest

_here = os.path.dirname(os.path.abspath(__file__))
_spec = importlib.util.spec_from_file_location("bump_version", os.path.join(_here, "bump-version.py"))
bv = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(bv)

CARGO = '''[workspace.package]
version    = "2.0.0"
edition    = "2021"

[workspace.dependencies]
lopatnov-conduit-core    = { path = "crates/conduit-core", version = "2.0.0" }
lopatnov-conduit-config  = { path = "crates/conduit-config", version = "2.0.0", default-features = false }
serde = { version = "2.0.0" }
'''


class NextVersion(unittest.TestCase):
    def test_keywords(self):
        self.assertEqual(bv.next_version("2.0.3", "patch"), "2.0.4")
        self.assertEqual(bv.next_version("2.0.3", "minor"), "2.1.0")
        self.assertEqual(bv.next_version("2.4.3", "major"), "3.0.0")

    def test_a_keyword_bump_drops_a_prerelease_tag(self):
        self.assertEqual(bv.next_version("2.1.0-rc.1", "patch"), "2.1.1")

    def test_explicit_versions(self):
        self.assertEqual(bv.next_version("2.0.0", "2.1.0-rc.1"), "2.1.0-rc.1")

    def test_rejects_garbage_and_no_ops(self):
        for bad in ("2.0", "v2.0.0", "two", ""):
            with self.assertRaises(ValueError):
                bv.next_version("2.0.0", bad)
        with self.assertRaises(ValueError):
            bv.next_version("2.0.0", "2.0.0")


class CargoToml(unittest.TestCase):
    def test_reads_the_workspace_version(self):
        self.assertEqual(bv.current_version(CARGO), "2.0.0")

    def test_bumps_package_and_conduit_dependencies_only(self):
        out, n = bv.bump_cargo_toml(CARGO, "2.0.0", "2.1.0")
        self.assertEqual(n, 2)
        self.assertIn('version    = "2.1.0"', out)
        self.assertIn('lopatnov-conduit-core    = { path = "crates/conduit-core", version = "2.1.0" }', out)
        self.assertIn('serde = { version = "2.0.0" }', out)  # a third-party crate is left alone

    def test_a_stale_dependency_literal_is_left_for_the_checker_to_flag(self):
        stale = CARGO.replace('crates/conduit-core", version = "2.0.0"', 'crates/conduit-core", version = "1.9.0"')
        out, n = bv.bump_cargo_toml(stale, "2.0.0", "2.1.0")
        self.assertEqual(n, 1)
        self.assertIn('version = "1.9.0"', out)

    def test_refuses_when_the_package_version_differs(self):
        with self.assertRaises(ValueError):
            bv.bump_cargo_toml(CARGO, "1.0.0", "1.1.0")


class PackageJson(unittest.TestCase):
    TEXT = '{\n  "name": "@lopatnov/conduit",\n  "version": "2.0.0",\n  "dependencies": {"x": "2.0.0"}\n}\n'

    def test_only_the_package_version_changes_and_formatting_is_kept(self):
        out = bv.bump_package_json(self.TEXT, "2.0.0", "2.1.0")
        self.assertEqual(json.loads(out)["version"], "2.1.0")
        self.assertEqual(json.loads(out)["dependencies"]["x"], "2.0.0")
        self.assertEqual(out.replace("2.1.0", "2.0.0", 1), self.TEXT)

    def test_refuses_on_drift(self):
        with self.assertRaises(ValueError):
            bv.bump_package_json(self.TEXT, "1.0.0", "1.1.0")


class Docs(unittest.TestCase):
    def test_full_versions_are_replaced_but_longer_numbers_are_not(self):
        text = "Conduit 2.0.0 and 12.0.0 and 2.0.01 and 2.0.0.1"
        self.assertEqual(bv.bump_doc(text, "2.0.0", "2.1.0"), "Conduit 2.1.0 and 12.0.0 and 2.0.01 and 2.0.0.1")

    def test_image_tag_aliases(self):
        text = "`:latest`, `:2.0.0`, `:2.0` / `:2.0.0-full`, `:2.0-full`, `:2.01`, `:12.0`"
        out = bv.bump_doc(text, "2.0.0", "3.4.0", with_tag_aliases=True)
        self.assertEqual(out, "`:latest`, `:3.4.0`, `:3.4` / `:3.4.0-full`, `:3.4-full`, `:2.01`, `:12.0`")

    def test_aliases_do_not_move_for_a_prerelease_target(self):
        out = bv.bump_doc("`:2.0.0`, `:2.0`, `:2.0-full`", "2.0.0", "2.1.0-rc.1", with_tag_aliases=True)
        self.assertEqual(out, "`:2.1.0-rc.1`, `:2.0`, `:2.0-full`")

    def test_aliases_are_left_alone_without_the_flag(self):
        self.assertEqual(bv.bump_doc("`:2.0`", "2.0.0", "2.1.0"), "`:2.0`")


if __name__ == "__main__":
    unittest.main()
