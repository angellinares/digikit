#!/usr/bin/env python3
"""Create a run-time content module from a template (`mise run module:new -- machine|fx|tool AUTHOR/NAME [--dir DIR]`).

    uv run python -m tools.patchkit.pk_new machine alice/drone         # a SHARC+ voice machine
    uv run python -m tools.patchkit.pk_new fx alice/crunch --title Crunch --short CRU
    uv run python -m tools.patchkit.pk_new tool alice/hello --dir ~/my-modules

The templates are the source trees in sdk/templates/<kind>/. The tokens `__ID__`, `__AUTHOR__`, `__NAME__` (lower snake),
`__UPPER__`, `__TITLE__`, `__SHORT__` and `__TAB__` (the first 3 characters of the short name) are replaced in file names and file contents. The module goes to
modules/machines/NAME, modules/fx/NAME or modules/tools/NAME, or to `DIR/NAME` with `--dir` (a folder that PATCHKIT_MODULES
names finds it). Then the objects are built with `patchkit build` and `patchkit gen` writes the generated
headers, so the template is known to compile. A run-time machine has no permanent type number: the runtime assigns one at
load (docs/design/module-sdk.md 11.5 D7). `core` and `framework` are refused as author.
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent

MODULES = ROOT / "modules"
TEMPLATES = ROOT / "sdk" / "templates"
KIND_DIRS = {"machine": "machines", "fx": "fx", "tool": "tools"}

AUTHOR_RE = re.compile(r"[a-z0-9]+(-[a-z0-9]+)*")
NAME_RE = re.compile(r"[a-z][a-z0-9]*(_[a-z0-9]+)*")
SHORT_RE = re.compile(r"[A-Z0-9]{3,6}")
TOKEN_RE = re.compile(r"__(ID|AUTHOR|NAME|UPPER|TITLE|SHORT|TAB)__")


def tokens(
    id_: str, title: str | None = None, short: str | None = None
) -> dict[str, str]:
    """The token values of a module id `AUTHOR/NAME`; refuses an invalid id, title or short name."""
    author, _, name = id_.partition("/")
    if not AUTHOR_RE.fullmatch(author) or not NAME_RE.fullmatch(name):
        raise SystemExit(
            f"module id is AUTHOR/NAME (author: lowercase, digits, dashes; name: lower snake case): {id_!r}"
        )
    if author == "core":
        raise SystemExit("core modules are internal: pick another author")
    if author == "framework":
        raise SystemExit(
            "framework is a module type of the core modules, not an author"
        )
    title = title or name.replace("_", " ").title()
    if not 1 <= len(title) <= 23:
        raise SystemExit(f"title is 1 to 23 characters (use --title): {title!r}")
    short = short or re.sub(r"[^A-Z0-9]", "", name.upper())[:4]
    if not SHORT_RE.fullmatch(short):
        raise SystemExit(
            f"short name is 3 to 6 characters of A-Z and 0-9 (use --short): {short!r}"
        )
    return {
        "ID": id_,
        "AUTHOR": author,
        "NAME": name,
        "UPPER": name.upper(),
        "TITLE": title,
        "SHORT": short,
        "TAB": short[:3],
    }


def render(text: str, values: dict[str, str]) -> str:
    return TOKEN_RE.sub(lambda m: values[m.group(1)], text)


def write_template(kind: str, base: Path, values: dict[str, str]) -> list[Path]:
    """Copy sdk/templates/<kind> to `base` with the tokens replaced; refuses an existing `base`. Does not build."""
    if base.exists():
        raise SystemExit(f"{base} exists")
    src = TEMPLATES / kind
    written = []
    for path in sorted(p for p in src.rglob("*") if p.is_file()):
        out = base / render(str(path.relative_to(src)), values)
        out.parent.mkdir(parents=True, exist_ok=True)
        out.write_text(render(path.read_text(), values))
        written.append(out)
    return written


def create(
    kind: str,
    id_: str,
    dir_: Path | None = None,
    title: str | None = None,
    short: str | None = None,
) -> Path:
    values = tokens(id_, title, short)
    base = (dir_ if dir_ is not None else MODULES / KIND_DIRS[kind]) / values["NAME"]
    write_template(kind, base, values)
    return base


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("kind", choices=sorted(KIND_DIRS))
    ap.add_argument("id", help="AUTHOR/NAME (for example alice/drone)")
    ap.add_argument(
        "--dir",
        type=Path,
        help="write DIR/NAME instead of modules/<kind folder>/NAME (list DIR in PATCHKIT_MODULES to use it)",
    )
    ap.add_argument("--title", help="display name (default: the name in Title Case)")
    ap.add_argument(
        "--short",
        help="short name, 3 to 6 characters of A-Z and 0-9 (default: from the name)",
    )
    ap.add_argument("--no-build", action="store_true")
    a = ap.parse_args(argv)
    base = create(a.kind, a.id, a.dir, a.title, a.short)
    shown = base.relative_to(ROOT) if base.is_relative_to(ROOT) else base
    print(f"created {shown}")
    import os

    from tools.patchkit import patchkit

    roots = [str(base.parent.resolve()), os.environ.get("PATCHKIT_MODULES", "")]
    os.environ["PATCHKIT_MODULES"] = os.pathsep.join(r for r in roots if r)
    r = patchkit.run(["gen", "--modules", a.id], capture=True)
    if r.returncode:
        print((r.stderr or r.stdout).strip(), file=sys.stderr)
        print(
            f"patchkit gen failed; {shown} is left in place: fix it, then run `patchkit gen --modules {a.id}`",
            file=sys.stderr,
        )
        return 1
    if not a.no_build:
        try:
            for line in patchkit.build_module(base, check=False):
                print(line)
        except patchkit.BuildError as e:
            print(e, file=sys.stderr)
            print(
                f"build failed; {shown} is left in place: fix it, then run `mise run module:build -- {base}`",
                file=sys.stderr,
            )
            return 1
    where = f"PATCHKIT_MODULES={base.parent} " if a.dir else ""
    print(f"next: {where}mise run module:check -- {a.id}")
    print(f"      {where}mise run module:pack -- {a.id} --device dt2")
    print(
        f"      {where}mise run dk:run -- {a.id} --emu --seconds 10    # needs `mise run workbench` running"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
