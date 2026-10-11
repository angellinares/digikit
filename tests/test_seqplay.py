"""tools/seqplay.py: note events from the frames sent to the DSP."""

import os
import struct
import sys

sys.path.insert(0, os.path.join(os.path.dirname(__file__), '..', 'tools'))

import seqplay  # noqa: E402


def capture(icount, on=0, off=0, notes=()):
    frame = bytearray(48)
    for voice, note in enumerate(notes):
        struct.pack_into('>H', frame, 2 + 2 * voice, int(note * 256))
    struct.pack_into('>4H', frame, 34, on, off, on, off)
    return {'icount': icount, 'hex': frame.hex()}


def test_a_mask_held_over_two_frames_is_one_event():
    caps = [capture(0), capture(10, on=1), capture(20, on=1), capture(30, off=1), capture(40)]
    found = seqplay.events(caps)
    assert [(e.icount, e.kind, e.voices) for e in found] == [
        (10, 'on', (0,)), (30, 'off', (0,))]


def test_the_same_voice_twice_in_a_row_needs_a_gap_frame():
    caps = [capture(0, on=2), capture(5), capture(9, on=2)]
    assert [e.icount for e in seqplay.events(caps)] == [0, 9]


def test_unison_sets_several_voice_bits():
    assert seqplay.events([capture(1, on=0x8003)])[0].voices == (0, 1, 15)


def test_gaps_and_tempo():
    caps = []
    for k in range(4):
        caps += [capture(k * 66_000_000, on=1 << k), capture(k * 66_000_000 + 1000)]
    gaps = seqplay.note_on_gaps(seqplay.events(caps))
    assert gaps == [66_000_000] * 3
    assert round(seqplay.bpm(gaps[0] / 4), 1) == 120.0


def test_step_counts_wrap_to_the_first_trig():
    assert seqplay.step_counts([1, 5, 9, 13], 5) == [4, 4, 4, 4, 4]
    assert seqplay.step_counts([1, 4], 3) == [3, 13, 3]


def test_short_captures_are_skipped():
    assert seqplay.events([{'icount': 0, 'hex': '00' * 10}]) == []


def test_the_note_is_read_from_each_voice_word_at_the_note_on():
    caps = [capture(0, on=0b101, notes=(60, 0, 64.5)),
            capture(9, off=0b101, notes=(60, 0, 64.5))]
    found = seqplay.events(caps)
    assert found[0].notes == (60.0, 64.5)
    assert found[1].notes == ()


def test_a_few_instructions_of_jitter_is_inside_the_tolerance():
    base = [(0, 'on', (0,), (60.0,)), (16_544_000, 'off', (0,), ())]
    mine = [(0, 'on', (0,), (60.0,)), (16_544_018, 'off', (0,), ())]
    assert seqplay.first_difference(base, mine, 1000) is None
    assert seqplay.first_difference(base, mine, 0) is not None


def test_a_different_voice_note_or_count_is_a_difference_at_any_tolerance():
    base = [(0, 'on', (0,), (60.0,)), (5, 'off', (0,), ())]
    assert seqplay.first_difference(base, [(0, 'on', (1,), (60.0,)), base[1]], 10**9)
    assert seqplay.first_difference(base, [(0, 'on', (0,), (61.0,)), base[1]], 10**9)
    assert seqplay.first_difference(base, base[:1], 10**9)


def test_signature_counts_from_the_first_note_on():
    found = [seqplay.NoteEvent(100, 'off', (0,)), seqplay.NoteEvent(500, 'on', (0,), (60.0,)),
             seqplay.NoteEvent(900, 'off', (0,))]
    assert seqplay.signature(found) == [(0, 'on', (0,), (60.0,)), (400, 'off', (0,), ())]


def test_the_script_selects_each_track_with_trk_held():
    steps = seqplay.script({1: [1], 2: [3]}, '10M')
    assert steps.index('press:16') < steps.index('tap:26')   # track 2 is trig key 26
    assert steps.count('press:16') == 2 and steps.count('release:16') == 2
    assert seqplay.script({1: [1]}, '10M').count('press:16') == 0
