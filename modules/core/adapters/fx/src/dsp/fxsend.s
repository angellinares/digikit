// core/fx, the send route (Digitakt II; docs/findings/62-fx-send-route.md): an effect row with route 1 does not process the master. Its input is a send bus: the sum
// over the 16 tracks of (the row's amount for the track) squared times the track's bus. Its wet output times the row's MIX is added to the stock FX return bus,
// which the master function mixes like the return of the stock send effects. The effect's process routine and the dk_fx contract are the same as in the master
// route (64 planar floats in: 32 left, 32 right; wet out).
//
// The source is in parts (fxsend-a.inc, fxsend-tracks.inc, fxsend-b.inc) that fxsend-dn2.s shares, with its own mix of the 16 voice slots (fxsend-slots.inc).
//
// The hook is the first instruction of the master function (FUN_1c207b, called at 0x1c3099; 48 bits, `I7 = MODIFY(I7, -0x14)`, one caller). The sends driver
// (FUN_1c18a6) has finished: the track buses 0x252df8 + 0x100 * t (16 tracks, 32 left then 32 right floats) are intact, the FX return bus 0x254778 is written (by
// the driver at 0x1c1ef2) and not yet read (by the master at 0x1c21ab). The entry is the same kind of hook as fx.s's: the displaced instruction runs in the tail
// (`_fxs_back`), every register is saved by `_fx_enter` of fx.s, which also pins the context, and `_fx_exec` of fx.s runs one row with the output check, the
// fault accounting and the fades. So the guard exists once.
//
// The block counter `_fx_blk` is advanced by the master hook only (once a block, also when the chain has no master row); this hook runs earlier in the block and
// passes the current value on, so a send effect sees the number of the block before.
//
// Row words (fx.s, module.toml): 18 route (0: master chain, 1: send; other values: the row is run by neither), 19 to 22 the amounts of the 16 tracks, one byte
// for each, track t in word 19 + t / 4, byte t % 4 (little endian; track 0 is the least significant byte of word 19). A byte is 0 to 127 (a larger value counts as 127).
// The gain of a track is (byte / 127) squared, as the stock sends square their knob value (FUN_1c18a6: `F2 = F2 * F2`). Each gain is ramped in a block, linear from
// the gain of the last block that ran this row to the new gain: the gain before sample k is old + (k + 1) * step, step = (new - old) / 32, by adding the step
// per sample (the stock sends ramp the same way). The old gains are kept per row in `_fxs_prev`. A track with the old and the new gain 0 costs a few instructions.
//
// Cost (concrete core, tests/patchkit/test_fx_send.py, the half test effect of about 210 instructions): a block with no send row costs the quick scan, 16 instructions
// (no row) to 31 (three free rows), about 8 for each effect row. A send row, steady: 1264 instructions with all amounts 0 (the context save and restore, the output
// check of fx.s, clearing the bus, the 16 gains, the return add: 256), 1414 with one track, each track with a gain adds 150 (4 instructions a sample), 3664 with 16 tracks.
// A track whose old and new gain are 0 costs 10.
//
// Register use: R0 to R15, I0 to I5, I12, I13, M1 to M4, B3 as fx.s (the hook saves them), M6 = 1 and M14 = 1 pinned by `_fx_enter`.

.EXTERN _fxs_trackbus;

#include "fxsend-a.inc"
#include "fxsend-tracks.inc"
#include "fxsend-b.inc"

// The scalars: 0 the row's index, 8 the track's bus.
.PKDATA _fxs_c, 16;
