#!/usr/bin/env python3
"""Checks that the names the release workflow publishes and the names the npm package asks for are the same (#665).

    python3 scripts/test_release_matrix.py

The npm package downloads `conduit-<target>[.exe]` (the full build) from the release of its own version: if the
workflow renamed an asset and `npm/bin/download.js` did not follow, every `npm install` would fail at the download.
Plain text parsing, so that it runs on a bare Python without PyYAML.
"""
import os
import re
import unittest

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


def read(path):
    with open(os.path.join(ROOT, path), encoding="utf-8") as f:
        return f.read()


def matrix_entries():
    """The build matrix of release.yml as dicts of the keys of each `- target:` entry."""
    text = read(".github/workflows/release.yml")
    start = text.index("        include:\n")
    end = text.index("\n    steps:\n", start)
    entries, current = [], None
    for line in text[start:end].splitlines():
        match = re.match(r"^\s+(?:- )?([a-z_]+): (.*)$", line)
        if not match:
            continue
        key, value = match.group(1), match.group(2).strip().strip('"')
        if line.lstrip().startswith("- target:"):
            current = {}
            entries.append(current)
        current[key] = value
    return entries


def docker_job():
    """The text of the `docker` job of release.yml, up to the npm job."""
    workflow = read(".github/workflows/release.yml")
    return workflow[workflow.index("\n  docker:\n"):workflow.index("  # ── 5. Publish to npm")]


def npm_assets(path):
    """The asset names listed in an npm file's platform table (the values of the `"os-arch": "conduit-..."` pairs)."""
    return sorted(re.findall(r'"[a-z0-9]+-[a-z0-9]+":\s*"(conduit-[^"]+)"', read(path)))


class ReleaseMatrix(unittest.TestCase):
    """The matrix, the npm tables and the bundles agree."""

    def test_there_are_four_bundles_and_every_asset_name_is_unique(self):
        """25 builds: full, gateway and static on six targets, standard on those six and RISC-V."""
        entries = matrix_entries()
        names = [e["asset_name"] for e in entries]
        self.assertEqual(len(names), len(set(names)), "two builds would publish the same file")
        counts = {b: sum(1 for e in entries if e["bundle"] == b) for b in ("full", "standard", "gateway", "static")}
        self.assertEqual(counts, {"full": 6, "standard": 7, "gateway": 6, "static": 6})

    def test_the_suffix_of_a_name_is_its_bundle(self):
        """Full is the plain name; the others end in their bundle name (before `.exe`)."""
        for e in matrix_entries():
            stem = e["asset_name"].removesuffix(".exe")
            if e["bundle"] == "full":
                self.assertTrue(stem.endswith(e["target"]), e["asset_name"])
            else:
                self.assertTrue(stem.endswith(f"{e['target']}-{e['bundle']}"), e["asset_name"])

    def test_gateway_and_static_server_are_built_without_default_features(self):
        """The two bundles are defined for `--no-default-features` builds; full and standard add to the defaults."""
        for e in matrix_entries():
            expected = "true" if e["bundle"] in ("gateway", "static") else "false"
            self.assertEqual(e["no_default_features"], expected, e["asset_name"])

    def test_each_bundle_builds_the_cargo_feature_of_its_name(self):
        """`static` is the bundle of the `static-server` feature; the rest carry their own name."""
        for e in matrix_entries():
            self.assertEqual(e["features"], "static-server" if e["bundle"] == "static" else e["bundle"])
        cargo = read("Cargo.toml")
        for feature in ("full", "standard", "gateway", "static-server"):
            self.assertRegex(cargo, rf"(?m)^{feature}\s*=\s*\[", f"Cargo.toml has no `{feature}` feature")

    def test_the_npm_package_asks_for_the_full_builds_the_workflow_publishes(self):
        """download.js and conduit.js name exactly the full assets of the five platforms npm supports."""
        full = sorted(
            e["asset_name"]
            for e in matrix_entries()
            if e["bundle"] == "full" and e["target"] != "x86_64-unknown-linux-musl"
        )
        self.assertEqual(npm_assets("npm/bin/download.js"), full)
        self.assertEqual(npm_assets("npm/bin/conduit.js"), full)

    def test_npm_vendors_only_the_full_binaries(self):
        """The npm job downloads `binary-full-*` and the build job names its artifacts by bundle."""
        workflow = read(".github/workflows/release.yml")
        self.assertIn("pattern: binary-full-*", workflow)
        self.assertIn("name: binary-${{ matrix.bundle }}-${{ matrix.target }}", workflow)

    def test_the_docker_matrix_has_the_same_four_bundles(self):
        """Full owns the plain tags; the other images carry `-standard`, `-gateway` and `-static`."""
        job = docker_job()
        self.assertEqual(re.findall(r"- bundle: (\w+)", job), ["full", "standard", "gateway", "static"])
        self.assertEqual(re.findall(r"tag_suffix: \"([^\"]*)\"", job), ["", "-standard", "-gateway", "-static"])
        self.assertEqual(
            re.findall(r"latest_tag: (\S+)", job), ["latest", "latest-standard", "latest-gateway", "latest-static"]
        )

    def test_the_metadata_action_does_not_add_its_own_latest_tag(self):
        """`flavor: latest=false`: otherwise every matrix job would also push a plain `latest` and race for it."""
        self.assertIn("flavor: latest=false", docker_job())


if __name__ == "__main__":
    unittest.main()
