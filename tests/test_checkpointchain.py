"""Synthetic source-chain checks; no firmware or emulator execution required."""

import hashlib
import json
from pathlib import Path
from types import SimpleNamespace
from typing import cast

import pytest

from tools import checkpointchain, checkpointprep
from tools.snapread import Snapshot


def digest(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


@pytest.fixture
def local_source(tmp_path, monkeypatch):
    syx = tmp_path / "firmware.syx"
    syx.write_bytes(b"toy source")
    section = tmp_path / "out/sections/toy"
    section.mkdir(parents=True)
    (section / ".source-sha256").write_text(digest(syx.read_bytes()) + "\n")
    image = section / "section_3_MAIN_OS.bin"
    image.write_bytes(b"toy image")
    snapshot = tmp_path / "out/early.snap"
    snapshot.write_bytes(b"toy checkpoint")
    monkeypatch.setattr(checkpointchain, "ROOT", tmp_path)
    monkeypatch.setitem(
        checkpointprep.PRODUCTS,
        "dt2",
        checkpointprep.Product(
            digest(syx.read_bytes()),
            digest(image.read_bytes()),
            "out/sections/toy",
            "out/early.snap",
            "out/unused.mstate",
        ),
    )
    checked = []
    monkeypatch.setattr(
        checkpointprep,
        "verify",
        lambda product, source, path: (
            checked.append((product, source, path)) or SimpleNamespace(path=path)
        ),
    )
    return syx, snapshot, checked


def test_anchor_rechecks_source_and_snapshot_before_writing(local_source, tmp_path):
    syx, snapshot, checked = local_source
    ledger = checkpointchain.anchor("dt2", syx, snapshot)
    assert checked == [("dt2", syx, snapshot)]
    assert checkpointchain.verify(ledger, syx)["snapshot_sha256"] == digest(
        snapshot.read_bytes()
    )
    assert ledger.is_relative_to(tmp_path / "out/native/checkpoint-chain")
    snapshot.write_bytes(b"tampered checkpoint")
    with pytest.raises(ValueError, match="snapshot SHA-256 mismatch"):
        checkpointchain.verify(ledger, syx)


def test_capture_refuses_untrusted_parent_without_running(local_source, monkeypatch):
    syx, snapshot, _ = local_source
    ledger = checkpointchain.anchor("dt2", syx, snapshot)
    snapshot.write_bytes(b"not the anchored checkpoint")
    monkeypatch.setattr(
        checkpointchain.subprocess,
        "run",
        lambda *_args, **_kwargs: pytest.fail("must not start the emulator"),
    )
    with pytest.raises(ValueError, match="snapshot SHA-256 mismatch"):
        checkpointchain.capture(ledger, syx, 1000)


def test_portable_refuses_tampered_chain_without_writing(local_source):
    syx, snapshot, _ = local_source
    ledger = checkpointchain.anchor("dt2", syx, snapshot)
    snapshot.write_bytes(b"tampered checkpoint")
    with pytest.raises(ValueError, match="snapshot SHA-256 mismatch"):
        checkpointchain.portable(ledger, syx)
    assert not (ledger.parent / "portable.mstate").exists()


def test_portable_rechecks_private_input_after_copy(local_source, monkeypatch):
    syx, snapshot, _ = local_source
    ledger = checkpointchain.anchor("dt2", syx, snapshot)
    monkeypatch.setattr(
        checkpointchain.shutil,
        "copyfile",
        lambda _source, destination: destination.write_bytes(b"replaced during copy"),
    )
    with pytest.raises(
        ValueError, match="private portable input snapshot SHA-256 mismatch"
    ):
        checkpointchain.portable(ledger, syx)
    assert not (ledger.parent / "portable.mstate").exists()


def test_first_events_requires_instruction_clock_and_nonzero_read_write(
    local_source, monkeypatch
):
    syx, snapshot, _ = local_source
    ledger = checkpointchain.anchor("dt2", syx, snapshot)
    trace = ledger.parent / "capture.mmio"
    trace.write_bytes(b"synthetic trace")
    monkeypatch.setattr(
        checkpointchain,
        "verify",
        lambda *_args: {
            "kind": "derived",
            "done": 8,
            "icount": True,
            "parent": str(ledger),
            "trace_sha256": checkpointchain.sha256(trace),
        },
    )

    class Reader:
        header = {"clock_resolution": "instruction"}

        def __init__(self, _path):
            pass

        def __iter__(self):
            return iter(
                [
                    SimpleNamespace(tag=checkpointchain.mmiotrace.TIME, clock=8),
                    SimpleNamespace(
                        tag=checkpointchain.mmiotrace.RD,
                        clock=11,
                        fields=(0xFC08C000, 0x1234, 0x40001000, 2),
                    ),
                    SimpleNamespace(
                        tag=checkpointchain.mmiotrace.WR,
                        clock=12,
                        fields=(0xFC08C000, 0x56, 0x40001002, 1),
                    ),
                ]
            )

    monkeypatch.setattr(checkpointchain.mmiotrace, "Reader", Reader)
    output = checkpointchain.first_events(ledger, syx, 2)
    values = checkpointchain.json.loads(output.read_text())
    assert [(event["kind"], event["step"]) for event in values["events"]] == [
        ("RD", 3),
        ("WR", 4),
    ]
    original_iter = Reader.__iter__

    def two_reads_before_write(self):
        records = list(original_iter(self))
        records.insert(2, records[1])
        return iter(records)

    monkeypatch.setattr(Reader, "__iter__", two_reads_before_write)
    with pytest.raises(ValueError, match="first event window lacks both"):
        checkpointchain.first_events(ledger, syx, 2)
    monkeypatch.setattr(Reader, "__iter__", original_iter)
    Reader.header["clock_resolution"] = "step"
    with pytest.raises(ValueError, match="instruction-accurate"):
        checkpointchain.first_events(ledger, syx, 2)
    Reader.header["clock_resolution"] = "instruction"
    monkeypatch.setattr(
        checkpointchain.shutil,
        "copyfile",
        lambda _source, destination: destination.write_bytes(b"replaced during copy"),
    )
    with pytest.raises(ValueError, match="private event trace SHA-256 mismatch"):
        checkpointchain.first_events(ledger, syx, 2)


def test_native_mmio_gate_binds_parent_and_private_outputs(local_source, monkeypatch):
    syx, snapshot, _ = local_source
    parent = checkpointchain.anchor("dt2", syx, snapshot)
    derived = parent.parent / "derived.json"
    calls = []

    def checked(ledger, _syx):
        return {
            "kind": "derived",
            "product": "dt2",
            "parent": str(parent if ledger == derived else derived),
        }

    def make_state(_ledger, _syx, *, output):
        output.write_bytes(b"private state")
        return output

    def make_events(_ledger, _syx, _count, *, output):
        output.write_bytes(b"private events")
        return output

    def native_run(command, *, cwd, env, timeout, check):
        assert cwd == checkpointchain.ROOT
        assert timeout <= 120 and check
        assert "--ignored" in command
        state = Path(env["DT2_LOCAL_MSTATE"])
        events = Path(env["DT2_LOCAL_EVENTS"])
        assert state.read_bytes() == b"private state"
        assert events.read_bytes() == b"private events"
        assert state.parent == events.parent
        calls.append(command)

    monkeypatch.setattr(checkpointchain, "verify", checked)
    monkeypatch.setattr(checkpointchain, "portable", make_state)
    monkeypatch.setattr(checkpointchain, "first_events", make_events)
    monkeypatch.setattr(checkpointchain.subprocess, "run", native_run)
    checkpointchain.first_mmio_gate(parent, derived, syx, 6)
    assert len(calls) == 1

    def wrong_parent(_ledger, _syx):
        return {"kind": "derived", "product": "dt2", "parent": str(derived)}

    monkeypatch.setattr(checkpointchain, "verify", wrong_parent)
    with pytest.raises(ValueError, match="not a direct child"):
        checkpointchain.first_mmio_gate(parent, derived, syx, 6)
    assert len(calls) == 1


def test_cpu_ram_reference_uses_private_verified_inputs(local_source, monkeypatch):
    syx, snapshot, _ = local_source
    parent = checkpointchain.anchor("dt2", syx, snapshot)
    seen = []

    class Machine:
        uc = object()

        def close(self):
            seen.append("closed")

    def fake_build(private_snapshot, *, syx, **kwargs):
        assert Path(private_snapshot).read_bytes() == b"toy checkpoint"
        assert Path(syx).read_bytes() == b"toy source"
        assert Path(private_snapshot) != snapshot
        assert Path(syx) != local_source[0]
        assert checkpointchain.os.environ["DT2_SECTIONS"].endswith("out/sections/toy")
        assert kwargs["deferred_components"] == ("timers",)
        seen.append("built")
        ev = {
            "restore_checkpoint_timers": lambda: object(),
            "checkpoint_manifest": {"main_sha256": digest(b"toy image")},
        }
        return Machine(), ev, None, 0x4000, None, None

    monkeypatch.setattr("emu.longrun.build", fake_build)
    monkeypatch.setattr(
        checkpointchain.checkpointcpu,
        "capture_window",
        lambda _uc, pc, limit, every, _regs: {
            "limit": limit,
            "every": every,
            "samples": [{"step": 0, "regs": {"pc": pc}}],
            "effects": [],
        },
    )
    monkeypatch.setenv("DT2_SECTIONS", "previous value")
    events = parent.parent / "first-events.json"
    events.write_text('{"events":[]}')
    output = checkpointchain.cpu_ram_reference(
        parent, syx, 4, 2, events=events, output=parent.parent / "cpu-ram.json"
    )
    assert seen == ["built", "closed"]
    assert checkpointchain.os.environ["DT2_SECTIONS"] == "previous value"
    assert json.loads(output.read_text())["product"] == "dt2"


def test_cpu_ram_reference_rehashes_copied_snapshot(local_source, monkeypatch):
    syx, snapshot, _ = local_source
    parent = checkpointchain.anchor("dt2", syx, snapshot)
    events = parent.parent / "first-events.json"
    events.write_text('{"events":[]}')
    monkeypatch.setattr(
        checkpointchain.shutil,
        "copyfile",
        lambda _source, destination: destination.write_bytes(b"replaced during copy"),
    )
    with pytest.raises(
        ValueError, match="private CPU/RAM input snapshot SHA-256 mismatch"
    ):
        checkpointchain.cpu_ram_reference(
            parent, syx, 4, 2, events=events, output=parent.parent / "cpu-ram.json"
        )
    assert not (parent.parent / "cpu-ram.json").exists()


def test_cpu_ram_reference_rejects_unbounded_limit_before_restore(
    local_source, monkeypatch
):
    syx, snapshot, _ = local_source
    parent = checkpointchain.anchor("dt2", syx, snapshot)
    events = parent.parent / "first-events.json"
    events.write_text('{"events":[]}')
    monkeypatch.setattr(
        "emu.longrun.build", lambda *_args, **_kwargs: pytest.fail("must not restore")
    )
    with pytest.raises(ValueError, match="CPU/RAM window"):
        checkpointchain.cpu_ram_reference(
            parent,
            syx,
            checkpointchain.checkpointcpu.MAX_STEPS + 1,
            1024,
            events=events,
            output=parent.parent / "cpu-ram.json",
        )


def test_verify_refuses_forged_child_before_parsing_snapshot(local_source, monkeypatch):
    syx, snapshot, _ = local_source
    parent = checkpointchain.anchor("dt2", syx, snapshot)
    run_dir = parent.parent / "derived"
    run_dir.mkdir()
    final = run_dir / "final.snap"
    final.write_bytes(b"toy derived")
    trace = run_dir / "capture.mmio"
    trace.write_bytes(b"toy trace")
    child = run_dir / "chain.json"
    child.write_text(
        json.dumps(
            {
                "version": 1,
                "kind": "derived",
                "product": "dt2",
                "source_sha256": digest(syx.read_bytes()),
                "image_sha256": checkpointprep.PRODUCTS["dt2"].image_sha256,
                "parent": str(parent),
                "parent_sha256": checkpointchain.sha256(parent),
                "input_snapshot_sha256": checkpointchain.sha256(snapshot),
                "snapshot_sha256": checkpointchain.sha256(final),
                "trace_sha256": "0" * 64,
                "limit": 1000,
                "done": 1000,
            }
        )
    )
    monkeypatch.setattr(
        checkpointchain,
        "_validate_derived_payload",
        lambda *_args: pytest.fail("must not parse an untrusted payload"),
    )
    with pytest.raises(ValueError, match="trace SHA-256 mismatch"):
        checkpointchain.verify(child, syx)
    child_data = json.loads(child.read_text())
    child_data["trace_sha256"] = checkpointchain.sha256(trace)
    child.write_text(json.dumps(child_data))
    parent.write_text(parent.read_text() + " ")
    with pytest.raises(ValueError, match="parent ledger SHA-256 mismatch"):
        checkpointchain.verify(child, syx)


def test_manifest_comparison_uses_the_json_trace_representation():
    # The trace header converts tuples to JSON lists; the pickle snapshot does not.
    assert checkpointchain._same_json({"unblock_except": []}, {"unblock_except": ()})
    assert not checkpointchain._same_json(
        {"unblock_except": [1]}, {"unblock_except": ()}
    )


def test_capture_passes_a_verified_private_input_to_the_recorder(
    local_source, monkeypatch
):
    syx, snapshot, _ = local_source
    parent = checkpointchain.anchor("dt2", syx, snapshot)

    class RecorderCalled(Exception):
        pass

    def inspect(args, **_kwargs):
        copied = checkpointchain.Path(args[3])
        assert "--icount" in args
        assert copied != snapshot
        assert copied.is_relative_to(checkpointchain._directory())
        assert copied.read_bytes() == snapshot.read_bytes()
        raise RecorderCalled

    monkeypatch.setattr(checkpointchain.subprocess, "run", inspect)
    with pytest.raises(RecorderCalled):
        checkpointchain.capture(parent, syx, 1000, icount=True)


def test_derived_image_check_rejects_modified_main_os(local_source):
    _syx, _snapshot, _ = local_source

    class DerivedSnapshot:
        def read(self, _addr, _size):
            return b"modified code"

    with pytest.raises(ValueError, match="derived loaded MAIN OS mismatch"):
        checkpointchain._check_derived_image("dt2", cast(Snapshot, DerivedSnapshot()))


def test_timer_step_may_exceed_idle_interval(monkeypatch, tmp_path):
    # Active timer periods have no 1M cap; a completed step may overshoot the
    # requested floor by more than the no-active-timer idle fallback.
    actual = 2_000_001
    monkeypatch.setattr(
        checkpointchain,
        "Snapshot",
        lambda _path: SimpleNamespace(
            _blob={"manifest": {}, "extra": {"mmio_record_done": actual}}
        ),
    )
    monkeypatch.setattr(checkpointchain, "_check_derived_image", lambda *_: None)

    class Reader:
        header = {
            "syx": {"sha256": "source"},
            "main_image": {"sha256": "image"},
            "snapshot": {"sha256": "parent"},
            "instrs": 1000,
            "manifest": {},
            "build": {"slc": False},
            "clock_resolution": "instruction",
        }

        def __init__(self, _path):
            pass

        def blobs(self, _tag):
            return iter([(0, {"stop": "limit", "done": actual, "errors": 0})])

    monkeypatch.setattr(checkpointchain.mmiotrace, "Reader", Reader)
    entry = {
        "product": "dt2",
        "source_sha256": "source",
        "image_sha256": "image",
        "input_snapshot_sha256": "parent",
        "limit": 1000,
        "done": actual,
        "icount": True,
    }
    checkpointchain._validate_derived_payload(
        entry,
        tmp_path / "snapshot",
        tmp_path / "trace",
    )
    Reader.header["clock_resolution"] = "step"
    with pytest.raises(ValueError, match="capture header/source manifest mismatch"):
        checkpointchain._validate_derived_payload(
            entry, tmp_path / "snapshot", tmp_path / "trace"
        )


def test_changed_main_image_requires_equal_clock_control(
    local_source, monkeypatch, tmp_path
):
    _syx, _snapshot, _ = local_source

    class ChangedSnapshot:
        path = "recorded.snap"
        _blob = {
            "components": {"timers": {"held": False}},
            "manifest": {},
            "extra": {"mmio_record_done": 10},
        }

        def read(self, _addr, _size):
            return b"modified code"

    monkeypatch.setattr(checkpointchain, "Snapshot", lambda _path: ChangedSnapshot())
    monkeypatch.setattr(checkpointchain.snapeq, "compare", lambda *_: None)
    control = tmp_path / "control.snap"
    checkpointchain._check_derived_image(
        "dt2", cast(Snapshot, ChangedSnapshot()), control
    )
    monkeypatch.setattr(
        checkpointchain.snapeq, "compare", lambda *_: "synthetic mismatch"
    )
    with pytest.raises(ValueError, match="control mismatch"):
        checkpointchain._check_derived_image(
            "dt2", cast(Snapshot, ChangedSnapshot()), control
        )
    monkeypatch.setattr(checkpointchain.snapeq, "compare", lambda *_: None)
    original = checkpointchain.Snapshot

    class WrongCount(ChangedSnapshot):
        _blob = {**ChangedSnapshot._blob, "extra": {"mmio_record_done": 11}}

    monkeypatch.setattr(checkpointchain, "Snapshot", lambda _path: WrongCount())
    with pytest.raises(ValueError, match="control completion count mismatch"):
        checkpointchain._check_derived_image(
            "dt2", cast(Snapshot, ChangedSnapshot()), control
        )
    monkeypatch.setattr(checkpointchain, "Snapshot", original)
