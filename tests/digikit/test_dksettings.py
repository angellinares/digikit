"""The settings record on the host (tools/device/dksettings.py) and the SETTINGS command of tools/device/dkupload.py, on the shared vectors
(modules/core/runtime/vectors/settings-vectors.json); `dkrt_sdk::settings` and `patchkit store dump` read the same file."""

import json
import struct
from pathlib import Path

import pytest

from tools.device import dkproto
from tools.device import dksettings as S
from tools.device import dkupload as D

VECTORS = json.loads(
    (
        Path(__file__).resolve().parents[2]
        / "modules"
        / "core"
        / "runtime"
        / "vectors"
        / "settings-vectors.json"
    ).read_text()
)


def test_the_limits_are_the_contract():
    assert VECTORS["area_bytes"] == S.AREA == dkproto.SETTINGS_SECTORS * 512
    assert VECTORS["key_max"] == S.KEY_MAX
    assert VECTORS["value_max"] == S.VALUE_MAX
    assert dkproto.SETTINGS_COPIES == 2
    assert S.kind_of(b"fx/route/core/fx") == "route"
    assert S.kind_of(b"fx/send/core/fx") == "u8x16"
    assert S.kind_of(b"fx/send/") == ""
    assert S.kind_of(b"other") == ""
    assert dkproto.CAPACITY_KINDS["SETTINGS"] == 13
    assert dkproto.CMD_SETTINGS == 0x19


@pytest.mark.parametrize("case", VECTORS["valid"], ids=lambda c: c["name"])
def test_valid_vectors_decode_and_encode_back(case):
    raw = bytes.fromhex(case["hex"])
    assert S.why(raw) == "ok"
    for padded in (raw, raw.ljust(S.AREA, b"\0")):
        r = S.decode(padded)
        assert r is not None
        assert (r.seq, r.used) == (case["seq"], case["used"])
        assert [(k.decode(), v.hex()) for k, v in r.entries] == [
            tuple(e) for e in case["entries"]
        ]
    entries = [(k.encode(), bytes.fromhex(v)) for k, v in case["entries"]]
    assert S.encode(case["seq"], entries) == raw[: case["used"]]


@pytest.mark.parametrize("case", VECTORS["invalid"], ids=lambda c: c["name"])
def test_invalid_vectors_are_refused(case):
    raw = bytes.fromhex(case["hex"])
    assert S.why(raw) == case["why"]
    assert S.decode(raw) is None
    assert S.decode(raw.ljust(S.AREA, b"\0")) is None


def test_pick_takes_the_valid_copy_with_the_higher_seq():
    a = S.encode(4, [(b"k", b"a")])
    b = S.encode(5, [(b"k", b"b")])
    assert S.pick(a, b) == 1
    assert S.pick(S.encode(5, []), b) == 0  # equal: A
    assert S.pick(a, b[:-1] + b"\xff") == 0  # B damaged
    assert S.pick(b"\0" * 512, b"\0" * 512) is None
    assert S.pick(None, b) == 1


def test_values_print_as_what_they_hold():
    amounts = bytes([0, 127, 64, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 100])
    assert S.decode_value("route", b"\x01")[0] == "send (1)"
    assert S.decode_value("route", b"\x00")[1] == "master"
    assert S.decode_value("route", b"\x07") == ("0x07", None)
    assert S.decode_value("u8x16", amounts)[1] == list(amounts)
    assert S.decode_value("u8x16", amounts[:3])[0] == "0x007f40"
    assert S.decode_value("", b"hello") == ('"hello"', "hello")
    r = S.decode(S.encode(2, [(b"fx/send/m", amounts)]))
    assert r is not None
    assert (
        S.lines(r)[1]
        == "    fx/send/m  [0, 127, 64, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 100]"
    )
    assert S.as_json(r)["entries"][0]["kind"] == "u8x16"


def test_the_settings_command_reads_the_record_in_pieces():
    from tools.firmware.build import encode_8in7

    rec = S.encode(
        9,
        [
            (b"fx/send/m", bytes(range(16))),
            (b"big", bytes(1000)),
            (b"fx/route/m", b"\x01"),
        ],
    )
    assert len(rec) > D.READ_MAX  # two pieces
    asked: list[tuple[int, int]] = []

    class Fake:
        def send(self, msg: bytes) -> None:
            self.msg = msg

        def recv(self) -> bytes:
            from tools.device.midisniff import unpack_8in7

            m = self.msg
            assert m[5] == D.CMD_SETTINGS == dkproto.CMD_SETTINGS
            off, ln = struct.unpack(">IH", unpack_8in7(m[7:-1]))
            asked.append((off, ln))
            body = (
                b"\x00"
                + struct.pack(">BBII", 1, 1, 9, len(rec))
                + rec[off : off + min(ln, D.READ_MAX)]
            )
            return m[:4] + bytes([1, m[5] | 0x40, m[6]]) + encode_8in7(body) + b"\xf7"

    flags, copy, seq, raw = D.Uploader(Fake()).settings_record()
    assert (flags, copy, seq) == (1, 1, 9)
    assert raw == rec
    assert asked == [(0, D.READ_MAX), (D.READ_MAX, D.READ_MAX)]
    assert S.decode(raw) is not None
