# 64. The send page of the track FX key (FX routing step 3, Digitakt II 1.17, 2026-10-25)

A digikit effect is a master insert or a send (finding 62). The amounts of a send are set like the stock sends: the track FX key (page 12: BR, OVER, SRR, SR.RT, DEL, REV, CHR, OD.RT) gets a **second page** with one knob for each send effect, for the current track.
The label is the effect's tab (`GAN`), and while the knob turns the title says `GAN Send=40` (0 to 127, default 0). Press FX again, or UP and DOWN, to change the page; the title shows `FX (2/2)`. The effect's own settings page stays in FUNC + FX (finding 61). No p-lock, no LFO, no MIDI CC
on these knobs. The page exists only when at least one loaded effect is routed send. This finding records the firmware facts, the machinery, the flow at run time and the proof. Marks: **[D]** measured or read once by the author (no second agent has checked the image bytes
yet, so nothing here is **[V]**), **[O]** open. Pieces: `modules/core/adapters/fxpages` (three hooks, a page row, two parameter rows), `modules/core/runtime/src/rs/src/fxsend.rs` (the route, the cells, the amounts), `dsp.rs` (the route and amount words of a row),
`registry.rs` (`write_send_param_row`), `[fx]` and `[limits] send_slots` of `devices/digitakt-ii.toml`, the scenario `REF:tests/digikit/scenarios/fx-route-dt2.toml` (`tools/patchkit/pk_digikit_fxroute.py`).

## Firmware facts **[D]**

| What | DT2 1.17 | Notes |
| --- | --- | --- |
| FX key page list | rodata `0x401f7864` = {12}, loaded by the main screen constructor `FUN_40066584` (hash `7160f06f...`) at `+0x6c0` (`move.l #list,D0`, 6 bytes; `0x40066c44`); the count is `moveq 1,D1` at `+0x6b6`, pushed at `+0x6ba` | the same builder that holds the F4 list at `+0x7f8`; the view is the generic `ParameterPageView` (page flipping is its shared `vfunc_2`, drawing its `vfunc_4`: no page-number special cases) |
| page 12 | cells `0xff` BR, `0x102` OVER, `0x100` SRR, `0x101` SR.RT, `0x44` DEL send, `0x45` REV send, `0x43` CHR send, `0x103` OD.RT; level slot `0xa` (Track Level) | the send page copies the name, the title and the level slot (so Track Level works on page 2 as on page 1) |
| the 16 Sound objects | **inline in the kit object**: track t at `*0x80004704 + 0x34 + 0x450 * t` (the Fx array follows them: `array_off 0x4534 = 0x34 + 16 * 0x450`) | the SRAM table at `0x8000470c` that `pk_harness.SOUND_PTRS` names holds 0 for track 0 on this image (tracks 1 to 15 are right): the layout above is what the runtime uses (`[fx] sound_off`, `sound_stride`) |
| Sound array | `Sound + 0x14`, 71 s16; the Sound setter accepts index < 0x47 | indices 69 and 70 are used by no stock parameter and are in neither the sound saver's table (69 entries) nor the loader's (77). **[C]** They do not reach the DSP: the per-track frame copy in `vector_191_handler` (`0x4002dd0c`, loop `0x4002eb2a..0x4002ecf4`) copies indices 25-34, 35-48, 49-61 and 64-68 (four `FUN_40136388` copies to frame +0xda, +0xfa, +0x116, +0x130); the 0x60-byte block per track is 42 mirror words and 6 words the type-6 slice path writes. Indices 62, 63, 69 and 70 are not sent **[D]** (disassembly and image bytes, out/runs/lanes/idx6970-study.log). An earlier line here read the block size as an index range 25 to 72 |
| sound loader | `FUN_400db3b2(sound, record)` clears the 71 halfwords at its start (the `memset` of `0x8e` bytes at `+0x66`) and returns through the common exit at `+0x450` (`movem.l (SP),...; lea 0x20(SP),SP`, 8 bytes, `0x400db802`) | every Load Kit, pattern kit and project load goes through it; the exit is the `cf.fxpages.sound` hook, which counts the loads |
| knob row | a parameter row is 15 words; owner (word 0) selects the object the firmware's set and get reach (owners up to `0x11` are the Sound array), the value class word (13) the display and the lock rule, word 6 the CC | the send row takes the owner of the stock send row `0x44` and the class of the kit's Delay rows (`template_percent` `0x75`, whose class flag makes it not lockable) and word 6 = -1 (no CC, no NRPN) |

## The machinery **[D]**

`core/fxpages` (`fxpages-dt2.s`): three more hooks and a page row (anchors `cf.fxpages.keylist` with `keystock`, `cf.fxpages.sound`, in `devices/ports/dt2-1.16.toml` and `dt2-1.17.toml`; the object is the Digitakt II's only).

- `fxp_acc` also answers the page number `page_first + fx_slots` (36) with the send row (`fxp_send_row`, `fxpages_info` word 40). The name, the title and the level slot are copied from the stock accessor's row 12 at each call (the accessor is called as a function with the page number
  pushed), the eight knob codes are the runtime's. No page of the stock table moves.
- `fxp_keylist` replaces the load of the FX key's list: while `fxp_send_count` is not 0 the count is 2 and the list is {12, 36} (rodata), else the stock list and count. The runtime sets the cell when it registers a send effect, before the main screen view is built (like the F4 list),
  so a changed route shows after a restart.
- `fxp_sound` counts the sound loads (`fxp_nsound`) and gives the loader's registers and result back untouched.
- `params.fs0` and `fs1` (`[[entry]]`, Digitakt II only): the two knob rows, written by the runtime. `fxpages_info` is version 2: words 39 to 48 are the send cells (`LIMIT_SEND_SLOTS`), the send row, the count cell, the first send parameter code, the Sound indices, the send template, `sound_off`, `sound_stride`
  and the load counter. A version 1 object (the Digitone II's) has none: the runtime sees no cell and routes nothing.

Device facts (`patchkit gen`, refused when inconsistent): `[limits] send_slots` (DT2 2, DN2 1 with no send page, so the image reports 0; at most `[limits.max] send_slots = 2` of `sdk/sdk.toml`), `[fx] track_page 12`, `sound_off 0x34`, `sound_stride 0x450`, `send_template 0x44`, `send_indices [69, 70]`.
CAPACITY has a new kind `SEND_SLOTS` (14): limit = the image's send cells (0 without the send page), used = loaded effects on the send route, largest 0 (not a module need: a module does not know its route).

## The flow at run time **[D]**

`fxsend::apply` (when `fxpage::apply` registers an effect row, at the boot): read `fx/route/<module id>` (one byte, 1 = send, 0 = master; **[C]** step 4: absent, or a route that the effect's record does not allow, gives the manifest's default `route`, which is master when the manifest says nothing; finding 65) and `fx/send/<module id>` (16 amounts) from the settings (step 2); a send effect takes the next free cell while cells are left (an effect beyond the limit
stays on the master); the cell's knob row is written (label `<tab> Send`, short name the tab, Sound index 69 or 70, class and owner as above) and its code goes into the send row; the route and the four amount words are published with the DSP row (the load sequence of `dsp.rs` writes them before the pc, so a send
effect never runs on the master; a freed row gets route and amounts 0 first). `fxsend::tick` (once per UI-loop pass):

1. After the loader counter changed and stayed still for 4 passes, and when the live kit object changed (a pattern's kit), the stored amounts are written back into `Sound[idx]` of the 16 tracks (`amount * 256`) *before* the arrays are compared; so the zeroes of a load are never taken as a knob turn.
   A 0 that appears in an array is also held back for 3 passes (the loader clears an array at its start and counts at its end).
2. Each array is read: a value that is not the last one written or seen is a knob turn: the amount `raw >> 8` (clamped to 127) is the effect's new amount for that track, `settings::put("fx/send/<id>")` (written to the card 2 s after the last change) and the DSP amount word
   (one word a pass through window 2, shared with the effect pages' values, one transfer at a time).

**The order of the FX view (step 4).** The page list of FUNC + FX (`fxpage::rebuild_list`) is DEL, REV, CHO, then the send effects, then the master effects, each group in row order; `fxsend::apply` builds the list again once the route of the row is known (`fxpage::relist`), so the order holds from the boot on and needs no hook (finding 65 has the proof).

Kits saved by stock firmware, or before the module existed, simply show the stored (global) amounts. A kit never holds an amount: the saver has no slot for the indices. Copying a track copies the Sound object, so the copy carries the amount and the next pass adopts it as a turn (a copied track
has the amount of its source; a feature). A page cell's randomise, copy and reload walk the page's cells and so touch the amounts (acceptable).

## Proof **[D]** (scenario `fx-route-dt2`, quick tier; results below are from the final run)

**[C]** Step 4 (finding 65): the scenario's effect is `test/dyn-fx-gain-both` (`route = "master"`, `routes = ["master", "send"]`; same gain, same page, tab GAN). `test/dyn-fx-gain` has no route fields and so allows the master only: a stored `send` for it is ignored. The first run of this scenario in the gate (it had no test file before step 4's `REF:tests/digikit/test_fxroute_dt2.py`) found that.

All 43 checks pass (345 s, 33 emulators; `pk_digikit_scen run fx-route-dt2 --image dt2-1.17`). Screenshots (3x): `out/runs/lanes/fxr-shots/` (`stock-fx1`, `send-page1`, `send-page2-track1`, `send-turning` = `GAN Send=40`, `send-page2-track1-40`, `send-page2-track2-0`, `send-turning-track2`, `limit-page2`).

- **No send effect, nothing changes.** On an image with the page machinery and no effect, and again with an effect on the master route: the FX key is pixel for pixel the key of an image without `core/fxpages` (frame hash equal), the count cell is 0, the PCM equals the PCM of the image without the module bit for bit
  (max error 0.0, onset shift 0). CAPACITY `SEND_SLOTS` is limit 2, used 0. The effect on the master has route and amount words 0.
- **Route from the settings.** With `fx/route/test/dyn-fx-gain` = 1 (put into settings copy A before the boot) row 0 of the chain has route 1 and four zero amount words, the count cell is 2, CAPACITY `SEND_SLOTS` is limit 2, used 1. The FX key shows `FX (1/2)` (the stock page, a different frame from the stock key only by the page count) and, after FX again (UP and DOWN
  too), `FX (2/2)` with the knob labelled `GAN` and the level slot; FX once more leads back to page 1.
- **The knob.** On track 1 turning knob A until the page shows 40: the title says `GAN Send=40`; only the halfword at `Sound + 0x9e` (index 69) moved, no other word of the 0x450-byte Sound object (so the record slot 0 of `updateMirror` is not touched on the encoder path). The DSP row's amount word 19 is `0x28` (track 1 = 40) and the 15
  other amounts are 0 (about 0.3 s later). Track 2 has its own amount: it reads 0, turning it to 100 sets byte 1 of word 19 (`0x6428`), track 1 stays 40; back on track 1 the page frame equals the earlier one.
- **The PCM gets the send.** The note on track 1 with the amount 40: the dry PCM times 1.0496, measured 0.04960 added against 0.5 x (40/127)^2 = 0.04960 (finding 62's gain law).
- **The settings.** About 2.6 s after the last change the record on the card holds `fx/send/test/dyn-fx-gain` = `28 64 00 ...` (40, 100, zeros) and `fx/route/...` = 1 (seq 4: writes at each pause of more than 2 s alternate between the two copies); the guest wrote only the selection sectors and the first sector of the two settings copies.
- **Kit loads.** Save Kit with 40 on track 1, track 2 set to 100, Load Kit: the loader hook counted the load, the runtime wrote the amounts back (32 restores in all: the boot's 16 and the load's 16), the knobs read 40 and 100 again, the DSP row's amount word read 0x6428 at every 2 ms sample over the 0.7 s of the load (never a 0), and
  the settings still hold 40 and 100 after it (the zeroes were not stored). Pattern A02's kit is another kit object (`0x4265882c`): the amounts are in it when it goes live and the DSP row did not change; back on A01 they are there too.
- **Limit.** Three effects (`test/dyn-fx-gain` under three names) all set to the send route on a device with `send_slots` 2: route words 1, 1, 0 (the third stays on the master, no cell), CAPACITY `SEND_SLOTS` 2 of 2; the page has two knobs (`limit-page2`), knob A sets the first effect's amount (20), knob B the second's (30), the third row has none.
- **No p-lock, no CC, nothing in Elektron files.** (a) The firmware's own lock test (`FUN_4006a3d0`: word 0 of the value-class object of the parameter's template; bit 0 or bit 3 set means no p-lock) read from the running image: the send knob rows have the template `0x75` (the Delay class; word `0x8`), the stock send row `0x44`'s class
  word is 0 (lockable), so the knob is not lockable by the rule the firmware applies to every row; the registry rows answer with their template's class object (`core/params` `pf_t2`). (b) A trig recorded on track 1 and held while the send knob is turned leaves pattern 1 as the trig alone left it (0 bytes differ in the project save), the knob value is not changed by the held turn,
  and no MIDI byte leaves the output while the knob turns. **[O]** the black-box side has no positive control: a held turn of a stock knob (the FILTER page's attack, a page-1 knob, the LEVEL knob) in the same harness left no lock data in a project save either, and playing the bar showed no lock in the DSP frame, so the panel script does not create p-locks here; the roundtrip scenario's
  "lock data" is the trig's own table bytes (the 14 bytes that differ from a project without a trig include the `0xff` to `0` bytes at offset 18948). Em should try a trig-held turn on the real send page (or a second agent with a working lock script). (c) A project saved with the amount 40 has the same kit and sound bytes as one saved without it (the FX key visited in both: a byte of the sound pool records the view, and it differs
  between a project that visited the FX key and one that did not): nothing of the amount is in an Elektron file.
- **Costs.** One UI-loop pass reads 16 halfwords for each cell (two reads of the live kit pointer and the loader counter) and costs about a send of one DSP word when an amount changes; nothing runs without a send effect (`fxsend::tick` returns when no cell is used). The route and amount words add 5 transfers to the load of an effect row (18 steps instead of 13).

## Open **[O]**

- **[C]** The Digitone II has the send page since step 6 (finding 67: the hooks `FUN_40060e72` `+0x67a` and the Sound loader `FUN_400dd34e` `+0x3d8`, Sound index 100, `send_slots` 1; the poke test of index 65 is recorded there).
- The route is read at the boot: a changed route (DIGIKIT > FX ROUTES, step 4, finding 65) shows after a restart, as the F4 pages do.
- `Sound::updateMirror` maps indices of 0x45 and up to record slot 0 on its notify path **[O]**: the encoder path writes only the SRAM mirror; the scenario shows that turning the knob moves no other word of the Sound object and that a project save has the same sound bytes with or without the amount.
  Other notify paths (MIDI CC in, the sound pool copy) are not exercised.
- A second agent checks the two new sites and the row class against the image bytes before the marks become **[V]**.
