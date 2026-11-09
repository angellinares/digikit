# 53. The shape of the stock machines, effects and parameters, for the module SDK (2026-10-21)

A static survey of DT2 1.17 and DN2 1.12 (sharcdb `out/sharcdb/{dt2-1.17,dn2-1.12}.sqlite`, the ColdFire databases
`out/cfdb/{dt2-1.17,dn2-1.12}-emac.sqlite`, the Ghidra dumps and `tools/coldfire/paramtable.py`), to decide the shape of the
SDK's DSP library, machine contract, FX contract and effect parameters. Nothing was run. Marks: **[D]** read once in the
databases or listings by one agent, **[O]** inferred or open. No claim here is **[V]** yet: a second agent checks it against
the image bytes first. The DSP blobs are byte-identical between 1.16 and 1.17 and between 1.11 and 1.12 (`meta.image_sha256`),
so the DSP addresses below also hold on the older versions. All addresses are short-word (sw) for the DSP.

## Calling convention and code style **[D]**

The stock DSP code is compiled code with one convention on both devices:

- Call: `CALL (linked, delayed)` = Type 25a CJUMP (1657 of 1718 calls on DT2, 1617 of 1698 on DN2). Prologue
  `I7 = MODIFY(I7, -N)`, saves to `DM(I6 - n)`. Return: `I12 = DM(I6, M7)`, `JUMP (M14, I12) (DB)` with `RFRAME` in the delay
  slot.
- Arguments R4, R8, R12, more on the stack (`DM(I6, M6)` = [I6 + 1]); result R0 (F0, R0:R1 for 64-bit). Render and stage
  kernels return nothing and write through pointers.
- Callee-saved (from the voice renderer `0x1c4ecf`): R3, R5-R7, R9-R11, R13-R15, I0-I3, I5, M1-M3, B3. Never written by voice
  code: M5 = 0, M6 = 1, M7 = -1, M13 = 0, M14 = 1, M15 = -1 (finding 45).
- The exception is the float divide `0x1c06ba` (plain `CALL`, R4/R8 in, F0 out), 56 of the 61 plain calls on DT2.

This is the convention selcc emits (finding 20), so a C module and the stock code agree on it.

## How a stock voice renders **[D]**

- **DT2.** The frame orchestrator `FUN_1c2b24` calls `FUN_1c642a` at `0x1c3083` (R4 = `0x2412c8`, R8 = `0x2506ec`, R12 =
  `0x252d3c`, which points at the block length 32). It runs 32 voices in three steps:
  - stage A, per selector (`0x2567c0` = `0 1 2 3 4 0 5`), through PM table `0x8055c840`;
  - the render loop at `0x1c6ae8`;
  - the post-voice chain.

  One block renderer serves selectors 0, 1, 3, 4 and 5: `0x1c4ecf` (call at `0x1c6b00`; body `0x1c4f81`, 381
  instructions). STRETCH (selector 2) has `0x1c5576` (call at `0x1c6af1`). Each call renders a whole block. It renders 64
  samples at 96 kHz (DO 64 at `0x1c5096` and `0x1c52ab`), then `0xb80000` decimates 64 to 32.

  Voice records: 32 of `0x1d8` bytes at `0x2412cc` (end `0x244dcc`; **[C]** finding 06 had `0x24cdcc`). The voice ends
  when the renderer clears byte `+0x1b8`; an inactive voice is zero-filled. Stereo: L is the even voice, R the odd one.
- **DN2.** `FUN_1c2712` calls `FUN_1c8ef1` at `0x1c3044` (R4 = `0x241298`). Per-block setup per slot type goes through PM
  table `0x8052db90`. Four type-guarded 16-slot loops then call one engine each, and each call renders a whole block:

  | Machine | Engine (call site) | Record stride |
  | --- | --- | --- |
  | FM TONE | `0x1c54c7` (`0x1c9627`) | `0x240` |
  | WAVETONE | `0x1c6d4a` (`0x1c9611`) | `0x30c` |
  | FM DRUM | `0x1c778b` (`0x1c95f9`) | `0x2e8` |
  | SWARMER | `0x1c85ad` (`0x1c95e1`) | `0x55c` |

  FM TONE runs its operators over 64 samples and decimates to 32 with `0xb82728`. The amplitude envelope (`0xb80c39`) ends a
  voice; a machine has no end signal of its own.
- **Parameters.** The ColdFire smooths parameter values before the frame (one-pole, `FUN_400d92a2`). The DSP converts the
  s16 words to floats once per block (scale 1/32512, `0x38010204`). Inside a block, stages ramp per sample by
  (new - old) / 32, or / 64 in the DN2 FM operators.

**For the SDK.** The machine contract (one `render` call per voice per 32-sample block, state per voice, the voice ends
itself) matches the stock shape. Two stock patterns are worth supplying in the library:
- 2x oversampling with a 64-to-32 half-band decimator (DT2 sample machines, DN2 FM TONE);
- the per-sample ramp of a parameter across a block.

## Shared DSP primitives **[D]**

The stock machines share little code. Each machine is one large function: none of the 20+ machine-specific renderers,
setters or engines matches between the devices, and no oscillator or interpolator is shared between machines. What is shared
is the post-voice chain and a few maths helpers. They are identical across devices, either reloc-hash equal or byte-identical:

| DT2 / DN2 | Instructions | Role |
| --- | --- | --- |
| `1ccbd8` + reset `1ccbca` / `b81368` + `b8135a` | 59 + 8 | block biquad: 8-word record (coefficients +2..+5, state +6, +7); leaf, DO over n |
| `1cb3d8` / `1cdb56` | 98 | saturator (overdrive) |
| `1cbb57` / `b80345` | 104 | envelope generator (11-word record, rate table `0x26ad30`) |
| `1cdd8b` / `b8251b` | 55 | block copy or fill, SIMD (n even); the voice hook of `core/dsp-voice` on DN2 |
| `1cdbb2`, `1cd286`, `1cc79e`, `1cbf07`, `1ccd96` / `b82342`, `b81a16`, `b80f2e`, `b806f5`, `b81526` | 99-173 | the five filter types; `1cbf07` is a comb or delay (ring of 8192 words) |
| `1c0d68` / `1c0c76` | 127 | powf (reads three tables, writes error words) |
| `1c127c` / `1c1226` | 23 (+90) | reciprocal square root |
| `1c06ba` | 47 | float divide |

The chain uses one object pattern: `reset(R4 = record)` and `process(R4 = record, R8 = in, R12 = out, n)`, a 32-sample block
per call.

**For the SDK.** The library shape `dk_x_reset(state)` / `dk_x_block(state, in, out, n)` is the stock shape. Calling stock
primitives from a module by anchor is possible for a few pure ones: the biquad and its reset, the float divide (it toggles
MODE1 bit 16), int to float `1c0874`, and rsqrt. The rest read hidden tables or globals. Before any call, the module must pin
the context (as `core/dsp-voice` does), check SIMD counts, resolve by anchor hash per OS, run the routine on the concrete core,
and confirm that its tables are initialised at that point of the frame. **[O]** whether that is worth it against our own
routines.

## The audio chain and where an effect can sit **[D]**, roles **[O]** where marked

Every stock bus holds planar floats, 32 L then 32 R; the voice blocks are mono. The DT2 order per block:

| # | Stage (function @ call site) | Buffers |
| --- | --- | --- |
| 1 | frame copy to `0x2558dc`; unpack `0x1c24e9` x32 | frame -> track records `0x2506ec + 0xdc*t` |
| 2 | voice render `0x1c4ecf` @ `0x1c6b00` | record -> mono block `0x252df8 + 0x80*v` |
| 3-5 | level `0x1cdd8b`, BR, overdrive `0x1cb3d8`, SRR `0x1cdecb`, filter (table `0x8055c874`), the fixed chain, pan | in place -> track bus `0x252df8 + 0x100*t` |
| 6 | sends and FX `0x1c18a6` @ `0x1c3090`: chorus `0xb82680`, delay `0xb82150`, reverb `0xb80fe0` | track buses -> send buses; FX return `0x254778` |
| 7 | master `0x1c207b` @ `0x1c3099`: buses A `0x254878` / B `0x254a78`, dynamics `0xb82d41`, clip, overdrive, gain ramp | -> `0x25f180` |
| 8 | post-master `0x1c14e7` @ `0x1c30a0` (all track buses x 3.13, probably USB out **[O]**), `0x1c80f2`, `0x1c28b5` (packs into ring C, probably USB out **[O]**) | |
| 9 | Q31 converter @ `0x1c7734` (the master hook of master-gain and master-drive) | `0x25f180` -> ring A |

DN2 runs the same shape with its own drivers: voices and track chain `FUN_1c8ef1`; 16 voice slots whose mapping to tracks the
ColdFire sets per note (finding 06); sends `0x1c1738` @ `0x1c3054`; master `0x1c1f1e` @ `0x1c305d`; converter `0x1c9fd2`.
The FX units, dynamics, overdrive and filters are shared code. The voice engines, orchestrators and master drivers are
device-specific.

Candidate effect sites. Patching a callee's first instruction is preferred to a call-site patch in the big orchestrator,
which bars its AOT region:

| Site | DT2 / DN2 | What the effect sees | Risk |
| --- | --- | --- | --- |
| H0 converter call | `0x1c7734` / `0x1c9fd2` (proven) | the final master | if stage 8 is USB out, a master effect here is not in the USB audio **[O]** |
| H1 post-master entry | `0x1c14e7` / `0x1c244d` (DN2 role **[O]**) | the final master at `0x25f180` / `0x268438` | before USB out, click and meters |
| H2 sends-driver entry | `0x1c18a6` / `0x1c1738` | the 16 track buses after pan and level, before the sends | DN2 has slots, not tracks: no per-track insert there; R8 and (DN2) R14 are live |
| H3 master entry | `0x1c207b` / `0x1c1f1e` | the FX return bus, the input buses | master mix only |
| H4 an FX unit | chorus, delay, reverb entries | replaces a stock unit; in/out pointer pairs in RAM (`0x252d40`/`0x252d48`; DN2 `0x257e70`/`0x257e78`) | conflicts with stock |
| H5 voice chain entry | `0x1cdd8b` / `0xb8251b` | one mono voice block before BR, overdrive, SRR and filter | 32 calls a block; DN2 hook already used by `core/dsp-voice` |

At H2 and H3 SIMD is off and no DO loop is active; M5/M6/M7 hold their constants. The stock FX state lives in DDR, and its
rings wrap by index arithmetic (the FX units never write B or L registers). **[C]** The stock voice renderer does
write them: it sets B3/L3 and B4/L4 and restores L = 0 and B3 on exit (DT2 1.17 around 0x1c508a-0x1c532b):
- chorus at `0x80459388`, 512-entry lines;
- delay at `0x8005912c`, rings of 524,288 floats, time clamped to 8 s;
- reverb at `0x80000018`, about 0x59114 bytes.

The units run every block, and no stop or clear path was found **[O]**. Static cost, an overcount:
- chorus 7.1k, delay 6.0k, reverb 17.2k instructions per block;
- the whole chain 241k on DT2 and 236k on DN2, against 211k and 335k measured (finding 06).

**For the SDK.** The master FX hook should move from H0 to H1 if stage 8 is confirmed as the USB output. Check it in the
emulator with master-gain at H0: is the USB stream scaled? A per-track insert is possible on DT2 at H2 and not on DN2. A
per-voice "voice FX" before the filter (H5) exists on both devices.

## Parameters, pages and persistence (ColdFire) **[D]**

- **Descriptor table.** Parameter table: [finding 100](100-module-limits.md). FX rows are codes `0x69..0x9f` on both devices: 55 rows, mirrors 25..69, the same on both. There are no
  free rows; `core/params` appends.
- **Defaults.** A new FX code gets default 0 in a new kit unless the FX index-to-code table is patched. That table is 71
  words at bss `0x4316e854` on DT2, built by `0x400da84e`.
- **Pages.**
  - DT2 page descriptors are `0x2c` bytes: two names plus 9 codes. 33 pages are reachable through `FUN_400c898a`, which
    returns -1 above 0x20.
  - The FX view lists pages `{13, 14, 15}` (DEL, REV, CHO); the master view lists `{16..20}`.
  - Stock leaves 9 encoder slots unused: DEL 1, REV 2, CHO 2, FX MIX 4.
  - A new page needs three changes: the accessor bound, the view's list and count, and the draw code, which special-cases
    pages 14 and 15.
- **Value path.** Kit value, then mirror, then the smoother, then the vector-191 handler, then the TX frame, every block. The
  DSP copies are DT2 `0x2558dc` and DN2 `0x25c48c`.
  - The FX block is `0x6da + 2(k-25)` on DT2 and `0x9fa + 2(k-25)` on DN2, for k = 25..69 only. **[C]** of finding 04's DN2
    range 25..100: fx[70..100] are not sent.
  - Free words: fx[40] is carried but statically unread. The frame tail past the data is zero in stock: 174 words on DT2 and
    15 on DN2.
- **Persistence.**
  - The kit's `fxSetupStorage_v0` is `0xa0` bytes: 46 halfwords and 56 unused bytes. Loader and saver tables: DT2
    `0x40213998` / `0x40213880`, DN2 `0x401fd278` / `0x401fd160`.
  - Stock ignores the spare bytes. A new FX index needs both tables and the loop bound extended. The format version stays,
    so stock still loads the file.
- **Locks, modulation and MIDI.** FX parameters are not lockable (locks cover track < 16 and Sound indices only) and are not
  LFO destinations. Added rows have no MIDI CC or NRPN.
- **Machine model check.** The SDK's machine parameter model matches stock: 8 positional slots, the level outside the frame
  slots, raw s16 storage, no clamp in stock. Slot 3 of a DT2 SAMPLE clone is the sample word.

**For the SDK, effect parameters.** Two routes:

| Route | Pieces | Effort (agent time) | Confidence |
| --- | --- | --- | --- |
| A: rename stock FX rows (as master-drive does with fx[66]) | generic `[[fx_param]]` row rename | about 0.5 day | about 85 % |
| B: own rows and slots | rows (core/params), default table, a ColdFire hook that writes free frame words, persistence tables, the 9 free page slots (or a new page) | 4-7 days | about 55 % |

Route A takes a stock function away for each parameter. Route B on the free slots gives up to 9 knobs without a new page.

## Open **[O]**

- ~~Is stage 8 the USB output (decides H0 against H1)?~~ **[C]** Yes, the master pair is the first channel pair of ring C, and H0 is not in it: finding 55 (which hooks `FUN_1c80f2` on the DT2 and H1 on the DN2).
- The roles of the five filter types and of the fixed chain functions.
- DN2: the origin of R8 in the engine calls; the role of `0x1c244d`.
- The stop and kit-change behaviour of the FX units.
- Which frame words besides the tail are really free (computed-address writers were not checked).
