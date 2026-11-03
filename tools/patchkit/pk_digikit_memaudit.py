#!/usr/bin/env python3
"""The ColdFire memory audit: the `debug` part `memaudit` of the digikit scenarios (docs/TESTING.md).

Only the scenarios `memaudit-{dt2,dn2}-full` still run it. The clean sets, the negative control and `mise run module:audit` are the TypeScript
device tests `modules/core/runtime/tests/memaudit*.device.test.ts` (shared code: tests/regression/runtime/memaudit-kit.ts). Delete this file
when the full set is a TypeScript test.

A module set is loaded on a ColdFire-only rig, the emulator's write guard (`guard.set`, docs/CONTROL.md) is armed, the UI is driven for a
bounded time (the DIGIKIT menu, a run-time machine on a track with parameter turns, a p-lock, one project save and load), and
`guard.report` must show no hit.

Protected: the ColdFire free regions of the port file (devices/ports/<image>.toml, every `cpu = "cf"` block: cf.flash_cave, cf.expansion,
minus `excludes`), except the block of each loaded module (`cf_base` .. `cf_base + cf_size` of the MODULES reply). The part of the
arena outside the module blocks is labelled `arena`.
Allowed writers (`allow_pc`): the code of the patch objects in the image (the placed objects of the plan) and the runtime's code (its
object up to the start of the arena, which is the end of the object). The runtime keeps the arena's bookkeeping; a module's code may
write its own block and nothing else.
Not covered: stores by DMA and by the host, and a store made by the runtime for a module (an API call: the writer is the runtime).
The UI drive runs with the guard armed after the boot; a second cold boot with the write watch on the same ranges audits the load and `dkm_init`.
"""

from __future__ import annotations

import tomllib
from pathlib import Path
from typing import Any

from tools.patchkit import pk_capacity as P
from tools.patchkit import pk_dkrt_boot as B
from tools.patchkit import pk_dkrt_machine as M

ROOT = Path(__file__).resolve().parents[2]


def free_regions(image: str) -> list[tuple[str, int, int]]:
    """The `cpu = "cf"` regions of the port file with their `excludes` cut out: (name, lo, hi), as tests/regression/support.ts reads them."""
    port = tomllib.loads((ROOT / "devices" / "ports" / f"{image}.toml").read_text())
    out: list[tuple[str, int, int]] = []
    for r in port.get("region", []):
        if r.get("cpu") != "cf":
            continue
        lo = r["start"]
        for xl, xh in sorted(map(tuple, r.get("excludes", []))):
            if xl > lo:
                out.append((r["name"], lo, xl))
            lo = max(lo, xh)
        if lo < r["end"]:
            out.append((r["name"], lo, r["end"]))
    if not out:
        raise M.Fail(f"{image}: no cf regions in the port file")
    return out


def cut(
    ranges: list[tuple[str, int, int]], lo: int, hi: int, label: str | None = None
) -> list[tuple[str, int, int]]:
    """RANGES without [lo, hi); with LABEL the cut-out part stays, under that label (the pieces of a range keep its name)."""
    out = []
    for name, a, b in ranges:
        if hi <= a or lo >= b:
            out.append((name, a, b))
            continue
        if a < lo:
            out.append((name, a, lo))
        if label is not None:
            out.append((label, max(a, lo), min(b, hi)))
        if hi < b:
            out.append((name, hi, b))
    return out


def patch_code(plan: dict, arena_lo: int) -> list[tuple[str, int, int]]:
    """(owner, lo, hi) of the code that may write anywhere it needs: every object the patchkit placed, and the hook tails; of the
    object that holds the arena only the part below the arena (code, data and the runtime's other bss)."""
    out = []
    for w in plan["writes"]:
        if w["cpu"] != "cf" or w["kind"] != "place":
            continue
        if not (w["what"].startswith("object in") or w["what"].startswith("hook tail")):
            continue
        lo, hi = w["addr"], w["addr"] + w["len"]
        if lo < arena_lo < hi:
            hi = arena_lo
        out.append((w["owner"].split("@")[0], lo, hi))
    return out


def who(pc: int, modules: list[dict], code: list[tuple[str, int, int]]) -> str:
    """The code a PC belongs to: a module's block, a patch object (the runtime included), else the stock firmware."""
    for m in modules:
        if m["cf_base"] <= pc < m["cf_base"] + m["cf_size"]:
            return f"module {m['name']}"
    for owner, lo, hi in code:
        if lo <= pc < hi:
            return f"patch {owner}"
    return "stock"


def plan_guard(
    image: str, plan: dict, mods: list[dict], arena: tuple[int, int]
) -> tuple[list[tuple[str, int, int]], list[tuple[str, int, int]]]:
    """(protected ranges, allowed code ranges) for the loaded MODULES and the arena bounds."""
    prot = free_regions(image)
    prot = cut(prot, arena[0], arena[1], "arena")
    for m in mods:
        prot = cut(prot, m["cf_base"], m["cf_base"] + m["cf_size"])
    prot = [p for p in prot if p[2] > p[1]]
    return prot, patch_code(plan, arena[0])


def arm(rig: Any, prot: list, code: list) -> None:
    rig.emu.call(
        "guard.set",
        protect=[{"lo": lo, "hi": hi, "label": n} for n, lo, hi in prot],
        allow_pc=[{"lo": lo, "hi": hi} for _, lo, hi in code],
    )


def block_words(rig: Any, mods: list[dict]) -> dict[str, bytes]:
    """The current bytes of every module's block, read from guest memory (PEEK is the debug module's, and this set does not load it)."""

    def block(base: int, size: int) -> bytes:
        words = (rig.w(base + 4 * i) for i in range((size + 3) // 4))
        return b"".join(w.to_bytes(4, "big") for w in words)[:size]

    return {m["name"]: block(m["cf_base"], m["cf_size"]) for m in mods}


def drive(c: Any, rig: Any, machines: list[dict]) -> None:
    """The UI for a bounded time: the DIGIKIT menu and its MODULES page (looked at, nothing ticked), the run-time machines on their tracks with
    parameter turns, a p-lock and a sound lock on step 1, one project save, an edit, a second save and a load."""
    from tools.patchkit import pk_digikit_project as P

    s = rig.s

    def tap(key: str, ms: int = 300) -> None:
        s.do_tap(key)
        s.run_ms(ms)

    tap(
        "NO", 400
    )  # SETTINGS > DIGIKIT (the ninth row) > MODULES, as in the menu scenarios
    tap("SETTINGS", 600)
    for _ in range(8):
        tap("DOWN", 200)
    tap("YES", 700)
    for _ in range(3):
        tap("DOWN", 200)
    tap("YES", 900)
    for key in ("DOWN", "DOWN", "UP", "NO", "NO", "NO"):
        tap(key, 300)
    P.save_project(rig)
    spec = {
        "machines": [
            {"track": m["track"], "type": rig.dyn_type(*m["machine"].split(":")), **m}
            for m in machines
        ]
    }
    P.build_project(c, rig, spec)
    P.save_project(rig)
    P.load_project(rig)
    s.run_ms(1000)


def load_pass(
    c: Any,
    img: Any,
    card: Path,
    prot: list[tuple[str, int, int]],
    code: list[tuple[str, int, int]],
    mods: list[dict],
) -> list[dict]:
    """The second pass: a cold boot of the same card with the write watch on the protected ranges from the first cache enable on, so the
    load of the modules and their `dkm_init` are audited too. The ranges come from the first pass (the arena is deterministic). Returns
    the CPU stores (DMA events are not judged), each with its writer."""
    import hashlib
    import json

    key = hashlib.sha256(card.name.encode() + img.syx.read_bytes()).hexdigest()[:10]
    tag = f"scn-ma{key}"
    out = c.work / f"{tag}.writewatch.jsonl"
    env = {
        "DIGI_WRITE_WATCH": ",".join(f"0x{a:x}-0x{b:x}" for _, a, b in prot),
        "DIGI_WRITE_WATCH_ALLOW_PC": ",".join(f"0x{a:x}-0x{b:x}" for _, a, b in code),
        "DIGI_WRITE_WATCH_AFTER": "cacr",
        "DIGI_WRITE_WATCH_OUT": str(out),
    }
    with c.env.boot(img, card, tag, watch=False, extra_env=env, cf_only=True) as rig:
        rig.boot_info()
        rig.s.run_ms(300)
    hits = []
    for f in sorted(c.work.glob(f"{tag}.writewatch*.jsonl")):
        for line in f.read_text().splitlines():
            e = json.loads(line)
            if e.get("t") != "w":
                continue
            pc, addr = int(e["pc"], 16), int(e["addr"], 16)
            hits.append(
                {
                    "pc": pc,
                    "addr": addr,
                    "size": e["size"],
                    "writer": who(pc, mods, code),
                }
            )
    return hits


def run_memaudit(c: Any) -> None:
    from tools.patchkit import pk_digikit_debug as DBG

    t = c.table()
    names = list(t["modules"])
    files = {m: c.pack(m, m.split("/")[-1] + ".dkm") for m in names}
    img = c.image_for(f"{B.RT},core/digikit-menu", "menu")
    # `full = true`: behind the real modules, the full set of the capacity tests (pk_capacity.full_set): every limit used at once and three
    # modules refused, so the audit covers a loader that has just refused modules and an arena with no free byte
    wants: list[int | str] = [P.LOADED] * len(names)
    if t.get("full"):
        g = P.Geometry.of(
            c.device,
            M.sym(img.plan, "core/runtime", "DKRT_ARENA"),
            M.sym(img.plan, "core/dsp-link", "_dl_area"),
        )
        folder = c.env.folder / "digikit" / "modules"
        uses = [P.usage((folder / Path(files[m]).name).read_bytes()) for m in names]
        fill = P.full_set(g, uses)
        for i, mod in enumerate(fill):
            files[mod.name] = c.pack_synth(mod.spec, f"full-{i}.dkm")
            names.append(mod.name)
            wants.append(mod.want)
    card = c.card(list(files.values()))
    loaded_names = [n for n, w in zip(names, wants, strict=True) if w == P.LOADED]
    with c.rig(img, card, "memaudit", cf_only=True) as rig:
        info = rig.boot_info()
        c.check(
            "every module of the set loaded (ColdFire part), the refused ones with their code",
            info["count"] == len(names)
            and info["safe"] == 0
            and all(
                (r in (0, B.E_LOADING)) if w == P.LOADED else r == w
                for r, w in zip(info["results"], wants, strict=True)
            ),
            f"{info['results']} for {wants}",
        )
        up = DBG.uploader(rig)
        m = up.modules()
        mods = m["modules"]
        st = M.sym(rig.plan, "core/runtime", "DKRT_STATE")
        arena = (rig.w(st + 28), rig.w(st + 32))
        c.check(
            "MODULES lists the loaded modules and every block lies inside the arena",
            sorted(x["name"] for x in mods) == sorted(loaded_names)
            and all(
                arena[0] <= x["cf_base"] and x["cf_base"] + x["cf_size"] <= arena[1]
                for x in mods
            ),
            f"arena {arena[0]:#x}..{arena[1]:#x}, {[(x['name'], hex(x['cf_base']), x['cf_size']) for x in mods]}",
        )
        if t.get("full"):
            c.check(
                "the full set uses the arena to its last byte and the DSP area to its last whole granule",
                m["arena_free"] == 0
                and m["arena_largest"] == 0
                and m["dsp_largest"] < P.DSP_GRANULE,
                f"arena free {m['arena_free']}, dsp free {m['dsp_free']} largest {m['dsp_largest']}",
            )
        prot, code = plan_guard(c.image, rig.plan, mods, arena)
        c.report["memaudit"] = {
            "protected": [(n, hex(a), hex(b)) for n, a, b in prot],
            "allowed_code": [(n, hex(a), hex(b)) for n, a, b in code],
            "modules": [
                (x["name"], hex(x["cf_base"]), hex(x["cf_base"] + x["cf_size"]))
                for x in mods
            ],
        }
        before = block_words(rig, mods)
        arm(rig, prot, code)
        drive(c, rig, t.get("machines", []))
        rep = rig.emu.call("guard.report")
        after = block_words(rig, mods)
        hits = [
            {
                "pc": h["pc"],
                "addr": h["addr"],
                "size": h["size"],
                "label": h["label"],
                "icount": h["icount"],
                "writer": who(h["pc"], mods, code),
            }
            for h in rep["hits"]
        ]
        c.report["memaudit"]["hits"] = hits
        c.report["memaudit"]["total"] = rep["total"]
        for x in mods:
            moved = sum(
                a != b for a, b in zip(before[x["name"]], after[x["name"]], strict=True)
            )
            c.report["memaudit"].setdefault("bytes_changed", {})[x["name"]] = moved
            c.report["memaudit"].setdefault("words_changed", {})[x["name"]] = [
                (hex(o), a.hex(), b.hex())
                for o in range(0, x["cf_size"] - 3, 4)
                if (a := before[x["name"]][o : o + 4])
                != (b := after[x["name"]][o : o + 4])
            ][:8]
        for name in t.get("must_run", []):
            c.check(
                f"{name} ran while the guard was armed (its block changed)",
                c.report["memaudit"]["bytes_changed"].get(name, 0) > 0,
                str(c.report["memaudit"]["bytes_changed"]),
            )
        lines = [
            f"pc {h['pc']:#x} ({h['writer']}) addr {h['addr']:#x} size {h['size']} in {h['label']}"
            for h in hits
        ]
        c.check(
            "no store outside the module blocks and the patch code while the UI was driven (guard total 0)",
            rep["total"] == 0,
            "; ".join(lines),
        )
        c.done(rig)
    late = load_pass(c, img, card, prot, code, mods)
    c.report["memaudit"]["load_hits"] = late
    c.check(
        "no store outside the module blocks and the patch code during the load of the modules and their init (second boot, watch from the first cache enable)",
        not late,
        "; ".join(
            f"pc {h['pc']:#x} ({h['writer']}) addr {h['addr']:#x} size {h['size']}"
            for h in late
        ),
    )
    c.common_checks_upload(project_writes_ok=True)
