from __future__ import annotations

from pathlib import Path

from tools.devtools import fast_roots

IMAGE_RS = """
pub static REGION_ENTRIES: &[&[u32]] = &[&[0x1000, 0x1010, 0x1020], &[0x2000], &[], &[0x3000, 0x3100]];
"""


def test_parse_region_entries_skips_empty_regions() -> None:
    assert fast_roots.parse_region_entries(IMAGE_RS) == [
        [0x1000, 0x1010, 0x1020],
        [0x2000],
        [0x3000, 0x3100],
    ]


def test_rank_suggests_entry_and_loop_body_and_counts_outside() -> None:
    regions = fast_roots.parse_region_entries(IMAGE_RS)
    # Region 0 holds nearly all work: 0x1000 starts runs, 0x1010 is a loop body that keeps running.
    interpreted = {
        0x1000: 50,
        0x1004: 20,
        0x1010: 9000,
        0x1014: 900,
        0x2000: 30,
        0x9000: 100,
    }
    starts = {0x1000: 400, 0x1010: 5, 0x2000: 6}
    ranking = fast_roots.rank_regions(regions, interpreted, starts, min_pct=1.0)

    assert ranking.total == sum(interpreted.values())
    assert ranking.outside == 100  # 0x9000 is far from every block
    assert [r.first for r in ranking.regions] == [0x1000]
    top = ranking.regions[0]
    assert top.starts[0] == 0x1000
    assert top.loops[0] == 0x1010
    assert {0x1000, 0x1010} <= set(ranking.roots())
    assert ranking.roots() == sorted(ranking.roots())
    assert "ROOTS 0x1000,0x1010" in fast_roots.format_ranking(ranking)


def test_parse_counts_sums_pc_lines() -> None:
    text = "1000 0 1 7\n1000 0 1 3\n# note\n1004 0 0 2\n"
    assert fast_roots.parse_counts(text, 3) == {0x1000: 10, 0x1004: 2}


def test_read_capture_sums_files_and_skips_trans(tmp_path: Path) -> None:
    (tmp_path / "cap.e1-0").write_text("1000 0 1 7\n")
    (tmp_path / "cap.e2-0").write_text("1000 0 1 3\n")
    (tmp_path / "cap.e1-0.entries").write_text("1000 4\n")
    (tmp_path / "cap.e1-0.trans").write_text("1000 2000 99\n")
    interpreted, starts = fast_roots.read_capture(str(tmp_path / "cap"))
    assert interpreted == {0x1000: 10}
    assert starts == {0x1000: 4}


def test_budget_evaluation() -> None:
    budget = fast_roots.Budget("s", "f.json", 3, 1000)
    assert fast_roots.over_budget(999.0, budget) == []
    assert fast_roots.over_budget(1001.0, budget) == ["interpreted"]
    report = {
        "scenarios": [
            {
                "name": "s",
                "audio_seconds": 2.0,
                "dsp_instructions": {"interpreter": 3000, "block_entries": 8000},
            }
        ]
    }
    assert fast_roots.interpreted_per_audio_second(report, "s") == 1500.0
    assert fast_roots.block_entries_per_audio_second(report, "s") == 4000.0


def test_block_entries_are_checked_only_when_the_budget_has_a_limit() -> None:
    open_budget = fast_roots.Budget("s", "f.json", 3, 1000)
    assert fast_roots.over_budget(1.0, open_budget, 1e12) == []
    limited = fast_roots.Budget("s", "f.json", 3, 1000, 5000)
    assert fast_roots.over_budget(1.0, limited, 4999.0) == []
    assert fast_roots.over_budget(1.0, limited, 5001.0) == ["block entries"]
    assert fast_roots.over_budget(2000.0, limited, 5001.0) == [
        "interpreted",
        "block entries",
    ]


def test_budget_file_lists_cases_per_state_and_the_reset_case_fails_when_over(
    tmp_path: Path,
) -> None:
    path = tmp_path / "budgets.json"
    path.write_text(
        '{"_comment": "x", "dn2": {'
        '"first-boot": {"scenario": "a", "scenarios_file": "f", "seconds": 3, "max_interpreted_per_audio_s": 100},'
        '"reset": {"scenario": "a", "scenarios_file": "f", "seconds": 3, "max_interpreted_per_audio_s": 200,'
        ' "max_block_entries_per_audio_s": 900}}}'
    )
    cases = fast_roots.load_budgets(path)["dn2"]
    assert list(cases) == ["first-boot", "reset"]
    assert cases["first-boot"].max_block_entries_per_audio_s is None
    # the same rates pass the first-boot case and fail the reset case
    assert fast_roots.over_budget(150.0, cases["reset"], 1000.0) == ["block entries"]
    assert fast_roots.over_budget(250.0, cases["reset"], 10.0) == ["interpreted"]


def test_fixture_key_changes_with_each_input(tmp_path: Path) -> None:
    tracked = tmp_path / "tracked.rs"
    gen = tmp_path / "image.rs"
    tracked.write_text("a")
    gen.write_text("g")

    def key(stock: str = "s") -> str:
        return fast_roots.fixture_key(stock, {"tracked.rs": tracked}, {"image.rs": gen})

    base = key()
    assert key() == base
    tracked.write_text("a2")
    assert key() != base
    tracked.write_text("a")
    gen.write_text("g2")
    assert key() != base
    gen.write_text("g")
    assert key("other") != base

    reset = fast_roots.fixture_key(
        "s", {"tracked.rs": tracked}, {"image.rs": gen}, "reset"
    )
    assert reset != base
    assert (
        fast_roots.fixture_key(
            "s", {"tracked.rs": tracked}, {"image.rs": gen}, "first-boot"
        )
        == base
    )
