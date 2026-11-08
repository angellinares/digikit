"""Module roots and module ids: the one place that maps a module id to its directory and a path to its module.

Roots: `modules/`, `tests/fixtures/modules/`, then the os.pathsep-separated `PATCHKIT_MODULES`. A module's id is the
`id` in its `module.toml`; it need not equal the directory name. Two modules with one id are an error.
"""

from __future__ import annotations

import json
import os
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]

# The built objects of content modules (every module but category "core"): packages/patchkit/src/build/mod.rs `cache_dir`.
# An entry is `out/cache/dko/<module id>/<src12>/<object path>`: ask `object_path`, which asks patchkit for the key of the sources now.

# module roots inside a repository, relative to its root
REPO_ROOTS = ("modules", "tests/fixtures/modules")

_cache: dict[tuple[str, ...], dict[str, Path]] = {}


def roots() -> list[Path]:
    out = [ROOT / r for r in REPO_ROOTS]
    out += [
        Path(p) for p in os.environ.get("PATCHKIT_MODULES", "").split(os.pathsep) if p
    ]
    return out


def manifests() -> dict[str, Path]:
    """Module id -> module directory, over all roots (cached per set of roots)."""
    rs = roots()
    key = tuple(str(r) for r in rs)
    if key not in _cache:
        found: dict[str, Path] = {}
        for r in rs:
            for toml in sorted(r.rglob("module.toml")):
                mid = tomllib.loads(toml.read_text())["module"]["id"]
                if mid in found:
                    raise SystemExit(
                        f"duplicate module id {mid}: {found[mid]} and {toml.parent}"
                    )
                found[mid] = toml.parent
        _cache[key] = found
    return _cache[key]


def module_dir(mid: str) -> Path:
    try:
        return manifests()[mid]
    except KeyError:
        raise SystemExit(f"unknown module {mid!r}") from None


def module_id(path: Path | str) -> str | None:
    """The id of the module whose directory contains `path` (None outside every module)."""
    p = Path(path)
    if not p.is_absolute():
        p = ROOT / p
    p = p.resolve()
    for mid, d in manifests().items():
        if p == d.resolve() or d.resolve() in p.parents:
            return mid
    return None


def object_path(manifest: dict, moddir: Path, obj: dict) -> Path:
    """Where the built object `obj` (an `[[object]]` table of the parsed `module.toml` MANIFEST in MODDIR) is. A core module keeps its
    objects in its tree (committed); any other module's objects are build output in `out/cache/dko/<module id>/<src12>/<path>`
    (`patchkit build` and `patchkit pack` write them there). An object without `sources` is a file of the module."""
    mod = manifest["module"]
    if mod["category"] != "core" and obj.get("sources"):
        return cached_objects(moddir)[obj["path"]]
    return moddir / obj["path"]


_cached: dict[Path, dict[str, Path]] = {}  # module dir -> object path -> file


def cached_objects(moddir: Path) -> dict[str, Path]:
    """`patchkit build MODDIR --paths`: the stored file of each object with sources for the sources now (nothing is built). Kept per
    process; `patchkit.build_module` and `_build_cmd` forget them, as a build can change the sources' key."""
    from tools.patchkit import patchkit

    key = moddir.resolve()
    if key not in _cached:
        r = patchkit._build_cmd(["build", str(moddir), "--paths"])
        if r.returncode:
            raise patchkit.BuildError(
                (r.stderr or r.stdout).strip().removeprefix("patchkit: ")
            )
        (files,) = json.loads(r.stdout).values()
        _cached[key] = {k: Path(v) for k, v in files.items()}
    return _cached[key]


def forget() -> None:
    """Drop the remembered object paths (after the sources of a module changed)."""
    _cached.clear()
