#!/usr/bin/env python3
"""Check the layering of the modules (docs/ARCHITECTURE.md): who may include, link and depend on what.

    uv run python -m tools.devtools.layercheck [--root DIR]        (mise run check:layers)

It reads manifests, sources and the built objects of the repository; it builds nothing and takes well under a second.
Every violation prints as `file:line: rule: what is wrong. Instead: what to do.` The exit status is 1 when there is one.

Rules:

  roles     Every module under modules/core has `role` in [module], and the role fits the folder: adapters/ -> adapter,
            apps/ -> app, runtime/ -> runtime, expansion/ -> installer.
  sources   Content modules (modules/machines, modules/fx, modules/tools, tests/fixtures/modules) and apps include only
            their own files and the SDK (sdk/include, sdk/asm). Apps may also include dkrt.h (runtime) and dk_ui_core.h
            (core/ui). Only runtime, installer and adapters include the firmware bindings (dk_fw_* headers, the dkfw crate).
            C, ColdFire and SHARC assembly (`#include`, `.include`) and Rust (`dkfw`, path dependencies) are read.
  objects   The undefined symbols of the built objects of apps and content modules are SDK imports, dkrt_*, dk_ui_*,
            dk_app_* or the SHARC externs the SDK documents. A symbol never names a firmware anchor, and `[object.externs]`
            never maps one to an anchor.
  apps      An app has no `[[op]]` and no `[[claim]]`: it never patches the firmware.
  self-contained
            No tracked file outside a user content module's own directory (modules/machines|fx|tools) names its id
            (`AUTHOR/NAME`) or its path (`machines/NAME`). Allowed: docs/**, any *.md, .agents/**, the fixtures
            (tests/fixtures/modules) and a module with `host = true` in [module] (the workbench speaks
            its protocol).
  imports   Every function of <dk/ui.h> is an `[[import]]` of sdk/sdk.toml with `requires = "core/ui"`, and every import
            with `requires` is in the runtime's `optional` list, so the runtime can link it by name (or hand out its stub).

Exceptions are named below with their reason; a new one needs a reason in this file.
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
import tomllib
from dataclasses import dataclass
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]

CORE = "modules/core"
# the include search path of the module builds: the tool's own copy, whatever tree is checked
PINS = Path(__file__).resolve().parents[2] / "tools/patchkit/toolchain/pins.toml"
# Content module roots (relative to the repository root).
CONTENT_ROOTS = (
    "modules/machines",
    "modules/fx",
    "modules/tools",
    "tests/fixtures/modules",
)
# The role that fits a folder under modules/core.
FOLDER_ROLES = {
    "adapters": "adapter",
    "apps": "app",
    "runtime": "runtime",
    "expansion": "installer",
}
# Content modules that patch the firmware themselves (they have [[op]] and anchor externs by design: patchkit's own
# example of a hook). They are exempt from the objects and apps rules.
FIRMWARE_PATCH_MODULES = {"test/registry-demo"}
# Test fixtures that stand in for an app (they use core/ui like core/digikit-menu does): app include rights.
APP_LIKE = {"test/ui-demo"}
# Headers of the core layers an app may include.
APP_HEADERS = {"dkrt.h", "dk_ui_core.h"}
ANCHOR = re.compile(r"^(cf|dsp|hook|pk)\.")
INC_C = re.compile(r'^\s*#\s*include\s*([<"])([^>"]+)[>"]')
INC_ASM = re.compile(r'^\s*\.include\s+"([^"]+)"')
RS_FW = re.compile(r"\bdkfw\s*::|\bextern\s+crate\s+dkfw\b")
EXTERN_DOC = re.compile(r"\.EXTERN\s+([A-Za-z_][A-Za-z0-9_]*)")
SOURCE_SUFFIXES = {".c", ".h", ".s", ".S", ".inc", ".rs"}


@dataclass(frozen=True)
class Violation:
    where: str  # file:line
    rule: str
    message: str
    instead: str

    def __str__(self) -> str:
        return f"{self.where}: {self.rule}: {self.message}. Instead: {self.instead}."


@dataclass
class Module:
    dir: Path
    manifest: dict
    text: str

    @property
    def mod(self) -> dict:
        return self.manifest["module"]

    @property
    def id(self) -> str:
        return self.mod["id"]


def rel(root: Path, p: Path) -> str:
    try:
        return p.relative_to(root).as_posix()
    except ValueError:
        return p.as_posix()


def line_of(text: str, needle: str, default: int = 1) -> int:
    for n, line in enumerate(text.splitlines(), 1):
        if needle in line:
            return n
    return default


def load_modules(root: Path) -> list[Module]:
    out = []
    for base in (root / "modules", root / "tests/fixtures/modules"):
        for toml in sorted(base.rglob("module.toml")):
            text = toml.read_text(encoding="utf-8")
            try:
                manifest = tomllib.loads(text)
            except tomllib.TOMLDecodeError:
                continue  # patchkit reports a broken manifest
            if "module" in manifest and "id" in manifest["module"]:
                out.append(Module(toml.parent, manifest, text))
    return out


def classify(root: Path, m: Module) -> str:
    """core:<role dir> for modules under modules/core, `content` for the content roots."""
    r = rel(root, m.dir)
    if r.startswith(CORE + "/"):
        return "core"
    return "content"


def check_roles(root: Path, mods: list[Module]) -> list[Violation]:
    out = []
    for m in mods:
        if classify(root, m) != "core":
            continue
        r = rel(root, m.dir)
        parts = r.split("/")[2:]  # below modules/core
        want = FOLDER_ROLES.get(parts[0]) if parts else None
        where = f"{r}/module.toml:{line_of(m.text, 'category', 1)}"
        have = m.mod.get("role")
        if want is None:
            out.append(
                Violation(
                    where,
                    "roles",
                    f"{m.id} sits in {r}, which is no role folder",
                    "move it to modules/core/adapters/NAME, modules/core/apps/NAME, modules/core/runtime or modules/core/expansion",
                )
            )
        elif have is None:
            out.append(
                Violation(
                    where,
                    "roles",
                    f"{m.id} has no `role` in [module]",
                    f'add role = "{want}" (the role of the folder {parts[0]})',
                )
            )
        elif have != want:
            out.append(
                Violation(
                    f"{r}/module.toml:{line_of(m.text, 'role', 1)}",
                    "roles",
                    f'{m.id} has role = "{have}" but sits in {parts[0]}/, whose role is "{want}"',
                    f'set role = "{want}" or move the module to the folder of role "{have}"',
                )
            )
    return out


class Resolver:
    """Finds the file an include names, with the search path of the module builds (the lists in
    tools/patchkit/toolchain/pins.toml, which `patchkit build` also reads)."""

    def __init__(self, root: Path):
        self.root = root
        pins = tomllib.loads(PINS.read_text(encoding="utf-8"))["include"]
        self.global_dirs = [
            root / d for d in dict.fromkeys([*pins["cf"], *pins["sharc"]])
        ]

    def find(self, name: str, file: Path, mod: Module, quoted: bool) -> Path | None:
        dirs = []
        if quoted:
            dirs += [file.parent]
        dirs += [mod.dir / "src", mod.dir / "build/gen", mod.dir, *self.global_dirs]
        for d in dirs:
            p = d / name
            if p.is_file():
                return p.resolve()
        return None


def sdk_symbols(root: Path) -> tuple[set[str], set[str]]:
    """(the SDK's import names, the SHARC externs that sdk/asm and sdk/templates document)."""
    imports: set[str] = set()
    sdk = root / "sdk/sdk.toml"
    if sdk.is_file():
        data = tomllib.loads(sdk.read_text(encoding="utf-8"))
        imports = {i["name"] for i in data.get("import", [])}
    externs: set[str] = set()
    for d in ("sdk/asm", "sdk/templates"):
        for p in sorted((root / d).rglob("*")) if (root / d).is_dir() else []:
            if p.is_file() and p.suffix in {".s", ".inc", ".asm"}:
                externs |= set(
                    EXTERN_DOC.findall(p.read_text(encoding="utf-8", errors="replace"))
                )
    return imports, externs


def module_sources(mod: Module) -> list[Path]:
    out = []
    for sub in ("src", "include"):
        d = mod.dir / sub
        if d.is_dir():
            out += [
                p
                for p in sorted(d.rglob("*"))
                if p.is_file() and p.suffix in SOURCE_SUFFIXES
            ]
    return out


def check_sources(root: Path, mods: list[Module]) -> list[Violation]:
    out = []
    res = Resolver(root)
    fw_dirs = [(root / CORE / "fw").resolve()]
    runtime_inc = (root / CORE / "runtime/include").resolve()
    adapters = (root / CORE / "adapters").resolve()
    all_mod_dirs = [(m.dir.resolve(), m) for m in mods]
    for m in mods:
        kind = classify(root, m)
        role = m.mod.get("role")
        is_app = (kind == "core" and role == "app") or m.id in APP_LIKE
        if kind == "core" and not is_app:
            continue  # runtime, installer and adapters may use the firmware bindings
        who = "an app" if is_app else "a content module"
        own = m.dir.resolve()
        sdk = (root / "sdk").resolve()
        for f in module_sources(m):
            text = f.read_text(encoding="utf-8", errors="replace")
            for n, line in enumerate(text.splitlines(), 1):
                where = f"{rel(root, f)}:{n}"
                if f.suffix == ".rs":
                    if RS_FW.search(line):
                        out.append(
                            Violation(
                                where,
                                "sources",
                                f"{who} ({m.id}) uses the firmware crate dkfw",
                                "call the adapter's API (<dk/...> or an export of core/runtime); the firmware bindings belong to adapters",
                            )
                        )
                    continue
                inc = INC_C.match(line)
                name, quoted = None, False
                if inc:
                    name, quoted = inc.group(2), inc.group(1) == '"'
                else:
                    asm = INC_ASM.match(line)
                    if asm:
                        name, quoted = asm.group(1), True
                if name is None:
                    continue
                target = res.find(name, f, m, quoted)
                if target is None:
                    continue  # a system header, or a file patchkit reports
                if own in target.parents or target.parent == own:
                    continue
                if sdk in target.parents:
                    continue
                base = Path(name).name
                if any(d in target.parents for d in fw_dirs):
                    out.append(
                        Violation(
                            where,
                            "sources",
                            f"{who} ({m.id}) includes the firmware binding {name}",
                            "use the SDK (<dk/...>) or move the firmware access into an adapter and call it by an exported function",
                        )
                    )
                elif (runtime_inc in target.parents or adapters in target.parents) and (
                    is_app and base in APP_HEADERS
                ):
                    continue
                elif runtime_inc in target.parents or adapters in target.parents:
                    out.append(
                        Violation(
                            where,
                            "sources",
                            f"{who} ({m.id}) includes {name}, a header of a core layer",
                            "include only <dk/...>"
                            + (
                                " (an app may add dkrt.h and dk_ui_core.h)"
                                if is_app
                                else ""
                            ),
                        )
                    )
                else:
                    foreign = next(
                        (o for d, o in all_mod_dirs if d in target.parents), None
                    )
                    out.append(
                        Violation(
                            where,
                            "sources",
                            f"{who} ({m.id}) includes {name}"
                            + (
                                f" of module {foreign.id}"
                                if foreign
                                else " outside its own files and the SDK"
                            ),
                            "include only <dk/...> and the module's own files",
                        )
                    )
        # Rust dependencies
        for cargo in sorted(m.dir.rglob("Cargo.toml")):
            if "target" in cargo.relative_to(m.dir).parts:
                continue
            ctext = cargo.read_text(encoding="utf-8")
            try:
                data = tomllib.loads(ctext)
            except tomllib.TOMLDecodeError:
                continue
            for table in ("dependencies", "dev-dependencies", "build-dependencies"):
                for dep, spec in data.get(table, {}).items():
                    path = spec.get("path") if isinstance(spec, dict) else None
                    target = (cargo.parent / path).resolve() if path else None
                    bad = dep == "dkfw" or (
                        target is not None
                        and target != own
                        and own not in target.parents
                        and (root / CORE).resolve() in target.parents
                        and target != (root / CORE / "runtime/sdk").resolve()
                    )
                    if bad:
                        out.append(
                            Violation(
                                f"{rel(root, cargo)}:{line_of(ctext, dep)}",
                                "sources",
                                f"{who} ({m.id}) depends on the core crate {dep}",
                                "depend only on dkrt-sdk (the Rust view of the SDK); firmware bindings belong to adapters",
                            )
                        )
    return out


def object_path(root: Path, m: Module, rel_path: str) -> Path:
    """Where the built object `rel_path` of the module is: in the module for `core/*`, else in the cache (packages/patchkit/src/build/mod.rs)."""
    if classify(root, m) == "core":
        return m.dir / rel_path
    return root / "out/cache/dko" / m.id / rel_path


def check_committed_objects(root: Path, mods: list[Module]) -> list[Violation]:
    """Objects of content modules and fixtures are build output (the cache); only `core/*` objects are committed."""
    out = []
    for m in mods:
        if classify(root, m) == "core":
            continue
        for f in sorted(m.dir.rglob("*.dko.json")):
            out.append(
                Violation(
                    f"{rel(root, f)}:1",
                    "objects",
                    f"{m.id} is a content module and has an object file in its tree",
                    "delete it; `patchkit build` and `patchkit pack` keep the objects in out/cache/dko",
                )
            )
    return out


def check_objects(root: Path, mods: list[Module]) -> list[Violation]:
    out = []
    imports, sdk_externs = sdk_symbols(root)
    for m in mods:
        kind = classify(root, m)
        is_app = (kind == "core" and m.mod.get("role") == "app") or m.id in APP_LIKE
        if (kind == "core" and not is_app) or m.id in FIRMWARE_PATCH_MODULES:
            continue
        mrel = rel(root, m.dir)
        for obj in m.manifest.get("object", []):
            for sym, anchor in (obj.get("externs") or {}).items():
                out.append(
                    Violation(
                        f"{mrel}/module.toml:{line_of(m.text, sym)}",
                        "objects",
                        f"{m.id} maps its symbol {sym} to the firmware anchor {anchor}",
                        "call an SDK import or an adapter export; anchors are resolved only by adapters",
                    )
                )
            path = object_path(root, m, obj.get("path", ""))
            if not path.is_file():
                continue  # not built: nothing to read
            try:
                data = json.loads(path.read_text(encoding="utf-8"))
            except json.JSONDecodeError:
                continue
            for s in data.get("symbols", []):
                name = s["name"]
                if s.get("section") is not None:
                    continue
                ok = (
                    name in imports
                    or name in sdk_externs
                    or name.startswith(("dkrt_", "dk_ui_", "dk_app_", "dkm_"))
                )
                if ANCHOR.match(name) or not ok:
                    out.append(
                        Violation(
                            f"{rel(root, path)}:1",
                            "objects",
                            f"the object of {m.id} needs the symbol {name}, which is no SDK import, dkrt_*, dk_ui_* or dk_app_*",
                            "use the SDK (sdk/sdk.toml [[import]]) or an adapter export; a firmware anchor belongs to an adapter",
                        )
                    )
    return out


def check_apps(root: Path, mods: list[Module]) -> list[Violation]:
    out = []
    for m in mods:
        kind = classify(root, m)
        if kind == "core" and m.mod.get("role") == "app":
            for table in ("op", "claim"):
                if m.manifest.get(table):
                    out.append(
                        Violation(
                            f"{rel(root, m.dir)}/module.toml:{line_of(m.text, f'[[{table}]]')}",
                            "apps",
                            f"the app {m.id} has [[{table}]]: it patches the firmware itself",
                            "let an adapter own the hook and call the app through an optional symbol (dk_app_*)",
                        )
                    )
    return out


UI_FN = re.compile(r"^(?!typedef)[A-Za-z_][A-Za-z0-9_ \*]*?\b(dk_ui_[a-z_]+)\(", re.M)


def check_imports(root: Path) -> list[Violation]:
    """The imports of other modules: <dk/ui.h> against sdk/sdk.toml against the runtime's `optional` list."""
    sdk, ui = root / "sdk/sdk.toml", root / "sdk/include/dk/ui.h"
    if not sdk.is_file():
        return []
    sdk_text = sdk.read_text(encoding="utf-8")
    data = tomllib.loads(sdk_text)
    out = []
    ui_imports = {
        i["name"] for i in data.get("import", []) if i.get("requires") == "core/ui"
    }
    if ui.is_file():
        ui_text = ui.read_text(encoding="utf-8")
        declared = set(UI_FN.findall(ui_text))
        for n in sorted(declared - ui_imports):
            out.append(
                Violation(
                    f"sdk/include/dk/ui.h:{line_of(ui_text, n + '(')}",
                    "imports",
                    f'{n} is declared in <dk/ui.h> but is no [[import]] of sdk/sdk.toml with requires = "core/ui"',
                    "add the [[import]] (with sig and absent), so a content module can call it",
                )
            )
        for n in sorted(ui_imports - declared):
            name_line = 'name = "' + n + '"'
            out.append(
                Violation(
                    f"sdk/sdk.toml:{line_of(sdk_text, name_line)}",
                    "imports",
                    f"{n} is an import of core/ui but <dk/ui.h> does not declare it",
                    "declare it in <dk/ui.h> or remove the [[import]]",
                )
            )
    runtime = root / CORE / "runtime/module.toml"
    if runtime.is_file():
        runtime_text = runtime.read_text(encoding="utf-8")
        optional: set[str] = set()
        for o in tomllib.loads(runtime_text).get("object", []):
            optional |= set(o.get("optional", []))
        needed = {i["name"] for i in data.get("import", []) if i.get("requires")}
        for n in sorted(needed - optional):
            out.append(
                Violation(
                    f"{CORE}/runtime/module.toml:{line_of(runtime_text, 'optional')}",
                    "imports",
                    f"{n} is an import of another module but is not in the runtime's optional list",
                    "add it to `optional`, or patchkit cannot link the runtime when that module is missing",
                )
            )
    return out


USER_MODULE_ROOTS = ("modules/machines/", "modules/fx/", "modules/tools/")
# Files that may name any module: documentation, agent notes and the fixtures.
SC_ALLOWED_PREFIXES = (
    "docs/",
    ".agents/",
    "tests/fixtures/modules/",
)
SC_SKIP_DIRS = {".git", "out", "node_modules", "target", "firmware", ".venv", "dist"}
SC_HINT = (
    "move the test or model into the module's tests/ directory, use a fixture module (tests/fixtures/modules), "
    "or write a placeholder (<author/name>) in help text"
)


def tracked_files(root: Path) -> list[str]:
    """Tracked files (`git ls-files`); outside a git repository, every file except build output."""
    try:
        done = subprocess.run(
            ["git", "-C", str(root), "ls-files", "-z"],
            capture_output=True,
            check=True,
        )
        names = [n for n in done.stdout.decode("utf-8", "replace").split("\0") if n]
        if names:
            return [n for n in names if (root / n).is_file()]
    except (subprocess.CalledProcessError, FileNotFoundError):
        pass
    out = []
    for p in sorted(root.rglob("*")):
        r = p.relative_to(root)
        if p.is_file() and not SC_SKIP_DIRS & set(r.parts):
            out.append(r.as_posix())
    return out


def user_modules(root: Path, mods: list[Module]) -> list[tuple[Module, str]]:
    """(module, relative directory) of every user content module that is not a `host` module."""
    out = []
    for m in mods:
        r = rel(root, m.dir)
        if r.startswith(USER_MODULE_ROOTS) and not m.mod.get("host", False):
            out.append((m, r))
    return out


def sc_hits(root: Path, mods: list[Module]) -> list[tuple[str, int, Module, str]]:
    """(file, line, module, module dir) for every mention of a user module outside its directory."""
    users = user_modules(root, mods)
    pats = []
    for m, d in users:
        names = {m.id, d.removeprefix("modules/")}
        pats.append(
            (
                m,
                d,
                re.compile(
                    r"(?<![\w.-])(?:"
                    + "|".join(re.escape(n) for n in sorted(names))
                    + r")(?![\w-])"
                ),
            )
        )
    out = []
    for f in tracked_files(root):
        if f.endswith(".md") or f.startswith(SC_ALLOWED_PREFIXES):
            continue
        data = (root / f).read_bytes()
        if b"\0" in data[:4096]:
            continue
        text = data.decode("utf-8", "replace")
        for m, d, pat in pats:
            if f.startswith(d + "/"):
                continue
            for n, line in enumerate(text.splitlines(), 1):
                if pat.search(line):
                    out.append((f, n, m, d))
    return out


def check_self_contained(root: Path, mods: list[Module]) -> list[Violation]:
    return [
        Violation(f"{f}:{n}", "self-contained", f"{m.id} named outside {d}", SC_HINT)
        for f, n, m, d in sc_hits(root, mods)
    ]


def check(root: Path = ROOT) -> list[Violation]:
    mods = load_modules(root)
    return [
        *check_roles(root, mods),
        *check_sources(root, mods),
        *check_committed_objects(root, mods),
        *check_objects(root, mods),
        *check_apps(root, mods),
        *check_imports(root),
        *check_self_contained(root, mods),
    ]


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument(
        "--root",
        type=Path,
        default=ROOT,
        help="repository root (default: this repository)",
    )
    args = ap.parse_args(argv)
    found = check(args.root.resolve())
    for v in found:
        print(v)
    if found:
        print(
            f"layercheck: {len(found)} violation(s); the layers are in docs/ARCHITECTURE.md",
            file=sys.stderr,
        )
        return 1
    print("layercheck: the layering holds")
    return 0


if __name__ == "__main__":
    sys.exit(main())
