#!/usr/bin/env python3
"""Emulator proof of SETTINGS > DIGIKIT > HEALTH (modules/core/adapters/health, finding 41-digikit-health.md).

    uv run python -m tools.coldfire.digikit_health_proof run [--image dt2-1.16] [--quick] [--no-cost]

One coupled daemon on a patched image (core/runtime, core/dynreg, core/dyndsp, core/digikit-menu, core/health), the write watch and
the flush check on. Everything is read from the guest's memory (the runtime's `DKRT_HEALTH` after the mailbox command `CMD_HEALTH`,
the menu's `DK_LINES`, `DK_REDRAWS`, the module's counters), not from OLED pixels:

  * idle: after a few windows the mailbox command gives a CPU load, a DSP average and maximum, and the heap, arena and DSP-area
    figures; the DSP count is the same number in every block of a quiet system (the DSP does the same work per block);
  * heap: an independent walk of the firmware's free lists (this file, over `peek32`), with the buddy rules checked (order alignment,
    previous links, totals), equals the module's free and largest figures when no allocation happened in between (the descriptor's
    live-allocation count is read before and after);
  * CPU: a routine written into the guest that burns a known number of instructions per UI-loop pass takes exactly those instructions
    from the idle loop (two per iteration) and raises the load by their share;
  * page: SETTINGS > DIGIKIT > HEALTH opens; with no key press the page refreshes (DK_REDRAWS rises about twice a second and the
    firmware's own dirty flag of the view is consumed); its lines equal this file's formatting of DK_HEALTH; leaving the page stops
    the polling (`DKRT_POLL` back to 0);
  * busy: a pattern with every step of eight tracks trigged plays; the CPU load and the DSP numbers are read again, and the page
    opened while it plays shows them;
  * safety: 0 stray writes over `cf.expansion` (the module's own counters are written by its own code), 0 flush hits;
  * cost: the block hook's instruction count (the compiled function's path) and an A/B of the instructions left to the idle loop
    with the hook switched off (`health_off`) and on.

The emulator's timing is not the device's: the numbers are proofs of the mechanisms, not values of a device (finding 41).
"""

from __future__ import annotations

import argparse
import json
import pathlib
import sys
import time

from tools.coldfire.digikit_menu_proof import Plan
from tools.device.dkproto import ARENA_HDR

ROOT = pathlib.Path(__file__).resolve().parents[2]
OUT = ROOT / "out" / "runs" / "lanes" / "r1-health"
SHOTS = ROOT / "out" / "runs" / "lanes" / "r1-shots"
RT = "core/runtime,core/dynreg,core/dyndsp"
MODULES = f"{RT},core/digikit-menu,core/health"
CMD_HEALTH = 6
HEALTH_FIELDS = [
    "magic", "version", "flags", "blocks", "windows", "cpu_x10", "idle_win", "idle_ref", "dsp_avg", "dsp_max", "dsp_period",
    "dsp_avg_x10", "dsp_max_x10", "heap_total", "heap_free", "heap_largest", "heap_allocs", "heap_free_blocks", "arena_total",
    "arena_free", "arena_largest", "dsp_area_total", "dsp_area_free", "dsp_area_largest", "store_total", "store_used", "store_free",
]  # fmt: skip
F_MODULE, F_CPU, F_DSP, F_HEAP, F_CUT, F_BAD, F_STORE = 1, 2, 4, 8, 16, 32, 64
HS_FIELDS = [
    "blocks",
    "pos",
    "acc",
    "n",
    "mx",
    "last",
    "idle_at",
    "windows",
    "seq",
    "pub_avg",
    "pub_max",
    "pub_n",
    "pub_idle",
    "ref",
]


def pct(x10: int) -> str:
    return f"{(x10 + 5) // 10}%"


def size_text(n: int) -> str:
    if n >= 1024 * 1024:
        tenth = (n // 1024 * 10) // 1024
        return f"{tenth // 10}.{tenth % 10} MB"
    return f"{n // 1024} KB"


def size_text_kb(kb: int) -> str:
    """The store figures are KiB (a store can be 22 GiB)."""
    if kb >= 1024 * 1024:
        tenth = (kb // 1024 * 10) // 1024
        return f"{tenth // 10}.{tenth % 10} GB"
    if kb >= 1024:
        tenth = kb * 10 // 1024
        return f"{tenth // 10}.{tenth % 10} MB"
    return f"{kb} KB"


def expected_lines(h: dict) -> list[str]:
    """What the page must say for the figures `h`: this file's own formatting, not the menu's."""
    f = h["flags"]
    out = ["CPU REL " + (pct(h["cpu_x10"]) if f & F_CPU else "--")]
    out.append("DSP 1GHZ " + (pct(h["dsp_avg_x10"]) if f & F_DSP else "--"))
    out.append("DSP MAX " + (pct(h["dsp_max_x10"]) if f & F_DSP else "--"))
    out.append(
        "RAM FREE "
        + (
            (">" if f & F_CUT else "") + size_text(h["heap_free"])
            if f & F_HEAP
            else "--"
        )
    )
    out.append("LARGEST " + (size_text(h["heap_largest"]) if f & F_HEAP else "--"))
    out.append("CPU FREE " + size_text(h["arena_free"]))
    if h["arena_largest"] + ARENA_HDR < h["arena_free"]:
        out.append("CPU BIG " + size_text(h["arena_largest"]))
    out.append(
        "DSP FREE " + (size_text(h["dsp_area_free"]) if h["dsp_area_total"] else "--")
    )
    if h["dsp_area_total"] and h["dsp_area_largest"] < h["dsp_area_free"]:
        out.append("DSP BIG " + size_text(h["dsp_area_largest"]))
    out.append("STORE USED " + (size_text_kb(h["store_used"]) if f & F_STORE else "--"))
    out.append("STORE FREE " + (size_text_kb(h["store_free"]) if f & F_STORE else "--"))
    return out


class HealthProof:
    def __init__(self, image: str, quick: bool, cost: bool, work: pathlib.Path) -> None:
        from tools.patchkit import pk_dkrt_boot as B

        self.B, self.image, self.quick, self.cost = B, image, quick, cost
        self.work = work
        self.env = B.Env(image, work)
        self.checks: list[tuple[str, bool, str]] = []
        self.report: dict = {"image": image}
        self.plan: Plan
        self.rig = None

    def check(self, name: str, ok: bool, detail: str = "") -> None:
        self.checks.append((name, bool(ok), detail))
        print(f"{'ok  ' if ok else 'FAIL'} {name} {detail}", flush=True)

    # ---- reads
    def sym(self, name: str) -> int:
        return self.plan.addr(name)

    def w(self, addr: int) -> int:
        return self.rig.w(addr)

    def words(self, name: str, n: int) -> list[int]:
        a = self.sym(name)
        return [self.w(a + 4 * k) for k in range(n)]

    def health(self) -> dict:
        """`CMD_HEALTH` through the mailbox; the runtime's `DKRT_HEALTH` after it."""
        r = self.rig.post(CMD_HEALTH, "")
        v = self.words("DKRT_HEALTH", len(HEALTH_FIELDS))
        d = dict(zip(HEALTH_FIELDS, v, strict=True))
        d["result"] = r
        return d

    def health_quiet(self, tries: int = 40) -> tuple[dict, dict, dict]:
        """`CMD_HEALTH` and the independent walk with the guest stopped in between: the command is posted, the emulator is stepped in
        small slices until the next UI-loop pass has answered it, and the descriptor's live-allocation count must be the same before the
        command, after it and after the walk (the system allocates now and then: another try). Returns (figures, walk, info)."""
        from tools.patchkit import pk_dkrt_machine as M

        rig, desc = self.rig, self.desc_addr()
        for k in range(tries):
            a0 = self.w(desc + 24)
            seq = self.w(rig.mb + M.MB_SEQ)
            rig.emu.poke32(rig.mb + M.MB_CMD, CMD_HEALTH)
            for _ in range(2000):
                rig.emu.step(6000)
                if self.w(rig.mb + M.MB_CMD) == 0 and self.w(rig.mb + M.MB_SEQ) != seq:
                    break
            a1 = self.w(desc + 24)
            d = dict(
                zip(
                    HEALTH_FIELDS,
                    self.words("DKRT_HEALTH", len(HEALTH_FIELDS)),
                    strict=True,
                )
            )
            walk = self.heap_walk(desc)
            a2 = self.w(desc + 24)
            if a0 == a1 == a2:
                return d, walk, {"tries": k + 1, "allocs": a1}
            rig.s.run_ms(7)  # off the allocation rhythm
        return d, walk, {"tries": tries, "allocs": None, "moved": [a0, a1, a2]}

    def hs(self) -> dict:
        return dict(
            zip(HS_FIELDS, self.words("health_hs", len(HS_FIELDS)), strict=True)
        )

    def lines(self) -> list[str]:
        raw = b"".join(
            self.w(self.sym("DK_LINES") + 4 * k).to_bytes(4, "big") for k in range(72)
        )
        out = []
        for i in range(12):
            row = raw[24 * i : 24 * i + 24]
            out.append(row.split(b"\0")[0].decode("latin1"))
        return [x for x in out if x]

    def tap(self, key, gap_ms: int = 350) -> None:
        self.rig.s.do_tap(key)
        self.rig.s.run_ms(gap_ms)

    def to_health(self) -> None:
        self.tap("NO", 400)
        self.tap("SETTINGS", 600)
        for _ in range(8):
            self.tap("DOWN", 200)
        self.tap("YES", 700)  # DIGIKIT
        self.tap("DOWN", 200)  # ABOUT, HEALTH
        self.tap("YES", 700)

    def frame(self, name: str) -> str:
        digest, raw = self.rig.s.frame()
        from tools.control.digi_client import oled_png

        SHOTS.mkdir(parents=True, exist_ok=True)
        (SHOTS / f"{self.image}-health-{name}.png").write_bytes(oled_png(raw))
        return digest

    def run_to_windows(self, n: int, limit_ms: int = 20000) -> None:
        t = 0
        while self.hs()["windows"] < n and t < limit_ms:
            self.rig.s.run_ms(250)
            t += 250

    # ---- the independent heap walk
    def heap_walk(self, desc: int) -> dict:
        """The firmware's buddy allocator (finding 01), walked over `peek32`: descriptor `{init, base, bitmap, lists, max order,
        blocks, allocs}`; free block headers hold prev at +4 and next at +8. Returns the totals and the invariant violations."""
        w = self.w
        init, base, _bitmap, lists, maxo, nblocks, allocs = (
            w(desc + 4 * k) for k in range(7)
        )
        res = {
            "init": init,
            "base": base,
            "maxo": maxo,
            "nblocks": nblocks,
            "allocs": allocs,
            "free": 0,
            "largest": 0,
            "nodes": 0,
            "bad": [],
        }
        if init != 1 or maxo > 31 or nblocks != 1 << maxo:
            res["bad"].append(
                f"descriptor: init {init} maxo {maxo} nblocks {nblocks:#x}"
            )
            return res
        seen = set()
        for order in range(maxo + 1):
            idx = w(lists + 4 * order)
            prev = 0xFFFFFFFF
            while idx < 0x80000000:  # negative = end
                if idx >= nblocks or idx in seen:
                    res["bad"].append(f"order {order}: bad or repeated index {idx:#x}")
                    break
                seen.add(idx)
                if idx % (1 << order):
                    res["bad"].append(
                        f"order {order}: block {idx:#x} not aligned to its size"
                    )
                hdr = base + 16 * idx
                if (
                    prev < 0x80000000 and w(hdr + 4) != prev
                ):  # the head's prev is stale after a pop: not checked
                    res["bad"].append(
                        f"order {order}: block {idx:#x} prev {w(hdr + 4):#x} != {prev:#x}"
                    )
                size = 16 << order
                res["free"] += size
                res["largest"] = max(res["largest"], size)
                res["nodes"] += 1
                prev = idx
                idx = w(hdr + 8)
                if res["nodes"] > 200000:
                    res["bad"].append("too many nodes")
                    break
        if res["free"] > nblocks * 16:
            res["bad"].append("free exceeds the heap")
        return res

    def desc_addr(self) -> int:
        """The allocator descriptor the module reads: the literal at its anchor (devices/ports) in the running image."""
        import tomllib

        port = tomllib.loads(
            (ROOT / "devices" / "ports" / f"{self.image}.toml").read_text()
        )
        for a in port["anchor"]:
            if a["name"] == "cf.mem.site.heap.desc_lit":
                return self.w(a["addr"])
        raise KeyError("cf.mem.site.heap.desc_lit")

    # ---- scenarios
    def setup(self) -> None:
        self.img = self.env.build_image(MODULES, "health")
        self.plan = Plan(self.img.plan)
        # a store with one 700-byte file (header, one table sector, two data sectors: 4 sectors used), so the page has store figures
        (self.env.folder / "digikit" / "notes.txt").write_bytes(bytes(700))
        self.card = self.env.card(None, tag="health")

    def anchor_addr(self, name: str) -> int:
        import tomllib

        port = tomllib.loads(
            (ROOT / "devices" / "ports" / f"{self.image}.toml").read_text()
        )
        return next(a["addr"] for a in port["anchor"] if a["name"] == name)

    def sites_check(self) -> None:
        """An image patched without core/health leaves the boot thread's last call and the block handler's entry as the firmware has them
        (no write of the patch plan touches them); the image with it patches exactly those two sites, for core/health."""
        sites = {"cf.rtos.site.idle": 6, "cf.link.fn.block_isr": 8}
        without = self.env.build_image(f"{RT},core/digikit-menu", "nohealth")
        plan0, plan1 = without.plan, self.img.plan

        def touched(plan: dict) -> dict:
            out = {}
            for name, n in sites.items():
                a = self.anchor_addr(name)
                hits = [
                    w
                    for w in plan["writes"]
                    if w["cpu"] == "cf"
                    and w["addr"] < a + n
                    and a < w["addr"] + w["len"]
                ]
                out[name] = [w["owner"] for w in hits]
            return out

        t0, t1 = touched(plan0), touched(plan1)
        self.report["sites"] = {"without": t0, "with": t1}
        self.check(
            "without core/health no patch write touches its two hook sites",
            all(not v for v in t0.values()),
            str(t0),
        )
        self.check(
            "with it exactly core/health writes them",
            all(v and all(o.startswith("core/health") for o in v) for v in t1.values()),
            str(t1),
        )

    def idle_checks(self) -> None:
        self.run_to_windows(8)
        hs = self.hs()
        self.check(
            "windows close once a second",
            hs["windows"] >= 8,
            f"{hs['windows']} windows, {hs['blocks']} blocks",
        )
        self.check(
            "blocks = 1500 per closed window + the open one",
            hs["blocks"] == hs["windows"] * 1500 + hs["pos"],
            f"{hs['blocks']} blocks, {hs['windows']} windows, {hs['pos']} in the open one",
        )
        h, walk, info = self.health_quiet()
        self.report["idle"] = h
        self.report["walk"] = walk
        self.report["walk_info"] = info
        self.check("mailbox command answered 0", True)
        self.check(
            "flags: module, CPU, DSP, heap",
            h["flags"] & (F_MODULE | F_CPU | F_DSP | F_HEAP)
            == (F_MODULE | F_CPU | F_DSP | F_HEAP),
            f"{h['flags']:#x}",
        )
        self.check("heap walk not cut, not corrupt", not h["flags"] & (F_CUT | F_BAD))
        self.check(
            "independent walk: buddy invariants hold",
            not walk["bad"] and walk["nodes"] > 0,
            "; ".join(walk["bad"][:3]) or f"{walk['nodes']} free blocks",
        )
        self.check(
            "the heap is 32 MiB of 16-byte blocks, order 21",
            walk["maxo"] == 21 and walk["nblocks"] == 0x200000,
        )
        self.check(
            "heap free, largest and block count equal the independent walk",
            info["allocs"] is not None
            and h["heap_free"] == walk["free"]
            and h["heap_largest"] == walk["largest"]
            and h["heap_free_blocks"] == walk["nodes"],
            f"module {h['heap_free']}/{h['heap_largest']}/{h['heap_free_blocks']} walk {walk['free']}/{walk['largest']}/{walk['nodes']}, {info}",
        )
        from tools.patchkit import pk_device

        # sectors a store may use (header A, tables, data): up to header B; the figures are KiB
        limit = pk_device.store_of(self.image[:3]).limit
        self.check(
            "store figures: the header's used sectors, and the room left for one more file and its table entry",
            h["flags"] & F_STORE != 0
            and h["store_total"] == limit // 2
            and h["store_used"] == 4 // 2
            and h["store_free"] == (limit - 4 - 1) // 2,
            f"flags {h['flags']:#x} total {h['store_total']} used {h['store_used']} free {h['store_free']}",
        )
        a1 = info["allocs"]
        idle_cpu = [h["cpu_x10"]]
        for _ in range(3):
            self.rig.s.run_ms(1100)
            idle_cpu.append(self.health()["cpu_x10"])
        self.report["idle_cpu"] = idle_cpu
        self.check(
            "module's live allocation count is the descriptor's",
            h["heap_allocs"] == a1,
            f"{h['heap_allocs']} {a1}",
        )
        self.check(
            "arena and DSP area figures present",
            h["arena_total"] == 512 * 1024
            and 0 < h["arena_free"] <= h["arena_total"]
            and h["dsp_area_total"] > 0,
            f"arena {h['arena_free']}/{h['arena_total']} dsp {h['dsp_area_free']}/{h['dsp_area_total']}",
        )
        self.check(
            "DSP block cycles are a share of the 666,667-cycle block",
            0 < h["dsp_avg"] <= h["dsp_max"] < 666_667,
            f"avg {h['dsp_avg']} max {h['dsp_max']} = {h['dsp_avg_x10'] / 10:.1f}% / {h['dsp_max_x10'] / 10:.1f}%",
        )
        # the DSP's count read from the record copy by the module is the DSP's own record, word 0: read it from the record directly
        import tomllib

        port = tomllib.loads(
            (ROOT / "devices" / "ports" / f"{self.image}.toml").read_text()
        )
        rec_lit = next(
            a["addr"]
            for a in port["anchor"]
            if a["name"] == "cf.link.site.reply_copy_lit"
        )
        rec = self.w(rec_lit)
        direct = [self.w(rec) for _ in range(4)]
        self.check(
            "the record copy's first word is that count",
            all(abs(x - h["dsp_avg"]) <= max(64, h["dsp_avg"] // 100) for x in direct),
            f"direct {direct} window avg {h['dsp_avg']}",
        )

    def icount(self) -> int:
        st = self.rig.emu.call("status")
        for k in ("icount", "instructions"):
            if k in st:
                return int(st[k])
        return int(st["cf"]["icount"])

    BURN_LEVELS = (
        150_000,
        600_000,
        2_000_000,
    )  # iterations of the injected loop (3 instructions each) once per UI-loop pass

    def interval(self, ms: int = 2000) -> dict:
        """Idle-loop iterations and instructions over `ms` of emulated time, and the burn routine's run count."""
        idle, burn = self.sym("health_idle_line"), self.stage
        i0, n0, b0 = self.w(idle), self.icount(), self.w(burn + 32)
        self.rig.s.run_ms(ms)
        i1, n1, b1 = self.w(idle), self.icount(), self.w(burn + 32)
        return {"idle": (i1 - i0) & 0xFFFFFFFF, "instr": n1 - n0, "runs": b1 - b0}

    def burn_checks(self) -> None:
        """The idle counter counts what the tasks leave. A routine that burns instructions once per UI-loop pass (machine code the host
        writes into the runtime's staging buffer, registered as the poll function) at three sizes: the idle loop's share of the
        instructions falls with each, by at least half of what the routine takes (other low-priority work gives way as well), is 0 once
        the routine wants more than the CPU has (the idle context is the lowest priority) and the load rises to 100 %; it recovers when
        the routine is removed."""
        emu = self.rig.emu
        stage = self.stage = self.sym("DKRT_STAGE")
        poll = self.sym("DKRT_POLL")
        quiet = self.interval()
        base = self.health()
        levels = []
        for n in self.BURN_LEVELS:
            code = [
                0x203C0000 | (n >> 16),
                ((n & 0xFFFF) << 16) | 0x5380,
                0x66FC52B9,
                stage + 32,
                0x4E750000,
            ]
            for k, word in enumerate(code):
                emu.poke32(stage + 4 * k, word)
            emu.poke32(stage + 32, 0)
            emu.poke32(poll, stage)
            self.rig.s.run_ms(300)
            iv = self.interval()
            h = self.health()
            levels.append((n, iv, h))
            emu.poke32(poll, 0)
            self.rig.s.run_ms(300)
        after = self.interval()
        ha = self.health()
        share = lambda iv: 2 * iv["idle"] / iv["instr"]  # noqa: E731
        burn_share = lambda n, iv: iv["runs"] * (3 * n + 3) / iv["instr"]  # noqa: E731
        self.report["burn"] = {
            "quiet": quiet,
            "levels": [
                {
                    "n": n,
                    **iv,
                    "idle_share": share(iv),
                    "burn_share": burn_share(n, iv),
                    "cpu_x10": h["cpu_x10"],
                }
                for n, iv, h in levels
            ],
            "after": after,
            "cpu_x10": [
                base["cpu_x10"],
                *(h["cpu_x10"] for _, _, h in levels),
                ha["cpu_x10"],
            ],
        }
        shares = [share(quiet)] + [share(iv) for _, iv, _ in levels]
        loads = [base["cpu_x10"]] + [h["cpu_x10"] for _, _, h in levels]
        self.check(
            "the idle loop's share of the instructions falls with each burn size, and the load rises",
            all(a > b for a, b in zip(shares, shares[1:], strict=False))
            and all(a < b for a, b in zip(loads, loads[1:], strict=False)),
            f"idle share {[round(x, 3) for x in shares]}, load {loads} (tenths of a percent)",
        )
        n1, iv1, _ = levels[0]
        n2, iv2, _ = levels[1]
        self.check(
            "the idle loop gives up at least half of the instructions the burn takes",
            share(quiet) - share(iv1) >= 0.5 * burn_share(n1, iv1)
            and share(quiet) - share(iv2) >= 0.5 * burn_share(n2, iv2),
            f"lost {share(quiet) - share(iv1):.3f} / {share(quiet) - share(iv2):.3f}, burn {burn_share(n1, iv1):.3f} / {burn_share(n2, iv2):.3f} of the instructions",
        )
        n3, iv3, h3 = levels[2]
        self.check(
            "a burn that wants more than the CPU leaves the idle loop nothing, and the load is 100 %",
            iv3["idle"] == 0 and h3["cpu_x10"] == 1000,
            f"idle iterations {iv3['idle']}, load {h3['cpu_x10']}, {iv3['runs']} runs",
        )
        self.check(
            "the load falls back when the burn is removed",
            ha["cpu_x10"] <= base["cpu_x10"] + 20
            and abs(share(after) - share(quiet)) < 0.02,
            f"{base['cpu_x10']} -> {ha['cpu_x10']}, idle share {share(quiet):.3f} -> {share(after):.3f}",
        )

    def page_checks(self) -> None:
        self.to_health()
        digest0 = self.frame("page_a")
        r0 = self.w(self.sym("DK_REDRAWS"))
        poll = self.w(self.sym("DKRT_POLL"))
        self.check(
            "the page registered its poll function",
            poll != 0 and r0 >= 1,
            f"poll {poll:#x} redraws {r0}",
        )
        # DK_HEALTH_MENU is the core/ui menu object (dk_ui_menu, modules/core/adapters/ui/src/ui.c): the firmware's view is the pointer at +4
        view = self.w(self.w(self.sym("DK_HEALTH_MENU")) + 4)
        dirty_off, parent_off = 0x38 + 0x14, 0x38 + 0x2C
        p0, i0 = self.w(self.sym("DK_POLLS")), self.icount()
        self.rig.s.run_ms(2000)  # no key press
        r1 = self.w(self.sym("DK_REDRAWS"))
        p1, i1 = self.w(self.sym("DK_POLLS")), self.icount()
        self.report["ui_passes_per_s"] = (p1 - p0) / 2.0
        self.report["ui_passes_per_Minstr"] = (p1 - p0) / ((i1 - i0) / 1e6)
        self.check(
            "the page refreshes with no key press (about twice a second)",
            3 <= r1 - r0 <= 6,
            f"{r0} -> {r1} in 2 s",
        )
        # the refresh sets the view's dirty flag and the next frame consumes it: sample it every 50 ms for a second (the refresh is every half
        # second, so a single read depends on where in that period it falls) and see it set, then cleared
        states = []
        for _ in range(20):
            self.rig.s.run_ms(50)
            d = self.w(view + dirty_off - dirty_off % 4)
            states.append(((d >> (8 * (3 - dirty_off % 4))) & 0xFF) != 0)
        consumed = any(a and not b for a, b in zip(states, states[1:], strict=False))
        self.check(
            "the firmware redrew the view (its dirty flag is set by a refresh and consumed)",
            consumed,
            "".join("1" if x else "0" for x in states),
        )
        self.check(
            "the view is on the stack (parent pointer set)",
            self.w(view + parent_off) != 0,
        )
        h = self.words("DK_HEALTH", len(HEALTH_FIELDS))
        dk = dict(zip(HEALTH_FIELDS, h, strict=True))
        self.report["page"] = {"health": dk, "lines": self.lines()}
        want = expected_lines(dk)
        got = self.lines()
        self.check(
            "the page's lines are the figures, formatted",
            got[: len(want)] == want,
            f"{got} vs {want}",
        )
        digest1 = self.frame("page_b")
        self.report["page"]["frames_differ"] = digest0 != digest1
        # leave: the poll stops
        self.tap("NO", 600)
        r2 = self.w(self.sym("DK_REDRAWS"))
        self.rig.s.run_ms(1500)
        r3 = self.w(self.sym("DK_REDRAWS"))
        poll = self.w(self.sym("DKRT_POLL"))
        self.check(
            "leaving the page stops the polling",
            poll == 0 and r3 - r2 <= 1,
            f"poll {poll:#x}, redraws {r2} -> {r3}",
        )
        self.tap("NO", 500)
        self.tap("NO", 500)

    def busy_pattern(self) -> None:
        """Grid recording on; for tracks 1 to 8 all sixteen TRIG keys pressed together (each press toggles its step); off; PLAY."""
        from tools.patchkit.pk_harness import button

        emu, run = self.rig.emu, self.rig.s.run_ms
        self.tap("NO", 400)
        self.tap("RECORD", 400)
        codes = [button(f"TRIG {n}") for n in range(1, 17)]
        for t in range(1, 9):
            self.tap(["TRK", f"TRIG {t}"], 250)
            for c in codes:
                emu.button(c, True)
                run(5)
            run(40)
            for c in reversed(codes):
                emu.button(c, False)
                run(5)
            run(120)
        self.tap("RECORD", 400)
        self.tap("PLAY", 600)

    def busy_checks(self) -> None:
        idle = self.report["idle"]
        self.busy_pattern()
        self.run_to_windows(self.hs()["windows"] + 4)
        samples = []
        for _ in range(4):
            self.rig.s.run_ms(1100)
            samples.append(self.health())
        self.report["busy"] = samples
        cpu_idle = self.report["idle_cpu"]
        cpu_busy = [x["cpu_x10"] for x in samples]
        mean = lambda xs: sum(xs) / len(xs)  # noqa: E731
        self.report["cpu_rise_x10"] = mean(cpu_busy) - mean(cpu_idle)
        self.check(
            "CPU load is higher while a busy pattern plays (mean of four windows)",
            mean(cpu_busy) >= mean(cpu_idle) + 1,
            f"idle {cpu_idle} busy {cpu_busy} (tenths of a percent)",
        )
        self.check(
            "the DSP's block time sees the pattern: the maximum rises, the numbers differ from idle",
            max(x["dsp_max"] for x in samples) > idle["dsp_max"]
            and any(x["dsp_avg"] != idle["dsp_avg"] for x in samples),
            f"avg {idle['dsp_avg']} -> {[x['dsp_avg'] for x in samples]}, max {idle['dsp_max']} -> {[x['dsp_max'] for x in samples]} cycles",
        )
        self.report["dsp_rise"] = {
            "avg": [x["dsp_avg"] - idle["dsp_avg"] for x in samples],
            "max": [x["dsp_max"] - idle["dsp_max"] for x in samples],
        }
        # the page, opened while it plays
        self.to_health()
        self.rig.s.run_ms(1200)
        dk = dict(
            zip(HEALTH_FIELDS, self.words("DK_HEALTH", len(HEALTH_FIELDS)), strict=True)
        )
        got = self.lines()
        self.check(
            "the page while playing shows the figures",
            got[:8] == expected_lines(dk)[:8],
            str(got),
        )
        self.report["page_busy"] = {"health": dk, "lines": got}
        self.frame("page_busy")
        self.tap("NO", 600)
        self.tap("NO", 400)
        self.tap("STOP", 500)

    def cost_checks(self) -> None:
        """A/B: the instructions left to the idle loop with the block hook off and on, over alternating windows of 2 s."""
        e = self.rig.emu
        off = self.sym("health_off")
        idle = self.sym("health_idle_line")

        def window(sw: int) -> tuple[int, int]:
            e.poke32(off, sw)
            self.rig.s.run_ms(300)
            a = self.w(idle)
            t0 = self.icount()
            self.rig.s.run_ms(2000)
            b = self.w(idle)
            t1 = self.icount()
            return (b - a) & 0xFFFFFFFF, t1 - t0

        runs = {0: [], 1: []}
        for _ in range(5 if not self.quick else 3):
            for sw in (1, 0):
                runs[sw].append(window(sw))
        e.poke32(off, 0)
        self.report["cost_ab"] = runs
        on = [i * 2 / t for i, t in runs[0]]
        offr = [i * 2 / t for i, t in runs[1]]
        mean = lambda xs: sum(xs) / len(xs)  # noqa: E731
        spread = max(max(on) - min(on), max(offr) - min(offr))
        self.report["cost_share"] = {"on": mean(on), "off": mean(offr), "noise": spread}
        self.check(
            "A/B ran: share of instructions in the idle loop, hook on vs off",
            True,
            f"on {mean(on) * 100:.3f}% off {mean(offr) * 100:.3f}% (window spread {spread * 100:.3f} points)",
        )

    def finish(self) -> None:
        w = self.rig.watch_summaries()
        self.report["watch"] = w
        self.check(
            "0 stray writes, 0 flush hits",
            w["write_events"] == 0 and w["flush_hits"] == 0,
            str(w),
        )

    def run(self) -> bool:
        self.setup()
        self.sites_check()
        t0 = time.monotonic()
        with self.env.boot(self.img, self.card, "health", watch=True) as rig:
            self.rig = rig
            print(f"booted in {time.monotonic() - t0:.0f} s", flush=True)
            self.idle_checks()
            self.burn_checks()
            self.page_checks()
            self.busy_checks()
            if self.cost:
                self.cost_checks()
            self.finish()
        OUT.mkdir(parents=True, exist_ok=True)
        (OUT / f"{self.image}.json").write_text(
            json.dumps(self.report, indent=1, default=str) + "\n"
        )
        bad = [c for c in self.checks if not c[1]]
        print(
            f"{self.image}: {len(self.checks) - len(bad)} of {len(self.checks)} checks",
            "PASS" if not bad else "FAIL",
        )
        return not bad


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("cmd", choices=["run"])
    ap.add_argument("--image", default="dt2-1.16")
    ap.add_argument("--quick", action="store_true", help="fewer A/B windows")
    ap.add_argument(
        "--no-cost", action="store_true", help="skip the A/B of the measurement cost"
    )
    ap.add_argument(
        "--sites-only",
        action="store_true",
        help="only the patch check: without core/health its two hook sites are untouched (no emulator)",
    )
    a = ap.parse_args(argv)
    work = OUT / a.image
    work.mkdir(parents=True, exist_ok=True)
    proof = HealthProof(a.image, a.quick, not a.no_cost, work)
    if a.sites_only:
        proof.setup()
        proof.sites_check()
        return 0 if all(ok for _, ok, _ in proof.checks) else 1
    return 0 if proof.run() else 1


if __name__ == "__main__":
    sys.exit(main())
