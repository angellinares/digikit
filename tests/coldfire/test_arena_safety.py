"""Arena and card-store safety tooling (tools/coldfire/arena_safety.py, panel_script.py; finding 25).

The helpers are checked without firmware. The firmware proofs (MIDI DIN input, the USB callback, one project save with
the write-watch on cf.expansion) need Digitakt_II_OS1.16.syx in firmware/ and a built regions_probe, and are slow.
"""

from __future__ import annotations

import json
import struct

import pytest

from tools.coldfire import arena_safety as a
from tools.coldfire import panel_script as ps


def _overlay(*sectors: int) -> bytes:
    return b"".join(struct.pack("<Q", s) + bytes(64 + 512) for s in sectors)


def test_card_store_hits_names_sectors_in_the_module_store_only():
    lo, hi = a.card_store(
        "dt2"
    )  # devices/*.toml [card.store]: the area differs per device
    ov = _overlay(0, lo - 1, lo, hi - 1, hi, 0x58000)
    assert a.card_store_hits(ov, "dt2") == [lo, hi - 1]
    assert a.card_store_hits(_overlay(0, 0x2000, 0x58000), "dt2") == []
    assert a.card_store_hits(a.parse_overlay(ov), "dt2", hi, hi + 0x1000) == [hi]
    # the DN2's area is another one: the DT2's first sector is inside it, the other way round is not
    dlo, dhi = a.card_store("dn2")
    assert (dlo, dhi) != (lo, hi) and dlo < lo < dhi
    assert a.card_store_hits(_overlay(lo, dlo - 1), "dn2") == [lo]


def test_card_store_hits_reads_a_file(tmp_path):
    lo, _ = a.card_store("dt2")
    f = tmp_path / "overlay.bin"
    f.write_bytes(_overlay(lo + 1))
    assert a.card_store_hits(f, "dt2") == [lo + 1]
    assert a.card_store_hits(str(f), "dt2", 0, 1) == []


def test_parse_overlay_refuses_a_torn_file():
    with pytest.raises(ValueError):
        a.parse_overlay(_overlay(1)[:-1])


def test_sector_ranges_and_project_slots():
    assert a.sector_ranges([5, 1, 2, 3, 9]) == [(1, 3), (5, 5), (9, 9)]
    assert a.sector_ranges([]) == []
    slot = lambda n: 0x58000 + 0x8000 * n  # noqa: E731
    assert a.project_slots([0, 0x50000, slot(0), slot(0) + 0x7FFF, slot(3)]) == [0, 3]
    assert a.project_slots([slot(128)]) == []


def test_expansion_regions_come_from_the_port_files():
    assert a.expansion("dt2-1.17") == (0x47E8B000, 0x47FE0000)
    assert a.expansion("dn2-1.12") == (0x4671A000, 0x47FE0000)


def test_script_builds_probe_arguments_in_order(tmp_path):
    s = ps.Script(t0=100)
    s.tap(12, hold=10, gap=5).chord(17, 8, hold=2, gap=1).turn(3, -2, gap=4).midi(
        b"\x90\x3c\x64"
    ).usb(b"\xf0\xf7")
    s.frame("shot")
    args = s.args(frame_dir=tmp_path, tail=50)
    assert args[:2] == ["--extra", str(s.t + 50)]
    events = [args[i + 1] for i, x in enumerate(args) if x == "--event"]
    assert events[:2] == ["100:12:down", "110:12:up"]
    assert "--turn" in args and args[args.index("--turn") + 1].endswith(":3:-2")
    assert any(x.endswith(":903c64") for x in args)
    assert any(x.endswith(":f0f7") for x in args)
    frame = args[args.index("--frame") + 1]
    assert frame.endswith("shot.raw") and frame.startswith("%d:" % s.t)
    assert "--frame" not in s.args(tail=1)


def test_split_sysex_ignores_bytes_outside_messages():
    stream = bytes.fromhex("90 3c f0 01 02 f7 f8 f0 05 f7 f0 09")
    assert ps.split_sysex(stream) == [
        bytes.fromhex("f00102f7"),
        bytes.fromhex("f005f7"),
    ]


def test_every_script_builds_and_its_expectations_name_known_keys():
    keys = {"project_slots", "frames", "usb_replies", "din_bytes", "pcs"}
    for name, fn in ps.SCRIPTS.items():
        script = fn()
        assert script.args(frame_dir="x")[0] == "--extra", name
        assert set(ps.EXPECT[name]) <= keys, name
        for frame in ps.EXPECT.get(name, {}).get("frames", []):
            assert frame in [n for n, _ in script.frames], (name, frame)


def test_regions_scenarios_include_the_panel_scripts():
    from tools.coldfire import regions

    args = regions.scenario_args("save_project")
    assert args[0] == "--extra" and "--event" in args
    assert regions.scenario_args("boot") == ["--extra", "300000000"]


def test_check_reached_judges_slots_replies_and_frames():
    facts = {
        "project_slots_touched": [0],
        "midi": {"status": {"rx_irqs": 28}},
        "pc_hits": {"0x4011d1f6": 28},
        "frame_sha256": {"save_ok": "aa"},
        "usb_reply_types": [1, 2, 3, 5, 6, 9],
    }
    assert (
        a.check_reached(
            "save_project", "dt2-1.16", facts, {"dt2-1.16/save_project/save_ok": "aa"}
        )
        == []
    )
    bad = a.check_reached(
        "save_project", "dt2-1.16", facts, {"dt2-1.16/save_project/save_ok": "bb"}
    )
    assert bad == ["frame save_ok differs from the golden"]
    facts["project_slots_touched"] = []
    assert "project slots" in a.check_reached("save_project", "dt2-1.16", facts, {})[0]
    assert a.check_reached("din_midi", "dt2-1.16", facts, {}) == []
    assert a.check_reached("midirpc_info", "dt2-1.16", facts, {}) == []
    facts["usb_reply_types"] = [1]
    assert a.check_reached("midirpc_info", "dt2-1.16", facts, {})


def test_golden_file_covers_every_expected_frame_of_the_checked_images():
    golden = json.loads(a.GOLDEN.read_text()) if a.GOLDEN.exists() else {}
    for image in ("dt2-1.16",):
        for name, expect in ps.EXPECT.items():
            if name.startswith("digikit_"):
                continue  # scripts for a patched image: pinned in tests/golden/digikit_menu_frames.json
            for frame in expect.get("frames", []):
                key = "%s/%s/%s" % (image, name, frame)
                assert key in golden, key


def test_static_check_finds_absolute_references_in_both_views(monkeypatch):
    from tools.coldfire import regions

    # move.l #$47e8b000,d0 (cached view) and move.l #$4fe8b010,d1 (uncached alias), then a sector immediate and an unrelated one
    code = bytes.fromhex("203c47e8b000223c4fe8b010243c02df1001263c00001234") + bytes(8)
    monkeypatch.setattr(regions, "load_main", lambda image: code)
    r = a.static_check("dt2-1.17", with_cf=False)
    assert [h["target"] for h in r["views"]["cached"]["hits"]] == [0x47E8B000]
    assert [h["target"] for h in r["views"]["uncached"]["hits"]] == [0x4FE8B010]
    assert [h["target"] for h in r["store"]["store_sector"]["hits"]] == [0x2DF1001]
    assert "0x47e8b000" in a._static_report(r)


def test_midirpc_write_script_expects_replies_per_device():
    s = ps.SCRIPTS["midirpc_write"]()
    assert s.args(frame_dir="x")[0] == "--extra"
    assert ps.EXPECT["midirpc_write"]["usb_replies"]["dt2"][0] == 0x40
    assert "midirpc_write" in a.SUITE


def test_device_rpc_request_is_8_in_7_packed():
    """Finding 42 [C]: the body after `10 00` is packed; a Ping is unchanged, a payload with bytes >= 0x80 round-trips."""
    from tools.device import devrpc
    from tools.device.midisniff import unpack_8in7

    assert devrpc.build_request(0x01, seq=1) == bytes.fromhex(
        "f000203c1000000001000001f7"
    )
    payload = bytes(range(0x7C, 0x8C)) + b"/path\x00"
    frame = devrpc.build_request(0x42, seq=0x0123, payload=payload)
    assert frame[:6] == devrpc.HDR and frame[-1] == 0xF7 and max(frame[1:-1]) < 0x80
    assert unpack_8in7(frame[6:-1]) == bytes((0x01, 0x23, 0, 0, 0x42)) + payload


def test_device_rpc_reply_is_unpacked_and_the_response_bit_dropped():
    from tools.device import devrpc
    from tools.firmware.build import encode_8in7

    body = bytes((0, 7, 0, 7, 0x81)) + bytes(range(0x7E, 0x86))
    reply = devrpc.HDR + encode_8in7(body) + b"\xf7"
    assert devrpc.parse_reply(reply) == (0x01, bytes(range(0x7E, 0x86)))
