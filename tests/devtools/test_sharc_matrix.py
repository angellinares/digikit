"""Matrix checkpoints represent whole, validated pairs; no firmware required."""

import json
import struct
import sys

import pytest

from tools.devtools import sharc_matrix as matrix

SOURCE_HASHES = matrix.tool_hashes


def put(path, data):
    path.write_text(json.dumps(data))
    return str(path)


@pytest.fixture
def setup(tmp_path, monkeypatch):
    monkeypatch.setattr(matrix, "tool_hashes", lambda: {"runner": "stable"})
    monkeypatch.setattr(matrix.render_cache, "devices_digest", lambda env: {})
    monkeypatch.setenv("BENCHLOCK_PATH", str(tmp_path / "benchlock"))
    monkeypatch.delenv("BENCHLOCK", raising=False)
    binary = tmp_path / "binary"
    binary.write_bytes(b"fake executable")
    firmware = tmp_path / "stock.syx"
    firmware.write_bytes(b"stock")
    manifest = tmp_path / "import.json"
    put(
        manifest,
        {
            device: {
                "coupled": str(binary),
                "daemon": str(binary),
                "profile_firmware": str(firmware),
            }
            for device in matrix.DEVICES
        },
    )
    monkeypatch.setattr(
        matrix.digi_env, "fast_env", lambda *args, **kwargs: {"SHARC_FAST_AUTO": "1"}
    )
    frozen = tmp_path / "frozen"
    matrix.freeze(frozen, manifest)
    scenario = tmp_path / "scenarios.json"
    put(scenario, {"scenarios": [{"name": "note", "seconds": 3}]})
    config = {
        "cells": [
            {
                "id": "dt2-stock",
                "device": "dt2",
                "kind": "stock",
                "inputs": {key: str(firmware) for key in matrix.INPUTS},
                "scenarios": str(scenario),
                "names": ["note"],
            }
        ],
        "instruments": [],
        "pairs": 2,
    }
    config_path = tmp_path / "matrix.json"
    put(config_path, config)
    calls = []

    def replay(binary, cell, scenario, env, seconds, folder, timeout):
        folder.mkdir(parents=True)
        (folder / "report.json").write_text("original child report")
        calls.append(str(folder))
        return {
            "pcm": "pcm",
            "state": "state",
            "icount": 10,
            "dsp_cpu": 2,
            "audio_s": 3,
            "tiers": {"interpreter": 5},
        }

    monkeypatch.setattr(matrix, "replay", replay)
    return config_path, frozen, tmp_path / "run", calls, replay


def test_resume_checks_artifacts_and_inputs(setup):
    config, frozen, out, calls, _ = setup
    assert matrix.run(config, frozen, frozen, out, True)["status"] == "partial-pass"
    assert len(calls) == 6
    matrix.run(config, frozen, frozen, out, True)
    assert len(calls) == 6
    report = out / "timing/dt2-stock/note/0/A/report.json"
    report.write_text("tampered")
    with pytest.raises(matrix.Invalid, match="artifact"):
        matrix.run(config, frozen, frozen, out, True)


def test_resume_rejects_changed_config_and_binary(setup):
    config, frozen, out, _, _ = setup
    matrix.run(config, frozen, frozen, out, True)
    config.write_text(config.read_text() + " ")
    with pytest.raises(matrix.Invalid, match="identity"):
        matrix.run(config, frozen, frozen, out, True)
    (frozen / "dt2-coupled").write_bytes(b"changed binary")
    with pytest.raises(matrix.Invalid, match="frozen file"):
        matrix.run(config, frozen, frozen, out, True)


def test_failure_stops_before_timings(setup, monkeypatch):
    config, frozen, out, calls, replay = setup

    def bad(*args):
        result = replay(*args)
        if str(args[-2]).endswith("/B"):
            result["state"] = "different"
        return result

    monkeypatch.setattr(matrix, "replay", bad)
    with pytest.raises(matrix.Invalid, match="mismatch"):
        matrix.run(config, frozen, frozen, out, True)
    assert len(calls) == 2
    assert all("correctness" in call for call in calls)
    assert matrix.read(out / "checkpoint.json")["steps"] == {}
    assert matrix.read(out / "summary.json")["status"] == "failed"


def test_partial_pair_restarts_both_sides(setup, monkeypatch):
    config, frozen, out, calls, replay = setup
    failed = False

    def interrupted(*args):
        nonlocal failed
        if "timing" in str(args[-2]) and str(args[-2]).endswith("/B") and not failed:
            failed = True
            raise matrix.Invalid("interrupted")
        return replay(*args)

    monkeypatch.setattr(matrix, "replay", interrupted)
    with pytest.raises(matrix.Invalid, match="interrupted"):
        matrix.run(config, frozen, frozen, out, True)
    assert len(calls) == 3  # correctness A+B, partial timing A
    matrix.run(config, frozen, frozen, out, True)
    assert len(calls) == 7  # reruns timing A as well as B
    assert sum(call.endswith("timing/dt2-stock/note/0/A") for call in calls) == 2


def test_full_coverage_and_env_precedence(setup, monkeypatch):
    config_path, frozen_path, _, _, _ = setup
    config = matrix.read(config_path)
    frozen = matrix.frozen(frozen_path)
    with pytest.raises(matrix.Invalid, match="dn2/runtime"):
        matrix.preflight(config, frozen, frozen, False)
    config["env"] = {"SHARC_FAST_AUTO": "2", "SHARC_AOT": "1"}
    config["cells"][0]["env"] = {"SHARC_FAST_AUTO": "3"}
    config["variants"] = {"B": {"SHARC_FAST_AUTO": None, "SHARC_AOT": "0"}}
    monkeypatch.setenv("SHARC_PCDUMP", "secret-ambient-diagnostic")
    result = matrix.preflight(config, frozen, frozen, True)
    assert result["env"]["dt2-stock/A"] == {"SHARC_FAST_AUTO": "3", "SHARC_AOT": "1"}
    assert result["env"]["dt2-stock/B"] == {"SHARC_AOT": "0"}
    assert "SHARC_PCDUMP" not in matrix.child_env({})
    for device in matrix.DEVICES:
        for kind in ("stock", "runtime"):
            cell = {
                **config["cells"][0],
                "id": f"{device}-{kind}",
                "device": device,
                "kind": kind,
            }
            if cell["id"] != "dt2-stock":
                config["cells"].append(cell)
    config["instruments"] = list(matrix.DEVICES)
    assert not matrix.preflight(config, frozen, frozen, False)["missing"]


def test_child_logs_success_failure_and_timeout(tmp_path):
    matrix.process([sys.executable, "-c", "print('done')"], {}, tmp_path / "ok", 5)
    assert "done" in (tmp_path / "ok/stdout.log").read_text()
    with pytest.raises(matrix.Invalid, match="exit 7"):
        matrix.process(
            [sys.executable, "-c", "raise SystemExit(7)"], {}, tmp_path / "bad", 5
        )
    with pytest.raises(matrix.Invalid, match="timed out"):
        matrix.process(
            [sys.executable, "-c", "import time; time.sleep(30)"],
            {},
            tmp_path / "timeout",
            0.05,
        )
    assert (tmp_path / "timeout/command.json").is_file()


def test_full_float_wav_pcm(tmp_path):
    pcm = struct.pack("<ff", 0.5, -0.5)
    fmt = struct.pack("<HHIIHH", 3, 2, 1, 8, 8, 32)
    payload = (
        b"WAVEfmt "
        + struct.pack("<I", len(fmt))
        + fmt
        + b"data"
        + struct.pack("<I", len(pcm))
        + pcm
    )
    path = tmp_path / "audio.wav"
    path.write_bytes(b"RIFF" + struct.pack("<I", len(payload)) + payload)
    assert matrix.wav_pcm(path) == (matrix.hashlib.sha256(pcm).hexdigest(), 1)


def test_persistent_sample_validates_original_artifacts(tmp_path):
    binary = tmp_path / "fake-coupled"
    binary.write_text(f"""#!{sys.executable}
import hashlib, json, pathlib, struct, sys
args = dict(zip(sys.argv[1::2], sys.argv[2::2]))
name = args['--scenario']
folder = pathlib.Path(args['--out']).parent
pcm = struct.pack('<ff', 0.5, -0.5) * 3
fmt = struct.pack('<HHIIHH', 3, 2, 1, 8, 8, 32)
payload = b'WAVEfmt ' + struct.pack('<I', len(fmt)) + fmt + b'data' + struct.pack('<I', len(pcm)) + pcm
pathlib.Path(args['--wav']).write_bytes(b'RIFF' + struct.pack('<I', len(payload)) + payload)
(folder / ('dsp-' + name + '.bin')).write_bytes(b'SHRDcomplete-state')
sc = dict(name=name, gates={{'ok':True, 'missing_blocks':0, 'halted':None, 'step_error':None, 'pcm_non_finite':0, 'guard_ok':True}}, audio_seconds=3, pcm={{'sha256':hashlib.sha256(pcm).hexdigest()}},
          cpu={{'dsp_thread_per_audio_s':2, 'cf_thread_per_audio_s':1}},
          dsp_dump={{'dsp_icount':100}}, dsp_instructions={{'interpreter':100}})
pathlib.Path(args['--out']).write_text(json.dumps({{'scenarios':[sc]}}))
""")
    binary.chmod(0o755)
    inputs = {key: tmp_path / "unused" for key in matrix.INPUTS}
    scenarios = tmp_path / "scenarios.json"
    put(scenarios, {"scenarios": [{"name": "note"}]})
    result = matrix.coupled_ab.artifact_run(
        str(binary),
        "note",
        str(scenarios),
        inputs=inputs,
        env=matrix.child_env({}),
        seconds=3,
        folder=tmp_path / "sample",
        timeout=5,
    )
    assert result["icount"] == 100
    assert result["audio_s"] == 3
    assert matrix.read(tmp_path / "sample/exit.json") == {"exit_code": 0}


def test_changed_recursive_port_rejects_resume_and_status(setup, monkeypatch):
    config, frozen, out, calls, _ = setup
    # Exercise the real source hasher on a small tree with a nested port TOML.
    root = config.parent / "source"
    port = root / "devices" / "ports" / "dt2" / "nested" / "anchors.toml"
    port.parent.mkdir(parents=True)
    port.write_text("anchor = 1\n")
    monkeypatch.setattr(matrix, "ROOT", root)
    monkeypatch.setattr(matrix, "tool_hashes", SOURCE_HASHES)
    assert SOURCE_HASHES() == {str(port.relative_to(root)): matrix.digest(port)}
    matrix.run(config, frozen, frozen, out, True)
    assert matrix.status(out)["status"] == "partial-pass"
    port.write_text("anchor = 2\n")
    with pytest.raises(matrix.Invalid, match="identity"):
        matrix.status(out)
    with pytest.raises(matrix.Invalid, match="identity"):
        matrix.run(config, frozen, frozen, out, True)
    assert len(calls) == 6


def test_status_rejects_frozen_manifest_edit(setup):
    config, frozen, out, _, _ = setup
    matrix.run(config, frozen, frozen, out, True)
    manifest = frozen / "frozen.json"
    manifest.write_text(manifest.read_text() + " ")
    with pytest.raises(matrix.Invalid, match="input changed"):
        matrix.status(out)


@pytest.mark.parametrize(
    "mutation", ["config", "input", "binary", "manifest", "tools", "devices"]
)
def test_mid_run_identity_mutation_cannot_pass(setup, monkeypatch, mutation):
    config, frozen, out, calls, replay = setup

    def changed(*args):
        result = replay(*args)
        if len(calls) == 6:  # Final side of final timing pair, before completion.
            if mutation == "config":
                config.write_text(config.read_text() + " ")
            elif mutation == "input":
                (config.parent / "stock.syx").write_bytes(b"changed stock")
            elif mutation == "binary":
                (frozen / "dt2-coupled").write_bytes(b"changed executable")
            elif mutation == "manifest":
                manifest = frozen / "frozen.json"
                manifest.write_text(manifest.read_text() + " ")
            elif mutation == "tools":
                monkeypatch.setattr(
                    matrix, "tool_hashes", lambda: {"runner": "changed"}
                )
            elif mutation == "devices":
                monkeypatch.setattr(
                    matrix.render_cache,
                    "devices_digest",
                    lambda env: {"dt2.toml": "changed"},
                )
        return result

    monkeypatch.setattr(matrix, "replay", changed)
    with pytest.raises(matrix.Invalid):
        matrix.run(config, frozen, frozen, out, True)
    state = matrix.read(out / "checkpoint.json")
    assert state["status"] == "failed"
    assert state["failure_stage"] == "completion/identity"
    assert matrix.read(out / "summary.json")["status"] == "failed"
