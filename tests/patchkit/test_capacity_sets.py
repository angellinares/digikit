"""The module sets of the capacity tests (tools/patchkit/pk_capacity.py) pack, and their sizes add up to the limits they claim to fill.
No emulator: the `memaudit-*-full` scenarios boot the full set (tests/digikit)."""

import dataclasses
import re

import pytest

from tools.patchkit import pk_capacity as P
from tools.patchkit import pk_device, pk_dkrt

DEVICES = pk_device.digikit_devices()


def geometry(dev, arena_base=0x47E00004, dsp_base=0x2090_0004):
    return P.Geometry.of(dev, arena_base, dsp_base)


@pytest.fixture(scope="module")
def tools():
    return pk_dkrt.Tools()


def pack_all(tools, tmp_path, dev, g):
    """Every module of every case packed; returns {case: [(Mod, file size, `dkm-info` text)]}."""
    out = {}
    for case in P.cases(g):
        rows = []
        for i, m in enumerate(case.mods):
            spec = tmp_path / f"{case.name}-{i}.toml"
            spec.write_text(m.spec)
            dkm = tmp_path / f"{case.name}-{i}.dkm"
            tools.patchkit_run("synth-dkm", spec, "--device", dev, "-o", dkm)
            info = ""
            # `dkm-info` refuses a file over the staging buffer and a DSP section of too many voices, as the loader does
            if len(dkm.read_bytes()) <= 0x10000 and m.want != P.E_DSP_FORMAT:
                info = tools.patchkit_run("dkm-info", dkm)
            rows.append((m, len(dkm.read_bytes()), info))
        out[case.name] = rows
    return out


def image_bss(info):
    m = re.search(r"image\s+(\d+) bytes, bss (\d+)", info)
    return int(m.group(1)) + int(m.group(2))


@pytest.mark.parametrize("dev", DEVICES)
def test_every_set_packs_and_its_sizes_are_the_limits(dev, tools, tmp_path):
    g = geometry(dev)
    packed = pack_all(tools, tmp_path, dev, g)
    assert set(packed) >= {
        "modules",
        "files",
        "arena",
        "dsp-area",
        "rows-all",
        "rows-split",
        "rows-types",
        "machines",
    }
    assert ("pages" in packed) == (g.page_rows > 0)
    # the module table: as many distinct names as records
    assert (
        len(packed["modules"])
        == g.modules
        == len({m.name for m, _, _ in packed["modules"]})
    )
    # the staging buffer: one file of exactly its size, one 4 bytes over
    sizes = [n for _, n, _ in packed["files"]]
    assert sizes[2] == g.stage_bytes and sizes[3] == g.stage_bytes + 4
    # the arena: the first two modules take every byte, in blocks of 16 plus the header
    arena = packed["arena"]
    blocks = [P.block(image_bss(info)) for _, _, info in arena[:2]]
    assert sum(blocks) == g.arena_free, (blocks, g.arena_free)
    assert (
        all(m.want == P.E_NOMEM for m, _, _ in arena[2:5])
        and arena[5][0].want == P.E_FORMAT
    )
    assert image_bss(arena[5][2]) > g.arena_bytes >= image_bss(arena[4][2]) - 16
    # the DSP area: R and A lie back to back from the first 8-aligned byte, less than a granule is left behind A
    end = g.dsp_first + P.DSP_GRANULE + g.dsp_tail
    assert end <= g.dsp_area < end + P.DSP_GRANULE and g.dsp_tail % P.DSP_GRANULE == 0
    assert all("dsp" in info for _, _, info in packed["dsp-area"])


@pytest.mark.parametrize("dev", DEVICES)
def test_the_sets_follow_the_profile_not_a_literal(dev):
    g = geometry(dev)
    small = dataclasses.replace(
        g,
        arena_hi=g.arena_lo + 0x4000,
        dsp_area=0x1000,
        dsp_base=0x2090_0000,
        modules=3,
    )
    cases = {c.name: c for c in P.cases(small)}
    assert len(cases["modules"].mods) == 3
    # the arena fill follows the arena's size: R and F are the whole of it
    arena = cases["arena"].mods
    f_bss = int(re.search(r"bss = (\d+)", arena[1].spec).group(1))
    assert P.block(P.CODE) + P.block(P.CODE + f_bss) == 0x4000
    # a DSP area of 4 KiB from an 8-aligned base: R at 0, A takes the rest
    assert small.dsp_first == 0 and small.dsp_tail == 0x1000 - 64
    # a profile whose rows exceed a DSP section's voices needs another set
    with pytest.raises(ValueError, match="voice rows"):
        P.rows_cases(dataclasses.replace(g, machine_rows=9))


def test_the_geometry_refuses_a_row_count_that_is_not_the_type_range(monkeypatch):
    monkeypatch.setattr(P, "dyn_range", lambda dev: (16, 25))
    with pytest.raises(ValueError, match="machine_rows"):
        P.Geometry.of("dt2", 0x47E00000, 0x2090_0000)
