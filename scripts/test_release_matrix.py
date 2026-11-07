#!/usr/bin/env python3
"""Checks that the names the release workflow publishes, the names the npm package asks for, and the Docker tags agree (#665).

    python3 scripts/test_release_matrix.py

The npm package downloads `conduit-<target>[.exe]` (the full build) from the release of its own version: if the
workflow renamed an asset and `npm/bin/download.js` did not follow, every `npm install` would fail at the download.
Plain text parsing, so that it runs on a bare Python without PyYAML.
"""
import fnmatch
import os
import re
import unittest

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

BUNDLES = ("full", "standard", "gateway", "static")
# Builds that exist only as the standard bundle: no other build is published for them.
STANDARD_ONLY = {"riscv64gc-unknown-linux-gnu"}
# npm's `process.platform-process.arch` keys and the release target each one downloads.
NPM_PLATFORMS = {
    "linux-x64": "x86_64-unknown-linux-gnu",
    "linux-arm64": "aarch64-unknown-linux-gnu",
    "darwin-x64": "x86_64-apple-darwin",
    "darwin-arm64": "aarch64-apple-darwin",
    "win32-x64": "x86_64-pc-windows-msvc",
    "win32-arm64": "aarch64-pc-windows-msvc",
}


def read(path):
    with open(os.path.join(ROOT, path), encoding="utf-8") as f:
        return f.read()


def workflow():
    return read(".github/workflows/release.yml")


def matrix_entries():
    """The build matrix of release.yml as dicts of the keys of each `- target:` entry."""
    text = workflow()
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


def asset_name(target, bundle):
    """The release file name of a build: the full build has the plain name, the others end in their bundle."""
    suffix = "" if bundle == "full" else f"-{bundle}"
    return f"conduit-{target}{suffix}{'.exe' if 'windows' in target else ''}"


def job_text(name, next_marker):
    """The text of one job of release.yml, up to the comment line that opens the next section."""
    text = workflow()
    return text[text.index(f"\n  {name}:\n"):text.index(next_marker)]


def docker_job():
    """The per-architecture build job."""
    return job_text("docker", "  # ── 4a. Join")


def manifest_job():
    """The job that joins the architectures into one manifest."""
    return job_text("docker-manifest", "  # ── 5. Publish to npm")


def npm_table(path):
    """The `"os-arch": "conduit-..."` pairs of an npm file's platform table, as a dict."""
    return dict(re.findall(r'"([a-z0-9]+-[a-z0-9]+)":\s*"(conduit-[^"]+)"', read(path)))


class ReleaseMatrix(unittest.TestCase):
    """The matrix, the npm tables, the bundles and the Docker images agree."""

    def test_the_builds_are_the_expected_set_and_every_asset_name_is_unique(self):
        """full, gateway and static on the eight main targets; standard on those and on RISC-V."""
        entries = matrix_entries()
        names = [e["asset_name"] for e in entries]
        self.assertEqual(len(names), len(set(names)), "two builds would publish the same file")
        counts = {b: sum(1 for e in entries if e["bundle"] == b) for b in BUNDLES}
        self.assertEqual(counts, {"full": 8, "standard": 9, "gateway": 8, "static": 8})
        for e in entries:
            if e["bundle"] != "standard":
                self.assertNotIn(e["target"], STANDARD_ONLY, f"{e['asset_name']} has no other build than standard")

    def test_every_asset_is_named_by_its_target_and_bundle(self):
        """The complete name, not only its ending: `conduit-<target>[-<bundle>][.exe]`."""
        for e in matrix_entries():
            self.assertEqual(e["asset_name"], asset_name(e["target"], e["bundle"]))

    def test_gateway_and_static_server_are_built_without_default_features(self):
        """The two bundles are defined for `--no-default-features` builds; full and standard add to the defaults."""
        for e in matrix_entries():
            expected = "true" if e["bundle"] in ("gateway", "static") else "false"
            self.assertEqual(e["no_default_features"], expected, e["asset_name"])

    def test_every_build_path_passes_the_flags(self):
        """The native build, the cross build and the Docker build all honour `no_default_features` and `features`."""
        text = workflow()
        for step in ("Build (native)", "Build (cross)"):
            body = text[text.index(f"- name: {step}"):]
            body = body[: body.index("      # ", 1) if "      # " in body[1:] else len(body)]
            self.assertIn('if [ "${{ matrix.no_default_features }}" = "true" ]; then flags="--no-default-features"; fi', body, step)
            self.assertIn('--features "${{ matrix.features }}"', body, step)
            self.assertIn("$flags", body, step)
        self.assertIn("NO_DEFAULT_FEATURES=${{ matrix.no_default_features }}", docker_job())
        self.assertIn("FEATURES=${{ matrix.features }}", docker_job())
        dockerfile = read("contrib/Dockerfile")
        self.assertIn('if [ "$NO_DEFAULT_FEATURES" = "true" ]; then flags="--no-default-features"; fi', dockerfile)
        self.assertEqual(dockerfile.count("$flags"), 2, "both cargo build commands of the Dockerfile take the flags")

    def test_each_bundle_builds_the_cargo_feature_of_its_name(self):
        """`static` is the bundle of the `static-server` feature; the rest carry their own name."""
        for e in matrix_entries():
            self.assertEqual(e["features"], "static-server" if e["bundle"] == "static" else e["bundle"])
        cargo = read("Cargo.toml")
        for feature in ("full", "standard", "gateway", "static-server"):
            self.assertRegex(cargo, rf"(?m)^{feature}\s*=\s*\[", f"Cargo.toml has no `{feature}` feature")

    def test_each_npm_platform_downloads_its_own_full_asset(self):
        """Per row, not as a set: a swap of two rows (linux-x64 and linux-arm64) must fail."""
        expected = {
            key: f"conduit-{target}{'.exe' if 'windows' in target else ''}" for key, target in NPM_PLATFORMS.items()
        }
        self.assertEqual(npm_table("npm/bin/download.js"), expected)
        self.assertEqual(npm_table("npm/bin/conduit.js"), expected)
        published = {e["asset_name"] for e in matrix_entries() if e["bundle"] == "full"}
        self.assertTrue(set(expected.values()) <= published, "npm asks for a file the release does not publish")

    def test_npm_vendors_exactly_the_full_binaries(self):
        """The npm job's artifact pattern matches the full builds' artifact names and no other build's."""
        text = workflow()
        pattern = re.search(r"Download the full-build binaries.*?pattern: (\S+)", text, re.S).group(1)
        self.assertIn("name: binary-${{ matrix.bundle }}-${{ matrix.target }}", text)
        matched = sorted(
            e["asset_name"] for e in matrix_entries() if fnmatch.fnmatch(f"binary-{e['bundle']}-{e['target']}", pattern)
        )
        full = sorted(e["asset_name"] for e in matrix_entries() if e["bundle"] == "full")
        self.assertEqual(matched, full)

    def test_every_bundle_has_an_image_for_both_architectures(self):
        """Eight per-architecture builds on native runners, pushed by digest."""
        job = docker_job()
        legs = re.findall(r"- bundle: (\w+)\n\s+features: \S+\n\s+no_default_features: \"[^\"]*\"\n\s+arch: (\w+)\n\s+runner: (\S+)", job)
        expected = [(b, a, r) for b in BUNDLES for a, r in (("amd64", "ubuntu-latest"), ("arm64", "ubuntu-24.04-arm"))]
        self.assertEqual(legs, expected)
        self.assertIn("push-by-digest=true", job)
        self.assertIn("platforms: linux/${{ matrix.arch }}", job)

    def test_the_manifest_job_gives_full_the_plain_tags(self):
        """Full owns the plain tags; the other images carry `-standard`, `-gateway` and `-static`."""
        job = manifest_job()
        self.assertEqual(re.findall(r"- bundle: (\w+)", job), list(BUNDLES))
        self.assertEqual(re.findall(r"tag_suffix: \"([^\"]*)\"", job), ["", "-standard", "-gateway", "-static"])
        self.assertEqual(
            re.findall(r"latest_tag: (\S+)", job), ["latest", "latest-standard", "latest-gateway", "latest-static"]
        )

    def test_the_metadata_action_does_not_add_its_own_latest_tag(self):
        """`flavor: latest=false`: otherwise every image would also push a plain `latest` and race for it."""
        self.assertIn("flavor: latest=false", manifest_job())

    def test_a_prerelease_tag_does_not_move_the_latest_tags(self):
        """The raw `latest*` tag is enabled only when the git tag has no `-` (a stable version)."""
        self.assertIn("enable=${{ !contains(github.ref_name, '-') }}", manifest_job())


    def test_the_deb_is_built_from_the_two_static_full_builds(self):
        """amd64 and arm64 packages, from the full musl builds, uploaded as `binary-*` so they are attested and attached."""
        text = workflow()
        condition = "if: matrix.bundle == 'full' && endsWith(matrix.target, '-linux-musl')"
        self.assertEqual(text.count(condition), 2, "the build step and the upload step")
        self.assertIn('scripts/build-deb.sh "dist/${{ matrix.asset_name }}" "$arch" "${GITHUB_REF_NAME#v}" deb-out', text)
        self.assertIn('if [ "${{ matrix.target }}" = "aarch64-unknown-linux-musl" ]; then arch=arm64; fi', text)
        self.assertIn("name: binary-deb-${{ matrix.target }}", text)
        musl_full = sorted(e["target"] for e in matrix_entries() if e["bundle"] == "full" and e["target"].endswith("-linux-musl"))
        self.assertEqual(musl_full, ["aarch64-unknown-linux-musl", "x86_64-unknown-linux-musl"])


if __name__ == "__main__":
    unittest.main()
