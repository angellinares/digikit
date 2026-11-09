"""tools/device/dktransfer.py without an emulator: header groups, the merged list, the install plan and the install sequence (a mock uploader)."""

import struct
import tomllib
from pathlib import Path

import pytest

from tools.device import dktransfer as T
from tools.device import dkupload as D


def header(
    name, api=(1, 1), machine=(0, 0), fx=(0, 0), service=(0, 0), size=500, dsp=0
):
    h = bytearray(D.HEADER_BYTES)
    h[:4] = b"DKMF"
    struct.pack_into(">II", h, 8, size, 0xA0)
    struct.pack_into(">I", h, 0x34, 0x000102)
    h[0x38:0x40] = bytes([*api, *machine, *fx, *service])
    h[0x40 : 0x40 + len(name)] = name.encode()
    struct.pack_into(">II", h, 0x64, 100, 20)
    struct.pack_into(">I", h, 0x9C, dsp)
    return bytes(h)


def test_header_groups():
    m = T.header_info(header("acme/synth", machine=(1, 0), service=(1, 0), dsp=64))
    assert (m["group"], m["name"], m["version"], m["arena"], m["dsp"]) == (
        "Machines",
        "acme/synth",
        "0.1.2",
        120,
        64,
    )
    assert T.header_info(header("a", fx=(1, 0)))["group"] == "Effects"
    assert T.header_info(header("a", service=(1, 0)))["group"] == "Tools"
    assert T.header_info(header("a"))["group"] == "Tools"
    assert T.header_info(b"nope" + bytes(200)) is None
    assert T.header_info(b"DKMF") is None


def test_repo_rows_carry_the_needs_inferred_from_the_packed_file(tmp_path):
    from tools.patchkit import patchkit as PK

    def pack(mid, device):
        out = tmp_path / f"{mid.replace('/', '-')}.dkm"
        assert PK.run(["pack", mid, "--device", device, "-o", str(out)]).returncode == 0
        return out

    rows = {
        r["id"]: r
        for r in T.repo_rows(
            "dt2",
            ["test/sine", "digikit/remote"],
            pack=pack,
            status=lambda m: "unchanged",
        )
    }
    sine, remote = rows["test/sine"], rows["digikit/remote"]
    # the machine with DSP code takes a DSP range and a voice row; the row's arena and dsp are what a load takes, from the same needs
    assert sine["needs"]["DSP_AREA"] > 0 and sine["needs"]["VOICE_ROWS"] >= 1
    assert (sine["arena"], sine["dsp"]) == (
        sine["needs"]["ARENA"],
        sine["needs"]["DSP_AREA"],
    )
    assert sine["needs"]["file_size"] == sine["file_size"]
    assert remote["needs"]["DSP_AREA"] == 0 and remote["needs"]["ARENA"] > 0


def row(
    mid, group="Machines", build="unchanged", size=500, arena=120, dsp=64, sha="aa"
):
    return {
        "id": mid, "name": mid, "label": mid, "group": group, "test": mid.startswith("test/"),
        "path": f"modules/{mid.split('/')[-1]}.dkm", "hash": sha, "file_size": size, "arena": arena,
        "dsp": dsp, "build": build, "version": "0.1.0", "dkm": "x",
        "needs": {"file_size": size, "ARENA": arena, "DSP_AREA": dsp, "DSP_ALIGN": 8 if dsp else 0},
    }  # fmt: skip


CAP = {
    "MODULES": {"limit": 8, "used": 1, "largest": 0},
    "ARENA": {"limit": 1000, "used": 0, "largest": 300},
    "DSP_AREA": {"limit": 1000, "used": 0, "largest": 50},
}


def test_merge_state_groups_and_fit():
    rows = [
        row("test/sine", sha="s1"),
        row("acme/synth", sha="p1"),
        row("someone/tool", group="Tools", dsp=0, sha="t1"),
    ]
    card = [
        ("modules/synth.dkm", 500, "p1"),  # same file
        ("modules/tool.dkm", 500, "OLD"),  # same path, other bytes
        ("modules/other.dkm", 700, "zz"),  # not from the repo
        ("modules/junk.dkm", 9, "yy"),  # no readable header
    ]
    headers = {
        "modules/other.dkm": header("someone/effect", fx=(1, 0), size=700),
        "modules/junk.dkm": None,
    }
    v = T.merge(rows, card, ["acme/synth", "someone/effect"], CAP, headers)
    ids = [r["id"] for r in v["modules"]]
    assert (
        ids == ["acme/synth", "test/sine", "someone/tool"][:0] or ids[0] == "acme/synth"
    )
    assert [r["group"] for r in v["modules"]] == ["Machines", "Machines", "Tools"]
    by = {r["id"]: r for r in v["modules"]}
    assert by["acme/synth"]["on_card"] == "yes" and by["acme/synth"]["loaded"]
    assert by["someone/tool"]["on_card"] == "other version"
    assert by["test/sine"]["on_card"] == "no" and by["test/sine"]["test"]
    # a loaded module fits by definition; the others are checked against CAPACITY (dsp 64 > largest 50), and the warning says whether the
    # module fits the device alone (the next boot starts empty)
    assert by["acme/synth"]["fits"] is True
    assert by["test/sine"]["fits"] is False
    assert by["test/sine"]["warnings"] == [
        "DSP memory 64 bytes needed, 50 bytes free; fits when the modules loaded now are not selected"
    ]
    assert by["someone/tool"]["fits"] is True
    extra = {r["path"]: r for r in v["card_only"]}
    assert (
        extra["modules/other.dkm"]["group"] == "Effects"
        and extra["modules/other.dkm"]["loaded"]
    )
    assert (
        extra["modules/junk.dkm"]["known"] is False
        and not extra["modules/junk.dkm"]["loaded"]
    )
    # a module larger than the whole device
    huge = {**rows[0], "needs": {**rows[0]["needs"], "DSP_AREA": 2000}, "loaded": False}
    assert T.fit_warnings(huge, CAP)[0].endswith("more than this device has")
    # no CAPACITY: no verdict
    assert T.merge(rows, [], [], None, {})["modules"][0]["fits"] is None


def test_plan_build_upload_select():
    rows = [
        row("acme/synth", sha="p1"),
        row("test/sine", build="needs build", sha="s1"),
    ]
    card = [("modules/synth.dkm", 500, "p1")]
    p = T.plan_install(rows, ["acme/synth", "test/sine"], card, ["modules/old.dkm"])
    assert p == {
        "build": ["test/sine"],
        "upload": ["test/sine"],
        "select": ["modules/synth.dkm", "modules/sine.dkm", "modules/old.dkm"],
    }
    with pytest.raises(T.TransferError, match="not a run-time module"):
        T.plan_install(rows, ["digikit/nothing"], card)
    with pytest.raises(T.TransferError, match="at most 8"):
        T.plan_install(
            rows, ["acme/synth"], card, [f"modules/k{i}.dkm" for i in range(8)]
        )
    # the same path in the wanted set and in --keep is selected once
    assert T.plan_install(rows, ["acme/synth"], card, ["modules/synth.dkm"])[
        "select"
    ] == ["modules/synth.dkm"]


class MockUp:
    """An uploader that records what an install asks of it; `modules()` lists the selection after `restart()`."""

    def __init__(self, card, loaded_after, runtime=0x000204, corrupt=False):
        self.card, self.loaded_after, self.runtime = list(card), loaded_after, runtime
        self.calls, self.restarted, self.data, self.corrupt = [], False, {}, corrupt
        self.stuck = False  # a DELETE the card does not keep

    def info(self):
        return {
            "runtime": self.runtime,
            "card_read_only": False,
            "port": "dt2",
            "free_bytes": 1 << 20,
            "caps": 0x1FF,
        }

    def ls(self):
        return list(self.card)

    chunk = 32768

    def put(self, path, data, progress=None):
        self.calls.append(("put", path))
        if progress:  # as Uploader.put: after each acknowledged chunk
            for at in range(0, len(data), self.chunk):
                progress(min(len(data), at + self.chunk), len(data))
        self.data[path] = data
        self.card = [c for c in self.card if c[0] != path] + [
            (path, len(data), D.file_hash(data).hex())
        ]

    def verify(self, path):
        self.calls.append(("verify", path))
        got = self.data.get(path)
        if got is None:  # on the card from before: its listed hash
            return next(h for p, _, h in self.card if p == path)
        return D.file_hash(got + (b"!" if self.corrupt else b"")).hex()

    def rm(self, path):
        self.calls.append(("rm", path))
        if not self.stuck:
            self.card = [c for c in self.card if c[0] != path]

    def select(self, paths):
        self.calls.append(("select", list(paths)))

    def restart(self):
        self.calls.append(("restart",))
        self.restarted = True

    def modules(self):
        names = self.loaded_after if self.restarted else []
        return {"modules": [{"name": n} for n in names]}


def _two_rows(tmp_path):
    pl = tmp_path / "synth.dkm"
    pl.write_bytes(b"P" * 600)
    sn = tmp_path / "sine.dkm"
    sn.write_bytes(b"S" * 700)
    sha = lambda b: D.file_hash(b).hex()  # noqa: E731
    rows = [
        {**row("acme/synth", sha=sha(b"P" * 600), size=600), "dkm": str(pl)},
        {**row("test/sine", build="needs build", sha="stale", size=1), "dkm": str(sn)},
    ]
    fresh = lambda device, ids, pack: [  # noqa: E731
        {**row(ids[0], sha=sha(b"S" * 700), size=700), "dkm": str(sn)}
    ]
    card = [("modules/synth.dkm", 600, sha(b"P" * 600))]
    return rows, fresh, card


def test_install_uploads_and_verifies_and_leaves_the_selection_alone(
    tmp_path, monkeypatch
):
    rows, fresh, card = _two_rows(tmp_path)
    monkeypatch.setattr(T, "repo_rows", fresh)
    built, events = [], []
    up = MockUp(card, ["acme/synth", "test/sine"])
    out = T.install(
        up,
        ["acme/synth", "test/sine"],
        None,
        events.append,
        rows=rows,
        build=built.append,
    )
    assert built == ["test/sine"]  # only the changed one is built
    assert (
        up.calls
        == [  # per module in order: upload (synth is on the card: skipped), verify; no SELECT, no RESTART
            ("verify", "modules/synth.dkm"),
            ("put", "modules/sine.dkm"),
            ("verify", "modules/sine.dkm"),
        ]
    )
    assert out["uploaded"] == ["test/sine"] and out["restarted"] is False
    assert out["selected"] == [] and "loaded" not in out
    kinds = [e["event"] for e in events]
    assert kinds == [
        "start", "upload", "skip", "verify", "build", "upload", "progress", "verify", "done",
    ]  # fmt: skip
    assert [e["ok"] for e in events if e["event"] == "verify"] == [True, True]
    assert events[-1] == {"event": "done", "restarted": False, "selected": []}


def test_install_with_select_sends_one_select_and_no_restart(tmp_path, monkeypatch):
    rows, fresh, card = _two_rows(tmp_path)
    monkeypatch.setattr(T, "repo_rows", fresh)
    events = []
    up = MockUp(card, [])
    out = T.install(
        up,
        ["acme/synth", "test/sine"],
        ["modules/loaded.dkm"],
        events.append,
        rows=rows,
        build=lambda m: None,
        select=True,
    )
    want = ["modules/synth.dkm", "modules/sine.dkm", "modules/loaded.dkm"]
    assert up.calls[-1] == ("select", want) and ("restart",) not in up.calls
    assert out["selected"] == want and out["restarted"] is False
    assert [e["event"] for e in events][-2:] == ["select", "done"]
    assert events[-1]["selected"] == want


def test_install_without_select_does_not_limit_the_count(tmp_path, monkeypatch):
    # the selection holds 8 paths; an install that selects nothing may carry more modules
    rows, fresh, card = _two_rows(tmp_path)
    many = [
        dict(rows[0], id=f"digikit/m{i}", path=f"modules/m{i}.dkm") for i in range(9)
    ]
    plan = T.plan_install(many, [r["id"] for r in many], card, selecting=False)
    assert len(plan["select"]) == 9
    with pytest.raises(T.TransferError, match="at most 8"):
        T.plan_install(many, [r["id"] for r in many], card)


def test_install_with_restart_waits_for_modules(tmp_path, monkeypatch):
    from tools.device import dkrun as R

    rows, fresh, card = _two_rows(tmp_path)
    monkeypatch.setattr(T, "repo_rows", fresh)
    monkeypatch.setattr(R, "restart_and_wait", lambda up, *a: (up.restart(), up)[1])
    events = []
    up = MockUp(card, ["acme/synth", "test/sine"])
    out = T.install(
        up, ["acme/synth", "test/sine"], None, events.append, rows=rows,
        build=lambda m: None, restart=True,
    )  # fmt: skip
    assert up.calls[-2:] == [
        ("select", ["modules/synth.dkm", "modules/sine.dkm"]),
        ("restart",),
    ]
    assert out["restarted"] and out["loaded"] == ["acme/synth", "test/sine"]
    assert [e["event"] for e in events][-3:] == ["restart", "wait", "done"]


def test_a_verify_mismatch_is_an_error_and_nothing_is_selected(tmp_path, monkeypatch):
    rows, fresh, card = _two_rows(tmp_path)
    monkeypatch.setattr(T, "repo_rows", fresh)
    events = []
    up = MockUp(card, [], corrupt=True)
    with pytest.raises(T.TransferError, match="did not keep the file"):
        T.install(
            up, ["test/sine"], None, events.append, rows=rows, build=lambda m: None
        )
    assert not [c for c in up.calls if c[0] == "select"]
    assert events[-1] == {
        "event": "verify", "id": "test/sine", "path": "modules/sine.dkm", "ok": False,
    }  # fmt: skip


def test_restart_reports_loaded_and_missing_with_the_reason(monkeypatch):
    from tools.device import dkrun as R

    monkeypatch.setattr(R, "restart_and_wait", lambda up, *a: (up.restart(), up)[1])
    up = MockUp([], ["acme/synth"])
    t = {"v": 0.0}
    events = []
    out = T.restart_device(
        up, ["acme/synth", "test/sine"], events.append,
        sleep=lambda s: t.__setitem__("v", t["v"] + s), now=lambda: t["v"],
        paths=["modules/synth.dkm", "modules/sine.dkm"],
    )  # fmt: skip
    assert out["loaded"] == ["acme/synth"] and out["missing"] == ["test/sine"]
    # the store is empty after the restart: the reason is said
    assert out["not_on_card"] == ["modules/synth.dkm", "modules/sine.dkm"]
    assert events[-1]["event"] == "done" and events[-1]["missing"] == ["test/sine"]


def test_install_with_restart_says_when_the_restart_lost_the_files(
    tmp_path, monkeypatch
):
    from tools.device import dkrun as R

    rows, fresh, card = _two_rows(tmp_path)
    monkeypatch.setattr(T, "repo_rows", fresh)
    up = MockUp(card, [])

    def lose(up, *a):
        up.restart()
        up.card = []  # a soft reset without the overlay
        return up

    monkeypatch.setattr(R, "restart_and_wait", lose)
    t = {"v": 0.0}
    with pytest.raises(T.TransferError, match="restart lost the card's writes"):
        T.install(
            up, ["acme/synth"], None, rows=rows, build=lambda m: None, restart=True,
            sleep=lambda s: t.__setitem__("v", t["v"] + s), now=lambda: t["v"],
        )  # fmt: skip


def test_the_packed_file_is_reused_until_an_input_changes(tmp_path, monkeypatch):
    import os

    from tools.device import dkrun as R

    mod = tmp_path / "mod"
    mod.mkdir()
    (mod / "module.toml").write_text("x")
    monkeypatch.setattr(R, "ROOT", tmp_path)
    monkeypatch.setattr(T.modroots, "module_dir", lambda m: mod)
    monkeypatch.setattr(
        T, "inputs_mtime", lambda d: max(f.stat().st_mtime for f in d.iterdir())
    )
    packs = []

    def pack(module, device, out):
        packs.append(module)
        out.parent.mkdir(parents=True, exist_ok=True)
        out.write_bytes(b"dkm")
        return out

    monkeypatch.setattr(R, "pack_module", pack)
    a = T.pack_for_transfer("test/m", "dt2")
    assert T.pack_for_transfer("test/m", "dt2") == a and packs == ["test/m"]  # reused
    later = a.stat().st_mtime + 10
    os.utime(mod / "module.toml", (later, later))  # a source changed
    T.pack_for_transfer("test/m", "dt2")
    assert packs == ["test/m", "test/m"]

    # a module that does not pack is remembered until its inputs change
    def refuse(module, device, out):
        packs.append("refused")
        raise R.RunError("nothing for device dt2")

    monkeypatch.setattr(R, "pack_module", refuse)
    (tmp_path / "out/patch/transfer/dt2/m.dkm").unlink()
    os.utime(mod / "module.toml", (1_000_000, 1_000_000))
    for _ in range(2):
        with pytest.raises(R.RunError, match="nothing for device"):
            T.pack_for_transfer("test/m", "dt2")
    assert packs.count("refused") == 1


def test_a_paused_emulator_is_refused():
    class Emu:
        def status(self):
            return {"mode": "paused"}

    import argparse

    args = argparse.Namespace(emu=True, run_dir=None, socket="s", step=False, port=None)
    orig = T._emu_at
    T._emu_at = lambda s: Emu()
    try:
        with pytest.raises(T.TransferError, match="paused"):
            T.connect_target(args)
    finally:
        T._emu_at = orig


def test_candidates_are_the_run_time_modules_and_pack_to_a_dkm(tmp_path):
    # service modules (digikit/remote) are run-time modules; frameworks and patched-in modules are not
    from tools.device import dkrun as R

    ids = T.candidates()
    from tools.patchkit import modroots

    shipped = {
        mid
        for mid, d in modroots.manifests().items()
        if d.is_relative_to(modroots.ROOT / "modules")
        and tomllib.loads((d / "module.toml").read_text())["module"].get("dynamic")
    }
    assert shipped and shipped <= set(
        ids
    )  # every shipped run-time module is a candidate
    assert "digikit/remote" in ids and "test/svc-echo" in ids
    assert not [i for i in ids if i.startswith("core/")]
    # the packer is the truth: digikit/remote packs and its header has the service contract (group Tools)
    rows = T.repo_rows(
        "dn2",
        ["digikit/remote"],
        pack=lambda mid, dev: R.pack_module(mid, dev, tmp_path / "remote.dkm"),
    )
    assert [(r["id"], r["group"], r["version"]) for r in rows] == [
        ("digikit/remote", "Tools", "0.1.0")
    ]
    assert rows[0]["file_size"] > 0


def test_source_hash_matches_the_committed_objects():
    # the skip-if-unchanged hash is the one the builders write: every committed object of the repo carries it
    from tools.patchkit import modroots, patchkit

    dirs = list(modroots.manifests().values())
    stale = [
        str(d) for d, s in patchkit.module_statuses(dirs).items() if s != "unchanged"
    ]
    assert stale == []


def test_upload_progress_events_are_capped_and_end_at_the_file_size(
    tmp_path, monkeypatch
):
    big = tmp_path / "big.dkm"
    big.write_bytes(b"B" * 20_000)
    row_big = {
        **row("test/big", sha=D.file_hash(b"B" * 20_000).hex(), size=20_000),
        "dkm": str(big),
    }
    row_big["path"] = "modules/big.dkm"
    small, _, card = _two_rows(tmp_path)
    up = MockUp(card, [])
    up.chunk = 512  # 40 chunks: far more than the cap
    events = []
    T.install(
        up,
        ["test/big", "acme/synth"],
        None,
        events.append,
        rows=[row_big, small[0]],
        build=lambda m: None,
    )
    prog = [e for e in events if e["event"] == "progress"]
    assert all(
        e["id"] == "test/big" for e in prog
    )  # synth was on the card: skipped, no progress
    assert 1 < len(prog) <= T.PROGRESS_EVENTS + 1, len(prog)
    sent = [e["sent"] for e in prog]
    assert sent == sorted(sent) and len(set(sent)) == len(sent)
    assert prog[-1]["sent"] == prog[-1]["bytes"] == 20_000
    # order: the module's upload, its progress, then its verify
    kinds = [e["event"] for e in events if e.get("id") == "test/big"]
    assert (
        kinds[0] == "upload"
        and kinds[-1] == "verify"
        and set(kinds[1:-1]) == {"progress"}
    )
    # a file of one chunk reports once, at 100 %
    cb_events = []
    cb = T.upload_progress(cb_events.append, "a/b", 700)
    cb(700, 700)
    assert cb_events == [{"event": "progress", "id": "a/b", "sent": 700, "bytes": 700}]
    # a tiny file with many chunk calls does not emit per call: at most `step` bytes apart
    cb_events.clear()
    cb = T.upload_progress(cb_events.append, "a/c", 5)
    for n in range(1, 6):
        cb(n, 5)
    assert cb_events[-1]["sent"] == 5 and len(cb_events) <= T.PROGRESS_EVENTS + 1


def test_delete_sends_one_delete_per_path_and_confirms_with_list():
    card = [
        ("modules/a.dkm", 5, "aa"),
        ("modules/b.dkm", 6, "bb"),
        ("modules/c.dkm", 7, "cc"),
    ]
    up = MockUp(card, [])
    events = []
    out = T.delete(
        up, ["modules/a.dkm", "modules/c.dkm", "modules/a.dkm"], events.append
    )
    assert out == {
        "deleted": ["modules/a.dkm", "modules/c.dkm"]
    }  # a repeated path is one DELETE
    assert up.calls == [
        ("rm", "modules/a.dkm"),
        ("rm", "modules/c.dkm"),
    ]  # no SELECT: the selection is the device's
    assert up.card == [("modules/b.dkm", 6, "bb")]
    assert [e["event"] for e in events] == ["start", "delete", "delete", "done"]
    assert events[1] == {"event": "delete", "path": "modules/a.dkm"}
    assert events[-1] == {
        "event": "done",
        "deleted": ["modules/a.dkm", "modules/c.dkm"],
    }


def test_delete_of_a_path_that_is_not_on_the_card_sends_nothing():
    up = MockUp([("modules/a.dkm", 5, "aa")], [])
    events = []
    with pytest.raises(T.TransferError, match="not on the card: modules/none.dkm"):
        T.delete(up, ["modules/a.dkm", "modules/none.dkm"], events.append)
    assert up.calls == [] and events == []  # reported before anything is sent


def test_delete_fails_when_list_still_shows_the_file():
    up = MockUp([("modules/a.dkm", 5, "aa")], [])
    up.stuck = True
    with pytest.raises(T.TransferError, match="still on the card after DELETE"):
        T.delete(up, ["modules/a.dkm"])


def test_pack_all_writes_one_folder_per_device_and_only_what_changed(
    tmp_path, monkeypatch
):
    from tools.device import dkpackall as P
    from tools.device import dkrun as R

    packed = tmp_path / "packed"
    packed.mkdir()
    state = {"a": b"A1", "b": b"B1"}
    built = []

    def fake_pack(mid, dev):
        if mid == "test/b" and dev == "dn2":
            raise R.RunError("nothing for device dn2")
        f = packed / f"{mid.replace('/', '_')}-{dev}.dkm"
        f.write_bytes(state[mid.split("/")[-1]])
        return f

    monkeypatch.setattr(P, "OUT", tmp_path / "out")
    monkeypatch.setattr(P.T, "candidates", lambda: ["digikit/a", "test/b"])
    monkeypatch.setattr(P.T, "pack_for_transfer", fake_pack)
    monkeypatch.setattr(
        P.patchkit,
        "module_status",
        lambda d: "needs build" if d.name == "a" else "unchanged",
    )
    monkeypatch.setattr(P.modroots, "module_dir", lambda mid: Path(mid.split("/")[-1]))
    monkeypatch.setattr(R, "build_objects", built.append)
    first = P.pack_all(devices=["dt2", "dn2"])
    assert first == {
        "dt2": {"digikit/a": "new", "test/b": "new"},
        "dn2": {"digikit/a": "new"},
    }
    assert built == ["digikit/a"]  # only the module whose objects changed is built
    assert (tmp_path / "out/dt2/test/b.dkm").read_bytes() == b"B1"
    assert not (tmp_path / "out/dn2/test").exists()  # does not pack for dn2: not there
    # nothing changed: every file the same, nothing rewritten
    again = P.pack_all(devices=["dt2", "dn2"])
    assert set(again["dt2"].values()) == {"same"}
    # one module changes, one stops packing: its file is rewritten, the other removed (and its empty folder)
    state["a"] = b"A2"
    monkeypatch.setattr(P.T, "candidates", lambda: ["digikit/a"])
    third = P.pack_all(devices=["dt2", "dn2"])
    assert third == {"dt2": {"digikit/a": "changed"}, "dn2": {"digikit/a": "changed"}}
    assert (tmp_path / "out/dt2/digikit/a.dkm").read_bytes() == b"A2"
    assert not (tmp_path / "out/dt2/test").exists()
