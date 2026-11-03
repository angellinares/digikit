"""The core/dsp-voice test rig: stand-in addresses, `Rig` and `stops_at`."""

import random
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]

from tools.patchkit import pk_sharc  # noqa: E402

CORE = (
    ROOT / "modules" / "core" / "adapters" / "dsp-voice" / "src" / "dsp" / "dsp-voice.s"
)
# a sine routine of the raw (no contract) form
SINE = ROOT / "tests" / "support" / "raw_sine.s"
CORE_BASE, SINE_BASE = 0xB90000, 0xB92000
STOCK_BP = 0xB9F000  # stands in for the stock renderer's second instruction
RETURN_SW = 0xB9F100  # the caller's return point
# byte addresses of the stand-ins (L2), one 4 KiB page each
TYPE_CACHE, FRAME, RECORDS, OUT, TABLE, STATE, STACK, DIAG = (
    0x20091000,
    0x20092000,
    0x20093000,
    0x20098000,
    0x2009A000,
    0x2009B000,
    0x2009C000,
    0x2009D000,
)
PARAM_REC = 0x2506EC  # DT2 1.17 param record base (the stock literal; the rig keeps the stock address)
SV_L4 = DIAG + 0x100  # the wrapper's data symbols (the build places them in free L2)
SV3, SV5, BCW, REC_PTRS = DIAG + 0x104, DIAG + 0x108, DIAG + 0x10C, DIAG + 0x110
STEP_WORD = (
    DIAG + 0x120
)  # a stand-in for the module step word `_dk_step` (core/dsp-link's); its address is `_dv_rec[2]`
MASK = (
    DIAG + 0x11C
)  # `_dv_mask`: the registered types, rebuilt by voice 0 from the table
GUARD = DIAG + 0x200  # the guard's scalars and arrays (_dv_g, 128 bytes)
# the contract adapter's data (`_dv_in` 24 bytes, `_dv_params` 32, `_dv_blk` 64, `_dv_c` 64, `_dv_st` 64): zero, and not used by a row of the old form
ADAPTER = {
    "_dv_in": DIAG + 0x300,
    "_dv_params": DIAG + 0x320,
    "_dv_blk": DIAG + 0x360,
    "_dv_c": DIAG + 0x3A0,
    "_dv_slots": DIAG + 0x3E0,
    "_dv_st": DIAG
    + 0x400,  # 16 words: the per-track stereo flag (machine contract 1.2)
}
ROW = 24  # words of a registry row: type, pc, the ranges of knobs 0 to 7, the guard fields, the contract fields (words 16 to 19)
RING_A, RING_B = (
    0x200A0000,
    0x200A2000,
)  # stand-ins for the two reply records (the breadcrumbs are written into both)
REC = 0x1D8
TYPE = 8


def nw(byte: int) -> int:
    return byte // 4


def core_externs() -> dict[str, int]:
    return {
        "_fw_type_cache": TYPE_CACHE,
        "_fw_render_next": STOCK_BP,
        "_fw_frame": FRAME,
        "_fw_param_rec": PARAM_REC,
        "_dv_table": TABLE,
        "_dv_diag": DIAG,
        "_dv_sv_l4": SV_L4,
        "_dv_sv3": SV3,
        "_dv_sv5": SV5,
        "_dv_bcw": BCW,
        "_dv_rec": REC_PTRS,
        "_dv_mask": MASK,
        "_dv_g": GUARD,
        **ADAPTER,
    }


# The context registers. The firmware restores M5, M6, M13, M15, the L registers and MODE1 per DSP context (finding 45), so
# the values at the hook are the context's, not constants: the rig draws them (never the "friendly" 0 / 1 / 0 / -1) and the
# wrapper must not depend on them. M7 = -1 and M14 = 1 stay: the stock epilogue itself (the pop of the return address and
# `JUMP (M14, I12)`) and every stack operation of the caller use them, so they hold in any context where the stock renderer
# works.
FRIENDLY = {
    "M5": 0,
    "M6": 1,
    "M7": -1,
    "M13": 0,
    "M14": 1,
    "M15": -1,
    "L0": 0,
    "L1": 0,
    "L2": 0,
    "L3": 0,
    "L4": 0,
    "L5": 0,
    "MODE1": 0x1011800,
}
M6_HOSTILE = (0x1B8, 4, 0, -1)
M13_HOSTILE = (4, -2, 0x10)
ROUND_BITS = 0x1011800  # the startup MODE1: IRPTEN, NESTM, RND32, CBUFEN
MODE1_CHOICES = (
    ROUND_BITS,
    ROUND_BITS | 0x8000,  # TRUNCATE
    ROUND_BITS | 0x2000,  # ALUSAT
    ROUND_BITS & ~0x10000,  # RND32 clear
    ROUND_BITS | 0x8000 | 0x2000,
)
CONTEXT_REGS = (
    "M5",
    "M6",
    "M7",
    "M13",
    "M14",
    "M15",
    "L0",
    "L1",
    "L2",
    "L3",
    "L4",
    "L5",
    "MODE1",
)


def draw_context(rng: random.Random) -> dict[str, int]:
    """One context as the firmware could leave it: every pinned register hostile with some probability."""
    pick = rng.choice
    ctx = dict(FRIENDLY)
    ctx["M5"] = pick((0, 4, 0x1B8, -2, 0x10, 7))
    ctx["M6"] = pick((1, *M6_HOSTILE, -2, 7))
    ctx["M13"] = pick((0, *M13_HOSTILE, 0x1B8))
    ctx["M15"] = pick((-1, 0, 4, 3))
    for n in range(6):
        ctx[f"L{n}"] = pick((0, 0, 5, 33, 0x1FD))
    ctx["MODE1"] = pick(MODE1_CHOICES)
    return ctx


class Rig:
    def __init__(
        self, seed: int = 0xD1617, defines: frozenset[str] = frozenset()
    ) -> None:
        self.ctx_rng = random.Random(seed)
        self.last_ctx: dict[str, int] = {}
        # `defines`: the build options of the wrapper (`#ifdef`, tools/patchkit/pk_sharc.py), e.g. DV_BISECT_C2
        self.core = pk_sharc.assemble(
            pk_sharc.expand_includes(
                pk_sharc.apply_conditionals(CORE.read_text(), defines),
                [*pk_sharc.sharc_include_dirs(CORE.parents[2]), CORE.parent],
            )
        )
        self.sine = pk_sharc.assemble(SINE.read_text())
        self.sine_linked = None
        ext_core = core_externs()
        # the sine links against the core's symbols; the core against the table address
        core_syms = pk_sharc.link(self.core, CORE_BASE, ext_core).symbols
        self.core_linked = pk_sharc.link(self.core, CORE_BASE, ext_core)
        ext_sine = {
            "_dv_exp2": core_syms["_dv_exp2"],
            "_dv_frame_hw": core_syms["_dv_frame_hw"],
            "_sv_state": STATE,
        }
        self.sine_linked = pk_sharc.link(self.sine, SINE_BASE, ext_sine)
        self.render_pc = self.sine_linked.symbols["_sv_render"]
        self.entry = "_dv_entry"
        self.dm: dict[int, int] = {}
        self.reset()

    def reset(self) -> None:
        self.dm = {}
        for k in range(17 * ROW):
            self.dm[nw(TABLE) + k] = 0
        for k in range(32):
            self.dm[nw(STATE) + k] = 0
        for k in range(8):
            self.dm[nw(TYPE_CACHE) + k] = 0
        for k in range(256):
            self.dm[nw(FRAME) + k] = 0
        for k in range(32 * 32):
            self.dm[nw(OUT) + k] = (
                0  # the voice buffers hold the last block (a routine that writes none leaves them)
            )
        for k in range(32 * REC // 4):
            self.dm[nw(RECORDS) + k] = 0
        self.dm[nw(TABLE)] = TYPE
        self.dm[nw(TABLE) + 1] = self.render_pc
        for k in range(3):
            self.dm[nw(REC_PTRS) + k] = (
                0  # no reply records yet (core/dsp-link has not run): the breadcrumbs stay in _dv_diag
            )
        for k in range(32):
            self.dm[nw(DIAG) + k] = 0
        for k in range(32):
            self.dm[nw(GUARD) + k] = 0
        for k in range(16):
            self.dm[nw(ADAPTER["_dv_st"]) + k] = (
                0  # no track has written R (machine contract 1.2)
            )
        for k, w in enumerate((1, 2, 4, 5, 255, 255, 255, 255)):
            self.dm[nw(ADAPTER["_dv_slots"]) + k] = (
                w  # the Digitakt II's knob positions of the parameters (ADM_PARAM_SLOT*)
            )

    def table_mask(self) -> int:
        """The mask voice 0 builds from the rows now in the table (type t sets bit t, 31 or more bit 31; free rows 0xffffffff and the
        zero row's end are not types)."""
        mask = 0
        for row in range(17):
            t = self.dm.get(nw(TABLE) + row * ROW, 0)
            if t == 0:
                break
            if t != 0xFFFFFFFF:
                mask |= 1 << min(t, 31)
        return mask

    def set_rings(self) -> None:
        """The two reply records core/dsp-link's block tail would have registered."""
        self.dm[nw(REC_PTRS)] = RING_A
        self.dm[nw(REC_PTRS) + 1] = RING_B
        self.dm[nw(REC_PTRS) + 2] = STEP_WORD

    def hw(self, k: int, v: int) -> None:
        word = nw(FRAME) + k // 2
        cur = self.dm.get(word, 0)
        sh = 16 * (k % 2)
        self.dm[word] = (cur & ~(0xFFFF << sh) & 0xFFFFFFFF) | ((v & 0xFFFF) << sh)

    def set_type(self, track: int, t: int) -> None:
        word = nw(TYPE_CACHE) + track // 2
        cur = self.dm.get(word, 0)
        sh = 16 * (track % 2)
        self.dm[word] = (cur & ~(0xFFFF << sh)) | (t << sh)

    def set_note(
        self, track: int, note: float, tune: float = 64.0, vel: int = 100
    ) -> None:
        self.hw(1 + track, int(note * 256))
        self.hw(0x6D + 0x30 * track, int(tune * 256))
        self.hw(0x1A + track, vel << 8)

    def rec_word(self, v: int, off: int, value: int) -> None:
        self.dm[nw(RECORDS + v * REC + off)] = value

    def call(
        self,
        v: int,
        regs: dict | None = None,
        extra_reads: dict | None = None,
        max_steps: int = 20000,
        ctx: dict | str | None = None,
        breaks: tuple[int, ...] = (),
        mask: int | None = None,
    ) -> dict:
        """One call the way the engine makes it. `ctx` is the context registers: None draws a new hostile context from the
        rig's seed, "friendly" the constants the old rig assumed, a dict is taken as given (missing names friendly). `mask` is
        `_dv_mask` as an earlier call of voice 0 left it (None: what voice 0 would build from the table now)."""
        if ctx is None:
            ctx = draw_context(self.ctx_rng)
        elif ctx == "friendly":
            ctx = dict(FRIENDLY)
        else:
            ctx = {**FRIENDLY, **ctx}
        self.last_ctx = dict(ctx)
        self.dm[nw(MASK)] = self.table_mask() if mask is None else mask
        r4 = RECORDS + v * REC
        r8 = OUT + v * 0x80
        saved = {f"R{n}": 0x7000 + n for n in (3, 5, 6, 7, 9, 10, 11, 13, 15)}
        saved.update(
            {
                "I0": 0x20000010,
                "I1": 0x20000020,
                "I2": 0x20000030,
                "I3": 0x20000040,
                "I5": 0x20000050,
            }
        )
        saved.update(
            {"M1": 11, "M2": 12, "M3": 13, "I8": 0x20000080, "I9": 0x20000090, "M8": 5}
        )
        r = {
            "R4": r4,
            "R8": r8,
            "R12": 32,
            "R14": 32 - v,
            **ctx,
            "I6": STACK,
            "I7": STACK,
            **saved,
            **(regs or {}),
        }
        self.dm[nw(STACK) - 1] = (
            RETURN_SW - 1
        )  # the return address the CJUMP pushed: [I6 - 1]
        self.dm[nw(STACK)] = 0x20000FF0  # the caller's frame pointer: [I6]
        reads = {"out": (nw(OUT + v * 0x80), 32), "table": (nw(TABLE), 4)}
        reads.update(extra_reads or {})
        out = pk_sharc.run_call(
            self.core_linked,
            self.entry,
            regs=r,
            dm_words=dict(self.dm),
            reads=reads,
            return_sw=RETURN_SW,
            others=(self.sine_linked,),
            read_regs=(
                "I6",
                "I7",
                "I4",
                *CONTEXT_REGS,
                "I8",
                "I9",
                "M8",
                "R3",
                "R5",
                "R15",
                "I0",
                "I5",
                "M1",
                "M3",
            ),
            max_steps=max_steps,
            extra_breaks=(STOCK_BP, *breaks),
        )
        out["in"] = r
        return out


def stops_at(out: dict, sw: int) -> bool:
    return out["halt"]["reason"] == "breakpoint" and out["final_pc_sw"] == sw
