"""A model rig for dsp-voice-dn2-stereo.s: the stock DN2 1.12 DSP image with the stereo object linked beside it, on the concrete SHARC+ core.

The original firmware is read at run time (out/sections/dn2-1.12/section_7_BLOB.bin), never copied into a fixture. The stock stages and sites
are the image's: the wrappers call and jump to the real functions, and the pan hook is patched into the real loop (an overlay of the 6 bytes at
0x1c9af6, the same bytes `patchkit` writes). The engine's memory is rebuilt synthetically at test addresses: the engine's base (`ENGINE`), the table of
voice buffers and the pan records behind it (`TABLE`), the table of plane pointers and the planes.

Pointers are byte addresses (the engine's convention); `poke` and `peek` take them as they are."""

import hashlib
import struct

import pytest

from support.paths import REPO_ROOT
from tools.patchkit import pk_sharc
from tools.sharc_dev.sharcldr import LoadedMemory

IMAGE = REPO_ROOT / "out/sections/dn2-1.12/section_7_BLOB.bin"
IMAGE_SHA256 = "336e340aa0cdcd34e314cfa44849f709a3134f6bd4cd57dfc7e15702c83115e2"
ADAPTER = REPO_ROOT / "modules/core/adapters/dsp-voice"
STEREO_SOURCE = ADAPTER / "src/dsp/dsp-voice-dn2-stereo.s"

CODE = 0xB90000  # the stereo object (short-word address)
BACK = 0xB9F000  # the stand-in return address of a call
PAN_HOOK = 0x1C9AF6
PAN_NEXT = 0x1C9AF9
PAN_END = 0x1C9B30
PRE_HOOK = 0x1C9AAF  # the first instruction after the fade loop
PRE_NEXT = 0x1C9AB2
PAN_BODY = 0x1C9AEA  # first instruction of the pan loop's body
BLOCK_FROM = 0x1C99DE  # the block ramp, the fade, the metadata clear and the pan loop
BLOCK_TO = 0x1C9B33  # the instruction after the pan loop

# stock functions (DN2 1.12)
STOCK = {
    "_gs_fo_stock": 0xB80C39,
    "_gs_fo_reset_stock": 0xB80B93,
    "_gs_uy_stock": 0xB81368,
    "_gs_uy_reset_stock": 0xB8135A,
    "_gs_gain_stock": 0xB80345,
    "_gs_c5_stock": 0xB81526,
    "_gs_held_stock": 0xB8265B,
    "_gs_shaper_stock": 0x1CDB56,
    "_gs_case1_stock": 0xB82342,
    "_gs_case2_stock": 0xB81A16,
    "_gs_case3_stock": 0xB80F2E,
    "_gs_ring_stock": 0xB806F5,
    "_gs_held_reset_stock": 0xB82440,
    "_gs_case1_reset_stock": 0xB81D6D,
    "_gs_case2_reset_stock": 0xB81C8D,
    "_gs_case3_reset_stock": 0xB811F1,
    "_gs_ring_reset_stock": 0xB805D8,
    "_gs_case5_reset_stock": 0xB818C7,
}

# test addresses (L2 bytes)
ENGINE = 0x20110000  # `_gs_engine_base`: the engine's base address I9
FRAME = 0x2010F000  # the callee frame of a wrapper call
CALLER = 0x2010F400  # the engine's frame: word -24 is the table of voice buffers
STACK = 0x2010E000  # the engine's frame in the pan block runs
TABLE = (
    0x20150000  # 16 pointers to the voice buffers; the pan records are at TABLE + 0x354
)
STRUCT = 0x20151000  # word 3 points to the table of plane pointers
PLANE_TABLE = 0x20152000
BUFFERS = 0x20170000  # voice buffer of slot v: BUFFERS + 0x80 v (the low 7 bits are 0x10 in the engine; the wrappers do not need it)
PLANES = 0x20180000  # plane A of slot v: PLANES + 0x100 v, plane B + 0x80
DV = 0x200C0000  # stand-ins of the adapter's data: _dv_diag (128 bytes), _dv_c (64), _dv_in (96)
PRIVATE = 0x200D0000  # the stereo object's data

HEARTBEAT = 7  # the block counter the tests use
STUB = 0xB92000  # a stand-in for every stock function (Rig(stub=True)): logs R4, R8, R12 and the call count at STUB_LOG, returns
STUB_LOG = 0x2010D000

REGS = {f"{kind}{i}": 0 for kind in "RIMLB" for i in range(16)}
REGS.update(M5=0, M6=1, M7=0xFFFFFFFF, M13=0, M14=1, M15=0xFFFFFFFF, MODE1=0)


def bits(value):
    return struct.unpack("<I", struct.pack("<f", value))[0]


def value(word):
    return struct.unpack("<f", struct.pack("<I", word))[0]


def f32(x):
    return struct.unpack("<f", struct.pack("<f", x))[0]


def nan_block(n):
    return [0xFFFFFFFF] * n


class Rig:
    """Assembles and links once; `session()` makes a fresh core."""

    def __init__(self, stub=False):
        if not IMAGE.exists():
            pytest.skip("requires original DN2 1.12 section 7")
        data = IMAGE.read_bytes()
        assert hashlib.sha256(data).hexdigest() == IMAGE_SHA256
        self.memory = LoadedMemory.from_stream(data)
        dirs = [*pk_sharc.sharc_include_dirs(ADAPTER), STEREO_SOURCE.parent]
        self.obj = pk_sharc.assemble(
            pk_sharc.expand_includes(STEREO_SOURCE.read_text(), dirs)
        )
        address, self.data, self.inits = PRIVATE, {}, []
        for section, (payload, alignment, is_bss) in self.obj.data.items():
            address = (address + alignment - 1) & -alignment
            for name, (sec, offset, _) in self.obj.data_symbols.items():
                if sec == section:
                    self.data[name] = address + offset
            self.inits.append((address, payload, is_bss))
            address += len(payload)
        self.data_end = address
        self.dv = {"_dv_diag": DV, "_dv_c": DV + 0x100, "_dv_in": DV + 0x200}
        self.stub = stub
        stock = {n: STUB for n in STOCK} if stub else STOCK
        externs = {
            **self.data,
            **self.dv,
            **stock,
            "_gs_engine_base": ENGINE,
            "_gs_pan_cont": PAN_NEXT,
            "_gs_pan_end": PAN_END,
            "_gs_pre_cont": PRE_NEXT,
        }
        self.linked = pk_sharc.link(self.obj, CODE, externs)
        self.sym = self.linked.symbols
        self.stub_code = None
        if stub:
            self.stub_code = pk_sharc.build(
                f""".SECTION/PM seg_pmco;
.GLOBAL _stub;
_stub:
    DM({STUB_LOG}) = R4;
    DM({STUB_LOG + 4}) = R8;
    DM({STUB_LOG + 8}) = R12;
    R0 = DM({STUB_LOG + 12});
    R1 = 1;
    R0 = R0 + R1;
    DM({STUB_LOG + 12}) = R0;
    I12 = DM(-1, I6);
    NOP;
    NOP;
    NOP;
    JUMP (M14, I12) (DB);
    NOP;
    RFRAME;
""",
                STUB,
            )[1]
        # the pan hook, as patchkit writes it: JUMP _gs_slot over the instruction at 0x1c9af6
        hook = pk_sharc.build(
            ".SECTION/PM seg_pmco;\n.GLOBAL _site;\n_site: JUMP _gs_slot;",
            PAN_HOOK,
            {"_gs_slot": self.sym["_gs_slot"]},
        )[1]
        assert len(hook.code_boot) == 6
        self.hook = hook
        pre = pk_sharc.build(
            ".SECTION/PM seg_pmco;\n.GLOBAL _site;\n_site: JUMP _gs_prepan;",
            PRE_HOOK,
            {"_gs_prepan": self.sym["_gs_prepan"]},
        )[1]
        assert len(pre.code_boot) == 6
        self.pre = pre

    def session(self, hooked=True, poison=True):
        linked = [self.linked, self.hook, self.pre] if hooked else [self.linked]
        if self.stub_code is not None:
            linked.append(self.stub_code)
        session = pk_sharc.CallSession(linked, BACK, base_memory=self.memory)
        session.runner.state.assume_nw32 = True
        session.symbols.update(_pan_body=PAN_BODY, _block=BLOCK_FROM)
        for address, payload, is_bss in self.inits:
            for off in range(0, len(payload), 4):
                word = (
                    0xFFFFFFFF
                    if is_bss and poison
                    else int.from_bytes(payload[off : off + 4], "little")
                )
                session.poke(address + off, word)
        for off in range(0, 128, 4):
            session.poke(DV + off, 0)
        session.poke(DV, HEARTBEAT)
        for off in range(0, 16, 4):
            session.poke(STUB_LOG + off, 0)
        return session


def words(session, address, count):
    return [session.peek(address + 4 * i) for i in range(count)]


def poke_words(session, address, values):
    for i, v in enumerate(values):
        session.poke(address + 4 * i, v & 0xFFFFFFFF)


# ---- the engine's data for the pan block and the wrapper calls ----


def build_engine(session, left, ramp=None, fade=None, pan=None, gain_words=None):
    """The engine's memory behind the pan block (BLOCK_FROM to BLOCK_TO) for 16 slots.

    left[v]: 32 float bits of the voice buffer of slot v; ramp[v] = (start, step) floats of the block ramp (default 1.0, 0.0);
    fade: the set of slots whose two fade flags are nonzero (byte v of the array at engine + 0x138fc, word 5 of the gain record); pan[v] = (gA, stepA, gB, stepB) floats of the pan record;
    gain_words[v]: the 11 words of the gain record (the fade's second flag is word 5)."""
    ramp = ramp or {}
    fade = fade or set()
    pan = pan or {}
    session.poke(STACK - 24 * 4, TABLE)
    session.poke(STACK - 30 * 4, STRUCT)
    session.poke(STRUCT + 12, PLANE_TABLE)
    session.poke(STACK, CALLER)
    for v in range(16):
        session.poke(TABLE + 4 * v, BUFFERS + 0x80 * v)
        session.poke(PLANE_TABLE + 8 * v, PLANES + 0x100 * v)
        session.poke(PLANE_TABLE + 8 * v + 4, PLANES + 0x100 * v + 0x80)
        poke_words(session, BUFFERS + 0x80 * v, left[v])
        poke_words(session, PLANES + 0x100 * v, nan_block(64))
        record = TABLE + 0x354 + 28 * v
        poke_words(session, record, [0] * 7)
        ga, step_a, gb, step_b = pan.get(
            v, (0.5 + v / 64, 0.0007 * (v + 1), 0.25 + v / 128, -0.0005 * (v + 1))
        )
        session.poke(record, bits(ga))
        session.poke(record + 8, bits(step_a))
        session.poke(record + 0x70 * 4, bits(gb))
        session.poke(record + 0x72 * 4, bits(step_b))
        start, step = ramp.get(v, (1.0, 0.0))
        poke_words(session, ENGINE + 0x1395C + 28 * v, [0] * 7)
        session.poke(ENGINE + 0x1395C + 28 * v, bits(start))
        session.poke(ENGINE + 0x1395C + 28 * v + 8, bits(step))
        gain = (gain_words or {}).get(v, [0] * 11)
        poke_words(session, ENGINE + 0x1351C - 20 + 44 * v, gain)
        if v in fade:
            session.poke(ENGINE + 0x1351C + 44 * v, 1)
    set_fade_flags(session, fade)


def set_fade_flags(session, slots):
    """The byte array engine + 0x138fc (one byte per slot): 1 for the slots, 0 for the others."""
    for word in range(4):
        value_word = 0
        for k in range(4):
            if 4 * word + k in slots:
                value_word |= 1 << (8 * k)
        session.poke(ENGINE + 0x138FC + 4 * word, value_word)


def engine_regs(**extra):
    regs = {
        **REGS,
        "I6": STACK,
        "I7": STACK - 0x100,
        "I9": ENGINE,
        "I4": ENGINE,
        "R9": 32,
        "R11": 0,
    }
    return regs | extra


def run_block(session, stop=BLOCK_TO, steps=60000):
    session.runner.breakpoints = frozenset((BACK, stop))
    result = session.call("_block", engine_regs(), max_steps=steps)
    assert result["halt"]["reason"] == "breakpoint", result
    assert result["final_pc_sw"] == stop
    return result


def planes(session):
    return [
        (
            words(session, PLANES + 0x100 * v, 32),
            words(session, PLANES + 0x100 * v + 0x80, 32),
        )
        for v in range(16)
    ]


def buffers(session):
    return [words(session, BUFFERS + 0x80 * v, 32) for v in range(16)]


# ---- a wrapper call the way the engine makes a linked call ----

CALLEE_SAVED = (
    "R3",
    "R5",
    "R6",
    "R7",
    "R9",
    "R10",
    "R11",
    "R13",
    "R14",
    "R15",
    "I0",
    "I1",
    "I2",
    "I3",
    "I5",
    "M1",
    "M2",
    "M3",
)


def new_session(rig, hooked=False):
    session = rig.session(hooked=hooked)
    session.poke(CALLER - 24 * 4, TABLE)
    for v in range(16):
        session.poke(TABLE + 4 * v, BUFFERS + 0x80 * v)
    return session


def set_stack(session, args):
    session.poke(FRAME, CALLER)
    session.poke(FRAME - 4, BACK - 1)
    for i, a in enumerate(args):
        session.poke(FRAME + 4 * (i + 1), a)


def call(session, entry_pc, rec, inp, out, args, sentinels=True, **regs):
    session.symbols["_entry"] = entry_pc
    set_stack(session, args)
    r = {
        **REGS,
        "I6": FRAME,
        "I7": FRAME - 8,
        "R2": CALLER,
        "R4": rec,
        "R8": inp,
        "R12": out,
    }
    if sentinels:
        for i, n in enumerate(CALLEE_SAVED):
            r[n] = 0x20000100 + 16 * i if n.startswith("I") else 0x3000 + i
    r.update(regs)
    session.runner.breakpoints = frozenset((BACK,))
    result = session.call("_entry", r, max_steps=40000, read_regs=CALLEE_SAVED)
    assert result["halt"]["reason"] == "breakpoint" and result["final_pc_sw"] == BACK, (
        result
    )
    if sentinels:
        for n in CALLEE_SAVED:
            assert result["regs"][n] == r[n], f"{n} is callee-saved"
    return result
