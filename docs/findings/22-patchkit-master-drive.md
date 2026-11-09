# 22. patchkit: digikit/master-drive on both devices

Plan: `docs/plans/patchkit.md` (Phase 4.1). Module: `modules/fx/master-drive` (README there). Tools:
`tools/patchkit/pk_proof_drive.py`, `tests/patchkit/test_master_drive.py`, `tests/patchkit/test_master_drive_emu.py`. Marks as in AGENTS.md. One
agent, emulator only, not checked by a second agent: **[D]**. Nothing was flashed.

## What the chain is **[D]**

The knob is the master compressor's Sidechain Filter, fx[66] (finding 04), the 7th of the 8 encoders of the FUNC+MOD
master page. `input.turn` and the scenario files number encoders from 1: **encoder 7**, not 8 (8 is MIX; turning it
mixes the compressor in and changes the PCM). The DSP routine (a soft clip with a dry/wet crossfade, one slewed
parameter) runs at the Q31 converter's call (finding 06, "Master hook"); the ColdFire side only renames the knob.

- DSP anchors by rule: the stock reader's short-word load of the word (`dsp.fx.sidechain_filter_word`, new rule kind
  `sharc_mem_addr`: the absolute address of a direct DM access in a function found by `reloc_hash`): DT2 `0x256008`
  (function `0x1c2b24`, +0x459), DN2 `0x25ced8` (function `0x1c2712`, +0x87d), the same on 1.16/1.11 (same DSP
  blobs). The two functions differ, so the rule has one entry per device.
- ColdFire anchor by rule: the 60-byte descriptor row (`cf_words`, a masked row signature: mirror index 66, min 0,
  max 0x7f00, default 0x5000, +0x14 = 0, +0x18 = 0x75ffff, +0x1c = 0x116; owner and string pointers wild; exactly one
  match per image). DT2 1.17 `0x40211a68`, DN2 1.12 `0x401fa870` (DT2 1.16 `0x40211658` = table + 0x3c*0x9d).
- The module writes two string pointers into the row (+0x28 long name `Master Drive`, +0x30 short label `DRV`), with
  the strings in `cf.expansion` (17 bytes). The master page then reads `DRV` under the knob and `Master Drive=63`
  while it turns (framebuffer PNGs under `out/runs/md-proof/<device>/`, not committed). The icon above the label stays
  the filter curve (the page layout picks it, not the row).

## The descriptor default is not where a kit starts **[D]** **[C]**

Patching the row's default (+0x10) from 0x5000 to 0 had **no effect on the live word**: after a coupled cold boot of
the patched MAIN the DSP copy of fx[66] still read `0x5000` (`dsp.peek` at `0x256008`, DT2). Factory kits are copied
from the stored preset project (finding 02, "Value storage"), so every kit starts at the preset's value, and every kit
saved by stock firmware holds `0x5000`. Neutral therefore has to be **0x5000 (UI 80; the page shows 16)**: the DSP
treats `0x5000` and below as no drive and maps `0x5000 .. 0x7f00` onto 0 .. 1. The module leaves the default alone.
**[O]** Whether the Clear gesture reads the descriptor default was not tested.

## Numbers **[D]**

DT2: one-sample card (220 Hz sine), TRIG 1 held; DN2: the TRIG 1 note of `tests/regression/scenarios/dn2-sendfx.ts`. Same panel
script for every run (`tools/patchkit/pk_proof_drive.py`), PCM from `pcm.tap`, left channel active part. "step" is the
largest |x[n] - x[n-1]|.

| run | word | DT2 RMS | DT2 non-fundamental | DT2 step/peak | DN2 RMS | DN2 non-fund. | DN2 step/peak |
|---|---|---|---|---|---|---|---|
| stock, knob at rest | 0x5000 | 0 dB | 1.67e-5 | 0.080 | 0 dB | 7.6e-5 | 0.033 |
| stock, knob to the top | 0x7f00 | 0 dB, bit-identical to rest | | | 0 dB, bit-identical | | |
| patched, rest | 0x5000 | 0 dB, bit-identical | 1.67e-5 | 0.080 | 0 dB, bit-identical | 7.6e-5 | 0.033 |
| patched, middle | 0x5a00 / 0x5c00 | +2.1 dB | 1.68e-5 | 0.080 | +2.9 dB | 7.6e-5 | 0.033 |
| patched, top | 0x7f00 | +14.6 dB | 6.7e-5 | 0.082 | +14.7 dB | 1.57e-4 | 0.035 |
| patched, top, knob swept during the note | to 0x7f00 | +6.5 dB | 7.2e-5 | 0.084 | +10.2 dB | 2.19e-4 | 0.036 |

- Stock insensitivity: with the compressor unmixed, moving the Sidechain Filter word 0x5000 -> 0x7f00 leaves the PCM
  bit-identical on both devices (the control the module relies on). **[D]**
- Bypass: patched at rest equals stock over the whole active region. DT2 at sample offset 0. DN2 sits **one DSP
  block (64 samples) away**: the cold-booted patched ready state has a different capture phase from the stock ready
  state; with that shift every active sample is equal (`aligned_equal` in the tool). Not an effect of the module.
- The test note is quiet (peak -29 dBFS DT2, -25 dBFS DN2), so the drive acts as a gain of about +15 dB on it
  (small-signal gain `G m` = 5.6 at the top) with a few 1e-4 of harmonics; a loud master peak at full drive comes out
  about 5 dB lower. The sweep run has the same step-to-peak ratio as the fixed settings (within 10 %): no clicks.
- No `cf.error`, no DSP halt in any run; the DSP kept running (3,864 frames DT2 per script). There is no fault event
  yet, so this is the status read-back (Phase 2 adds events).
- Coupled cold boot of the patched MAIN: DT2 about 11 s, DN2 about 7 s uncached (consistent with finding 17).

## Cost **[D]**

Concrete SHARC+ core, one block (`tests/patchkit/test_master_drive.py`): **20 instructions** with no drive (early exit before
the saved registers), **968** at full drive (two 32-sample loops of 12 instructions plus the coefficient set-up).
A block is 666,667 cycles at 1 GHz, so the full-drive cost is about 0.15 % of a block (one instruction per cycle,
no cycle factors). The routine has no division and no RECIPS.

## Toolchain facts found on the way **[D]**

- **Selache ignores the `(SW)`, `(SE)`, `(SWSE)` suffix of a direct load** (`R0 = DM(sym) (SWSE);`) and emits the
  normal-word 14d form without a diagnostic. `tools/patchkit/pk_sharc.py` now assembles such a line as the normal-word load and
  rewrites its first parcel pair to the short-word form (opcode 0x1a, bits 38 and 36), so the re-decode sees
  `R0 = DM(0x256008) (swse)` exactly as the firmware's instruction. `tests/patchkit/test_master_drive.py` asserts the text.
  **[D]** Fixed in the Selache fork (`fix/encodings`, "selas: encode (BW)/(BWSE)/(SW)/(SWSE) on a direct load or store as Type 14d"): `selas`
  writes Type 14d itself, for loads and stores (a store takes no `SE`), and patchkit's rewrite is gone. The indirect forms
  (`R1 = DM(I4, M5) (SW);`) are still assembled as normal-word accesses **[O]**.
- An uninitialised `.VAR x;` is bss with no size: eight of them all got one address. Module data uses initialised
  `.VAR`.
- The concrete core models `RECIPS` and `RSQRTS` with the measured seed tables ([102](102-recips-rsqrts-seed-tables.md)). The routine does not use them.
- A stale `target/release/cfdis` (built before `--ranges`) made `pk_link.check_isa` fail with exit 2; rebuilding it
  fixed `tests/patchkit/test_pk_cf.py`. Data-only objects (no text) skip the ISA check.

## Open **[O]**

- Hardware: no DSP block is known to be accepted by the real SHARC boot ROM past the stock stream (finding 06); the
  ColdFire loader runs from reset in the emulator only.
- R5-R7 and R9-R13 are saved and restored by the routine because nothing documents them as dead at the call; whether
  they are dead was not checked.
- Kits saved with a high Sidechain Filter value (the compressor user's setting above UI 80) will start with drive on.
- DT2 1.16 and DN2 1.11 resolve the anchors (the ports were regenerated) but were not run with the module.
