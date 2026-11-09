"""tools/device/dkprobe_run.py: the steps of `dkprobe run` against a fake device (no emulator, no MIDI): what is uploaded, what is selected, the faults it stops on, and
that the previous selection comes back."""

import json

import pytest
from test_dkprobe import CAP, FakeEmu, FakeUp, stream

from tools.device import dklog as L
from tools.device import dkprobe as P
from tools.device import dkprobe_facts as F
from tools.device import dkprobe_run as PR
from tools.device import dkrun as R

KIND = L.K


def ev(seq, kind, a0=0, a1=0, flags=0):
    return (seq, 0, KIND[kind], flags, (a0, a1, 0, 0, 0))


class FakeDevice(FakeUp):
    """The collector (a full ring from the start) and the rest of the device the run touches: the store, the selection, a boot that loads it."""

    def __init__(self, store, loaded, ring, fault_after_boot=False, bad_load=False):
        super().__init__(ring, fill_after=0)
        self.store = dict(store)  # path -> bytes
        self.loaded = list(loaded)  # module names loaded now
        self.selected: list[list[str]] = []
        self.puts: list[str] = []
        self.restarts = 0
        self.fault_after_boot = fault_after_boot
        self.bad_load = bad_load
        self.events = [ev(1, "BOOT_START", 5)]

    # the commands of dkupload.Uploader that dkprobe_run uses
    def info(self):
        return {
            "port": "dn2",
            "runtime": 0x000503,
            "image_id": "ab12",
            "free_bytes": 1 << 20,
            "safe": False,
            "card_read_only": False,
            "crash_found": False,
            "caps": 1 << R.D.CAP["LOG"],  # the debug module is selected: the log exists
        }

    def ls(self):
        return [(p, len(b), R.D.file_hash(b).hex()) for p, b in self.store.items()]

    def put(self, path, data, progress=None):
        self.store[path] = data
        self.puts.append(path)

    def modules(self):
        return {
            "modules": [
                {"name": n, "faults": 0, "guard_off": False} for n in self.loaded
            ]
        }

    def select(self, paths):
        self.selected.append(list(paths))

    def restart(self):
        self.restarts += 1
        sel = self.selected[-1]
        names = [p.removeprefix("modules/").removesuffix(".dkm") for p in sel]
        ids = {
            "remote": "digikit/remote",
            "probe": "digikit/probe",
            "debug": "digikit/debug",
            "sharc-probe": "test/sharc-probe",
            "dsp-facts": "test/dsp-facts",
        }
        self.loaded = [ids.get(n, "x/" + n) for n in names]
        seq = 1
        self.events = [ev(seq, "BOOT_START", 5)]
        for i, _ in enumerate(names):
            seq += 1
            self.events.append(
                ev(
                    seq,
                    "MOD_LOAD",
                    i,
                    -32 & 0xFFFFFFFF if self.bad_load and i == 1 else 0,
                )
            )
        if self.fault_after_boot:
            self.events.append(ev(seq + 1, "DSP_STOPPED", 7))

    def log_page(self, from_seq):
        evs = [e for e in self.events if e[0] >= from_seq]
        newest = self.events[-1][0]
        return 1, newest, evs

    def request(self, cmd, body=b""):
        if cmd == P.CMD_RESET:
            self.calls = 0  # a new capture waits again, as after the real reset
        return super().request(cmd, body)


class Fakes:
    def __init__(self, tmp_path, monkeypatch):
        self.tmp = tmp_path
        self.packed: list[tuple[str, str]] = []
        monkeypatch.setattr(R, "check_fit", lambda up, info, data: None)

    def pack(self, module, device):
        self.packed.append((module, device))
        out = self.tmp / f"{module.rsplit('/', 1)[-1]}-{device}.dkm"
        out.write_bytes(f"dkm of {module}".encode())
        return out


def _run(tmp_path, monkeypatch, dev, modules=("test/sharc-probe",), **kw):
    fk = Fakes(tmp_path, monkeypatch)
    lines: list[str] = []
    out = PR.run(
        dev,
        list(modules),
        tmp_path / "cap.json",
        sleep=lambda s: None,
        pack=fk.pack,
        log=lines.append,
        **kw,
    )
    return fk, lines, out


def _dev(**kw):
    store = {"modules/pluck.dkm": b"p", "modules/remote.dkm": b"old remote"}
    return FakeDevice(store, ["x/pluck"], stream(300, CAP), **kw)


def test_a_run_packs_uploads_only_what_changed_selects_captures_and_restores(
    tmp_path, monkeypatch
):
    dev = _dev()
    dev.store["modules/remote.dkm"] = (
        b"dkm of digikit/remote"  # already current: not uploaded
    )
    fk, lines, out = _run(tmp_path, monkeypatch, dev)
    assert fk.packed == [
        ("digikit/remote", "dn2"),
        ("digikit/debug", "dn2"),
        ("test/sharc-probe", "dn2"),
        ("digikit/probe", "dn2"),
    ]
    assert dev.puts == [
        "modules/debug.dkm",
        "modules/sharc-probe.dkm",
        "modules/probe.dkm",
    ]
    assert dev.selected == [
        [
            "modules/remote.dkm",
            "modules/debug.dkm",
            "modules/sharc-probe.dkm",
            "modules/probe.dkm",
        ],
        ["modules/pluck.dkm"],  # the selection of before comes back
    ]
    assert dev.restarts == 2 and dev.loaded == ["x/pluck"]
    doc = json.loads((tmp_path / "cap.json").read_text())
    assert doc["host"]["complete"] and out["doc"]["host"]["items_present"] == len(
        P.seq.items()
    )
    text = "\n".join(lines)
    assert (
        "loaded now: x/pluck" in text
        and "no fault after the boot" in text
        and "restored" in text
    )
    assert "not uploaded" in text


def test_keep_leaves_the_probe_modules_selected(tmp_path, monkeypatch):
    dev = _dev()
    _, lines, _ = _run(tmp_path, monkeypatch, dev, keep=True)
    assert len(dev.selected) == 1 and dev.restarts == 1
    assert dev.loaded == [
        "digikit/remote",
        "digikit/debug",
        "test/sharc-probe",
        "digikit/probe",
    ]
    assert any("--keep" in x for x in lines)


def test_a_fault_after_the_boot_stops_the_capture_and_still_restores(
    tmp_path, monkeypatch
):
    dev = _dev(fault_after_boot=True)
    with pytest.raises(PR.ProbeRunError, match="fault after the boot.*DSP_STOPPED"):
        _run(tmp_path, monkeypatch, dev)
    assert dev.selected[-1] == ["modules/pluck.dkm"] and dev.restarts == 2
    assert not (tmp_path / "cap.json").exists()


def test_a_module_that_did_not_load_is_named_and_the_selection_comes_back(
    tmp_path, monkeypatch
):
    dev = _dev(bad_load=True)
    with pytest.raises(R.RunError, match="did not load"):
        _run(tmp_path, monkeypatch, dev)
    assert dev.selected[-1] == ["modules/pluck.dkm"]


def test_a_capture_that_is_not_complete_is_an_error(tmp_path, monkeypatch):
    dev = FakeDevice({}, [], stream(300, CAP)[: CAP // 2] + [0] * (CAP // 2))
    with pytest.raises(PR.ProbeRunError, match="misses"):
        _run(tmp_path, monkeypatch, dev)


def test_compare_prints_a_summary_against_a_reference(tmp_path, monkeypatch):
    ref = P.capture(FakeUp(stream(300, CAP), fill_after=0), FakeEmu(), step=1)
    ref["items_table"][5]["result"] = "0x00000001"
    (tmp_path / "ref.json").write_text(json.dumps(ref))
    _, lines, out = _run(
        tmp_path, monkeypatch, _dev(), compare_path=tmp_path / "ref.json"
    )
    assert out["compare"]["differing_items"] == 1
    assert any("1 differ" in x for x in lines)


def test_the_loaded_modules_are_selected_again_only_when_the_store_has_them(
    tmp_path, monkeypatch
):
    dev = FakeDevice({"modules/a.dkm": b"a"}, ["x/a", "x/gone"], stream(300, CAP))
    lines: list[str] = []
    assert PR.previous_selection(
        dev, {p: (1, "") for p in dev.store}, lines.append
    ) == ["modules/a.dkm"]
    assert any("not in the store" in x and "modules/gone.dkm" in x for x in lines)


def test_the_probe_module_picks_the_sequence():
    assert PR.sequence_for(["test/dsp-facts"], None) is F
    assert PR.sequence_for(["test/sharc-probe"], None) is P.seq
    assert PR.sequence_for(["test/dsp-facts"], "sharc") is P.seq
    with pytest.raises(PR.ProbeRunError):
        PR.sequence_for(["fx/gain"], None)


def test_the_selection_is_remote_debug_modules_collector_without_repeats():
    assert PR.selection_paths(["test/dsp-facts", "digikit/remote"]) == [
        "modules/remote.dkm",
        "modules/debug.dkm",
        "modules/dsp-facts.dkm",
        "modules/probe.dkm",
    ]


def facts_stream(first: int, nwords: int) -> list[int]:
    """The words of the dsp-facts stream from frame `first` on: record 0 and 1, then item i = (i, i + 1002, i + 2), then padding."""
    n = len(F.items())
    ws: list[int] = []
    g = first
    while len(ws) < nwords:
        run, pos = g // F.frames_per_run() + 1, g % F.frames_per_run()
        pl: list[int] = []
        for k in range(F.RECORDS_PER_FRAME):
            r = pos * F.RECORDS_PER_FRAME + k
            if r == 0:
                pl += [run, n, F.table_checksum()]
            elif r == 1:
                pl += [0x12345, 0x75000000, 0]
            elif r < 2 + n:
                pl += [r - 2, r + 1000, r]
            else:
                pl += [F.PAD] * 3
        hdr = F.header_word(g)
        ws += [hdr, *pl, F.fletcher([hdr, *pl])]
        g += 1
    return ws[:nwords]


def test_a_lost_word_is_a_warning_when_other_runs_hold_the_items(tmp_path, monkeypatch):
    ring = facts_stream(0, CAP)
    del ring[
        1000
    ]  # one word lost: one frame of one run is spoiled, the other runs hold its items
    ring += [0]
    _, lines, out = _run(
        tmp_path,
        monkeypatch,
        FakeDevice({}, [], ring),
        modules=("test/dsp-facts",),
    )
    h = out["doc"]["host"]
    assert h["usable"] and not h["complete"] and h["runs"]
    assert any("WARNING framing errors" in x for x in lines)
    row = out["doc"]["facts"]["items"][10]
    assert row["op"] == "empty" or row["runs"] > 1
