# 61. Effect pages in the FX view (F4 steps 2 and 3, 2026-10-24)

A run-time effect (`docs/design/digikit-dkm.md`, finding 55) can have a page of its own in the FX view of the Digitakt II, with up to 7 knobs and MIX, built from records
and no effect-specific code. This finding records the firmware facts the page machinery rests on (DT2 1.17; the hooks also resolve by hash on 1.16), how the values travel,
and what was measured. Marks: **[D]** measured or read once by the author (no second agent has checked the image bytes yet, so nothing here is **[V]**), **[O]** open.
Pieces: `modules/core/adapters/fxpages` (patch time), `modules/core/runtime/src/rs/src/fxpage.rs` (run time), `[fx]` in `devices/digitakt-ii.toml`, the EFFECT record
(`packages/patchkit/src/register.rs`), the scenarios `REF:tests/digikit/scenarios/fx-pages-dt2.toml` (`tools/patchkit/pk_digikit_fxpage.py`) and `REF:tests/digikit/scenarios/fx-persist-dt2.toml`
(`REF:tools/patchkit/pk_digikit_fxpersist.py`, persistence, step 3).

## The page machinery **[D]**

| What | DT2 1.17 | Notes |
| --- | --- | --- |
| page descriptor table | RAM `0x4293ba94`, 33 rows of `0x2c` bytes | built by the static constructor `FUN_401be1be` before the boot hook; a row is the name and the title as char pointers, then nine parameter codes (eight knobs and the level slot; 0 = empty cell). Page 30 is a sentinel; no row is free |
| page accessor | `FUN_400c898a(page) -> row`, `moveq 0x20,D1` (bound) and `addi.l #0x4293ba94,D0` | the only reader of the table's address; 18 callers, all view code |
| FX view page list | rodata `0x401f7848` = {13, 14, 15} (DEL, REV, CHO) | the view builder `FUN_40066584` pushes the count (`moveq 3,D1` at `0x40066d6e`) and loads the list (`move.l #0x401f7848,D0` at `0x40066d7c`); the page vector copies them. A page whose cells are all empty is not skipped, so the list must hold exactly the loaded effects' pages. The title shows `(n/N)`, N the list size |
| tab label | `FUN_40074d88`, called by `FxPageView::vfunc_4` | draws a hard-coded string for the pages 14, 15 and 13 (tests at `0x40074dc0..0x40074dce`); every other number draws no tab. The view's `vfunc` at `+0x88` returns the page number |
| filter graph | `FxPageView::vfunc_4` at `0x40074ff8` (`cmp.l D0,D5; beq.w`, D5 = 15) | the box over the knobs 5 and 6 is drawn on every page except CHO |

`core/fxpages` therefore does not relocate the table (as the first design had it): `fxp_acc` replaces the first six bytes of the accessor and answers the page numbers 33 and up
(one per effect slot) with rows of its own table; everything else runs the stock code. That avoids copying 33 rows at boot and any assumption about when the constructor has run.
`fxp_list` replaces the load of the list (and the pushed count), `fxp_label` the third page test of the tab label (an effect page pushes its row's name and runs the label code's
own tail), `fxp_graph` the compare before the graph. The four sites are anchors by function hash (`cf_func`, `cf.fxpages.*`), resolved on 1.16 and 1.17; the stock list and
count are anchors too (`cf.fxpages.stock`, `stock_n`), so the runtime copies them in front of its pages. With no effect loaded the FX view is pixel for pixel the view of an
image without the module, and the PCM is the same bit for bit (scenario `fx-pages-dt2`, parts `step1` and `stock`).

Rows: the three effect pages are 33, 34 and 35. The name and the title are std::string character pointers (the runtime builds the representations in its state, as for machine rows).
The level slot (the ninth code) is the page's MIX knob.

## Parameter rows and the value array **[D]**

- A parameter row is 15 words (finding 02): owner, mirror index, min, max, default, flag, `(id << 16) | kind`, key, two template words, name, a template word, short, formatter, a
  template word. For the FX parameters the owner is 0x12 (chorus), 0x13 (reverb), 0x14 (delay) and **the mirror index is the index in the kit's effect value array**: delay 0x20 to
  0x27, reverb 0x29 to 0x2f, chorus 0x19 to 0x1f (the array has 71 s16 values; 25 to 69 are these and the other stock effect parameters). The display class is the formatter word:
  `0x400e1588` is a plain 0 to 127, `0x400e17ac` a bipolar -64 to +63 (Delay "Stereo Width", code 0x73); the delay owner's rows are the templates (`[fx] template_*`: 0x75, 0x73 and the level
  row 0x79 for MIX). The raw value is an s16 from 0 to 0x7f00; the display shows raw / 256.
- The indices 1 to 24 are used by no stock parameter (and 70): the kit's values there are the LFO default pattern at start, nothing but copies touches them, and the DSP does not get them
  (`out/runs/lanes/f4-static.log`, probes A1 and E1 to E3: all 25 poked, PCM identical). Effect row k owns `value_indices[8k .. 8k + 8]` of the device file: knob j (word p_j) and, as the
  eighth, MIX. A kit load (Load Kit) resets them to 0 before the loader runs; a pattern change gives a new kit object, all 0 **[D]** (step 2). Since step 3 the hooks below give them their
  stored values or the defaults at each init and load; see "Persistence".
- **The live kit is `*0x80004704`**, not the object of the firmware's kit accessor: `fw_kit_of(fw_project())` answered `0x44fb9d34` where `*0x80004704` was `0x426532b8`; the encoders
  edit the second, whose array (`+0x4534`) equals the SRAM mirror at `0x80003c42` index by index. Turning Delay Time changed index 32 of that array and nothing else **[D]**. So
  the device file has `[fx] kit_ptr` (the SRAM word) and `array_off`.
- The first knob row copies owner, flag, formatter and class from its template, takes the effect's index as mirror index and keeps the template's kind and key words (`write_fx_param_row`):
  the machine rows' `0x10 + slot` ids would collide with stock effect parameters. Several rows share one template, so one value-class object (`pf_t2` answers the template's); turning the GAIN
  knob changed the kit array at its index only (check in the scenario) **[D]**.
- An encoder detent moves the raw value by one step per slow event (about 0x1c) and by about 100 for a fast burst; the range 0 to 0x7f00 is therefore used at full resolution (a knob
  float is `min + (max - min) * raw / 0x7f00`, MIX is `raw / 0x7f00`: 0 and 0x7f00 are exactly 0.0 and 1.0).

## MIX is the adapter's **[D]**

(The row's words 18 to 22 are the send route, finding 62; on a send row MIX is the return level.)

`core/fx` row word 13 is the mix (a float, default 1.0 in the registry's field): the effect writes only the wet signal and the adapter makes `master = dry + mix * (wet - dry)`. Mix 1.0
copies the wet block (bit for bit), mix 0 leaves the master alone (the dry block bit for bit, the effect still runs and is checked), anything else blends per sample; in the fades the weight
is multiplied by the mix. Cost on the concrete core (`tests/patchkit/test_fx.py`): one effect steady 743 instructions per block at mix 1.0 (736 before: +7), 1142 at an intermediate mix
(+399), 605 at mix 0 (the copy is skipped); fades 1142. The empty chain (16), three free rows (31) and the all-off chain (178) are unchanged.

## Values to the DSP **[D]**

`fxpage::tick` runs once per UI-loop pass (`dkrt_tick`): when no load is in progress it reads the values of the loaded effects from the live kit's array and sends a changed one to the
effect's row, one word per transfer through window 2 of `core/dsp-link` (knob j to p_j as a float, MIX to word 13). The state `DKRT_FXP` shows it (`sent`, `errors`, `seeds`).
The scenario turns GAIN and MIX on the page and compares the PCM with the dry sound: the default 0.5 gives 0.5 x dry (0.50 ulp), GAIN 0.98 gives 0.98 x (1.97 ulps), MIX at 0.52
gives the linear blend (1.87 ulps against 1 + mix * (gain - 1)), MIX 0 the dry PCM bit for bit, MIX 127 the wet PCM again; deselecting the effect and restarting brings the three stock pages
(pixel for pixel) and the dry PCM (bit for bit) back. Screenshots (the fourth page: tab GAN, title `Test Gain (4/4)`, the GAIN knob, MIX on the level slot, `Gain=98` while turning) are
in the scenario's work folder; `DIGIKIT_SHOTS=DIR` copies them.

## Persistence (F4 step 3) **[D]**

The values of the effect rows are saved with the kit (Save Kit, a project, a pattern's kit) and come back with a load, only to the effect they belong to.

**Firmware facts (DT2 1.17; 1.16 has the same code, the anchors resolve by hash on both).**

| What | 1.17 | Notes |
| --- | --- | --- |
| kit loader / saver | `FUN_400db898(kit, file)` / `FUN_400dbdca(file, kit)` | the only users of the Fx functions below; the file is the kit storage container (magic `0xbacef00c` at +0x573a; no checksum in the loader) |
| Fx storage record | `fxSetupStorage_v0`, `0xa0` bytes at file `+0x458c`: u32 at +0 (must be 0 to load), 46 halfwords from +4, **28 spare halfwords from +0x60 to +0x98**, u32 at +0x98, 4 bytes cleared at +0x9c | the stock loader reads the halfwords by table position and ignores the spare; the saver writes the 46 halfwords and leaves the spare |
| Fx loader | `FUN_400daf54(array, record)` (array = kit + 0x4534): the table `0x40213998` (46 entries, position to array index, `moveq 0x5c` bound), then `move.l (0x98,A0),(0x8e,A1)` | when it refuses the record (u32 at +0 not 0) the kit loader calls the Fx initialiser instead |
| Fx saver | `FUN_400daf98(record, array)`: the table `0x40213880` (70 entries, array index to position; indices 0 to 24 all go to position 0, so position 0 holds Fx[24] and loads back into Fx[0]: a stock quirk, Fx[0] is read by nothing), then 4 bytes cleared, then `move.l (0x8e,A3),(0x98,A2)` | called about 390 times in the first minute of a boot (the firmware snapshots the kit continuously), plus once per Save Kit |
| Fx initialiser | `FUN_400e5698(array)`: 0x92 bytes zeroed, then the default of each of the 71 codes; called by the kit initialiser `FUN_400e5700` (new kit, the fresh kit that Load Kit fills, a project), by the `FxSetupCopy` constructor and by the kit loader on a refused record | |
| lock map | `FUN_400dae88` (bound `0x2e`) and `FUN_400daec2` (bound `0x46`) index **the same two tables** for track `0x10`, the FX track (callers: `FxSetup::vfunc_17`, `MidiPreset::vfunc_17`, `Sound::updateMirror`) | extending the tables would move this map and its bounds |

**Mechanism: three hooks, the stock tables stay as they are.** `core/fxpages` hooks the last instruction of the loader (`cf.fxpages.load`, offset 0x32 of the function), of the saver (`cf.fxpages.save`, 0x40)
and the common exit of the initialiser (`cf.fxpages.init`, 0x5c); each is a 6-byte (init: 8-byte) site found by function hash, and its routine does the displaced instruction and then the
copy, so every path that moves a kit goes through it. The alternatives were (a) widening the loader and saver tables: the loader count `0x5c + 2N <= 127` allows N <= 17 values without widening the immediate,
we need 27 halfwords; the saver table must stay 70 entries (the end test is `cmpa.l #0x40213998`, the address after the table); and the lock map above uses both tables with their own bounds,
so a longer table changes locks; (b) a runtime copy after the stock functions: the runtime has no point after the loader (a poll would see the zeroed array first). The hooks need no table, no widening, and
leave the lock map and the stock bytes untouched.

**Layout in the spare halfwords** (`[fx] store_off = 0x60`, `store_halfwords = 28` in `devices/digitakt-ii.toml`): halfword i of the 24 `value_indices` (effect row k, word j is i = 8k + j: knobs, MIX last) at
`0x60 + 2i`, then the effect ID of row k at `0x60 + 2 * (24 + k)` (`0x90`, `0x92`, `0x94`), halfword 27 (`0x96`) unused. An empty row has ID 0 and zero values. A record that stock firmware saved has all of it 0
(the saver does not touch the spare; in the emulator the buffer it writes into is zero).

**Effect ID.** One 16-bit number for each effect, from the packer only: `patchkit register::effect_id(module id, effect id)`: FNV-1a 32 of `<module id>:<effect id>` (UTF-8), the high and low halves xor-ed, 0 turned into
1. The EFFECT record's fourth word carries it (was reserved 0; runtime and packer refuse 0 or more than 0xffff). It follows the manifest ids, not the file's name (`pack --name`). A clash needs two effects of one image
with the same 16 bits (about 1 in 65535 per pair); the later load then takes the other's values (a wrong but valid state, never a crash).

**Rules** (the hooks' routines, `modules/core/adapters/fxpages/src/cf/fxpages-dt2.s`):
- *save*: values of the rows that have an effect, zero for an empty row, then the 3 IDs (the ID in `fxp_ids`, which the runtime writes from the record when it registers the effect). Fx[70] is not part of the record.
- *init*: each row with an effect gets the record's defaults (`fxp_defs`: the knobs' defaults as raw 0 to 0x7f00, MIX = `mix * 256`; a word without a knob 0), and the mark; so a kit that the firmware initialises (new kit, project load) starts with defaults, never zeros.
- *load* (after the stock loop): for each row with an effect, the stored values when the stored ID is the row's ID, else the defaults; a stored value above 0x7f00 is replaced by the default too. An empty row is left alone; a row with an effect sets the mark. Then
  the UI-loop poll sees final values only: no window in which it could send zeros to the DSP (step 2 left that gap, see the measurement).
- *the mark.* The kit objects of the other patterns (and of the boot) are made by the firmware before the runtime loads a module, so no hook ever touched them: a pattern change swaps the live kit pointer
  to such an object, whose Fx[1..24] hold the LFO default pattern (measured: `7000 100 4000 .. 4000` in the effect's row on pattern A02 **[D]**). Init and load therefore write a mark, `0x46a7`, into Fx[70]
  (`[fx] mark_index`: no stock parameter uses it, the DSP gets 25 to 69 only, step 0b poked it without effect, the saver table ends at 69 so it is not saved, the init zeroes it) when a row has an effect. Each
  UI-loop pass the runtime reads the mark of the live array first: without it the effect rows get their defaults and the mark, and nothing is read or sent that pass; with it the poll goes on. The runtime does the same
  for the live kit when it registers an effect (the boot's kit exists by then: `seeds` is 2 for two effects). A kit that was already marked and is copied keeps its mark; one that the firmware zeroes in place loses it.
- A kit saved by us loads on stock firmware: its loader reads the 46 table halfwords and the u32 at +0x98 only, never the spare bytes, and checks only `*(record) == 0` (the saver writes 0 there) and the container's magic **[D]** (static
  reading of `FUN_400daf54` and `FUN_400db898`; no checksum covers the record). Stock saves also do not write the spare, so a kit with effects saved on stock firmware afterwards has ID 0 and loads as defaults.
- Not kept: a kit saved while an effect's module is absent loses that effect's stored values (the saver writes ID 0 and zeros for an empty row); a second save then keeps the zeros.

**Measured (scenario `fx-persist-dt2`, 267 s, all checks pass; `fx-persist-restart-dt2` is the same with the two restarts of a user, 632 s) [D].**
- Spare bytes: after turning (A: GAIN `0x6400`, MIX `0x4200`; B: GAIN `0x2800`, MIX `0x2200`) and Save Kit the saver's record, and the record the firmware read back from the kit file at Load Kit, hold
  `6400 0100 4000 0000 0100 0000 0000 4200 | 2800 0300 4000 0000 0000 0000 0000 2200 | 0 x 8 | 4625 be85 0000 0000` from `+0x60` (the words of a page without a knob, 1 to 6, are whatever the array holds: the LFO
  default pattern; they come back only to a row with the same ID). Another kit saved while no effect had an ID holds 28 zero halfwords.
- Load Kit gives the saved 16 values back; the DSP rows then hold the floats of the saved values (p0 `0x3f7fffff`, mix `0x3f050a14`, ...); the PCM is the PCM from before the save **bit for bit** (max error 0.0); the
  pages (A and B) are pixel for pixel the pages from before the save; the stock Delay Time saved in the same kit comes back too.
- Zero window: with the rows read every 2 ms from the key press to 0.6 s (a kit load takes about 0.18 s; the DSP words follow one word per UI pass, so p0 and mix of a row change about 35 ms apart), no row word was ever 0 (a Load Kit
  of the saved kit, of the stock kit, and with the rows swapped), and 10 ms steps for 7 s over Load Project gave the same.
- A kit saved with no effect gives each effect its defaults (A `0x3200`, `0x7f00`; B `0x1900`, `0x4000`), not zeros and not the old values; the PCM after it is the default PCM (2.1 ulps off: the knob's raw `0x3200`
  is the float 0.49999997 and the record's default is 0.5 exactly).
- Rows swapped (A in row 1, B in row 0, restart or, in the quick scenario, the ID and default tables swapped): Load Kit gives each row the defaults of the effect now in it.
- Pattern A02 (another kit object, `0x4265882c`, made before the effect loaded) first held `7000 100 4000 .. 4000` in the effect's row; with the mark it gets the defaults (`3200 .. 7f00`, `1900 .. 4000`) the first pass
  it is live, and back on A01 the turned values are there.
- Save Project calls the saver 258 times (every kit of the project) and Load Project the loader 129 times; the values come back with it (A GAIN `0x5200`, MIX `0x3000`; B `0x1000`, `0x6000`) and the DSP rows follow.
- Unchanged: stock FX values save and load as before (the stock tables are untouched), and the scenario's write watch, flush check and card-write checks pass; the boot's kit was initialised before the effects loaded
  (`FUN_400e5698` ran once, the loader not at all), so the runtime seeded the two rows when it registered them.


## The Digitone II (F4 step 5, 1.12) **[D]**

The same machinery runs on the Digitone II 1.12: `core/fxpages` has an object per device (`fxpages-dn2.s`, one source file for each, the same code), the anchors are found by their own function hashes
(`devices/ports/dn2-1.12.toml`, `cf.fxpages.*`), and the device facts are `[fx]` of `devices/digitone-ii.toml`. Every site has the shape of the Digitakt II's, read from the 1.12 image (disassembly and `cfdb` hashes, **[D]**):

| What | DN2 1.12 | Digitakt II 1.17 for comparison |
| --- | --- | --- |
| page accessor | `FUN_400c25d8`, `moveq 0x24,D1` (bound), table `0x42433c00`, rows of `0x2c` bytes | `FUN_400c898a`, bound `0x20` |
| stock / effect pages | pages 0 to 36 are stock; the effect pages are **37, 38, 39** (`[fx] page_first = 37`) | 33, 34, 35 |
| FX view list | rodata `0x401e0448` = {17, 18, 19}; `moveq 3` at `0x40061616`, `move.l #list` at `0x40061624`, in the builder `0x40060e72` (offsets `0x7a4`, `0x7b2`) | {13, 14, 15}, offsets `0x7ea`, `0x7f8` |
| tab label | `FUN_4006f4a8`, tests for pages 0x12, 0x13, then 0x11 (the hook is the third, offset `0x40`; the common tail is at `+0x1e`) | pages 14, 15, 13 |
| filter graph | `FxPageView::vfunc_4` (`0x4006f5e4`) at offset `0x134`: `cmp.l D0,D5` (D5 = 0x13, CHO), `beq.w` to `+0x1c4`; the REV graph test (`moveq 0x12`, `bne`) skips an effect page by itself | D5 = 15 |
| kit loader | `FUN_400dcef0`, hook at `0x32` (`move.l (0x98,A0),(0xca,A1)`); table `0x401fd278`, 46 entries | `FUN_400daf54`, `(0x8e,A1)` |
| kit saver | `FUN_400dcf34`, hook at `0x40` (`move.l (0xca,A3),(0x98,A2)`); table `0x401fd160`, 70 entries | `FUN_400daf98` |
| array initialiser | `FUN_400e7140` (clears `0xce` bytes, the default of each of 101 codes), hook at the common exit `0x5c` | `FUN_400e5698` (`0x92`, 71 codes) |
| storage record | the same: u32 at +0, 46 halfwords from +4, spare from `0x60` (28 halfwords), u32 at `+0x98` | same |
| live kit | `*0x800052a0` (kit_t), the Fx array at `+0x48e4`, 101 halfwords and a u32 at `+0xca` (`[fx] tail_off`) | `*0x80004704`, `+0x4534`, 71 halfwords, u32 at `0x8e` |

Three literals of the Digitakt II source became device facts or anchor reads, so that the two sources differ only in their include: the stock bound (`FX_PAGE_FIRST - 1`), the first page of the stock list (the routine reads the first word of
the stock list anchor instead of a `moveq #13`) and the offset of the u32 behind the array (`[fx] tail_off`, `FX_TAIL_OFF`). The Digitakt II object is rebuilt from the edited source (the label hook's displaced instruction is a
`move.l` of the list's first word now) and its scenarios pass as before.

**Free value indices.** The Fx owners of the Digitone II (LFO 1 to 3, Chorus, Delay, Reverb, Ext-in, Master) use the array indices 1 to 69 and nothing else; 70 to 100 belong to no Fx parameter, the frame copy to the DSP takes 25 to 69 and the
stock saver table ends at 69 (static: `out/runs/lanes/f4-static.log`, parameter table of the image). So the effect rows take **70 to 93** (`value_indices`) and the mark is index **100**. Poke test in the emulator, in the scenario
`fx-pages-dn2` (part `stock`, no effect loaded): the 25 halfwords take distinct values (`0x1000 + 0x111 n`), keep them, no other index moves, and a note plays at the same level as the control note before it (within 0.2 dB; the two notes retrigger a sine that still sounds, so their captures are not bit for bit equal; the stock PCM against an image without `core/fxpages` is still bit for bit). **[C]** An earlier note here said the note start varies under host load; the capture start was fixed in b2e0edbd (`capture_pcm` syncs) and the bit-for-bit compare still fails on both devices: in `stock-ctl.wav` the sine of the stock note is still sounding at sample 0 (the sine lasts 2 s, a capture is 1 s), in `stock-poked.wav` the retrigger leaves a short silence at sample 88 to 129, and the first non-zero sample is no note start (onset shift 0 and error 0.065 on the Digitakt II, shift 208 on the Digitone II). A pattern change, a kit load and a
project load behave as on the Digitakt II (`fx-persist-dn2`; the first kit object of a boot is seeded by the runtime, another pattern's kit by the mark).

**Which kit object the encoders edit [D].** The object of `*0x800052a0` + `0x48e4`: turning GAIN in the scenario changes that array at the effect's own index only, and no other index moves except those that move on their own (LFO smoothing; the same check as on the
Digitakt II). The kit's own address is **not aligned**: the Digitone II allocates its kit objects at odd addresses (the sound arrays have the odd stride `0x48b`; pattern A02's kit was at `0x42112dfd`). The firmware's own loaders move
halfwords there, and so do the hooks (ColdFire allows it), but the runtime's first version refused a kit pointer that was not a multiple of 4 and never seeded such a kit: the runtime now reads and writes the array's halfwords a byte at a time and
accepts any alignment in DDR. The scenario's helpers (`u16`, `put16`) do the same.

**Measured (scenarios on dn2-1.12, quick tier) [D].**
- `fx-pages-dn2` (199 s, 35 checks): the three stock pages are pixel for pixel the pages of an image without `core/fxpages`, the stock PCM is equal bit for bit (and with the 25 free indices poked the sound has the control note's level); with `test/dyn-fx-gain` the FX view lists
  {17, 18, 19, 37}, the page has tab GAN, title `Test Gain (4/4)`, the GAIN knob and MIX on the level slot (index 70 and 77, defaults `0x3200` and `0x7f00`); GAIN to raw `0x6400` gives p0 = 0.98 in the row and 0.98 x dry PCM, an intermediate MIX
  the linear blend, MIX 0 the dry PCM bit for bit, MIX 127 the wet PCM; deselecting the effect and a restart bring the three pages (pixel for pixel) and the dry PCM back.
- `fx-persist-dn2` (265 s, 37 checks): the Digitakt II's checks, with the kit library at sectors `0x2040` to `0x2054` of the card: Save Kit and Load Kit restore the 16 values (kit array, DSP row floats, PCM bit for bit, pages pixel for pixel), the stock kit and
  swapped rows give the defaults, no row word was 0 at any time during a load, a project, pattern A02 (a kit object made before the effects, seeded by the runtime after 300 ms) gets the defaults and back on A01 the turned values are there.
- `fx-freqshift-dn2` (167 s, 25 checks): the routine is the Digitakt II's (`docs/findings/54-module-cookbook.md`): MIX 0 is the dry PCM bit for bit, SHIFT 0 keeps the sine (amplitude ratio 1.00001), +40 gives +96.79 Hz and -42 gives -124.72 Hz exactly as the routine's mapping says,
  the image is 70.1 dB down, Load Kit restores SHIFT and MIX.
- Screenshots (`out/runs/lanes/f4-shots/dn2-*.png`): the stock DEL page (title `Delay (1/4)` with the effect loaded, `(1/3)` without), REV and CHO (`Chorus (3/3)`); the fourth page with tab GAN, title `Test Gain (4/4)`, a GAIN knob, seven empty cells and MIX
  on the level slot; the same while turning (`Gain=100`, `Mix=0`); the frequency shifter's page (tab FSH, `Freq Shift=40` while turning).
- The Digitone II needs `core/dynfx` and `core/fxpages` in its image: they are in the `device` patch set, and since 2026-10-24 in `emulator` too (`core/fx` cost the emulator x2.38 DSP CPU there until the engine barred only the changed blocks, x1.01 now, finding 55). The scenarios still name them in their module list.

## Open **[O]**

- The init hook (`fxp_init`) has run only on the boot kit, before any effect was registered (`fxp_ninit` 1 at boot, no effect rows to fill). A kit initialised after the effects are loaded (the New Kit paths
  of the firmware: `FUN_400e5700`, ten callers, e.g. the project loader and the kit library) is covered by the loader hook and the mark (a kit without the mark gets the defaults), but the init hook's own copy of the
  defaults was not seen to run with an effect loaded. Load Project did not call it either (`fxp_ninit` stayed 1; the kits come from the loader).
- Project save and load were run inside a session; a power-up with a saved project (the boot's project load before or after the runtime's registration) was not: the emulator's ready snapshot starts from a
  fresh project. In the boot the effects registered after the first kit initialisation and before any load.
- A kit saved while an effect's module is absent loses that effect's values (ID 0 and zeros are written), and a second save keeps the loss.
- The Digitone II (step 5, section above): persistence was run in the emulator only (`fx-persist-dn2`); the init hook's own copy of the defaults has not been seen to run there either.
- A page whose effect fails to load its DSP part after the boot hook stays in the view (the view is built once); `remove` empties its cells.
- A second agent checks the sites against the image bytes before the marks become [V]. The Digitone II sites above are read from the image and exercised in the emulator by the scenarios.


## The send page (FX routing step 3) **[D]**

`core/fxpages` also has the send page of the track FX key (finding 64): page number `page_first + fx_slots` (36 on the Digitakt II) in `fxp_acc`, a hook on the FX key's list (`cf.fxpages.keylist`: the same builder `FUN_40066584`, `+0x6c0`), a hook on the sound loader's exit (`cf.fxpages.sound`)
and two more parameter rows (`params.fs0`, `fs1`). `fxpages_info` is version 2 (words 39 to 48). The F4 pages above are unchanged: with no send effect the FX key and the F4 view are the stock ones and the PCM is the same bit for bit.
