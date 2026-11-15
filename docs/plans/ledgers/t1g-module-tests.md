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

The tables are `tb303_ref.Tb303` output (`uv run python -m tools.devtools.rig_refs modules/machines/tb303`), the data the test compared with. DT2 only (the
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

## digikit/pluck (`test_pluck.py`, `test_pluck_range.py`, `test_pluck_tune.py`, `pluck_rig.py`, `pluck_ref.py`; kit: `tests/rig/pluck.toml`, tables `tests/rig/ref/*.f32`)

The tables are `pluck_rig.model_blocks` output (`uv run python -m tools.devtools.rig_refs modules/machines/pluck`); the tolerance of every case is the Python one,
2e-5 * max(1, model peak) = 2e-5 (every model peak is below 1). DT2 only (the Python rig is DT2).

| Python | Kit |
| --- | --- |
| `test_the_routine_matches_the_float32_model` (4 cases, 40 blocks, finite, model peak above 0.05) | 4 cases `the routine matches the model: NAME`: `matches-table` atol 2e-5, `peak` min 0.05, finite by the implicit rules |
| `test_instruction_counts_per_block` (note-on block below 60000, steady below 2200, idle voice below 750) | case `instruction counts per block` (`instructions` first max 59999, steady max 2199) and case `an idle voice costs little` (step `record_word = 0`, `instructions` idle max 749) |
| `test_the_voice_ends_when_it_is_quiet_and_clears_its_flags` | case `the voice ends when it is quiet and clears its flags`: `voice-ends` (after block 5, within 1499, mark cleared), then an idle step with `peak` 0 |
| `test_a_new_note_restarts_the_string` | case `a new note restarts the string`: `peak` min 0.05 of the new block and `differs` from the first block |
| `test_the_routine_stays_finite_and_matches_the_model_at_the_corners` (5 corners, 8 blocks; the string state `lp'`, `ap'` finite) | 5 cases `the string stays finite and matches the model at the corner NAME`: `matches-table` atol 2e-5 and the implicit finite rule; the state words are not read (a bad state shows in the next blocks' samples, which the table checks) |
| `test_the_string_is_in_tune` (8 notes, within 1.5 cents) | 8 cases `the string is in tune: note N`: `pitch-hz` method `bandpass`, `skip_periods` 4, cents 1.5, the Python block counts |
| `test_the_loop_period_is_servable_for_every_note_and_tune_word`, `test_the_string_stays_finite_and_bounded_for_200_blocks` | not ported: they run the Python model only (every note and tune word), no routine; the corner cases above are the routine's side of the same regression |
| `test_dkm_layout.py` | not ported: a `patchkit pack` layout test, no rig |

Mutation: `0x45b777e3` (P0 = 1 / INC0) to `0x45b787e3` in `src/dsp/pluck.s`: Python `test_the_routine_matches_the_float32_model` fails (error
0.039 against 2e-5); the kit fails 6 `matches-table` cases (the four model cases, corner note0 and low-tune).

## digikit/freqshift (`test_freqshift.py`, `test_freqshift_ref.py`, `test_fxshift_analysis.py`, `freqshift_ref.py`, `freqshift_long.py`, `freqshift_tone.py`; kit: `tests/rig/freqshift.toml`, tables `tests/rig/ref/*.f32`)

The input tables hold the numpy-generator inputs of the Python tests (a TypeScript run cannot draw them) and the output tables the float32 model's
output for them (`uv run python -m tools.devtools.rig_refs modules/fx/freqshift`). The device test `freqshift.device.test.ts` stays.

| Python | Kit |
| --- | --- |
| `test_the_routine_matches_the_float32_model` (8 shifts, 36 blocks, 2e-7 on the effect's own output block) | 8 cases `the routine matches the model: shift V`: `matches-table` (`channel = "both"`, atol 2e-7) against `ref/model-vN.f32` for input `ref/mixed-in-3.f32` |
| `test_the_routine_follows_the_model_while_the_shift_moves` | case with 10 steps of 5 blocks, `ref/model-moving.f32`, atol 2e-7 |
| `test_a_tone_moves_by_the_shift_and_the_image_is_40_db_down` (100, -100, 2000 Hz) | 3 cases: `tone` (amplitude 0.5 within 0.005 at the exact frequency) with `image_freq` and `image_db_min` 40; shift word and exact frequency are the model's (`rig_refs` prints them) |
| `test_shift_zero_keeps_the_magnitude` (1000 and 12000 Hz, 2e-3 relative) | 2 cases: `tone` amplitude 0.5, tolerance 0.001 |
| `test_mix_is_the_frameworks_zero_is_dry_bit_for_bit_and_one_is_wet` | 2 cases: `mix = 0` with `master-equals-input`; `mix` 1.0 with `master-equals-scratch` from block 1 |
| `test_the_output_stays_below_the_level_bound_and_the_adapter_never_refuses_a_block` | 3 cases with `square-noise` input: `peak` of both channels at most float32(7.9), at least 3.0 for amplitude 8; the adapter's refusal is an implicit rule. The right channel's random signs come from the kit's generator, not numpy's: the input is not pinned in the Python test either |
| `test_writes_stay_in_the_output_block_and_the_state_and_the_cost_matches_the_description` | case with 6 blocks: implicit write-set rule, `description` words, `instructions` of the last block 3001 to 4799 |
| `test_the_state_is_the_description_state_and_nothing_else` | case with `state-untouched-beyond` (the 4 KiB state block is read after the last block) |
| `test_the_oscillator_does_not_drift_in_a_long_run` (1,500 blocks) | case `... 1,500 blocks`: `tone-stable` (eight windows of 400 blocks: amplitude spread 1e-4, phase drift 1e-3 rad), `state-word` 0 = the closed-form phase 989059184 (the model's value for inc 3690415), `peak` below 0.8 |
| `test_the_oscillator_does_not_drift_in_a_run_of_a_minute` (`slow`, 90,000 blocks) | not ported: a `slow` test; the kit keeps whole traces in memory (90,000 blocks) and the closed form is exact for any block count by integer arithmetic |
| `test_the_module_declares_one_effect_by_builder_calls`, `test_the_committed_shift_knob_is_bipolar` | not ported: they read module.toml and gallery.json, no rig |
| `test_freqshift_ref.py` | not ported: model only (mapping, Hilbert pair, polynomial, tones through the model); no routine |
| `test_fxshift_analysis.py` | not ported: the offline adapter of the device PCM oracles (`pk_digikit_fxshift`), no rig |

Mutation: `0x3e3a2e8c` (the scale of the shift mapping, x = |v| * c) to `0x3e3a2e9c` in `src/dsp/freqshift.s`: Python `test_the_routine_follows_the_model_while_the_shift_moves`
fails (error 3.1e-4 against 2e-7); the kit fails 7 cases (five model shifts, the moving shift, the long run).

## digikit/shot (`test_shot_playback.py`, `support/sample_pool.py`; kit: `tests/rig/shot.toml`, tables and PCM in `tests/rig/ref/`)

`shot.toml` has 42 cases; the pool entries are published by the kit (`samplepool.ts`, planes then record, `ready` last) from `ref/pcm-*.i16` (the numpy
generator's int16 words) and the case's `[[case.pool]]` tables. The tables are the output of the Python reader model `model()` of the test for the same
scenarios (`uv run python -m tools.devtools.rig_refs modules/machines/shot`; the generator asserts, as the Python test does, that the rig equals the model). Where the test
took the model's rate from the rig's cursor, the rig's rate is in the model table and the toml checks it against the analytic rate with the Python tolerance.
All comparisons are exact (atol 0), as `np.array_equal`. After every block the pool's count word must be 0 (`pool_ctl`), and the module's stores must stay in
its own memory (implicit rule, which also replaces `test_the_module_stores_only_inside_its_own_memory`).

| Python | Kit cases |
| --- | --- |
| `test_default_params_play_the_whole_sample_with_distinct_channels` (both devices; DN2 left only) | `default parameters play the whole sample (dt2)` and `(dn2)`: tables, `peak` min 0.04, `cursor` rate 65536 |
| `test_start_end_and_play_select_the_frames` (4) | `start end and play select the frames: NAME` (4) |
| `test_pitch_is_the_note_plus_the_pitch_parameter` (5) | `pitch is the note plus the pitch parameter: NAME` (5), tables and the rate check |
| `test_a_source_rate_other_than_48k_scales_the_cursor`, `test_a_mono_sample_plays_centred`, `test_another_pool_entry_reads_its_own_record` | one case each |
| `test_two_smp_values_pick_two_records` (both devices) | `two SMP values pick two records (dt2)` and `(dn2)`: two pool entries, two tables |
| `test_an_empty_or_out_of_range_entry_is_silent` (4) | `an empty or out-of-range entry is silent: SMP N`: `peak` 0 on both channels; the count word read is entry 0's, not entry N's (the Python test read entry N's) |
| `test_the_voice_ends_after_the_sample` | `the voice ends after the sample`: `voice-ends`, then a silent block |
| `test_a_stale_token_is_silent_and_ends_the_voice` (both devices) | `a stale token is silent and ends the voice (dt2)` / `(dn2)`: step `pokes` of the record's token word, `voice-ends` (DT2), silent odd voice |
| `test_an_entry_that_is_not_playable_is_silent_with_no_pcm_read` (4) | `an entry that is not playable ...: NAME`: pokes as in the test (the bad plane address 0xFFFFFFF0 makes a PCM read fault the call, which the implicit rules catch) |
| `test_instruction_count_is_inside_the_ceiling` (both) | `instruction count is inside the ceiling (DEV)`: first block max 8699, steady max 8499 |
| `test_the_module_stores_only_inside_its_own_memory` | implicit rule of every case |
| live parameter tests (7: flip to reverse, loop modes (2), loop to fwd, end behind, end above, start above forward, start above reverse, end below start) | one case each: tables for both channels, `cursor` words where the test asserted them, `voice-ends`, `voice-active` for the loops |
| `test_a_pure_tone_keeps_its_level_at_any_pitch` (3) | `a pure tone keeps its level at any pitch: NAME`: `tone-fit` (least-squares sine fit over samples 64 to 576, amplitude 16000 / 32768 * 0.4045 within 2 %) |
| `test_smp_is_latched_but_other_parameter_words_remain_live` | not ported: it reads the adapter's latch words `_dv_params` and `_dv_call_params`, a property of core/dsp-voice (phase 5 adapter tests); the entry-5 part is `an empty or out-of-range entry is silent` |

Mutation: `0x3daaaaab` (1/12, the semitone scale of the pitch) to `0x3dabaaab` in `src/dsp/shot.s`: Python `test_pitch_is_the_note_plus_the_pitch_parameter`
fails on the rate bound (111821 against 2); the kit fails the 5 pitch cases. Note for the S6a rebase: the tables and the instruction ceilings hold for
a bit-identical kernel; `dsp_cost_voice` in module.toml is what the conformance run writes.
