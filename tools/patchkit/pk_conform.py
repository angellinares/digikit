"""Conformance of a machine module: what its manifest declares, and the cases every machine must pass.

A machine module (a `[[voice]]` or an `[[entry]] registry = "voices"` with a DSP object for a device) is run on the concrete SHARC+
core through the wrapper (core/dsp-voice) by tests/support/machine_rig.py. This file holds the side that needs no core:

  * `spec(module_id, device)`: the module's DSP sources, entry symbol, machine type, parameters (knob slots, min, max, default)
    and the frame layout of the device, all read from the manifests;
  * `budget(device, spec)`: the instruction budget of one voice-block (rule below; the numbers come from `patchkit budget`, packages/dkprof);
  * `cases(spec)`: the cases (`Case`) of tests/modules/test_machine_conformance.py, generated from the parameters.

Parameter kinds do not exist in the manifest yet, so every parameter is treated as possibly pitch-like.

The budget rule. A machine shares the DSP block with the stock firmware. The block is `dsp_clock_hz / dsp_blocks_per_second`
cycles (devices/*.toml `[board]`). The stock firmware with one held note uses `stock_busy` instructions per block
(devices/cycle-factors.json); instructions are projected to cycles with the "high" factor of the same file, and a patched image
stays under DEFAULT_BUDGET_PCT (85) of the block (packages/dkprof/src/budget.rs, run by `patchkit budget`). The headroom
(instructions) is what is left between the stock load and that limit. The `voices` of the device's `[adapter.machine]` share it:

    voice-block budget  = headroom / voices        (every block that is not a note-on)
    note-on budget      = headroom                 (the block of a trigger may take the whole headroom once)

No manifest replaces either number. `module:check` writes the module's own cost (the worst steady block of one voice, or of one effect
call) into the manifest's `[uses.<device>]` (`dsp_cost_voice`, `dsp_cost_fx`), and the broker checks the selection with it
(docs/plans/sdk.md section 9).
"""

from __future__ import annotations

import dataclasses
import json
import random
import struct
import tomllib
from dataclasses import dataclass
from functools import cache
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]

from tools.patchkit import modroots, pk_device  # noqa: E402

DEVICES = ("dt2", "dn2")
# Stock rows of the knob list that a DSP routine reads (a code in `knobs`, not a `[[param]]` id): name, min, max, default.
# 0xca is TUNE of SAMPLE (a pitch offset in semitones, 64.0 at rest; the routines read it as 64 * 256 at rest).
STOCK_KNOBS = {0xCA: ("tune", 0, 0x7F00, 0x4000)}
# Level bound of a block: full scale is +-1.0; a bound 18 dB above it flags a runaway (a state that grows by orders of magnitude),
# not a loud but valid sound. The engine's amp stage and the master limit after the machine.
LEVEL_BOUND = 8.0
NOTES_LIMITS = (0, 24, 48, 60, 72, 96, 127)
NOTES_CORNERS = (0, 60, 127)
SETTLE = 3  # blocks run after the note-on block in the short cases
WALK_BLOCKS = 500
LONG_BLOCKS = 2000
RETRIG_EVERY = 7
RETRIG_BLOCKS = 280


# Fixtures that must FAIL the suite: the suite's own test runs them and expects the named failure.
NEGATIVE = {
    "test/dyn-dsp-hang": "did not return",
    "test/dyn-guard-nan": "guard stepped in",
}


# Negative fixtures that are a source file of a module, not a module of their own: id `module#name` -> (path under the module,
# render symbol, note_on symbol, description symbol, expected message). The spec is the module's with these replaced.
NEGATIVE_VARIANTS = {
    "test/dyn-guard-nan#overrun": (
        "src/dsp/overrun.s",
        "_ov_render",
        "_ov_note_on",
        "_ov_desc",
        "wrote memory it does not own",
    ),
}
NEGATIVE.update({k: v[4] for k, v in NEGATIVE_VARIANTS.items()})


class ConformError(RuntimeError):
    pass


@dataclass(frozen=True)
class Param:
    id: str
    knob: int  # position in the machine's knob list
    lo: int
    hi: int
    default: int
    stock: bool = False  # a stock knob of the base machine (the tune knob), not a `[[param]]` of the module
    latched_word: int | None = None

    @property
    def mid(self) -> int:
        return (
            (self.lo + self.hi) // 2
        ) & ~0xFF  # a value on the UI grid (words are UI * 256)


@dataclass(frozen=True)
class Frame:
    """Halfword of the DSP's frame copy that holds knob j of voice v: base + stride * v + j + (j // group) * gap."""

    base: int
    stride: int
    group: int = 0
    gap: int = 0

    def hw(self, v: int, j: int) -> int:
        return (
            self.base
            + self.stride * v
            + j
            + ((j // self.group) * self.gap if self.group else 0)
        )


@dataclass(frozen=True)
class MachineSpec:
    module: str
    device: str
    moddir: Path
    sources: tuple[Path, ...]  # the DSP sources of the module for the device
    entry: str
    machine_type: int
    params: tuple[Param, ...]
    frame: Frame
    note_on: str | None = (
        None  # a contract voice (docs/design/module-sdk.md 4.1): the note_on routine's symbol and the `dk_machine_desc` symbol
    )
    desc: str | None = None
    fixtures: tuple[
        str, ...
    ] = ()  # `[conformance] fixtures` of module.toml: what the rig sets up before a case
    sample_records: str | None = (
        None  # `[conformance] sample_records`: the data symbol of the module's sample pool records
    )

    @property
    def label(self) -> str:
        return f"{self.module}@{self.device}"

    @property
    def key(self) -> tuple:
        """What decides the behaviour of the routine: two modules built from the same sources and parameters run once."""
        return (self.device, self.sources, self.params)


@cache
def _manifest(moddir: Path) -> dict:
    return tomllib.loads((moddir / "module.toml").read_text(encoding="utf-8"))


def _fx_records(mid: str, man: dict) -> list[dict]:
    """The effects of an effect module (`[[fx]]` in module.toml): the DSP entry names (`_<prefix>_process`, `_<prefix>_desc`; the prefix is the
    id, or `dsp`) and the knob ranges of the committed rows (build/gen/gallery.json, written by `module:check`)."""
    if man.get("module", {}).get("type") != "fx" or not man.get("fx"):
        return []
    path = modroots.module_dir(mid) / "build/gen/gallery.json"
    if not path.is_file():
        raise ConformError(f"{mid}: no build/gen/gallery.json: run `module:check`")
    effects = json.loads(path.read_text(encoding="utf-8")).get("effects") or []
    out = []
    for n, f in enumerate(man["fx"]):
        p = f.get("dsp") or f["id"].replace("-", "_")
        knobs = effects[n]["knobs"] if n < len(effects) else []
        out.append(
            {
                "id": f["id"],
                "process": f"_{p}_process",
                "desc": f"_{p}_desc",
                "knob": [{"min": k["min"], "max": k["max"]} for k in knobs],
            }
        )
    return out


def _voices(mid: str, man: dict) -> list[dict]:
    """The voice records of a machine module (`[[machine]]` in module.toml): the DSP entry names of its machine (`_<prefix>_render`,
    `_<prefix>_note_on`, `_<prefix>_desc`; the prefix is the id, or `dsp`)."""
    if man.get("module", {}).get("type") == "machine" and man.get("machine"):
        m = man["machine"][0]
        p = m.get("dsp") or m["id"].replace("-", "_")
        return [
            {
                "machine": m["id"],
                "render": f"_{p}_render",
                "note_on": f"_{p}_note_on",
                "desc": f"_{p}_desc",
            }
        ]
    return []


def _entry_row(mid: str, man: dict) -> tuple[str, str | None] | None:
    """(render symbol, literal type or None) of the module's voice."""
    for v in _voices(mid, man):
        return v["render"], None
    for e in man.get("entry", []):
        if e.get("registry") == "voices" and isinstance(
            e.get("pc"), str
        ):  # core/dyndsp reserves rows with pc = 0
            return e["pc"], e.get("type")
    return None


def _contract(mid: str, man: dict) -> tuple[str | None, str | None]:
    for v in _voices(mid, man):
        return v.get("note_on"), v.get("desc")
    return None, None


def _dsp_sources(man: dict, moddir: Path, device: str) -> tuple[Path, ...]:
    out: list[Path] = []
    for o in man.get("object", []):
        if device not in o.get("devices", DEVICES):
            continue
        for s in o.get("sources", []):
            if s.endswith(".s") and "dsp" in Path(s).parts:
                out.append((moddir / s).resolve())
    return tuple(out)


def _type_number(mid: str, man: dict, device: str, literal: str | None) -> int:
    if (
        literal is not None
        and str(literal).lstrip("-").isalnum()
        and str(literal)[0].isdigit()
    ):
        return int(literal, 0)
    types = tomllib.loads(
        (ROOT / "modules/core/adapters/machines/types.toml").read_text()
    )[device]
    # a module: the id of module.toml
    mach = man["machine"][0]["id"] if man.get("machine") else None
    if literal is not None:  # "machines.<name>.index": the type allocated to <name>
        mach = str(literal).split(".")[1]
    key = f"{mid}:{mach}"
    if key in types.get("alloc", {}):
        return int(types["alloc"][key])
    if man.get("module", {}).get("dynamic") and "dynamic_first" in types:
        # a run-time machine has no fixed number (the runtime assigns one at load); the rig only needs a type in the run-time range
        return int(types["dynamic_first"])
    raise ConformError(
        f"{mid}: no machine type for {key} in core/machines types.toml [{device}]"
    )


def _params(moddir: Path, man: dict, device: str) -> tuple[Param, ...]:
    mid = man.get("module", {}).get("id", moddir.name)
    path = moddir / "build/gen/gallery.json"
    if not path.is_file():
        raise ConformError(f"{mid}: no build/gen/gallery.json: run `patchkit gen`")
    gallery = json.loads(path.read_text(encoding="utf-8"))
    machines = gallery.get("machines") or []
    if not machines:
        return ()
    dev = machines[0].get("devices", {}).get(device)
    if dev is None:
        return ()
    listed = gallery.get("params", {}).get(device)
    decl = {p["id"]: p for p in (listed or [])}
    out = []
    for j, k in enumerate(dev.get("knobs", [])):
        if not isinstance(k, dict):
            continue
        if "param" in k and k["param"] in decl:
            p = decl[k["param"]]
            out.append(
                Param(
                    k["param"],
                    j,
                    int(p["min"]),
                    int(p["max"]),
                    int(p["default"]),
                    latched_word=int(p["latched_word"])
                    if p.get("latched_word") is not None
                    else None,
                )
            )
        elif k.get("stock") in STOCK_KNOBS:
            name, lo, hi, dflt = STOCK_KNOBS[k["stock"]]
            out.append(Param(name, j, lo, hi, dflt, True))
    return tuple(out)


def spec(mid: str, device: str) -> MachineSpec | None:
    """The spec of module MID on DEVICE, or None when the module is no machine with a DSP object for that device."""
    if mid.split("/")[0] == "core":  # the framework renders no machine of its own
        return None
    if mid in NEGATIVE_VARIANTS:
        base = spec(mid.split("#")[0], device)
        if base is None:
            return None
        path, render, note_on, desc, _msg = NEGATIVE_VARIANTS[mid]
        return dataclasses.replace(
            base,
            module=mid,
            sources=((base.moddir / path).resolve(),),
            entry=render,
            note_on=note_on,
            desc=desc,
        )
    moddir = modroots.module_dir(mid)
    man = _manifest(moddir)
    ports = man.get("module", {}).get("ports", [])
    if ports and not any(p.split("-")[0] == device for p in ports):
        return None
    row = _entry_row(mid, man)
    if row is None:
        return None
    sources = _dsp_sources(man, moddir, device)
    if not sources:
        return None
    f = pk_device.load(device)["modules"]["frame"]
    frame = Frame(f["base"], f["stride"], f.get("group", 0), f.get("gap", 0))
    return MachineSpec(
        mid,
        device,
        moddir,
        sources,
        row[0],
        _type_number(mid, man, device, row[1]),
        _params(moddir, man, device),
        frame,
        *_contract(mid, man),
        tuple(man.get("conformance", {}).get("fixtures", ())),
        man.get("conformance", {}).get("sample_records"),
    )


# Fixtures that declare a machine and that the suite does not run.
MACHINE_SKIP = {
    "test/commit-demo": "its DSP does nothing; the fixture of modules/core/runtime/tests/capacity/commit.device.test.ts"
}


def machine_specs(negative: bool = False) -> list[MachineSpec]:
    """Every (machine module, device) the suite runs (NEGATIVE fixtures only with `negative`)."""
    out = []
    for mid in modroots.manifests():
        if (mid in NEGATIVE) != negative or mid in MACHINE_SKIP:
            continue
        for dev in DEVICES:
            s = spec(mid, dev)
            if s is not None:
                out.append(s)
    if negative:
        out += [s for vid in NEGATIVE_VARIANTS for d in DEVICES if (s := spec(vid, d))]
    return out


def is_machine_module(mid: str) -> bool:
    return any(spec(mid, d) is not None for d in DEVICES)


@dataclass(frozen=True)
class Budget:
    voice_block: int
    note_on_block: int
    headroom: int


@cache
def _device_budgets() -> dict:
    """`patchkit budget`: the Rust computation (packages/dkprof budget), one process per run; every device and the effect budget."""
    import subprocess

    from tools.patchkit import patchkit

    r = subprocess.run(
        [
            str(patchkit.binary()),
            "--root",
            str(ROOT),
            "budget",
            "--fx-max",
            str(FX_MAX),
        ],
        cwd=ROOT,
        capture_output=True,
        text=True,
    )
    if r.returncode:
        raise RuntimeError(f"patchkit budget failed: {r.stderr.strip()}")
    return json.loads(r.stdout)


def budget(device: str) -> Budget:
    d = _device_budgets()["devices"][device]
    return Budget(int(d["voice_block"]), int(d["note_on_block"]), d["headroom"])


# ---- cases ----------------------------------------------------------------------------------------------------------------


@dataclass(frozen=True)
class Case:
    """A run on a fresh rig: `blocks` blocks, block k with the parameter words `words(k)`; a trigger at the blocks of `trig`."""

    group: str
    name: str
    note: int
    vel: int
    words: tuple[
        tuple[tuple[str, int], ...], ...
    ]  # per block: (param id, word) pairs set before the block
    blocks: int
    trig: tuple[int, ...] = (0,)
    setup: str | None = (
        None  # a fixture the worker sets up before the first block ("sample-pool")
    )
    sample: tuple[int, int, int] | None = (
        None  # (frames, source rate in Hz, channels) of the sample the "sample-pool" setup publishes
    )


def _defaults(sp: MachineSpec) -> dict[str, int]:
    return {p.id: p.default for p in sp.params}


def cases(sp: MachineSpec) -> list[Case]:
    out: list[Case] = []
    dflt = _defaults(sp)

    def static(
        group: str, name: str, note: int, vals: dict[str, int], blocks: int = 1 + SETTLE
    ) -> Case:
        return Case(group, name, note, 100, (tuple(vals.items()),), blocks)

    # 1. limits: every parameter at min, max, default and mid, the others at default, per note
    for note in NOTES_LIMITS:
        out.append(static("limits", f"all-default/note{note}", note, dflt))
        for p in sp.params:
            for tag, w in (("min", p.lo), ("max", p.hi), ("mid", p.mid)):
                if w == p.default:
                    continue
                out.append(
                    static(
                        "limits", f"{p.id}={tag}/note{note}", note, {**dflt, p.id: w}
                    )
                )
    # 2. corners: all parameters together at {min, max}, notes {0, 60, 127}
    n = len(sp.params)
    for note in NOTES_CORNERS:
        for bits in range(1 << n):
            vals = {
                p.id: (p.hi if bits >> i & 1 else p.lo) for i, p in enumerate(sp.params)
            }
            tag = "".join("H" if bits >> i & 1 else "L" for i in range(n)) or "-"
            out.append(static("corners", f"corner-{tag}/note{note}", note, vals))
    # 3. per-block modulation: every parameter moved each block, a seeded random walk over its full range, and a square
    #    wave between its limits (an LFO at the highest speed and depth)
    for note in (24, 100):
        rng = random.Random(0xC0FFEE + note)
        pos = {p.id: float(p.default) for p in sp.params}
        walk = []
        for _ in range(WALK_BLOCKS):
            row = []
            for p in sp.params:
                pos[p.id] += rng.uniform(-0.6, 0.6) * (p.hi - p.lo)
                if pos[p.id] < p.lo:
                    pos[p.id] = 2 * p.lo - pos[p.id]
                if pos[p.id] > p.hi:
                    pos[p.id] = 2 * p.hi - pos[p.id]
                pos[p.id] = min(max(pos[p.id], p.lo), p.hi)
                row.append((p.id, int(pos[p.id])))
            walk.append(tuple(row))
        out.append(
            Case(
                "modulation",
                f"random-walk/note{note}",
                note,
                100,
                tuple(walk),
                WALK_BLOCKS,
            )
        )
    square = tuple(
        tuple((p.id, p.hi if (k + i) % 2 else p.lo) for i, p in enumerate(sp.params))
        for k in range(WALK_BLOCKS)
    )
    out.append(
        Case("modulation", "square-every-block/note60", 60, 100, square, WALK_BLOCKS)
    )
    # 4. long note, retriggers, velocity limits
    dw = (tuple(dflt.items()),)
    out.append(Case("long", "long-note/note60", 60, 100, dw, LONG_BLOCKS))
    for vel in (1, 127):
        out.append(
            Case(
                "retrigger",
                f"retrigger-every-{RETRIG_EVERY}/vel{vel}",
                60,
                vel,
                dw,
                RETRIG_BLOCKS,
                tuple(range(0, RETRIG_BLOCKS, RETRIG_EVERY)),
            )
        )
    # 5. a machine that plays a pool sample: the same shapes with a sample published (setup "sample-pool"), so the render path runs
    if "sample-pool" in sp.fixtures and sp.sample_records:
        out += _sample_cases(sp, dflt)
    return out


# The `sample` group. PLAY words are mode * 256 (the module README): REV 0, REV.L 1, FWD.L 2, FWD 3.
SAMPLE_RATE_HZ = 48000
SAMPLE_FRAMES = (
    16000  # 8000 packed words per plane: the two test planes are 0x8000 bytes apart
)
SAMPLE_PLAY = {"rev-loop": 1 << 8, "fwd-loop": 2 << 8, "fwd": 3 << 8}


def _sample_cases(sp: MachineSpec, dflt: dict[str, int]) -> list[Case]:
    by_id = {p.id: p for p in sp.params}
    if not {"sample", "start", "end", "play", "pitch"} <= by_id.keys():
        return []
    out: list[Case] = []

    def case(
        name: str,
        note: int,
        words: tuple[tuple[tuple[str, int], ...], ...],
        blocks: int,
        vel: int = 100,
        trig: tuple[int, ...] = (0,),
        channels: int = 2,
        frames: int = SAMPLE_FRAMES,
    ) -> None:
        out.append(
            Case(
                "sample",
                name,
                note,
                vel,
                words,
                blocks,
                trig,
                "sample-pool",
                (frames, SAMPLE_RATE_HZ, channels),
            )
        )

    def one(blocks_words: dict[str, int]) -> tuple[tuple[tuple[str, int], ...], ...]:
        # sample 0 (the pool index the worker publishes), the rest of the module's parameters at their defaults
        return (tuple({**dflt, "sample": 0, **blocks_words}.items()),)

    end = by_id["end"].hi
    fwd = {"start": 0, "end": end, "play": SAMPLE_PLAY["fwd"], "pitch": 0x4000}
    case("stereo-fwd-oneshot/note60", 60, one(fwd), 300)
    case("mono-fwd-oneshot/note60", 60, one(fwd), 300, channels=1)
    # frame = round(frames * word / 0x7f00); a block is 32 frames. END 2000 is frame 984: the cursor passes it after about 31 blocks, so the end ramp runs
    case("fwd-end-reached/note60", 60, one({**fwd, "end": 2000}), 120)
    # START 256..END 512 is frames 126..252: the cursor wraps every 4 blocks
    loop = {"start": 256, "end": 512}
    case(
        "fwd-loop/note60",
        60,
        one({**fwd, **loop, "play": SAMPLE_PLAY["fwd-loop"]}),
        300,
    )
    case(
        "rev-loop/note60",
        60,
        one({**fwd, **loop, "play": SAMPLE_PLAY["rev-loop"]}),
        300,
    )
    pitch = by_id["pitch"]
    case("pitch-min/note0", 0, one({**fwd, "pitch": pitch.lo}), 200)
    case("pitch-max/note127", 127, one({**fwd, "pitch": pitch.hi}), 200)
    for vel in (1, 127):
        case(
            f"retrigger-every-{RETRIG_EVERY}/vel{vel}",
            60,
            one(fwd),
            RETRIG_BLOCKS,
            vel,
            tuple(range(0, RETRIG_BLOCKS, RETRIG_EVERY)),
        )
    # the random walk of group 3, over every parameter but the sample index (a walk of it would leave the published pool entry)
    for note in (24, 100):
        rng = random.Random(0xC0FFEE + note)
        pos = {p.id: float(p.default) for p in sp.params}
        walk = []
        for _ in range(WALK_BLOCKS):
            row = [("sample", 0)]
            for p in sp.params:
                if p.id == "sample":
                    continue
                pos[p.id] += rng.uniform(-0.6, 0.6) * (p.hi - p.lo)
                if pos[p.id] < p.lo:
                    pos[p.id] = 2 * p.lo - pos[p.id]
                if pos[p.id] > p.hi:
                    pos[p.id] = 2 * p.hi - pos[p.id]
                pos[p.id] = min(max(pos[p.id], p.lo), p.hi)
                row.append((p.id, int(pos[p.id])))
            walk.append(tuple(row))
        case(f"random-walk/note{note}", note, tuple(walk), WALK_BLOCKS)
    return out


# ---- effects (the FX contract, <dk/fx/fx.h>) -------------------------------------------------------------------------------
# An effect module is a DSP object behind an `[[entry]] registry = "fx"` row. It is run on the concrete core through core/fx
# (tests/support/fx_rig.py) one block at a time; the cases and checks are those of tests/modules/test_fx_conformance.py.
FX_MAX = max(
    pk_device.limits(d).fx_slots for d in pk_device.digikit_devices()
)  # effects in the chain (devices/*.toml [limits] fx_slots, at most <dk/fx/fx.h> DK_FX_MAX)
# Instructions of core/fx around one process call at most (a fade block: copy, output check, crossfade; measured 855 with an effect of 210).
# The budget below is the effect's own.
FX_ADAPTER_MAX = 900
FX_PARAM_MAX = 0x7F00  # the range of a parameter word until the manifest declares one
FX_NEGATIVE = {"test/fx-nan": "guard stepped in", "test/dyn-fx-nan": "guard stepped in"}
# Fixtures of the runtime's fault tests that the suite does not run.
FX_SKIP = {
    "test/dyn-fx-hang": "stops the DSP on purpose after 5000 calls; modules/core/runtime/tests/fault-trace.device.test.ts runs it"
}


@dataclass(frozen=True)
class FxSpec:
    module: str
    moddir: Path
    sources: tuple[Path, ...]  # the DSP sources of the module
    process: str
    desc: str
    state: str  # "": a run-time effect (`DK_FX`), whose state the runtime reserves (the rig does the same)
    knobs: tuple[
        tuple[float, float], ...
    ] = ()  # (min, max) of the page knob that feeds parameter word i (a run-time effect with a page)

    @property
    def label(self) -> str:
        return f"{self.module}@fx"


def fx_word(spec: FxSpec, j: int, raw: int) -> int:
    """The parameter word j for the case value RAW (0 .. FX_PARAM_MAX): the raw integer, or for a knob of a page the float32 that the runtime
    sends, `min + (max - min) * raw / 0x7f00`."""
    if j >= len(spec.knobs):
        return raw
    lo, hi = spec.knobs[j]

    def f(x: float) -> float:
        return float(struct.unpack("<f", struct.pack("<f", x))[0])

    v = f(f(lo) + f(f(hi) - f(lo)) * f(f(raw) / f(FX_PARAM_MAX)))
    return int(struct.unpack("<I", struct.pack("<f", v))[0])


def fx_spec(mid: str) -> FxSpec | None:
    """The spec of effect module MID, or None when it has no `[[entry]] registry = "fx"` row (patched in) and no `DK_FX` record (run time) with a DSP object."""
    if mid.split("/")[0] == "core":
        return None
    moddir = modroots.module_dir(mid)
    man = _manifest(moddir)
    for e in man.get("entry", []):
        if e.get("registry") == "fx" and isinstance(e.get("pc"), str):
            sources = tuple(s for d in DEVICES for s in _dsp_sources(man, moddir, d))
            if not sources:
                return None
            return FxSpec(
                mid,
                moddir,
                tuple(dict.fromkeys(sources)),
                e["pc"],
                e["desc"],
                e["state"],
            )
    for fx in _fx_records(mid, man):
        if isinstance(fx.get("process"), str) and isinstance(fx.get("desc"), str):
            sources = tuple(s for d in DEVICES for s in _dsp_sources(man, moddir, d))
            if not sources:
                return None
            return FxSpec(
                mid,
                moddir,
                tuple(dict.fromkeys(sources)),
                fx["process"],
                fx["desc"],
                "",
                tuple((float(k["min"]), float(k["max"])) for k in fx.get("knob", [])),
            )
    return None


def fx_specs(negative: bool = False) -> list[FxSpec]:
    """Every effect module the FX suite runs (FX_NEGATIVE fixtures only with `negative`)."""
    out = []
    for mid in modroots.manifests():
        if mid in FX_SKIP or (mid in FX_NEGATIVE) != negative:
            continue
        s = fx_spec(mid)
        if s is not None:
            out.append(s)
    return out


def is_fx_module(mid: str) -> bool:
    return fx_spec(mid) is not None


def fx_budget() -> int:
    """Instructions of one process call: the headroom of the tighter device shared by FX_MAX effects (module.toml may not override yet)."""
    return _device_budgets()["fx_budget"]


@dataclass(frozen=True)
class FxCase:
    """A run on a fresh rig: `blocks` blocks of `signal`, block k with the parameter words `words(k)`; `toggle`: the effect is switched off
    and on every that many blocks (0: never)."""

    group: str
    name: str
    signal: str
    blocks: int
    words: tuple[tuple[int, ...], ...]  # per block (a single entry: every block)
    toggle: int = 0


def fx_cases(n_params: int) -> list[FxCase]:
    zero = (0,) * n_params
    top = (FX_PARAM_MAX,) * n_params
    mid = (FX_PARAM_MAX // 2 & ~0xFF,) * n_params
    out: list[FxCase] = []
    for sig in ("silence", "dc", "square", "noise", "impulse", "sine", "ramp"):
        out.append(FxCase("signals", sig, sig, 24, (zero,)))
    for tag, w in (("min", zero), ("max", top), ("mid", mid)):
        for sig in ("noise", "square"):
            out.append(FxCase("params", f"all-{tag}/{sig}", sig, 24, (w,)))
    for j in range(n_params):
        for tag, v in (("min", 0), ("max", FX_PARAM_MAX)):
            w = tuple(
                v if i == j else FX_PARAM_MAX // 2 & ~0xFF for i in range(n_params)
            )
            out.append(FxCase("params", f"p{j}={tag}/noise", "noise", 24, (w,)))
    rng = random.Random(0xFEED)
    walk = tuple(
        tuple(rng.randrange(0, FX_PARAM_MAX + 1) for _ in range(n_params))
        for _ in range(WALK_BLOCKS)
    )
    out.append(FxCase("modulation", "random-walk/noise", "noise", WALK_BLOCKS, walk))
    square = tuple(
        tuple(FX_PARAM_MAX if (k + i) % 2 else 0 for i in range(n_params))
        for k in range(WALK_BLOCKS)
    )
    out.append(
        FxCase("modulation", "square-every-block/noise", "noise", WALK_BLOCKS, square)
    )
    out.append(FxCase("switching", "toggle-every-7/noise", "noise", 300, (mid,), 7))
    out.append(FxCase("switching", "toggle-every-1/square", "square", 60, (mid,), 1))
    out.append(FxCase("long", "long/noise", "noise", LONG_BLOCKS, (mid,)))
    return out
