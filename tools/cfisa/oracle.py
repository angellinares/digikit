#!/usr/bin/env python3
"""Check the generated ColdFire decoder (native/coldfire, from
tools/cfisa/coldfire.json) against independent decoders, and count which
instruction forms the firmware uses.

    uv run python tools/cfisa/oracle.py check [IMAGE ...] [--no-unicorn]
    uv run python tools/cfisa/oracle.py census [IMAGE ...] [--out FILE]

IMAGE is a name under out/sections/ with a Ghidra dump in
out/ghidra/<IMAGE>-emac/ (default: dt2-1.16 dn2-1.11). The instruction set
under test is every instruction in the dump's function listings (disasm/*.s,
tools/ghidradump.py), i.e. the code Ghidra reached; data regions and code
outside functions are not included.

Oracles, per instruction:
  ghidra   length from the listing's bytes.
  sleigh   length and operand text from pypcode with the repo's ColdfireEMAC
           language (tools/ghidra/ColdfireEMAC), normalised to our text form.
           Same SLEIGH spec as the listing, but raw numbers instead of labels.
  unicorn  length from the patched Unicorn (QEMU's m68k decoder, an
           independent implementation): one instruction is executed and the PC
           read back. Only for straight-line instructions; ISA_C BITREV,
           BYTEREV and FF1 are not implemented there (emu/boot.py) and are
           reported as unsupported, not as disagreements.

The manual is the arbiter for every disagreement; known SLEIGH/Unicorn
deviations are listed in KNOWN (with the manual page) and counted separately.
"""

from __future__ import annotations

import argparse
import collections
import glob
import json
import os
import re
import sqlite3
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
CRATE = ROOT / "native" / "coldfire"
CFDIS = CRATE / "target" / "release" / "cfdis"
LANG_DIR = ROOT / "tools" / "ghidra" / "ColdfireEMAC" / "data" / "languages"
DEFAULT_IMAGES = ["dt2-1.16", "dn2-1.11"]

LINE = re.compile(r"^([0-9a-f]{8})  ((?:[0-9a-f]{4} ?)+?)\s{2,}(\S.*)$")


# ---------------------------------------------------------------------------
# inputs


def image_paths(name: str) -> tuple[Path, Path]:
    return (
        ROOT / "out" / "sections" / name / "section_3_MAIN_OS.bin",
        ROOT / "out" / "ghidra" / (name + "-emac"),
    )


def load_base(dump: Path) -> int:
    """The load address of the image: the dump's initialised 'ram' block."""
    con = sqlite3.connect(dump / "xrefs.sqlite")
    try:
        (lo,) = con.execute(
            "SELECT lo FROM blocks WHERE name='ram' AND initialized=1"
        ).fetchone()
    finally:
        con.close()
    return int(lo)


def listing(dump: Path) -> dict[int, tuple[bytes, str]]:
    """{address: (instruction bytes, Ghidra text)} from the dump's disasm/*.s."""
    out: dict[int, tuple[bytes, str]] = {}
    for path in glob.glob(str(dump / "disasm" / "*.s")):
        with open(path) as fh:
            for line in fh:
                m = LINE.match(line)
                if m:
                    out[int(m.group(1), 16)] = (
                        bytes.fromhex(m.group(2).replace(" ", "")),
                        m.group(3),
                    )
    return out


def build_cfdis() -> Path:
    subprocess.run(["cargo", "build", "--release", "--quiet"], cwd=CRATE, check=True)
    return CFDIS


def ours(image: Path, base: int, addrs: list[int]) -> dict[int, tuple[int, str, str]]:
    """{address: (length, form id, text)} from native/coldfire's cfdis; length 0 = illegal."""
    exe = build_cfdis()
    inp = "".join("%08x\n" % a for a in addrs)
    res = subprocess.run(
        [str(exe), str(image), "%x" % base],
        input=inp,
        capture_output=True,
        text=True,
        check=True,
    )
    out = {}
    for line in res.stdout.splitlines():
        a, n, form, text = line.split(" ", 3)
        out[int(a, 16)] = (int(n), form, text)
    return out


def sleigh(
    image_bytes: bytes, base: int, addrs: list[int]
) -> dict[int, tuple[int, str, str]]:
    """{address: (length, mnemonic, body)} from pypcode; length 0 = no decode."""
    import xml.etree.ElementTree as ET

    from pypcode import ArchLanguage, Context

    ldef = ET.parse(LANG_DIR / "coldfire_emac.ldefs").getroot().find("language")
    assert ldef is not None
    ctx = Context(ArchLanguage(str(LANG_DIR), ldef))
    out = {}
    for a in addrs:
        off = a - base
        try:
            ins = ctx.disassemble(
                image_bytes[off : off + 10], a, max_instructions=1
            ).instructions
        except Exception:
            ins = []
        if ins:
            out[a] = (ins[0].length, ins[0].mnem, ins[0].body)
        else:
            out[a] = (0, "", "")
    return out


def unicorn_lengths(
    image_bytes: bytes, base: int, addrs: list[int]
) -> dict[int, int | str]:
    """{address: length or reason} by executing one instruction in Unicorn."""
    from unicorn import (
        UC_ARCH_M68K,
        UC_HOOK_INTR,
        UC_HOOK_MEM_UNMAPPED,
        UC_HOOK_MEM_WRITE,
        UC_MODE_BIG_ENDIAN,
        Uc,
        UcError,
    )
    from unicorn import m68k_const as K

    uc = Uc(UC_ARCH_M68K, UC_MODE_BIG_ENDIAN)
    uc.ctl_set_cpu_model(K.UC_CPU_M68K_CFV4E)
    lo, hi = base & ~0xFFF, (base + len(image_bytes) + 0xFFF) & ~0xFFF
    uc.mem_map(lo, hi - lo)
    uc.mem_write(base, image_bytes)
    fill = b"\x11" * 0x1000

    def unmapped(uc, access, addr, size, value, ud):
        page = addr & ~0xFFF
        uc.mem_map(page, 0x1000)
        uc.mem_write(page, fill)
        return True

    dirty: list[tuple[int, int]] = []
    state = {"intr": False}

    def wrote(uc, access, addr, size, value, ud):
        dirty.append((addr, size))

    def intr(uc, intno, ud):
        state["intr"] = True
        uc.emu_stop()

    uc.hook_add(UC_HOOK_MEM_UNMAPPED, unmapped)
    uc.hook_add(UC_HOOK_MEM_WRITE, wrote, begin=base, end=base + len(image_bytes) - 1)
    uc.hook_add(UC_HOOK_INTR, intr)
    dregs = [getattr(K, "UC_M68K_REG_D%d" % i) for i in range(8)]
    aregs = [getattr(K, "UC_M68K_REG_A%d" % i) for i in range(8)]
    out: dict[int, int | str] = {}
    for a in addrs:
        for i, r in enumerate(dregs):
            uc.reg_write(r, 0x11111111 + i)
        for i, r in enumerate(aregs):
            uc.reg_write(r, 0x20000000 + 0x10000 * i)
        uc.reg_write(K.UC_M68K_REG_SR, 0x2700)
        state["intr"] = False
        try:
            uc.emu_start(a, 0xFFFFFFFF, count=1)
            pc = uc.reg_read(K.UC_M68K_REG_PC)
            out[a] = "exception" if state["intr"] else pc - a
        except UcError as e:
            out[a] = "error: %s" % e
        for w_addr, size in dirty:
            off = w_addr - base
            uc.mem_write(w_addr, image_bytes[off : off + size])
        dirty.clear()
    return out


# ---------------------------------------------------------------------------
# SLEIGH text -> our text form


ALIASES = {"trapf": "tpf"}
CACHE = {
    "data": "dc",
    "inst": "ic",
    "insn": "ic",
    "both": "bc",
    "bc": "bc",
    "dc": "dc",
    "ic": "ic",
}


def _num(tok: str) -> int:
    return int(tok, 16) if tok.lower().lstrip("-").startswith("0x") else int(tok)


def norm_operands(body: str, mac: bool = False) -> str:
    s = body.strip().lower()
    s = re.sub(r"\{([^}]*)\}", lambda m: "{" + "/".join(m.group(1).split()) + "}", s)
    s = s.replace(" ", "").replace("#", "")
    s = re.sub(r"\bsp\b", "a7", s)
    if mac:
        # SLEIGH writes the MAC word halves as Dnu (upper) and Dnw (lower)
        s = re.sub(r"\b([da][0-7])u\b", r"\1.u", s)
        s = re.sub(r"\b([da][0-7])w\b", r"\1.l", s)
    s = re.sub(r"\b([da][0-7])[bw]\b", r"\1", s)
    return s


CTRL_NAMES: dict[str, str] = {}


def load_ctrl_names() -> None:
    table = json.loads((ROOT / "tools" / "cfisa" / "coldfire.json").read_text())
    for k, v in table["control_registers"].items():
        if not k.startswith("_"):
            CTRL_NAMES[k.lower()] = v


def norm_ctrl(name: str) -> str:
    m = re.match(r"^unk_ctl_(0x[0-9a-f]+)$", name)
    if m:
        return CTRL_NAMES.get("0x%03x" % int(m.group(1), 16), m.group(1))
    return name


def norm_ours(text: str) -> tuple[str, str]:
    mn, _, ops = text.partition(" ")
    ops = re.sub(
        r"\{([^}]*)\}", lambda m: "{" + "/".join(m.group(1).split()) + "}", ops
    )
    return mn, ops.replace(" ", "").replace("#", "")


def norm_sleigh(mnem: str, body: str, form: str, addr: int) -> tuple[str, str]:
    """Rewrite a SLEIGH mnemonic and body into our text form, where the two
    differ only in spelling. `form` (our decode) selects the rewrite for
    SLEIGH spellings that cover several forms (DIVSL covers DIVS and REMS)."""
    mn = mnem.lower().rstrip(":")
    mn = ALIASES.get(mn, mn)
    base, dot, size = mn.partition(".")
    ops = norm_operands(body, mac=base in ("mac", "msac"))
    if base in ("jsr", "jmp"):
        # SLEIGH writes absolute targets bare and (d16,PC) as the target
        m = re.match(r"^(-?0x[0-9a-f]+)\.([wl])$", ops)
        if m:
            ops = "(%s).%s" % m.groups()
        elif re.match(r"^-?0x[0-9a-f]+$", ops) and form in ("jsr", "jmp"):
            d = (int(ops, 16) - (addr + 2)) & 0xFFFFFFFF
            d = d - (1 << 32) if d & 0x80000000 else d
            ops = "(%s,pc)" % ("-0x%x" % -d if d < 0 else "0x%x" % d)
    if base == "movec":
        r, _, c = ops.partition(",")
        ops = r + "," + norm_ctrl(c)
    if base in ("divsl", "divul"):
        m = re.match(r"^(.*),(d[0-7]):(d[0-7])$", ops)
        if m:
            src, dr, dq = m.groups()
            kind = "div" if dr == dq else "rem"
            base = kind + base[3]
            ops = "%s,%s" % (src, dq) if kind == "div" else "%s,%s,%s" % (src, dr, dq)
    if base == "cpushl":
        c, _, rest = ops.partition(",")
        ops = CACHE.get(c, c) + "," + (rest if rest.endswith(")") else rest + ")")
    return base + dot + size, ops


def compare_text(
    ours_text: str, form: str, s_mnem: str, s_body: str, addr: int
) -> str | None:
    """None if equal, else a short reason."""
    mn, ops = norm_ours(ours_text)
    smn, sops = norm_sleigh(s_mnem, s_body, form, addr)
    if mn.partition(".")[0] != smn.partition(".")[0]:
        return "mnemonic"
    if smn.partition(".")[2] and mn.partition(".")[2] != smn.partition(".")[2]:
        return "size"
    if ops != sops:
        return "operands"
    return None


# ---------------------------------------------------------------------------
# commands


def instruction_stream(image: Path, base: int, lst: dict) -> tuple[dict, list[int]]:
    """Our decode of the listing's instructions, re-synchronised where our
    decode of one instruction covers the start of the next listed one (the
    listing is then misaligned: Ghidra decoded that instruction shorter).
    -> ({address: (length, form, text)} for the stream, [overlapped listing addresses])."""
    addrs = sorted(lst)
    d = ours(image, base, addrs)
    stream: dict[int, tuple[int, str, str]] = {}
    overlapped: list[int] = []
    pos = None  # end of our previous instruction, while re-synchronising
    for a in addrs:
        if pos is not None and a < pos:
            overlapped.append(a)
            continue
        while pos is not None and pos < a:
            n, form, text = ours(image, base, [pos])[pos]
            stream[pos] = (n, form, text)
            if n == 0:
                pos = None
                break
            pos += n
        if pos is not None and pos > a:
            overlapped.append(a)
            continue
        pos = None
        n, form, text = d[a]
        stream[a] = d[a]
        if n and a + n > a + len(lst[a][0]):
            pos = a + n
    return stream, overlapped


# Disagreements where the manual shows the oracle is wrong. Each is counted
# and reported, but does not fail the check.
def known_deviation(form: str, text: str, n: int, slen: int) -> str | None:
    if (
        form in ("mac_load", "msac_load")
        and re.search(r"\(-?0x[0-9a-f]+,a[0-7]\)", text)
        and slen == n - 2
    ):
        # CFPRM p.172/p.191: the MAC extension word follows the opword and the
        # (d16,Ay) displacement follows it; the SLEIGH spec reads the extension
        # word as the displacement and ends the instruction 2 bytes early.
        return "sleigh_mac_load_d16"
    return None


def check_image(name: str, use_unicorn: bool, show: int) -> dict:
    image, dump = image_paths(name)
    base = load_base(dump)
    img = image.read_bytes()
    lst = listing(dump)
    stream, overlapped = instruction_stream(image, base, lst)
    addrs = sorted(stream)
    sl = sleigh(img, base, addrs)
    uni = (
        unicorn_lengths(
            img, base, [a for a in addrs if stream[a][0] and _straight(stream[a][1])]
        )
        if use_unicorn
        else {}
    )
    stats: collections.Counter = collections.Counter()
    bad: dict[str, list] = collections.defaultdict(list)
    known: dict[str, list] = collections.defaultdict(list)
    for a in overlapped:
        known["ghidra_listing_misaligned"].append((a, lst[a][0].hex(), lst[a][1]))
    stats["listing_instructions"] = len(lst)
    for a in addrs:
        n, form, text = stream[a]
        stats["total"] += 1
        stats["in_listing" if a in lst else "resynced"] += 1
        ok = True
        if n == 0:
            bad["illegal"].append(
                (a, lst[a][0].hex() if a in lst else "", lst[a][1] if a in lst else "")
            )
            continue
        if a in lst:
            gbytes, gtext = lst[a]
            dev = known_deviation(form, text, n, len(gbytes))
            if n == len(gbytes):
                stats["len_ghidra_ok"] += 1
            elif dev:
                known[dev + "_listing"].append((a, gbytes.hex(), n, text, gtext))
            else:
                bad["len_ghidra"].append((a, gbytes.hex(), n, text, gtext))
                ok = False
        slen, smn, sbody = sl[a]
        if slen == n:
            stats["len_sleigh_ok"] += 1
            why = compare_text(text, form, smn, sbody, a)
            if why:
                bad["text_" + why].append((a, form, text, smn + " " + sbody))
                ok = False
            else:
                stats["text_ok"] += 1
        elif known_deviation(form, text, n, slen):
            known[str(known_deviation(form, text, n, slen))].append(
                (a, n, text, smn + " " + sbody)
            )
        else:
            bad["len_sleigh"].append((a, n, text, smn + " " + sbody))
            ok = False
        if a in uni:
            u = uni[a]
            if isinstance(u, int) and u == n:
                stats["len_unicorn_ok"] += 1
            elif isinstance(u, int):
                bad["len_unicorn"].append((a, n, text, u))
                ok = False
            else:
                stats["unicorn_" + u.split(":")[0]] += 1
        if ok:
            stats["agree"] += 1
    stats["unicorn_checked"] = len(uni)
    return {
        "name": name,
        "base": base,
        "stats": dict(stats),
        "bad": {k: v[:show] for k, v in bad.items()},
        "bad_counts": {k: len(v) for k, v in bad.items()},
        "known": {k: v[:show] for k, v in known.items()},
        "known_counts": {k: len(v) for k, v in known.items()},
    }


# Forms the Unicorn oracle cannot run: MOVEC to control registers QEMU does
# not model aborts the process (RGPIOBAR 0x009 in the images); ISA_C BITREV,
# BYTEREV and FF1 are not implemented in QEMU's ColdFire V4e model.
UNICORN_UNSUPPORTED = {"movec", "bitrev", "byterev", "ff1"}


def _straight(form: str) -> bool:
    return FLOW.get(form, "") == "" and form not in UNICORN_UNSUPPORTED


FLOW: dict[str, str] = {}
UNIT: dict[str, str] = {}


def load_flows() -> None:
    table = json.loads((ROOT / "tools" / "cfisa" / "coldfire.json").read_text())
    for f in table["forms"]:
        FLOW[f["id"]] = f.get("flow", "")
        UNIT[f["id"]] = f["unit"]


def _cell(x) -> str:
    return "%08x" % x if isinstance(x, int) and x >= 0x10000 else str(x)


def cmd_check(args) -> int:
    load_flows()
    load_ctrl_names()
    rc = 0
    reports = []
    for name in args.images:
        r = check_image(name, not args.no_unicorn, args.show)
        reports.append(r)
        s = r["stats"]
        print(
            "== %s (base 0x%x): %d instructions (%d listed by Ghidra, %d re-synchronised), %d agree (%.4f%%)"
            % (
                name,
                r["base"],
                s["total"],
                s.get("in_listing", 0),
                s.get("resynced", 0),
                s.get("agree", 0),
                100.0 * s.get("agree", 0) / max(1, s["total"]),
            )
        )
        for k in (
            "len_ghidra_ok",
            "len_sleigh_ok",
            "text_ok",
            "unicorn_checked",
            "len_unicorn_ok",
        ):
            print("   %-24s %d" % (k, s.get(k, 0)))
        for k, v in sorted(s.items()):
            if k.startswith("unicorn_") and k not in ("unicorn_checked",):
                print("   %-24s %d" % (k, v))
        for k, n in sorted(r["known_counts"].items()):
            print("   KNOWN %-24s %d" % (k, n))
            for row in r["known"][k]:
                print("      ", " | ".join(_cell(x) for x in row))
        for k, n in sorted(r["bad_counts"].items()):
            print("   DISAGREE %-20s %d" % (k, n))
            for row in r["bad"][k]:
                print("      ", " | ".join(_cell(x) for x in row))
            rc = 1
    if args.json:
        Path(args.json).write_text(json.dumps(reports, indent=1))
    return rc


def cmd_census(args) -> int:
    load_flows()
    per: dict[str, collections.Counter] = {}
    modes: dict[str, collections.Counter] = {}
    details: dict[str, collections.Counter] = {}
    for name in args.images:
        image, dump = image_paths(name)
        base = load_base(dump)
        lst = listing(dump)
        d, overlapped = instruction_stream(image, base, lst)
        c: collections.Counter = collections.Counter()
        mc: collections.Counter = collections.Counter()
        dc: collections.Counter = collections.Counter()
        for n, form, text in d.values():
            if n == 0:
                c["<illegal>"] += 1
                continue
            mn, _, ops = text.partition(" ")
            c[form + " " + mn] += 1
            mc[form + " " + ea_shape(ops)] += 1
            if form in ("movec",):
                dc["movec " + ops.split(",")[1]] += 1
            if UNIT.get(form) == "emac":
                dc["emac " + mn + " " + ea_shape(ops)] += 1
            if form == "move_to_macsr" and ops.startswith("#"):
                dc["emac macsr value " + ops.split(",")[0]] += 1
            if form in (
                "cpushl",
                "intouch",
                "halt",
                "stop",
                "rte",
                "move_to_sr",
                "move_from_sr",
                "move_to_usp",
                "move_from_usp",
                "trap",
                "illegal",
                "wdebug",
                "wddata",
                "pulse",
                "tas",
                "sats",
            ):
                dc["sys " + text] += 1
        c["<instructions>"] = sum(1 for v in d.values() if v[0])
        c["<ghidra listing misaligned>"] = len(overlapped)
        c["<code bytes>"] = sum(v[0] for v in d.values())
        c["<image bytes>"] = image.stat().st_size
        per[name] = c
        modes[name] = mc
        details[name] = dc
    lines = ["# ColdFire instruction census", ""]
    lines.append(
        "Source: Ghidra function listings (out/ghidra/<image>-emac/disasm), decoded by native/coldfire."
    )
    lines.append("")
    names = args.images
    keys = sorted(set().union(*[set(c) for c in per.values()]))
    lines.append("| form mnemonic | " + " | ".join(names) + " |")
    lines.append("|---|" + "---|" * len(names))
    for k in keys:
        lines.append(
            "| %s | %s |" % (k, " | ".join(str(per[n].get(k, 0)) for n in names))
        )
    lines.append("")
    lines.append("## Differences (forms present in one image only)")
    lines.append("")
    for k in keys:
        vals = [per[n].get(k, 0) for n in names]
        if k.startswith("<"):
            continue
        if min(vals) == 0 and max(vals) > 0:
            lines.append(
                "- %s: %s"
                % (k, ", ".join("%s=%d" % (n, per[n].get(k, 0)) for n in names))
            )
    lines.append("")
    lines.append("## Form x operand shape")
    lines.append("")
    mkeys = sorted(set().union(*[set(c) for c in modes.values()]))
    lines.append("| form shape | " + " | ".join(names) + " |")
    lines.append("|---|" + "---|" * len(names))
    for k in mkeys:
        lines.append(
            "| %s | %s |" % (k, " | ".join(str(modes[n].get(k, 0)) for n in names))
        )
    lines.append("")
    lines.append("## Supervisor, cache and EMAC detail")
    lines.append("")
    dkeys = sorted(set().union(*[set(c) for c in details.values()]))
    lines.append("| item | " + " | ".join(names) + " |")
    lines.append("|---|" + "---|" * len(names))
    for k in dkeys:
        lines.append(
            "| %s | %s |"
            % (
                k.replace("|", "\\|"),
                " | ".join(str(details[n].get(k, 0)) for n in names),
            )
        )
    text = "\n".join(lines) + "\n"
    if args.out:
        Path(args.out).parent.mkdir(parents=True, exist_ok=True)
        Path(args.out).write_text(text)
        print("wrote", args.out)
    else:
        print(text)
    return 0


# ---------------------------------------------------------------------------
# sweep: every opword, not just the firmware's


SWEEP_EXTS = [(0x0000, 0x0000), (0x0800, 0x0800), (0x1804, 0x2000)]
# Unicorn aborts the process on these (QEMU cpu_abort: MOVEC to an unmodelled
# control register, WDEBUG, FBcc with predicate bit 5 set) or stops the CPU
# (HALT, STOP).
SWEEP_UNICORN_SKIP = (
    {0x4E7B, 0x4AC8, 0x4E72} | set(range(0xFBC0, 0xFC00)) | set(range(0xF2A0, 0xF2C0))
)
# QEMU exception numbers that mean "not an instruction": illegal, line A, line F.
QEMU_ILLEGAL = {4, 10, 11}


def ours_words(rows: list[tuple[int, int, int, int]]) -> list[tuple[int, str, str]]:
    exe = build_cfdis()
    inp = "".join("%08x %04x %04x %04x\n" % r for r in rows)
    res = subprocess.run(
        [str(exe), "--words"], input=inp, capture_output=True, text=True, check=True
    )
    out = []
    for line in res.stdout.splitlines():
        _, n, form, text = line.split(" ", 3)
        out.append((int(n), form, text))
    return out


def unicorn_sweep(codes: list[bytes], base: int) -> list[str | int]:
    """Execute each 6-byte code at its own address; -> PC delta, or 'illegal'
    (QEMU raised illegal/line-A/line-F), or 'exc<N>' for another exception."""
    from unicorn import (
        UC_ARCH_M68K,
        UC_HOOK_INTR,
        UC_HOOK_MEM_UNMAPPED,
        UC_MODE_BIG_ENDIAN,
        Uc,
        UcError,
    )
    from unicorn import m68k_const as K

    uc = Uc(UC_ARCH_M68K, UC_MODE_BIG_ENDIAN)
    uc.ctl_set_cpu_model(K.UC_CPU_M68K_CFV4E)
    size = (len(codes) * 8 + 0xFFFF) & ~0xFFFF
    uc.mem_map(base, size)
    uc.mem_write(base, b"".join(c.ljust(8, b"\0") for c in codes))
    fill = b"\x11" * 0x1000

    def unmapped(uc, access, addr, sz, value, ud):
        page = addr & ~0xFFF
        uc.mem_map(page, 0x1000)
        uc.mem_write(page, fill)
        return True

    state = {"intr": -1}

    def intr(uc, intno, ud):
        state["intr"] = intno
        uc.emu_stop()

    uc.hook_add(UC_HOOK_MEM_UNMAPPED, unmapped)
    uc.hook_add(UC_HOOK_INTR, intr)
    dregs = [getattr(K, "UC_M68K_REG_D%d" % i) for i in range(8)]
    aregs = [getattr(K, "UC_M68K_REG_A%d" % i) for i in range(8)]
    out: list[str | int] = []
    for i in range(len(codes)):
        a = base + 8 * i
        for j, r in enumerate(dregs):
            uc.reg_write(r, 0x11111111 + j)
        for j, r in enumerate(aregs):
            uc.reg_write(r, 0x20000000 + 0x10000 * j)
        uc.reg_write(K.UC_M68K_REG_SR, 0x2700)
        state["intr"] = -1
        try:
            uc.emu_start(a, 0xFFFFFFFF, count=1)
        except UcError as e:
            out.append("error %s" % e)
            continue
        if state["intr"] in QEMU_ILLEGAL:
            out.append("illegal")
        elif state["intr"] >= 0:
            out.append("exc%d" % state["intr"])
        else:
            out.append(uc.reg_read(K.UC_M68K_REG_PC) - a)
    return out


def cmd_sweep(args) -> int:
    """Compare legality and length of every opword (with a few extension-word
    patterns) between our decoder, SLEIGH and Unicorn, grouped for review."""
    import xml.etree.ElementTree as ET

    from pypcode import ArchLanguage, Context

    load_flows()
    ldef = ET.parse(LANG_DIR / "coldfire_emac.ldefs").getroot().find("language")
    assert ldef is not None
    ctx = Context(ArchLanguage(str(LANG_DIR), ldef))
    base = 0x10000000
    groups: dict[str, collections.Counter] = collections.defaultdict(
        collections.Counter
    )
    examples: dict[tuple[str, str], str] = {}
    totals: collections.Counter = collections.Counter()
    for vi, (e1, e2) in enumerate(SWEEP_EXTS):
        rows = [(base + 8 * w0, w0, e1, e2) for w0 in range(0x10000)]
        od = ours_words(rows)
        run_uni = vi == 0 and not args.no_unicorn
        codes = [
            bytes([w0 >> 8, w0 & 0xFF, e1 >> 8, e1 & 0xFF, e2 >> 8, e2 & 0xFF])
            for w0 in range(0x10000)
        ]
        if run_uni:
            ucodes = [
                c if (i not in SWEEP_UNICORN_SKIP) else b"\x4e\x71"
                for i, c in enumerate(codes)
            ]
            uni = unicorn_sweep(ucodes, base)
        for w0 in range(0x10000):
            n, form, text = od[w0]
            code = codes[w0] + b"\0\0\0\0"
            try:
                ins = ctx.disassemble(
                    code, base + 8 * w0, max_instructions=1
                ).instructions
            except Exception:
                ins = []
            slen = ins[0].length if ins else 0
            sdesc = (ins[0].mnem + " " + ins[0].body) if ins else "-"
            totals["cases"] += 1
            totals["ours_legal"] += n > 0
            totals["sleigh_legal"] += slen > 0
            example = "%04x %04x %04x: ours=%s | sleigh=%s" % (w0, e1, e2, text, sdesc)
            found: list[tuple[str, str]] = []
            if n and slen and n != slen:
                found.append(("len ours!=sleigh", form))
            elif n and not slen:
                found.append(("ours legal, sleigh illegal", form))
            elif not n and slen:
                found.append(("ours illegal, sleigh legal", sdesc.split()[0]))
            if run_uni and w0 not in SWEEP_UNICORN_SKIP:
                u = uni[w0]
                totals["unicorn_run"] += 1
                if n and u == "illegal":
                    found.append(("ours legal, unicorn illegal", form))
                elif not n and u != "illegal":
                    key = sdesc.split()[0] if slen else "%04x" % (w0 & 0xFFC0)
                    found.append(("ours illegal, unicorn legal", key))
                elif n and isinstance(u, int) and FLOW.get(form, "") == "" and u != n:
                    found.append(("len ours!=unicorn", form))
            for kind, key in found:
                groups[kind][key] += 1
                examples.setdefault((kind, key), example)
    print("totals:", dict(totals))
    for kind in sorted(groups):
        c = groups[kind]
        print("== %s: %d cases, %d groups" % (kind, sum(c.values()), len(c)))
        for key, cnt in c.most_common(args.show):
            print("   %6d %-22s e.g. %s" % (cnt, key, examples[(kind, key)]))
    return 0


def ea_shape(ops: str) -> str:
    """Operand text with register numbers and values abstracted."""
    s = re.sub(r"\{[^}]*\}", "{list}", ops)
    s = re.sub(r"-?0x[0-9a-f]+", "N", s)
    s = re.sub(r"\bd[0-7]\b", "Dn", s)
    s = re.sub(r"\ba[0-7]\b", "An", s)
    s = re.sub(r"\bacc[0-3]\b", "ACCn", s)
    return s


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    sub = ap.add_subparsers(dest="cmd", required=True)
    c = sub.add_parser("check", help="compare the decoder with the oracles")
    c.add_argument("images", nargs="*", default=DEFAULT_IMAGES)
    c.add_argument("--no-unicorn", action="store_true")
    c.add_argument("--show", type=int, default=12, help="disagreements shown per kind")
    c.add_argument("--json", help="write the full report as JSON")
    c.set_defaults(fn=cmd_check)
    s = sub.add_parser("census", help="count instruction forms per image")
    s.add_argument("images", nargs="*", default=DEFAULT_IMAGES)
    s.add_argument("--out", help="write the census as Markdown")
    s.set_defaults(fn=cmd_census)
    w = sub.add_parser(
        "sweep", help="every opword: legality and length against SLEIGH and Unicorn"
    )
    w.add_argument("--no-unicorn", action="store_true")
    w.add_argument("--show", type=int, default=40, help="groups shown per kind")
    w.set_defaults(fn=cmd_sweep, images=None)
    args = ap.parse_args(argv)
    if args.images is not None and not args.images:
        args.images = DEFAULT_IMAGES
    return args.fn(args)


if __name__ == "__main__":
    os.chdir(ROOT)
    sys.exit(main())
