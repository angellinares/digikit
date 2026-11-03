"""The host side of the debug log (tools/device/dklog.py): event decoding, the crash record from its events, `follow` with gaps and restarts,
the output file. No emulator: a fake device answers LOG. The runtime's side is modules/tools/debug/tests/debug-module.device.test.ts."""

import json

from tools.device import dklog as L


class FakeUp:
    """What `Uploader.log_page` gives, from a list of rings the test sets up (a restart swaps the ring)."""

    def __init__(self, ring):
        self.ring = ring  # list of (seq, time, kind, flags, args)

    def log_page(self, from_seq):
        if not self.ring:
            return 0, 0, []
        oldest, newest = self.ring[0][0], self.ring[-1][0]
        evs = [e for e in self.ring if e[0] >= max(from_seq, oldest)][:27]
        return oldest, newest, evs


def ev(seq, kind, a=(0, 0, 0, 0, 0), t=None, flags=0):
    return (seq, t if t is not None else seq * 150, L.K[kind], flags, tuple(a))


def word(s):
    return int.from_bytes(s.encode().ljust(4, b"\0")[:4], "big")


def test_the_guard_events_decode():
    # type 18, voice 2 (track 2), kind 1 (nonfinite), 9 faults, knobs 0 .. 5 as the DSP packed them
    d = L.event_dict(
        ev(
            1,
            "MACHINE_FAULT",
            (18 | 2 << 16, 1 | 9 << 8, 0x1000_4000, 0x0000_2000, 0x0100_7F00),
            flags=1,
        )
    )
    f = d["fields"]
    assert d["error"] and d["kind"] == "MACHINE_FAULT"
    assert (f["type"], f["voice"], f["track"], f["kind"], f["faults"]) == (
        18,
        2,
        2,
        "nonfinite",
        9,
    )
    assert f["params"] == ["4000", "1000", "2000", "0000", "7f00", "0100"]
    assert (
        L.event_dict(ev(2, "MACHINE_FAULT", (18, 2 | 3 << 8, 0, 0, 0)))["fields"][
            "kind"
        ]
        == "overlevel"
    )
    f = L.event_dict(ev(3, "MACHINE_DISABLED", (18, 1, 17, 0, 0), flags=1))["fields"]
    assert (f["type"], f["disabled"], f["faults"]) == (18, 1, 17)
    # the diagnostic words 17 to 23 are named
    assert L.DIAG_NAMES[17:24] == (
        "guard_faults",
        "guard_last",
        "guard_p01",
        "guard_p23",
        "guard_p45",
        "guard_p67",
        "guard_disabled_type",
    )
    # the fault table follows: the effect site's last word and the module step
    assert L.DIAG_NAMES[24:26] == ("fx_last", "module_step")
    assert len(L.DIAG_NAMES) == 32


def test_events_decode_to_readable_fields():
    d = L.event_dict(ev(7, "CARD_CHECK", (3, 100, 0x0F498B0E, 0, 0), flags=1))
    assert d["error"] and d["kind"] == "CARD_CHECK" and d["t"] == 0.7
    assert (
        d["fields"]["state"] == "foreign"
        and d["fields"]["first_nonblank"] == 100
        and d["fields"]["hash"] == "0f498b0e"
    )
    assert (
        L.event_dict(ev(1, "CARD_CHECK", (2, 0xFFFFFFFF, 0, 0, 0)))["fields"][
            "first_nonblank"
        ]
        is None
    )
    d = L.event_dict(ev(2, "MOD_LOAD", (0, 0xFFFFFFD3, word("dyn-"), word("plu"), 0)))
    assert d["fields"]["name"] == "dyn-plu" and d["fields"]["result"] == -45
    d = L.event_dict(ev(3, "UPLOAD", (0x12, 0xFFFFFFC7, 5, 0, 0)))
    assert d["fields"]["status"] == -57 and d["fields"]["cmd"] == "0x12"
    d = L.event_dict(
        ev(4, "HEALTH", ((2 | 4) << 16 | 371, 120, 450, 100000, 0xFFFFFFFF))
    )
    assert (
        d["fields"]["cpu_pct"] == 37.1
        and d["fields"]["dsp_max_pct"] == 45.0
        and d["fields"]["heap_free_kb"] is None
    )
    d = L.event_dict(ev(5, "MOD_LOG", (word("cras"), word("h-pc"), 0, 0, 0x47E8B610)))
    assert d["fields"] == {"msg": "crash-pc", "value": 0x47E8B610}
    assert (
        L.event_dict((9, 1, 999, 0, (1, 2, 3, 4, 5)))["kind"] == "KIND999"
    )  # a kind a newer runtime has
    line = L.text_of(L.event_dict(ev(10, "SAFE", (1, 2, 3, 0, 0), flags=1)))
    assert "!SAFE" in line and "reason=counter" in line


def test_dsp_events_show_the_breadcrumb_as_names():
    crumb = 4 | 12 << 8 | 18 << 16  # ROUTINE_CALLED, voice 12, type 18
    d = L.event_dict(ev(7, "DSP_STOPPED", (30000, 29999, crumb, 55, 0), flags=1))
    assert d["error"] and d["fields"]["crumb"] == "ROUTINE_CALLED voice=12 type=18"
    assert d["fields"]["dispatches"] == 55 and d["fields"]["resumed"] is False
    assert L.crumb_of(crumb) == {"stage": "ROUTINE_CALLED", "voice": 12, "type": 18}
    assert L.crumb_of(9 | 2 << 8 | 18 << 16) == {
        "stage": "C3_FRAME_SAVED",
        "voice": 2,
        "type": 18,
    }
    # the diagnostic set, chunk 3 of 4: L7, MODE1, the breadcrumb, the dispatch count
    d = L.event_dict(ev(8, "DSP_DIAG", (16 << 8 | 3, 0x3E2, 0x39011CF8, crumb, 55)))
    assert d["fields"]["crumb"] == "ROUTINE_CALLED voice=12 type=18"
    assert d["fields"]["L7"] == "000003e2" and d["fields"]["dispatches"] == 55
    d = L.event_dict(ev(9, "DSP_DIAG", (16 << 8 | 0, 0xD1A6 | 16 << 16, 1500, 1500, 0)))
    assert d["fields"]["link_beat"] == 1500 and d["fields"]["M5"] == "00000000"


def crash_events(first_seq=10):
    """The events a boot writes for a crash record, as `crash.rs at_boot` writes them."""
    s = first_seq
    out = [
        ev(
            s,
            "CRASH_FOUND",
            (0x47E8B610, 4 | 4 << 8, 0x2004, 0x40B2_0000, 15000),
            flags=1,
        )
    ]
    out.append(
        ev(
            s + 1,
            "CRASH_MOD",
            (0, 1 | 0 << 16, word("test"), word("/dyn"), word("-cra")),
            flags=1,
        )
    )
    out.append(ev(s + 2, "CRASH_MOD", (0x100, 0x40102004, 1, word("sh"), 0), flags=1))
    for k in range(0, 16, 4):
        out.append(
            ev(
                s + 3 + k // 4,
                "CRASH_REG",
                (k, 0x1000 + k, 0x1001 + k, 0x1002 + k, 0x1003 + k),
                flags=1,
            )
        )
    for k in range(0, 32, 4):
        out.append(
            ev(s + 7 + k // 4, "CRASH_STACK", (k, k, k + 1, k + 2, k + 3), flags=1)
        )
    # one event before the crash: seq 5, time 700, kind MOD_LOG, flags 0, args (a0..a4), as two events
    kf = L.K["MOD_LOG"] << 16
    a = (word("cras"), word("h-pc"), 0, 0, 0x47E8B610)
    out.append(ev(s + 15, "CRASH_TAIL", (5, 700, kf, a[0], a[1]), flags=1))
    out.append(ev(s + 16, "CRASH_TAIL", (5, a[2], a[3], a[4], 1), flags=1))
    return out


def test_the_crash_record_is_rebuilt_from_its_events():
    evs = [ev(1, "BOOT_START"), *crash_events(2), ev(30, "UI_FIRST")]
    rec = L.crash_record(evs)
    assert (
        rec["pc"] == 0x47E8B610
        and rec["vector"] == 4
        and rec["vector_name"] == "illegal instruction"
        and rec["format"] == 4
    )
    assert rec["sr"] == 0x2004 and rec["a7"] == 0x40B20000
    assert rec["module"] == {
        "index": 0,
        "name": "test/dyn-cra",
        "slots_entered": 1,
        "boot_was_stable": False,
    }
    assert (
        rec["regs"]["d0"] == 0x1000
        and rec["regs"]["a7"] == 0x100F
        and len(rec["regs"]) == 16
    )
    assert rec["stack"] == {i: i for i in range(32)}
    assert rec["frame_word"] == 0x40102004 and rec["faults_in_handler"] == 1
    assert rec["tail"] == [
        (5, 700, L.K["MOD_LOG"], 0, (word("cras"), word("h-pc"), 0, 0, 0x47E8B610))
    ]
    text = L.crash_text(rec)
    assert (
        "pc 47e8b610" in text
        and "illegal instruction" in text
        and "test/dyn-cra" in text
        and "MOD_LOG" in text
        and "crash-pc" in text
    )


def test_a_panic_record_is_rebuilt_from_its_events():
    evs = [
        ev(1, "BOOT_START"),
        ev(2, "FAULT_FOUND", (3, 0x40B2_0000, 0, 0, 0xFFFFFFFF), flags=1),
    ]
    for k in range(0, 32, 4):
        evs.append(ev(3 + k // 4, "CRASH_STACK", (k, k, k + 1, k + 2, k + 3), flags=1))
    rec = L.panic_record(evs)
    assert rec["a7"] == 0x40B20000 and rec["stack"] == {i: i for i in range(32)}
    text = L.crash_text(rec)
    assert "CPU PANIC in runtime" in text and "+000  00000000 00000001" in text
    assert L.panic_record([ev(1, "BOOT_START")]) is None
    # the CPU and DSP records are not taken for a panic
    assert L.panic_record([ev(2, "FAULT_FOUND", (1, 0, 0, 0x1234, 0), flags=1)]) is None


def test_the_last_crash_wins_and_a_log_without_one_has_none():
    assert L.crash_record([ev(1, "BOOT_START")]) is None
    first = crash_events(2)
    second = crash_events(40)
    second[0] = ev(40, "CRASH_FOUND", (0x1234, 2, 0x2000, 0x40000000, 1), flags=1)
    rec = L.crash_record([*first, ev(39, "UI_FIRST"), *second])
    assert (
        rec["pc"] == 0x1234
        and rec["vector"] == 2
        and rec["vector_name"] == "access error"
    )
    bad = L.crash_record([ev(1, "CRASH_FOUND", (0, 0, 0, 0, 0xBAD), flags=1)])
    assert bad == {"corrupt": True, "seq": 1}
    assert "CRC" in L.crash_text(bad)


def test_follow_writes_text_and_json_notes_a_gap_and_a_restart(tmp_path):
    ring = [ev(s, "UPLOAD", (1, 0, s, 0, 0)) for s in range(1, 6)]
    up = FakeUp(ring)
    out = tmp_path / "d" / "log.txt"
    assert L.follow(up, out, once=True, echo=False) == 5
    # nothing new: nothing written; then the ring moved on past what was read (a gap), and the events come in order
    assert L.follow(up, out, once=True, echo=False, start=6) == 0
    up.ring = [ev(s, "UPLOAD", (1, 0, s, 0, 0)) for s in range(40, 44)]
    fol = L.Follower(up, lambda text, d: lines.append((text, d)), start=6)
    lines: list = []
    assert fol.poll() == 4
    assert "34 events lost" in lines[0][0] and lines[0][1] is None
    assert [d["seq"] for _, d in lines[1:]] == [40, 41, 42, 43]
    # a restart: the newest sequence number is lower than what was seen
    up.ring = [ev(1, "BOOT_START"), ev(2, "CARD_CHECK", (2, 0xFFFFFFFF, 0, 0, 0))]
    lines.clear()
    assert fol.poll() == 2
    assert "restarted" in lines[0][0] and [d["seq"] for _, d in lines[1:]] == [1, 2]
    # the file: readable lines start with "# ", the rest is JSON lines
    rows = out.read_text().splitlines()
    js = [json.loads(x) for x in rows if not x.startswith("#")]
    assert [j["seq"] for j in js] == [1, 2, 3, 4, 5] and all(
        "host_time" in j and j["kind"] == "UPLOAD" for j in js
    )
    assert sum(1 for x in rows if x.startswith("#") and "UPLOAD" in x) == 5


def lv(level, src=0, error=False):
    return level << 1 | src << 8 | (1 if error else 0)


def test_level_and_source_are_decoded():
    d = L.event_dict(ev(1, "MOD_LOG", flags=lv(4, 3)))
    assert (d["level"], d["source"], d["error"]) == ("trace", 3, False)
    d = L.event_dict(ev(2, "SAFE", flags=lv(0, 0, True)))
    assert (d["level"], d["source"], d["error"]) == ("error", 0, True)
    assert L.event_dict(ev(3, "MENU", flags=7 << 1))["level"] == "7"
    line = L.text_of(L.event_dict(ev(4, "MOD_LOG", flags=lv(2, 2))))
    assert "info" in line and "mod#1" in line
    assert "c-demo" in L.text_of(
        L.event_dict(ev(5, "MOD_LOG", flags=lv(2, 2))), {2: "c-demo"}
    )


def test_display_filters_by_level_and_module():
    ring = [
        ev(1, "MENU", flags=lv(2)),
        ev(2, "MENU", flags=lv(4)),
        ev(3, "SAFE", flags=lv(0, 0, True)),
        ev(4, "MOD_LOG", flags=lv(3, 1)),
        ev(5, "MOD_LOG", flags=lv(2, 2)),
    ]
    out: list = []
    fol = L.Follower(FakeUp(ring), lambda t, d: out.append(d), level=L.level_of("info"))
    assert fol.poll() == 3 and [d["seq"] for d in out] == [
        1,
        3,
        5,
    ]  # errors always, trace and debug hidden
    out.clear()
    fol = L.Follower(FakeUp(ring), lambda t, d: out.append(d), source=2)
    assert fol.poll() == 1 and out[0]["seq"] == 5
    out.clear()
    fol = L.Follower(FakeUp(ring), lambda t, d: out.append(d), level=3, source=1)
    assert fol.poll() == 1 and out[0]["seq"] == 4
    assert L.level_of("Debug") == 3 and L.level_of("0") == 0


def test_module_names_come_from_mod_list():
    evs = [ev(1, "MOD_LIST", (1, 3, word("c-de"), word("mo"), 0))]
    names = L.module_names(evs)
    assert names == {2: "c-demo"}
    assert L.source_of("c-demo", names) == 2 and L.source_of("3", names) == 4


class FakeCfg:
    """What `Uploader.logcfg` does against the runtime's LOGCFG: the state lives here."""

    def __init__(self):
        self.default, self.modules, self.debug = 2, [0xFF] * 8, True

    def logcfg(self, default=None, modules=None):
        if default is not None:
            self.default = default
        for i, v in (modules or {}).items():
            self.modules[i] = 0xFF if v is None else v
        return {
            "default": self.default,
            "modules": list(self.modules),
            "debug": self.debug,
        }


def test_level_subcommand_round_trip():
    up = FakeCfg()
    assert "default: info" in L.level_config(up)
    text = L.level_config(up, "error", ["c-demo=trace", "3=warn"], {2: "c-demo"})
    assert "default: error" in text and "(c-demo): trace" in text
    assert up.default == 0 and up.modules[1] == 4 and up.modules[0] == 0xFF
    text = L.level_config(up, None, ["c-demo=default"], {2: "c-demo"})
    assert up.modules[1] == 0xFF and "default: error" in text


def test_read_all_pages_through_the_ring():
    up = FakeUp([ev(s, "MENU", (1, 0, 0, 0, 0)) for s in range(1, 70)])
    oldest, newest, evs = L.read_all(up)
    assert (oldest, newest, len(evs)) == (1, 69, 69) and [e[0] for e in evs] == list(
        range(1, 70)
    )


def fault_blob(kind, site, stage, ident, step, at, name):
    w = [L.FAULT_MAGIC, kind | site << 8 | stage << 16, ident, step, at, 0x00050300]
    return b"".join(x.to_bytes(4, "big") for x in w) + name.encode().ljust(16, b"\0")


def test_the_stored_fault_gives_the_same_rows_as_the_menu():
    # a send effect that stopped the DSP in its third step; the rows are those of dkrt_sdk::fault::rows
    f = L.fault_value(fault_blob(2, 3, 4, 96, 3, 0x00030004, "test/dyn-fx-hang"))
    assert (f["kind"], f["site"], f["core_step"], f["module_step"]) == (
        "dsp stopped",
        "fx send",
        "CALL",
        3,
    )
    assert L.fault_rows(f) == [
        "FAULT: DSP STOP",
        "IN DYN-FX-HANG",
        "FX SEND STEP 3",
    ]
    # without a module step the core step is shown; a CPU crash shows the PC
    f = L.fault_value(fault_blob(2, 2, 4, 0, 0, 0, "fx/gain"))
    assert L.fault_rows(f)[2] == "FX MASTER CALL"
    f = L.fault_value(fault_blob(1, 0, 0, 0, 0, 0x0012A4F0, "test/crash"))
    assert L.fault_rows(f) == ["FAULT: CPU CRASH", "IN CRASH", "PC 0012A4F0"]
    assert L.fault_value(b"\0" * 40) is None
    assert L.fault_value(fault_blob(2, 3, 4, 0, 0, 0, "x")[:39]) is None


def test_a_hang_record_is_read_from_its_events():
    evs = [
        ev(1, "BOOT_START"),
        # kind 2 (dsp stopped) | site 3 (fx send) << 8 | stage 4 (CALL) << 16; row byte offset 96; module step 3; last word; module index 0
        ev(2, "FAULT_FOUND", (2 | 3 << 8 | 4 << 16, 96, 3, 0x00600304, 0), flags=1),
        ev(3, "CRASH_MOD", (0, 0, word("test"), word("/dyn"), word("-fx\0")), flags=1),
        ev(4, "DSP_DIAG", (32 << 8 | 6, 0x600304, 3, 7, 9, 0), flags=1),
    ]
    rec = L.hang_record(evs)
    assert rec and (rec["site"], rec["core_step"], rec["module_step"]) == (
        "fx send",
        "CALL",
        3,
    )
    assert rec["module"]["name"] == "test/dyn-fx"
    assert rec["diag"]["fx_last"] == 0x600304 and rec["diag"]["module_step"] == 3
    text = L.hang_text(rec)
    assert (
        "fx send" in text and "effect table row 1" in text and "module step 3" in text
    )
    assert L.hang_record(evs[:1]) is None
    # FX_FAULT and FAULT_FOUND decode to readable fields
    d = L.event_dict(ev(5, "FX_FAULT", (1, 8, 0, 0, 0), flags=1))
    assert d["fields"] == {"row": 1, "faults": 8}
