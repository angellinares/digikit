#!/usr/bin/env python3
"""Export and import a module as a `.dkmod` package (`mise run module:export`, `mise run module:import`).

    uv run python -m tools.patchkit.pk_package export ID [-o FILE]
    uv run python -m tools.patchkit.pk_package import FILE [--dir DIR] [--replace]

A `.dkmod` is a deterministic zip of a module folder without `build/`, plus a top-level `package.toml`::

    id = "em/demo"
    version = "0.1.0"
    author = "em"
    type = "machine"

    [files]
    "module.toml" = "<sha256>"
    "src/dsp/demo.s" = "<sha256>"

Deterministic: entries sorted by path (`package.toml` first), modification time 1980-01-01, fixed permissions, no OS junk files
(`.DS_Store`, `__pycache__`, ...). Two exports of one tree are byte-identical.

A `.dkmod` carries no SDK code and no SDK version: the module builds against the SDK of the repository that imports it (`patchkit build`
links the `sdk/src` components the module calls). Rebuild it after an SDK update.

`import` checks the sha256 of every file against `package.toml` (and refuses extra or missing files), refuses the author `core` and
the type `framework`, refuses an id that the module roots already know unless `--replace`, and unpacks into `DIR/<name>`. Without
`--dir` it uses the first entry of PATCHKIT_MODULES; with neither it fails. It never writes into the repository's modules/ unless
`--dir` names it.
"""

from __future__ import annotations

import argparse
import hashlib
import io
import os
import shutil
import sys
import tomllib
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]

from tools.patchkit import modroots  # noqa: E402

PACKAGE = "package.toml"
FIXED_TIME = (1980, 1, 1, 0, 0, 0)
JUNK_NAMES = {".DS_Store", "Thumbs.db", "desktop.ini"}
JUNK_DIRS = {"__pycache__", ".git", ".venv", "node_modules", ".pytest_cache"}


class PackageError(SystemExit):
    pass


def _junk(rel: Path) -> bool:
    return (
        rel.parts[0] == "build"
        or rel.name in JUNK_NAMES
        or rel.name.startswith("._")
        or rel.suffix == ".pyc"
        or any(p in JUNK_DIRS for p in rel.parts)
    )


def module_files(moddir: Path) -> dict[str, bytes]:
    """The files of a module folder that a package holds: path (posix) -> bytes, sorted."""
    out: dict[str, bytes] = {}
    for p in sorted(moddir.rglob("*")):
        rel = p.relative_to(moddir)
        if p.is_file() and not p.is_symlink() and not _junk(rel):
            if rel.as_posix() == PACKAGE:
                continue
            out[rel.as_posix()] = p.read_bytes()
    return dict(sorted(out.items()))


def package_toml(manifest: dict, files: dict[str, bytes]) -> str:
    m = manifest["module"]
    lines = [
        f'id = "{m["id"]}"',
        f'version = "{m["version"]}"',
        f'author = "{m.get("author", "")}"',
        f'type = "{m.get("type", "")}"',
        "",
        "[files]",
    ]
    for rel, data in files.items():
        if '"' in rel or "\\" in rel:
            raise PackageError(f"{rel}: a quote or a backslash in a file name")
        lines.append(f'"{rel}" = "{hashlib.sha256(data).hexdigest()}"')
    return "\n".join(lines) + "\n"


def build_zip(manifest: dict, files: dict[str, bytes]) -> bytes:
    buf = io.BytesIO()
    entries = {PACKAGE: package_toml(manifest, files).encode(), **files}
    with zipfile.ZipFile(buf, "w", zipfile.ZIP_DEFLATED, compresslevel=9) as z:
        for rel, data in entries.items():
            zi = zipfile.ZipInfo(rel, FIXED_TIME)
            zi.compress_type = zipfile.ZIP_DEFLATED
            zi.create_system = 3
            zi.external_attr = 0o100644 << 16
            z.writestr(zi, data, compresslevel=9)
    return buf.getvalue()


def read_package(path: Path) -> tuple[dict, dict[str, bytes]]:
    """(package.toml as a dict, the other entries) of a .dkmod; refuses an unsafe entry name."""
    with zipfile.ZipFile(path) as z:
        entries: dict[str, bytes] = {}
        for zi in z.infolist():
            name = zi.filename
            parts = Path(name).parts
            if (
                name.endswith("/")
                or name.startswith("/")
                or ".." in parts
                or "\\" in name
            ):
                raise PackageError(f"{path}: unsafe entry {name!r}")
            entries[name] = z.read(zi)
    if PACKAGE not in entries:
        raise PackageError(f"{path}: no {PACKAGE}")
    pkg = tomllib.loads(entries.pop(PACKAGE).decode())
    return pkg, entries


def export(mid: str, out: Path | None = None) -> Path:
    moddir = modroots.module_dir(mid)
    manifest = tomllib.loads((moddir / "module.toml").read_text(encoding="utf-8"))
    files = module_files(moddir)
    data = build_zip(manifest, files)
    ver = manifest["module"]["version"]
    out = out or ROOT / "out" / "patch" / f"{mid.replace('/', '-')}-{ver}.dkmod"
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_bytes(data)
    return out


def import_package(file: Path, dir_: Path | None = None, replace: bool = False) -> Path:
    pkg, files = read_package(file)
    listed = pkg.get("files", {})
    if set(listed) != set(files):
        extra, missing = (
            sorted(set(files) - set(listed)),
            sorted(set(listed) - set(files)),
        )
        raise PackageError(
            f"{file}: files and package.toml disagree (not listed: {extra}; missing: {missing})"
        )
    for rel, want in listed.items():
        got = hashlib.sha256(files[rel]).hexdigest()
        if got != want:
            raise PackageError(f"{file}: {rel}: sha256 {got} is not the listed {want}")
    if "module.toml" not in files:
        raise PackageError(f"{file}: no module.toml")
    mod = tomllib.loads(files["module.toml"].decode())["module"]
    mid = pkg.get("id", "")
    if mod.get("id") != mid:
        raise PackageError(
            f"{file}: package id {mid!r} is not the module's id {mod.get('id')!r}"
        )
    for who, d in (("package.toml", pkg), ("module.toml", mod)):
        if d.get("author") == "core" or mid.split("/")[0] == "core":
            raise PackageError(
                f"{file}: {who}: the author core is reserved for the repository's own modules"
            )
        if d.get("type") == "framework":
            raise PackageError(
                f"{file}: {who}: the type framework is internal and cannot be imported"
            )
    if dir_ is None:
        extra_roots = [
            p for p in os.environ.get("PATCHKIT_MODULES", "").split(os.pathsep) if p
        ]
        if not extra_roots:
            raise PackageError(
                "no target: pass --dir DIR or set PATCHKIT_MODULES to a folder (the repository's modules/ is never written by default)"
            )
        dir_ = Path(extra_roots[0])
    target = dir_ / mid.split("/", 1)[1]
    known = modroots.manifests().get(mid)
    if known is not None:
        if not replace:
            raise PackageError(
                f"{mid} is already known ({known}); use --replace to overwrite it"
            )
        if known.resolve() != target.resolve():
            raise PackageError(
                f"{mid} lives in {known}, not in {target}: pass --dir {known.parent} to replace it"
            )
        shutil.rmtree(target)
    elif target.exists():
        raise PackageError(f"{target} exists and is not module {mid}")
    for rel, data in files.items():
        p = target / rel
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_bytes(data)
    return target


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=(__doc__ or "").splitlines()[0])
    sub = ap.add_subparsers(dest="cmd", required=True)
    e = sub.add_parser("export", help="write a .dkmod of a module")
    e.add_argument("id")
    e.add_argument(
        "-o", "--out", type=Path, help="default out/patch/<id>-<version>.dkmod"
    )
    i = sub.add_parser("import", help="unpack a .dkmod")
    i.add_argument("file", type=Path)
    i.add_argument(
        "--dir", type=Path, help="default: the first entry of PATCHKIT_MODULES"
    )
    i.add_argument(
        "--replace", action="store_true", help="overwrite the known module of this id"
    )
    a = ap.parse_args(argv)
    if a.cmd == "export":
        print(f"exported {export(a.id, a.out)}")
    else:
        print(f"imported {import_package(a.file, a.dir, a.replace)}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
