# 48. DSP breadcrumbs: where the SHARC is when it stops, and a rehearsal of rung C

Phase 9, rung C (docs/plans/digikit-runtime.md). Rungs A and B ran on Em's Digitakt II (finding 47); rung C plays an added machine type for the first
time, the step at which another user's Digitakt II halted its SHARC for a cause nobody has found (finding 45). This finding adds what lets the next halt
name its place, and rehearses rung C in the coupled emulator with the images and commands Em will use. Marks as in AGENTS.md: **[D]** measured here,
**[O]** open; nothing is **[V]** (no second agent has checked the image bytes). Emulator only: **the device has not run any of this.**

## 1. The breadcrumb

`core/dsp-voice` (Digitakt II wrapper) writes one word before each step of a block's machine path:

`word = stage | voice << 8 | machine type << 16`

| stage | step | written by | when |
| --- | --- | --- | --- |
| 1 | HOOK_ENTERED (type not known yet: field 0) | every voice | DSP-local word only |
| 2 | TYPE_FOUND: the type is in the registry | registered type | DSP-local word and both reply records |
| 3 | STOCK_PATH_TAKEN for a type of 7 or more that no module registered | such a voice | both |
| 4 | ROUTINE_CALLED: about to jump into the routine (even voice) | registered type | both |
| 5 | ROUTINE_RETURNED | registered type, even voice | both |
| 6 | ODD_COPY_DONE: the odd voice copied the even voice's block | registered type, odd voice | both |
| 7 | EPILOGUE_DONE: every register back but the saved R registers and L4 | registered type | both |

A stock type (below 7, the engine's own) leaves only stage 1: its cost is 6 instructions a call. With no registered type (`_dv_mask` is 0, built by voice 0 once per block) a voice other than 0 leaves no breadcrumb and returns to the stock renderer after one check. Two counters go with the word: dispatches to registered
types (counted at stage 4) and routines that returned (at stage 5). `dispatches - returns` is 0 between calls, 1 while a routine runs, and stays 1 when
the core stops inside one.

**Layout of the reply-record diagnostics, now 17 words** (`dl_diag`, `modules/core/adapters/dsp-link/README.md`, `src/dsp/dsp-link.inc`): words 0 to 13 as in
finding 45 (header `0xD1A6 | count << 16` with count **17**, the link hook's beat, the voice hook's beat, M5 M6 M7 M13 M14 M15, I6 I7 B7 L7 MODE1), and

| word | content | record bytes |
| --- | --- | --- |
| 14 | the breadcrumb | row 8, +0x44 (0x300) |
| 15 | dispatches to registered types | row 8, +0x48 (0x304) |
| 16 | routines that returned | row 9, +0x44 (0x354) |

Rows 8 and 9 are two more slot rows, entries 11 and 12, which no DSP code writes and the ColdFire reads only for parameters 36 to 39 (finding 31). On the
Digitone II the three words are 0 (its wrapper has no breadcrumbs; its `_dv_diag` grew to 64 bytes so that the shared tail copies zeros).
`tools/patchkit/pk_ctxprobe.py` reads the 17 words from the DSP and from the ColdFire's record copy; `tools/device/dklog.py` shows them by name.

## 2. Does a stopped core's last breadcrumb reach the ColdFire?

**Why the obvious design fails.** The block-tail hook of `core/dsp-link` copies the diagnostics into the reply record, and it runs once per block, at the
end of the block handler. A core that stops inside a routine never reaches it, so the record would carry the breadcrumbs of the last *complete* block:
nothing about the routine. The engine also clears and refills the record in every block (finding 31). Therefore the wrapper writes the breadcrumb (with
the header and the two counters) **straight into both reply records** at stages 2 to 7. The addresses come from `core/dsp-link`'s tail, which stores each block's
record address in `_dv_rec` (two entries: this block's and the one before; the next block builds into the one before). A zero entry (before the first
block, or without `core/dsp-link`) skips the write.

**Static evidence that the record keeps being sent without the core [D].** The SPI2 transmit DMA of the DSP runs in descriptor-list mode (flow type 4 of
the DMA configuration): the DMA engine takes the next work unit from the descriptor chain by itself when one is done, and the work units are the two reply
rings of finding 31 (DT2 `0x266220` and `0x267220`). The emulator's model of that channel (`packages/emulator/sharc-rt/src/rt/periph.rs`, `dma_start`,
`dma_done`, `dma_fetch`) accepts only the stop and descriptor-list flows for it and walks the chain with no core involvement; the firmware's ring toggle word is
the handler's own bookkeeping. This is the model of the hardware the stock firmware runs on in the emulator, not a measurement of a halted core.

**Emulator evidence [D].** Scenario `debug-hang` (`tests/digikit/scenarios/debug-hang.toml`, full tier, about 5 minutes): the test module
`test/dyn-dsp-hang` (run-time type 19) has a routine that renders silence and, on its third active block, enters an endless loop. Held on track 1:

- the stop is declared after 4.0 s of emulated time (3 s without a beat, polled once a second); `DSP_STOPPED` carries `heartbeat=15560 voice_beat=11157
  crumb=ROUTINE_CALLED voice=0 type=19 dispatches=4`, and the five `DSP_DIAG` events of the same reply show `returns=3`: **the last breadcrumb reached the
  ColdFire through the reply record while the core was in the loop**;
- both reply records arrive alternately with their last words (the second shows the beats one block later): two distinct sets, both logged, both with the same
  breadcrumb word;
- `MOD_LIST` follows (one event per loaded module), and ABOUT shows `DSP STOPPED` and `LAST: CALL V0 T19` (PNG of the page kept by the scenario).

**What the emulator does exactly.** It does not halt: each frame the DSP worker runs the whole period (666,667 instructions) in the loop, the SPI2 exchange
takes the work unit the DMA names and advances the chain, the ColdFire receives the last records. A *trap* in the emulated core (an unmodelled peripheral
register, an illegal opcode) is another case: the worker marks the DSP halted and from then on answers every frame with zeros (`sharc_peer.rs`, `exchange`: "Detached or halted: zero
reply"). That case is not what a stuck loop on silicon does (the DMA keeps going there as in the first case) and the scenario does not exercise it; the runtime
handles it: a reply without the diagnostic block (`dl_diag` = -2) counts as "no beat" once the DSP has been seen alive, so `DSP_STOPPED` is declared after 3 s
with `crumb=NONE` and no breadcrumb. It is implemented, not run **[O]**.

**On silicon [O].** What is not shown: that the real DMA keeps sending the two rings after the core stops inside a routine. Evidence for it: the descriptor-list
mode above, and the 2026-10 report (the ColdFire's DSPI keeps shifting frames, the +Drive freezes). Against: nothing known. If it does not, the ColdFire sees an
unchanging or all-zero reply (the DSP's SPI would send its FIFO leftovers); the runtime then declares the stop from the frozen heartbeat, or from the missing
diagnostic block, and the best signal left is **which heartbeat froze**: `link_beat` frozen means the block handler never finished; with the DMA dead the
words are all that came through before. The first rung C halt on the device answers this by itself: a `DSP_STOPPED` with a named crumb confirms it; with
`crumb=NONE` it refutes it.

**Two limits of the breadcrumb.** A halt in stock code (a stock voice, the rest of the engine) leaves the last registered voice's breadcrumb, usually `EPILOGUE_DONE`:
read `dispatches - returns` (0: no routine was running) and the frozen beats. The crumb word the ColdFire sees in a healthy run alternates between two values
depending on which record the copy came from: the last voice that wrote one and the last registered voice's `EPILOGUE_DONE`. With no registered type only voice 0 writes (`HOOK_ENTERED voice=0`).

## 3. Quieter log, full words on a stop

`dlog.rs` (finding 46): `DSP_DIAG` (five events: 17 words, four to an event) at the first poll, then only when a word other than a counter (words 1, 2, 15, 16) changes,
and in any case every ten seconds. On `DSP_STOPPED` the event carries `heartbeat, voice_beat, breadcrumb, dispatches, resumed` and the whole diagnostic set follows
as error events (kept when DEBUG is off); for 400 UI passes the poll then reads the reply on every pass and logs each distinct one (the two records). The
heartbeat watch counts only a larger value as a beat: with two records in play a stopped DSP would otherwise look alive (the alternating words differ).
The ABOUT page (`DkrtDiag::dsp_crumb`, `modules/core/apps/digikit-menu`) adds `LAST: CALL V0 T19` under `DSP STOPPED`. `dklog.py` decodes the breadcrumb as stage name, voice
and type. **[D]** `debug-log` full tier: 16 `DSP_DIAG` events in 32 s (four sets: the first poll and three ten-second ones), against over 25 before.

## 4. Cost **[D]**

The emulator counts one cycle per instruction. DT2 1.17, `pk_ctxprobe --cost --modules core/dsp-voice,core/dsp-link`, the DSP's own block count, mean of 200 blocks
after 300 warm-up blocks, HEAD against this change:

| | HEAD | now | difference |
| --- | --- | --- | --- |
| block, no registered voice | 207,136 | 207,328 | +192 (+0.09 %) |
| tail hook, idle path (counted from the source) | 80 | 91 | +11 |

Per call, on the concrete core with the wrapper rig (`tests/support/dsp_voice_rig.py`; the device case has both reply records registered):

| call | HEAD | now | difference |
| --- | --- | --- | --- |
| stock type below 7 | 37 | 43 | +6 |
| unregistered type of 7 or more | 37 | 84 | +47 |
| registered, even voice (idle routine) | 201 | 340 | +139 |
| registered, odd voice | 192 | 294 | +102 |
| registered, even voice, without reply records (before the first block) | 201 | 272 | +71 |

One track on a run-time machine costs +241 instructions per block (+0.12 %); all 16 tracks +3,856 (+1.9 %, about 0.8 points of DSP load).

PCM [D]: `equiv-dyn-pluck` on dt2-1.17 (quick): the run-time machine equals the patched-in `digikit/pluck` sample for sample after the onset alignment (0 differ);
the onset shift between the two images was -64 samples at HEAD and is +128 now: the exact-equality flag was already false at HEAD (finding 45 recorded it true
before the link and the voice hook grew), the phase comes from the image sizes and the cost of the code before the note. `tests/patchkit/test_pluck.py` and
`test_dsp_voice.py` (including six new breadcrumb tests: each step, both records, the odd voice, the stock paths, no records) pass.

## 5. The rehearsal of rung C [D]

Scenario `rung-c` (`tests/digikit/scenarios/rung-c.toml`, `tools/patchkit/pk_digikit_rungc.py`, full tier, about 4 minutes): the rung C image on a card with a
sample filesystem (three WAV files) and a blank store, driven the way Em drives the device; `dkupload` and `dklog` run as command lines with `--emu --run-dir`
where Em types `--port Digitakt`.

- Image `out/patch/rungC-dt2-1.17.syx` (`core/runtime,core/digikit-menu,core/health,core/dynreg,core/dyndsp`): 1,885,088 bytes, sha256 `fb6caf044c633db18c803b0797731fcfec78a73e9b2e30a717e45c1ecd779cbf`;
  `mise run preflight` ends with `PREFLIGHT OK` (sections 3 and 7 differ from stock, 2, 4, 5 and 8 identical, checksum and HMAC hold, port limits). Module
  `out/patch/dyn-pluck-dt2.dkm`: 4,484 bytes, sha256 `e66c4660c028fda721e680f3b1aa0c3106133647f9016aa0531773c1d6e64b41`.
- First boot: no module, no safe mode, `CARD_CHECK state=blank`. `dkupload info`, `put` (`4484 bytes ... sha256 checked on the device`), `ls`. The upload wrote
  12 card sectors, all inside the store range 0x38000 to 0x3e000 (the overlay before and after the upload compared).
- MODULES lists the module; YES ticks it; DOWN; YES gives `CONFIRM RESTART` and the popup `UNSAVED CHANGES LOST`; NO; YES saves and restarts.
- The boot after: `BOOT: 1 OF 1 OK`, `MOD_LOAD result=1` then `MOD_DSP result=0`, **RAM KEPT: YES and `RAM_KEEP kept=1`** (RAM survives the menu restart in the emulator; on the device
  rung B only showed it across the update reboot), HEALTH CPU 21 %, DSP 31 %, heap free 26.7 MB.
- FUNC + SRC, seven presses of DOWN, YES: type 18 on track 1. Held for 13 s: dispatches 0 to 19,574, returns equal, heartbeats of both hooks up, the ten-second
  `DSP_DIAG` sets show `crumb=EPILOGUE_DONE voice=1 type=18 dispatches=15813 returns=15813`, no `DSP_STOPPED`.
- The note equals the patched-in pluck: 96,000 samples, **exact**, 0 differ. Observation: the same pluck is 12 % louder (x1.1236 for the whole note) on the card with samples than on the
  blank card (the track shows `SMP 1`: the project picks up a sample), for both images alike; the reference of the scenario uses the same card.
- The PNGs of the OLED (main screen, DIGIKIT menu, MODULES before and after the tick, the popup, ABOUT, HEALTH, the machine menu, the track) are in `out/runs/lanes/b-rungC/`.

The commands, the menu path and what a good and a halted log look like are in docs/PATCHING.md, "Rung C".

## 6. Open items

- **[O]** Whether the device's DMA keeps sending the reply records after the core stops inside a routine (section 2). The first halt at rung C answers it.
- **[O]** The cause of the halt in the 2026-10 report. The breadcrumb now says at which step it stops; the cause needs that run.
- **[O]** The runtime's no-block path (`crumb=NONE`) was not run; the Digitone II wrapper has no breadcrumbs.
- **[O]** The breadcrumbs cost +139 / +102 instructions per registered voice pair; no change was tried to shorten the stage writes.
