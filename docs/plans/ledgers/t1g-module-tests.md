# T1g phase 4: module tests on the rig

Each Python test of a module is mapped to a check of `tests/rig/*.toml` ([TESTING.md](../../TESTING.md) "Sound checks"), or the reason it
is not ported. A reference table is the output of the Python float32 model that the test compared with, so a check against it keeps the
old assertion (no re-pin). Mutation proof: one DSP constant changed in the working tree, the Python test and the kit check both fail,
the change reverted (never committed).

## test/sine (`tests/patchkit/test_sine.py`, kit: `tests/fixtures/modules/sine/tests/rig/sine.toml`)

| Python | Kit |
| --- | --- |
| `test_the_sine_follows_the_note_within_a_cent` (7 notes, 2 devices: pitch within 1 cent, peak 0.35 to 0.40) | 7 cases `follows note N within a cent`: `pitch-hz` (cents 1) and `peak` (0.35 to 0.40) on both devices; the block count is the Python one |
| `test_tune_shifts_the_pitch_by_semitones` | case `tune shifts the pitch by semitones` (DT2): `pitch-hz` note 61 |
| `test_the_level_parameter_scales_the_amplitude` (ratio 0.5 within 0.01) | two cases per device, full level `peak` 0.3937 to 0.3938 (100 / 127 * 0.5) and level 0x3F80 `peak` 0.1929 to 0.2008 (that ratio window); the ratio test of two runs becomes two absolute windows |
| `test_a_retrigger_restarts_the_oscillator_and_the_voice_ends_after_two_seconds` | case `a retrigger restarts the oscillator`: `sample-at` the first sample of the first and of the second note (DN2: one block later); case `the voice ends after two seconds (dt2)`: 3014 blocks, `peak` 0 in the last block |
| `test_a_slot_with_an_unregistered_type_goes_back_to_the_stock_code_without_the_table_walk` | not ported: a property of the adapter core/dsp-voice (stock path, 70 instruction bound), not of the module; it belongs to the adapter tests of phase 5 |

Mutation: `0x39329a62` (440 * 2^(-69/12) / 48000) to `0x39339a62` in `src/dsp/sine.s`: Python `test_the_sine_follows_the_note_within_a_cent[60-dt2]`
and `[60-dn2]` fail; the kit fails all 8 pitch cases (note 12 is 9.6 cents off).

## tom/tb303 (`modules/machines/tb303/tests/test_tb303.py`, `tb303_ref.py`; kit: `tests/rig/tb303.toml`, tables `tests/rig/ref/*.f32`)

The tables are `tb303_ref.Tb303` output (`uv run python -m tools.devtools.rig_refs tb303`), the data the test compared with. DT2 only (the
module has no DN2 port).

| Python | Kit |
| --- | --- |
| `test_the_routine_matches_the_model` (5 cases, 60 blocks, atol 1e-5, peak of the model above 0.05) | 5 cases `the routine matches the model: NAME`: `matches-table` `ref/model-NAME.f32` atol 1e-5 and `peak` min 0.05 |
| `test_a_held_note_slides_into_the_next` (held and released, tracks 0 and 5, 20 + (3) + 30 blocks, atol 1e-5; the model's `freq` asserts on the model are not rig data) | 4 cases `a note after a note, ...`, steps note 41, (release), note 53, `matches-table` `ref/slide-held.f32` or `slide-released.f32`; the model-internal `m.freq` asserts are not ported (model state, no rig output) |
| `test_cost_per_block` (max count below 7500 + 900) | case `the cost per block`: `instructions` of all blocks, max 8399 |
| `test_tb303_tracks_unpack_a_level_of_100` | not ported: it calls the adapter's `_dv_unpack` hook on tracks 2 to 5, a property of core/dsp-voice (phase 5 adapter tests) |
| `test_a_tie_restarts_neither_envelope`, `test_a_release_ends_the_note`, `test_velocity_above_100_is_an_accent_of_amount_acc`, `test_knobs_flipping_every_block_stay_bounded` | not ported: they run the Python model only (no routine); the routine is tied to the model by the table checks above, whose cases cover accent velocities, release and glides |
| `test_the_model_sounds_like_open303` | not ported: model against Open303, no routine; it goes with the model |

Mutation: `0x4102d013` (440 x 2^(-69/12), the pitch scale) to `0x4112d013` in `src/dsp/tb303.s`: Python `test_the_routine_matches_the_model`
fails (5 of 5); the kit fails all 9 `matches-table` cases. (`0x41333333`, the 11.2 octave clamp, is not reached by any case: no check sees it, in
Python or in the kit.)
