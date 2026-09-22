"""Tests for machinecheck.py's pure functions: input-sequence building, wire
byte encoding (via the real emu.panelin encoders, not reimplemented), and
frame-offset maths. No emulator, no guest memory, no snapshot.

Run with: uv run --with pytest python -m pytest tests/test_machinecheck.py -q
(from this tmp dir; add the repo root to sys.path first if emu.panelin is
not importable, as done below).
"""
import os
import sys

TMP_ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, os.path.join(TMP_ROOT, 'tools'))
sys.path.insert(0, '/Users/em/src/digi/digitakt2')

import pytest  # noqa: E402

import machinecheck as mc  # noqa: E402


# --- derive_down_taps ------------------------------------------------------

@pytest.mark.parametrize('select_type,expected_pos', [
    (0, 0), (1, 1), (2, 2), (3, 3), (6, 4), (4, 5), (5, 6),
])
def test_derive_down_taps_stock_types(select_type, expected_pos):
    # machine_count=7, margin=0 isolates the stock-order lookup itself.
    assert mc.derive_down_taps(select_type, machine_count=7, margin=0) == expected_pos


def test_derive_down_taps_appended_type():
    # type 7 is not in STOCK_ORDER, so it's assumed appended at machine_count.
    assert mc.derive_down_taps(7, machine_count=7, margin=0) == 7
    assert mc.derive_down_taps(8, machine_count=7, margin=0) == 7


def test_derive_down_taps_margin_only_applies_to_appended_last_row():
    # The list clamps only at its last row, so the safety margin must apply
    # only there. SLICE (type 6) sits at an interior row (position 4 in
    # STOCK_ORDER) and gets no margin: exactly 4 taps, not 6.
    assert mc.derive_down_taps(6, machine_count=7, margin=2) == 4
    # type 7 is appended last (not in STOCK_ORDER), so the margin still
    # applies there.
    assert mc.derive_down_taps(7, machine_count=7, margin=2) == 7 + 2


def test_derive_down_taps_custom_stock_order():
    # A hypothetical 5-machine device (e.g. Digitone II) with its own order.
    order = (0, 1, 2, 3, 4)
    assert mc.derive_down_taps(3, machine_count=5, stock_order=order, margin=0) == 3
    assert mc.derive_down_taps(5, machine_count=5, stock_order=order, margin=0) == 5


# --- parse_turn / parse_frame_words ----------------------------------------

def test_parse_turn():
    assert mc.parse_turn('2:30') == (2, 30)
    assert mc.parse_turn('0x2:-30') == (2, -30)


def test_parse_turn_rejects_missing_colon():
    with pytest.raises(Exception):
        mc.parse_turn('230')


def test_parse_frame_words():
    assert mc.parse_frame_words('0x94,0xde,0xe6') == [0x94, 0xde, 0xe6]
    assert mc.parse_frame_words('10, 0x20 ,30') == [10, 0x20, 30]


# --- frame_addr --------------------------------------------------------

def test_frame_addr_type_word_is_special_cased():
    # Not track*0x60-tiled: a flat 2-byte-per-track array.
    assert mc.frame_addr(0x94, 0) == mc.TX_BASE + 0x94
    assert mc.frame_addr(0x94, 1) == mc.TX_BASE + 0x94 + 2
    assert mc.frame_addr(0x94, 5) == mc.TX_BASE + 0x94 + 10


def test_frame_addr_param_offsets_are_track_tiled():
    for off in (0xde, 0xe6, 0xe8, 0xea):
        assert mc.frame_addr(off, 0) == mc.TX_BASE + off
        assert mc.frame_addr(off, 3) == mc.TX_BASE + 3 * 0x60 + off


def test_frame_addr_cfade_matches_the_finding():
    # docs/findings/04-coldfire-dsp-link.md: mirror 27 (CFADE) -> frame +0xde,
    # absolute 0x80005348 + track*0x60 + 0xde, for track 2.
    assert mc.frame_addr(0xde, 2) == 0x80005348 + 2 * 0x60 + 0xde


# --- input-sequence building -------------------------------------------

def test_build_func_src_chord_shape():
    seq = mc.build_func_src_chord()
    assert seq == [
        (mc.FUNC_CH, 1 << mc.FUNC_BIT),
        (mc.SRC_CH, 1 << mc.SRC_BIT),
        (mc.SRC_CH, 0x00),
        (mc.FUNC_CH, 0x00),
    ]
    # FUNC asserted before SRC, and released last -- a held chord, not two
    # independent taps (docs/findings/03-ui-and-panel.md).
    func_press_idx = seq.index((mc.FUNC_CH, 1 << mc.FUNC_BIT))
    func_release_idx = seq.index((mc.FUNC_CH, 0x00))
    src_release_idx = seq.index((mc.SRC_CH, 0x00))
    assert func_press_idx < src_release_idx < func_release_idx


def test_build_down_taps_count_and_shape():
    seq = mc.build_down_taps(3)
    assert len(seq) == 6
    assert seq == [
        (mc.DOWN_CH, 1 << mc.DOWN_BIT), (mc.DOWN_CH, 0x00),
        (mc.DOWN_CH, 1 << mc.DOWN_BIT), (mc.DOWN_CH, 0x00),
        (mc.DOWN_CH, 1 << mc.DOWN_BIT), (mc.DOWN_CH, 0x00),
    ]


def test_build_down_taps_zero():
    assert mc.build_down_taps(0) == []


def test_build_yes_is_one_press_release_pair():
    assert mc.build_yes() == [(mc.YES_CH, 1 << mc.YES_BIT), (mc.YES_CH, 0x00)]


def test_build_select_sequence_ends_with_two_yeses():
    seq = mc.build_select_sequence(7, machine_count=7, margin=0)
    yes = (mc.YES_CH, 1 << mc.YES_BIT)
    # exactly two YES presses (commit, then close -- MachineSelectionView::vfunc_2)
    assert seq.count(yes) == 2
    assert seq[-4:] == mc.build_yes() + mc.build_yes()


def test_build_select_sequence_down_tap_count():
    seq = mc.build_select_sequence(6, machine_count=7, margin=1)
    down_press = (mc.DOWN_CH, 1 << mc.DOWN_BIT)
    # position 4 (SLICE/type 6 in STOCK_ORDER), an interior row, so the
    # margin (1) does not apply: 4 taps.
    assert seq.count(down_press) == 4


# --- wire-byte encoding: our sequences through the real encoder ------------

def test_down_taps_encode_to_expected_wire_bytes():
    from emu import panelin
    seq = mc.build_down_taps(2)
    wire = [panelin.encode_buttons(ch, mask) for ch, mask in seq]
    # DOWN = code 14 -> channel 1, bit 5 (docs/findings/03-ui-and-panel.md)
    assert wire[0] == bytes([(0x2 << 4) | 1, 1 << 5])
    assert wire[1] == bytes([(0x2 << 4) | 1, 0x00])
    assert wire[0] == wire[2]
    assert wire[1] == wire[3]


def test_yes_encodes_to_expected_wire_bytes():
    from emu import panelin
    press, release = mc.build_yes()
    assert panelin.encode_buttons(*press) == bytes([(0x2 << 4) | 1, 1 << 1])
    assert panelin.encode_buttons(*release) == bytes([(0x2 << 4) | 1, 0x00])


def test_func_src_chord_encodes_to_expected_wire_bytes():
    from emu import panelin
    seq = mc.build_func_src_chord()
    wire = [panelin.encode_buttons(ch, mask) for ch, mask in seq]
    assert wire == [
        bytes([(0x2 << 4) | 2, 0x01]),   # FUNC down
        bytes([(0x2 << 4) | 0, 0x02]),   # SRC down, FUNC still held
        bytes([(0x2 << 4) | 0, 0x00]),   # SRC up
        bytes([(0x2 << 4) | 2, 0x00]),   # FUNC up, last
    ]


def test_turn_encodes_with_encode_encoder():
    from emu import panelin
    ch, delta = mc.parse_turn('2:30')
    assert panelin.encode_encoder(ch, delta) == bytes([(0x3 << 4) | 2, 30])
