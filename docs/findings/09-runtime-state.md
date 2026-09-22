# Runtime state

Live musical state in RAM: the pattern and kit working-set tables.

## Live musical state in RAM: the pattern and kit working-set tables **[D][O]**

Where the currently-loaded project lives while the device is running, found while
looking for a way to capture live memory (the MIDI/UART memory-access surface
audit is in `docs/MIDI-SYSEX-RPC.md`; this is the 1.16 image). Two parallel
flat-POD tables, both 128 slots, indexed by the same pattern index (`< 0x80`
guard, or clamp `0..0x7f`):

- **Pattern table — `0x41776cb8`, stride `0x1db8c` (117,644 B), 128 slots.** The
  sequencer/trig data for the one loaded project (128 patterns). The live
  step-record path `FUN_4011fe12` writes per-track step data at
  `entry + 0x481 + track*0x6a5` and `entry + 0x482 + track*0x6a5` (8 tracks,
  per-track stride `0x6a5` ≈ header + 128 steps), so the trig region starts
  around `+0x481`. The rest of the 117 KB entry (p-lock lanes, song/scale
  metadata, sample assignment) is uncharacterised. `#PLAY_PATTERN` (UART console)
  hard-codes slot 0 = `0x41776cb8`; pattern undo/copy (`FUN_40041d68`,
  `FUN_400442f0`, caption `"Undo Pattern(s)"`) memcpy whole `0x1db8c` slots,
  confirming the block is flat POD.
- **Kit / sound-parameter table — `0x426532b8`, stride `0x5574` (21,876 B), 128
  slots.** The kit bound to each pattern slot. Per-track machine/sound parameters
  at `entry + 0x48 + track*0x450` (8 tracks, stride `0x450`), passed to the
  sound-engine setup `FUN_400d67b2`/`FUN_400d6cbc` keyed off the current machine
  type `DAT_402b49f4`. FX and kit-level data (the remainder) uncharacterised.

`ProjectCache` (`0x400f49fc`, a `StaticSingleton` `Observer`) is the
wrapper/observer, not the backing store; the buffers are reached through a
pattern-manager object (`+0x43d0` = 128 Pattern wrappers of `0x954` B, vfunc
`+0x28` returns the `0x41776cb8` slot pointer; `+0xf4` = the kit collection,
vfunc `+0x30`/`+0x5c` return the `0x426532b8` slot). "Cache" here is the single
in-RAM working copy of the loaded project, not a multi-project cache.

A full live snapshot of the active pattern needs **both** tables at the same slot
index. Both are flat POD, so an emulator can memcpy `[base + idx*stride, +stride)`
from each. This is the practical route to inspect/capture live musical state:
there is **no** MIDI or UART command that dumps these tables (`#MRAM_DUMP` stops
24 bytes short, at `0x41776c9c`; see `docs/MIDI-SYSEX-RPC.md` §9). **[D][O]**

Open: the flash→RAM project-load routine that populates these tables; the bulk of
each entry's offset map; confirming the pattern-manager `param_1` resolves to the
`Project` singleton. Single-agent reads (2026-09-20), not second-agent
byte-checked. **[O]**

