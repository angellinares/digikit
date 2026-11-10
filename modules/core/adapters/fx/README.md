# FX chain

**Category:** core  **Devices:** Digitakt II 1.17; Digitone II 1.12 (the send route: both)  **Status:** experimental (emulator only)

A framework module: it lets other modules process the final stereo block of the device with their own DSP routine (the FX contract,
`<dk/fx.h>`). On its own it changes nothing: a patched image with an empty chain plays exactly the stock audio.

- What changes on the device: the first instruction of a function the DSP calls once per block after the master mix and before the USB pack (finding 55:
  Digitakt II `FUN_1c80f2`, Digitone II `FUN_1c244d`) is replaced by a jump into this module. The effect runs before the USB pack and the DAC converter, so it is in the USB main output and in the
  DAC. The track buses (per-track USB outputs) are not touched.
- The chain: up to four effects, rows of the registry `fx` (module order). A row is 24 words: pc, desc, state, enabled, the eight parameter words p0 to p7
  (`DK_FX_MAX_PARAMS`), then the adapter's own words (see `module.toml`). Patched-in effect modules add a row with `[[entry]] registry = "fx"`
  (`pc`, `desc`, `state`). A row with pc `0xffffffff` is free and skipped: `core/dynfx` reserves such rows for effects that the digikit runtime loads at run time
  (a `.dkm` with a DSP section of version 4; `docs/design/digikit-dkm.md`); the runtime fills one through window 2 of `core/dsp-link`, the pc word last.
- The send route (Digitakt II finding 62; Digitone II finding 67): a row with route 1 (word 18) does not process the master. A second hook, the first instruction of the master function (`FUN_1c207b`, before the master mixes the
  FX return), builds a send bus: the sum over the 16 tracks of the track's bus times its gain, gain = (amount byte / 127) squared, ramped over the block like the stock sends. The effect processes that bus (the same
  contract: 64 planar floats in, wet out) and its wet output times MIX is added to the stock FX return bus, which the master mixes at unity. The amounts are the bytes of row words 19 to 22 (track t: word 19 + t / 4, byte
  t % 4, 0 to 127). The master chain skips send rows. On the Digitone II (`FUN_1c1f1e`, object `fxsend-dn2.s`) there are no track buses before the master function, so the bus is the sum over the 16 voice slots of the slot's track gain times the slot's bus (the slot-to-track map `0x257898`; a chord is the sum of its slots). The two hooks share the guard (`_fx_enter`, `_fx_exec`, `_fx_leave` of `fx.s`: output check, faults, switch-off). A block with no send row costs the send hook 16
  to about 31 instructions (the master hook with a send row: about 29, it counts the block); one send row 1264 with all amounts 0, plus 150 for each track with an amount (3664 with 16 tracks); the emulator pays x1.05 DSP CPU for the hook (finding 62).
- Per block and effect: the output is checked (finite, magnitude at most 8.0); a bad block plays dry and clears the effect's state; the 8th
  fault within 1500 blocks switches the effect off until the next start. A switch on or off is a linear crossfade over one block.
- MIX: row word 13 is the row's dry/wet blend (a float, default 1.0): the effect writes only the wet signal and the adapter makes `dry + mix * (wet - dry)`; mix 1.0 is the wet block bit for bit, mix 0 the
  dry block bit for bit. The effect page of a run-time effect sets it (`core/fxpages`, finding 61).
- Empty chain: 16 instructions per block with no row, 21 with one free row, 31 with three free rows (a quick scan of at most four rows, about 5 instructions per free row); one effect adds
  4 to the bookkeeping of its row (steady 760 at mix 1.0, 1159 at another mix, 622 at mix 0, off 195 instructions per block).
- Tests: `tests/test_fx.py` (the adapter on the concrete core), `tests/test_fx_send.py` (the send route), `tests/support/fx_rig.py`.
