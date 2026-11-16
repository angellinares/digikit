"""Run-time effects, the static side (F4 step 1; docs/design/digikit-dkm.md "DSP section", effect records): the fixture `test/dyn-fx-gain` packs into a `.dkm` whose
header says fx 1.0 and no machine, its DSP section (version 4) holds one effect record that the reference loader and the packer's own numbers agree on, and the needs
the file infers are one effect slot and no row of any other kind. The emulator proof is modules/core/adapters/dynfx/tests/fx-chain.device.test.ts (both devices)."""

import re
import struct
import tomllib

import pytest
from support.paths import REPO_ROOT

ROOT = REPO_ROOT

from tools.device import dkneeds, dkproto  # noqa: E402
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
@pytest.mark.parametrize("mod", ["core/dynfx", FX])
def test_committed_objects_are_current(mod):
    front_end.build_module(modroots.module_dir(mod), check=True)


def test_the_header_has_no_contract_pairs(packed):
    _, out = packed
    b = out.read_bytes()
    # 0x38..0x40 are reserved zero; the word at 0x30 is the newest `since` among the imports the fixture links (the builder calls of its init),
    # packed major << 16 | minor << 8 | patch, with the trace flag in the top bit
    assert bytes(b[0x38:0x40]) == bytes(8)
    since = {
        i["name"]: i["since"]
        for i in tomllib.loads((ROOT / "sdk/sdk.toml").read_text())["import"]
        if "since" in i
    }
    source = (ROOT / "tests/fixtures/modules/dyn-fx-gain/src/module.c").read_text()
    linked = {n for n in re.findall(r"\b(dk_\w+)\(", source) if n in since}
    assert linked == {"dk_module_ctx_add_fx", "dk_fx_add_page", "dk_page_add_fx_param"}
    want = max(
        int(a) << 16 | int(b_) << 8 | int(c)
        for a, b_, c in (since[n].split(".") for n in linked)
    )
    # ... and at least the version that introduced the header table (`[runtime] table`): a module of the 0.7 format needs another minor
    a, b_, c = tomllib.loads((ROOT / "sdk/sdk.toml").read_text())["runtime"][
        "table"
    ].split(".")
    want = max(want, int(a) << 16 | int(b_) << 8 | int(c))
    assert int.from_bytes(b[0x30:0x34], "big") & 0x7FFFFFFF == want


def effect_ids(dkm: bytes) -> list[int]:
    """The effect ids of the file's header table (the header's 0x90 pair): the keys of the effect kind in the KEYS entry (tag 3)."""
    off, size = struct.unpack_from(">II", dkm, 0x90)
    magic, version, entries = struct.unpack_from(">IHH", dkm, off)
    assert magic == dkproto.TABLE_MAGIC
    assert version == 1  # an ordinary module stays table version 1
    ids: list[int] = []
    at = off + 8
    for _ in range(entries):
        tag, count, length = struct.unpack_from(">HHI", dkm, at)
        if tag == 3:
            g = at + 8
            for _ in range(count):
                kind, n = struct.unpack_from(">II", dkm, g)
                if kind == dkproto.RECORD_KIND["EFFECT"]:
                    ids += struct.unpack_from(f">{n}I", dkm, g + 8)
                g += 8 + 4 * n
        at += 8 + length
    assert at == off + size
    return ids


def fnv_effect_id(module: str, effect: str) -> int:
    """`register::effect_id` (patchkit): FNV-1a 32 of `<packed module name>:<effect id>`, the halves xor-ed, never 0."""
    h = 0x811C9DC5
    for b in f"{module}:{effect}".encode():
        h = ((h ^ b) * 0x01000193) & 0xFFFFFFFF
    return ((h >> 16) ^ (h & 0xFFFF)) or 1


def test_the_effect_record_carries_the_id_the_kit_stores(packed, tmp_path):
    # the ID is the packer's definition (the packed name and the effect id: an alias made with --name has ids of its own); the runtime stores it with the values in a kit (finding 61 "Persistence")
    tools, out = packed
    ids = effect_ids(out.read_bytes())
    assert ids == [fnv_effect_id(FX, "gain")] and ids[0] == 0x4625
    renamed = tmp_path / "renamed.dkm"
    tools.patchkit_run(
        "pack", FX, "--device", "dt2", "--name", "other/name", "-o", renamed
    )
    assert (
        effect_ids(renamed.read_bytes()) == [fnv_effect_id("other/name", "gain")] != ids
    )


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
    # (a set of four effects stops at the fourth: the "sets" vectors of modules/core/runtime/vectors/fit-vectors.json, tested in modules/core/runtime/fit)


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
