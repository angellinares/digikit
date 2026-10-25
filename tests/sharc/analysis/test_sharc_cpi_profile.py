"""tools/sharc_dev/sharc_cpi.py reads the engine's A lines (MMR accesses by address)."""

from tools.sharc_dev import sharc_cpi


def test_a_lines_are_read_by_pc_and_address(tmp_path):
    tsv = tmp_path / "mix.tsv"
    tsv.write_text(
        "P\t0x1c253f\t10\n"
        "A\t0x1c253f\t0x3102d210\t7\n"
        "A\t0x1c253f\t0x3102d210\t3\n"
        "A\t0xb8f3de\t0x3108b840\t1\n"
    )
    prof = sharc_cpi.Profile(tsv)
    assert prof.mmr[(0x1C253F, 0x3102D210)] == 10
    assert prof.mmr[(0xB8F3DE, 0x3108B840)] == 1
