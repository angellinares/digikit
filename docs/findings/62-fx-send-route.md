# 62. The send route of core/fx (FX routing step 1, Digitakt II 1.17, 2026-10-25)

A run-time or patched-in effect of `core/fx` can run on a send bus instead of the master: each of the 16 tracks has an amount for the effect, the tracks are summed into a send bus of our own, the effect
processes that bus (the same `dk_fx` contract: 64 planar floats in, wet out) and its wet output times MIX is added to the stock FX return bus, which the master function mixes like the return of the stock send
effects. This finding records the firmware facts the hook rests on, the row layout, the gain law, the proof and the costs. Marks: **[D]** measured or read once by the author (no second agent has checked the image
bytes yet, so nothing here is **[V]**), **[O]** open. Pieces: `modules/core/adapters/fx/src/dsp/fxsend.s` (the send hook, a Digitakt II object), `fx.s` (the shared guard: `_fx_enter`, `_fx_exec`, `_fx_leave`),
`tests/patchkit/test_fx_send.py` (concrete core), `tools/patchkit/pk_proof_fx.py` part `send`, the scenario `REF:tests/digikit/scenarios/fx-send-dt2.toml` (`tools/patchkit/pk_digikit_fxsend.py`).

## The site and the buses **[D]**

| What | DT2 1.17 | Anchor |
| --- | --- | --- |
| the hook | `FUN_1c207b`, the master function, first instruction `I7 = modify(I7, -0x14)` (48 bits), one caller at `0x1c3099`; reloc hash `cd922cc4...7454` | `dsp.fx.send` |
| track buses | 16 tracks of 64 floats (32 left, 32 right), `0x252df8 + 0x100 * t` (byte addresses, as the other DSP pointers of finding 53); the literal `I13 = 0x252df8` at `0x1c20ea` | `dsp.fx.trackbus` |
| FX return bus | `0x254778`, 64 floats planar like the send buses; written by the sends driver `FUN_1c18a6` (at `0x1c1ef2`), read by the master at `0x1c21ab`; the literal `I5 = 0x254778` | `dsp.fx.return` |

At the entry of the master function the sends driver has finished: the track buses are intact (the stage after the master, `FUN_1c14e7`, probably scales them in place: at the end of a block the track bus is 3.1261 x the bus the effect saw, a constant over all samples, in the proof) and the return is
written and not yet read. The return is mixed into the master at unity: with `test/fx-gain` (0.5) on a track of amount 127 the PCM is the dry PCM times 1.50000, the added part is 0.5 x the track x 1.0000
(scenario `fx-send-dt2`). The rules hold on 1.16 (the DSP blobs are byte-identical; the rules are in `[[anchor_rule]]` of `core/fx`, resolved into `devices/ports/dt2-1.16.toml` and `dt2-1.17.toml`).
The Digitone II has the same stages (master function `FUN_1c1f1e`, finding 53; track buses by `0x258f68 + (slot << 8)` with a slot-to-track map at `0x257898`, return `0x25b868`) and is wired since step 6 **[C]** (finding 67: the send bus is the sum over the 16 voice slots).

## The row **[D]**

A row of the registry `fx` is 24 words (finding 55). The send route takes the former spare words 18 to 22:

| word | name | meaning |
| --- | --- | --- |
| 18 | `route` | 0: the master chain (the default, also for every free row); 1: a send bus; any other value: neither hook runs the row |
| 19 to 22 | `amt0` to `amt3` | the 16 amounts, one byte each: track t (0 to 15) in word 19 + t / 4, byte t % 4, little endian (track 0 is the least significant byte of word 19). A byte is 0 to 127; a larger value counts as 127 |
| 13 | `mix` | on a send row the return level: the wet block times MIX is added to the FX return (1.0 default; MIX 0 adds nothing, the effect still runs) |
| 3 | `enabled` | as for the master route: a switch on zeroes the state and fades the return add in over one block, a switch off fades it out over one block |

The runtime writes route and amounts like the other words (window 2 of `core/dsp-link`, one word per transfer). The master chain skips a row with a route other than 0 (also in its quick scan: a chain with
only send rows costs the master hook the scan, not the context save).

## The gain law **[D]**

The stock sends square their knob value (`FUN_1c18a6`: `F2 = F2 * F2`) and ramp the result over the block (`new^2 - old^2` divided by 32, added per sample). The send route does the same: the gain of track t is
`(byte / 127)^2` (`byte` 127 is 1.0, 64 is 0.2540, 32 is 0.0635; the factor 1 / 127 is the float `0x3c010204`, 32512 / 127 = 256 is the stock raw range 0 to 0x7f00 shifted by 8). Per block the gain goes
linearly from the gain of the last block that ran this row (kept per row and track in `_fxs_prev`; 0 at the start) to the new one: the gain before sample k is `old + (k + 1) * step`, `step = (new - old) / 32`, formed
by adding the step per sample in float32. A track with the old and the new gain 0 is skipped. The send bus is the sum over the tracks in order of increasing track number of `g_k * track[k]` (float32, left and
right with the same gain), starting from 0.

## Where the guard is **[D]**

`fx.s` was split: `_fx_enter` (save the registers, pin the context, count the block), `_fx_exec` (one row: decide switch on, fade or steady, call the process routine, check the output, count faults, set `dis`) and
`_fx_leave` are routines that both hooks call, so the fault window, the 8th-fault switch-off and the output check exist once. `_fx_exec` leaves the output in `_fx_scratch` and returns 1 (blend or add it) or 0; the master
loop blends it into the master (as before), the send hook adds it to the return. The block counter `_fx_blk` (the fault window's clock and `dk_fx_in.block`) advances exactly once per real block, by the master hook only: in `_fx_run`, or, when the chain has a send row and no
master row to run, in the quick scan (the scan counts at the first send row and goes on without counting again). `_fx_enter` only passes the current value on. The send hook runs earlier in the block than the master hook, so a send effect sees the number of the block before (0 in the first block); a master effect sees the same numbers with and without a send row, and the window is 1500 real blocks (`tests/patchkit/test_fx_send.py`).

## Proof **[D]**

- **Concrete core** (`tests/patchkit/test_fx_send.py`, 23 tests; `test_fx.py` 31): the send bus equals a float32 model of the routine bit for bit (ramp block, steady blocks, a change of amount, two send rows in one block, a free
  row in front), the return equals the model bit for bit (fade in, steady, fade out, MIX 0, 0.25, 1), only tracks with an amount contribute, the gain is `(byte / 127)^2` for bytes 0 to 255, all amounts 0 leave the return bit
  for bit, a bad block (NaN, too big, negative result) adds nothing and counts, the 8th fault switches the effect off, 24 hostile contexts give every register back, the stores stay in the module's own memory, the master
  chain does not run a send row (its state is untouched), a master row and a send row work side by side.
- **Emulator, stock images with `test/fx-gain`** (`pk_proof_fx dt2 --only send --quick`, 48 s, 16 checks): route 0 gives master = 0.5 x stock exactly and an untouched return; route 1 with all amounts 0: PCM (38,528 active
  samples), flag, master, ring A and ring C equal stock bit for bit, the effect ran and the master chain did not run it; amount 127 on track 1: the send bus is a constant multiple (the post-master scale, spread 4.8e-8) of the
  track bus, the FX return is the stock return plus 0.5 x the bus bit for bit; amount 64: the bus is 0.253953 of the full one ((64/127)^2 = 0.253953); amount 127 on a silent track 2: PCM, master and return equal stock;
  MIX 0.5: return = stock return + 0.25 x the bus bit for bit.
- **Emulator, run-time effect** (`fx-send-dt2`, kind `fxsend`, quick tier, 83 s): `test/dyn-fx-gain` loaded from the card into row 0, the route and amount words written with `dsp.patch`; all amounts 0 and an amount on a
  silent track give the dry PCM bit for bit, an amount of 127 on the playing track gives dry x 1.50000, an amount of 64 adds 0.25395 of that, the effect's call counter goes up by one a block (the master chain does not run it too).
- Master-route effects are unchanged: `pk_proof_fx` parts `empty` and `gain` (stock-equal PCM and the exact 0.5 master), `fx-dyn-dt2` and `fx-freqshift-dt2` as before.

## Costs **[D]**

- **DSP instructions per block** (concrete core, the half test effect of about 210 instructions): a block with no send row costs the send hook the quick scan, 16 instructions with no row, 24 with one effect row, 31 with three free
  rows. One send row, steady: 1264 with all amounts 0, 1414 with one track, 3664 with 16 tracks (150 for each track with a gain: 4 instructions a sample, software pipelined; the return add is 256). The master chain costs 17
  instructions more for an effect (steady 760, was 743; off 195, was 178) for the call of `_fx_exec` and the route test; the master hook with a send row but no master row costs about 29 (it counts the block); the empty chain (16) and free rows (21, 31, 33) are unchanged.
- **Emulator CPU, the hook alone** (`coupled_ab`, `dt2-sample-trig`, 3 audio s, 3 pairs, same binary, images through `--dsp-patch`; the DT2 `emulator` set with `core/fx` of the base commit against the same set with the
  send hook; PCM equal): DSP CPU per audio s 2.417 -> 2.513, **paired ratio 1.048** (1.015 to 1.059). The changed block at the master function's entry bars the generated region it lies in: +728 interpreted instructions
  a block (66.4 M -> 69.7 M over 3 s), the stock code that the region ran as generated code.
- **Emulator CPU of an active send** (`core/fx` + `test/fx-gain` alone, route 1; same method, 3 pairs): the master route 1.300 per audio s, route 1 with all amounts 0 1.365 (x1.05), one track 1.380 (x1.06), 16 tracks 1.796
  (x1.38). The patched code runs in the interpreter, so each instruction costs more than a stock (generated) one; a send effect with many tracks is a measurable load on the emulator, not on the device (3664
  instructions are 1.7 % of the 211k instructions of a stock block, finding 06).

## Open **[O]**

- The runtime writes route and amounts since step 3 (finding 64: `fxsend.rs`, the load sequence of `dsp.rs`, route and amounts before the pc; a freed row gets both reset to 0 after its pc); the route is the setting `fx/route/<module id>`, the amounts `fx/send/<module id>`, the knobs the send page of the FX key, the limit of two send effects `send_slots` (`fx_slots` stays 3 in total). **[C]** Step 4 (finding 65) added the manifest's `route` and `routes` (the default route and the allowed ones, in the flags word of the effect record) and the FX ROUTES page: the route of a row is the stored `fx/route/<id>` when the effect allows it, else the manifest's default (master when the manifest says nothing); an effect that allows only the master never takes route 1.
- A send effect has a wet-only return: the stock sends' own return levels (the per-effect return knobs in the master, `0x2526e0` ff.) are not modelled; MIX is the only level.
- **[C]** The Digitone II port is done in step 6 (finding 67): `core/fx` builds `fxsend-dn2.s` for it, anchors `dsp.fx.send`, `dsp.fx.slotbus`, `dsp.fx.slotmap`, `dsp.fx.return` in `dn2-1.11.toml` and `dn2-1.12.toml`.
- The first block of a send row is a fade-in of the return (as the master route's), with the gains ramping from 0 at the same time: about 1 block (0.67 ms) of ramp.
- A second agent checks the sites against the image bytes before the marks become [V].
