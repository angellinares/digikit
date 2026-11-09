"""The probe sequence (tools/device/dkprobe_seq.py): its table is what the DSP module includes, and the framing numbers hold together."""

from support.paths import REPO_ROOT

from tools.device import dkprobe_seq as S

INC = REPO_ROOT / "tests/fixtures/modules/sharc-probe/src/probe_seq.inc"


def test_the_module_include_is_the_generated_one():
    assert INC.read_text() == S.inc_text(), (
        "run: uv run python -m tools.device.dkprobe_seq --write " + str(INC)
    )


def test_the_sequence_covers_every_table_index_for_both_ops():
    its = S.items()
    assert len(its) % 2 == 0 and len(its) < 8192
    for op, exps in ((S.OP_RECIPS, S.EXP_RECIPS), (S.OP_RSQRTS, S.EXP_RSQRTS)):
        seen = {
            ((v >> 16) & 0x7F, (v >> 23) & 0xFF)
            for o, v in its
            if o == op and (v >> 31) == 0 and 0 < ((v >> 23) & 0xFF) < 255
        }
        assert seen == {(i, e) for i in range(128) for e in exps}
    # recips first, then rsqrts; the module switches at PR_NRECIP
    ops = [o for o, _ in its]
    assert ops == [S.OP_RECIPS] * S.n_recips() + [S.OP_RSQRTS] * (
        len(its) - S.n_recips()
    )
    assert all(v in {x for _, x in its} for v in S.SPECIALS)


def test_a_run_is_whole_frames_and_the_header_carries_the_payload_size():
    assert (
        S.records_per_run() % S.RECORDS_PER_FRAME == 0
        and S.records_per_run() >= len(S.items()) + 2
    )
    assert (
        S.words_per_run()
        == S.frames_per_run() * S.FRAME_WORDS
        == S.frames_per_run() * 14
    )
    h = S.header_word(0x1FFFF)
    assert (
        h >> 24 == 0xB0 and (h >> 16) & 0xFF == S.PAYLOAD_WORDS and h & 0xFFFF == 0xFFFF
    )


def test_the_checksum_is_the_two_sum_xor():
    assert S.fletcher([]) == 0
    assert S.fletcher([1]) == 1 ^ 1
    assert S.fletcher([1, 2]) == 3 ^ 4
