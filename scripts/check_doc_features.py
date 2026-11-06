#!/usr/bin/env python3
"""Check that the documentation describes every Cargo feature, and describes the bundles as Cargo defines them (#568).

    python3 scripts/check_doc_features.py [--cargo Cargo.toml] [--config docs/configuration.md] [--cli docs/cli.md]

Reads the `[features]` table of the root Cargo.toml and checks three things:

1. docs/configuration.md, section "Optional build features": every feature except `default` has a row in one of
   its tables (a row starts with the feature's name in backticks);
2. the same section's bundle table: for each bundle column (`standard`, `gateway`, `static-server`, `full`) the
   features marked with a check mark are exactly the features the bundle turns on in Cargo.toml (following
   features that name other features, so `cache` brings `proxy`); a row may name several features;
3. docs/cli.md, "Feature overview": every feature except `default` has a row there too.

A feature added to Cargo.toml without a line of documentation, or a bundle that changed without its table, fails
the check. Exit status: 0 all good, 1 something is missing or wrong, 2 a file cannot be read or has no such section.
"""
import argparse
import os
import re
import sys
import tomllib

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BUNDLES = ("standard", "gateway", "static-server", "full")
CHECK = "✓"
CONFIG_SECTION = "### Optional build features"
CLI_SECTION = "### Feature overview"
ROW_NAMES = re.compile(r"`([a-z][a-z0-9-]*)`")


class DocError(Exception):
    """A file that cannot be used for the check (missing, or without the section)."""


def read(path):
    """The text of `path`, or a DocError that says why not."""
    try:
        with open(path, encoding="utf-8") as handle:
            return handle.read()
    except OSError as err:
        raise DocError(f"cannot read {path}: {err}") from err


def features(cargo_text):
    """The `[features]` table of a Cargo.toml: {name: [entries]}."""
    return tomllib.loads(cargo_text).get("features", {})


def closure(table, name):
    """Every feature `name` turns on, itself included. Entries that are not features (`dep:x`, `crate/feature`) are skipped."""
    seen, todo = set(), [name]
    while todo:
        current = todo.pop()
        if current in seen or current not in table:
            continue
        seen.add(current)
        todo.extend(table[current])
    return seen


def section(text, heading, stop_at):
    """The lines from `heading` to the first line that starts with one of `stop_at`."""
    lines = text.split("\n")
    try:
        start = lines.index(heading)
    except ValueError as err:
        raise DocError(f"no '{heading}' section") from err
    out = []
    for line in lines[start + 1:]:
        if any(line.startswith(stop) for stop in stop_at):
            break
        out.append(line)
    return out


def rows(lines):
    """Table rows as lists of stripped cells (the separator row is dropped)."""
    result = []
    for line in lines:
        if not line.startswith("|"):
            continue
        cells = [cell.strip() for cell in line.strip().strip("|").split("|")]
        if all(re.fullmatch(r":?-+:?", cell) for cell in cells):
            continue
        result.append(cells)
    return result


def tables(lines):
    """The tables of `lines`: each a list of consecutive table lines."""
    found, current = [], []
    for line in lines:
        if line.startswith("|"):
            current.append(line)
        elif current:
            found.append(current)
            current = []
    if current:
        found.append(current)
    return found


def is_bundle_table(table):
    """True for the table whose header names the bundles (a matrix, not a description of each feature)."""
    header = rows(table)[0] if rows(table) else []
    return any(cell.strip("`") in BUNDLES for cell in header[1:])


def documented(lines):
    """Feature names that start a row (the first cell) of a description table in `lines`; the bundle matrix does not count."""
    names = set()
    for table in tables(lines):
        if is_bundle_table(table):
            continue
        for cells in rows(table):
            names.update(ROW_NAMES.findall(cells[0].replace("(default)", "")))
    return names


def bundle_columns(lines):
    """From the bundle table: {bundle: set of features marked with a check mark}. None when there is no such table."""
    for index, cells in enumerate(rows(lines)):
        if cells and cells[0] == "Feature" and any(cell.strip("`") in BUNDLES for cell in cells[1:]):
            header = cells
            table = rows(lines)[index + 1:]
            break
    else:
        return None
    marked = {cell.strip("`"): set() for cell in header[1:] if cell.strip("`") in BUNDLES}
    for cells in table:
        names = ROW_NAMES.findall(cells[0])
        if not names:
            break
        for column, cell in zip(header[1:], cells[1:]):
            if column.strip("`") in marked and CHECK in cell:
                marked[column.strip("`")].update(names)
    return marked


def check(cargo_text, config_text, cli_text):
    """Compare the docs with Cargo.toml; return a list of problem descriptions (empty when all is well)."""
    table = features(cargo_text)
    names = sorted(name for name in table if name != "default")
    problems = []

    config = section(config_text, CONFIG_SECTION, ("### ", "---"))
    # The section holds #### sub-headings; only a heading of its own level or a rule ends it.
    config_names = documented(config)
    # A bundle is documented by its column in the bundle table (checked below); every other feature needs a row.
    for name in names:
        if name not in config_names and name not in BUNDLES:
            problems.append(f"docs/configuration.md: feature `{name}` has no row in '{CONFIG_SECTION[4:]}'")
    for name in sorted(config_names - set(names)):
        problems.append(f"docs/configuration.md: `{name}` is documented as a feature but is not in Cargo.toml [features]")

    marked = bundle_columns(config)
    if marked is None:
        problems.append("docs/configuration.md: no bundle table (a table whose header lists the bundles)")
    else:
        for bundle in BUNDLES:
            if bundle not in table:
                continue
            if bundle not in marked:
                problems.append(f"docs/configuration.md: the bundle table has no column for `{bundle}`")
                continue
            # The table lists plain features; a bundle that includes another bundle includes that bundle's features.
            expected = closure(table, bundle) - set(BUNDLES)
            if marked[bundle] != expected:
                missing = sorted(expected - marked[bundle])
                extra = sorted(marked[bundle] - expected)
                detail = []
                if missing:
                    detail.append("missing " + ", ".join(f"`{n}`" for n in missing))
                if extra:
                    detail.append("not in the bundle: " + ", ".join(f"`{n}`" for n in extra))
                problems.append(f"docs/configuration.md: bundle `{bundle}` differs from Cargo.toml ({'; '.join(detail)})")

    cli = section(cli_text, CLI_SECTION, ("### ", "## "))
    cli_names = documented(cli)
    for name in names:
        if name not in cli_names:
            problems.append(f"docs/cli.md: feature `{name}` has no row in '{CLI_SECTION[4:]}'")
    return problems


def main(argv=None):
    """Command-line entry point: 0 all good, 1 problems found, 2 unusable input."""
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    parser.add_argument("--cargo", default=os.path.join(REPO, "Cargo.toml"))
    parser.add_argument("--config", default=os.path.join(REPO, "docs", "configuration.md"))
    parser.add_argument("--cli", default=os.path.join(REPO, "docs", "cli.md"))
    args = parser.parse_args(argv)
    try:
        problems = check(read(args.cargo), read(args.config), read(args.cli))
    except (DocError, tomllib.TOMLDecodeError) as err:
        print(f"error: {err}", file=sys.stderr)
        return 2
    for problem in problems:
        print(f"FAIL {problem}")
    if problems:
        return 1
    print("every Cargo feature is documented and the bundle table matches Cargo.toml")
    return 0


if __name__ == "__main__":
    sys.exit(main())
