# Live and latched parameters in the SDK

Status: proposal, for Em's OK. Not built.

## Problem

An engine decides when a parameter takes effect. The stock one-shot on DT2 1.17 re-applies direction, start, length and loop every block and fixes only the sample slot at the trigger (static read of the stock sample setter `FUN_1c4afe` and renderer `FUN_1c4f81`). SHOT first copied all its parameters into its own state at note-on, so REV did nothing mid-note; it now reads REV, START and END every block and keeps SMP from the trigger (`modules/machines/shot/src/dsp/shot.s`). Every engine will make this choice, and today each one writes its own copy code.

## Proposal

1. Two views in the voice adapter (`modules/core/adapters/dsp-voice`): `live` (this block's parameter words, as `_dv_params` is today) and `trig` (a per-voice snapshot the adapter takes at note-on). An engine reads the view it needs; no copy code in the engine. Cost: a few words per voice.
2. An optional declaration per parameter, `DK_PARAM_LATCHED(OWNER, WORD)`, a separate descriptor record like `DK_PARAM_GLYPH`. It does not change the DSP. Docs, host test models and the UI ("applies on the next trigger") read it.

## Open questions

- Which parameters of the planned PLAY mode and loop position are latched in stock (loop start is live in stock).
- Whether the `trig` view also covers p-locks on the trigger step (it should: the snapshot is the value at note-on).

## Steps

1. [ ] Em's OK on this note.
2. [ ] Adapter: `trig` snapshot at note-on, both devices; host tests.
3. [ ] SDK record and header; patchkit parse.
4. [ ] SHOT reads SMP from `trig`; its own copy code goes.
