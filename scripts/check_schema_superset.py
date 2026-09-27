#!/usr/bin/env python3
"""Best-effort check that schema/conduit.schema.json stays a superset of the
Rust config structs it's meant to describe (issue #496).

schema/conduit.schema.json is hand-maintained, not generated from `serde` —
nothing enforces that it actually matches the real config types. Full
schemars-based generation isn't practical here: several config enums are
`#[serde(untagged)]` (ConfigFile, ProxyConfig, the bool/object shorthands —
see CLAUDE.md decisions #4/#6/#7), which schemars doesn't represent the way
this schema is hand-shaped.

What this script does instead (the weaker, cheaper option #496 itself
proposed): for every top-level `$defs` entry in the schema, look for a Rust
`struct` of the same name, extract its serde-visible field names (honoring
`#[serde(rename)]`/`rename_all`/`skip`/`skip_deserializing`), and diff that
set against the schema's own `properties` keys for that def.

Known, accepted limitations (report these as SKIPPED, not failures):
  - `$defs` entries backed by a Rust *enum* rather than a struct (the
    untagged bool/object shorthands, `ProxyConfig`, `ProxyTarget`, etc.) —
    resolving every variant's fields is out of scope for this pass.
  - Structs containing a `#[serde(flatten)]` field — the flattened type's
    fields can't be resolved without deeper type introspection.
  - Config types that are inlined directly into a parent's schema block
    instead of getting their own named `$defs` entry (e.g. `CacheConfig`,
    `LimitsConfig`) — this script only walks named `$defs`, so these are
    never checked at all. A gap left for a future, more thorough pass.

This is a net, not a guarantee: a clean run means the checked subset didn't
drift, not that the whole schema is provably a superset.
"""
from __future__ import annotations

import json
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SCHEMA_PATH = ROOT / "schema" / "conduit.schema.json"

SERDE_RENAME_RE = re.compile(r'rename\s*=\s*"([^"]+)"')
SERDE_RENAME_ALL_RE = re.compile(r'rename_all\s*=\s*"([^"]+)"')
FIELD_RE = re.compile(r"^\s*pub\s+([a-zA-Z_][a-zA-Z0-9_]*)\s*:")
STRUCT_DEF_RE = re.compile(r"^\s*pub\s+struct\s+(\w+)\b")
ENUM_DEF_RE = re.compile(r"^\s*pub\s+enum\s+(\w+)\b")


def snake_to_camel(name: str) -> str:
    """Match serde's `rename_all = "camelCase"` (the `heck` crate): only the
    first character of each subsequent word is uppercased, not every
    alphanumeric run — `consecutive_5xx` -> `consecutive5xx`, not
    `consecutive5Xx` (which is what Python's `str.title()` would give)."""
    parts = name.split("_")
    return parts[0] + "".join(p[:1].upper() + p[1:] for p in parts[1:])


def strip_comment_lines(lines: list[str]) -> list[str]:
    """Drop any line whose trimmed content is a `//`/`///` comment.

    Doc-comment lines can contain YAML/Rust example snippets with braces
    (e.g. `{{ jwt.sub }}`) that would otherwise confuse brace-depth
    matching of the struct body; removing the whole line sidesteps that.
    """
    out = []
    for line in lines:
        if line.strip().startswith("//"):
            continue
        out.append(line)
    return out


def find_struct_span(lines: list[str], struct_name: str) -> tuple[int, int, str] | None:
    """Return (start_idx, end_idx_inclusive, attrs_text_before) for `pub struct <name> { ... }`."""
    for i, line in enumerate(lines):
        m = STRUCT_DEF_RE.match(line)
        if m and m.group(1) == struct_name:
            # Gather attribute/derive lines immediately above (contiguous, skipping blanks).
            j = i - 1
            attr_lines = []
            while j >= 0 and (lines[j].strip().startswith("#[") or lines[j].strip() == "" or lines[j].rstrip().endswith("]")):
                if lines[j].strip() == "":
                    j -= 1
                    continue
                attr_lines.append(lines[j])
                if lines[j].strip().startswith("#["):
                    j -= 1
                    break
                j -= 1
            attrs_text = "\n".join(reversed(attr_lines))

            # Find matching closing brace by depth counting from this line onward.
            depth = 0
            started = False
            for k in range(i, len(lines)):
                for ch in lines[k]:
                    if ch == "{":
                        depth += 1
                        started = True
                    elif ch == "}":
                        depth -= 1
                if started and depth == 0:
                    return i, k, attrs_text
            return None
    return None


def extract_fields(body_lines: list[str], rename_all: str | None) -> set[str] | None:
    """Return the serde-visible field name set, or None if the struct has
    a `#[serde(flatten)]` field (unresolvable without type introspection)."""
    fields: set[str] = set()
    pending_attr = ""
    in_attr = False
    for line in body_lines:
        stripped = line.strip()
        if not stripped:
            continue
        if in_attr:
            pending_attr += "\n" + line
            if stripped.endswith(")]") or stripped == "]":
                in_attr = False
            continue
        if stripped.startswith("#["):
            pending_attr = line
            if not (stripped.endswith(")]") or stripped.endswith("]") and stripped.count("[") == stripped.count("]")):
                in_attr = True
            continue
        m = FIELD_RE.match(line)
        if m:
            field_name = m.group(1)
            if "serde" in pending_attr:
                if re.search(r"\bskip\b", pending_attr) and "skip_serializing_if" not in pending_attr:
                    pending_attr = ""
                    continue
                if "flatten" in pending_attr:
                    return None
                rn = SERDE_RENAME_RE.search(pending_attr)
                if rn:
                    fields.add(rn.group(1))
                    pending_attr = ""
                    continue
            if rename_all == "camelCase":
                fields.add(snake_to_camel(field_name))
            else:
                fields.add(field_name)
            pending_attr = ""
    return fields


def find_rust_file_for(name: str) -> Path | None:
    try:
        out = subprocess.run(
            ["grep", "-rl", rf"struct {name}\b", "--include=*.rs", str(ROOT / "crates")],
            capture_output=True, text=True, check=False,
        ).stdout.strip()
    except FileNotFoundError:
        out = ""
    if not out:
        return None
    return Path(out.splitlines()[0])


def find_enum_file_for(name: str) -> Path | None:
    out = subprocess.run(
        ["grep", "-rl", rf"enum {name}\b", "--include=*.rs", str(ROOT / "crates")],
        capture_output=True, text=True, check=False,
    ).stdout.strip()
    if not out:
        return None
    return Path(out.splitlines()[0])


def main() -> int:
    schema = json.loads(SCHEMA_PATH.read_text())
    defs = schema.get("$defs", {})

    mismatches = 0
    checked = 0
    skipped: list[str] = []

    for def_name, def_schema in defs.items():
        if not isinstance(def_schema, dict) or def_schema.get("type") != "object":
            skipped.append(f"{def_name} (not a plain object def)")
            continue
        schema_props = set(def_schema.get("properties", {}).keys())

        rs_file = find_rust_file_for(def_name)
        if rs_file is None:
            if find_enum_file_for(def_name):
                skipped.append(f"{def_name} (Rust enum, not a struct — untagged/shorthand type, out of scope)")
            else:
                skipped.append(f"{def_name} (no matching Rust struct found)")
            continue

        raw_lines = rs_file.read_text().splitlines()
        span = find_struct_span(strip_comment_lines(raw_lines), def_name)
        if span is None:
            skipped.append(f"{def_name} (struct body not parseable in {rs_file})")
            continue
        start, end, attrs_text = span
        rename_all = None
        m = SERDE_RENAME_ALL_RE.search(attrs_text)
        if m:
            rename_all = m.group(1)

        clean_lines = strip_comment_lines(raw_lines)
        body = clean_lines[start:end + 1]
        rust_fields = extract_fields(body, rename_all)
        if rust_fields is None:
            skipped.append(f"{def_name} (has #[serde(flatten)] — unresolvable here)")
            continue

        checked += 1
        missing_from_schema = rust_fields - schema_props
        if missing_from_schema:
            mismatches += 1
            print(
                f"MISMATCH: {def_name} ({rs_file.relative_to(ROOT)}) has field(s) "
                f"not present in schema/conduit.schema.json's $defs.{def_name}.properties: "
                f"{sorted(missing_from_schema)}",
                file=sys.stderr,
            )

    print(f"Checked {checked} $defs against their Rust struct. {len(skipped)} skipped (see below).")
    if skipped:
        print("Skipped (known limitations — not failures):")
        for s in skipped:
            print(f"  - {s}")

    if mismatches:
        print(f"\n{mismatches} $defs have Rust fields missing from the schema. Fix schema/conduit.schema.json.", file=sys.stderr)
        return 1

    print("\nOK: no drift found in the checked subset.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
