#!/usr/bin/env python3
"""Front end for the patchkit command line (packages/patchkit): builds it when stale and runs it from the repo root.

    uv run python -m tools.patchkit.patchkit bin                       print the path of the built binary
    uv run python -m tools.patchkit.patchkit patch --firmware X.syx --modules a,b [-o OUT.syx]   apply; writes out/patch/
    uv run python -m tools.patchkit.patchkit patch --firmware X.syx [--set NAME]   no --modules: the device's patch set (default `device`)
    uv run python -m tools.patchkit.patchkit explain --firmware X.syx --modules a,b              site-by-site account
    uv run python -m tools.patchkit.patchkit list|inspect|check|verify|preflight|gen|desc ...                  passed through
    uv run python -m tools.patchkit.patchkit build [MODULE...] [--check] [--force] [--define NAME --out DIR] [--status]   module sources -> build/*.dko.json
    uv run python -m tools.patchkit.patchkit dko show FILE.dko.json [--relocs]   an object's arch, toolchain, sections, symbols, relocations
    uv run python -m tools.patchkit.patchkit needs [--json] FILE.dkm...   what a load of each file takes (arena, DSP area, rows), inferred from its contents
    uv run python -m tools.patchkit.patchkit pack MODULE --device dt2|dn2 [--trace] [-o OUT.dkm]  pack a run-time module (default out/patch/<name>-<device>.dkm; --trace builds it with DK_TRACE)
    uv run python -m tools.patchkit.patchkit bundle [-o FILE] [--modules-dir DIR]  ports + modules as one JSON (what patchkit.wasm reads)
    uv run python -m tools.patchkit.patchkit inject [OUT.syx] [--hot]  load a patched image into the running emulator
    uv run python -m tools.patchkit.patchkit inject STOCK.syx --runtime [--patch-set NAME]  the emulator patches STOCK.syx itself

`DIGI_PK_ROOT=DIR` runs the binary with another repository root (a module variant, tools/patchkit/pk_variant.py).

`patch` defaults the output to out/patch/<firmware stem>+<modules>.syx and writes plan.json next to it. `explain`
adds a SHARC+ listing of the DSP sites to the report of the Rust binary. Output files are firmware-derived: they stay
under out/ and are never committed.
"""

from __future__ import annotations

import json
import os
import re
import subprocess
import sys
import tomllib
from dataclasses import dataclass
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent

from tools.emulator import native_target  # noqa: E402
from tools.patchkit import modroots  # noqa: E402


def target_dir() -> Path:
    # patchkit has no generated SHARC core: the "no core" key keeps its build away from sharc-gen's
    return Path(native_target.target_dir(None)).parent / "nogen"


_BUILT: Path | None = None


def binary(build: bool = True) -> Path:
    """The patchkit binary to run: an immutable copy (tools/devtools/stablebin.py), never cargo's own
    `release/patchkit`, which every `cargo build` rewrites (a process that execs it meanwhile can hang)."""
    global _BUILT
    from tools.devtools import stablebin

    target = target_dir()
    exe = target / "release" / "patchkit"
    if build and _BUILT is not None:
        return _BUILT
    if not build and not exe.is_file():
        return exe
    with stablebin.build_lock(target):
        if build:
            r = subprocess.run(
                [
                    "cargo",
                    "build",
                    "-p",
                    "patchkit",
                    "--release",
                    "--offline",
                    "--target-dir",
                    str(target),
                ],
                cwd=ROOT,
                capture_output=True,
                text=True,
            )
            if r.returncode:
                raise SystemExit(f"cannot build patchkit:\n{r.stderr[-3000:]}")
        stable = stablebin.stable_copy(exe, target)
    if build:
        _BUILT = stable
    return stable


def run(args: list[str], capture: bool = False) -> subprocess.CompletedProcess:
    return subprocess.run(
        [str(binary()), "--root", os.environ.get("DIGI_PK_ROOT") or str(ROOT), *args],
        cwd=ROOT,
        capture_output=capture,
        text=True,
    )


class BuildError(RuntimeError):
    """`patchkit build` failed (the message is the binary's)."""


def _build_cmd(args: list[str]) -> subprocess.CompletedProcess:
    # always the real repository: the toolchains, caches and shared include directories are in it (DIGI_PK_ROOT names a module variant)
    modroots.forget()  # object paths carry the key of the sources, which a build or a source edit changes
    return subprocess.run(
        [str(binary()), "--root", str(ROOT), *args],
        cwd=ROOT,
        capture_output=True,
        text=True,
    )


def build_module(
    moddir: Path,
    check: bool = False,
    defines: frozenset[str] | set[str] = frozenset(),
    out_root: Path | None = None,
    force: bool = False,
) -> list[str]:
    """`patchkit build MODDIR`: every object of the module that lists sources; returns one message per object. With `defines` (build
    options of the DSP sources) the objects go under `out_root` at their `path` and the committed ones stay. Raises BuildError."""
    args = ["build", str(moddir)]
    args += ["--check"] if check else []
    args += ["--force"] if force else []
    for d in sorted(defines):
        args += ["--define", d]
    args += ["--out", str(out_root)] if out_root else []
    r = _build_cmd(args)
    if r.returncode:
        raise BuildError((r.stderr or r.stdout).strip().removeprefix("patchkit: "))
    return r.stdout.splitlines()


def module_statuses(moddirs: list[Path]) -> dict[Path, str]:
    """`patchkit build --status`: "unchanged" when every object of the module that has sources carries the hash of its sources now,
    else "needs build". Builds nothing."""
    r = _build_cmd(["build", *map(str, moddirs), "--status"])
    if r.returncode:
        raise BuildError((r.stderr or r.stdout).strip().removeprefix("patchkit: "))
    by_name = json.loads(r.stdout)
    return {d: by_name[str(d)] for d in moddirs}


def module_status(moddir: Path) -> str:
    return module_statuses([moddir])[moddir]


def isa_digest(obj: dict) -> str:
    """The ISA check record (`toolchain.isa_check`) of an object with these sections, symbols and relocations: `patchkit pack` takes a
    ColdFire object only when the record matches its bytes. For tests that edit a built object after the build."""
    import tempfile

    with tempfile.TemporaryDirectory(prefix="pk-isa-") as d:
        f = Path(d) / "o.dko.json"
        f.write_text(json.dumps(obj))
        r = _build_cmd(["dko", "isa-digest", str(f)])
    if r.returncode:
        raise BuildError((r.stderr or r.stdout).strip().removeprefix("patchkit: "))
    return r.stdout.strip()


ISA_CHECK_KEY = "isa_check"


@dataclass
class CfLinked:
    """C and assembly sources linked at `base` (`patchkit cf-link`)."""

    base: int
    image: bytes  # text, rodata and data at base; bss (zeros) follows
    bss_size: int
    symbols: dict[str, int]
    text_ranges: list[tuple[int, int]]
    relocs_applied: dict[str, int]
    histogram: dict[str, int]


def cf_link(
    sources: list[Path],
    base: int,
    symbols: dict[str, int],
    include_dirs: list[Path] | None = None,
) -> CfLinked:
    """Compile, link at `base` against `symbols` (every extern the sources leave undefined) and ISA-check ColdFire sources: for unit
    tests of C routines against firmware addresses. Raises BuildError."""
    import tempfile

    with tempfile.TemporaryDirectory(prefix="pk-cflink-") as d:
        binp, jsonp = Path(d) / "image.bin", Path(d) / "link.json"
        args = ["cf-link", *map(str, sources), "--base", f"{base:x}"]
        for n, v in symbols.items():
            args += ["--sym", f"{n}={v:x}"]
        for i in include_dirs or []:
            args += ["-I", str(i)]
        r = _build_cmd([*args, "--bin", str(binp), "--json", str(jsonp)])
        if r.returncode:
            raise BuildError((r.stderr or r.stdout).strip().removeprefix("patchkit: "))
        j = json.loads(jsonp.read_text())
        return CfLinked(
            j["base"],
            binp.read_bytes(),
            j["bss_size"],
            j["symbols"],
            [tuple(x) for x in j["text_ranges"]],
            j["relocs_applied"],
            j["histogram"],
        )


def flag(args: list[str], name: str) -> str | None:
    return (
        args[args.index(name) + 1]
        if name in args and args.index(name) + 1 < len(args)
        else None
    )


GALLERY = "build/gen/gallery.json"


def bundle(extra: list[Path] | None = None) -> dict:
    """Ports and modules as texts: the input of `pk_apply` in patchkit.wasm.

    Only the committed module roots by default (modules/ and the fixtures); `extra` roots (e.g. out/modules for generated
    oracles) are for tests, never for the published page. Builds the content modules' objects first (they are cached in out/cache/dko).
    """
    ports = [p.read_text() for p in sorted((ROOT / "devices" / "ports").glob("*.toml"))]
    modules = []
    roots = [*(ROOT / r for r in modroots.REPO_ROOTS), *(extra or [])]
    manifests = [
        (m, tomllib.loads(m.read_text()))
        for r in roots
        for m in sorted(r.rglob("module.toml"))
    ]
    # the objects of content modules are build output: build what is stale, then read them from the cache.
    # Core objects are committed and only read: a build here would rewrite a stale one (`patchkit:objects-check` finds it).
    committed = [ROOT / r for r in modroots.REPO_ROOTS]
    content = [
        str(m.parent)
        for m, d in manifests
        if any(r in m.parents for r in committed)
        and d["module"].get("category") != "core"
        and any(o.get("sources") for o in d.get("object", []))
    ]
    if content:
        r = _build_cmd(["build", *content])
        if r.returncode:
            raise BuildError((r.stderr or r.stdout).strip().removeprefix("patchkit: "))
    for m, d in manifests:
        files = {
            o["path"]: modroots.object_path(d, m.parent, o).read_text()
            for o in d.get("object", [])
        }
        for r in d.get("registry", []):  # the permanent-number tables of frameworks
            if r.get("index_table"):
                files[r["index_table"]] = (m.parent / r["index_table"]).read_text()
        gallery = (
            m.parent / GALLERY
        )  # `patchkit gen`: machines and parameters, for the page
        if gallery.is_file():
            files[GALLERY] = gallery.read_text()
        modules.append({"manifest": m.read_text(), "files": files})
    # the device files with a [modules] section: the knob models the typed module sections are lowered with
    devices = []
    for f in sorted((ROOT / "devices").glob("*.toml")):
        text = f.read_text()
        if "modules" in tomllib.loads(text):
            devices.append(text)
    return {"ports": ports, "modules": modules, "devices": devices}


def sharc_listing(syx: Path, plan: dict) -> list[str]:
    """Disassemble the DSP sites of a plan from the patched image's boot stream."""
    from tools.patchkit.dsp_patch import Stream, section_blob
    from tools.sharc_dev import sharc_asm, sharc_isa

    stream = Stream(section_blob(syx, 7))
    isa = sharc_isa.load_instruction_set()
    out: list[str] = []

    def listing(addr: int, length: int, title: str) -> None:
        out.append(f"  {title}")
        data = stream.read(addr, length)
        off = 0
        while off < len(data):
            r = isa.decode_bytes(data[off : off + 6])
            n = r.instruction.extent_bytes if r.instruction else 2
            parcels = [
                int.from_bytes(data[off + i : off + i + 2], "little")
                for i in range(0, n, 2)
            ]
            try:
                text = sharc_asm.decode_text(0, parcels)
            except Exception as e:  # noqa: BLE001
                text = f"?? ({e})"
            pc = (
                ((addr + off - 0x28000000) // 2)
                if addr >= 0x28000000
                else 0xB80000 + (addr + off - 0x20000000) // 2
            )
            out.append(
                f"    {pc:#08x}  {' '.join(f'{p:04x}' for p in parcels):<14} {text}"
            )
            off += n

    for w in plan["writes"]:
        if w["cpu"] != "sharc":
            continue
        if w["kind"] == "patch":
            listing(w["addr"], w["len"], f"[{w['owner']}] {w['what']} (after)")
        elif w["kind"] == "place" and w.get("code"):
            for lo, hi in w["code"]:
                listing(lo, hi - lo, f"[{w['owner']}] code at {lo:#x}")
    return out


def cmd_explain(args: list[str]) -> int:
    r = run(["explain", *args], capture=True)
    sys.stdout.write(r.stdout)
    sys.stderr.write(r.stderr)
    fw, mods = flag(args, "--firmware"), flag(args, "--modules")
    if r.returncode or not fw:
        return r.returncode
    tmp = ROOT / "out" / "patch" / "explain"
    tmp.mkdir(parents=True, exist_ok=True)
    out, plan = tmp / "out.syx", tmp / "plan.json"
    a = run(
        [
            "apply",
            "--firmware",
            fw,
            "--modules",
            mods or "",
            "-o",
            str(out),
            "--plan",
            str(plan),
        ],
        capture=True,
    )
    if a.returncode:
        return a.returncode
    lines = sharc_listing(out, json.loads(plan.read_text()))
    if lines:
        print("\nSHARC+ listing (patched image):")
        print("\n".join(lines))
    return 0


def cmd_patch(args: list[str]) -> int:
    fw, mods = flag(args, "--firmware"), flag(args, "--modules")
    if not fw:
        raise SystemExit(
            "patch needs --firmware X.syx (and --modules a,b; without it the device's patch set, `--set NAME` for another)"
        )
    # no --modules: the device's patch set (devices/*.toml [patch_sets]); the file name carries the set's name
    label = mods or f"set-{flag(args, '--set') or 'device'}"
    out = flag(args, "-o")
    if out is None:
        d = ROOT / "out" / "patch"
        d.mkdir(parents=True, exist_ok=True)
        out = str(d / f"{Path(fw).stem}+{re.sub(r'[^A-Za-z0-9.-]+', '_', label)}.syx")
    plan = flag(args, "--plan") or str(Path(out).with_suffix(".plan.json"))
    rest = [a for a in args if a not in ("-o", out, "--plan", plan)]
    r = run(["apply", *rest, "-o", out, "--plan", plan])
    if r.returncode == 0:
        print(f"plan: {plan}")
    return r.returncode


def cmd_pack(args: list[str]) -> int:
    """`pack MODULE [--device dt2|dn2] [-o OUT.dkm] [--name N] [--runtime-min V]`: a run-time module as a .dkm."""
    module = next(
        (
            a
            for i, a in enumerate(args)
            if not a.startswith("-") and (i == 0 or args[i - 1] not in VALUE_FLAGS)
        ),
        None,
    )
    if module is None:
        raise SystemExit("pack needs a module id, e.g. pack <author/name> --device dt2")
    out = flag(args, "-o")
    if out is None:
        d = ROOT / "out" / "patch"
        d.mkdir(parents=True, exist_ok=True)
        device = flag(args, "--device")
        out = str(
            d / (module.rsplit("/", 1)[-1] + (f"-{device}" if device else "") + ".dkm")
        )
        args = [*args, "-o", out]
    r = run(["pack", *args])
    if r.returncode == 0:
        print(f"dkm: {out}")
    return r.returncode


VALUE_FLAGS = ("-o", "--device", "--name", "--runtime-min")


def cmd_inject(args: list[str]) -> int:
    from tools.control.digi_client import main as client_main

    runtime = "--runtime" in args
    set_name = flag(args, "--patch-set")
    syx = next(
        (
            a
            for i, a in enumerate(args)
            if not a.startswith("-") and (i == 0 or args[i - 1] != "--patch-set")
        ),
        None,
    )
    if syx is None and runtime:
        raise SystemExit("inject --runtime needs the stock .syx to patch")
    if syx is None:
        outs = sorted(
            (ROOT / "out" / "patch").glob("*.syx"), key=lambda p: p.stat().st_mtime
        )
        if not outs:
            raise SystemExit(
                "inject: no patched image under out/patch/; run `mise run patch` first"
            )
        syx = str(outs[-1])
    rest = ["inject", syx]
    if runtime:  # the emulator patches the stock image itself (firmware.load {runtime}); nothing is patched here
        rest += ["--runtime"] + (["--patch-set", set_name] if set_name else [])
    if "--hot" in args:
        rest += ["--start", "snapshot"]
    return client_main(rest)


def main(argv: list[str]) -> int:
    if not argv:
        print(__doc__)
        return 2
    cmd, args = argv[0], argv[1:]
    if args[:1] == ["--"]:
        args = args[1:]
    if cmd == "bin":
        print(binary())
        return 0
    if cmd == "bundle":
        out = Path(flag(args, "-o") or ROOT / "out" / "patch" / "bundle.json")
        out.parent.mkdir(parents=True, exist_ok=True)
        extra = flag(args, "--modules-dir")
        out.write_text(
            json.dumps(bundle([Path(extra)] if extra else None), sort_keys=True)
        )
        print(out)
        return 0
    if cmd == "patch":
        return cmd_patch(args)
    if cmd == "explain":
        return cmd_explain(args)
    if cmd == "inject":
        return cmd_inject(args)
    if cmd == "pack":
        return cmd_pack(args)
    if cmd in ("build", "dko", "cf-link", "sharc"):
        r = _build_cmd([cmd, *args])
        sys.stdout.write(r.stdout)
        sys.stderr.write(r.stderr)
        return r.returncode
    if cmd in (
        "list",
        "inspect",
        "check",
        "verify",
        "preflight",
        "apply",
        "flash",
        "rebuild",
        "gen",
        "reference",
        "needs",
        "dkm-info",
        "desc",
    ):
        return run([cmd, *args]).returncode
    print(__doc__)
    return 2


if __name__ == "__main__":
    os.chdir(ROOT)
    sys.exit(main(sys.argv[1:]))
