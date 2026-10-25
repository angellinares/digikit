"""Cycles per DSP block of the SHARC+ code the firmware runs, from the manual's stall rules.

    uv run python -m tools.sharc_dev.sharc_cpi memspec IMAGE            # DAG base registers per PC for the run
    uv run python -m tools.sharc_dev.sharc_cpi run DEVICE IMAGE NAME:SECONDS... [--patch FILE]
    uv run python -m tools.sharc_dev.sharc_cpi report --out out/clock/sharc.json [--md out/clock/SHARC-REPORT.md] \\
        --image dn2=dn2-1.11 --image dt2=dt2-1.16 [--pair BASE=TEST]... FILE.tsv...

Counting: a `coupled-run` built with the engine's SHARC_CYCMIX diagnostic (packages/emulator/sharc/src/cycmix.rs;
interpreter only, so the run uses SHARC_AOT=0 SHARC_FAST_REGIONS= SHARC_BLOCKTRANS=) writes every executed PC with its
count, every executed (previous PC, PC) pair, the address bucket of the data accesses of each PC and, for L2 and
external data, how often the 64-byte line changed. The idle loop is skipped analytically by the engine and not counted.
The decoded instruction (registers read and written, memory access, branch kind) comes from the program database
`out/sharcdb/IMAGE.sqlite` (`tools/sharc_dev/sharcdb`), which is derived from the firmware and stays under out/.

The rules and their page references are in tools/sharc_dev/sharc_timing.py. Every cost is a (low, nominal, high)
triple. Low has no assumed memory latency at all; nominal and high use the assumed L2 and DDR fill times of
sharc_timing.py (the manual gives none).
"""

from __future__ import annotations

import argparse
import json
import os
import pathlib
import re
import sqlite3
import subprocess
import sys
from collections import Counter, defaultdict

from tools import python_env
from tools.sharc_dev import sharc_timing as T
from tools.sharc_dev.sharcimm import name_address

ROOT = pathlib.Path(__file__).resolve().parents[2]
SHARCDB = ROOT / "out" / "sharcdb"
LOOP = re.compile(r"^DO 0x([0-9a-f]+) UNTIL (\w+)")
PM = re.compile(r"\bPM\(")


# ---------------------------------------------------------------------------------------------------------------
# The profile written by the engine


class Profile:
    def __init__(self, path: pathlib.Path, blocks: int | None = None):
        self.path = path
        self.counts: dict[int, int] = {}
        self.pairs: dict[tuple[int, int], int] = {}
        self.mem: dict[int, list[list[int]]] = defaultdict(lambda: [[0] * 8, [0] * 8])
        self.same: dict[int, int] = {}
        self.lines: dict[int, int] = defaultdict(int)  # pc -> line changes (both slots)
        self.far: Counter = Counter()
        self.mmr: Counter = (
            Counter()
        )  # (pc, address) -> MMR accesses, from the engine's A lines
        self.unknown: Counter = Counter()
        self.idle = (0, 0, 0)
        self.cache: dict[tuple[str, int], tuple[int, int]] = {}
        self.btb: dict[int, list[int]] = {}
        for raw in path.read_text().splitlines():
            f = raw.split("\t")
            tag = f[0]
            if tag == "P":
                self.counts[int(f[1], 16)] = int(f[2])
            elif tag == "T":
                self.pairs[(int(f[1], 16), int(f[2], 16))] = int(f[3])
            elif tag == "M":
                self.mem[int(f[1], 16)][int(f[2])] = [int(x) for x in f[3:11]]
            elif tag == "S":
                self.same[int(f[1], 16)] = int(f[2])
            elif tag == "L":
                self.lines[int(f[1], 16)] += int(f[3])
            elif tag == "R":
                self.far[int(f[1], 16)] += int(f[2])
            elif tag == "A":
                self.mmr[(int(f[1], 16), int(f[2], 16))] += int(f[3])
            elif tag == "U":
                self.unknown[int(f[2], 16)] += int(f[3])
            elif tag == "C":
                self.cache[(f[1], int(f[2]))] = (int(f[3]), int(f[4]))
            elif tag == "B":
                self.btb[int(f[1], 16)] = [int(x) for x in f[2:10]]
            elif tag == "I":
                self.idle = (int(f[1], 16), int(f[2], 16), int(f[3], 16))
        self.blocks = blocks

    def is_idle(self, pc: int) -> bool:
        lo, hi, _ = self.idle
        return lo <= pc <= hi


# ---------------------------------------------------------------------------------------------------------------
# The decoded instructions


def load_instructions(image: str, pcs: set[int]) -> dict[int, T.Ins]:
    db = sqlite3.connect(f"file:{SHARCDB / (image + '.sqlite')}?mode=ro", uri=True)
    db.execute("create temp table want(sw integer primary key)")
    db.executemany("insert into want values (?)", [(p,) for p in pcs])
    rows = db.execute(
        "select i.sw, i.width, i.mnemonic, i.cond, i.form from insn i join want w on w.sw = i.sw where i.image = ?",
        (image,),
    ).fetchall()
    defs: dict[int, list[tuple[str, str]]] = defaultdict(list)
    for sw, reg, kind in db.execute(
        "select r.sw, r.reg, r.kind from regdef r join want w on w.sw = r.sw where r.image = ?",
        (image,),
    ):
        defs[sw].append((reg, kind))
    uses: dict[int, set[str]] = defaultdict(set)
    for sw, reg in db.execute(
        "select r.sw, r.reg from reguse r join want w on w.sw = r.sw where r.image = ?",
        (image,),
    ):
        uses[sw].add(reg)
    mem: dict[int, list[tuple[str, str]]] = defaultdict(list)
    for sw, space, base in db.execute(
        "select m.sw, m.space, m.base_reg from mem_access m join want w on w.sw = m.sw where m.image = ?",
        (image,),
    ):
        mem[sw].append((space or "", base or ""))
    edges: dict[int, list[tuple[int, str, int]]] = defaultdict(list)
    for frm, to, kind, delayed in db.execute(
        "select e.from_sw, e.to_sw, e.kind, e.delayed from edges e join want w on w.sw = e.from_sw where e.image = ?",
        (image,),
    ):
        edges[frm].append((to, kind, delayed or 0))
    out: dict[int, T.Ins] = {}
    for sw, width, text, cond, form in rows:
        text = text or ""
        kinds = {k for _, k, _ in edges.get(sw, [])}
        branch = ""
        for k in ("cond_jump", "call", "return", "indirect", "jump"):
            if k in kinds:
                branch = k
                break
        delayed = any(d for _, k, d in edges.get(sw, []) if k != "fallthrough")
        targets = tuple(
            t
            for t, k, _ in edges.get(sw, [])
            if k not in ("fallthrough",) and t is not None
        )
        loop = LOOP.match(text)
        loop_end = 0
        counter = False
        if loop:
            branch = "loop"
            loop_end = int(loop.group(1), 16)
            counter = loop.group(2) == "LCE"
        out[sw] = T.Ins(
            pc=sw,
            nbytes=width or 2,
            text=text,
            always=(cond in (None, 31)) and not text.startswith("IF "),
            defs=tuple(defs.get(sw, [])),
            uses=frozenset(uses.get(sw, set())),
            mem=tuple(mem.get(sw, [])),
            branch=branch,
            delayed=delayed,
            targets=targets,
            loop_end=loop_end,
            loop_counter=counter,
            extra={"form": form or ""},
        )
    db.close()
    return out


def mmr_accesses(
    image: str, pcs: set[int]
) -> tuple[dict[int, bool], dict[int, tuple[bool, int]]]:
    """From sharcdb, for the instructions PCS: whether each one's data access is a store, and the absolute
    MMR accesses (Type 14a operands, which have no DAG register and so are not in the engine's M lines):
    PC -> (store, address)."""
    db = sqlite3.connect(f"file:{SHARCDB / (image + '.sqlite')}?mode=ro", uri=True)
    db.execute("create temp table want(sw integer primary key)")
    db.executemany("insert into want values (?)", [(p,) for p in pcs])
    store: dict[int, bool] = {}
    absolute: dict[int, tuple[bool, int]] = {}
    for sw, direction, address in db.execute(
        "select m.sw, m.direction, m.abs_address from mem_access m join want w on w.sw = m.sw where m.image = ?",
        (image,),
    ):
        is_store = direction == "store"
        store[sw] = store.get(sw, False) or is_store
        if address is not None and T.is_mmr_address(address):
            absolute[sw] = (is_store, address)
    db.close()
    return store, absolute


# ---------------------------------------------------------------------------------------------------------------
# Cost of one profile


def cost_profile(prof: Profile, image: str, blocks: int) -> dict:
    pcs = (
        {p for p in prof.counts if not prof.is_idle(p)}
        | {a for a, _ in prof.pairs}
        | {b for _, b in prof.pairs}
    )
    insns = load_instructions(image, pcs)
    busy_pcs = {p for p in prof.counts if not prof.is_idle(p)}
    n_busy = sum(prof.counts[p] for p in busy_pcs)
    missing = sum(
        prof.counts[p] for p in busy_pcs if p not in insns or not insns[p].text
    )
    res: dict = {
        "blocks": blocks,
        "busy_instructions": n_busy,
        "undecoded_instructions": missing,
    }
    parts: dict[str, T.Cost] = {}

    def acc(name: str, c: T.Cost) -> None:
        parts[name] = T.add(parts.get(name, T.ZERO), c)

    # adjacent-pair rules
    for (a, b), n in prof.pairs.items():
        ia, ib = insns.get(a), insns.get(b)
        if ia is None or ib is None or prof.is_idle(a) or prof.is_idle(b):
            continue
        acc("dag_load_to_use", T.scale(T.dag_use(ia, ib), n))
        acc("float_forward", T.scale(T.float_forward(ia, ib), n))
        acc("float_to_move", T.scale(T.float_to_move(ia, ib), n))
        acc("cjump_frame_use", T.scale(T.cjump_frame_use(ia, ib), n))
        acc("anomaly_20000072", T.scale(T.anomaly_20000072(ia, ib), n))
    # the same DAG and CJUMP/RFRAME stalls at a distance of 2 to MAX_STALL_DISTANCE. The profile holds
    # adjacent pairs only, so the paths are walked back through them: at an instruction with several
    # predecessors (a join, a function entry) the flow splits by the pair counts (a Markov estimate,
    # exact on straight-line code)
    preds: dict[int, list[tuple[int, int]]] = defaultdict(list)
    for (a, b), n in prof.pairs.items():
        if not (prof.is_idle(a) or prof.is_idle(b)):
            preds[b].append((a, n))
    into = {b: sum(n for _, n in v) for b, v in preds.items()}

    def walk(
        user: T.Ins, here: int, flow: float, d: int, between: tuple[T.Ins, ...]
    ) -> None:
        if d > T.MAX_STALL_DISTANCE or flow < 1e-3:
            return
        total = into.get(here, 0)
        for back, n in preds.get(here, ()):
            iw = insns.get(back)
            if iw is None or not total:
                continue
            f = flow * n / total
            acc("dag_load_to_use_far", T.scale(T.dag_use_at(iw, user, d, between), f))
            acc(
                "cjump_frame_use_far",
                T.scale(T.cjump_frame_use_at(iw, user, d, between), f),
            )
            walk(user, back, f, d + 1, between + (iw,))

    for (w1, u), n in prof.pairs.items():
        iu, iw1 = insns.get(u), insns.get(w1)
        if iu is None or iw1 is None or prof.is_idle(u) or prof.is_idle(w1):
            continue
        if not (
            iu.mem
            or iu.branch == "indirect"
            or {"I6", "I7"} & iu.uses
            or any(k == "dag_modify" for _, k in iu.defs)
        ):
            continue  # only an address or I6/I7 use can stall
        walk(iu, w1, float(n), 2, (iw1,))
    # branches against the simulated BTB (engine side: 128 sets x 2 ways, 2-bit counters)
    branch_pcs = [p for p in prof.btb if p in insns]
    res["btb"] = {
        "branches": len(branch_pcs),
        "executions": sum(prof.btb[p][0] for p in branch_pcs),
        "hit_fraction": (
            sum(
                prof.btb[p][2] + prof.btb[p][3] + prof.btb[p][4] + prof.btb[p][5]
                for p in branch_pcs
            )
            / max(1, sum(prof.btb[p][0] for p in branch_pcs))
        ),
        "mispredicted_fraction_of_conditional": 0.0,
    }
    ncond = nmis = 0
    for p in branch_pcs:
        ins = insns[p]
        if ins.is_rti:  # Table 4-40 footnote **: 7 cycles of flush, no BTB
            acc("branch_uncond", (float(T.RTI_FLUSH * prof.btb[p][1]),) * 3)
            continue
        acc(
            "branch_cond"
            if ins.conditional or ins.branch == "cond_jump"
            else "branch_uncond",
            T.branch_cost_sim(ins, prof.btb[p]),
        )
        if ins.conditional or ins.branch == "cond_jump":
            ncond += prof.btb[p][0]
            nmis += prof.btb[p][4] + prof.btb[p][5]
    res["btb"]["mispredicted_fraction_of_conditional"] = nmis / max(1, ncond)
    sorted_pcs = sorted(insns)
    for p in busy_pcs:
        loop_ins = insns.get(p)
        if loop_ins is None or loop_ins.branch != "loop":
            continue
        ins = loop_ins
        end = ins.loop_end
        body = [q for q in sorted_pcs if ins.next_pc <= q <= end]
        tail = body[-11:]
        cof = any(
            insns[q].branch in ("jump", "call", "return", "indirect", "cond_jump")
            or insns[q].is_idle
            for q in tail
        )
        executions = prof.counts[p]
        iters = prof.counts.get(end, 0) / executions if executions else 0
        acc("loop_exit", T.loop_exit_cost(ins, len(body), cof, iters, executions))
    # data bus structure: PM access conflicts (code in L1) and two accesses in one L1 block
    pm_pcs = [
        p
        for p in busy_pcs
        if p in insns
        and any(s == "PM" for s, _ in insns[p].mem)
        and T.region_of_pc(p) == "l1"
    ]
    pm_exec = sum(prof.counts[p] for p in pm_pcs)
    acc("pm_conflict_cache", T.conflict_cache_misses(len(pm_pcs), pm_exec))
    same = sum(v for p, v in prof.same.items() if p in busy_pcs)
    acc("same_block", T.scale(T.SAME_BLOCK, same))
    # instruction fetch from L2 through the I-cache (engine side: 2 ways, 64-byte lines)
    ic = {kb: m for (k, kb), m in prof.cache.items() if k == "i"}
    res["icache"] = {
        "accesses_per_block": {kb: a / blocks for kb, (a, _) in ic.items()},
        "misses_per_block": {kb: m / blocks for kb, (_, m) in ic.items()},
    }
    misses = ic.get(16, (0, 0))[1]
    acc("l2_code_fill", T.scale(T.L2_FILL, misses))
    # data in L2 / external memory through the data cache (engine side: 2 ways, 64-byte lines); system MMRs
    far = {"l2": 0, "ext": 0, "mmr": 0}
    for p, slots in prof.mem.items():
        if p not in busy_pcs:
            continue
        for slot in slots:
            far["l2"] += slot[5]
            far["ext"] += slot[6]
            far["mmr"] += slot[7]
    # two data caches per core, DM ("d") and PM ("p"), 16 KB each assumed
    dc: dict[int, tuple[int, int]] = {}
    for (k, kb), (a, m) in prof.cache.items():
        if k in ("d", "p"):
            da, dm = dc.get(kb, (0, 0))
            dc[kb] = (da + a, dm + m)
    d_acc, d_miss = dc.get(16, (0, 0))
    share_ext = far["ext"] / max(1, far["l2"] + far["ext"])
    far["accesses"] = d_acc
    far["misses_16kb"] = d_miss
    res["dcache"] = {
        "accesses_per_block": {kb: a / blocks for kb, (a, _) in dc.items()},
        "misses_per_block": {kb: m / blocks for kb, (_, m) in dc.items()},
        "misses_16kb_dm": prof.cache.get(("d", 16), (0, 0))[1] / blocks,
        "misses_16kb_pm": prof.cache.get(("p", 16), (0, 0))[1] / blocks,
    }
    # high: each miss may also write a dirty line back (two line transfers)
    acc(
        "l2_data_fill",
        (
            T.L2_FILL[0] * d_miss * (1 - share_ext),
            T.L2_FILL[1] * d_miss * (1 - share_ext),
            2 * T.L2_FILL[2] * d_miss * (1 - share_ext),
        ),
    )
    acc(
        "ext_data_fill",
        (
            T.EXT_FILL[0] * d_miss * share_ext,
            T.EXT_FILL[1] * d_miss * share_ext,
            2 * T.EXT_FILL[2] * d_miss * share_ext,
        ),
    )
    # MMR accesses by peripheral (EE-412 Table 6, Table 4-43). The engine's A lines give each pointer
    # access's address; absolute accesses (Type 14a) are not in the M lines, so they come from sharcdb's
    # operand and the instruction's executions. Accesses neither names keep MMR_ACCESS.
    store, absolute = mmr_accesses(image, busy_pcs)
    by_peripheral: Counter = Counter()
    priced = absolute_n = low = 0
    for (p, address), n in prof.mmr.items():
        if p not in busy_pcs:
            continue
        name = name_address(address)
        if name is None and address < 0x30000:
            # no register there: a pointer the emulator left at 0 (on the DN2, the frame unpack's
            # per-track table read through I0 at sw 0x1c253f); not an MMR access on the part
            low += n
            priced += n
            continue
        cost = T.mmr_cost(name, store.get(p, False))
        if cost is None:
            continue
        acc("mmr_access", T.scale(cost, n))
        priced += n
        by_peripheral[(name or "?").split("_")[0]] += n
    for p, (is_store, address) in absolute.items():
        n = prof.counts.get(p, 0)
        if not n:
            continue
        name = name_address(address)
        acc("mmr_access", T.scale(T.mmr_cost(name, is_store) or T.MMR_ACCESS, n))
        absolute_n += n
        by_peripheral[(name or "?").split("_")[0]] += n
    acc("mmr_access", T.scale(T.MMR_ACCESS, max(0, far["mmr"] - priced)))
    res["mmr"] = {
        "pointer_accesses": far["mmr"],
        "pointer_priced_by_peripheral": priced - low,
        "absolute_accesses": absolute_n,
        "unnamed_low_addresses": low,
        "by_peripheral_per_block": {
            k: v / blocks for k, v in by_peripheral.most_common()
        },
    }
    res["data"] = far
    res["data_by_bucket"] = {
        name: sum(s[i] for slots in prof.mem.values() for s in slots)
        for i, name in enumerate(T.BUCKETS)
    }
    res["code_by_region"] = {
        r: sum(
            c
            for p, c in prof.counts.items()
            if not prof.is_idle(p) and T.region_of_pc(p) == r
        )
        for r in ("l1", "l2", "other")
    }
    res["conditional_branch_executions"] = ncond
    # per block
    per_block = {k: [x / blocks for x in v] for k, v in parts.items()}
    stalls = [
        sum(v[i] for k, v in per_block.items() if k in STALL_RULES) for i in range(3)
    ]
    memory = [
        sum(v[i] for k, v in per_block.items() if k in MEMORY_RULES) for i in range(3)
    ]
    base = n_busy / blocks
    res["per_block"] = per_block
    res["base_cycles_per_block"] = base
    res["stall_cycles_per_block"] = stalls
    res["memory_cycles_per_block"] = memory
    res["cycles_per_block"] = [base + stalls[i] + memory[i] for i in range(3)]
    # sensitivity of the nominal total to the I-cache size (SHL1C_CFG.ICASIZ, not observed) and to the clock
    sens = {}
    for kb, (_, m) in sorted((kb, v) for (k, kb), v in prof.cache.items() if k == "i"):
        sens[f"icache_{kb}KB"] = (
            res["cycles_per_block"][1]
            - per_block["l2_code_fill"][1]
            + m * T.L2_FILL[1] / blocks
        )
    res["nominal_cycles_by_icache"] = sens
    res["load_1ghz"] = [c / T.PERIOD_INSNS for c in res["cycles_per_block"]]
    res["load_800mhz"] = [c / (T.PERIOD_INSNS * 0.8) for c in res["cycles_per_block"]]
    res["emulator_load"] = base / T.PERIOD_INSNS
    return res


STALL_RULES = {
    "dag_load_to_use", "dag_load_to_use_far", "float_forward", "float_to_move", "cjump_frame_use",
    "cjump_frame_use_far", "anomaly_20000072", "branch_cond",
    "branch_uncond", "loop_exit", "pm_conflict_cache", "same_block",
}  # fmt: skip
MEMORY_RULES = {"l2_code_fill", "l2_data_fill", "ext_data_fill", "mmr_access"}


# ---------------------------------------------------------------------------------------------------------------
# Commands


def cmd_memspec(args) -> int:
    """The per-PC file the engine's profile reads: `pc nbytes branch delayed [I register numbers of the data accesses]`."""
    db = sqlite3.connect(f"file:{SHARCDB / (args.image + '.sqlite')}?mode=ro", uri=True)
    pcs = {
        r[0]
        for r in db.execute(
            "select sw from insn where image = ? and mnemonic != ''", (args.image,)
        )
    }
    db.close()
    insns = load_instructions(args.image, pcs)
    out = pathlib.Path(
        args.out or ROOT / "out" / "clock" / "sharc" / f"memspec-{args.image}.txt"
    )
    out.parent.mkdir(parents=True, exist_ok=True)
    lines = []
    for pc in sorted(insns):
        ins = insns[pc]
        branch = 0
        if ins.branch in ("jump", "call", "return", "indirect", "cond_jump"):
            branch = 2 if (ins.conditional or ins.branch == "cond_jump") else 1
        regs = [b[1:] for _, b in ins.mem if re.fullmatch(r"I\d+", b)]
        lines.append(
            f"{pc:x} {ins.nbytes} {branch} {int(ins.delayed)} {' '.join(regs)}".rstrip()
        )
    out.write_text("\n".join(lines) + "\n")
    print(out, len(lines))
    return 0


def cmd_run(args) -> int:
    """Run named scenarios interpreter-only with the profile on (about 50 s of wall per audio second)."""
    out_dir = ROOT / "out" / "clock" / "sharc" / "mix"
    out_dir.mkdir(parents=True, exist_ok=True)
    memspec = ROOT / "out" / "clock" / "sharc" / f"memspec-{args.image}.txt"
    if not memspec.is_file():
        cmd_memspec(argparse.Namespace(image=args.image, out=None))
    status = 0
    for spec in args.scenarios:
        name, _, seconds = spec.partition(":")
        label = f"{args.label or args.device}-{name}"
        env = {
            **python_env(),
            "SHARC_CYCMIX": "1",
            "SHARC_CYCMIX_OUT": str(out_dir / f"{label}.tsv"),
            "SHARC_CYCMIX_MEM": str(memspec),
            "SHARC_FAST_REGIONS": "",
            "SHARC_BLOCKTRANS": "",
            "SHARC_AOT": "0",
        }
        cmd = [
            sys.executable, "-m", "tools.emulator.coupled", "scenario", "--no-build", "--no-cache",
            "--device", args.device, "--out", str(out_dir / f"{label}.json"),
        ]  # fmt: skip
        if seconds:
            cmd += ["--seconds", seconds]
        cmd += [name]
        if args.patch:
            cmd += ["--", "--dsp-patch", args.patch]
        status |= subprocess.run(cmd, cwd=ROOT, env=env, check=False).returncode
    return status


def blocks_of(tsv: pathlib.Path) -> int:
    report = json.loads(tsv.with_suffix(".json").read_text())
    return int(report["scenarios"][0]["frames"])


def cmd_report(args) -> int:
    images = dict(x.split("=", 1) for x in args.image)
    results = {}
    for f in args.files:
        path = pathlib.Path(f)
        device = path.name[:3]  # dn2 or dt2 (the label of `run` starts with the device)
        prof = Profile(path)
        res = cost_profile(prof, images[device], blocks_of(path))
        res["device"] = device
        results[path.stem] = res
    pairs = {}
    for spec in args.pair or []:
        base, test = spec.split("=", 1)
        b, t = results[base], results[test]
        pairs[spec] = {
            "added_instructions_per_block": t["busy_instructions"] / t["blocks"]
            - b["busy_instructions"] / b["blocks"],
            "added_cycles_per_block": [
                t["cycles_per_block"][i] - b["cycles_per_block"][i] for i in range(3)
            ],
            "total_load_1ghz": t["load_1ghz"],
        }
    out = {"results": results, "pairs": pairs}
    pathlib.Path(args.out).parent.mkdir(parents=True, exist_ok=True)
    pathlib.Path(args.out).write_text(json.dumps(out, indent=1, sort_keys=True))
    print(args.out)
    if args.md:
        pathlib.Path(args.md).write_text(markdown(out))
        print(args.md)
    return 0


def pct(v: list[float]) -> str:
    return " / ".join(f"{x * 100:.0f}" for x in v)


def markdown(out: dict) -> str:
    lines = [
        "| run | busy instr/block | emulator load | cycles/block lo / nom / hi | load % at 1 GHz lo / nom / hi | at 800 MHz |",
        "|---|---:|---:|---|---|---|",
    ]
    for name, r in sorted(out["results"].items()):
        b = r["blocks"]
        cyc = " / ".join(f"{x:,.0f}" for x in r["cycles_per_block"])
        lines.append(
            f"| {name} | {r['busy_instructions'] / b:,.0f} | {r['emulator_load'] * 100:.0f} % | {cyc} | "
            f"{pct(r['load_1ghz'])} | {pct(r['load_800mhz'])} |"
        )
    rules = sorted({k for r in out["results"].values() for k in r["per_block"]})
    lines += ["", "Cycles per block by rule (nominal; low / high in the JSON):", ""]
    names = sorted(out["results"])
    lines.append("| rule | " + " | ".join(names) + " |")
    lines.append("|---|" + "---:|" * len(names))
    for rule in rules:
        row = " | ".join(
            f"{out['results'][n]['per_block'].get(rule, [0, 0, 0])[1]:,.0f}"
            for n in names
        )
        lines.append(f"| {rule} | {row} |")
    if out["pairs"]:
        lines += ["", "Added by a patch (paired runs of the same scenario):", ""]
        lines.append(
            "| pair | instructions/block | cycles/block lo / nom / hi | total load 1 GHz % lo / nom / hi |"
        )
        lines.append("|---|---:|---|---|")
        for k, v in out["pairs"].items():
            lines.append(
                f"| {k} | {v['added_instructions_per_block']:.1f} | "
                + " / ".join(f"{x:,.0f}" for x in v["added_cycles_per_block"])
                + f" | {pct(v['total_load_1ghz'])} |"
            )
    return "\n".join(lines) + "\n"


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    sub = ap.add_subparsers(dest="cmd", required=True)
    p = sub.add_parser("memspec")
    p.add_argument("image")
    p.add_argument("--out")
    p.set_defaults(fn=cmd_memspec)
    p = sub.add_parser("run")
    p.add_argument("device", choices=("dn2", "dt2"))
    p.add_argument("image")
    p.add_argument("scenarios", nargs="+")
    p.add_argument("--patch")
    p.add_argument("--label")
    p.set_defaults(fn=cmd_run)
    p = sub.add_parser("report")
    p.add_argument("files", nargs="+")
    p.add_argument("--image", action="append", required=True)
    p.add_argument("--pair", action="append")
    p.add_argument("--out", required=True)
    p.add_argument("--md")
    p.set_defaults(fn=cmd_report)
    args = ap.parse_args(argv)
    os.chdir(ROOT)
    return args.fn(args)


if __name__ == "__main__":
    raise SystemExit(main())
