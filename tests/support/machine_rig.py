"""A rig for any machine module: its DSP routine on the concrete SHARC+ core, through core/dsp-voice, block by block as the engine calls it.

    rig = MachineRig("<author/name>", "dt2")
    rig.set_params({"pluck_decay": 0x7F00})       # parameter words by manifest id
    rig.set_note(60, vel=100); rig.trigger()
    b = rig.block()                                 # b.samples (32 floats), b.instructions, b.returned, b.context_bad

The rig is plain Python (no numpy, no pytest): it also runs under PyPy (tests/modules/test_machine_conformance.py).
The rig reads the module's manifest (tools/patchkit/pk_conform.py): DSP sources of the device, entry symbol, machine type and
parameters with their frame slots. It assembles the wrapper and the module, places the data symbols of both (`.PKDATA`, `.PKBSS`,
`.VAR`) in free L2, and keeps the core's memory from block to block (tools/patchkit/pk_sharc.py CallSession), so a delay line or a
state row carries over without being copied. Unwritten `.PKBSS` memory reads as NaN, and the output block is NaN before every call,
so a routine that reads what it did not write, or does not write the whole block, shows in the samples.

Per device (the engine's contract, core/dsp-voice headers):
  dt2   voice record per voice (the even voice 2t of track t), trigger = record word +0x1b8 set to 1 (ACTIVE, mark cleared); the
        call returns to the caller (RETURN_SW). Frame: pitch hw 1 + t, velocity hw 0x1a + t, machine type in the type cache.
  dn2   voice slot v, trigger = the rising edge of bit v of frame hw 0x11 (one block), pitch hw 1 + v, velocity hw 26 + v, type hw
        74 + v; the call ends in the stock helper (STOCK_BP).
The context registers (M5 ... MODE1) are drawn hostile per call from the rig's seed and must come back unchanged."""

from __future__ import annotations

import random
import struct
from dataclasses import dataclass, field
from pathlib import Path

import support.dsp_voice_rig as dv
from tools.patchkit import pk_conform, pk_sharc

ROOT = pk_conform.ROOT
_DSP = ROOT / "modules/core/adapters/dsp-voice/src/dsp"
WRAPPER = {  # the adapter's sources of a device, assembled as separate objects that see each other's data symbols
    "dt2": (_DSP / "dsp-voice.s",),
    "dn2": (_DSP / "dsp-voice-dn2.s", _DSP / "dsp-voice-dn2-stereo.s"),
}
CODE_BASE = 0xB90000
CODE_END = dv.STOCK_BP  # code must end below the stand-ins at STOCK_BP / RETURN_SW
DATA_BASE = 0x200A0000  # byte address of the first data symbol (free L2)
DATA_END = 0x20100000
NAN = 0xFFFFFFFF
OUT_DN2 = (
    dv.OUT + 0x1000
)  # DN2 voice buffers (low 7 bits of the address are 0x10: the wrapper checks)
ROW_WORDS = 24  # words of a registry row of core/dsp-voice
nw = dv.nw
# The module stack grows below I7 at entry, exclusive. DT2 lowers I7 by
# 40 words before entering the module (dsp-voice.s); DN2 saves context in
# static _dv_ctx and enters without lowering I7. DN2's MODIFY(I7, -18)
# runs only AFTER module return, as the displaced stock-helper instruction.
ADAPTER_FRAME_WORDS = {"dt2": 40, "dn2": 0}
DEFAULT_STACK_BYTES = 256  # a raw routine (no dk_machine_desc) declares no stack


@dataclass
class Block:
    samples: tuple[
        float, ...
    ]  # 32 float32 values (NaN where the routine did not write)
    instructions: int
    returned: bool  # the call came back to the caller within the step limit
    halt: dict
    context_bad: list[str]  # context registers that did not come back as they went in
    out: dict = field(default_factory=dict)
    faults: int = 0  # the runtime guard's fault count of the type since the start (registry row word 13)
    foreign: list[str] = field(
        default_factory=list
    )  # stores of the module's code outside the memory it may write (rig with record_stores), one text each
    right: tuple[
        float, ...
    ] = ()  # DT2: the odd voice's block after the call (a stereo machine's R; NaN when nothing wrote it)


@dataclass
class _Built:
    objs: list  # SharcObject per source: wrapper first
    linked: list  # Linked per object
    data: dict[str, int]  # data symbol -> byte address
    inits: list[tuple[int, bytes, bool]]  # (byte address, bytes, is_bss)
    symbols: dict[str, int]
    state: int = 0  # contract voice: byte address of voice 0's state block (the rig reserves it behind the data symbols)
    stride: int = 0  # bytes between two voices' states
    clear: int = 0  # bytes the adapter zeroes at note-on
    desc: tuple[int, ...] = ()  # the module's `dk_machine_desc` words
    base: dict[int, int] = field(
        default_factory=dict
    )  # the core's memory after reset's static pokes (filled by the first rig)
    module_code: tuple[
        tuple[int, int], ...
    ] = ()  # (first pc, end pc) of each module object (not the wrapper), short words
    module_data: tuple[
        tuple[int, int], ...
    ] = ()  # (byte address, bytes) of the module's own data and bss
    wrapper_data: tuple[
        tuple[int, int], ...
    ] = ()  # the same for the adapter (core/dsp-voice)


def _assemble(path: Path, moddir: Path) -> pk_sharc.SharcObject:
    """A DSP source the way tools/patchkit/pk_sharc.py reads it: `#include` from the module's build/gen, the SDK and the source's directory."""
    dirs = [*pk_sharc.sharc_include_dirs(moddir), path.parent]
    return pk_sharc.assemble(pk_sharc.expand_includes(path.read_text(), dirs))


# The stock functions and sites the stereo hooks of the DN2 adapter name (dsp-voice-dn2-stereo.s). The machine rig does not run them: the stand-ins
# only have to link.
STEREO_STANDINS = (
    "_gs_fo_stock",
    "_gs_fo_reset_stock",
    "_gs_uy_stock",
    "_gs_uy_reset_stock",
    "_gs_gain_stock",
    "_gs_c5_stock",
    "_gs_held_stock",
    "_gs_shaper_stock",
    "_gs_case1_stock",
    "_gs_case2_stock",
    "_gs_case3_stock",
    "_gs_ring_stock",
    "_gs_held_reset_stock",
    "_gs_case1_reset_stock",
    "_gs_case2_reset_stock",
    "_gs_case3_reset_stock",
    "_gs_ring_reset_stock",
    "_gs_case5_reset_stock",
    "_gs_engine_base",
    "_gs_pan_cont",
    "_gs_pan_end",
    "_gs_pre_cont",
)


def _externs_fixed(device: str, spec: pk_conform.MachineSpec) -> dict[str, int]:
    fixed = {
        "_fw_type_cache": dv.TYPE_CACHE,
        "_fw_render_next": dv.STOCK_BP,
        "_fw_frame": dv.FRAME,
        "_fw_param_rec": dv.PARAM_REC,
        "_dv_table": dv.TABLE,
    }
    if device == "dn2":
        fixed.update(
            {n: dv.STOCK_BP + 0x10 + 4 * i for i, n in enumerate(STEREO_STANDINS)}
        )
    return fixed


def build(module: str, device: str) -> _Built:
    """Assemble, place and link the wrapper and the module's DSP sources (cached per module and device)."""
    sp = pk_conform.spec(module, device)
    if sp is None:
        raise pk_conform.ConformError(
            f"{module} has no machine DSP object for {device}"
        )
    return build_spec(sp)


_BUILT: dict[tuple[object, ...], _Built] = {}


def build_spec(sp: pk_conform.MachineSpec) -> _Built:
    """`build` for a spec the caller made (a fixture source of a module: `dataclasses.replace(spec, sources=..., entry=..., note_on=..., desc=...)`)."""
    key = (sp.module, sp.device, sp.sources, sp.entry, sp.note_on, sp.desc, sp.params)
    if key not in _BUILT:
        _BUILT[key] = _build(sp)
    return _BUILT[key]


def _build(sp: pk_conform.MachineSpec) -> _Built:
    module, device = sp.module, sp.device
    nwrap = len(WRAPPER[device])
    objs = [_assemble(w, w.parents[2]) for w in WRAPPER[device]]
    objs += [_assemble(Path(s), sp.moddir) for s in sp.sources]
    # data symbols: one 4-byte aligned run per section, in L2 above the stand-ins
    addr = DATA_BASE
    data: dict[str, int] = {}
    inits: list[tuple[int, bytes, bool]] = []
    own: list[
        list[tuple[int, int]]
    ] = []  # per object: (byte address, bytes) of its data sections
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
    # a contract voice: the module's description, and its state blocks behind the data symbols (what the runtime reserves at load)
    desc: tuple[int, ...] = ()
    state = stride = clear = 0
    if sp.desc is not None:
        at = data[sp.desc]
        for a0, raw, _bss in inits:
            if a0 <= at < a0 + len(raw):
                desc = struct.unpack("<10I", raw[at - a0 : at - a0 + 40])
        if len(desc) != 10 or desc[0] != 0x444B4D41:
            raise pk_conform.ConformError(f"{module}: {sp.desc} is no dk_machine_desc")
        stride = (desc[4] + 7) & ~7
        clear = desc[5]
        addr = (addr + 7) & ~7
        state = addr
        inits.append((addr, bytes(desc[3] * stride), True))
        addr += desc[3] * stride
    if addr > DATA_END:
        raise pk_conform.ConformError(
            f"{module}: data symbols need {addr - DATA_BASE:#x} bytes of L2, {DATA_END - DATA_BASE:#x} are free"
        )
    # code: sequential, each object after the previous one
    externs = {**_externs_fixed(device, sp), **data}
    bases, base = [], CODE_BASE
    for o in objs:
        bases.append(base)
        base += (len(o.section) // 2 + 0xFF) & ~0xFF
    if base > CODE_END:
        raise pk_conform.ConformError(
            f"{module}: code ends at {base:#x}, past the stand-ins at {CODE_END:#x}"
        )
    syms: dict[str, int] = {}
    for o, b in zip(objs, bases, strict=True):
        syms.update({n: b + off for n, (off, _g) in o.symbols.items()})
    ext = {**externs, **syms}
    linked = [pk_sharc.link(o, b, ext) for o, b in zip(objs, bases, strict=True)]
    code = tuple(
        (b, b + len(o.section) // 2)
        for o, b in zip(objs[nwrap:], bases[nwrap:], strict=True)
    )
    return _Built(
        objs,
        linked,
        data,
        inits,
        syms,
        state,
        stride,
        clear,
        desc,
        module_code=code,
        module_data=tuple(r for rs in own[nwrap:] for r in rs),
        wrapper_data=tuple(r for rs in own[:nwrap] for r in rs),
    )


class MachineRig:
    def __init__(
        self,
        module: str,
        device: str,
        seed: int = 0xD1617,
        track: int = 0,
        step_limit: int = 400_000,
        spec: pk_conform.MachineSpec | None = None,
        record_stores: bool = False,
    ) -> None:
        self.spec = spec or pk_conform.spec(module, device)
        if self.spec is None:
            raise pk_conform.ConformError(
                f"{module} has no machine DSP object for {device}"
            )
        self.device = device
        self.track = track
        self.step_limit = step_limit
        self.built = build_spec(self.spec) if spec else build(module, device)
        self.record_stores = record_stores
        self.ctx_rng = random.Random(seed)
        self.last_ctx: dict[str, int] = {}
        self.by_id = {p.id: p for p in self.spec.params}
        self.reset()

    # -- memory ------------------------------------------------------------------------------------------------------------
    def reset(self) -> None:
        """A fresh core: zeroed stand-ins, initialised data, params at their defaults, the voice idle."""
        b = self.built
        self.sess = pk_sharc.CallSession(b.linked, dv.RETURN_SW, breaks=(dv.STOCK_BP,))
        if b.base:  # the static memory is the same for every rig of this module: copy it instead of poking 64k words again
            self.sess.runner.state.overlay.update(b.base)
        else:
            poke = self.sess.poke
            for k in range(1024):
                poke(nw(dv.FRAME) + k, 0)
            for k in range(8):
                poke(nw(dv.TYPE_CACHE) + k, 0)
            for k in range(32 * dv.REC // 4):
                poke(nw(dv.RECORDS) + k, 0)
            for k in range(2 * ROW_WORDS):
                poke(nw(dv.TABLE) + k, 0)
            poke(nw(dv.TABLE), self.spec.machine_type)
            poke(nw(dv.TABLE) + 1, b.symbols[self.spec.entry])
            for p in self.spec.params:  # words 2 to 9: the declared range of knob j (min | max << 16; none for a stock knob)
                if not p.stock and 0 <= p.lo <= p.hi <= 0xFFFF:
                    poke(nw(dv.TABLE) + 2 + p.knob, p.lo | p.hi << 16)
            if (
                self.spec.note_on is not None
            ):  # words 16 to 19: note_on pc, state base, stride, clear bytes
                poke(nw(dv.TABLE) + 16, b.symbols[self.spec.note_on])
                poke(nw(dv.TABLE) + 17, b.state)
                poke(nw(dv.TABLE) + 18, b.stride)
                poke(nw(dv.TABLE) + 19, b.clear)
            for addr, raw, is_bss in b.inits:
                for k in range(len(raw) // 4):
                    w = (
                        NAN
                        if is_bss
                        else int.from_bytes(raw[4 * k : 4 * k + 4], "little")
                    )
                    poke(nw(addr) + k, w)
            b.base.update(self.sess.runner.state.overlay)
        if self.record_stores:
            self.sess.record_stores()
        self.voice = self.track if self.device == "dn2" else 2 * self.track
        t, v = self.track, self.voice
        if self.device == "dt2":
            self._set_type_dt2(t, self.spec.machine_type)
        else:
            self.hw(74 + v, self.spec.machine_type)
            self.set_owner(t)
        for p in self.spec.params:
            self.set_param(p.id, p.default)
        self.set_note(60, 100)
        self._trig = False

    def use(self, track: int) -> None:
        """Make `track` the voice of the following calls (state and memory stay: several voices run on one core)."""
        self.track = track
        self.voice = track if self.device == "dn2" else 2 * track
        self._trig = False

    def select(self, track: int) -> None:
        """`use` for a voice that has not run yet: it gets the machine type, its parameters at their defaults and a note, like a
        fresh rig's voice."""
        self.use(track)
        if self.device == "dt2":
            self._set_type_dt2(track, self.spec.machine_type)
        else:
            self.hw(74 + self.voice, self.spec.machine_type)
            self.set_owner(track)
        for p in self.spec.params:
            self.set_param(p.id, p.default)
        self.set_note(60, 100)

    def hw(self, k: int, value: int) -> None:
        addr = nw(dv.FRAME) + k // 2
        cur = self.sess.peek(addr) or 0
        sh = 16 * (k % 2)
        self.sess.poke(
            addr, (cur & ~(0xFFFF << sh) & 0xFFFFFFFF) | ((value & 0xFFFF) << sh)
        )

    def _set_type_dt2(self, track: int, t: int) -> None:
        addr = nw(dv.TYPE_CACHE) + track // 2
        cur = self.sess.peek(addr) or 0
        sh = 16 * (track % 2)
        self.sess.poke(addr, (cur & ~(0xFFFF << sh)) | (t << sh))

    def rec_word(self, v: int, off: int, value: int) -> None:
        self.sess.poke(nw(dv.RECORDS + v * dv.REC + off), value)

    def read_rec_word(self, v: int, off: int) -> int:
        return self.sess.peek(nw(dv.RECORDS + v * dv.REC + off)) or 0

    # -- the engine's inputs -----------------------------------------------------------------------------------------------
    def set_owner(self, track: int) -> None:
        """Set DN2's current frame owner independently of the physical voice slot."""
        if self.device != "dn2":
            raise ValueError("only DN2 slots have a separate frame owner")
        self.hw(0x52D + self.voice, track)

    def set_param(self, pid: str, word: int) -> None:
        p = self.by_id[pid]
        self.hw(
            self.spec.frame.hw(
                self.track if self.device == "dt2" else self.voice, p.knob
            ),
            word,
        )

    def set_params(self, words: dict[str, int]) -> None:
        for pid, w in words.items():
            self.set_param(pid, w)

    def set_note(self, note: float, vel: int = 100) -> None:
        v = self.voice
        self.hw(1 + (self.track if self.device == "dt2" else v), int(note * 256))
        self.hw(0x1A + (self.track if self.device == "dt2" else v), vel << 8)

    def trigger(self) -> None:
        """A new note at the next block (DT2: the arm routine's store; DN2: the trig bit, one block)."""
        self._trig = True

    # -- a block -----------------------------------------------------------------------------------------------------------
    def _regs(self, ctx: dict[str, int]) -> tuple[dict, int]:
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
        v = self.voice
        if self.device == "dt2":
            out = dv.OUT + v * 0x80
            regs = {"R4": dv.RECORDS + v * dv.REC, "R8": out, "R12": 32, "R14": 32 - v}
        else:
            out = OUT_DN2 + (v + 1) * 0x100 + 0x10
            regs = {"R8": out, "R12": out, "R4": 0, "R15": 16 - v}
        return {**saved, **regs, **ctx, "I6": dv.STACK, "I7": dv.STACK}, out

    def block(self, ctx: dict | str | None = None) -> Block:
        """One call the way the engine makes it. `ctx` as in dsp_voice_rig.Rig.call (None draws a hostile context)."""
        if ctx is None:
            ctx = dv.draw_context(self.ctx_rng)
        elif ctx == "friendly":
            ctx = dict(dv.FRIENDLY)
        else:
            ctx = {**dv.FRIENDLY, **ctx}
        self.last_ctx = dict(ctx)
        v = self.voice
        if self._trig:
            if self.device == "dt2":
                self.rec_word(v, 0x1B8, 1)
            else:
                cur = self._hw_get(0x11)
                self.hw(0x11, cur | (1 << v))
        regs, out_addr = self._regs(ctx)
        for k in range(32):
            self.sess.poke(nw(out_addr) + k, NAN)
            if self.device == "dt2":
                self.sess.poke(nw(out_addr + 0x80) + k, NAN)
        self.sess.poke(nw(dv.STACK) - 1, dv.RETURN_SW - 1)
        self.sess.poke(nw(dv.STACK), 0x20000FF0)
        # the mask of the registered types, as voice 0 of an earlier block leaves it (the table holds the one row)
        self.sess.poke(
            nw(self.built.data["_dv_mask"]), 1 << min(self.spec.machine_type, 31)
        )
        res = self.sess.call(
            "_dv_entry",
            regs,
            max_steps=self.step_limit,
            read_regs=("I6", "I7", "I4", *dv.CONTEXT_REGS),
        )
        if self._trig and self.device == "dn2":
            self.hw(0x11, self._hw_get(0x11) & ~(1 << v))
        self._trig = False
        stop = dv.RETURN_SW if self.device == "dt2" else dv.STOCK_BP
        halt = res["halt"]
        returned = halt["reason"] == "breakpoint" and res["final_pc_sw"] == stop
        words = [self.sess.peek(nw(out_addr) + k) for k in range(32)]
        samples = struct.unpack(
            "<32f", struct.pack("<32I", *[NAN if w is None else w for w in words])
        )
        right = ()
        if self.device == "dt2":
            rw = [self.sess.peek(nw(out_addr + 0x80) + k) for k in range(32)]
            right = struct.unpack(
                "<32f", struct.pack("<32I", *[NAN if w is None else w for w in rw])
            )
        bad = [
            n
            for n in dv.CONTEXT_REGS
            if (res["regs"][n] or 0) & 0xFFFFFFFF != ctx[n] & 0xFFFFFFFF
        ]
        faults = self.sess.peek(nw(dv.TABLE) + 13) or 0
        foreign = self._foreign_stores(out_addr) if self.record_stores else []
        return Block(
            samples,
            res["instructions"],
            returned,
            halt,
            bad if returned else [],
            res,
            faults & 0xFFFF,
            foreign,
            right,
        )

    def odd_block(self, ctx: dict | str | None = "friendly") -> tuple[float, ...]:
        """DT2: the odd voice of the pair, called after `block` the way the engine does: it copies the even voice's block unless
        the even voice wrote R. Returns the odd voice's 32 floats (NaN where nothing wrote)."""
        if self.device != "dt2":
            raise ValueError("only the Digitakt II has a voice pair")
        ctx = dict(dv.FRIENDLY) if ctx == "friendly" else {**dv.FRIENDLY, **(ctx or {})}
        v = 2 * self.track + 1
        regs = {
            **self._regs(ctx)[0],
            "R4": dv.RECORDS + v * dv.REC,
            "R8": dv.OUT + v * 0x80,
            "R14": 32 - v,
        }
        self.sess.poke(nw(dv.STACK) - 1, dv.RETURN_SW - 1)
        self.sess.poke(nw(dv.STACK), 0x20000FF0)
        res = self.sess.call(
            "_dv_entry", regs, max_steps=self.step_limit, read_regs=("I6", "I7")
        )
        assert res["halt"]["reason"] == "breakpoint", res["halt"]
        words = [self.sess.peek(nw(dv.OUT + v * 0x80) + k) for k in range(32)]
        return struct.unpack(
            "<32f", struct.pack("<32I", *[NAN if w is None else w for w in words])
        )

    # -- the write set -----------------------------------------------------------------------------------------------------
    def _allowed(self, out_addr: int) -> list[tuple[int, int, str]]:
        """The DM a routine of the module may store to in a call of the current voice (docs/design/module-sdk.md 11.5 D6, <dk/machine.h>):
        the output block, the voice's own state block (base + stride * track, `state_bytes`), its data stack and its own data and bss (shared by all voices) (`stack_bytes` bytes below the device's actual module-entry I7). A routine without a description (the raw form) has no state; it gets DEFAULT_STACK_BYTES of stack."""
        b = self.built
        out = [(out_addr, 4 * 32, "output block")]
        if self.device == "dt2":  # a stereo machine writes R into the odd voice's block
            out.append((out_addr + 0x80, 4 * 32, "right output block"))
        elif (
            "_gs_rbuf" in self.built.data
        ):  # DN2: the slot's R buffer (a stereo machine's R, dsp-voice-dn2-stereo.s)
            out.append(
                (
                    self.built.data["_gs_rbuf"] + 128 * self.voice,
                    4 * 32,
                    "right output block",
                )
            )
        if b.desc:
            out.append(
                (
                    b.state + b.stride * self.track,
                    b.desc[4],
                    f"voice {self.track} state",
                )
            )
        out += [(a, n, "module data") for a, n in b.module_data]  # shared by all voices
        frame = ADAPTER_FRAME_WORDS[self.device]
        stack = b.desc[9] if b.desc else DEFAULT_STACK_BYTES
        out.append((dv.STACK - 4 * frame - stack, stack, "module stack"))
        return out

    def _names(self) -> list[tuple[int, int, str]]:
        """Regions to name a foreign address by: the other voices' states, the adapter's data and frame, the stand-ins."""
        b = self.built
        out = [(a, n, "adapter data") for a, n in b.wrapper_data]
        if b.desc:
            out += [
                (b.state + b.stride * t, b.desc[4], f"voice {t} state")
                for t in range(b.desc[3])
            ]
            out.append((b.state, b.stride * b.desc[3], "voice state array"))
        out += [
            (
                dv.STACK - 4 * ADAPTER_FRAME_WORDS[self.device],
                4 * ADAPTER_FRAME_WORDS[self.device],
                "adapter frame",
            ),
            (dv.FRAME, 0x1000, "frame copy"),
            (dv.RECORDS, 0x1000, "voice records"),
            (dv.TYPE_CACHE, 0x1000, "type cache"),
            (dv.TABLE, 0x1000, "registry"),
            (dv.OUT, 0x1000, "output buffers"),
            (dv.DIAG, 0x1000, "adapter diagnostics"),
        ]
        return out

    def _foreign_stores(self, out_addr: int) -> list[str]:
        """Stores of the last call made by the module's code outside `_allowed`, as text (at most 4)."""
        b = self.built
        allowed = self._allowed(out_addr)
        found = []
        for e in self.sess.stores:
            if not any(lo <= e.pc_sw < hi for lo, hi in b.module_code):
                continue
            # the core keeps a store to a not yet mapped address at the loader alias (pk_sharc.SW_ALIAS_BASE + address)
            at = (
                e.address - pk_sharc.SW_ALIAS_BASE
                if e.address >= pk_sharc.SW_ALIAS_BASE
                else e.address
            )
            lo, hi = at, at + e.width
            if any(a <= lo and hi <= a + n for a, n, _ in allowed):
                continue
            found.append(
                f"in {self._call_of(e.pc_sw)}: store of {e.width} bytes to {at:#x} at pc {e.pc_sw:#x}, {self._near(lo, allowed)}"
            )
            if len(found) == 4:
                break
        return found

    def _call_of(self, pc: int) -> str:
        """`note_on` or `render`: the entry symbol at or below the pc (the routines they call are in the same object, after them)."""
        sy = self.built.symbols
        named = [(sy[self.spec.entry], "render")]
        if self.spec.note_on is not None:
            named.append((sy[self.spec.note_on], "note_on"))
        below = [(a, n) for a, n in named if a <= pc]
        return max(below)[1] if below else "render"

    def _near(self, addr: int, allowed: list[tuple[int, int, str]]) -> str:
        for a, n, name in self._names():
            if a <= addr < a + n:
                return f"inside {name} +{addr - a:#x}"
        best = min(
            allowed, key=lambda r: min(abs(addr - r[0]), abs(addr - (r[0] + r[1])))
        )
        a, n, name = best
        gap = addr - (a + n) if addr >= a + n else a - addr
        if gap <= 64:
            return f"{gap} bytes {'past the end of' if addr >= a + n else 'before'} its own {name}"
        return "unknown memory"

    def _hw_get(self, k: int) -> int:
        cur = self.sess.peek(nw(dv.FRAME) + k // 2) or 0
        return (cur >> (16 * (k % 2))) & 0xFFFF
