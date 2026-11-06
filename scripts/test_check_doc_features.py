#!/usr/bin/env python3
"""Tests of scripts/check_doc_features.py (#568). Run: python3 scripts/test_check_doc_features.py"""
import os
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import check_doc_features as cdf  # noqa: E402

CARGO = """
[features]
default = ["proxy", "static"]
proxy = ["crate-a/proxy", "dep:url"]
static = ["crate-b/static"]
jwt = ["crate-c/jwt"]
cache = ["proxy", "crate-d/cache"]
standard = ["jwt", "cache", "proxy", "static"]
gateway = ["proxy", "jwt", "cache"]
static-server = ["static"]
full = ["standard", "redis"]
redis = ["crate-e/redis"]
"""

CONFIG = """# Configuration

### Optional build features

Text.

#### Baseline features

| Feature | What |
| --- | --- |
| `proxy` | proxying |
| `static` | files |

#### Optional features

| Feature | What |
| --- | --- |
| `jwt` | tokens |
| `cache` | cache |
| `redis` | shared |

#### Bundles

| Feature | `standard` | `gateway` | `static-server` | `full` |
| --- | :-: | :-: | :-: | :-: |
| `proxy` | ✓ | ✓ | | ✓ |
| `static` | ✓ | | ✓ | ✓ |
| `jwt`, `cache` | ✓ | ✓ | | ✓ |
| `redis` | | | | ✓ |

Prose with a `static-server` mention.

---

## Table of Contents
"""

CLI = """## Build features

### Feature overview

| Feature | What |
| --- | --- |
| `proxy` (default) | x |
| `static` (default) | x |
| `jwt` | x |
| `cache` | x |
| `redis` | x |
| `standard` | bundle |
| `gateway` | bundle |
| `static-server` | bundle |
| `full` | bundle |

Text after.
"""


def problems(cargo=CARGO, config=CONFIG, cli=CLI):
    """Run the check on texts."""
    return cdf.check(cargo, config, cli)


class Check(unittest.TestCase):
    """The comparison of the docs with Cargo.toml."""

    def test_the_consistent_docs_pass(self):
        """The consistent docs pass."""
        self.assertEqual(problems(), [])

    def test_a_feature_missing_from_the_configuration_guide_is_reported(self):
        """A feature missing from the configuration guide is reported."""
        out = problems(config=CONFIG.replace("| `redis` | shared |\n", ""))
        self.assertTrue(any("`redis` has no row" in p for p in out), out)

    def test_a_feature_missing_from_the_cli_guide_is_reported(self):
        """A feature missing from the cli guide is reported."""
        out = problems(cli=CLI.replace("| `jwt` | x |\n", ""))
        self.assertTrue(any("docs/cli.md" in p and "`jwt`" in p for p in out), out)

    def test_a_new_cargo_feature_without_documentation_is_reported_in_both_files(self):
        """A new cargo feature without documentation is reported in both files."""
        out = problems(cargo=CARGO + 'newthing = ["crate-f/x"]\n')
        self.assertEqual(sum("`newthing`" in p for p in out), 2, out)

    def test_a_documented_feature_that_cargo_does_not_have_is_reported(self):
        """A documented feature that cargo does not have is reported."""
        out = problems(config=CONFIG.replace("| `redis` | shared |", "| `redis` | shared |\n| `ghost` | nothing |"))
        self.assertTrue(any("`ghost`" in p and "not in Cargo.toml" in p for p in out), out)

    def test_a_bundle_cell_that_is_missing_is_reported(self):
        """A bundle cell that is missing is reported."""
        out = problems(config=CONFIG.replace("| `jwt`, `cache` | ✓ | ✓ |", "| `jwt`, `cache` | ✓ | |"))
        self.assertTrue(any("`gateway`" in p and "missing `cache`, `jwt`" in p for p in out), out)

    def test_a_bundle_cell_that_is_extra_is_reported(self):
        """A bundle cell that is extra is reported."""
        out = problems(config=CONFIG.replace("| `redis` | | | | ✓ |", "| `redis` | ✓ | | | ✓ |"))
        self.assertTrue(any("`standard`" in p and "not in the bundle: `redis`" in p for p in out), out)

    def test_a_feature_that_names_another_feature_brings_it_into_the_bundle(self):
        # `gateway` lists `cache`, which implies `proxy`: the table must show `proxy` for it, and a table without does fail.
        """A feature that names another feature brings it into the bundle."""
        out = problems(config=CONFIG.replace("| `proxy` | ✓ | ✓ | | ✓ |", "| `proxy` | ✓ | | | ✓ |"))
        self.assertTrue(any("`gateway`" in p and "missing `proxy`" in p for p in out), out)

    def test_a_bundle_that_includes_another_bundle_includes_its_features(self):
        """A bundle that includes another bundle includes its features."""
        out = problems(config=CONFIG.replace("| `static` | ✓ | | ✓ | ✓ |", "| `static` | ✓ | | ✓ | |"))
        self.assertTrue(any("`full`" in p and "missing `static`" in p for p in out), out)

    def test_a_missing_bundle_table_is_reported(self):
        """A missing bundle table is reported."""
        out = problems(config=CONFIG.replace("| Feature | `standard` | `gateway` | `static-server` | `full` |", "| Feature | A | B |"))
        self.assertTrue(any("no bundle table" in p for p in out), out)

    def test_a_bundle_without_a_column_is_reported(self):
        """A bundle without a column is reported."""
        head, tail = CONFIG.split("#### Bundles\n")
        table, rest = tail.split("\nProse")
        kept = []
        for line in table.split("\n"):
            if line.startswith("|"):
                cells = line.strip("|").split("|")
                del cells[2]  # the `gateway` column: cells[0] is the feature, cells[1] standard, cells[2] gateway
                line = "|" + "|".join(cells) + "|"
            kept.append(line)
        out = problems(config=head + "#### Bundles\n" + "\n".join(kept) + "\nProse" + rest)
        self.assertTrue(any("no column for `gateway`" in p for p in out), out)

    def test_a_missing_section_is_an_error_not_a_pass(self):
        """A missing section is an error not a pass."""
        with self.assertRaises(cdf.DocError):
            problems(config="# no section here\n")
        with self.assertRaises(cdf.DocError):
            problems(cli="## nothing\n")


class Closure(unittest.TestCase):
    """What a feature turns on."""

    def test_the_closure_follows_features_and_skips_dependencies(self):
        """The closure follows features and skips dependencies."""
        table = cdf.features(CARGO)
        self.assertEqual(cdf.closure(table, "cache"), {"cache", "proxy"})
        self.assertEqual(cdf.closure(table, "full"), {"full", "standard", "jwt", "cache", "proxy", "static", "redis"})


class Main(unittest.TestCase):
    """The command line."""

    def write(self, directory, name, text):
        """Write a file and return its path."""
        path = os.path.join(directory, name)
        with open(path, "w", encoding="utf-8") as handle:
            handle.write(text)
        return path

    def test_exit_codes(self):
        """Exit codes."""
        with tempfile.TemporaryDirectory() as tmp:
            cargo = self.write(tmp, "Cargo.toml", CARGO)
            config = self.write(tmp, "configuration.md", CONFIG)
            cli = self.write(tmp, "cli.md", CLI)
            self.assertEqual(cdf.main(["--cargo", cargo, "--config", config, "--cli", cli]), 0)
            broken = self.write(tmp, "broken.md", CONFIG.replace("| `redis` | shared |\n", ""))
            self.assertEqual(cdf.main(["--cargo", cargo, "--config", broken, "--cli", cli]), 1)
            self.assertEqual(cdf.main(["--cargo", cargo, "--config", os.path.join(tmp, "missing.md"), "--cli", cli]), 2)
            self.assertEqual(cdf.main(["--cargo", self.write(tmp, "bad.toml", "[features\n"), "--config", config, "--cli", cli]), 2)


class Repository(unittest.TestCase):
    """The real documentation of this repository."""

    def test_the_repository_docs_pass(self):
        """The repository docs pass."""
        self.assertEqual(cdf.main([]), 0)


if __name__ == "__main__":
    unittest.main()
