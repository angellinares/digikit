#!/usr/bin/env python3
"""Check that every SHARC_* and DIGI_* environment name in the sources is listed in config/env-registry.json.

    uv run python -m tools.devtools.env_registry [--list]

A name is any quoted string literal that starts with the SHARC_ or DIGI_ prefix, in tracked Rust, TypeScript and Python files.
The registry has `vars` (name -> owner and purpose), `prefixes` (names built at run time from a prefix) and
`not_env` (names that look like variables but are not). An unlisted name fails the check.
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
REGISTRY = ROOT / "config" / "env-registry.json"
LITERAL = re.compile(r"""["'`]((?:SHARC|DIGI)_[A-Z0-9_]+)["'`]""")
GLOBS = ("*.rs", "*.ts", "*.tsx", "*.mjs", "*.py")


def names_in_sources() -> dict[str, str]:
    """Name -> first file:line that holds it."""
    files = subprocess.run(
        ["git", "ls-files", *GLOBS],
        cwd=ROOT,
        capture_output=True,
        text=True,
        check=True,
    ).stdout.split()
    found: dict[str, str] = {}
    for rel in files:
        try:
            lines = (ROOT / rel).read_text(errors="ignore").split("\n")
        except OSError:
            continue
        for number, line in enumerate(lines, 1):
            for match in LITERAL.finditer(line):
                found.setdefault(match.group(1), f"{rel}:{number}")
    return found


def unlisted(found: dict[str, str], registry: dict) -> list[str]:
    known = set(registry["vars"]) | set(registry.get("not_env", {}))
    prefixes = tuple(registry.get("prefixes", {}))
    return sorted(n for n in found if n not in known and not n.startswith(prefixes))


def main() -> int:
    argparse.ArgumentParser(description=__doc__).parse_args()
    registry = json.loads(REGISTRY.read_text())
    found = names_in_sources()
    bad = unlisted(found, registry)
    for name in bad:
        print(
            f"{found[name]}: env-registry: {name} is not in config/env-registry.json. "
            "Instead: add it with an owner and a purpose (or to `prefixes` / `not_env`)."
        )
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
