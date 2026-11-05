# Live and latched parameters in the SDK

Status: implemented for machine contract 1.3. This supersedes the earlier unbuilt proposal: `DK_PARAM_LATCHED` now changes the values delivered to DSP callbacks; it is not metadata only. No second callback argument or new voice-input layout is introduced.

## Declaration and behavior

After declaring a machine parameter, opt it in with:

```c
DK_PARAM(shot, 0, shot_smp, "Sample", "SMP", 0, 15, 0);
DK_PARAM_LATCHED(shot, 0);
```

The owner must be a declared machine and the word must name one of its parameters. A duplicate declaration is an error. Undeclared words keep their existing live behavior.

The voice adapter collects effective parameter words from the current DSP frame and clamps them to their declared ranges. At the actual `note_on` callback, it snapshots declared latched words for that adapter voice. Each subsequent `render` gets those snapshots overlaid on the current live words through the existing `params` pointer. A retrigger replaces the snapshot. There is no change to the callback ABI, `dk_voice_in`, module-owned state size or firmware-owned layouts.

DT2 uses one snapshot per track's even rendering voice. DN2 uses one per physical voice slot, independently of source track ownership, so overlapping notes from the same track retain separate snapshots. DN2's raw trigger-edge block is silent; snapshotting occurs in the following block, when the adapter actually invokes `note_on`.

`_dv_params` remains the live effective scratch buffer used by UI/debug feedback. Callbacks and their fault diagnostics use `_dv_call_params`, the merged view. `_dv_trig_params` reserves 16 × 8 words (512 bytes) of adapter-private DSP storage; the merged buffer adds 32 bytes. Only the active voice's declared words are copied or reused.

## Packaging and compatibility

`patchkit` reads the separate `dk_desc_param_latched` record, joins it to the parameter, and lowers its word into a voice mask. The DSP section uses the previously reserved word at byte 56 of the existing 64-byte v3 voice record. The runtime publishes this mask in digikit voice-row word 22 before publishing the machine type. No record stride or registry-row size changes.

A packed module with a nonzero latch mask requires machine contract 1.3. An older runtime rejects that contract before loading DSP. Modules without declarations retain their old requirements: mono 1.1, stereo 1.2. The callback values of those modules remain live.

## SHOT and validation limits

SHOT declares SMP only. Its render routine reads SMP from the adapter's merged parameter view. START, END, PLAY and PITCH remain live. The bound sample metadata, epoch token and admission/acquire/release state stay in SHOT's voice state; the parameter snapshot does not replace those lifetime checks.

Focused descriptor/packaging tests cover owner/word validation, duplicates and contract requirements. Concrete SHARC adapter tests cover clamping, delayed DN2 capture, retrigger, and overlapping physical slots; SHOT tests cover SMP retention alongside live START/END/PLAY/PITCH and existing PCM/epoch behavior. These tests provide effective frame values at the callback boundary. Real firmware sequencer timing for trigger-step p-lock delivery remains unverified by these host tests; no device p-lock timing claim is made.
