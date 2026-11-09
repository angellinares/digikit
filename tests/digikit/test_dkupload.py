"""The host side of module upload (tools/device/dkupload.py): the framing of docs/design/digikit-upload.md, no emulator."""

import pytest

from tools.device import dkupload as D


def test_the_packing_examples_of_the_spec():
    # seven bytes with the first high: `80 01 02 03 04 05 06` packs to `40 00 01 02 03 04 05 06`; `FF` packs to `40 7F`
    msg = D.frame(D.CMD_DATA, 5, bytes.fromhex("80010203040506"))
    assert msg == bytes.fromhex(
        "f07d646b01 11 05 40 00010203040506 f7".replace(" ", "")
    )
    assert D.frame(D.CMD_INFO, 1, b"\xff")[7:-1] == bytes.fromhex("407f")


def test_a_reply_parses_and_foreign_messages_do_not():
    body = bytes([0xC5, 0x12, 0x80])  # status -59 (E_NOFILE), then two bytes
    from tools.firmware.build import encode_8in7

    reply = (
        bytes.fromhex("f07d646b01")
        + bytes([D.CMD_DELETE | 0x40, 9])
        + encode_8in7(body)
        + b"\xf7"
    )
    assert D.parse_reply(reply) == (D.CMD_DELETE, 9, -59, bytes([0x12, 0x80]))
    assert D.parse_reply(D.frame(D.CMD_INFO, 1)) is None  # a request is not a reply
    assert (
        D.parse_reply(bytes.fromhex("f000203c10000100f7")) is None
    )  # the stock MidiRpc


def test_split_messages_takes_complete_ones_and_keeps_the_rest():
    stream = bytearray(b"\x90\x01\x02\xf0\x01\xf7\xf0\x02\x03")
    assert D.split_messages(stream) == [b"\xf0\x01\xf7"]
    assert bytes(stream) == b"\xf0\x02\x03"


def test_a_put_is_begin_data_end_with_two_in_flight():
    sent: list[tuple[int, int]] = []

    class Fake:
        def __init__(self):
            self.queue: list[bytes] = []

        def send(self, msg: bytes) -> None:
            from tools.firmware.build import encode_8in7

            cmd, seq = msg[5], msg[6]
            sent.append((cmd, len(self.queue)))
            self.queue.append(
                msg[:4] + bytes([1, cmd | 0x40, seq]) + encode_8in7(b"\x00") + b"\xf7"
            )
            if cmd == D.CMD_END or cmd == D.CMD_BEGIN:
                self.queue = self.queue[-1:]

        def recv(self) -> bytes:
            return self.queue.pop(0)

    up = D.Uploader(Fake(), chunk=512)
    up._file_hash_ok = True
    up.put("a.bin", bytes(512 * 5 + 7))
    cmds = [c for c, _ in sent]
    assert cmds == [D.CMD_BEGIN] + [D.CMD_DATA] * 6 + [D.CMD_END]
    # at most two DATA requests were outstanding when one was sent
    assert max(n for c, n in sent if c == D.CMD_DATA) <= 1
    with pytest.raises(ValueError):
        D.Uploader(Fake(), chunk=100)


def test_select_and_restart_bodies_and_frames():
    # SELECT (runtime 4): u8 n, then each path and a NUL
    assert D.select_body([]) == b"\x00"
    body = D.select_body(["modules/a.dkm", "modules/b.dkm"])
    assert body == b"\x02modules/a.dkm\0modules/b.dkm\0"
    with pytest.raises(ValueError):
        D.select_body(["x"] * 9)
    msg = D.frame(D.CMD_SELECT, 3, body)
    assert msg[:7] == bytes.fromhex("f07d646b01 30 03".replace(" ", ""))
    from tools.device.midisniff import unpack_8in7

    assert unpack_8in7(msg[7:-1]) == body
    assert D.frame(D.CMD_RESTART, 4) == bytes.fromhex(
        "f07d646b01 31 04 f7".replace(" ", "")
    )
    assert D.STATUS[-40] == "E_SEL_PATH" and D.STATUS[-47] == "E_CARD_RO"


def test_uploader_select_and_restart_send_the_commands():
    from tools.firmware.build import encode_8in7

    seen: list[int] = []

    class Fake:
        def __init__(self):
            self.q: list[bytes] = []

        def send(self, msg: bytes) -> None:
            from tools.device.midisniff import unpack_8in7

            seen.append((msg[5], unpack_8in7(msg[7:-1])))
            self.q.append(
                msg[:4]
                + bytes([1, msg[5] | 0x40, msg[6]])
                + encode_8in7(b"\x00")
                + b"\xf7"
            )

        def recv(self) -> bytes:
            return self.q.pop(0)

    up = D.Uploader(Fake())
    up.select(["modules/a.dkm"])
    up.restart()
    assert seen == [(D.CMD_SELECT, b"\x01modules/a.dkm\0"), (D.CMD_RESTART, b"")]


def _foreign_replies(count: int):
    from tools.firmware.build import encode_8in7

    class Fake:
        def __init__(self):
            self.q: list[bytes] = []

        def send(self, msg: bytes) -> None:
            # replies to another host's requests (other seq), then ours
            for i in range(count):
                self.q.append(
                    msg[:4]
                    + bytes([1, msg[5] | 0x40, (msg[6] + 1 + i) & 0x7F])
                    + encode_8in7(b"\x00")
                    + b"\xf7"
                )
            self.q.append(
                msg[:4]
                + bytes([1, msg[5] | 0x40, msg[6]])
                + encode_8in7(b"\x00")
                + b"\xf7"
            )

        def recv(self) -> bytes:
            return self.q.pop(0)

    return Fake()


def test_skip_foreign_drops_replies_to_other_hosts_up_to_a_bound():
    D.Uploader(_foreign_replies(3), skip_foreign=True).restart()
    with pytest.raises(D.UploadError):
        D.Uploader(_foreign_replies(3)).restart()
    with pytest.raises(D.UploadError):
        D.Uploader(_foreign_replies(64), skip_foreign=True).restart()


def _info_of(runtime: int, tail: bytes) -> dict:
    import struct

    from tools.firmware.build import encode_8in7

    body = (
        b"\x00"
        + struct.pack(">HH", 1, runtime & 0xFFFF)
        + b"dt2\0"
        + struct.pack(">IIB", 1024, 32768, 0)
        + tail
    )

    class Fake:
        def send(self, msg: bytes) -> None:
            self.msg = msg

        def recv(self) -> bytes:
            m = self.msg
            return m[:4] + bytes([1, m[5] | 0x40, m[6]]) + encode_8in7(body) + b"\xf7"

    return D.Uploader(Fake()).info()


def test_info_short_and_long_replies_and_has_cap():
    import struct

    log = struct.pack(">II", 3, 9)
    old = _info_of(4, log)
    assert old["caps"] is None and old["runtime"] == 4 and old["log_newest"] == 9
    assert D.has_cap(old, "SELECT") and D.has_cap(old, "PEEK")
    assert not D.has_cap(old, "LOGCFG") and not D.has_cap(old, "MODULES")
    assert D.has_cap(_info_of(0x100, log), "LOGCFG")
    assert not D.has_cap(_info_of(2, b""), "LOG")
    caps = (1 << D.CAP["LOG"]) | (1 << D.CAP["MODULES"])
    new = _info_of(0x0002, log + struct.pack(">II", 0x000200, caps))
    assert new["runtime"] == 0x000200 and new["caps"] == caps
    assert D.has_cap(new, "MODULES") and not D.has_cap(new, "PEEK")
    # runtime 0.5.2 appends the image id after the capability bits; an older reply has none
    assert old["image_id"] is None and new["image_id"] is None
    with_id = _info_of(
        0x0002, log + struct.pack(">II", 0x000502, caps) + bytes(range(1, 9))
    )
    assert with_id["image_id"] == "0102030405060708" and with_id["caps"] == caps


def test_modules_pages_by_start_index_and_decodes_the_records():
    """MODULES (protocol command 3): the host asks again from the last index + 1 until it has `total` records."""
    import struct

    from tools.firmware.build import encode_8in7

    def record(index: int, name: str, types: list[int], faults: int, off: int) -> bytes:
        return (
            bytes([index])
            + name.encode().ljust(32, b"\0")
            + struct.pack(">I", 0x010203)
            + bytes([1, 1, 1, 0, 0, 0, 1, 0])
            + struct.pack(
                ">IIIII",
                0x1000 + index,
                0x40,
                off,
                0x100 if off else 0,
                1 << index if off else 0,
            )
            + bytes([len(types), *types])
            + struct.pack(">I", faults)
            + bytes([1 if faults >= 8 else 0])
        )

    recs = [record(1, "a/one", [16], 0, 0x40), record(3, "b/two", [17, 18], 8, 0)]
    starts: list[int] = []

    class Fake:
        def __init__(self):
            self.queue: list[bytes] = []

        def send(self, msg: bytes) -> None:
            from tools.device.midisniff import unpack_8in7

            cmd, seq = msg[5], msg[6]
            assert cmd == D.CMD_MODULES
            start = unpack_8in7(msg[7:-1])[0]
            starts.append(start)
            # one record a reply, so that the host has to page
            mine = [r for r in recs if r[0] >= start][:1]
            body = (
                struct.pack(">IIII", 100, 90, 50, 40)
                + bytes([len(recs), len(mine)])
                + b"".join(mine)
            )
            self.queue.append(
                msg[:4]
                + bytes([1, cmd | 0x40, seq])
                + encode_8in7(b"\x00" + body)
                + b"\xf7"
            )

        def recv(self) -> bytes:
            return self.queue.pop(0)

    m = D.Uploader(Fake()).modules()
    assert starts == [0, 2]
    assert (
        m["arena_free"],
        m["arena_largest"],
        m["dsp_free"],
        m["dsp_largest"],
        m["total"],
    ) == (100, 90, 50, 40, 2)
    a, b = m["modules"]
    assert (
        a["index"],
        a["name"],
        a["version"],
        a["types"],
        a["faults"],
        a["guard_off"],
    ) == (1, "a/one", 0x010203, [16], 0, False)
    assert (
        a["contracts"] == [(1, 1), (1, 0), (0, 0), (1, 0)]
        and a["dsp_off"] == 0x40
        and a["rows"] == 2
    )
    assert (b["index"], b["types"], b["faults"], b["guard_off"], b["dsp_len"]) == (
        3,
        [17, 18],
        8,
        True,
        0,
    )
    lines = D.modules_lines(m)
    assert (
        lines[0].startswith("arena free 100")
        and "types 17,18" in lines[2]
        and "GUARD OFF" in lines[2]
    )


def test_capacity_decodes_the_entries_by_kind_name_and_prints_them():
    """CAPACITY (protocol command 4): `u8 version, u8 n`, then n entries of u8 kind, u32 limit, u32 used, u32 largest; a kind the tool does not
    know is `kind<N>`; the `info` command prints one line each."""
    import struct

    from tools.device.midisniff import unpack_8in7
    from tools.firmware.build import encode_8in7

    k = D.dkproto.CAPACITY_KINDS
    entries = [
        (k["MODULES"], 8, 3, 0),
        (k["ARENA"], 524288, 4096, 520000),
        (99, 1, 2, 3),
        # a 19 GiB store: KiB fit a u32 where bytes (INFO's free_bytes, saturated at 2^32 - 1) do not
        (k["STORE"], 20_000_000, 1024, 0),
    ]

    class Fake:
        def __init__(self):
            self.queue: list[bytes] = []

        def send(self, msg: bytes) -> None:
            cmd, seq = msg[5], msg[6]
            assert cmd == D.CMD_CAPACITY == 0x04 and unpack_8in7(msg[7:-1]) == b""
            body = bytes([1, len(entries)]) + b"".join(
                struct.pack(">BIII", *e) for e in entries
            )
            self.queue.append(
                msg[:4]
                + bytes([1, cmd | 0x40, seq])
                + encode_8in7(b"\x00" + body)
                + b"\xf7"
            )

        def recv(self) -> bytes:
            return self.queue.pop(0)

    cap = D.Uploader(Fake()).capacity()
    assert cap == {
        "MODULES": {"limit": 8, "used": 3, "largest": 0},
        "ARENA": {"limit": 524288, "used": 4096, "largest": 520000},
        "kind99": {"limit": 1, "used": 2, "largest": 3},
        "STORE": {"limit": 20_000_000, "used": 1024, "largest": 0},
    }
    assert cap["STORE"]["limit"] * 1024 > 2**32
    assert D.capacity_lines(cap) == [
        "capacity modules: 3/8",
        "capacity arena: 4096/524288  largest free 520000",
        "capacity kind99: 2/1  largest free 3",
        "capacity store: 1024/20000000 KiB",
    ]
    assert D.CAP["CAPACITY"] == 6


def _read_fake(files: dict[str, bytes]):
    """A device that answers READ as the runtime does: the size, then at most 768 bytes from the offset; E_NOFILE, E_LENGTH, E_OFFSET."""
    import struct

    from tools.device.midisniff import unpack_8in7
    from tools.firmware.build import encode_8in7

    seen: list[bytes] = []

    class Fake:
        def send(self, msg: bytes) -> None:
            self.msg = msg
            seen.append(unpack_8in7(msg[7:-1]))

        def recv(self) -> bytes:
            m = self.msg
            body = seen[-1]
            path, tail = body.split(b"\0", 1)
            off, n = struct.unpack(">II", tail)
            f = files.get(path.decode())
            if f is None:
                out = bytes([0xC5])  # -59
            elif n == 0:
                out = bytes([0xC9])  # -55
            elif off >= len(f):
                out = bytes([0xCA])  # -54
            else:
                out = b"\0" + struct.pack(">I", len(f)) + f[off : off + min(n, 768)]
            return m[:4] + bytes([1, m[5] | 0x40, m[6]]) + encode_8in7(out) + b"\xf7"

    return D.Uploader(Fake()), seen


def test_read_request_and_reply_encoding():
    data = bytes(range(256)) * 10  # 2560 bytes
    up, seen = _read_fake({"modules/a.dkm": data})
    assert D.frame(D.CMD_READ, 2)[5] == 0x16
    assert up.read("modules/a.dkm", 5, 20) == (2560, data[5:25])
    assert seen[-1] == b"modules/a.dkm\0" + bytes.fromhex("00000005 00000014")
    assert up.header("modules/a.dkm") == data[:0xA0]
    assert (
        up.read("modules/a.dkm", 0, 5000)[1] == data[:768]
    )  # the device caps a reply at 768 bytes
    assert (
        up.read("modules/a.dkm", 2500, 100)[1] == data[2500:]
    )  # and stops at the end of the file
    assert up.read_file("modules/a.dkm") == data
    assert D.read_range(up, "modules/a.dkm", 700, 1000) == data[700:1700]
    assert D.read_range(up, "modules/a.dkm", 2000, 9999) == data[2000:]


def test_read_errors_are_the_upload_statuses():
    up, _ = _read_fake({"a": b"x" * 10})
    for args, name in [
        (("none", 0, 4), "E_NOFILE"),
        (("a", 0, 0), "E_LENGTH"),
        (("a", 10, 4), "E_OFFSET"),
    ]:
        with pytest.raises(D.UploadError) as e:
            up.read(*args)
        assert D.STATUS[e.value.status] == name


def test_verify_request_and_reply():
    from tools.device.midisniff import unpack_8in7
    from tools.firmware.build import encode_8in7

    sha = D.file_hash(b"abc")
    seen = []

    class Fake:
        def send(self, msg):
            self.msg = msg
            seen.append((msg[5], unpack_8in7(msg[7:-1])))

        def recv(self):
            m = self.msg
            gone = seen[-1][1].startswith(b"none")
            out = bytes([0xC5]) if gone else b"\0" + sha
            return m[:4] + bytes([1, m[5] | 0x40, m[6]]) + encode_8in7(out) + b"\xf7"

    up = D.Uploader(Fake())
    up._file_hash_ok = True
    assert D.CMD_VERIFY == 0x17 and D.CAP["VERIFY"] == 8
    assert up.verify("modules/a.dkm") == sha.hex()
    assert seen[-1] == (0x17, b"modules/a.dkm\0")
    with pytest.raises(D.UploadError) as e:
        up.verify("none.dkm")
    assert D.STATUS[e.value.status] == "E_NOFILE"


def test_folders_prefix_lists_ranged_verify_and_a_streamed_put(tmp_path):
    """Runtime 0.3.0: LIST with a prefix, MKDIR, VERIFY of a range, STORE in KiB, and `put_file` reads the file in pieces."""
    import struct

    from tools.device.midisniff import unpack_8in7
    from tools.firmware.build import encode_8in7

    assert D.CMD_MKDIR == 0x18 and D.CAP["FOLDERS"] == 10
    assert D.dkproto.CAPACITY_KINDS["STORE"] == 12
    seen: list[tuple[int, bytes]] = []
    entries = [("samples/a.wav", 5, b"\x11" * 32), ("samples/b", 0, bytes(32))]

    class Fake:
        def __init__(self):
            self.queue: list[bytes] = []

        def send(self, msg):
            cmd, body = msg[5], unpack_8in7(msg[7:-1])
            seen.append((cmd, body))
            if cmd == D.CMD_LIST:
                first = struct.unpack(">I", body[:4])[0]
                out = struct.pack(">IB", len(entries), len(entries) - first)
                for p, n, h in entries[first:]:
                    out += p.encode() + b"\0" + struct.pack(">I", n) + h
                out = b"\0" + out
            elif cmd == D.CMD_VERIFY:
                out = b"\0" + D.file_hash(body)
            else:
                out = b"\0"
            self.queue.append(
                msg[:4] + bytes([1, cmd | 0x40, msg[6]]) + encode_8in7(out) + b"\xf7"
            )

        def recv(self):
            return self.queue.pop(0)

    up = D.Uploader(Fake())
    up._file_hash_ok = True
    assert [p for p, _, _ in up.ls("samples/")] == ["samples/a.wav", "samples/b"]
    assert seen[-1] == (D.CMD_LIST, struct.pack(">I", 0) + b"samples/\0")
    up.ls()
    assert seen[-1] == (D.CMD_LIST, struct.pack(">I", 0))  # no prefix: the old body
    up.mkdir("samples/new")
    assert seen[-1] == (D.CMD_MKDIR, b"samples/new\0")
    up.verify("samples/a.wav", 512, 100)
    assert seen[-1] == (D.CMD_VERIFY, b"samples/a.wav\0" + struct.pack(">II", 512, 100))
    assert D.capacity_lines({"STORE": {"limit": 1000, "used": 10, "largest": 0}}) == [
        "capacity store: 10/1000 KiB"
    ]
    # put_file: BEGIN carries the size and the file hash of the whole file; the chunks are read from disk
    src = tmp_path / "s.bin"
    data = bytes(range(256)) * 300  # 76,800 bytes: three chunks of 32 KiB
    src.write_bytes(data)
    seen.clear()
    up.put_file("samples/s.bin", src)
    cmds = [c for c, _ in seen]
    assert cmds[0] == D.CMD_BEGIN and cmds[-1] == D.CMD_END
    assert seen[0][1] == b"samples/s.bin\0" + struct.pack(
        ">I", len(data)
    ) + D.file_hash(data)
    got = b"".join(b[4:] for c, b in seen if c == D.CMD_DATA)
    assert got == data


def test_remote_commands_use_the_generated_numbers_and_pace_a_turn():
    """KEY, TURN, RELEASE and STATE (digikit/remote) frame the generated service numbers; a TURN the device takes in parts is sent again for the rest."""
    from tools.device import dkproto
    from tools.firmware.build import encode_8in7

    seen: list[int] = []
    takes = [30, 30, 30, 10]  # a turn of 100: the device takes at most 30 each time

    class Fake:
        def __init__(self):
            self.queue: list[bytes] = []

        def send(self, msg: bytes) -> None:
            cmd, seq = msg[5], msg[6]
            seen.append(cmd)
            out = b"\x00"
            if cmd == dkproto.SERVICE_CMD["REMOTE_TURN"]:
                out += bytes((takes.pop(0),))
            elif cmd == dkproto.SERVICE_CMD["REMOTE_STATE"]:
                out += bytes((1, 1, 30))
            self.queue.append(
                msg[:4] + bytes([1, cmd | 0x40, seq]) + encode_8in7(out) + b"\xf7"
            )

        def recv(self) -> bytes:
            return self.queue.pop(0)

    up = D.Uploader(Fake())
    assert up.remote_state() == {"enabled": 1, "format": 1, "turn_limit": 30}
    up.remote_key(2, True)
    up.remote_turn(1, 100, pace_s=0)
    up.remote_release()
    assert seen == [
        dkproto.SERVICE_CMD[n]
        for n in ["REMOTE_STATE", "REMOTE_KEY"]
        + ["REMOTE_TURN"] * 4
        + ["REMOTE_RELEASE"]
    ]


def test_file_hash_is_the_odd_content_hash_then_28_zeros():
    """The vectors of `dksample::file_hash` (the same table as packages/dk-session/tests/file-hash.test.ts)."""
    want = {
        0: "43fa243b",
        1: "82ea421b",
        5: "dc43f42f",
        13: "ef8c52e1",
        25: "36603f21",
        1000: "ed9b9ced",
        32775: "fc7a664d",
    }
    for n, h in want.items():
        assert (
            D.file_hash(bytes((i * 7 + 3) & 255 for i in range(n))).hex()
            == h + "0" * 56
        ), n
    assert D.file_hash(b"abc").hex() == "49bec845" + "0" * 56


def test_a_runtime_without_the_file_hash_capability_is_refused_at_once():
    import struct

    from tools.firmware.build import encode_8in7

    seen = []

    class Old:
        def send(self, msg):
            self.msg = msg
            seen.append(msg[5])

        def recv(self):
            m = self.msg
            # INFO of a runtime with the log range, runtime32 and caps (VERIFY only)
            info = (
                b"\0"
                + struct.pack(">HH", 1, 0x300)
                + b"p\0"
                + struct.pack(">II", 100, 32768)
                + b"\0"
                + bytes(8)
            )
            info += struct.pack(">II", 0x300, 1 << D.CAP["VERIFY"])
            return m[:4] + bytes([1, m[5] | 0x40, m[6]]) + encode_8in7(info) + b"\xf7"

    up = D.Uploader(Old())
    for call in (lambda: up.put("a.bin", b"x"), up.ls, lambda: up.verify("a.bin")):
        with pytest.raises(
            D.UploadError,
            match="Update the firmware: this runtime uses the old store format",
        ):
            call()
    assert set(seen) == {D.CMD_INFO}


def test_file_hasher_streams_like_the_whole_file_for_any_split():
    """The carry of up to 12 bytes between updates: any split of the input gives the hash of the whole (the runtime's `Hash32` rule)."""
    import random

    rnd = random.Random(7)
    for n in [0, 1, 11, 12, 13, 24, 25, 100, 511, 512, 513, 4096, 32775]:
        data = bytes((i * 7 + 3) & 255 for i in range(n))
        want = D.file_hash(data)
        for _ in range(12):
            cuts = sorted(rnd.randint(0, n) for _ in range(rnd.randint(0, 6)))
            h = D.FileHasher()
            at = 0
            for c in [*cuts, n]:
                h.update(data[at:c])
                at = c
            assert h.digest() == want, (n, cuts)
        h = D.FileHasher()  # DATA chunks: multiples of 512, the last one short
        for at in range(0, n, 512):
            h.update(data[at : at + 512])
        assert h.digest() == want, n
