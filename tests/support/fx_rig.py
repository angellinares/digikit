"""A rig for the FX adapter core/fx: its chain on the concrete SHARC+ core, one block per call, as the hook makes it.

    rig = FxRig([Row("_te_half", "_te_state_a")])      # rows of the chain: process symbol, state symbol, enabled, parameter words
    b = rig.block(master)                                # master: 64 floats (32 left, 32 right) before the hook
    b.master                                             # the 64 floats after the hook; b.instructions, b.returned, b.ctx_bad, b.regs_bad

The adapter (modules/core/adapters/fx/src/dsp/fx.s) and the effect sources are assembled and linked the way the patch does: the table at TABLE, the master
block at MASTER, the hook's tail (the stock function's second instruction) at STOCK_BP. Data symbols (`.PKDATA`, `.PKBSS`, `.VAR`) go to free L2;
`_dk_step` (the `DK_STEP` word) is one zeroed word. `.PKBSS` memory (the effects' states) starts poisoned, so an effect that reads a state the adapter did not zero shows. Every call draws a hostile
context (M5 ... MODE1) and fills the registers the stock function expects back with markers; both must come back unchanged.
Plain Python (no numpy, no pytest): it also runs under PyPy."""

from __future__ import annotations

import random
import struct
from dataclasses import dataclass, field
from pathlib import Path

import support.dsp_voice_rig as dv
from tools.patchkit import pk_sharc

ROOT = dv.ROOT
ADAPTER = ROOT / "modules/core/adapters/fx/src/dsp/fx.s"
SEND = (
    ROOT / "modules/core/adapters/fx/src/dsp/fxsend.s"
)  # the send route's hook (Digitakt II)
SEND_DN2 = (
    ROOT / "modules/core/adapters/fx/src/dsp/fxsend-dn2.s"
)  # the same with the slot mix (Digitone II)
EFFECTS = ROOT / "tests/support/fx_effects.s"
CODE_BASE = 0xB90000
DATA_BASE = 0x200A0000
DATA_END = 0x20100000
MASTER = 0x2009E000  # byte address of the 64 master floats
RETURN = 0x2009E400  # the FX return bus: 64 floats (the send route adds to it)
TRACKBUS = (
    0x2009F000  # the 16 track buses of 64 floats (32 left, 32 right), 0x100 bytes apart
)
TRACKS = 16
SLOTBUS = TRACKBUS  # Digitone II: the 16 voice slots' buses lie where the Digitakt II has its track buses
SLOTMAP = 0x2009E800  # Digitone II: the slot-to-track map, 16 halfwords in 8 words
# Digitone II: the slot fade flags, 16 bytes in 4 words (0x2578b8 behind the map at 0x257898)
FADETAB = SLOTMAP + 0x20
TABLE = dv.TABLE
ROW_WORDS = 24
FREE_PC = 0xFFFFFFFF  # the pc of a free row (core/dynfx): the adapter skips it
POISON = 0x7FC00001  # the initial word of every .PKBSS
AUTO_STATES = 4  # a Row with `state=""` (a run-time effect has no state symbol: the runtime reserves it) gets `_fx_auto_state<row>`,
AUTO_STATE_BYTES = 4096  # this many bytes of poisoned bss each
nw = dv.nw
# the row words (core/fx module.toml)
PC, DESC, STATE, ENABLED, P0, WAS, MIX, DIS, WSTART, WCOUNT, NFAULT, ROUTE, AMT0 = (
    0,
    1,
    2,
    3,
    4,
    12,
    13,
    14,
    15,
    16,
    17,
    18,
    19,
)
ROUTE_MASTER, ROUTE_SEND = 0, 1
ONE = 0x3F800000  # 1.0f: the default mix of a row
# registers the stock function expects back (callee-saved, its arguments) and the markers they carry into the hook
MARKS = {f"R{n}": 0x7000 + n for n in range(16)}
MARKS.update(
    {
        "I0": 0x20000010,
        "I1": 0x20000020,
        "I2": 0x20000030,
        "I3": 0x20000040,
        "I4": 0x20000050,
        "I5": 0x20000060,
        "I12": 0x20000070,
        "I13": 0x20000080,
        "I8": 0x20000090,
        "I9": 0x200000A0,
        "I10": 0x200000B0,
        "I11": 0x200000C0,
        "I14": 0x200000D0,
        "I15": 0x200000E0,
        "M1": 11,
        "M2": 12,
        "M3": 13,
        "M4": 14,
        "M8": 5,
        "M9": 6,
        "M10": 7,
        "M11": 8,
        "M12": 9,
        "B3": 0x20000040,
    }
)


def f2w(x: float) -> int:
    return struct.unpack("<I", struct.pack("<f", x))[0]


def w2f(w: int) -> float:
    return struct.unpack("<f", struct.pack("<I", w & 0xFFFFFFFF))[0]


@dataclass
class Row:
    pc: str  # the process routine's symbol; "" is a free row (pc 0xffffffff)
    state: str  # the state block's data symbol ("": the rig reserves `_fx_auto_state<row>`, as the runtime does for a run-time effect)
    enabled: int = 1
    params: tuple[int, ...] = (0, 0, 0, 0)
    desc: str = "_te_desc"
    mix: int = ONE  # row word 13: the dry/wet blend (a float, 0 = dry, 1.0 = wet); on a send row the return level
    route: int = ROUTE_MASTER  # row word 18: 0 master chain, 1 send bus
    amounts: tuple[
        int, ...
    ] = ()  # row words 19 to 22: the send amounts of the 16 tracks, bytes 0 to 127 (fewer than 16: the rest 0)


@dataclass
class Block:
    master: tuple[float, ...]  # the 64 floats after the hook
    instructions: int
    returned: bool  # the hook ended in the stock function's second instruction
    halt: dict
    ctx_bad: list[str]  # context registers that did not come back
    regs_bad: list[str]  # marked registers that did not come back
    raw: dict = field(default_factory=dict)
    ret: tuple[float, ...] = ()  # the 64 floats of the FX return bus after the hook


@dataclass
class _Built:
    linked: list
    data: dict[str, int]
    inits: list[tuple[int, bytes, bool]]
    symbols: dict[str, int]
    module_code: tuple[
        tuple[int, int], ...
    ] = ()  # (first pc, end pc) of each module object (not the adapter), short words
    module_data: tuple[
        tuple[int, int], ...
    ] = ()  # (byte address, bytes) of the module's own data and bss


_BUILT: dict[tuple, _Built] = {}


def _assemble(path: Path) -> pk_sharc.SharcObject:
    """A DSP source the way tools/patchkit/pk_sharc.py reads it: `#include` from the module's directories, the SDK
    and the source's directory (a source outside a module: the SDK and its directory)."""
    mods = [p for p in path.parents if (p / "module.toml").exists()]
    sdk = [ROOT / "sdk" / "dsp" / "contract", ROOT / "sdk"]
    dirs = [*(pk_sharc.sharc_include_dirs(mods[0]) if mods else sdk), path.parent]
    return pk_sharc.assemble(pk_sharc.expand_includes(path.read_text(), dirs))


def build(sources: tuple[Path, ...] = (EFFECTS,), device: str = "dt2") -> _Built:
    key = (ADAPTER, device, *sources)
    if key not in _BUILT:
        _BUILT[key] = _build(sources, device)
    return _BUILT[key]


def _build(sources: tuple[Path, ...], device: str) -> _Built:
    send = SEND_DN2 if device == "dn2" else SEND
    objs = [_assemble(ADAPTER), _assemble(send)] + [_assemble(Path(s)) for s in sources]
    addr = DATA_BASE
    data: dict[str, int] = {}
    inits: list[tuple[int, bytes, bool]] = []
    own: list[list[tuple[int, int]]] = []
    for o in objs:
        where: dict[str, int] = {}
        own.append([])
        for sec, (raw, _al, is_bss) in o.data.items():
            where[sec] = addr
            inits.append((addr, raw, is_bss))
            own[-1].append((addr, len(raw)))
            addr += (len(raw) + 3) & ~3
        for name, (sec, off, _g) in o.data_symbols.items():
            data[name] = where[sec] + off
    for i in range(AUTO_STATES):
        data[f"_fx_auto_state{i}"] = addr
        inits.append((addr, bytes(AUTO_STATE_BYTES), True))
        own[-1].append((addr, AUTO_STATE_BYTES))
        addr += AUTO_STATE_BYTES
    # the module step word (`DK_STEP`, sdk/dsp/contract/dk_trace.inc); core/dyndsp exports it on the device
    data["_dk_step"] = addr
    inits.append((addr, bytes(4), True))
    own[-1].append((addr, 4))
    addr += 4
    if addr > DATA_END:
        raise RuntimeError(f"data symbols need {addr - DATA_BASE:#x} bytes of L2")
    externs = {
        "_fx_table": TABLE,
        "_fx_master": MASTER,
        "_fx_back": dv.STOCK_BP,
        "_fxs_back": dv.STOCK_BP,  # one entry runs in a call: both tails end at the stand-in
        "_fxs_trackbus": TRACKBUS,
        "_fxs_return": RETURN,
        "_fxs_slotbus": SLOTBUS,
        "_fxs_slotmap": SLOTMAP,
        "_fxs_fadetab": FADETAB,
        **data,
    }
    bases, base = [], CODE_BASE
    for o in objs:
        bases.append(base)
        base += (len(o.section) // 2 + 0xFF) & ~0xFF
    if base > dv.STOCK_BP:
        raise RuntimeError(f"code ends at {base:#x}, past the stand-ins")
    syms: dict[str, int] = {}
    for o, b in zip(objs, bases, strict=True):
        syms.update({n: b + off for n, (off, _g) in o.symbols.items()})
    ext = {**externs, **syms}
    linked = [pk_sharc.link(o, b, ext) for o, b in zip(objs, bases, strict=True)]
    code = tuple(
        (b, b + len(o.section) // 2) for o, b in zip(objs[2:], bases[2:], strict=True)
    )
    return _Built(
        linked, data, inits, syms, code, tuple(r for rs in own[2:] for r in rs)
    )


class FxRig:
    def __init__(
        self,
        rows: list[Row],
        sources: tuple[Path, ...] = (EFFECTS,),
        seed: int = 0xD1617,
        step_limit: int = 200_000,
        record_stores: bool = False,
        device: str = "dt2",
    ) -> None:
        if len(rows) > 4:
            raise ValueError("a chain has at most 4 effects")
        self.built = build(sources, device)
        self.rows = rows
        self.ctx_rng = random.Random(seed)
        self.step_limit = step_limit
        self.last_ctx: dict[str, int] = {}
        self.record_stores = record_stores
        self.reset()

    def reset(self) -> None:
        b = self.built
        self.sess = pk_sharc.CallSession(b.linked, dv.RETURN_SW, breaks=(dv.STOCK_BP,))
        poke = self.sess.poke
        for k in range(5 * ROW_WORDS):
            poke(nw(TABLE) + k, 0)
        for k in range(64):
            poke(nw(MASTER) + k, 0)
            poke(nw(RETURN) + k, 0)
        for k in range(TRACKS * 64):
            poke(nw(TRACKBUS) + k, 0)
        for k in range(8):
            poke(nw(SLOTMAP) + k, 0)
        for k in range(4):
            poke(nw(FADETAB) + k, 0)
        for addr, raw, is_bss in b.inits:
            for k in range(len(raw) // 4):
                w = (
                    POISON
                    if is_bss
                    else int.from_bytes(raw[4 * k : 4 * k + 4], "little")
                )
                poke(nw(addr) + k, w)
        for i, r in enumerate(self.rows):
            base = nw(TABLE) + i * ROW_WORDS
            if not r.pc:
                poke(base + PC, FREE_PC)
                continue
            poke(base + PC, b.symbols[r.pc])
            poke(base + DESC, b.data[r.desc])
            poke(base + STATE, b.data[self.state_symbol(i)])
            poke(base + ENABLED, r.enabled)
            poke(base + MIX, r.mix)
            poke(base + ROUTE, r.route)
            amounts = (tuple(r.amounts) + (0,) * TRACKS)[:TRACKS]
            for w in range(4):
                poke(
                    base + AMT0 + w,
                    sum((amounts[4 * w + b] & 0xFF) << (8 * b) for b in range(4)),
                )
            for j, p in enumerate(r.params):
                poke(base + P0 + j, p & 0xFFFFFFFF)
        if self.record_stores:
            self.sess.record_stores()

    # -- the table -------------------------------------------------------------------------------------------------------
    def row_word(self, i: int, word: int) -> int:
        return self.sess.peek(nw(TABLE) + i * ROW_WORDS + word) or 0

    def set_row_word(self, i: int, word: int, value: int) -> None:
        self.sess.poke(nw(TABLE) + i * ROW_WORDS + word, value & 0xFFFFFFFF)

    def set_enabled(self, i: int, on: bool) -> None:
        self.set_row_word(i, ENABLED, 1 if on else 0)

    def state_symbol(self, i: int) -> str:
        return self.rows[i].state or f"_fx_auto_state{i}"

    def state_word(self, i: int, k: int) -> int:
        return self.sess.peek(nw(self.built.data[self.state_symbol(i)]) + k) or 0

    def symbol_word(self, name: str, k: int = 0) -> int:
        return self.sess.peek(nw(self.built.data[name]) + k) or 0

    # -- a block ---------------------------------------------------------------------------------------------------------
    def set_track(self, t: int, samples) -> None:
        """The 64 floats (32 left, 32 right) of track bus T, as the sends driver leaves them before the master function."""
        for k, x in enumerate(samples):
            self.sess.poke(nw(TRACKBUS + 0x100 * t) + k, f2w(x))

    def set_slot(self, v: int, samples) -> None:
        """Digitone II: the 64 floats of voice slot V's bus (the same cells as the Digitakt II's track buses)."""
        self.set_track(v, samples)

    def set_slotmap(self, tracks) -> None:
        """Digitone II: the slot-to-track map, the track of each of the 16 slots (slot v in half v % 2 of word v / 2, low half first)."""
        t = (tuple(tracks) + (0,) * TRACKS)[:TRACKS]
        for k in range(8):
            self.sess.poke(
                nw(SLOTMAP) + k, (t[2 * k] & 0xFFFF) | (t[2 * k + 1] & 0xFFFF) << 16
            )

    def set_fade(self, slots) -> None:
        """Digitone II: the slot fade flags (0x2578b8): the slots in SLOTS get the flag byte 1, the others 0."""
        flags = [1 if v in slots else 0 for v in range(TRACKS)]
        for k in range(4):
            self.sess.poke(
                nw(FADETAB) + k,
                sum((flags[4 * k + j] & 0xFF) << (8 * j) for j in range(4)),
            )

    def set_return(self, samples) -> None:
        """The FX return bus as the stock sends driver wrote it."""
        for k, x in enumerate(samples):
            self.sess.poke(nw(RETURN) + k, f2w(x))

    def set_amounts(self, i: int, amounts, route: int = ROUTE_SEND) -> None:
        """Row I: the route word and the 16 amount bytes (words 19 to 22)."""
        amounts = (tuple(amounts) + (0,) * TRACKS)[:TRACKS]
        self.set_row_word(i, ROUTE, route)
        for w in range(4):
            self.set_row_word(
                i,
                AMT0 + w,
                sum((amounts[4 * w + b] & 0xFF) << (8 * b) for b in range(4)),
            )

    def bus(self) -> tuple[float, ...]:
        """The send bus as the last send block built it (32 left, 32 right)."""
        at = nw(self.built.data["_fxs_bus"]) + 1
        return tuple(w2f(self.sess.peek(at + k) or 0) for k in range(64))

    def prev_gains(self, i: int) -> tuple[float, ...]:
        at = nw(self.built.data["_fxs_prev"]) + 16 * i
        return tuple(w2f(self.sess.peek(at + k) or 0) for k in range(16))

    def send_block(
        self, master=None, ctx: dict | str | None = None, regs: dict | None = None
    ) -> Block:
        """One block of the send hook (the master function's entry): the tracks, the return and the master as set before the call."""
        return self.block(
            master if master is not None else [0.0] * 64, ctx, regs, entry="_fxs_entry"
        )

    def block(
        self,
        master,
        ctx: dict | str | None = None,
        regs: dict | None = None,
        entry: str = "_fx_entry",
    ) -> Block:
        if ctx is None:
            ctx = dv.draw_context(self.ctx_rng)
        elif ctx == "friendly":
            ctx = dict(dv.FRIENDLY)
        else:
            ctx = {**dv.FRIENDLY, **ctx}
        self.last_ctx = dict(ctx)
        for k, x in enumerate(master):
            self.sess.poke(nw(MASTER) + k, f2w(x))
        self.sess.poke(nw(dv.STACK) - 1, dv.RETURN_SW - 1)
        self.sess.poke(nw(dv.STACK), 0x20000FF0)
        marks = {**MARKS, **(regs or {})}
        call_regs = {**marks, **ctx, "I6": dv.STACK, "I7": dv.STACK}
        names = tuple(sorted(marks))
        res = self.sess.call(
            entry,
            call_regs,
            max_steps=self.step_limit,
            read_regs=("I6", "I7", *names, *dv.CONTEXT_REGS),
        )
        halt = res["halt"]
        returned = halt["reason"] == "breakpoint" and res["final_pc_sw"] == dv.STOCK_BP
        out = tuple(w2f(self.sess.peek(nw(MASTER) + k) or 0) for k in range(64))
        ret = tuple(w2f(self.sess.peek(nw(RETURN) + k) or 0) for k in range(64))
        mask = 0xFFFFFFFF
        ctx_bad = [
            n for n in dv.CONTEXT_REGS if (res["regs"][n] or 0) & mask != ctx[n] & mask
        ]
        regs_bad = [n for n in names if (res["regs"][n] or 0) & mask != marks[n] & mask]
        for n in ("I6", "I7"):
            if (res["regs"][n] or 0) & mask != dv.STACK:
                regs_bad.append(n)
        return Block(
            out,
            res["instructions"],
            returned,
            halt,
            ctx_bad if returned else [],
            regs_bad if returned else [],
            res,
            ret,
        )

    # -- the module's view ------------------------------------------------------------------------------------------------
    def desc_words(self, sym: str, n: int = 7) -> tuple[int, ...]:
        """The words of a `dk_fx_desc` data symbol as built (before any call)."""
        at = self.built.data[sym]
        for a0, raw, _bss in self.built.inits:
            if a0 <= at < a0 + len(raw):
                return struct.unpack_from(f"<{n}I", raw, at - a0)
        raise KeyError(sym)

    def scratch(self) -> tuple[float, ...]:
        """The effect's output block as it left it."""
        at = nw(self.built.data["_fx_scratch"])
        return tuple(w2f(self.sess.peek(at + k) or 0) for k in range(64))

    def allowed(self, stack_bytes: int) -> list[tuple[int, int, str]]:
        """The DM that the module's code may store to in a call (docs/design/module-sdk.md 11.5 D6, <dk/fx/fx.h>): the output block, its own data
        and bss (its state included) and `stack_bytes` of data stack below I7 (the adapter does not lower I7 before the call)."""
        out = [(self.built.data["_fx_scratch"], 4 * 64, "output block")]
        out += [(a, n, "module data") for a, n in self.built.module_data]
        out.append((dv.STACK - stack_bytes, stack_bytes, "module stack"))
        return out

    def foreign_stores(self, stack_bytes: int) -> list[str]:
        """Stores of the last call made by the module's code outside `allowed`, as text (at most 4). Needs `record_stores`."""
        allowed = self.allowed(stack_bytes)
        names = [
            (self.built.data["_fx_save"], 128, "adapter save area"),
            (self.built.data["_fx_in"], 16, "dk_fx_in"),
            (MASTER, 256, "the master block (read only for an effect)"),
            (TABLE, 5 * ROW_WORDS * 4, "the chain table"),
        ]
        found = []
        for e in self.sess.stores:
            if not any(lo <= e.pc_sw < hi for lo, hi in self.built.module_code):
                continue
            at = (
                e.address - pk_sharc.SW_ALIAS_BASE
                if e.address >= pk_sharc.SW_ALIAS_BASE
                else e.address
            )
            lo, hi = at, at + e.width
            if any(a <= lo and hi <= a + n for a, n, _ in allowed):
                continue
            near = next(
                (f"inside {nm} +{at - a:#x}" for a, n, nm in names if a <= at < a + n),
                "unknown memory",
            )
            found.append(
                f"store of {e.width} bytes to {at:#x} at pc {e.pc_sw:#x}, {near}"
            )
            if len(found) == 4:
                break
        return found
