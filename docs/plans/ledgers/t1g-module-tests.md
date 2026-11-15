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
