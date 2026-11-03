"""Run-time effects, the static side (F4 step 1; docs/design/digikit-dkm.md "DSP section", effect records): the fixture `test/dyn-fx-gain` packs into a `.dkm` whose
header says fx 1.0 and no machine, its DSP section (version 4) holds one effect record that the reference loader and the packer's own numbers agree on, and the needs
the file infers are one effect slot and no row of any other kind. The emulator proof is modules/core/adapters/dynfx/tests/fx-chain.device.test.ts (both devices)."""

import struct
import tomllib

import pytest
from support.paths import REPO_ROOT

ROOT = REPO_ROOT

from tools.device import dkneeds  # noqa: E402
from tools.patchkit import (  # noqa: E402
    modroots,
    pk_device,
    pk_dkrt,
    pk_dkrt_machine,
    pk_toolchains,
)
from tools.patchkit import patchkit as front_end  # noqa: E402

needs_selache = pytest.mark.skipif(
    pk_toolchains.selache_dir() is None, reason="SHARC+ assembler not installed"
)
FX = "test/dyn-fx-gain"


@pytest.fixture(scope="module")
def packed(tmp_path_factory):
    tools = pk_dkrt.Tools()
    out = tmp_path_factory.mktemp("dynfx") / "fx.dkm"
    tools.patchkit_run("pack", FX, "--device", "dt2", "-o", out)
    again = out.with_name("fx2.dkm")
    tools.patchkit_run("pack", FX, "--device", "dt2", "-o", again)
    assert out.read_bytes() == again.read_bytes()  # deterministic
    return tools, out


@needs_selache
@pytest.mark.parametrize("mod", ["core/dynfx", FX, "test/dyn-fx-gain-b"])
def test_committed_objects_are_current(mod):
    front_end.build_module(modroots.module_dir(mod), check=True)


def test_the_header_says_fx_and_not_machine(packed):
    _, out = packed
    b = out.read_bytes()
    sdk = tomllib.loads((ROOT / "sdk" / "sdk.toml").read_text())["contract"]
    # This fixture needs no newer API entries or sample-track table: the packer
    # declares its compatibility floor, rather than the latest SDK minor.
    assert tuple(b[0x38:0x40]) == (sdk["api"][0], 4, 0, 0, *sdk["fx"], 0, 0)
    assert sdk["fx"][0] != 0  # the runtime implements the contract


def effect_ids(dkm: bytes) -> list[int]:
    """The `id` word (the fourth) of each EFFECT record (kind 4) of the file's register section."""
    at = dkm.find(b"DKRG")
    assert at >= 0 and struct.unpack_from(">II", dkm, at)[1] == 1
    n = struct.unpack_from(">I", dkm, at + 12)[0]
    p, out = at + 16, []
    for _ in range(n):
        kind, length = struct.unpack_from(">II", dkm, p)
        if kind == 4:
            out.append(struct.unpack_from(">I", dkm, p + 8 + 12)[0])
        p += length
    return out


def fnv_effect_id(module: str, effect: str) -> int:
    """`register::effect_id` (patchkit): FNV-1a 32 of `<module id>:<effect id>`, the halves xor-ed, never 0."""
    h = 0x811C9DC5
    for b in f"{module}:{effect}".encode():
        h = ((h ^ b) * 0x01000193) & 0xFFFFFFFF
    return ((h >> 16) ^ (h & 0xFFFF)) or 1


def test_the_effect_record_carries_the_id_the_kit_stores(packed, tmp_path):
    # the ID is the packer's definition (module id and effect id, not the file's name); the runtime stores it with the values in a kit (finding 61 "Persistence")
    tools, out = packed
    ids = effect_ids(out.read_bytes())
    assert ids == [fnv_effect_id(FX, "gain")] and ids[0] == 0x4625
    renamed = tmp_path / "renamed.dkm"
    tools.patchkit_run(
        "pack", FX, "--device", "dt2", "--name", "other/name", "-o", renamed
    )
    assert effect_ids(renamed.read_bytes()) == ids
    other = tmp_path / "b.dkm"
    tools.patchkit_run("pack", "test/dyn-fx-gain-b", "--device", "dt2", "-o", other)
    assert effect_ids(other.read_bytes()) == [
        fnv_effect_id("test/dyn-fx-gain-b", "gain")
    ]
    assert effect_ids(other.read_bytes()) != ids


def test_the_inferred_needs_are_one_effect_slot_and_the_state_in_the_range(packed):
    _, out = packed
    b = out.read_bytes()
    n = dkneeds.needs_of(b)
    assert n["FX_SLOTS"] == 1
    assert (n["MACHINE_ROWS"], n["VOICE_ROWS"], n["PARAM_ROWS"], n["PAGE_ROWS"]) == (
        0,
        0,
        0,
        0,
    )
    sec = pk_dkrt_machine.parse_dsp_section(b)
    # image, bss and the state (4 bytes -> 8) rounded to the 64-byte granule: the packer's `range_len` and the file's needs agree
    want = (len(sec["image"]) + sec["bss"] + 7) & ~7
    want = (want + 8 + 63) & ~63
    assert n["DSP_AREA"] == want


def test_the_fit_rule_counts_the_effect_slots_of_the_device(packed):
    _, out = packed
    n = dkneeds.needs_of(out.read_bytes())
    slots = pk_device.limits("dt2").fx_slots
    assert slots == 3  # the Digitakt II profile: three run-time effects
    caps = {"FX_SLOTS": {"limit": slots, "used": 0, "largest": 0}}
    assert dkneeds.fit(caps, [n])["each"][0] is None
    caps["FX_SLOTS"]["used"] = slots
    short = dkneeds.fit(caps, [n])["each"][0]
    assert (short["resource"], short["need"], short["free"]) == ("FX_SLOTS", 1, 0)
    assert short["describe"] == "effect slots 1 needed, 0 free"
    assert short["text"] == "FX 1 > 0 FREE"
    # (a set of four effects stops at the fourth: the "sets" vectors of sdk/vectors/fit-vectors.json, tested in modules/core/runtime/fit)


def test_the_effect_packs_for_the_digitone_too(packed, tmp_path):
    tools, _ = packed
    out = tmp_path / "fx-dn2.dkm"
    r = tools.run(
        tools.patchkit,
        "--root",
        ROOT,
        "pack",
        FX,
        "--device",
        "dn2",
        "-o",
        out,
    )
    assert r.returncode == 0, r.stderr
    sec = pk_dkrt_machine.parse_dsp_section(out.read_bytes())
    assert sec["voices"] == [] and len(sec["fxs"]) == 1
