#!/usr/bin/env python3
"""Build every Rust plugin of the WASM guide and let `conduit validate` look at it, so that a sample cannot rot (issue #565).

    python3 scripts/check_doc_wasm.py [--conduit "path/to/conduit"] [--cargo cargo] [--keep DIR] [FILE ...]

It finds the fenced ```rust blocks of docs/wasm.md (or of the files given) that are marked with an HTML comment on
the line before the fence, invisible in the rendered page:

    <!-- docs-check: plugin api_key_check -->

Each marked block is the whole `src/lib.rs` of a plugin named as the marker says. The script writes it into a
temporary crate (`crate-type = ["cdylib"]`, edition 2021, the release profile the guide recommends), builds it for
`wasm32-unknown-unknown`, and runs `conduit validate` on a configuration that points a `wasm` middleware at the result.
`validate` reads, compiles and links the module, so a sample that does not build, does not export `on_request`, or
imports a host function that does not exist fails here. It does not run the plugin; the behaviour of the first plugin
is shown by `scripts/repro-wasm-guide.sh`, which takes the modules this script leaves in the directory
given with `--keep`.

Needs `rustup target add wasm32-unknown-unknown` and a conduit built with `--features wasm` (or `full`).
Exit status: 0 all good, 1 something failed, 2 usage or setup error.
"""
import argparse
import os
import re
import shlex
import shutil
import subprocess
import sys
import tempfile

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
FENCE = re.compile(r"^(?P<indent>[ \t]*)(?P<fence>`{3,}|~{3,})(?P<info>.*)$")
MARKER = re.compile(r"^\s*<!--\s*docs-check:\s*plugin\s+(?P<name>[A-Za-z][A-Za-z0-9_-]*)\s*-->\s*$")
TIMEOUT_SECS = 600
CARGO_TOML = """[package]
name = "{name}"
version = "0.1.0"
edition = "2021"

[lib]
crate-type = ["cdylib"]

[profile.release]
opt-level = "s"
strip = true
"""


def extract(path, text):
    """Return (plugins, problems): plugins are (name, source, line) of marked rust blocks."""
    plugins, problems = [], []
    rel = os.path.relpath(path, REPO)
    pending = None  # (line, name) of a marker waiting for its fence
    open_fence = None  # (char, length, start line, lang, collected lines, name)
    for number, line in enumerate(text.splitlines(), start=1):
        if open_fence is None:
            marker = MARKER.match(line)
            if marker:
                if pending is not None:
                    problems.append(f"{rel}:{pending[0]}: plugin marker is not followed by a fence")
                pending = (number, marker.group("name"))
                continue
            fence = FENCE.match(line)
            if fence:
                info = fence.group("info").strip()
                lang = info.split()[0].lower() if info else ""
                name = None
                if pending is not None:
                    name = pending[1]
                    if lang != "rust":
                        problems.append(f"{rel}:{pending[0]}: plugin marker is followed by a {lang or 'plain'} fence, not rust")
                        name = None
                    pending = None
                open_fence = (fence.group("fence")[0], len(fence.group("fence")), number, lang, [], name)
            elif pending is not None and line.strip():
                problems.append(f"{rel}:{pending[0]}: plugin marker is not followed by a fence")
                pending = None
            continue
        char, length, start, lang, body, name = open_fence
        close = FENCE.match(line)
        if close and close.group("fence")[0] == char and len(close.group("fence")) >= length and not close.group("info").strip():
            if name is not None:
                plugins.append((name, "\n".join(body) + "\n", start))
            open_fence = None
        else:
            body.append(line)
    if open_fence is not None:
        problems.append(f"{rel}:{open_fence[2]}: the fence opened here is never closed")
    if pending is not None:
        problems.append(f"{rel}:{pending[0]}: plugin marker is not followed by a fence")
    names = {}
    for name, _, line in plugins:
        if name in names:
            problems.append(f"{rel}:{line}: plugin {name!r} is also defined at line {names[name]}")
        names.setdefault(name, line)
    return plugins, problems


def build(cargo, name, source, workdir, target_dir):
    """Build one plugin; return (path of the .wasm, None) or (None, output excerpt)."""
    crate = os.path.join(workdir, name)
    os.makedirs(os.path.join(crate, "src"))
    with open(os.path.join(crate, "Cargo.toml"), "w", encoding="utf-8", newline="\n") as f:
        f.write(CARGO_TOML.format(name=name))
    with open(os.path.join(crate, "src", "lib.rs"), "w", encoding="utf-8", newline="\n") as f:
        f.write(source)
    env = dict(os.environ, CARGO_TARGET_DIR=target_dir)
    done = subprocess.run(
        [*cargo, "build", "--release", "--target", "wasm32-unknown-unknown", "--manifest-path", os.path.join(crate, "Cargo.toml")],
        capture_output=True, text=True, timeout=TIMEOUT_SECS, encoding="utf-8", errors="replace", env=env,
    )
    if done.returncode != 0:
        lines = (done.stdout + done.stderr).strip().splitlines()
        return None, "\n".join(lines[-15:])
    wasm = os.path.join(target_dir, "wasm32-unknown-unknown", "release", name.replace("-", "_") + ".wasm")
    if not os.path.exists(wasm):
        return None, f"the build succeeded but {wasm} does not exist"
    return wasm, None


def validate(conduit, wasm, workdir, name):
    """Run `conduit validate` on a configuration that uses the plugin; return (ok, output excerpt)."""
    config = os.path.join(workdir, f"{name}.yaml")
    with open(config, "w", encoding="utf-8", newline="\n") as f:
        f.write(f"sites:\n  - port: 8080\n    proxy: http://127.0.0.1:4000\n    middleware:\n      - type: wasm\n        path: \"{wasm.replace(chr(92), '/')}\"\n")
    done = subprocess.run([*conduit, "validate", "-c", config], capture_output=True, text=True, timeout=60, encoding="utf-8", errors="replace")
    output = (done.stdout + done.stderr).strip()
    return done.returncode == 0, "\n".join(output.splitlines()[:6])


def main(argv=None):
    """Command-line entry point."""
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("files", nargs="*", help="markdown files to check (default: docs/wasm.md)")
    parser.add_argument("--conduit", default=os.environ.get("CONDUIT_BIN", "conduit"), help="the conduit binary (default: $CONDUIT_BIN or `conduit` on PATH)")
    parser.add_argument("--cargo", default="cargo", help="the cargo command")
    parser.add_argument("--keep", metavar="DIR", help="copy every module that built into DIR (created if needed)")
    args = parser.parse_args(argv)
    conduit = [t.strip('"') for t in shlex.split(args.conduit, posix=os.name != "nt")]
    cargo = shlex.split(args.cargo, posix=os.name != "nt")
    try:
        subprocess.run([*conduit, "--version"], capture_output=True, timeout=60, check=False)
        subprocess.run([*cargo, "--version"], capture_output=True, timeout=60, check=True)
    except (OSError, subprocess.SubprocessError) as e:
        print(f"cannot run {args.conduit!r} or {args.cargo!r}: {e}", file=sys.stderr)
        return 2
    files = [os.path.abspath(f) for f in args.files] or [os.path.join(REPO, "docs", "wasm.md")]
    plugins, problems = [], []
    for path in files:
        try:
            with open(path, encoding="utf-8") as f:
                found, bad = extract(path, f.read())
        except (OSError, UnicodeDecodeError) as e:
            print(f"cannot read {path}: {e}", file=sys.stderr)
            return 2
        plugins += found
        problems += bad
    failures = []
    with tempfile.TemporaryDirectory() as workdir:
        target_dir = os.path.join(workdir, "target")
        for name, source, line in plugins:
            wasm, why = build(cargo, name, source, workdir, target_dir)
            if wasm is None:
                failures.append((f"{name} (line {line}): build", why))
                continue
            if args.keep:
                os.makedirs(args.keep, exist_ok=True)
                shutil.copy(wasm, os.path.join(args.keep, name.replace("-", "_") + ".wasm"))
            ok, detail = validate(conduit, wasm, workdir, name)
            if not ok:
                failures.append((f"{name} (line {line}): validate", detail))
    for line in problems:
        print(f"PROBLEM  {line}")
    for where, detail in failures:
        print(f"FAIL     {where}")
        for line in detail.splitlines():
            print(f"           {line}")
    print(f"{len(plugins)} plugins built and validated, {len(failures)} failed, {len(problems)} marker or fence problems")
    return 1 if failures or problems or not plugins else 0


if __name__ == "__main__":
    sys.exit(main())
