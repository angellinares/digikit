# core/overdrive

Seven more curves for the tracks' overdrive. The FX page's OVR ROUTING knob becomes OVR TYPE: pre or post filter, as before,
times eight curves. OVERDRIVE still sets the amount, with more at the top: the drive is the stock drive d times 1 + d^2 / 2,
1.5x at full (d is about 0.99 there), 1.03x at half. Every machine gets it, samples included; the master overdrive is
unchanged.

| OVR TYPE | curve, on z = 8 x drive x (the stock curve's small-signal gain) |
|---|---|
| PRE STK, PST STK | the stock curve: clamp to +-1, then y (2 - \|y\|) three times. Old projects (0 / 0x100) load as these |
| TNH4 | tanh(4 z) |
| HARD | clamp(z, -1, 1) |
| SOFT | z / (1 + \|z\|) |
| TUBE | tanh(z + 0.4) - tanh(0.4): asymmetric, even harmonics |
| FOLD | a triangle wavefolder |
| FUZZ | clamp(4 z + 0.3, -0.6, 1) - 0.3: asymmetric, high gain |
| SQR | clamp(64 z, -1, 1): close to a square |

Every curve keeps the stock dry/wet blend (out = x + mix (y - x)) and the stock drive and mix ramps.

How (firmware 1.17, docs in `src/dsp/overdrive.s`):

- **ColdFire:** OVR Routing (parameter 0x103, sound mirror 68) gets the range 0 to 0xf00, the name OVR Type / OD.TY and a
  formatter (`src/cf/fmt.s`), through core/params' `param_edits`. The value is flavour << 9 | post << 8. The knob cell
  draws as a plain value (class 0xd1), so it shows the type; the header shows it too.
- **DSP, routing:** the frame consumer (`FUN_1c2b24`) copies the frame twice per block; the hooks
  `dsp.frame.hook.frame_rx.copy_cond` and `copy_block` keep each track's flavour (bits 9-15) and leave only bit 8 in the
  frame copy, so the stock code still sees pre / post.
- **DSP, curves:** the two per-track calls of the overdrive stage (`dsp.voice.fn.overdrive`, `FUN_1cb3d8`) are retargeted
  to `_od_sat` (`dsp.voice.hook.chain.overdrive_call`, `chain_ool.overdrive_call`). It finds the voice from the chain
  driver's countdown (R14 = 32 - voice) and runs the stock record handling with the boosted drive and the flavour's curve
  (flavour 0 is a copy of the stock curve). Only a block that is not 32 samples goes to the stock routine.

The firmware facts are declared in `devices/fw/dsp/frame.toml` and `devices/fw/dsp/voice.toml`. `src/dsp/gen_split.py`
writes `split.inc` (the split, unrolled over the 16 tracks) and the curve constants (`consts.inc`, `k.inc`).

Tests: `tests/test_overdrive.py` runs `_od_sat` and the split on the concrete SHARC core (every curve against a float reference,
the record updates and the boost, the registers it must keep, the stock fallback).
