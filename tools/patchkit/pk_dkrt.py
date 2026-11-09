#!/usr/bin/env python3
"""Run the digikit runtime (core/runtime) in the emulator: patch, pack modules, build a card, boot, observe.

    uv run python -m tools.patchkit.pk_dkrt proof [--image dt2-1.17] [--keep DIR] [--control]

`proof` is the loader gate on one image (docs/findings/29-runtime-v0.md, converted to boot-time loading in finding 37 when hot reload
was archived): it patches core/runtime into the stock firmware, packs test/dyn-demo (and four broken variants) into `.dkm` files,
builds a +Drive card with them in the module store and a selection record that names, in order, a file with a bad sha256, a file
with an import the runtime does not have, a file that is not there, the good module with api major 2, the good module with api
minor 255 (both E_CONTRACT), the good module, and the good module again. The boot hook loads
the selection (through packages/emulator/boot/examples/cf_script.rs); the proof reads the boot report (the five refusals, the load,
the duplicate refused), the module's counter rising while the UI task runs, and checks that the refused files left nothing behind
(arena, slots, module records). After the boot a `dkrt_load` (mailbox and direct call) answers `E_BOOT_ONLY` and a command 2 (the
old unload) is refused as unknown. The write-watch on `cf.expansion` (the runtime's code and the arena allowed as writers) and
the flush check run through all of it. Prints one JSON report; exit status 0 when every check holds.

The state layout peeked here is `State` and `Mailbox` in modules/core/runtime/src/rs/src/lib.rs and modules/core/runtime/sdk/src/lib.rs.
"""

from __future__ import annotations

import argparse
import copy
import hashlib
import json
import os
import shutil
import struct
import subprocess
import sys
import tempfile
import tomllib
import zlib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]

from tools import python_env  # noqa: E402
from tools.device.dkproto import SETTINGS_SECTORS  # noqa: E402
from tools.emulator import digi_env, native_target  # noqa: E402
from tools.firmware import config  # noqa: E402
from tools.patchkit import modroots, pk_device  # noqa: E402

# dkrt-sdk result codes
OK, E_NOT_FOUND, E_BAD_STORE, E_READ, E_SHA, E_FORMAT, E_RUNTIME, E_IMPORT = (
    0,
    -1,
    -2,
    -3,
    -4,
    -5,
    -6,
    -7,
)
E_NOMEM, E_INIT, E_LOADED, E_SLOT = -8, -9, -10, -11
E_FORMAT_CMD, E_BOOT_ONLY, E_CONTRACT = -5, -45, -48
SEL_MAGIC, SEL_VERSION, SEL_PATH = 0x444B534C, 1, 56
# the selection record's paths (sdk/sdk.toml [limits.max])
SEL_MAX = pk_device.contract_max()["sel_max"]
BOOT_FIELDS = 10  # DkrtBootInfo words before `results`
ARENA_BYTES = pk_device.shared_limits().arena_bytes  # devices/*.toml [limits]
MAX_MODULES = pk_device.shared_limits().modules
SLOTS, LOG_ENTRIES = 8, 16
# offsets in State (repr(C); sizes in the comments of lib.rs)
ST_TICKS, ST_LAST, ST_LOADS, ST_SLOT_CALLS, ST_FREE = 8, 12, 16, 20, 24
ST_ARENA_LO, ST_ARENA_HI, ST_LOG_NEXT = 28, 32, 36
ST_SLOTS, ST_ENTERED, ST_MODULES, ST_LOG = 40, 72, 104, 808
MODULE_SIZE = 88
STATE_BYTES = 1256
MB_CMD, MB_SEQ, MB_RESULT, MB_ARG = 0, 4, 8, 12
DEVICE_SYX = config.DEVICE_SYX  # re-exported for pk_dkrt_boot, pk_cdemo and the tests
RUN = 25_000_000  # instructions between samples: the UI task's event loop passes about every 4 million


class ProofError(RuntimeError):
    pass


def be32(b: bytes, off: int) -> int:
    return struct.unpack_from(">I", b, off)[0]


def sbe32(b: bytes, off: int) -> int:
    return struct.unpack_from(">i", b, off)[0]


def cstr(b: bytes) -> str:
    return b.split(b"\0", 1)[0].decode("latin-1")


class Tools:
    """The release binaries the proof needs, built in the 'nogen' target (no SHARC core)."""

    def __init__(self) -> None:
        from tools.devtools import stablebin

        env = digi_env.rust_env()
        target = Path(native_target.target_dir(""))
        # One build at a time, then immutable copies: a cargo build rewrites release/<bin> even when
        # nothing changed, and a process that execs it meanwhile can hang (tools/devtools/stablebin.py).
        with stablebin.build_lock(target):
            r = subprocess.run(
                [
                    "cargo", "build", "--release", "--locked", "--offline", "--target-dir", str(target),
                    "-p", "patchkit", "--bin", "patchkit",
                    "-p", "plusdrive-format", "--bin", "plusdrive",
                    "-p", "elektron-native-boot", "--example", "cf_script",
                ],
                cwd=ROOT, env=env, capture_output=True, text=True,
            )  # fmt: skip
            if r.returncode:
                raise ProofError(f"cargo build failed:\n{r.stderr[-3000:]}")
            rel = target / "release"
            self.patchkit = stablebin.stable_copy(rel / "patchkit", target)
            self.plusdrive = stablebin.stable_copy(rel / "plusdrive", target)
            self.script = stablebin.stable_copy(rel / "examples" / "cf_script", target)

    def run(self, *argv: object, **kw) -> subprocess.CompletedProcess:
        return subprocess.run(
            [str(a) for a in argv], cwd=ROOT, env=python_env(), capture_output=True, text=True, **kw
        )  # fmt: skip

    def patchkit_run(self, *args: object) -> str:
        r = self.run(
            self.patchkit, "--root", os.environ.get("DIGI_PK_ROOT") or ROOT, *args
        )
        if r.returncode:
            raise ProofError(
                f"patchkit {' '.join(map(str, args[:2]))}: {r.stderr}{r.stdout}"
            )
        return r.stdout


def patch(tools: Tools, syx: Path, out: Path, modules: str = "core/runtime") -> dict:
    plan = out.with_suffix(".plan.json")
    tools.patchkit_run(
        "apply", "--firmware", syx, "--modules", modules, "-o", out, "--plan", plan
    )
    return json.loads(plan.read_text())


def pack(tools: Tools, module: str, out: Path, *extra: object) -> None:
    out.parent.mkdir(parents=True, exist_ok=True)
    tools.patchkit_run("pack", module, "-o", out, *extra)


def broken_variants(work: Path, tools: Tools, module: str, folder: Path) -> None:
    """From a module's object: `bad-sha.dkm` (one byte of the image changed after packing) and `bad-import.dkm`
    (a relocation against a name the runtime does not provide), and `contract-major.dkm` / `contract-minor.dkm` (api major 2; api
    minor 255, the file sha256 recomputed)."""
    man = (modroots.module_dir(module) / "module.toml").read_text()
    obj_path = next(
        ln.split("=", 1)[1].strip().strip('"')
        for ln in man.splitlines()
        if ln.startswith("path =")
    )
    # the object is build output (`pack` of the good file built it); the edited copy is a module without sources
    parsed = tomllib.loads(man)
    built = modroots.object_path(
        parsed, modroots.module_dir(module), parsed["object"][0]
    )
    obj = json.loads(built.read_text())
    good = folder / "modules" / "dyn-demo.dkm"
    bad = bytearray(good.read_bytes())
    bad[-1] ^= (
        0x5A  # the image is the last part of the file: the header sha256 no longer matches
    )
    (folder / "modules" / "bad-sha.dkm").write_bytes(bad)
    imp = copy.deepcopy(obj)
    data = next(s for s in imp["sections"] if s["kind"] == "data")
    imp["symbols"].append(
        {"name": "fw_does_not_exist", "section": None, "value": 0, "global": True}
    )
    imp["relocs"].append(
        {
            "section": data["name"],
            "offset": 0,
            "type": "cf.32",
            "symbol": "fw_does_not_exist",
            "addend": 0,
        }
    )
    # the edited object carries the build's ISA check record for its new bytes: the instructions are the checked ones, only a relocation is
    # added (`patchkit pack` refuses an object whose record does not match its bytes)
    from tools.patchkit import patchkit

    imp["toolchain"][patchkit.ISA_CHECK_KEY] = patchkit.isa_digest(imp)
    # the dyn-demo object has a .rodata string section of 14+ bytes: reuse it as the place of the new reloc
    tmp = work / "mods" / "test" / "dyn-demo-import"
    (tmp / "build").mkdir(parents=True, exist_ok=True)
    (tmp / "build" / "o.dko.json").write_text(json.dumps(imp))
    no_sources = "".join(
        ln for ln in man.splitlines(keepends=True) if not ln.startswith("sources =")
    )
    (tmp / "module.toml").write_text(
        no_sources.replace(
            'id = "test/dyn-demo"', 'id = "test/dyn-demo-import"'
        ).replace(obj_path, "build/o.dko.json")
    )
    r = tools.run(tools.patchkit, "--root", ROOT, "pack", "test/dyn-demo-import", "--modules-dir", work / "mods",
                  "-o", folder / "modules" / "bad-import.dkm", "--name", "bad-import")  # fmt: skip
    if r.returncode:
        raise ProofError(f"pack bad-import: {r.stderr}")
    # the api contract of the header (bytes 0x38 major, 0x39 minor) changed, the file sha256 made right again;
    # minor 0xFF is above any runtime's, so the file stays too new when the SDK's api minor grows
    for name, offset, value in (
        ("contract-major", 0x38, 2),
        ("contract-minor", 0x39, 0xFF),
    ):
        forged = bytearray(good.read_bytes())
        forged[offset] = value
        struct.pack_into(">I", forged, 0x08, len(forged))
        forged[0x10:0x30] = bytes(32)
        forged[0x10:0x30] = hashlib.sha256(bytes(forged)).digest()
        (folder / "modules" / f"{name}.dkm").write_bytes(forged)


def build_card(tools: Tools, folder: Path, image: Path, syx: Path, device: str) -> None:
    image.unlink(missing_ok=True)
    r = tools.run(
        tools.plusdrive, "build", folder, "-o", image, "--device", device, "--syx", syx
    )
    if r.returncode:
        raise ProofError(f"plusdrive build: {r.stderr}{r.stdout}")


def make_record(
    paths: list[str], seq: int = 1, counter: int = 0, flags: int = 0
) -> bytes:
    """One copy of the selection record (docs/design/digikit-store.md)."""
    if len(paths) > SEL_MAX or any(len(p) >= SEL_PATH or not p for p in paths):
        raise ProofError("selection: at most 8 paths of 1 to 55 characters")
    b = bytearray(512)
    struct.pack_into(
        ">IIIIII", b, 0, SEL_MAGIC, SEL_VERSION, seq, counter, flags, len(paths)
    )
    for i, p in enumerate(paths):
        b[32 + i * SEL_PATH : 32 + i * SEL_PATH + len(p)] = p.encode()
    struct.pack_into(">I", b, 508, zlib.crc32(bytes(b[:508])))
    return bytes(b)


def write_selection(card: Path, paths: list[str], device: str) -> None:
    """Both copies of a selection record over the two record sectors of a card image file of DEVICE (`dt2`, `dn2`)."""
    rec = make_record(paths)
    s = pk_device.store_of(device)
    with open(card, "r+b") as f:
        for sector in (s.sel_a, s.sel_b):
            f.seek(sector * 512)
            f.write(rec)


def watch_ranges(rt: dict, lo: int, hi: int) -> list[tuple[int, int]]:
    """[lo, hi) minus the buffers the card reads fill by DMA (the staging buffer, the 512-byte record buffer)."""
    holes = [(rt["stage"], 64 * 1024)]
    # 512-byte buffers the card reads land in by DMA: the selection record, the store's header sectors, the fingerprint of a list
    for name in (
        "DKRT_SECBUF",
        "DKRT_STORE_PEEK",
        "DKRT_STORE_FP",
        "DKRT_SETTINGS_HDR",
    ):
        if name in rt["sym"]:
            holes.append((rt["sym"][name], 512))
    # the settings record's RAM copy: the boot's card read of the settings area lands in it by DMA
    if "DKRT_SETTINGS" in rt["sym"]:
        holes.append((rt["sym"]["DKRT_SETTINGS"], SETTINGS_SECTORS * 512))
    out, at = [], lo
    for start, size in sorted(holes):
        if start > at:
            out.append((at, start))
        at = start + size
    out.append((at, hi))
    return out


def runtime_ranges(plan: dict) -> dict:
    """Addresses from a patch plan: the runtime's code ranges, its statics, the arena."""
    sym = {
        k.split(":", 1)[1]: v
        for k, v in plan["symbols"].items()
        if k.startswith("core/runtime@")
    }
    code = [
        tuple(r)
        for w in plan["writes"]
        if w["kind"] == "place" and w["owner"].startswith("core/runtime@")
        for r in w.get("code", [])
    ]
    return {
        "state": sym["DKRT_STATE"],
        "mailbox": sym["DKRT_MAILBOX"],
        "arena": sym["DKRT_ARENA"],
        "stage": sym["DKRT_STAGE"],
        "code": code,
        "sym": sym,
    }


def run_script(tools: Tools, syx: Path, base_sha: str, card: Path, script: list[dict], env: dict[str, str],
               work: Path, state: Path | None = None, timeout: int = 1800) -> list[dict]:  # fmt: skip
    path = work / "script.json"
    path.write_text(json.dumps(script))
    cmd = [tools.script, syx, path, "--base-sha", base_sha, "--card", card]
    if state:
        cmd += ["--state", state]
    r = subprocess.run([str(c) for c in cmd], cwd=ROOT, capture_output=True, text=True, timeout=timeout,
                       env={**python_env(), **env})  # fmt: skip
    if r.returncode:
        raise ProofError(f"cf_script failed ({r.returncode}):\n{r.stderr[-2500:]}")
    return [json.loads(ln) for ln in r.stdout.splitlines() if ln.startswith("{")]


def cmd_ops(
    rt: dict, kind: int, path: str, tag: str, wait: int = 40_000_000
) -> list[dict]:
    """Script ops that post a mailbox command and wait for its result: path, then the sequence read, then cmd."""
    mb = rt["mailbox"]
    return [
        {
            "op": "poke",
            "addr": hex(mb + MB_ARG),
            "hex": (path.encode() + b"\0").hex(),
            "tag": f"{tag}:arg",
        },
        {"op": "peek", "addr": hex(mb + MB_SEQ), "len": 4, "tag": f"{tag}:seq0"},
        {"op": "poke32", "addr": hex(mb + MB_CMD), "value": kind, "tag": f"{tag}:cmd"},
        {
            "op": "wait32",
            "addr": hex(mb + MB_CMD),
            "eq": 0,
            "max": wait,
            "tag": f"{tag}:wait",
        },
        {"op": "peek", "addr": hex(mb + MB_RESULT), "len": 8, "tag": f"{tag}:result"},
    ]


def proof(
    image: str = "dt2-1.17", keep: Path | None = None, control: bool = False
) -> dict:
    """The full proof; returns the report with a list of checks (`ok` is their conjunction)."""
    if image not in DEVICE_SYX:
        raise ProofError(f"no proof for {image}")
    device = image[:3]
    syx_stock = config.stock(DEVICE_SYX[image])
    if not syx_stock.exists():
        raise ProofError(f"{syx_stock} is missing")
    tools = Tools()
    work = keep or Path(tempfile.mkdtemp(prefix="pk-dkrt-"))
    work.mkdir(parents=True, exist_ok=True)
    folder = work / "plus_drive"
    store = folder / "digikit"
    (store / "modules").mkdir(parents=True, exist_ok=True)
    patched = work / f"{image}-runtime.syx"
    plan = patch(tools, syx_stock, patched)
    rt = runtime_ranges(plan)
    pack(tools, "test/dyn-demo", store / "modules" / "dyn-demo.dkm")
    broken_variants(work, tools, "test/dyn-demo", store)
    card = work / f"{device}.img"
    build_card(tools, folder, card, syx_stock, device)
    selection = [
        "modules/bad-sha.dkm",
        "modules/bad-import.dkm",
        "modules/nope.dkm",
        "modules/contract-major.dkm",
        "modules/contract-minor.dkm",
        "modules/dyn-demo.dkm",
        "modules/dyn-demo.dkm",
    ]
    write_selection(card, selection, device)

    st, mb, arena = rt["state"], rt["mailbox"], rt["arena"]
    sym = rt["sym"]
    region_lo = min(
        w["addr"]
        for w in plan["writes"]
        if w["kind"] == "place" and "cf.expansion" in w["what"]
    )
    region_hi = 0x47FE0000
    # the staging buffer is written by the card read's DMA (a legitimate writer that has no PC): leave it out
    # (the top 64 KiB hold the stack of the harness's direct calls)
    watch = watch_ranges(rt, region_lo, region_hi - 0x10000)
    allow = list(rt["code"]) + [(arena, arena + ARENA_BYTES)]
    env = {
        "DIGI_FLUSH_CHECK": "1",
        "DIGI_WRITE_WATCH": ",".join(f"{a:x}-{b:x}" for a, b in watch),
        "DIGI_WRITE_WATCH_ALLOW_PC": "40000000-40000001"
        if control
        else ",".join(f"{a:x}-{b:x}" for a, b in allow),
        "DIGI_WRITE_WATCH_AFTER": "cacr",
        "DIGI_WRITE_WATCH_OUT": str(work / "ww.jsonl"),
    }

    def peek(tag: str, addr: int, n: int, ptr: bool = False) -> dict:
        return {
            "op": "peek",
            "addr": hex(addr),
            "len": n,
            "tag": tag,
            **({"ptr": True} if ptr else {}),
        }

    boot = sym["DKRT_BOOT"]
    counter_ptr = st + ST_LOG + 24  # value of log entry 0 = the demo's counter address
    run = lambda n, tag: {"op": "run", "insns": n, "tag": tag}  # noqa: E731
    script: list[dict] = [
        run(30_000_000, "settle"),
        peek("boot", boot, 4 * (BOOT_FIELDS + SEL_MAX)),
        peek("state0", st, STATE_BYTES),
        peek("c1", counter_ptr, 16, ptr=True),
        run(RUN, "r1"),
        peek("c2", counter_ptr, 16, ptr=True),
        run(RUN, "r2"),
        peek("c3", counter_ptr, 16, ptr=True),
        # after the boot: the module set is fixed (mailbox load, the old unload command, a direct call)
        *cmd_ops(rt, 1, "modules/dyn-demo.dkm", "late"),
        *cmd_ops(rt, 2, "test/dyn-demo", "unload"),
        {
            "op": "poke",
            "addr": hex(mb + MB_ARG),
            "hex": b"modules/dyn-demo.dkm\0".hex(),
            "tag": "p1",
        },
        {
            "op": "call",
            "pc": hex(sym["dkrt_load"]),
            "args": [mb + MB_ARG],
            "tag": "late_direct",
        },
        peek("state1", st, STATE_BYTES),
        peek("table_bad", sym["DKRT_TABLE_BAD"], 4),
        run(RUN, "r3"),
        peek("c4", counter_ptr, 16, ptr=True),
    ]
    out = run_script(
        tools, patched, plan["base"]["syx_sha256"], card, script, env, work
    )
    by: dict[str, dict] = {r["tag"]: r for r in out if r.get("tag")}
    raw = lambda tag: bytes.fromhex(by[tag]["hex"])  # noqa: E731
    checks: list[tuple[str, bool, str]] = []

    def check(name: str, ok: bool, detail: str = "") -> None:
        checks.append((name, bool(ok), detail))

    s0, s1, bi = raw("state0"), raw("state1"), raw("boot")
    arena_lo, arena_hi = be32(s0, ST_ARENA_LO), be32(s0, ST_ARENA_HI)
    check("runtime started by the UI-loop hook", be32(s0, 0) == 0x444B5254 and be32(s0, ST_TICKS) > 0,
          f"magic {be32(s0, 0):#x}, ticks {be32(s0, ST_TICKS)}")  # fmt: skip
    check("arena set up", arena_lo == (arena + 15) & ~15 and arena_hi - arena_lo >= ARENA_BYTES - 32,
          f"{arena_lo:#x}..{arena_hi:#x}")  # fmt: skip
    results = [sbe32(bi, 4 * (BOOT_FIELDS + k)) for k in range(len(selection))]
    want = [
        E_SHA,
        E_IMPORT,
        E_NOT_FOUND,
        E_CONTRACT,
        E_CONTRACT,
        OK,
        E_SLOT,
    ]  # the duplicate meets the taken slot before the name check
    check(
        "boot report: five refusals, the load, the duplicate refused (slot taken)",
        results == want,
        f"{results}, want {want}",
    )
    check("boot report: count 7, loaded 1", be32(bi, 4 * 7) == len(selection) and be32(bi, 4 * 8) == 1,
          f"count {be32(bi, 4 * 7)}, loaded {be32(bi, 4 * 8)}")  # fmt: skip
    m0 = ST_MODULES
    base = be32(s0, m0 + 40)
    check("module record", be32(s0, m0) == 1 and cstr(s0[m0 + 4 : m0 + 36]) == "test/dyn-demo"
          and be32(s0, ST_SLOTS) != 0 and be32(s0, ST_SLOTS) > base,
          f"base {base:#x} slot0 {be32(s0, ST_SLOTS):#x}")  # fmt: skip
    check("refused files left nothing behind: the first arena block is the module's, one module, one slot, one load",
          base == arena_lo + 16
          and all(be32(s0, ST_MODULES + i * MODULE_SIZE) == 0 for i in range(1, MAX_MODULES))
          and all(be32(s0, ST_SLOTS + 4 * i) == 0 for i in range(1, SLOTS)) and be32(s0, ST_LOADS) == 1,
          f"base {base:#x} (arena {arena_lo:#x}), loads {be32(s0, ST_LOADS)}")  # fmt: skip
    cs = [raw(f"c{i}") for i in range(1, 5)]
    n = [be32(c, 4) for c in cs]
    check("counter magic", be32(cs[1], 0) == 0x44594E4D)
    check("counter rises while loaded", n[0] < n[1] < n[2] < n[3], f"{n}")

    def result(tag: str) -> tuple[int, int]:
        r = raw(f"{tag}:result")
        return sbe32(r, 0), be32(raw(f"{tag}:seq0"), 0)

    check("a mailbox load after the boot is refused (E_BOOT_ONLY)", result("late")[0] == E_BOOT_ONLY,
          f"{result('late')[0]}")  # fmt: skip
    check("the old unload command (2) is refused as unknown", result("unload")[0] == E_FORMAT_CMD,
          f"{result('unload')[0]}")  # fmt: skip
    check("a direct dkrt_load after the boot answers E_BOOT_ONLY", by["late_direct"]["ok"]
          and by["late_direct"]["d0"] == (E_BOOT_ONLY & 0xFFFFFFFF), str(by["late_direct"]))  # fmt: skip
    check("the late requests changed nothing (one module, one load)",
          be32(s1, ST_LOADS) == 1 and be32(s1, m0) == 1, f"loads {be32(s1, ST_LOADS)}")  # fmt: skip
    # the table check (table.rs) ran after every boot-time load attempt; it logs TABLE_BAD and sets this byte when an invariant failed
    check("table check: no TABLE_BAD (the runtime's tables stayed consistent through the boot loads)",
          raw("table_bad")[0] == 0, f"DKRT_TABLE_BAD {raw('table_bad')[0]}")  # fmt: skip
    summary = json.loads((work / "ww.summary.json").read_text())
    if control:
        check(
            "control: the watch sees the runtime's writes",
            summary["events"] > 0,
            f"{summary['events']} events",
        )
    else:
        check("write-watch: no write into cf.expansion by any other code", summary["events"] == 0,
              f"{summary['events']} events: {summary['per_pc'][:3]} {summary['per_dma'][:3]}")  # fmt: skip
    check(
        "flush check: 0 hits",
        summary["flush_hits"] == [],
        f"{summary['flush_hits'][:2]}",
    )
    check("flush check saw traffic", summary["flush_fetches_checked"] > 0 and summary["cpushl"] > 0,
          f"fetches {summary['flush_fetches_checked']}, cpushl {summary['cpushl']}")  # fmt: skip
    return {
        "image": image,
        "work": str(work),
        "ok": all(c[1] for c in checks),
        "checks": [{"name": a, "ok": b, "detail": c} for a, b, c in checks],
        "numbers": {
            "boot_results": results,
            "counter": n,
            "write_watch": {
                k: summary[k]
                for k in (
                    "events",
                    "flush_stores_tracked",
                    "flush_fetches_checked",
                    "cpushl",
                )
            },
            "runtime_code_bytes": sum(b - a for a, b in rt["code"]),
            "arena_bytes": arena_hi - arena_lo,
        },
    }


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    sub = ap.add_subparsers(dest="cmd", required=True)
    p = sub.add_parser("proof")
    p.add_argument("--image", default="dt2-1.17")
    p.add_argument("--keep", type=Path)
    p.add_argument("--control", action="store_true")
    a = ap.parse_args(argv)
    rep = proof(a.image, a.keep, a.control)
    print(json.dumps(rep, indent=1))
    if not a.keep:  # a temp work dir: removed after a clean run, kept after a failure
        if rep["ok"]:
            shutil.rmtree(rep["work"], ignore_errors=True)
        else:
            print(f"work dir kept: {rep['work']}", file=sys.stderr)
    return 0 if rep["ok"] else 1


if __name__ == "__main__":
    sys.exit(main())
