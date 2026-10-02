"""Reset-table parsing stays explicit and rejects contradictory inputs."""

import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "tools"))
from sharc_mmr_reset import parse_reset_tables


def test_reads_concrete_rows_and_deduplicates_identical_facts():
    text = """Register Address Reset
0x3108D004 CGU0_PLLCTL PLL Control Register 0x00000000
0x3108D008 CGU0_STAT Status Register 0x0000000F
0x3108D004 CGU0_PLLCTL PLL Control Register 0x00000000
0x10000000 NOT_A_REGISTER Synthetic address 0x00000000
0x3108D000 CGU0_CTL Unspecified reset varies
"""
    assert parse_reset_tables(text) == {
        0x3108D004: (0, "CGU0_PLLCTL"),
        0x3108D008: (15, "CGU0_STAT"),
    }


def test_conflicting_reset_fact_is_an_error():
    with pytest.raises(ValueError, match="conflicting reset values"):
        parse_reset_tables(
            "0x3108D008 CGU0_STAT Status 0x0000000F\n"
            "0x3108D008 CGU0_STAT Status 0x00000005\n"
        )


def test_missing_reset_table_is_an_error():
    with pytest.raises(ValueError, match="no ADSP-2156x"):
        parse_reset_tables("not a reset register extraction")
