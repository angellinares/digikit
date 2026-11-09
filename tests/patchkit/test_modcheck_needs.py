"""`module:check` prints what a load of the packed module takes (inferred from the file by `patchkit needs`, tools/device/dkneeds.py) and whether it fits
an empty device (tools/patchkit/pk_modcheck.py). The needs come from a real packed module; the stub of `dkneeds` in the verdict tests only sets numbers."""

import pytest

from tests.support import synth_spec as P
from tools.device import dkneeds
from tools.patchkit import pk_device, pk_modcheck


def needs(**n):
    """All the fields of `patchkit needs`, 0 unless given."""
    names = (
        "ARENA",
        "ARENA_XFER",
        "DSP_AREA",
        "DSP_ALIGN",
        "MACHINE_ROWS",
        "VOICE_ROWS",
        "PARAM_ROWS",
        "PAGE_ROWS",
        "FX_SLOTS",
    )
    return {"file_size": 600, **dict.fromkeys(names, 0), **n}


def test_a_module_that_fits_and_one_that_does_not(monkeypatch):
    stub = {}
    monkeypatch.setattr(dkneeds, "needs_of", lambda data: stub[data])
    stub[b"ok"] = needs(ARENA=48, DSP_AREA=4096, DSP_ALIGN=8, VOICE_ROWS=1)
    ok = pk_modcheck.needs_lines(b"ok", "dt2")
    assert ok[0] == "dt2: needs    CPU memory 48, DSP memory 4096, voices 1"
    assert ok[1] == "dt2: fits     fits an empty device"
    big = pk_device.limits("dt2").dsp_area_bytes + 64
    stub[b"big"] = needs(ARENA=48, DSP_AREA=big, DSP_ALIGN=8, VOICE_ROWS=1)
    no = pk_modcheck.needs_lines(b"big", "dt2")
    assert no[1].startswith(
        "dt2: fits     DOES NOT FIT even an empty device: DSP memory "
    )
    # a module that takes nothing but its code says so
    stub[b"none"] = needs()
    assert (
        pk_modcheck.needs_lines(b"none", "dt2")[0]
        == "dt2: needs    nothing but its code"
    )


def test_a_file_the_loader_refuses_has_no_needs():
    lines = pk_modcheck.needs_lines(b"not a module", "dt2")
    assert lines[0].startswith("dt2: needs    none: not a .dkm")
    assert "the loader refuses the file" in lines[1]


def test_the_lines_of_a_real_packed_module(tmp_path):
    from tools.patchkit import patchkit as PK

    spec = tmp_path / "m.toml"
    spec.write_text(
        P.spec("chk/m", dsp={"image": 64, "bss": 4032, "voices": [{"type": 16}]})
    )
    out = tmp_path / "m.dkm"
    assert (
        PK.run(["synth-dkm", str(spec), "--device", "dt2", "-o", str(out)]).returncode
        == 0
    )
    lines = pk_modcheck.needs_lines(out.read_bytes(), "dt2")
    assert "DSP memory 4096" in lines[0] and "voices 1" in lines[0]
    assert lines[1] == "dt2: fits     fits an empty device"
    # the same fields the command line reports
    assert dkneeds.needs_of(out.read_bytes())["DSP_AREA"] == 4096


def test_dkneeds_needs_many_matches_needs_of(tmp_path):
    from tools.patchkit import patchkit as PK

    files = []
    for i, bss in enumerate((64, 4032, 8128)):
        spec = tmp_path / f"{i}.toml"
        spec.write_text(
            P.spec(
                f"chk/m{i}", dsp={"image": 64, "bss": bss, "voices": [{"type": 16 + i}]}
            )
        )
        out = tmp_path / f"{i}.dkm"
        assert (
            PK.run(
                ["synth-dkm", str(spec), "--device", "dt2", "-o", str(out)]
            ).returncode
            == 0
        )
        files.append(out.read_bytes())
    many = dkneeds.needs_many([*files, b"junk"])
    assert many[:3] == [dkneeds.needs_of(f) for f in files]
    assert many[3] is None
    assert [m["DSP_AREA"] for m in many[:3]] == [128, 4096, 8192]
    with pytest.raises(dkneeds.NeedsError, match="not a .dkm"):
        dkneeds.needs_of(b"junk")
    assert dkneeds.needs_many([]) == []
