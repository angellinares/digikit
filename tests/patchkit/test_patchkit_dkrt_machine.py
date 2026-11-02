"""Run-time machines with DSP code (core/dyndsp, the `.dkm` DSP section, the runtime's DSP loader; finding 32).

Static checks: the committed objects are current, the DSP section is deterministic, its relocation agrees in three places
(the Rust reference loader behind `patchkit dkm-dsp-image`, this file's own, and the loader's checks), and the two sides of the
DSP area's size agree. The emulator proofs are the boot-time equivalence scenarios (equiv-dyn-* in tests/digikit and modules/core/adapters/dynreg/tests/equiv.device.test.ts: a selected
module against the patched-in one, PCM sample for sample); the hot-load proof that was here is archived."""

import json
import re
import tomllib

import pytest
from support.paths import REPO_ROOT

ROOT = REPO_ROOT

from tools.device.dklog_kinds import RUNTIME_VERSION  # noqa: E402
from tools.patchkit import (  # noqa: E402
    modroots,
    pk_dkrt,
    pk_dkrt_machine,
    pk_toolchains,
)
from tools.patchkit import patchkit as front_end  # noqa: E402

MODS = ROOT / "modules"
needs_selache = pytest.mark.skipif(
    pk_toolchains.selache_dir() is None, reason="SHARC+ assembler not installed"
)


@needs_selache
@pytest.mark.parametrize(
    "mod",
    ["core/dsp-link", "core/dyndsp", "test/sine"],
)
def test_committed_objects_are_current(mod):
    front_end.build_module(modroots.module_dir(mod), check=True)


def test_the_area_size_is_the_same_on_both_sides():
    # One source: `[digikit] dsp_area_bytes` of the device file, generated into both sides by `patchkit gen`.
    files = {"dt2": "digitakt-ii.toml", "dn2": "digitone-ii.toml"}
    link_dir = modroots.module_dir("core/dsp-link")
    dyn_dir = modroots.module_dir("core/dyndsp")
    for dev, name in files.items():
        want = tomllib.loads((ROOT / "devices" / name).read_text())["digikit"][
            "dsp_area_bytes"
        ]
        inc = (link_dir / f"build/gen/device-{dev}.inc").read_text()
        words = int(re.search(r"#define DL_AREA_WORDS (\d+)", inc).group(1))
        nbytes = int(re.search(r"#define DL_AREA_BYTES (\d+)", inc).group(1))
        assert nbytes == want and words * 4 == nbytes, dev
        link = (link_dir / f"src/dsp/{dev}.s").read_text()
        assert re.search(r"\.PKBSS _dl_area, DL_AREA_BYTES;", link), dev
        cf = (dyn_dir / f"build/gen/device-{dev}.inc").read_text()
        assert (
            int(re.search(r"\.equ DL_AREA_BYTES, (0x[0-9a-f]+)", cf).group(1), 16)
            == want
        ), dev
        info = (dyn_dir / f"src/cf/dyndsp-{dev}.s").read_text()
        assert re.search(r"\.long\s+DL_AREA_BYTES\b", info), dev
        assert nbytes <= 4 << 17  # the 17-bit offset of a chunk reaches 512 KiB


def test_dsp_section_is_deterministic_and_relocates_like_the_reference(tmp_path):
    tools = pk_dkrt.Tools()
    a, b = tmp_path / "a.dkm", tmp_path / "b.dkm"
    for out in (a, b):
        tools.patchkit_run("pack", "test/sine", "--device", "dt2", "-o", out)
    assert a.read_bytes() == b.read_bytes()
    info = tools.patchkit_run("dkm-info", a)
    # the voice names its machine by key: the 24-bit hash of "test/sine:sine" above a zero type byte
    h = pk_dkrt_machine.machine_hash("test/sine", "sine")
    assert (
        f"voice type h{h:06x} entry +0x0 note_on +0x14a state 16x8 clear 8" in info
        and "imports _dv_exp2" in info
    )
    # the guard's ranges: LEVEL (knob 1) declares 0 .. 0x7f00; TUNE and LEV are stock rows (undeclared)
    assert "ranges [0, 7f000000, 0, 0, 0, 0, 0, 0]" in info
    assert (
        f"runtime   >= {'.'.join(map(str, RUNTIME_VERSION))}" in info
    )  # DSP sections of version 2 (voices with knob ranges) need runtime 5
    sec = pk_dkrt_machine.parse_dsp_section(a.read_bytes())
    imports = {"_dv_exp2": 0xB9D7E0, "_dv_frame_hw": 0xB9D80E}
    for base in (0x2000_0000, 0x2001_AE80, 0x2003_F000):
        want = pk_dkrt_machine.relocate(sec, base, imports)
        out = tools.patchkit_run(
            "dkm-dsp-image", a, f"{base:x}", *[f"{k}={v:x}" for k, v in imports.items()]
        )
        assert bytes.fromhex(out.strip()) == want, hex(base)
    # a placement that is not 8-aligned is refused by the reference loader
    r = tools.run(tools.patchkit, "--root", ROOT, "dkm-dsp-image", a, "2000004")
    assert r.returncode != 0


def test_runtime_watch_env_preserves_dma_holes_and_sequential_mode(
    tmp_path, monkeypatch
):
    from tools.coldfire import arena_safety

    plan = tmp_path / "runtime.plan.json"
    plan.write_text(
        json.dumps(
            {
                "base": {"device": "dt2", "version": "1.17"},
                "regions": {"cf.expansion": [0x180]},
                "symbols": {"core/runtime@0.1.0:DKRT_STAGE": 0x20000},
            }
        )
    )
    monkeypatch.setattr(arena_safety, "expansion", lambda _image: (0x1000, 0x80000))
    env = pk_dkrt_machine.runtime_watch_env(
        plan, tmp_path / "watch.jsonl", sequential=True
    )
    assert env["DIGI_WRITE_WATCH"] == "0x1000-0x20000,0x30000-0x80000"
    assert env["DIGI_WRITE_WATCH_ALLOW_PC"] == "0x1000-0x1180"
    assert env["DIGI_WRITE_WATCH_AFTER"] == "cacr" and env["DIGI_FLUSH_CHECK"] == "1"
    assert env["DIGI_WRITE_WATCH_SEQ"] == "1"
    assert "DIGI_WRITE_WATCH_SEQ" not in pk_dkrt_machine.runtime_watch_env(
        plan, tmp_path / "watch.jsonl"
    )
