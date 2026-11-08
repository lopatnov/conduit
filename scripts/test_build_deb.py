#!/usr/bin/env python3
"""Tests of scripts/build-deb.sh: what it refuses, and (where dpkg-deb exists) the file it writes.

    python3 scripts/test_build_deb.py

The version of a release comes from a git tag, which may contain anything, and goes into a sed script and a file
name, so the script accepts only a semantic version.
"""
import os
import shutil
import stat
import subprocess
import tempfile
import unittest

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SCRIPT = os.path.join(ROOT, "scripts", "build-deb.sh")


def run(binary, arch, version, outdir):
    """Run the script and return (exit status, stderr, stdout)."""
    done = subprocess.run(["bash", SCRIPT, binary, arch, version, outdir], capture_output=True, text=True)
    return done.returncode, done.stderr, done.stdout


class BuildDeb(unittest.TestCase):
    """The checks of the arguments and the package it builds."""

    def setUp(self):
        self.tmp = tempfile.mkdtemp()
        self.binary = os.path.join(self.tmp, "conduit")
        with open(self.binary, "w", encoding="utf-8") as f:
            f.write("#!/bin/sh\necho conduit\n")
        os.chmod(self.binary, os.stat(self.binary).st_mode | stat.S_IXUSR)
        self.out = os.path.join(self.tmp, "out")

    def tearDown(self):
        shutil.rmtree(self.tmp, ignore_errors=True)

    def test_only_a_semantic_version_is_accepted(self):
        """Anything else is refused before a file is written, including what would reach sed."""
        for bad in ("v2.2.0", "2.2", "2.2.0.1", "1/;e id", "2.2.0 ", "2.2.0-", "$(id)", "2.2.0&x", "", "a.b.c"):
            code, err, _ = run(self.binary, "amd64", bad, self.out)
            self.assertEqual(code, 2, repr(bad))
            self.assertIn("VERSION must be like 2.2.0", err, repr(bad))
            self.assertFalse(os.path.exists(self.out), f"{bad!r} must not create the output directory")

    def test_the_architecture_must_be_amd64_or_arm64(self):
        """The Debian names, not the Rust targets."""
        for bad in ("x86_64", "aarch64", "riscv64", ""):
            code, err, _ = run(self.binary, bad, "2.2.0", self.out)
            self.assertEqual(code, 2, repr(bad))
            self.assertIn("ARCH must be amd64 or arm64", err)

    def test_the_binary_must_be_an_executable_file(self):
        """A missing file or one without the execute bit is refused."""
        code, err, _ = run(os.path.join(self.tmp, "absent"), "amd64", "2.2.0", self.out)
        self.assertEqual(code, 2)
        self.assertIn("is not an executable file", err)

    @unittest.skipUnless(shutil.which("dpkg-deb"), "dpkg-deb is not installed")
    def test_it_builds_the_package_and_a_prerelease_sorts_before_the_release(self):
        """2.2.0 gives conduit_2.2.0_amd64.deb; 2.2.0-rc.1 becomes 2.2.0~rc.1, which sorts first."""
        code, err, out = run(self.binary, "amd64", "2.2.0", self.out)
        self.assertEqual(code, 0, err)
        self.assertEqual(os.path.basename(out.strip()), "conduit_2.2.0_amd64.deb")
        info = subprocess.run(["dpkg-deb", "-f", out.strip()], capture_output=True, text=True).stdout
        self.assertIn("\nPackage: conduit\n", "\n" + info)  # the whole line: "conduit-proxy" would also contain it
        self.assertIn("\nReplaces: conduit-proxy\n", "\n" + info)
        self.assertIn("\nConflicts: conduit-proxy\n", "\n" + info)
        self.assertIn("Architecture: amd64", info)
        code, err, out = run(self.binary, "arm64", "2.2.0-rc.1", self.out)
        self.assertEqual(code, 0, err)
        self.assertEqual(os.path.basename(out.strip()), "conduit_2.2.0~rc.1_arm64.deb")
        listing = subprocess.run(["dpkg-deb", "-c", out.strip()], capture_output=True, text=True).stdout
        for path in ("./usr/bin/conduit", "./etc/conduit/conduit.yaml", "./usr/lib/systemd/system/conduit.service"):
            self.assertIn(path, listing)


if __name__ == "__main__":
    unittest.main()
