# Stock one-shot playback on a Digitakt II (OS 1.17)

Static reads of the stock SAMPLE (one-shot) machine on DT2 1.17, checked on 2026-11-02 against a real Digitakt II with the SHOT machine (modules/machines/shot) and the device probe (tools/device/devprobe.py, USB audio through PortAudio). Marks: [D] read or measured once; a second agent must check against image bytes before [V].

## Live and latched parameters [D]

- The per-voice loop calls the sample setter `FUN_1c4afe` every block for every voice (dispatch 0x1c6530..0x1c6acc, selector 0 through table 0x8055c840 to 0x1c65bd).
- Every block: the step from note, TUNE and the rate word; the step sign from PLAY (reverse flag, byte +0x1bb); the loop flag (+0x1bc); START, LEN and LOOP turned into positions by `FUN_1c4914` (end = START + LEN, clamped).
- At the trigger only: the sample slot (`FUN_1c3289` load block 0x1c3329, `FUN_1c4e70`), and the phase seed (written only while +0x1ba is set: START, or end-1 in reverse).
- So a PLAY direction change flips at the current cursor; an end moved behind the cursor stops the voice in that block; in forward play a START above the cursor has no effect.
- On the device, SHOT with the same rules (REV/PLAY, START and END read every block, SMP latched) flips direction mid-note when PLAY crosses the middle (checked by Em by hand) and stops early when END passes the cursor.

## End of playback [D]

- The renderer `FUN_1c4f81` renders 64 work samples per block at 96 kHz and writes 32 outputs through `FUN_b80000`.
- When a forward non-loop cursor passes the end, ACTIVE is cleared but the block is finished and multiplied by a linear ramp from 1 to 0 over the 64 work samples (code from 0x1c51e7); later blocks are silent. A fade-in ramp exists at 0x1c518a (flag +0x17c; its setter is [O]).
- SHOT now applies the same one-block ramp (sdk/dsp/gain_ramp.inc). On the device the last note decays over about 150 samples with no step larger than a 2 kHz tone's own sample step.

## Level [D]

- The stock 6-tap interpolator table at 0x25d940 (256 rows, Q31) has a DC gain of 0.40439 to 0.40465 per row (mean 0.4045, -7.86 dB). Nothing after the voice adds stock-only gain (emulator: voice buffer, pan planes and main out compared for stock and SHOT on the same track).
- `DK_SAMPLE_STOCK_GAIN` (sdk/dsp/sample.inc) is that value. With it, SHOT on the same track is +0.006 dB against stock in the emulator; on the device SHOT dropped by the expected 7.9 dB.
- In the emulator, track 2 played 9.2 dB louder than track 1 for both machines (voice buffer 2.68x, pan +0.6 dB); voice record words +0xcc differ (0.828 vs 0.859). Cause [O].

## Device input through the remote module [D]

- Remote KEY and TURN work on the device (`digikit/remote`). One remote encoder detent moved END by about 1/6 of a knob unit; TUNE +120 detents gave about +20 semitones. Fast turns may accelerate, so a reached value is unknown without a read-back [O].
- The Python screenshot path (tools/device/dkscreen.py) once returned the stock page while the device showed the digikit page; the workbench Remote page (dk-session `screen()`) showed it correctly. Both send MidiRpc type 0x04 and match replies by type only, not by sequence number [O].
