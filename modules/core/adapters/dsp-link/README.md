# DSP link

`core/dsp-link` is a framework module: it lets the digikit runtime send bytes (DSP code and data) to the DSP while
the device runs. Nothing changes for the player. It ran on a Digitakt II (OS 1.17), where `fx/freqshift` loaded through it and ran (2026-10-30, finding 91); the Digitone II has run it in the emulator only.

- Firmware: Digitakt II 1.17 and Digitone II 1.12 (the same anchor rules on both).
- What it adds: a hook on the ColdFire's per-block frame sender (to put a chunk in the unused end of the frame) and a hook
  at the end of the DSP's block handler (to take the chunk, copy it into a reserved area of free L2 (512 KiB on the Digitakt II, 320 KiB on the Digitone II), and
  acknowledge). The ColdFire interface is `dl_send(dst_l2, buf, len)`, `dl_send_win(win, dst_word, buf, len)` (window 1: the voices registry of
  `core/dsp-voice`, window 2: the effect rows of `core/fx`, one word at a time) and `dl_poll(status)` (C ABI).
- Speed: about 960 KB/s on the Digitakt II and 72 KB/s on the Digitone II; with no transfer it costs the DSP about 90
  instructions per block (about 0.04 % of a block's cycles: 24 for the link itself, 23 to pin and restore M5, M6, L2 and L3, 43 for
  the diagnostic words below and the record addresses; findings 45 and 48).
- Diagnostics (the fault table): `int dl_diag(u32 *out, u32 n)` (ColdFire C ABI, optional link) returns the DSP's diagnostic words of the last
  block, 32 words (24 before runtime 0.5.3, 17 before runtime 5), or a negative number when there are none (-1: no record address in the image;
  -2: the reply carries no diagnostic block, or the DSP has stopped writing it). The link fills the table in every image, with or without
  `core/dsp-voice`; the words of a site that is not in the image stay 0. The words travel in the DSP's reply record, two per slot row (rows 1 to 16, bytes
  +0x44 and +0x48 of the row, the entries 11 and 12 that no DSP code writes); the exact offsets are in `src/dsp/dsp-link.inc`, and
  `tools/patchkit/pk_ctxprobe.py` reads them in the emulator. The record has 32 rows, so no row is added; on the Digitone II these bytes are samples of
  the effect buses (finding 31), and rows 13 to 16 add four to the twelve used before. Layout:

  | word | content |
  | --- | --- |
  | 0 | `0xD1A6` in the low half, the word count (32) in the high half |
  | 1 | blocks that ran the link's hook (a heartbeat of the block handler) |
  | 2 | blocks that reached `core/dsp-voice`'s hook (its heartbeat) |
  | 3-8 | M5, M6, M7, M13, M14, M15 |
  | 9-13 | I6, I7, B7, L7, MODE1 |
  | 14 | the voice site's `last`, the breadcrumb: stage in bits 0-7, voice (0-31) in bits 8-15, machine type in bits 16-31 |
  | 15 | dispatches to registered machine types since the start |
  | 16 | routines that returned (15 minus 16 is 1 while a routine runs; a core stopped inside one leaves it at 1) |
  | 17 | the guard of `core/dsp-voice` (finding 51): faults counted so far (low 16 bits) \| kind of the last fault << 16 (1 Inf or NaN, 2 over the level bound) \| machine types disabled << 24 |
  | 18 | type \| voice << 16 of the last fault |
  | 19-22 | the eight machine parameter words of that call (clamped, as the routine saw them), knob 2k in the low half, knob 2k + 1 in the high half of word 19 + k |
  | 23 | the machine type the guard disabled last |
  | 24 | the effect site's `last` (`core/fx`): stage (4 the core is about to call the module, 5 it returned), site << 8 (2 master route, 3 send route), the row's byte offset in the effect table << 16 |
  | 25 | the module step: what the last module call of the block stored into `_dk_step` (`DK_STEP`); the sites clear `_dk_step` right before each call into a module, the tail copies it |
  | 26-27 | module calls of the master route and of the send route |
  | 28 | effect faults (low 16 bits) \| the row of the last fault (byte offset + 1) << 16 |
  | 29-30 | the addresses of this block's reply record and of the one before |
  | 31 | the effect row switched off last (byte offset + 1) |

  Words 3-13 are the registers as the firmware left them at the first voice of the block, before the wrapper pins anything. A frozen heartbeat is a halted
  core. The breadcrumb names the step of a machine's path that `core/dsp-voice` reached last: 1 hook entered (every voice; the type is not known yet and
  the field is 0), 2 type registered, 3 stock path taken for a type of 7 or more that no module registered, 4 routine about to be called, 5 routine returned,
  6 the odd voice's copy done, 7 epilogue done. The wrapper writes it into the reply record at once (both reply records, header and dispatch count with it),
  not only at the end of the block, so that it is there when the core stops inside a routine and never reaches the block's end: the DMA that sends the record
  does not need the core (finding 48). The effect site (word 24) is written the same way. On the Digitone II the voice wrapper
  writes only the stages 4 and 5 (the slot is the voice) and words 15 to 23 are 0 (the dispatch counts and the guard belong to the Digitakt II wrapper; the
  Digitone II wrapper only scans rows of the new size).
  The module step (word 25): `core/dyndsp` exports `_dk_step`, a 32-bit DM word of this module. A module stores its step there. The sites that call modules
  (`core/fx`, `core/dsp-voice`) clear `_dk_step` right before each call into a module, so a step belongs to the call that stored it. The block tail (`src/dsp/dsp-link.inc`)
  copies `_dk_step` into word 25 of the record of the block, after all module calls of the block, and clears it again. So every record carries the step of its block
  (the last module call that stored one; 0 when the last module stored nothing or no module ran), 1500 words a second. The tail also gives each site the address
  of `_dk_step` (`_fx_rec[2]`, `_dv_rec[2]`; 0 until the first block with the link), because the sites do not depend on this module.
  A step stored in a call that stops the DSP is lost with the core: the record of a stopped DSP carries the step of the last block that ended (finding 77).
  **[D]** Why the first design lost the word (emulator, DN2 1.12, 2026-10-31, `test/step-beacon` with `digikit/probe`, `docs/findings/101-module-step-word-on-hardware.md`).
  The two reply records alternate, one per block, and the ColdFire reads a record while the next block runs: the record that the tail filled at the end of block B is
  read after the hooks of block B + 1 have run. The sites cleared word 25 in both records before each module call (`_fx_rec`, `_dv_rec` hold the record of block B and
  the one before), so the clear in block B + 1 removed the step that the tail had just copied into the record of block B. The old scheme (the runtime pointed the module's stores at
  word 25 of one fixed record, the older of the two, `patch_steps`) worked only because the module stored after the clear, into that record, and only every second record has that
  address. With the clear taken out of the records (the experiment: the mark did not clear, the tail copied the static word) the module's words arrived in every block, so the static
  word, the relocation of the import (the store in the module image held the address of `_dk_step`) and the tail copy were right all along. On a Digitakt II (OS 1.17) the clear also
  ran while the reply DMA was sending the record, and 3995 of 4000 words were 0 there.
  Order of events in one block **[D]** (read in the images): the effect hooks run before the tail in the same block. DT2 1.17: the handler `0x1c75d8` calls the orchestrator
  `0x1c2b24` at `0x1c771e` and the function `0x1c74cd` (hook site `0x1c75c1`) at `0x1c7756`; the orchestrator calls the master hook site `0x1c80f2` at `0x1c311f` and the send hook
  site `0x1c207b` at `0x1c3099`. DN2 1.12: handler `0x1c9e76`, orchestrator `0x1c2712` at `0x1c9fbc`, tail function `0x1c9d6b` at `0x1c9ff4`; effect sites `0x1c244d` (call
  `0x1c307b`) and `0x1c1f1e` (call `0x1c305d`).
- Per-block callback: `dl_set_tick_cb(cb)` (a runtime import for modules, `requires core/dsp-link`; a no-op stub when the module is absent) sets one
  function, `void cb(const volatile u16 *rec)`, that `dl_tick` calls every block in the block interrupt, before the transfer code, with the copy of the
  previous reply record (diagnostic word j: see `dl_diag`). NULL is off and costs one pointer test per block. The callback must be a short C function with
  no allocation and no logging, and its module clears it in `fini`; the runtime also clears it after a `fini` when it points into the module
  (`dl_tick_cb_drop`).
- Limits: the destination must lie in the module's area (`_dl_area`); the DSP code that runs from it must be linked for
  that address or relocated by the runtime (`docs/design/digikit-dkm.md`, "DSP section"). The emulator does not model the DSP caches, so the cache invalidate after the last chunk has not run on
  a device.

Design, measurements and open points: [docs/findings/31-dsp-link.md](../../../../docs/findings/31-dsp-link.md) and, for the windows and
the wider chunk header, [docs/findings/32-runtime-dsp-machines.md](../../../../docs/findings/32-runtime-dsp-machines.md). Proof:
`uv run python -m tools.patchkit.pk_dsplink proof --device dt2|dn2`; tests: `tests/patchkit/test_dsplink.py`.
