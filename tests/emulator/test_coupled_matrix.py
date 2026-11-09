"""tools/emulator/coupled_matrix.py: job planning, environments, aggregation, the report."""

import json
from pathlib import Path

import pytest
from support.paths import REPO_ROOT

ROOT = REPO_ROOT
from tools.emulator import coupled_matrix as cm  # noqa: E402


def fake_report(name, dsp=1.9, rtf=0.5, over=1.0, ok=True, sha="ab" * 32):
    return {
        "name": name,
        "audio_seconds": 10.0,
        "wall_seconds": 10.0 / rtf,
        "real_time_factor": rtf,
        "cpu": {"dsp_thread_per_audio_s": dsp, "cf_thread_per_audio_s": 0.7},
        "dsp_period_wall": {
            "over_deadline_fraction": over,
            "p50_ms": 1.1,
            "p99_ms": 1.9,
            "max_ms": 9.0,
        },
        "dsp_instructions": {
            "idle_skip_pct": 50.0,
            "interpreter_pct": 0.2,
            "aot_block_pct": 32.0,
            "fast_tier_pct": 17.8,
        },
        "cf_instructions": {"interpreted_pct": 68.0, "idle_fast_forward_pct": 32.0},
        "pcm": {"rms": 0.25, "peak": 0.69, "sha256": sha},
        "gates": {"ok": ok},
    }


def test_plan_overrides_and_no_validation():
    jobs = cm.plan(list(cm.CONFIGS.values()), ["idle"], 2, 3.0, False)
    assert len(jobs) == 6
    assert {j.seconds for j in jobs} == {3.0}
    assert {j.repeat for j in jobs} == {1, 2}


def test_list_forwards_region_profile_to_the_plan(monkeypatch):
    seen = {}

    def capture_plan(*args):
        seen["profile_regions"] = args[-1]
        return []

    monkeypatch.setattr(cm, "plan", capture_plan)
    assert cm.main(["--profile-regions", "--list"]) == 0
    assert seen["profile_regions"] is True


def test_config_env_clears_tuning_and_sets_the_configs(monkeypatch):
    monkeypatch.setattr(cm, "launcher_fast_regions", lambda syx: "0x1,0x2")
    base = {"PATH": "/bin", "SHARC_FAST_AUTO": "9,9", "NOTE_EVENTS": "trig"}
    prod = cm.config_env(cm.CONFIGS["prod"], Path("x.syx"), base)
    assert prod["SHARC_FAST_REGIONS"] == "0x1,0x2"
    assert "SHARC_FAST_AUTO" not in prod and "NOTE_EVENTS" not in prod
    assert prod["SHARC_GEN_DIR"].endswith("dn2-audio/gen")
    generic = cm.config_env(cm.CONFIGS["generic"], Path("x.syx"), base)
    assert "SHARC_FAST_REGIONS" not in generic
    assert generic["SHARC_GEN_DIR"].endswith("gen-generic")
    auto = cm.config_env(cm.CONFIGS["generic-auto"], Path("x.syx"), base)
    assert auto["SHARC_FAST_AUTO"] == "256,64"


def test_aggregate_takes_medians_and_flags_failures_and_hash_splits():
    results = [
        {
            "config": "prod",
            "scenario": "idle",
            "repeat": r,
            "report": fake_report("idle", dsp=d, rtf=rtf),
        }
        for r, (d, rtf) in enumerate([(1.8, 0.55), (2.1, 0.47), (1.9, 0.52)], 1)
    ]
    results.append({"config": "prod", "scenario": "idle", "repeat": 4, "error": "boom"})
    a = cm.aggregate(results)["prod/idle"]
    assert (a["runs"], a["failed"]) == (4, 1)
    assert a["median"]["dsp_cpu_per_audio_s"] == pytest.approx(1.9)
    assert a["median"]["over_deadline_pct"] == pytest.approx(100.0)
    assert a["errors"] == ["boom"] and a["gates_ok"] is True
    bad = results + [
        {
            "config": "prod",
            "scenario": "idle",
            "repeat": 5,
            "report": fake_report("idle", ok=False, sha="cd" * 32),
        }
    ]
    b = cm.aggregate(bad)["prod/idle"]
    assert b["gates_ok"] is False and len(b["pcm_sha256"]) == 2


def test_run_matrix_writes_report_and_summary(tmp_path, monkeypatch):
    monkeypatch.setattr(cm, "launcher_fast_regions", lambda syx: "0x1")
    monkeypatch.setattr(cm, "git_state", lambda: "abc1234")
    monkeypatch.setattr(cm, "host_description", lambda: "test host")
    monkeypatch.setattr(cm, "rustc_version", lambda: "rustc test")
    seen = []

    def fake_runner(binary, job, syx, env, raw_dir, timeout):
        seen.append((job.config, job.scenario, job.repeat, env["SHARC_GEN_DIR"]))
        if job.scenario == "kb-chord16":
            return {"error": "exit 1: crash", "load1": 3.0}
        return {**fake_report(job.scenario), "load1": 0.2}

    report = cm.run_matrix(
        [cm.CONFIGS["prod"]],
        ["idle", "kb-chord16"],
        1,
        2.0,
        True,
        None,
        Path("x.syx"),
        tmp_path / "20260101-000000",
        60.0,
        builder=lambda c, pgo, syx, profile_regions: Path(f"/bin/{c.name}"),
        runner=fake_runner,
    )
    out = tmp_path / "20260101-000000"
    assert json.loads((out / "report.json").read_text())["meta"]["git"] == "abc1234"
    summary = (out / "summary.md").read_text()
    assert "| prod | idle | 1/1 |" in summary
    assert "| prod | exactness-100m |" in summary  # validation table
    assert "prod/kb-chord16: exit 1: crash" in summary
    assert report["aggregate"]["prod/kb-chord16"]["gates_ok"] is False
    assert [s[:3] for s in seen] == [
        ("prod", "exactness-100m", 0),
        ("prod", "trig1-noqa-40m", 0),
        ("prod", "idle", 1),
        ("prod", "kb-chord16", 1),
    ]
