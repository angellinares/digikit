"""Conformance of a machine or effect module: what its manifest declares (the side of the module that needs no core).

A machine module (a `[[voice]]` or an `[[entry]] registry = "voices"` with a DSP object for a device) is run on the concrete SHARC+
core through the wrapper (core/dsp-voice) by tests/support/machine_rig.py (the adapter tests; the module conformance runs in
packages/sharc-rig-kit, which reads the same manifests in `spec.ts`). This file holds the side that needs no core:

  * `spec(module_id, device)`: the module's DSP sources, entry symbol, machine type, parameters (knob slots, min, max, default)
    and the frame layout of the device, all read from the manifests;
  * `budget(device, spec)`: the instruction budget of one voice-block (rule below; the numbers come from `patchkit budget`, packages/dkprof);

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

import json
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


# ---- effects (the FX contract, <dk/fx/fx.h>) -------------------------------------------------------------------------------
# An effect module is a run-time module (`DK_FX`) with a DSP object. It is run on the concrete core through core/fx
# (tests/support/fx_rig.py) one block at a time.
FX_MAX = max(
    pk_device.limits(d).fx_slots for d in pk_device.digikit_devices()
)  # effects in the chain (devices/*.toml [limits] fx_slots, at most <dk/fx/fx.h> DK_FX_MAX)


@dataclass(frozen=True)
class FxSpec:
    module: str
    moddir: Path
    sources: tuple[Path, ...]  # the DSP sources of the module
    process: str
    desc: str
    state: str  # always "": the runtime reserves the state of a run-time effect (the rig does the same)

    @property
    def label(self) -> str:
        return f"{self.module}@fx"


def fx_spec(mid: str) -> FxSpec | None:
    """The spec of effect module MID, or None when it has no `DK_FX` record (a run-time effect) with a DSP object."""
    if mid.split("/")[0] == "core":
        return None
    moddir = modroots.module_dir(mid)
    man = _manifest(moddir)
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
            )
    return None


def is_fx_module(mid: str) -> bool:
    return fx_spec(mid) is not None
