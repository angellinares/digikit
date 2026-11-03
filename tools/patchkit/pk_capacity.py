"""The module sets of the capacity tests (docs/plans/capacity-isolation.md): for each limit of the runtime, a set of synthetic modules
(`patchkit synth-dkm`, packages/patchkit/src/dkmsynth.rs) that fills the limit and a module that does not fit, with the result the loader
must give for each. Nothing here is a fixture folder and no number is a literal limit: the sets are computed from the device file
(`pk_device.limits`), `types.toml` and the address of the arena in the plan, so a device that sets another limit gets its own sets.

A result is a result code of `dkrt_load` (sdk/sdk.toml `[load.status]`) or `LOADED`: 0, or `LOADING` (1: the ColdFire part is in and the DSP
part follows; a ColdFire-only rig never completes it).

Why these sets reach each code (the loader's order of checks, modules/core/runtime/src/rs/src/loader.rs):

- The selection holds at most `SEL_MAX` (8) paths and there are `modules` (8) records, so `E_TABLE_FULL` cannot be reached: the ninth path
  is refused by SELECT (`E_SEL_COUNT`). `modules_case` loads all records.
- A hashed machine gets its type number from the boot's assignment; with all `machine_rows` numbers given out, the next machine has none and
  the registry refuses it with `E_TYPE_RANGE`, before the DSP checks run. `E_DSP_ROWS` and `E_DSP_TYPE` are reached by voices that carry a
  pinned type number (a DSP-only module): the row count is checked before the types.
- A ColdFire arena or DSP area is filled exactly by one module whose `bss` is the rest: `bss` is not stored in the file.
"""

from __future__ import annotations

import tomllib
from dataclasses import dataclass
from pathlib import Path

from tools.device import dkneeds
from tools.device.dkproto import ARENA_HDR, DKM_HEADER, GRANULE, MAX_VOICES
from tools.patchkit import pk_device

ROOT = Path(__file__).resolve().parents[2]

# result codes (sdk/sdk.toml [load.status])
E_FORMAT, E_NOMEM, E_LOADED, E_TOO_BIG = -5, -8, -10, -13
E_TYPE_RANGE, E_PARAM_FULL, E_PAGE_FULL = -22, -24, -25
E_DSP_FORMAT, E_DSP_NOSPACE, E_DSP_ROWS, E_DSP_TYPE = -31, -33, -34, -37
E_SEL_PATH, E_SEL_COUNT = -40, -41
LOADED = "loaded"

# the loader's format limits ([limits.format] of sdk/sdk.toml)
HDR = ARENA_HDR  # the arena's block header
DSP_GRANULE = GRANULE  # the DSP area is handed out in granules
CODE = 4  # the synthetic module's code: moveq #0,d0; rts


@dataclass(frozen=True)
class Mod:
    """One module of a set: its TOML spec for `synth-dkm` and the result the loader must give."""

    name: str
    spec: str
    want: int | str


@dataclass(frozen=True)
class Case:
    """A set loaded at one boot, in order."""

    name: str
    what: str
    mods: tuple[Mod, ...]


@dataclass(frozen=True)
class Geometry:
    """What the sets are computed from: the device's limits, its run-time machine type range and the arena's address."""

    dev: str
    arena_lo: int
    arena_hi: int
    dsp_area: int
    dsp_base: int
    stage_bytes: int
    arena_bytes: int
    modules: int
    machine_rows: int
    param_rows: int
    page_rows: int
    type_first: int

    @classmethod
    def of(cls, dev: str, arena_base: int, dsp_base: int) -> Geometry:
        """The geometry of device DEV; ARENA_BASE is the address of `DKRT_ARENA` in the plan (the runtime aligns it to 16 inside), DSP_BASE
        the L2 byte address of `_dl_area`, the start of the DSP area."""
        lim = pk_device.limits(dev)
        first, last = dyn_range(dev)
        if last - first + 1 != lim.machine_rows:
            raise ValueError(
                f"{dev}: machine_rows {lim.machine_rows} is not the run-time type range {first}..{last}"
            )
        return cls(
            dev=dev,
            arena_lo=(arena_base + 15) & ~15,
            arena_hi=(arena_base + lim.arena_bytes) & ~15,
            dsp_area=lim.dsp_area_bytes,
            dsp_base=dsp_base,
            stage_bytes=lim.stage_bytes,
            arena_bytes=lim.arena_bytes,
            modules=lim.modules,
            machine_rows=lim.machine_rows,
            param_rows=lim.param_rows,
            page_rows=lim.page_rows,
            type_first=first,
        )

    @property
    def dsp_first(self) -> int:
        """Offset in the area of the first 8-aligned byte address: the first range of a module with `align = 8` starts here (the area's
        base is not 8-aligned on the Digitakt II)."""
        return -self.dsp_base % 8

    @property
    def dsp_tail(self) -> int:
        """Bytes of the area behind the first module R (one granule from `dsp_first`) that the next module can use: whole granules."""
        return (
            (self.dsp_area - self.dsp_first - DSP_GRANULE) // DSP_GRANULE * DSP_GRANULE
        )

    @property
    def arena_free(self) -> int:
        """Bytes of the arena's one free block at the start (header included)."""
        return self.arena_hi - self.arena_lo


def dyn_range(dev: str) -> tuple[int, int]:
    """`dynamic_first ..= last` of the device in core/machines types.toml: the run-time machine type numbers."""
    t = tomllib.loads((ROOT / "modules/core/adapters/machines/types.toml").read_text())[
        dev
    ]
    return t["dynamic_first"], t["last"]


def block(n: int) -> int:
    """The arena bytes a module of image + bss = N takes: its payload rounded to 16 and the header."""
    return ((n + 15) & ~15) + HDR


def spec(
    name: str,
    *,
    pad: int = 0,
    bss: int = 0,
    fill: int | None = None,
    machines: int = 0,
    params: int = 0,
    pages: int = 0,
    oversize: bool = False,
    dsp: dict | None = None,
) -> str:
    """The TOML text of a `synth-dkm` spec. DSP is `{image, bss, align, voices: [{type | machine, state_bytes, voices}]}`."""
    lines = [f'name = "{name}"']
    for k, v in (
        ("pad", pad),
        ("bss", bss),
        ("machines", machines),
        ("params", params),
        ("pages", pages),
    ):
        if v:
            lines.append(f"{k} = {v}")
    if fill is not None:
        lines.append(f"fill = {fill}")
    if oversize:
        lines.append("oversize = true")
    if dsp is not None:
        lines += ["[dsp]", f"image = {dsp.get('image', 64)}"]
        if dsp.get("bss"):
            lines.append(f"bss = {dsp['bss']}")
        if dsp.get("align"):
            lines.append(f"align = {dsp['align']}")
        for v in dsp.get("voices", []):
            lines.append("[[dsp.voice]]")
            for k in ("type", "machine"):
                if k in v:
                    lines.append(f"{k} = {v[k]!r}".replace("'", '"'))
            for k in ("state_bytes", "voices"):
                if k in v:
                    lines.append(f"{k} = {v[k]}")
    return "\n".join(lines) + "\n"


def pinned(types: list[int]) -> list[dict]:
    """Voices that name machine types by number (a DSP-only module: no machine record, no hashed key)."""
    return [{"type": t} for t in types]


def stage_file_pad(stage_bytes: int, extra: int = 0) -> int:
    """`pad` of a raw module whose file is `stage_bytes + extra` bytes (the file is the header, the code and the pad)."""
    return stage_bytes + extra - DKM_HEADER - CODE


def modules_case(g: Geometry) -> Case:
    """Every module record in use: `modules` raw modules with distinct names."""
    mods = tuple(
        Mod(f"cap/mod{i}", spec(f"cap/mod{i}"), LOADED) for i in range(g.modules)
    )
    return Case("modules", f"{g.modules} raw modules fill the module table", mods)


def files_case(g: Geometry) -> Case:
    """The staging buffer and the name check: a file of exactly `stage_bytes` loads, one of 4 bytes more is `E_TOO_BIG`; a second
    module of a loaded module's name is `E_LOADED`."""
    pad = stage_file_pad(g.stage_bytes)
    return Case(
        "files",
        "a file of the staging buffer's size loads, one byte over is too big, a repeated name is refused",
        (
            Mod("cap/f-first", spec("cap/f-first"), LOADED),
            Mod("cap/f-first", spec("cap/f-first", pad=4), E_LOADED),
            Mod("cap/f-exact", spec("cap/f-exact", pad=pad, fill=0xA5), LOADED),
            Mod(
                "cap/f-big",
                spec("cap/f-big", pad=stage_file_pad(g.stage_bytes, 4), oversize=True),
                E_TOO_BIG,
            ),
        ),
    )


def arena_case(g: Geometry) -> Case:
    """The ColdFire arena filled exactly (R takes the smallest block, F the rest), then four modules that do not fit: a raw one (X), one
    with a DSP section (Y: refused where the arena block for its DSP image fails), one with a `bss` as large as the arena (Z) and one
    whose `bss` is above the arena (W: a format error, not `E_NOMEM`). Raw modules only in the fill: a DSP module's image copy stays
    in the arena until the DSP transfer ends, and a ColdFire-only rig never ends it."""
    r_total = CODE
    f_total = (
        g.arena_free - block(r_total) - HDR
    )  # the block of F is `f_total` rounded up and the header: it takes the rest
    return Case(
        "arena",
        "the arena is filled to the last byte; a raw module, a DSP module and a huge bss are refused with E_NOMEM, a bss above the arena with E_FORMAT",
        (
            Mod("cap/a-r", spec("cap/a-r"), LOADED),
            Mod("cap/a-f", spec("cap/a-f", bss=f_total - CODE), LOADED),
            Mod("cap/a-x", spec("cap/a-x"), E_NOMEM),
            Mod("cap/a-y", spec("cap/a-y", dsp={"image": 64}), E_NOMEM),
            Mod("cap/a-z", spec("cap/a-z", bss=g.arena_bytes - 16), E_NOMEM),
            Mod("cap/a-w", spec("cap/a-w", bss=g.arena_bytes + 4), E_FORMAT),
        ),
    )


def dsp_area_case(g: Geometry) -> Case:
    """The DSP area filled as far as its granules go: R takes the first granule (from `dsp_first`, the first 8-aligned byte), A every whole
    granule behind it; less than a granule is left (the free figure counts it, but no module fits). Then B (one granule) does not fit
    (`dsp::reserve`) and C is larger than the whole area (`dsp::check`). DSP sections without voices: the area is the only limit in play."""
    return Case(
        "dsp-area",
        "the DSP area is filled to its last whole granule; a module that does not fit is E_DSP_NOSPACE, at reserve and at check",
        (
            Mod("cap/d-r", spec("cap/d-r", dsp={"image": 64}), LOADED),
            Mod(
                "cap/d-a",
                spec("cap/d-a", dsp={"image": 64, "bss": g.dsp_tail - 64}),
                LOADED,
            ),
            Mod("cap/d-b", spec("cap/d-b", dsp={"image": 64}), E_DSP_NOSPACE),
            Mod(
                "cap/d-c",
                spec("cap/d-c", dsp={"image": 64, "bss": g.dsp_area - 56}),
                E_DSP_NOSPACE,
            ),
        ),
    )


def rows_cases(g: Geometry) -> list[Case]:
    """The voice rows (`machine_rows`): one module with all rows, then one more voice; the same split over two modules; and the type
    checks while rows are free."""
    n = g.machine_rows
    types = list(range(g.type_first, g.type_first + n))
    if n > MAX_VOICES:
        raise ValueError(
            f"{n} voice rows: a DSP section holds {MAX_VOICES} voices, the sets need more modules"
        )
    out_of_range = g.type_first + n + 16  # a number no run-time machine has

    def one(name: str, ts: list[int], want: int | str) -> Mod:
        return Mod(name, spec(name, dsp={"image": 64, "voices": pinned(ts)}), want)

    return [
        Case(
            "rows-all",
            "one module takes every voice row; one more voice is E_DSP_ROWS (the row count is checked before the type)",
            (
                one("cap/r-all", types, LOADED),
                one("cap/r-b", [types[0]], E_DSP_ROWS),
                one("cap/r-c", [out_of_range], E_DSP_ROWS),
            ),
        ),
        Case(
            "rows-split",
            "the rows split over two modules; a third module with one voice is E_DSP_ROWS",
            (
                one("cap/s-a", types[:-1], LOADED),
                one("cap/s-b", types[-1:], LOADED),
                one("cap/s-c", [types[0]], E_DSP_ROWS),
            ),
        ),
        Case(
            "rows-types",
            "with rows free: a type used by a loaded module and a type outside the run-time range are E_DSP_TYPE; more voices than a section holds is E_DSP_FORMAT",
            (
                one("cap/t-p", [types[0]], LOADED),
                one("cap/t-q", [types[0]], E_DSP_TYPE),
                one("cap/t-r", [out_of_range], E_DSP_TYPE),
                Mod(
                    "cap/t-s",
                    spec(
                        "cap/t-s",
                        dsp={
                            "image": 64,
                            "voices": pinned([types[0]] * (MAX_VOICES + 1)),
                        },
                    ),
                    E_DSP_FORMAT,
                ),
            ),
        ),
    ]


def registry_cases(g: Geometry) -> list[Case]:
    """The machine types (every number given out: the next machine is `E_TYPE_RANGE`), the parameter rows (every row taken: the next
    parameter is `E_PARAM_FULL`) and, on a device that has them, the page rows (`E_PAGE_FULL`)."""
    out = [
        Case(
            "machines",
            "every machine type and parameter row is taken by one module; one more machine is E_TYPE_RANGE and one more parameter E_PARAM_FULL",
            (
                Mod(
                    "cap/m-a",
                    spec("cap/m-a", machines=g.machine_rows, params=g.param_rows),
                    LOADED,
                ),
                Mod("cap/m-b", spec("cap/m-b", machines=1), E_TYPE_RANGE),
                Mod("cap/m-c", spec("cap/m-c", params=1), E_PARAM_FULL),
            ),
        )
    ]
    if g.page_rows:
        out.append(
            Case(
                "pages",
                "every page row is taken by one module; one more page is E_PAGE_FULL",
                (
                    Mod(
                        "cap/p-a",
                        spec("cap/p-a", machines=1, pages=g.page_rows),
                        LOADED,
                    ),
                    Mod("cap/p-b", spec("cap/p-b", machines=1, pages=1), E_PAGE_FULL),
                ),
            )
        )
    return out


def cases(g: Geometry) -> list[Case]:
    """Every case of device G, in the order the report lists them."""
    return [
        modules_case(g),
        files_case(g),
        arena_case(g),
        dsp_area_case(g),
        *rows_cases(g),
        *registry_cases(g),
    ]


# ---- the full set: every limit used at once, behind a reference module --------------------------------------------------------


@dataclass(frozen=True)
class Usage:
    """What a packed module takes of each resource: the needs `dkneeds` infers from its file (docs/design/digikit-dkm.md; the `inferred needs`
    tests of `capacity.device.test.ts` prove they are what the loader takes)."""

    name: str
    arena: int  # the module's arena block
    arena_xfer: int  # the arena block of its relocated DSP image (held until the transfer ends); 0: no DSP section
    dsp_len: int  # bytes of the DSP range (a multiple of the granule); 0: none
    dsp_align: int
    rows: int  # DSP voice rows
    machines: int
    params: int
    pages: int


def usage(dkm: bytes) -> Usage:
    """The resources a packed module takes, as `patchkit needs` infers them from the file (one implementation, Rust: packages/dkneeds)."""
    n = dkneeds.needs_of(dkm)
    return Usage(
        dkm[0x40:0x60].split(b"\0")[0].decode(),
        n["ARENA"],
        n["ARENA_XFER"],
        n["DSP_AREA"],
        n["DSP_ALIGN"],
        n["VOICE_ROWS"],
        n["MACHINE_ROWS"],
        n["PARAM_ROWS"],
        n["PAGE_ROWS"],
    )


def full_set(g: Geometry, refs: list[Usage]) -> list[Mod]:
    """The modules that, behind the reference modules REFS (loaded first), use every limit at once and are followed by three that do not fit:

    M takes the machine types, parameter rows and page rows the references leave; V the voice rows (pinned types, behind the references'
    hashed ones); D the DSP area, to its last whole granule; F the arena, to the last byte (the references' and V's and D's blocks and
    the arena copies of their DSP images counted: a ColdFire-only rig keeps those until a transfer that never comes). Then X (a raw module:
    `E_NOMEM`), Y (a voice: `E_DSP_ROWS`) and Z (a DSP module: `E_DSP_NOSPACE`)."""
    n_total = len(refs) + 7
    if n_total > g.modules:
        raise ValueError(
            f"the full set needs {n_total} modules, the device has {g.modules}"
        )
    types_left = g.machine_rows - sum(r.machines for r in refs)
    rows_left = g.machine_rows - sum(r.rows for r in refs)
    params_left = g.param_rows - sum(r.params for r in refs)
    pages_left = g.page_rows - sum(r.pages for r in refs)
    if rows_left > MAX_VOICES or rows_left < 1 or types_left < 1:
        raise ValueError(
            f"the references leave {rows_left} rows and {types_left} types"
        )
    if pages_left > types_left:
        raise ValueError(
            f"{pages_left} page rows left, but only {types_left} machines to put them on"
        )
    first_pinned = g.type_first + sum(r.rows for r in refs)
    mods = [
        Mod(
            "cap/full-m",
            spec(
                "cap/full-m", machines=types_left, params=params_left, pages=pages_left
            ),
            LOADED,
        ),
        Mod(
            "cap/full-v",
            spec(
                "cap/full-v",
                dsp={
                    "image": 64,
                    "voices": pinned(
                        list(range(first_pinned, first_pinned + rows_left))
                    ),
                },
            ),
            LOADED,
        ),
    ]
    # the DSP area: references, then V (one granule), then D takes every whole granule left (first fit from the area's start, each range
    # at the first address its alignment allows)
    cursor = 0
    for length, align in [(r.dsp_len, r.dsp_align) for r in refs if r.dsp_len] + [
        (DSP_GRANULE, 8)
    ]:
        start = (g.dsp_base + cursor + align - 1) // align * align - g.dsp_base
        cursor = start + length
    d_start = (g.dsp_base + cursor + 7) // 8 * 8 - g.dsp_base
    d_len = (g.dsp_area - d_start) // DSP_GRANULE * DSP_GRANULE
    if d_len < DSP_GRANULE:
        raise ValueError("the references leave no DSP area for D")
    mods.append(
        Mod(
            "cap/full-d",
            spec("cap/full-d", dsp={"image": 64, "bss": d_len - 64}),
            LOADED,
        )
    )
    # every raw block is `block(CODE)`; V's and D's DSP images keep an arena copy of `block(64)` each (as does every reference's)
    used = sum(r.arena + r.arena_xfer for r in refs)
    used += 3 * block(CODE) + 2 * block(64)
    f_total = g.arena_free - used - HDR
    if f_total < CODE:
        raise ValueError("the references leave no arena for F")
    mods += [
        Mod("cap/full-f", spec("cap/full-f", bss=f_total - CODE), LOADED),
        Mod("cap/full-x", spec("cap/full-x"), E_NOMEM),
        Mod(
            "cap/full-y",
            spec("cap/full-y", dsp={"image": 64, "voices": pinned([first_pinned])}),
            E_DSP_ROWS,
        ),
        Mod("cap/full-z", spec("cap/full-z", dsp={"image": 64}), E_DSP_NOSPACE),
    ]
    return mods
