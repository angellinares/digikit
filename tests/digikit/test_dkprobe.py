"""tools/device/dkprobe.py: frame reassembly, placing records in runs, the JSON table, compare, and the capture loop against a fake collector. No emulator."""

import struct

from tools.device import dkprobe as P
from tools.device import dkprobe_seq as S

CAP = 24576
ITEMS = S.items()


def model(inp: int) -> tuple[int, int, int]:
    """A stand-in for the core: (result, ASTAT after, STKY after)."""
    return (
        (inp * 0x9E3779B1 ^ 0x1234) & 0xFFFFFFFF,
        0x75000000 | (inp & 3),
        (inp >> 3) & 1,
    )


def payload(g: int, tweak: int = 0) -> list[int]:
    """The 12 payload words of global frame g (frames count from 0; a run is frames_per_run frames; run counters from 1)."""
    run, pos = g // S.frames_per_run() + 1, g % S.frames_per_run()
    out: list[int] = []
    for k in range(S.RECORDS_PER_FRAME):
        r = pos * S.RECORDS_PER_FRAME + k
        if r == 0:
            out += [run, len(ITEMS), S.table_checksum()]
        elif r == 1:
            out += [0x12345, 0x75000000, 0]
        elif r < 2 + len(ITEMS):
            res, a, s = model(ITEMS[r - 2][1])
            out += [res ^ tweak, a, s]
        else:
            out += [S.PAD] * 3
    return out


def stream(first: int, nwords: int, tweak: int = 0) -> list[int]:
    """The words of frames first, first + 1, ... cut to nwords, as a capture that starts at a frame boundary."""
    ws: list[int] = []
    g = first
    while len(ws) < nwords:
        pl = payload(g, tweak)
        hdr = S.header_word(g)
        ws += [hdr, *pl, S.fletcher([hdr, *pl])]
        g += 1
    return ws[:nwords]


def test_a_window_that_starts_inside_a_run_gives_every_item_once_placed_right():
    ws = [0xDEAD, 0xB00C0000] + stream(
        900, CAP
    )  # start in the middle of a run, with junk before the first frame
    fr = P.reassemble(ws)
    assert fr.seq_errors == 0 and fr.bad_checksum == 0
    tab = P.build_table(fr)
    assert len(tab.items) == len(ITEMS) and tab.table_checksum_ok
    for i in (0, 1, 4623, 4624, len(ITEMS) - 1):
        it = tab.items[i]
        assert (it.input, (it.result, it.astat, it.stky)) == (
            ITEMS[i][1],
            model(ITEMS[i][1]),
        )
        assert it.op == ITEMS[i][0]
    assert tab.runs_seen == [2] and tab.conflicts == []
    assert tab.run_info == {2: {"mode1": 0x12345, "astat": 0x75000000, "stky": 0}}
    # run 1's tail (frames 900 on) sits before run 2's header: placed backwards, so item 0 of run 1 is not invented
    assert all(it.index < len(ITEMS) for it in tab.items.values())


def test_a_dropped_and_a_repeated_word_are_counted_and_the_rest_is_kept():
    ws = stream(0, CAP)
    del ws[5000]
    ws.insert(12000, ws[12000])
    fr = P.reassemble(ws)
    assert fr.bad_checksum >= 2 and fr.seq_errors >= 2
    tab = P.build_table(fr)
    assert (
        0 < len(tab.items) < len(ITEMS)
    )  # the window starts at run 1's header and the damaged frames leave holes


def test_two_runs_that_disagree_are_listed():
    a = stream(S.frames_per_run(), 3 * S.FRAME_WORDS)  # frames 0 to 2 of run 2 ...
    b = stream(
        2 * S.frames_per_run(), 3 * S.FRAME_WORDS, tweak=0xFF
    )  # ... and of run 3, every result different
    tab = P.build_table(P.reassemble(a + b))
    assert tab.runs_seen == [2, 3]
    assert (
        tab.conflicts == list(range(10))
    )  # frame 0 holds records 0 to 3 (header, state, items 0 and 1); frames 1 and 2 items 2 to 9
    assert len(tab.items) == 10


class FakeEmu:
    def __init__(self) -> None:
        self.steps = 0

    def step(self, n: int) -> None:
        self.steps += n


class FakeUp:
    """The collector's three commands over a ring that is full from the start."""

    def __init__(self, ring: list[int], fill_after: int = 2) -> None:
        self.ring, self.calls, self.fill_after = ring, 0, fill_after

    def request(self, cmd: int, body: bytes = b"") -> tuple[int, bytes]:
        if cmd == P.CMD_RESET:
            return 0, b""
        if cmd == P.CMD_STATUS:
            self.calls += 1
            full = self.calls > self.fill_after
            n = len(self.ring) if full else 100
            fr = P.reassemble(self.ring) if full else P.Frames()
            vals = [
                2,
                1,
                1,
                0,
                0,
                0,
                n,
                0,
                CAP,
                n if full else 0,
                len(fr.frames),
                fr.seq_errors,
                fr.bad_checksum,
                0,
                0,
                1,
                0x2C49D0,
            ]
            return 0, struct.pack(">17I", *vals)
        first, count = struct.unpack(">IH", body)
        w = self.ring[first : first + count]
        return 0, struct.pack(">II", len(self.ring), first) + struct.pack(
            f">{len(w)}I", *w
        )

    def info(self) -> dict:
        return {"image_id": "abc", "runtime": 1283}

    def modules(self) -> dict:
        return {"modules": [{"name": "test/sharc-probe"}]}

    def ls(self) -> list[tuple[str, int, str]]:
        return [("modules/a.dkm", 10, "ff" * 32), ("settings", 1, "00" * 32)]


def test_capture_waits_for_a_full_ring_reads_it_and_writes_a_complete_table():
    emu = FakeEmu()
    doc = P.capture(FakeUp(stream(300, CAP)), emu, step=1000)
    assert emu.steps >= 2000
    h = doc["host"]
    assert (
        h["complete"]
        and h["items_present"] == len(ITEMS)
        and h["items_missing"] == 0
        and h["seq_errors"] == 0
        and h["checksum_errors"] == 0
    )
    assert doc["collector"]["frames_ok"] == h["frames"] and doc["identity"][
        "store"
    ] == [{"path": "modules/a.dkm", "size": 10, "hash": "ff" * 32}]
    assert doc["items_table"][0]["input"] == f"0x{ITEMS[0][1]:08x}" and len(
        doc["items_table"]
    ) == len(ITEMS)


def test_compare_counts_the_fields_that_differ():
    a = P.capture(FakeUp(stream(300, CAP)), FakeEmu(), step=1)
    b = P.capture(FakeUp(stream(300, CAP)), FakeEmu(), step=1)
    assert P.compare(a, b)["differing_items"] == 0
    b["items_table"][5]["result"] = "0x00000001"
    b["items_table"][6]["astat"] = "0x00000000"
    b["items_table"][6]["stky"] = "0x00000008"
    del b["items_table"][-1]
    c = P.compare(a, b)
    assert c["differing_items"] == 2 and c["field_counts"] == {
        "result": 1,
        "astat": 1,
        "stky": 1,
    }
    assert c["only_a"] == [len(ITEMS) - 1] and c["same_sequence"]
    assert c["per_op"] == {"recips": 2}


def test_status_needs_the_whole_reply():
    import pytest

    with pytest.raises(P.ProbeError):
        P.parse_status(bytes(10))
    assert P.parse_status(struct.pack(">16I", *range(16)))["synced"] == 15
