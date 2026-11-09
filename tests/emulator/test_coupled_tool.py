"""coupled: pure helpers of tools/emulator/coupled.py (no cargo, no emulator)."""

from pathlib import Path

import tools.emulator.coupled as coupled


def test_bisect_configs_labels_and_overrides():
    configs = coupled.bisect_configs(
        {"SHARC_FAST_REGIONS": "0x1,0x2", "SHARC_BLOCKTRANS": "0x9"}
    )
    assert [label for label, _ in configs] == [
        "no-fast",
        "no-aot",
        "fast-minus-0x1",
        "fast-minus-0x2",
        "roots-minus-0x9",
    ]
    env = dict(configs)
    assert env["fast-minus-0x1"]["SHARC_FAST_REGIONS"] == "0x2"
    assert env["fast-minus-0x1"]["SHARC_BLOCKTRANS"] == "0x9"
    assert env["no-aot"]["SHARC_AOT"] == "0"


def test_first_jump():
    assert coupled.first_jump([0.1, 0.1, 0.4, 0.4]) == 2
    assert coupled.first_jump([0.1, 0.11]) is None
    assert coupled.first_jump([0.0, 0.0, 0.2]) == 2


def test_first_divergence():
    assert coupled.first_divergence([1.0, 2.0, 3.0], [1.0, 2.0, 3.5]) == 2
    assert coupled.first_divergence([1.0, 2.0], [1.0, 2.0]) is None


def test_scenario_results_reads_the_report_keys():
    report = {
        "scenarios": [
            {
                "name": "idle",
                "pcm": {"sha256": "ab" * 32, "window_rms_100ms": [0.0, 0.5]},
                "gates": {"ok": True},
            }
        ]
    }
    [result] = coupled.scenario_results(report)
    assert result["name"] == "idle"
    assert result["sha256"] == "ab" * 32
    assert result["ok"] is True
    assert result["windows"] == [0.0, 0.5]


def test_shard_names_balances_and_keeps_order():
    names = ["a", "b", "c", "d", "e"]
    secs = {"a": 4.0, "b": 1.0, "c": 3.0, "d": 1.0, "e": 3.0}
    shards = coupled.shard_names(names, secs, 2)
    assert sorted(n for s in shards for n in s) == names
    for s in shards:
        assert s == sorted(s, key=names.index)
    loads = sorted(sum(secs[n] for n in s) for s in shards)
    assert loads == [6.0, 6.0]
    assert coupled.shard_names(["a", "b"], {}, 5) == [["a"], ["b"]]
    assert coupled.shard_names(names, secs, 1) == [names]


def test_merge_reports_keeps_requested_order():
    r1 = {"schema_version": 1, "scenarios": [{"name": "c"}, {"name": "a"}]}
    r2 = {"schema_version": 1, "scenarios": [{"name": "b"}]}
    merged = coupled.merge_reports([r1, r2], ["a", "b", "c"])
    assert [s["name"] for s in merged["scenarios"]] == ["a", "b", "c"]
    assert merged["schema_version"] == 1
    try:
        coupled.merge_reports([r2], ["a", "b"])
    except RuntimeError as exc:
        assert "a" in str(exc)
    else:
        raise AssertionError("missing scenario not reported")


def test_check_generated_refuses_stale_core_before_any_binary_or_render(monkeypatch):
    import pytest

    args = coupled.parser().parse_args(
        ["scenario", "one", "--no-build", "--check-generated"]
    )
    args.extra = []
    monkeypatch.setattr(
        coupled, "generated", lambda device: Path("gen") if device == "dn2" else None
    )

    def stale(path):
        assert path == Path("gen")
        raise SystemExit("stale core")

    monkeypatch.setattr(coupled.digi_env, "check_generated", stale)
    monkeypatch.setattr(
        coupled,
        "ensure_binary",
        lambda *args, **kw: pytest.fail(
            "binary preparation before stale-core rejection"
        ),
    )
    with pytest.raises(SystemExit, match="stale core"):
        coupled.cmd_scenario(args)


def test_fallback_env_defaults_and_override_precedence(monkeypatch, tmp_path):
    import hashlib
    import json

    from tools.emulator import digi_env

    syx = tmp_path / "firmware.syx"
    syx.write_bytes(b"fixture")
    table = tmp_path / "fast.json"
    table.write_text(
        json.dumps(
            {
                hashlib.sha256(b"fixture").hexdigest(): {
                    "fast_regions": "0x1",
                    "fallback_regions": "0x2",
                }
            }
        )
    )
    monkeypatch.setattr(digi_env, "FAST_TABLE", table)
    base = {"DIGI_EMU_SYX": str(syx)}
    assert digi_env.fast_env(base)["SHARC_FAST_FALLBACK_REGIONS"] == "0x2"
    for explicit in ["", "0x9"]:
        env = {**base, "SHARC_FAST_REGIONS": explicit}
        assert "SHARC_FAST_FALLBACK_REGIONS" not in digi_env.fast_env(env)
        for fallback in ["", "0x8"]:
            env["SHARC_FAST_FALLBACK_REGIONS"] = fallback
            assert digi_env.fast_env(env)["SHARC_FAST_FALLBACK_REGIONS"] == fallback
    assert digi_env.fast_env({**base, "DIGI_EMU_FAST": "0"}) == {}
    # Explicit auto selection must retain the chance to arm ordinary roots.
    auto = {**base, "SHARC_FAST_AUTO": "10,100"}
    assert "SHARC_FAST_FALLBACK_REGIONS" not in digi_env.fast_env(auto)
    assert (
        digi_env.fast_env({**auto, "SHARC_FAST_FALLBACK_REGIONS": "0x8"})[
            "SHARC_FAST_FALLBACK_REGIONS"
        ]
        == "0x8"
    )
    monkeypatch.setattr(coupled.os, "environ", base)
    assert coupled.run_env()["SHARC_FAST_FALLBACK_REGIONS"] == "0x2"
    assert "SHARC_FAST_FALLBACK_REGIONS" not in coupled.run_env(
        {"SHARC_FAST_REGIONS": ""}
    )
    assert (
        coupled.run_env({"SHARC_FAST_FALLBACK_REGIONS": ""})[
            "SHARC_FAST_FALLBACK_REGIONS"
        ]
        == ""
    )
    assert "SHARC_FAST_FALLBACK_REGIONS" not in coupled.run_env({"DIGI_EMU_FAST": "0"})
    unknown = tmp_path / "unknown.syx"
    unknown.write_bytes(b"unknown")
    assert digi_env.fast_env({"DIGI_EMU_SYX": str(unknown)}) == {}


def test_run_env_uses_actual_firmware_without_mutating_selectors(monkeypatch, tmp_path):
    import hashlib
    import json

    from tools.emulator import digi_env

    dn2, dt2, patched = [
        tmp_path / name for name in ["dn2.syx", "dt2.syx", "patched.syx"]
    ]
    for syx in [dn2, dt2, patched]:
        syx.write_bytes(syx.name.encode())
    table = tmp_path / "fast.json"
    table.write_text(
        json.dumps(
            {
                hashlib.sha256(dn2.read_bytes()).hexdigest(): {"fast_regions": "dn2"},
                hashlib.sha256(dt2.read_bytes()).hexdigest(): {
                    "fallback_regions": "dt2"
                },
            }
        )
    )
    monkeypatch.setattr(digi_env, "FAST_TABLE", table)
    selectors = {"DIGI_EMU_SYX": str(dn2)}
    monkeypatch.setattr(coupled.os, "environ", selectors)
    env = coupled.run_env(syx=dt2)
    assert env["DIGI_EMU_SYX"] == str(dn2)
    assert env["SHARC_FAST_FALLBACK_REGIONS"] == "dt2"
    assert "SHARC_FAST_REGIONS" not in env
    assert coupled.run_env(syx=patched) == selectors
    assert (
        coupled.run_env({"SHARC_FAST_FALLBACK_REGIONS": "explicit"}, syx=patched)[
            "SHARC_FAST_FALLBACK_REGIONS"
        ]
        == "explicit"
    )
    assert selectors == {"DIGI_EMU_SYX": str(dn2)}
