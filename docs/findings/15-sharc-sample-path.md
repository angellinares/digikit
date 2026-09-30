# The SHARC sample path: link port 0, the slot table, the trig and the first listen

How a sample on the +Drive reaches a SHARC voice on Digitakt II 1.16, and the
first time a real TRIG played a firmware-loaded sample through the emulated
DSP (2026-09-28). The ColdFire side of the transfer is in
[04](04-coldfire-dsp-link.md) ("Sample data crosses FlexBus to link port 0 on
1.16"), the file format and the boot project in
[14](14-plus-drive-format.md), and the emulator runs in
[07](07-emulator.md).

Marks: **[V]** checked here and by a second agent against the image bytes
(a separate verification lane, 2026-09-28, re-read each address with
`tools/sharc.py`, the raw loader block and the manuals); **[D]** executed or
read once; **[O]** open; **[C]** corrects an earlier claim.

Tools that reproduce this: `tools/sharc_lp0.py` (wire decode and LP0
feed), `tools/sharc_replay.py` (`--flexbus-log`, `--fields`,
`--armed-voice-wav`, `--record-voice`, `--voices-wav`),
`tools/sharc_capture_run.py` (`--press`, `--trig-hold`,
`--settle-smoother`, `--poke-slot`), `tests/test_sharc_lp0.py`.

## Link port 0 receives the sample pages **[V]**

- Loader block 35 (target `0x28269250`, not part of the main program) holds
  a peripheral-instance table like the SPI/SPORT tables in finding 04. At DM
  `0x269468`: LP0 `{0x30FFE000, 0x30FFF000, 81, 82, 168, 0}`; at `0x269480`:
  LP1 `{0x30FFE100, 0x30FFF080, 83, 84, 169, 0}`. The ADSP-2156x hardware
  reference (Table A-64/A-65, Table 14-2) gives `0x30FFE000` = LP0_CTL and
  SEC 81/82/168 = LP0_DMA/LP0_STAT/LP0_DMA_ERR.
- No instruction references `0x269480` or `0x30FFE000`-`0x30FFE1FF`: LP1 is
  unused, and LP0 is reached only through the table.
- Boot chain (single callers): `0x1c13e6` -> `FUN_1c7ff9` -> `FUN_1c7ec5`
  -> `FUN_1c8895`. `FUN_1c8895` opens LP0 and installs, through the shared
  install routine `0xb8afa2`, SEC 81 -> `0x1c8705` (DMA done), 82 ->
  `0x1c84e6`, 168 -> `0x1c880d`. The two status/error handlers have no
  callers in the edge table; they are interrupt targets.
- `FUN_1c7ec5` builds the receive descriptor at DM `0x268220`: `+4`
  ADDRSTART `0x268240`, `+8` CFG `0x100000`, `+0xC` XCNT `0x401`, `+0x10`
  XMOD 4. One transfer is 0x401 words: a tag word and 1024 payload words.
  `FUN_1c83ff` registers the callback `0x1c7e1f` (`I12 = 0x1c7e1f` at
  sw `0x1c840f`); `FUN_1c834a` submits the descriptor.
- On completion the DMA ISR `0x1c8705` reaches `FUN_1c85ba`, which calls the
  callback with `R8 = 4` (sw `0x1c86a8`-`0x1c86b0`). **[D]** (P0
  2026-09-29: `R8 = 0x4` at `0x1c86aa` in both decoders; the path from the
  ISR, `0x1c8735` conditional jump to `0x1c87cd` and `CALL 0x1c85ba` at
  `0x1c87f9`, is from sharcdb only.)
- *P0 2026-09-29.* The LP0 and LP1 records occur once each in the raw
  `section_7_BLOB.bin` (little-endian, 24 bytes apart, file offsets 21080 and
  21104); sharcdb has literals for `0x269468` (in `FUN_1c8895`, from
  `0x1c88c0`) and none for `0x269480` or `0x30FFE000`-`0x30FFE1FF`. The
  HWR text (`out/refs/adsp-2156x-hwr/all.txt`) lists `0x30FFE000` as
  LP0_CTL and SEC sources 81/82/168 as LP0_DMA/LP0_STAT/LP0_DMA_ERR.

Callback `FUN_1c7e1f` **[V]**:

| tag = DM(`0x268240`) | action |
|---|---|
| `0xFFFFFFFF` | slot header, words `0x268244`.. = slot, startL, startR, rate, len: `FUN_1c400f(startL, len)`, `FUN_1c400f(startR, len)`, stereo = (startL != startR), `FUN_1c3fe5(slot, ...)` |
| signed `< 0x19200` | page: `FUN_1c403c(tag, 0x268244)` |
| other | ignored |

In every case it rewrites the descriptor and re-submits it (`FUN_1c834a`).

**Page mapping [V].** `FUN_1c403c` copies the 1024 words to
`0x8422B9C8 + (page << 12)`. So SHARC byte address = `0x8422B9C8 +
(ColdFire sample-memory offset - 0x400)`. Pages `0..0x191FF` fit (top
`0x9D42B9C8`), just above the ColdFire's `0x19000000` allocator limit.
`0x8045A6C8`, every voice's word 0 after init, is the 3,316-byte curve
table (finding 06), not sample memory.

**Wire packing [V] (P0 2026-09-29; was [D]).** The link port latches a byte on the clock's falling
edge and packs the first byte of a word into the low bits (HWR 14-4/14-5;
the extracted text of Figure 14-3 is ambiguous). The firmware's own range
checks confirm it: the ColdFire sends tag and header words least significant
byte first, and with MSB-first packing page 1 would read `0x01000000` and
fail the `< 0x19200` check. P0 evidence: (1) the HWR text says the receiver
"uses the falling edge of LP_CLK ... to latch the byte" and, for LP_TX,
"The least significant byte is transmitted first"; (2) the ColdFire side
sends LSB first (finding 04, "The wire", re-read in two passes), with the
clock bit 7 high then low around each byte; (3) the `0x19200` bound is in
both the callback (`R2 = 0x19200`, `comp`, `JUMP IF LT` at
`0x1c7e3c`-`0x1c7e40`) and `FUN_1c403c` (`compu`, `JUMP IF GE` at
`0x1c4045`-`0x1c404b`, the same fields in the SLEIGH dump); (4) parsing
`flexbus-drive3.raw` as little-endian words gives page tags 0..321 and
headers `{slot, 0x20, 0xa1020, 48000, 0xa0b70}`.

## The slot table **[V]**

`FUN_1c3fe5` (slot `< 0x401`) writes a 0x14-byte record at DM
`0x257810 + slot * 0x14`: `+0` startL, `+4` startR (bytes, relative to
`0x8422B9C8`), `+8` len (bytes per channel), `+12` rate (Hz), `+16` stereo
(byte). Slots 0..0x400; 0x400 is the preview slot (`FUN_1c2ac9` reads it).

`FUN_1c3f78(slot)` is the reader. For slot `< 0x401` it branches to
`0x1c3fa8`: record at `0x257810 + slot*0x14`, startL and startR plus
`0x8422b9c8` (`0x1c3fc6`, `0x1c3fcb`), len `>> 1` = frames (`R1 = M7`,
`lshift` at `0x1c3fc1`), rate, stereo. For slot `>= 0x401` it copies the
default record at `0x25C914` (`{0x8422b9c8, 0x8422b9c8, 0, 48000, 0}`).
(The verification lane first read the branch the wrong way round; the
listing at `0x1c3f89`/`0x1c3fa8` settles it.)

*P0 2026-09-29, re-checked in sharcdb and in the Ghidra SLEIGH dump
(`out/ghidra/sharc-dt2-1.16`, an older language, decoded independently):*
the writer tests `compu(R4, 0x401)` and exits on GE (`0x1c3fea`-
`0x1c3ff1`), forms `I4 = slot * 0x14 + 0x257810` (`0x1c3ff8`-`0x1c3ffe`)
and stores R8 at +0, R12 at +4 (pre-modify M6), R15 as a byte at +16
(Type 4b `(bw)`), R1 at +8; the reader tests `compu(R4, 0x401)` and jumps
to `0x1c3fa8` on LT, otherwise copies five words from `I4 = 0x25c914`. The
page copy `FUN_1c403c` forms `I4 = (page << 12) + 0x8422b9c8` (`lshift` 12
at `0x1c404e`, `modify(I4, 0x8422b9c8)` at `0x1c4053`). The slot header
branch of the callback reads slot, startL, startR, rate and len from
`0x268244`-`0x268254`, calls `FUN_1c400f` for startL and startR, and sets
stereo from `comp(startL, startR)` (`0x1c7e83`-`0x1c7ea8`). The hat values
below follow: `0x8422b9c8 + 0x20 = 0x8422b9e8`, `+ 0xa1020 = 0x842cc9e8`,
`0xa0b70 >> 1 = 0x505b8`.

For hat fed as slot 7: table `{0x20, 0xa1020, 0xa0b70, 48000, 1}`, reader
`{0x8422b9e8, 0x842cc9e8, 0x505b8, 48000, 1}` **[D, executed]**.
`FUN_1c400f` copies the first words of a channel to just past its end (an
interpolation guard) **[D]**; its exact span **[O]**.

## From the frame to a voice's sample pointer **[V]**

- Track t owns voices 2t and 2t+1; there is no dynamic allocation
  (`FUN_1c2b24` loop at `0x1c2c94`, 16 passes, `FUN_1c24e9` called for
  `R15` and `R15 + 1`). Voice records are at `0x2412cc + v*0x1d8`
  (`0x1c149b`-`0x1c14ab`); per-voice parameter records at
  `0x2506ec + v*0xdc` **[D]**.
- The sample-load block `0x1c3329` (inside `FUN_1c3289`): slot = signed
  int16 loaded at sw `0x1c3338` from `I4 + 6`, `I4` = landing ring + `0xDA`
  + track*0x60, i.e. TX offset `0xE0 + 0x60t`. `FUN_1c3f78(slot)`, then
  `FUN_1c7442` -> `FUN_1c4e70(voice, ptr, frames, 0, rate)` for both voices:
  voice 2t the L address, 2t+1 the R address (`IF SZ R8 = DM(I6-12)` at
  `0x1c336b`).
- `FUN_1c4e70`: if frames `< 0x8D` (141) it clears word 0 (sw `0x1c4e86`)
  and ACTIVE `+0x1B8` (`0x1c4e8a`, pre-modify). Otherwise `0x1c4e91`
  `DM(I4, M4) = R8` stores the pointer at word 0 (post-modify, then `I4 +=
  0x65`) and fills rate `+0x184`, frames `+0x188`, `+0x18C` = 0,
  `+0x190`..`+0x19C` = 0, and the frames as a Q31 pair at `+0x1A0/+0x1A4`
  **[D for the field list]**. (The verification lane read `0x1c4e91` as a
  store to `I4 + 0x65`; the instruction is post-modify.)
- **[C]** finding 06 "The firmware writer of word +0 for a playing voice is
  not yet found": it is `FUN_1c4e70` at `0x1c4e91`, fed from the slot
  table.
- **[C]** The "generic per-frame clear" of earlier lanes (K1/N2,
  `REARM_CLEAR_PC 0x1c4e86`, `REARM_ACTIVE_CLEAR_PC 0x1c4e8a`) is
  `FUN_1c4e70`'s reject path for an empty slot: every capture carried slot
  0 and slot 0's record is empty.

What this replaces in the old `--inject-real-sample` hand step: the
pointer, length and rate pokes come from `FUN_1c4e70`, and ACTIVE is no
longer cleared once the slot is filled. The loop, start and end fields are
initialised by `FUN_1c4e70` and then rewritten by the per-frame setters
(`FUN_1c4914`/`4a31`/`4afe`/`4bf9`/`4d88`, finding 06) **[D]**; step and
phase come from the setters and the render **[D]**; `+0x17C/D/E`, `+0x180`,
`+0x1BB`, `+0x1BC` are not written by this path **[O]**.

## The trig: mask, latch and arm **[V]**

Frame fields (TX payload, big-endian halfwords, written by
`vector_191_handler` `0x4002dd0c`; finding 04 has the ColdFire side):
`0x22` = trig mask (bit t = track t note-on this frame), `0x24` = release
mask, `0xE0 + 0x60t` = sample slot, `0x94 + 2t` = machine type. The
SHARC reads them in `FUN_1c24e9` (per track, from render_frame's working
copy at `0x2558dc`) and directly from the landing ring.

Per-voice trig bytes in the engine object E = `0x2412c8`: A pending at
E+`0xdde4`+v (`0x24f0ac`), B latched at E+`0xde04`+v, C released at
E+`0xde24`+v, D at E+`0xde44`+v (from hw `0x73a` via `FUN_1c6048`), F/G at
E+`0xde64`/`0xde84`+v (hw `0x26`/`0x28`) **[D for C, D, F, G]**.

Sequence, frame N = the frame whose hw `0x22` bit t is set:

1. Frame N, voice loop: sw `0x1c2d1a` tests the landing ring's hw `0x22`
   bit and jumps to the trig block `0x1c33bc`; `0x1c33ce` sets A[v] = 1.
2. Frame N, end of `FUN_1c642a` (`0x1c7182`-`0x1c71a6`, SIMD, 16 passes of
   two voices): B = A (load `0x1c71a0`, byte store `0x1c71a3`), then A, C,
   F, G cleared.
3. Frame N+1: the working copy now holds frame N, so the flag table
   `0x2522ac + 2v` (written from hw `0x22` bit t by `FUN_1c24e9` at
   `0x1c258b`) is set while the landing bit is clear. sw `0x1c2d22` loads the
   flag and `0x1c2d27` jumps to the sample-load block `0x1c3329` (above).
   This is the only per-track path to `FUN_1c7442`/`FUN_1c4e70` in the
   frame (other callers: the init `FUN_1c15e3` and the preview
   `FUN_1c2ac9`).
4. Frame N+1, `FUN_1c642a` per-voice loop: GUARD_B (sw `0x1c6545`, byte B[v])
   -> `0x1c657b` -> `FUN_1c4eaf`: ACTIVE `+0x1b8` = 1, seed pending.
5. On release, `FUN_1c6056` (via `0x2522ec + 2v`, sw `0x1c2d2a`) clears A/B
   and sets C **[D, executed]**.
6. Rendering, `FUN_1c642a` second loop (sw `0x1c6ae8`): parameter record
   `+0x4c` (machine type) 2 goes to `FUN_1c5576`, all others to the sample
   renderer `FUN_1c4ecf` **[V for the dispatch; D for the +0x4c source]**
   (P0 2026-09-29, was [D]: `R2 = btgl(R2, bit=1)` at `0x1c6ae8`, `JUMP IF
   NOT SZ 0x1c6afd` at `0x1c6aeb`, `CALL FUN_1c5576` at `0x1c6af1`, `CALL
   FUN_1c4ecf` at `0x1c6b00`; sharcdb and the Ghidra SLEIGH dump agree).

*P0 2026-09-29.* Steps 1-4 re-read in sharcdb and the SLEIGH dump: `btst(R7,
R5)` at `0x1c2d1a` and `JUMP IF NOT SZ 0x1c33bc` at `0x1c2d1d`; `I4 =
0x24f0ac` at `0x1c33c9` and the store of M14 (1) at `0x1c33ce`; the byte
load, byte store and M13 clear at `0x1c71a0`/`0x1c71a3`/`0x1c71a6` (Type
3d); `I1 = 0x2522ec` at `0x1c2588` and the store at `0x1c258b`; the flag
load at `0x1c2d22` and `JUMP IF NOT SV 0x1c3329` at `0x1c2d27`; the B load
at `0x1c6545` and `JUMP IF NOT SV 0x1c657b` at `0x1c6549`; `FUN_1c4eaf`
forms `I4 = voice + 0x1ba` and stores M14 at `0x1c4eb9` and pre-modified
by M7 at `0x1c4ebb`. The byte offset of those two stores (the "+0x1b8"
above) depends on the core's Type 3 scaling rules and was not re-derived
by hand. `0x1c2d1a`'s R7 source (the hw `0x22` word) was not traced here.
The copy gate: sharcdb's `ptr` table has one store to `0x2567dc`
(`0x1c2c8b`) and one load (`0x1c2c41`), and the only literal is at
`0x1c2c88`.

**The copy gate [V].** The only SHARC store to DM `0x2567dc` is the clear
at sw `0x1c2c8b`. `tools/sharc_harness.py`'s `drive_dma_completion()` used
to re-arm it every frame (a stand-in from lane G1), which made render_frame
copy the landing ring at the start of every frame; the flag table and the
trig test then read the same frame and step 3 was dead code. It now
defaults to `rearm_copy_gate=False` (run_init leaves it 1, so the first
frame still copies). **[D]** that the real firmware never re-arms it; the
real setter, if any, **[O]**.

Corrections to finding 06 (Lane J1, lanes K1/N2):

- **[C]** "Per frame, `FUN_1c2b24`'s tail loop runs for tracks in service"
  and "voice 5 word 0 = `0x842cc9e8` every frame": artifacts of the
  misaddressed Type 4b stores (next section), which wrote floats into the
  16-bit flag table at `0x2522ac`. The load block runs only in the frame
  after a trig.
- **[C]** Lane J1's "voice 4 armed at frame 304 for one silent frame": the
  arm came from `FUN_1c642a` reading per-track record fields the
  misaddressed stores had corrupted. With all core fixes (below) the arm in
  `dt2-1.16-running-trig.dt2cap` is real and follows the sequence above
  (trig 303, load and arm 304, release seen 305); the voice stays ACTIVE,
  and it is silent because slot 0 is empty and track 2 is machine type 2
  (rendered by `FUN_1c5576`, which the replay's master-mix hand step does
  not inject) **[D]**.

## Core fixes that the path needed

Each was a real decode or semantics error in `tools/sharc_core/`, found on
this path.

- **Type 4b has no (lw) form [V][C].** PRM p.13-32 (Type 4b BH/BHSE encode
  tables) and Table 13-12: (l,x,w) = (1,1,1) is the plain normal-word
  access. Commit `49cdbc5` had applied Type 3b's ACCESS table, which made
  (1,1,1) a 64-bit access (every such load Unknown, every store dropped).
  sw `0x1c336b` (I6, data6 -12, R8, cond SZ) is a 32-bit load of the L
  address stored at I6-12. 156 Type 4b (1,1,1) sites in dt2-1.16, 152 in
  dn2-1.11 **[D]**. `TYPE4B_ACCESS_WIDTHS` in `sharc_core/encoding.py`.
- **(lw) offsets scale like a normal word [V].** PRM p.6-9 ("scaling is by
  the size of the access (except in the case of (lw))") and Table 6-2:
  byte space `mod << 2`. The core scaled by 8.
- **Plain MODIFY does not scale [V][C].** PRM p.6-10: "Ia = MODIFY(Ib,Mc)
  ... Does not scale the modifier, whatever the address space"; only (sw)
  and (nw) scale. The core scaled Type 7b and blank 7a by 4. Symptom: `0xb82c56`
  read 16 bytes too far and overwrote the caller's frame.
- **ShiftImm 0x19 is OR FDEP, not BITEXT (NU) [V][C].** PGR Table 12-11
  (six-bit column): or-fdep = 0x19, or-fdep(se) = 0x1B, BITEXT(NU) = 0x16,
  BITDEP = 0x1D. PRM Table 17-9's six-bit column is off by one row for these
  three, and `compute_table.json` had copied it. The sites carry sensible
  fdep fields: `0xb88fa4` `R0 = R0 OR FDEP R12 BY 31:1` (sign bit),
  `0xb88fe6` `R0 OR FDEP R2 BY 23:8` (float exponent), in the
  integer-to-float helpers. **[C]** finding 06's "`BITLEN12` = 95/384/192/
  256/535" sightings (lanes F2/G2/H2) and the open "BITEXT(NU) with
  BITLEN12 > 32" were this decode error. `FRAME_PATCH_TABLE[0x1C4965]`
  (forcing F6/F4 = 1.0f in `FUN_1c4914`) existed only because of it and is
  removed; `FRAME_PATCH_TABLE` is now empty.
- *P0 2026-09-29, the four fixes above re-checked against the manual text
  (`out/refs/`):* the Type 4b BH and BHSE encode tables list (1,1,1) with no
  suffix; PRM p.6-9 has "scaling is by the size of the access (except in the
  case of (lw))" and p.6-10 "Does not scale the modifier, whatever the
  address space"; PGR Table 12-11 gives OR FDEP `0110 0100`, OR FDEP (SE)
  `0110 1100`, BITEXT (NU) `0101 1000` and BITDEP `0111 0100`, whose upper
  six bits are 0x19, 0x1B, 0x16 and 0x1D; PRM Table 17-9 pairs six-bit
  `011001` with bitext (nu) while its own eight-bit column pairs `01100100`
  with or fdep, the one-row shift. The two OR FDEP sites were decoded by
  hand from the raw compute fields (sharcdb `shiftimm` 0x195f0c with DATAEX
  0 and 0x191702 with DATAEX 2; the SLEIGH dump shows the same opcode 0x19
  and data): `R0 = R0 OR FDEP R12 BY 31:1` and `R0 = R0 OR FDEP R2 BY 23:8`.
  `0x1c336b` is (l,x,w) = (1,1,1) in the SLEIGH fields. The 156/152 site
  counts stay **[D]**.
- **SIMD companions [D]:** Type 4a (normal word, PRM p.212 Table 6-10),
  Type 3a (normal word), Type 3d (byte, PRM p.13-20; `FUN_1c642a`'s latch
  copy needs it, or only even voices are ever latched), Type 4b/4d and 3b
  sub-word (PRM p.7-5: +1 byte / +2 bytes; `FUN_1c2b24`'s copy of the 1024
  frame shorts at `0x1c2c7c`/`0x1c2c7f` needs it). Stores of uncomplemented
  UREGs in SIMD write both locations (PRM Table 4-22); `FUN_1c642a`'s end
  loop clears A/C/F/G with `DM(I4, M3) = M13`. PEy compute in the move forms
  3a, 4a, 1a, 5a (PRM p.101); the block handler's ring-A pass (sw
  `0x1c7593`-`0x1c759f`) needs it.
- Golden hashes: only `trace_voice` changed (`4054c194...` ->
  `262d1e39...`), back to its value before `49cdbc5`. `FRAME_MILESTONE`
  (synthetic frame) 96,044 -> 213,247 instructions, same halt.
  `tools/sharc_widthaudit.py --root frame`: 0 mismatches over 46,128
  events.
- Still open **[O]**: Type 16 immediate stores in SIMD (should duplicate);
  per-PE conditions for conditional 3a/5a in SIMD; `sharcdb`
  `mem_access.width` for the Type 4b and Type 3d (0,0,0) rows (needs a
  `DB_VERSION` bump to 14 and a rebuild; the database still prints
  (1,1,1) Type 4b as "long"). **[C] (P0 2026-09-29)** for Type 4b:
  `DB_VERSION` is 14, the database is current, and `0x1c336b` now reads
  `normal-word` in `mem_access`; the Type 3d (0,0,0) rows were not
  re-checked.

## First listen: TRIG 1 plays hat through the SHARC **[D]**

Source data, all from the firmware: `tools/plusdrive.py` built an image
with hat.wav as a native sample and the built-in project pointing every
slot at it (finding 14); a cold boot loaded it and the FlexBus stream was
logged (finding 07); a capture of a panel TRIG 1 (track 0: ONESHOT, slot 7)
supplied the frames.

Capture (30 s wall, 1188 frames; TRIG at frame 77, never released):

    DT2_SYX=Digitakt_II_OS1.16.syx uv run python tools/sharc_capture_run.py \
      snapshots/dt2-1.16-drive3/loaded.snap \
      --out out/captures/drive3/dt2-1.16-drive3-trig1-settled.dt2cap \
      --kind note --trig-track 0 --press NO@1000000 --press NO@2000000 \
      --trig-at 4000000 --trig-hold 1000000000 --settle-smoother \
      --instrs 62000000 --unblock --card-image out/plusdrive/native/dt2.img

Replay (about 80 min, about 4.5 s per 32-sample frame, peak RSS about
25 GB):

    uv run python tools/sharc_replay.py dt2-1.16 \
      out/captures/drive3/dt2-1.16-drive3-trig1-settled.dt2cap \
      --armed-voice-wav --voice 0 --start-frame 74 --frames 1000 --extra-frames 0 \
      --record-voice 0 --record-voice 1 --flexbus-log out/captures/drive3/flexbus-drive3.raw \
      --report long.json --out long-ringa.wav --voices-wav long-voices.wav

`flexbus-drive3.raw` (not committed) is the byte log of the cold-boot run that saved `loaded.snap`
(`tools/guirun.py --flexbus-log`; the file must end in `.raw`, or
`tools/sharc_lp0.py` parses it as 6-byte records): 1940 transfers, 322
pages, 1024 + 297 reset headers and 297 real headers.

Arm events, from the firmware's own PCs, no voice pokes: frame 77 trig
voices 0 and 1; frame 78 load, `set_sample` voice 0 ptr `0x8422b9e8` and
voice 1 ptr `0x842cc9e8` (rate 48000), arm both. Word 0 changes from
`0x8045a6c8` at `0x1c4e91`; ACTIVE is set at `0x1c4ebb` and held for all
996 remaining frames.

Comparison with the native 48 kHz file (fit `voice[k] = g * hat(r*(k -
k0))`, linear interpolation of the reference):

| voice | vs | correlation | ratio | semitones | gain |
|---|---|---|---|---|---|
| 0 | hat L | 0.9877 | 0.84016 | -3.015 | 0.452 (-6.9 dB) |
| 1 | hat R | 0.9876 | 0.84016 | -3.015 | 0.454 (-6.9 dB) |

- L and R are not swapped (voice 0 vs hat R: -0.01).
- Pitch: the frame's TUNE word is 15614 = `0x3cfe` = 60.99 on the 0..127
  scale, centre 64, so -3.01 semitones. The SHARC's pitch formula was not
  traced; the match is numeric only.
- Gain: flat at 0.45 through the first 60 ms. Velocity 100/127 gives
  -2.1 dB; the other -4.8 dB is not explained **[O]**.
- The voice starts at the head of the sample, in the arm frame.
- Residual SNR 16 dB, mostly from the quiet tail (linear-interpolation
  reference against the firmware's 6-tap polyphase interpolator, and two
  missing frames).
- Ring A (`--out`, via the master-mix hand step) holds -voice 0 on both
  channels (hat L, inverted); `sharc_harness.py`'s "(-L, +R)" note does not
  fully match **[O, minor]**.
- Frames 656 and 666 stopped early at `0x1c1cf9`, an unmodelled MMR
  `0x30000` load in `FUN_1c18a6`; their voice output is zero-filled **[O]**.

WAVs (not committed): `out/listen/hat-trig1-voices.wav` (voice 0/1 render
output as L/R, no injection) and `out/listen/hat-trig1.wav` (ring A).

### Hand steps still in the path

1. ColdFire `--settle-smoother`: copies the smoother's input over its output
   every tick, a stand-in for the fractional EMAC bug (finding 07). It
   writes the firmware's own values. Goes away once the EMAC fix is
   installed and the capture is remade. *2026-09-29: the fix is installed
   and `dt2-1.16-drive3-trig1-emac.dt2cap` was captured with it (used by
   the live check below); the long replay above has not been rerun on it.*
2. ColdFire vector 191 is forced every 50k instructions and the frame-build
   gate is opened (the existing capture method, finding 04).
3. Two NO presses close two modal windows (real panel input; finding 03).
4. LP0: `sharc_lp0.deliver` models the DMA engine, the ISR dispatch and the
   re-submit; the bytes are the real FlexBus capture.
5. Replay: `--start-frame 74` skips frames 0-73; `drive_dma_completion` is a
   host model.
6. Ring A WAV only: `inject_master_mix` of voice 0 (the per-track mix into
   the master bus is not modelled). The voices WAV has no injection.

The earlier `--inject-real-sample`, `setup_voice` pokes and
`--frame-override` are not used.

## Live: a GUI pad press through the native core **[D]**

`emu.gui --live-audio` (`emu/livesharc.py`, `native/live`) runs this path
live: `emu.dspi2.Dspi2Link` hands every TX frame to the native SHARC core,
which renders it in real time. Checked by execution on 2026-09-29 with
`tools/live_gui_check.py` (emu.gui's own worker, no window, no output
device) from a `tools/dt2gui.py` ready snapshot, TRIG 1 pressed:

- **Byte order.** The Dspi2Link wire bytes (the low halves of the PUSHR
  entries) equal the driver's own TX buffer, the capture format, over the
  whole 0x802-byte payload in all 39 frames of a 12M-instruction run: the
  ColdFire's big-endian halfwords. They need the same 16-bit swap as the
  capture packs before the SHARC's receive ring; `native/live`'s
  `LiveSource` does it, the only place. A cargo test renders the capture's
  frames, un-swapped to wire order, through that path and gets the capture
  path's output bit for bit. **[D]**
- **The trig frame.** TRIG 1 gives one frame with 0x22 = 0x26 = 0x0001, and
  the release one frame with 0x24 = 0x28 = 0x0001. Machine type 0, slot 7,
  pitch 0x3c00 and velocity 0x6400 equal capture frame 77 of
  `dt2-1.16-drive3-trig1-emac`; 19 payload halfwords differ (0x74-0x92,
  0x21c, 0x39c, 0x5ee). **[D]**
- **One-shot words.** In the captures 0x26 mirrors 0x22 and 0x28 mirrors
  0x24, each set only in its event's frame. A repeated frame clears
  0x22-0x2b (with 0x2a, the all-voice reset). **[D]**
- **Sound.** Voices 0/1 turn nonzero in the SHARC frame after the one that
  carried the trig (latch, then load and arm, as above); every frame ends
  cleanly; peak 0.24. From the press reaching the firmware to the first
  sound: 0.26 s at the default frame period. **[D]**
- **Start state.** The core starts from `armed_start` with the LP0 feed of
  the sample load's FlexBus log. The log `tools/dt2gui.py` records for the
  hat image is byte-identical to `out/captures/drive3/flexbus-drive3.raw`;
  both images are byte-identical to `out/plusdrive/native/dt2.img`. **[D]**

> **[C][D] Current local fixture, 2026-09-30:** The historical LP0 byte-identity
> statement above does **not** hold for the files now on disk. The sibling
> `flexbus.raw` of `snapshots/dt2-1.16-auto/223c5811012f/ready.snap` and
> `out/captures/drive3/flexbus-drive3.raw` are both 7,954,000 bytes, but their
> SHA-256 values are respectively `22e82b0882d5be5f45ecfe40fcccf8ee5c1e844829c36526d7bd913158e86767`
> and `ab91cba7290791bd87d0c4b8a959cfb887a2ee1da5cab1910e07233ec89e2a78`;
> the first different byte is at offset 5,748,197. The auto ready snapshot's
> `.ladder.json` MAIN OS and card hashes match the current extracted source
> and auto card, and its firmware source matches
> `out/sections/dt2-1.16/.source-sha256`.
> None of these checks proves that the sibling LP0 log came from that card;
> do not substitute the drive3 log when reproducing the auto-fixture GUI run.

**[D] Current auto-fixture headless replay, 2026-09-30.** The existing
`tools/live_gui_check.py` ran from that auto `ready.snap` with its matching
card and sibling LP0 log, `--limit 12M --press-at 3M --hold 2M --trig 1
--tail 0.5`, plus an external 360 s process timeout. `--limit` is an
instruction **threshold** checked at GUI chunk boundaries, not an exact cap:
the selected passing trace ended at 14,365,479 ColdFire instructions.
Waiting for the GUI worker's pause acknowledgement at the threshold now
prevents its audio tail from continuing to advance ColdFire instructions.
The v3 frameless state pack was regenerated under ignored `out/` with
`state-pack --rebuild --limit 2000000`: its complete pre-trailer body is identical
to the previous v2 pack, its card hash matches the ready snapshot's sidecar,
and its core-source hash matches both current `tools/sharc_core` and the
selected native SHARC library. The `native/live` release library had to be
rebuilt to load the v3 trailer.

The bounded wire trace `out/native/integrated-auto-smoke/auto-wire-ack.dtfr`
records the **68 actual ColdFire DSPI2 TX frames accepted by the native
queue**, not capture-pack frames: format `DTFR`, little-endian u32 version 1,
u32 count, then repeated u32 byte length and wire-order bytes.
Each observed TX buffer is 2,748 bytes; the native SHARC receive DMA consumes
only the leading `0x802` bytes of each buffer after 16-bit swapping. Its SHA-256
is `e21cb015102bd6c1222b7c733238ddf7402a725ee7e63b0fb8313d00fd1f8e8b`;
frame 18 is the sole trig frame (SHA-256
`48ccef5ab084785fb3929efc760e7a85e9fad2e90bb151be9be2b8ccaaff8767`),
and frame 51 carries its release. The trig frame's effective `0x802`-byte
DMA prefix hashes to
`6b6231c3b1360be7110da86778f8a620e6e95b4e4dd19c61faa2d896413e735c`.
The paired report shows 68 pushed/taken frames, no merges, 1,690 clean SHARC
renders, 1,314 nonzero renders, peak 0.23668, and 0.039 s to first sound;
the report and guest input-delivery clocks stay under the same ignored
directory. This is a concrete **Python ColdFire → native SHARC** comparison
target, not proof of a native ColdFire producer, real device timing, or
authentication of the snapshot/LP0 log.

**[O] Native ColdFire handoff target.** Reproduce the same ready-state input
delivery clocks from `run-ack.log` (press at 3,516,425, release at 10,552,659
instructions), then compare ordered full wire-order DSPI2 TX frames against
`auto-wire-ack.dtfr`. Wall-time-triggered reruns can deliver the pad input at
different guest clocks, so their whole-trace hashes need not match this one.
`native/live` already accepts these wire buffers; `native/machine` does not
yet own a DSPI2 peripheral or offer a ColdFire-to-player frame handoff.
Passing the wire comparison would establish only the named replay window;
independent boot, real device timing and later audio parity stay open.

**[O] Intermittent frame-stop diagnostic:** one earlier v3 run had two
`Stopped` frames out of 1,641 even though its trig sounded and the pack body
matched v2. Several subsequent bounded runs were clean, but a later
headless run stopped at SHARC frame 252 with `native-trap: unmodeled MMR at
0x1c1cd7` (`R12 = DM(I0, M5)` inside `FUN_1c18a6`); its frame sequence is
retained in ignored `out/native/integrated-auto-smoke/diagnose-13.dtfr`.
This is a PC, **not** proof of the accessed MMR address or root cause; the
separately observed `0x30000` load at `0x1c1cf9` above may be related but
has not been connected to this stop. `native/live` now exposes the first
frame-stop index/reason through `live_first_stop`, and the headless report
fails closed when `render.stopped` is nonzero. Clean later runs do not fix
this intermittent stop.
- **Reply.** The DSPI2 peer answers zeros, as every capture recorded; the
  trig-to-voice path works with zero replies. **[D]**

Hand steps: vector 191 forced every 200,000 instructions with the
frame-build gate open (item 2 above), the LP0 model (item 4) and
`drive_dma_completion`'s host model. Each frame costs the ColdFire about
50,000 instructions: over 30M instructions the forced frames made the run
8% slower in wall time, and the main task's loop ran 16 times instead of
48. **[D]**
