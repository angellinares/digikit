"""tools/sharc_dn2_aot.py: argument checks, profile merging and the manifest
comparison, without firmware (the pipeline itself needs private inputs)."""

from __future__ import annotations

import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "tools"))

import sharc_dn2_aot as aot
import sharc_rsgen as rg

BASE = ["--state", "s.bin", "--capture", "c.dt2cap"]


def test_out_must_lie_outside_the_repo(tmp_path: Path) -> None:
    with pytest.raises(SystemExit):
        aot.parse_args(["--out", str(aot.ROOT / "out" / "x"), *BASE])
    with pytest.raises(SystemExit):
        aot.parse_args(["--out", str(aot.ROOT), *BASE])
    a = aot.parse_args(["--out", str(tmp_path / "x"), *BASE])
    assert a.cycles == 3 and a.gate == "full"


def test_coupled_inputs_come_together(tmp_path: Path) -> None:
    out = ["--out", str(tmp_path)]
    with pytest.raises(SystemExit):
        aot.parse_args([*out, *BASE, "--syx", "x.syx"])
    a = aot.parse_args(
        [
            *out,
            *BASE,
            "--syx",
            "x.syx",
            "--cf-snapshot",
            "m.snap",
            "--dsp-image",
            "i.bin",
        ]
    )
    assert aot.settings(a)["coupled_extra"] == 100_000_000
    assert aot.settings(aot.parse_args([*out, *BASE]))["coupled_extra"] is None


def test_expect_keys() -> None:
    assert aot.parse_expect("pcm=581339b332e9") == ("pcm", "581339b332e9")
    assert aot.parse_expect("state:5599=7dee") == ("state:5599", "7dee")
    for bad in ("pcm", "pcm=XYZ1", "pcm=12", "wav=1234"):
        with pytest.raises(ValueError):
            aot.parse_expect(bad)
    results = {"pcm": "581339b332e9aa", "state:5599": "7dee61"}
    assert aot.check_expect(["pcm=5813", "state:5599=7dee"], results) == []
    assert len(aot.check_expect(["pcm=0000", "coupled_cf=1234"], results)) == 2


def test_merged_coverage_reads_like_the_concatenation(tmp_path: Path) -> None:
    c0 = "0x10 0x1000 1 5\n0x20 0x0 0 2\n0x10 0x1400 1 1\n"
    c1 = "0x10 0x1000 1 3\n0x8 0x1000 1 1\n"
    merged = aot.merge_kind([c0, c1], "")
    assert merged == (
        "0x8 0x1000 1 1\n0x10 0x1000 1 8\n0x10 0x1400 1 1\n0x20 0x0 0 2\n"
    )
    (tmp_path / "m").write_text(merged)
    (tmp_path / "cat").write_text(c0 + c1)
    assert rg.read_coverage(str(tmp_path / "m")) == rg.read_coverage(
        str(tmp_path / "cat")
    )


def test_merged_entries_transitions_and_exits() -> None:
    assert aot.merge_kind(["0x20 2\n0x10 1\n", "0x20 5\n"], ".entries") == (
        "0x10 1\n0x20 7\n"
    )
    assert aot.merge_kind(["0x1 0x2 3\n", "0x1 0x2 4\n0x0 0x9 1\n"], ".trans") == (
        "0x0 0x9 1\n0x1 0x2 7\n"
    )
    assert aot.merge_kind(["0x10 1 0x14 2\n", "0x10 1 0x14 1\n"], ".exits") == (
        "0x10 1 0x14 3\n"
    )
    # Lines of another width (a header, a blank) are skipped.
    assert aot.merge_kind(["\n0x10\n0x10 1\n"], ".entries") == "0x10 1\n"


def test_rsgen_command_names_every_profile(tmp_path: Path) -> None:
    a = aot.parse_args(["--out", str(tmp_path), *BASE, "--region-insns", "64"])
    cmd = aot.rsgen_command(
        "dn2-1.11", tmp_path / "gen", tmp_path / "w", tmp_path / "p", a
    )
    text = " ".join(cmd)
    for part in (
        "--coverage %s/p " % tmp_path,
        "--entries %s/p.entries" % tmp_path,
        "--transitions %s/p.trans" % tmp_path,
        "--model-safe",
        "--explicit-memory-model 0",
        "--region-insns 64",
    ):
        assert part in text


def test_unknown_fallback_selection_is_reproducible(tmp_path: Path) -> None:
    base = aot.parse_args(["--out", str(tmp_path), *BASE])
    selected = aot.parse_args(
        ["--out", str(tmp_path), *BASE, "--unknown-fallbacks", "0x1c253f"]
    )
    cmd = aot.rsgen_command(
        selected.image, tmp_path / "gen", tmp_path / "work", tmp_path / "p", selected
    )
    i = cmd.index("--unknown-fallbacks")
    assert cmd[i + 1] == "0x1c253f"
    assert "unknown_fallbacks" not in aot.settings(base)
    assert aot.settings(selected)["unknown_fallbacks"] == [0x1C253F]
    differences = aot.compare_manifests(
        {"settings": aot.settings(base)}, {"settings": aot.settings(selected)}
    )
    assert len(differences) == 1
    assert differences[0].startswith("settings.unknown_fallbacks")


def test_tree_hash_ignores_reports(tmp_path: Path) -> None:
    (tmp_path / "a.rs").write_text("fn a() {}\n")
    (tmp_path / "insns.bin").write_bytes(b"\x01\x02")
    first, files = aot.tree_hash(tmp_path)
    assert sorted(files) == ["a.rs", "insns.bin"]
    (tmp_path / "rsgen-report.json").write_text('{"seconds": 1}')
    assert aot.tree_hash(tmp_path)[0] == first
    (tmp_path / "a.rs").write_text("fn a() { }\n")
    assert aot.tree_hash(tmp_path)[0] != first


def test_compare_manifests() -> None:
    old = {
        "inputs": {"state": {"path": "/a/s.bin", "sha256": "11"}},
        "final": {"gen_sha256": "aa", "rsgen": ["--out", "OUT/gen"]},
        "cycles": [{"profile": {"cov": "01"}}],
        "timing": {"total": 1.0},
    }
    new = {
        "inputs": {"state": {"path": "/b/s.bin", "sha256": "11"}},
        "final": {"gen_sha256": "aa", "rsgen": ["--out", "OUT/gen"]},
        "cycles": [{"profile": {"cov": "01"}}],
        "timing": {"total": 2.0},
    }
    assert aot.compare_manifests(old, new) == []
    new["cycles"][0]["profile"]["cov"] = "02"
    new["final"]["gen_sha256"] = "ab"
    diffs = aot.compare_manifests(old, new)
    assert len(diffs) == 2
    assert diffs[0].startswith("cycles[0].profile.cov")
    assert diffs[1].startswith("final.gen_sha256")
