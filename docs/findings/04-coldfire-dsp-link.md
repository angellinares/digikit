# ColdFire-DSP link

The periodic DSPI2 frame the ColdFire sends the SHARC, the frame capture and the frame link on 1.16, how the machine type reaches the SHARC, and the mirror/SRC-parameter path.

## The ColdFire tells the SHARC through a periodic DSPI2 frame **[V][O]**

Static reading only: Ghidra, the repo disassembler and `tools/refscan.py`.
No emulator run. In this section **[V]** means a second agent re-checked the
claim against the image bytes, and **[D]** means one agent read it from
Ghidra or disassembly output and it was not re-checked.

- `FUN_4002ce4a` writes the handler `0x4002d652` to the RAM vector slot
  `0x400002fc` (vector 191, INTC1 source 63) and 5 to `ICR1_63`
  (`0xFC04C07F`). The handler reads eDMA channel 50's SADDR (`0xFC045640`).
  When the counter at `0x4028ac90` is zero, it calls
  `FUN_400cf9c4(0x802, 0x80005348, 0xabc, 0x8000488c)` at `0x4002d6ba`.
  **[V]**
- **Correction: the vector-191 producer is now identified.** Channel 50 is
  SSI0 transmit, but its major-loop completion is INTC1 source 42/vector 170,
  not source 63. The RM lists source 63 as unused hardware and documents bit
  31 of `INTFRCH1` (`0xFC04C010`) as its software-force bit. On 1.16,
  initialization arms TCD48/TCD50, writes SERQ 48 and 50, and initially
  installs the generic vector-170 handler `0x400d2f98`. It also retains
  `0x4002d322` as the later channel-50 ISR. That ISR writes 50 to
  `EDMA_CINT` at `0x4002d35a`, then sets `INTFRCH1` bit 31 at `0x4002d360`.
  Vector 191 enters `0x4002dd0c` and clears the force bit at `0x4002dd30`.
  The same chain is present at relocated addresses in 1.15C. Thus the real
  cadence is SSI0-paced eDMA50 completion -> vector 170 -> software-forced
  vector 191. No instruction reloads the frame gate counter by its literal
  address, so the exchange cadence and counter reload remain open. **[V][C][O]**
- `FUN_400cf9c4` is a DSPI2 (`0xEC038000`) send and receive driver using
  eDMA channels 28 and 29. `FUN_400cf67c` sets CTAR0 to `0xFA010000`
  (16-bit frames). Each PUSHR entry is `0x8001xxxx` (CONT, PCS0). The last
  entry gets EOQ, TCD 29 DADDR is PUSHR (`0xEC038034`), and SERQ is written
  with `0x1c`. The counts are bytes: TX `0x802` (2050 bytes, 1025 frames)
  and RX `0xabc` (2748 bytes). Both buffers are in the 64 KB on-chip SRAM
  at `0x80000000`. **[V]**
- "TX length" and "RX length" above are misleading: those are not two
  independently sized directions. The eDMA word count of both the receive and
  the transmit descriptor is set from one variable, written once at boot by a
  function that returns `0xabc`, so the SPI transfer is a single fixed
  2,748-byte full-duplex frame. The first argument -- `0x802` on Digitakt II,
  `0xa80` on Digitone II -- is how many real payload words the driver copies
  out of the caller's buffer; the rest of the frame is tag-only entries. The
  third argument happens to equal the frame length, so the whole received
  frame is copied back. Frame offsets such as the machine type at `0x94 + 2i`
  are offsets into the payload, which maps one to one onto the frame's data
  words, so they are unaffected. Found by `angellinares` on Digitone II 1.11
  (PR #12) and checked here against `FUN_400cd2bc` on 1.16. **[V][C]**
- Ghidra lists no callers for `FUN_400cf9c4` or for the SHARC boot routine
  `FUN_400cef6c`. The two `jsr` calls to `FUN_400cf9c4` (`0x4002d6ba`,
  `0x400d13d4`) are in code that Ghidra did not assign to a function,
  because the stock ColdFire language cannot decode `movclr` (next
  section). An empty Ghidra caller list is not evidence of dead code in
  this image. **[V]**
- Frame content: a header written at `+0x00` (the constant 2) and at
  `+0x22` to `+0x32`, then a 16-pass loop at `0x4002e470`-`0x4002e63a`. The
  loop reads three per-track SRAM tables: `0x800047fc + 4*i` (longwords,
  shifted by `asr.l #8`), `0x80003340 + i*0x9a` and `0x80005b50 + i*0x8e`.
  The last base is the constant that `FUN_400db9aa` returns. **[V]** What
  the fields mean is not known. **[O]**
- A second handler, `0x400d1378`, installed by `FUN_400d15bc`, sends
  `0x802` bytes from `0x429307f4` with only the first word set (1) and
  receives nothing. **[V]** `FUN_400d15bc` is called by the console command
  parser `FUN_400cd594` on `ENTER TEST MODE` and by `FUN_400cef6c`.
  `EXIT TEST MODE` calls `FUN_4002d5c4`, which reinstalls `0x4002d652`.
  **[D]**
- **Superseded on the machine-type question by "The machine type reaches
  the SHARC, at TX frame offset `0x94 + 2i`" below.** The negative result in
  the next bullet is correct as stated -- the handler does not read `+0xa2`
  -- but it does read a copy of that byte in SRAM, and the type does reach
  the frame. **[C]**
- Machine type: no call to `FUN_4004fc02` or `FUN_400caf48`, and no read
  of `+0xa2`, was found in the handler or its frame loop. The one link
  found is in the commit `FUN_40035e90`: when the new type is 5 (MIDI) it
  calls `FUN_4002ed16(track, 0x7fff)`, which writes 1 to
  `0x80004684 + 4*track` and `0x800046c4 + 4*track` (addressed as
  `0x80003340 + (track + 0x4d1)*4` and `+ (track + 0x4e1)*4`). **[V]**
- Not known: whether the handler reads those two arrays, and whether a
  SLICE or PLACEHOLDER track produces a different frame. The writers of the
  per-track tables are linked with the handler, among functions that use
  file-browser strings (`ENTER DIR NAME`, `WRITE PROTECTED`), in about
  `0x4002c0fa`-`0x4002f000`. About 45 of them are called from elsewhere and
  they were not traced one by one. So "stock engine and own parameters" for
  a new sample machine is still open. No engine id has been found in the
  frame. **[D][O]**
- The 1.16 post-gesture checkpoint's SSI0 configuration is externally
  clocked: `MISCCR=0`, `CDRH=0`, `TCR=0x82` (FIFO0 enabled, external bit
  clock and frame sync), `RCR=0x482`, and `CCR=0x16f00` (24-bit words,
  16 words per frame). `TMASK=RMASK=0xffff0000`; `FCSR=0x88` sets the FIFO0
  receive and transmit watermarks to eight words. TCD48/50 minor loops are
  32 bytes, so one DMA request accounts for eight serial words and a 64-minor
  major loop accounts for 512 words. The external `SSI_CLKIN`/frame frequency
  is not captured, so there is no defensible numeric request-rate default;
  for a continuous stream the request rate is external bit clock / (24\*8),
  or twice the frame rate with all 16 slots active. **[D][O]**
- The SHARC-side three-wire output candidate is now narrowed to DAI1 SPORT4A.
  The direct DAI setup is identical in 1.15C and 1.16 apart from its relocated
  instruction address. On 1.16, the stores at `0x1cb2dd`-`0x1cb31a` set
  `DAI1_CLK0=0x3def7b9c`, `DAI1_CLK4=0x3def7bce`,
  `DAI1_FS0=0x3def7b9c`, `DAI1_PBEN0=0x00001041`, and
  `DAI1_PIN0=0x0fc51d38`. The public selector tables decode those values as:
  SPORT4A and SPORT4B take clock and frame sync from PCG C; DAI1 pins 1-3
  are enabled outputs carrying PCG C clock, PCG C frame sync, and SPORT4A
  primary data. Pin 4's selector is LOW, but its output buffer is disabled,
  so this does not establish an electrical low on the pin.
  `DAI1_CLK4.IN0=0x0e` also makes the cross-DAI0 pin-3 clock available as
  PCG C's external input; it does not prove PCG C selects that source. This
  is the firmware-configured SHARC-side candidate for ColdFire SSI0, but the
  DAI mux alone does not prove board wiring or runtime signal activity.
  **[V][O]**
- The loader's immutable SPORT instance table is 16 records of `0x28` bytes at
  DM `0x26954c..0x2697cb` (loader aliases
  `0x2826954c..0x282697cb`). Its record IDs are `0..7, 0x0a..0x11`.
  Record `0x0a` at `0x26968c` pairs SPORT4A control base `0x31002400`
  with DMA10 base `0x31023000`; record `0x0b` at `0x2696b4` pairs
  SPORT4B base `0x31002480` with DMA11 base `0x31023080`. The public MMR
  table identifies those DMA bases as SPORT4 half A/B. Function
  `0x1ca58a` brackets the array with `0x269548` at `0x1ca5a1` and
  `0x2697c8` at `0x1ca66f`, then adds the latter base to `I4` at
  `0x1ca698` before the record-field loads. A second agent independently
  checked the initializer and instruction bytes. **[V]**
- The documented `SHIFTOP=0xc8` operation at `0x1ca69d` is `btgl`, and the
  concrete tracer now models it. With the explicit
  `--assume-32bit-normal-words` interpretation, Type-4a and non-LW Type-15b
  immediate modifiers use four-byte displacements. A calibration seed
  `I4=-0x13c` (`0x26968c-0x2697c8`) makes the consumer read SPORT4A fields
  `0x269694..0x2696b0`, including `0x31002400` and `0x31023000`; the
  independent SPORT4B control seed `I4=-0x114` reads the analogous
  `0x31002480` and `0x31023080` fields. A second agent checked the public
  `btgl` definition, opt-in address interpretation, and trace values.
  **[V][O]** These are calibrated consumer slices, not runtime provenance:
  the firmware path supplying SPORT4A's relative `I4` value is still open,
  and neither trace establishes execution, direction, descriptor state,
  application-buffer ownership, or signal activity. Both stop at the
  undocumented Type-23 prefix at `0x1ca6e9`. Reproducible evidence is in
  `out/experiments/sharc-ssi-peer/instance-array-003/report.json`.
- No direct immediate store to SPORT4 or DMA10/11 was found in the main SHARC
  code. The immutable association and calibrated consumer therefore do not
  yet recover the runtime DMA descriptor or application buffer. **[O]**
- **The PCG C part of that main-code negative is now superseded by the loaded
  L2 service image.** At loader byte alias `0x282d7158`, the final 16 bytes are
  `98 b1 b8 00 45 b4 b8 00 06 b7 b8 00 b3 b9 b8 00`, four little-endian
  VISA SW pointers `0xb8b198`, `0xb8b445`, `0xb8b706`, and `0xb8b9b3`.
  The third routine family, rooted at `0xb8b706`, contains aligned Type-14a
  accesses to PCG C's `CTLC0` (`0x310ca300`), `CTLC1` (`0x310ca304`), shared
  C/D pulse-width register `PW2` (`0x310ca310`), and `SYNC2` (`0x310ca314`).
  Byte-checked stores include `0xb8b75a` to CTLC1, `0xb8b7bc` to CTLC0,
  `0xb8b801` to PW2, and `0xb8b71d` to SYNC2; the public hardware reference
  supplies those register names. A second agent independently replayed the
  loader, checked the table and instruction bytes, and cross-checked the
  public register table. **[C][V]** This establishes the driver code and its
  possible writes, not that a particular path ran or what values it wrote.
  The bounded concrete trace reaches a CTLC0 load and then stops at the
  provisional `Type23p_undoc16`, so source selection, divisors, clock input,
  and cadence remain open. **[O]** Reproducible evidence is in
  `out/experiments/sharc-l2/l2-001/report.json`.
- The public Type-8 encoding table makes field `j=0` a non-delayed branch and
  `j=1` the `(DB)` form; Type-25 `cjump` is explicitly delayed. The concrete
  tracer previously treated every Type-8 transfer as delayed, which produced
  false `nested delayed transfer` stops at the PCG entry. It now applies delay
  slots only when `j=1`; with that correction, the PCG trace reaches the
  register access above before the provisional instruction boundary. **[C][V]**
- The DAI result does not yet supply a numeric request cadence. PCG C's
  `CTLC0/CTLC1` divisors and source-select bits have not been recovered, and
  the frequency presented at the possible external source on DAI0 pin 3 is
  unknown. Therefore 48,000 requests/s remains an exploration profile, not a
  firmware-derived default. **[O]**
- No decoded immediate or loaded 32-bit word equal to `0x007fffff` was found
  in the available 1.16 SHARC main image/loader data. Decoded immediates equal
  to `0x7fffffff` do occur, including `0x1c4eb3`, but no path from any such
  value to SPORT4A/DMA10 has been established. The nearby
  `DAI1_PIN4/PADS0` setup value `0x000fffff` is a routing/pad configuration
  value, not the ColdFire RX synchronization marker. RX replay must therefore
  remain disabled until the runtime SPORT DMA producer or an equivalent
  firmware-derived buffer is identified. **[D][O]**
  The L2-aware decode adds another `0x7fffffff` immediate at SW `0xb88d06`
  (function entry `0xb88cee`), but it is still not the `0x007fffff` marker and
  has no established path to SPORT4A/DMA10. **[D][O]**
  The reproducible command list, input hashes, store table and open items are
  recorded under `out/experiments/sharc-ssi-peer/static-001/report.json`.
- The same checkpoint has no `0x007fffff` row head among the 32 entries in
  any of `0x4fe57100`, `0x4fe57900`, `0x4fe58100`, or `0x4fe58900`; all are
  zero. The generic vector-170 handler at `0x400d2f98` acknowledges CINT50,
  scans the currently completed RX bank for that marker, increments
  `0x43153a20` only when the marker is at index zero, and installs the pending
  `0x43153a28` callback into vector slot `0x400002a8` only after that counter
  exceeds 63. At this checkpoint the counter is zero, the pending callback is
  `0x4002d322`, and the slot still contains the generic handler. This is the
  missing external RX synchronization/handover, not merely a missing TX DMA
  tick. A second agent checked the marker scan, threshold, row heads, globals,
  and vector slot against the 1.16 bytes/restored pages. **[V][O]**
- `emu/ssi.py` now supplies an opt-in, exact-deadline event source for only
  TCD48/TCD50. It models the observed 32-byte minors, 64-minor completion,
  scatter/gather reload and vector 170; preserves RX destination bytes rather
  than inventing peer data; lets the guest CINT50/`INTFRCH1[31]` ISR run; and
  hands vector 191 over only at that ISR's image-resolved RTE boundary. Its
  request rate is mandatory, its checkpoint component is separate from the
  version-1 PIT/DTIM layout, and adding it to a legacy checkpoint requires an
  explicit upgrade. **[D]**
- At the explicitly exploratory 48,000-request/s profile,
  `out/experiments/ssi0-dma/control-001/` reaches the generic vector-170
  handler 16 times in 400,140 exact instructions with zero fault pages. A
  clean and narrow-traced run produce byte-identical endpoint snapshots
  (`11f81e1c...`). They correctly do not reach `0x4002d322` or vector 191,
  because no external RX marker was supplied. The separate
  `vector-chain-control-001` host-patches only the vector-170 slot and, at a
  separate 1,000-request/s exploration rate, records two each of the normal
  vector-170 ISR and vector 191, proving the model's
  CINT/force handoff as calibration, not natural behavioral provenance. No RX
  or SHARC payload was synthesized. **[D][O]**
- A marker-only host calibration now separates synchronization from useful RX
  payload. `out/experiments/a2-marker-calibration/report.json` records four
  explicit `0x007fffff` row-head pokes. At the deliberately exploratory
  1,000-request/s rate, the guest increments the handover counter 64 times,
  replaces vector 170 with `0x4002d322`, and subsequently takes vector 191.
  Replaying the accepted real-panel machine-selection recipe _after_ that
  handover still gives one commit/setter, eight invalidations, 54 normal
  vector-170 entries and 54 vector-191 entries, but zero `FUN_4002d438` hits
  and zero track-0 row writes. The marker is therefore sufficient for the
  guest handover control but not for useful queue work: the missing
  firmware-backed RX payload/queue producer remains independently blocking.
  This is host-state calibration, not behavioral A2 evidence, and 1,000 Hz is
  not a recovered cadence. **[D][O]**
- The corresponding 48,000-request/s host-marker stress control takes 2,565
  normal vector-170 and 1,807 vector-191 entries while the panel window records
  no main-loop pass or commit. That profile is unsuitable for the panel
  experiment on this emulator; it does not measure or disprove the board
  cadence. **[D][O]**
- DSPI2 and eDMA channels 28/29 remain outside a general peripheral model:
  `emu/edma.py` handles UART8 channel 35, `emu/ssi.py` narrowly handles SSI0
  channels 48/50, and `0xEC03802C` is still a constant. **[V][O]**
- Ruled out as the control link **[D]**:
  - FlexBus `0x8C000000` (`FUN_400cfd40`, callers `FUN_40146148`,
    `FUN_4014653c`, `FUN_401465a4`) carries sample pages and slot headers
    (address, length, loop point). Only `FUN_400cf4a8`, `FUN_400cf534` and
    the boot routine `FUN_400cf67c` access `0x8C000000`-`0x8C00000F`. This
    agrees with `docs/REMAINING.md` B.6.
  - `FUN_4012720e` builds 28-byte-header packets (sequence number,
    fragment offset, checksum) for the SysEx dump and receive code.
  - The `Digisharc::rpcMsgOpReq_t` and `rpcMsgPingRequest_t` constructors
    (`FUN_401bf37c`, `FUN_401bf3be`) are called from `FUN_40115372`, which
    references `"MY ANALOG FOUR"`. `rpcMsgHeader_t`'s constructor is
    `FUN_401b6316`.
  - DSPI1 (`0xFC03C000`, eDMA 14/15, `FUN_400cfb92` via `FUN_40011df8`)
    carries short opcode request and reply transactions, probably to a
    codec.
  - eDMA channel 59 is eSDHC block I/O (`FUN_400f0f7a` write,
    `FUN_401208fe` read).

## Digitone II 1.11: the same link and the same machine table shape **[V][D][O]**

- The sections come from `uv run python -m emu.extract
Digitone_II_OS1.11.syx -o DIR`. 1.11 also has a section 8 (159,948
  bytes, mostly `0xFF`) that 1.10E does not have. **[D]**
- Dispatcher `FUN_400c248e`: `moveq #4,d1`, a bound check, `muls` by
  `0x2c`, `addi.l #0x42432b24`. Out of range it returns `0x42432bd4`
  (entry 4). **[V]** The five descriptors are 0 FM TONE, 1 WAVETONE,
  2 FM DRUM, 3 SWARMER and 4 MIDI. **[D]**
- `FUN_4004b7f2` calls vtable `+0x28` twice and returns the byte at `+0xde`
  of the result: the machine type. **[V]** `FUN_4004b860` reads `+0xdf`,
  probably the filter type. RTTI names `Digisharc::machineType_t`,
  `Digisharc::synthParams_t` and `VoiceConfig::updateMirror`. **[D]**
- Handler `FUN_40025e36` reads `0xFC045640`. When the counter
  `0x402876f8` is zero, it calls
  `FUN_400cf7be(0xa80, 0x80005e60, 0xabc, 0x800053a4)` at `0x40025e9e`:
  TX 2688 bytes, RX 2748 bytes. The installer at `0x400d11d4` puts the
  test handler `0x400d0f90` into `0x400002fc` and 5 into `0xFC04C07F`.
  **[V]**
- The frame has 16 per-track slots of `0x92` bytes, copied from
  `0x800068e4 + i*0xca` with four `FUN_40134490` copies per slot.
  `FUN_400db12a` rebuilds that table from `0x80003af0` and returns its base
  as a constant. **[D]** No read of the machine type was found on this
  path, and the code that fills `0x80003af0` was not found. **[O]**
- On Digitone II the engine must reach the DSP, yet static reading did not
  find it there either. Not finding the machine type on either image is a
  limit of static reading, not evidence that the frame lacks it. **[O]**

## LFOs and the modulation matrix are ColdFire code, in the frame ISR **[D][O]**

Read statically from the image bytes of **Digitakt II 1.16** and **Digitone II
1.11** by `angellinares/dn2_firmware_explore` (its `docs/modulation-matrix.md`),
not run in this emulator — hence **[D]**. It names the function behind the
MSAC-with-load instruction this project patched Unicorn for, and places both
kinds of modulation on the ColdFire side of the DSPI2 frame link.

**Neither the LFOs nor the MIDI modulation sources reach the SHARC as
parameters.** Both are computed per audio frame on the ColdFire and applied into
the per-track parameter value array before the frame is built. On Digitone II
the frame carries value-array indices 25–99 only; LFO parameters are indices
1–24 and are never sent.

### The modulation kernel and the six sources

|                                                  | Digitakt II 1.16 | Digitone II 1.11 |
| ------------------------------------------------ | ---------------- | ---------------- |
| MAC kernel, one source through four destinations | `0x400d9354`     | `0x400db1dc`     |

The kernel heads are byte-identical. Each destination descriptor is a longword
`depth:s16 << 16 | dest:s16`, and the kernel applies
`msacw %d1l,%d2u,%a1@+,%d2,%acc0` — **a MSAC with load, the form Unicorn got
wrong** — then reads, saturates and writes back the parameter at `dest`. It is
reached as `lea %pc@(...),%a3; jsr %a3@`, so a direct-call scan finds no callers.

On Digitone II 1.11 the driver `0x400db22c` runs **six sources x 4 destinations
x 16 tracks**. The six are the MIDI performance modulators — **Velocity, Mod
Wheel, Pitch Bend, Breath Controller, Aftertouch, Key Tracking** — identified
from a contiguous string run before `Sound::updateMirror`, a registration
sequence naming five of six objects, and six `*SetupView` RTTI classes; Velocity
is pinned independently by its note-time write. Rows 2–5 are assigned by
registration order, not individually pinned. **[D][O]**

### The LFO tick

|                                           | Digitakt II 1.16           | Digitone II 1.11                        |
| ----------------------------------------- | -------------------------- | --------------------------------------- |
| LFO tick, generator and apply             | `0x40139342` (806 B)       | `0x40137726` (1,028 B)                  |
| called once per frame from                | `jsr` at `0x4002e91c`      | `jsr` at `0x400272d4` in `FUN_40025e36` |
| inner loop start, `moveq #2` (three LFOs) | `0x4013935e`               | `0x40137784`                            |
| state initialisers, 16 x 3 x 40 B         | `0x40138f50`, `0x40138fa4` | `0x401372f4`, `0x40137348`              |
| waveform function table                   | `0x4022231c`               | `0x4020b340`                            |
| random-wave slew table                    | `0x40222334`               | `0x4020b358`                            |
| value-array stride per track              | 142                        | 202                                     |
| `DEST` upper bound                        | 70                         | 100                                     |

Per track, per LFO, the tick reads `SPD MULT FADE DEST WAVE SPH MODE DEP` from
the value array and:

- **Tempo-synced or free.** `MULT` is clamped to 0–23. For 0–11 the rate is an
  argument the ISR passes in; for 12–23 it subtracts 12 and uses a fixed
  `14400`. Increment is `SPD x rate >> (11 - MULT)`, with `SPD` and `DEP` bipolar
  about `0x4000`.
- **Phase** wraps at 1,382,400,000 on Digitone II.
- **Waves 0–5** are dispatched through the function table. The six entries keep
  the same relative spacing on both images; by their arithmetic, in order:
  triangle, parabolic sine (EMAC `x*|x|` with 0.9 and 0.194), square, inverting
  ramp, exponential, `max(x, 0)`. Entry 6 is null.
- **Wave 6 is sample-and-hold** from a private lagged Fibonacci generator
  (Digitone II `0x4013739c`) — the LFO never calls `rand()` — slewed through the
  table above, indexed by `SPH >> 8`.
- **Apply:** `value[DEST] = clamp(value[DEST] + sample x DEP x fade, 0, 32512)`.

The number of LFOs is a literal loop start with field offsets written for the
last LFO, so a fourth needs the start, the offsets, the 120-byte state stride and
the 1,920-byte state arrays changed, plus somewhere for its parameters. **[O]**

**Why a search for it fails.** It is called from inside a function whose
boundaries a caller heuristic misplaces; stock Ghidra stops at the `movclr` in
that function (see the section above); its random wave uses a private
generator; and it lives in a clock module rather than beside the kernel.

**Open.** Neither is exercised in this emulator. The rate argument's source (the
tempo) is not traced, and a second single-track tick on Digitone II 1.11
(`0x401373dc`, its own 3 x 40 state array) is not identified. **[O]**

## The machine type reaches the SHARC, at TX frame offset `0x94 + 2i` **[V][C]**

This corrects "The ColdFire tells the SHARC through a periodic DSPI2 frame"
above, which recorded that no link from a track's machine type into the frame
could be found. The link exists. Earlier searches missed it because the frame
handler never reads the machine-type field of a track object: it reads a
**copy** of that byte in on-chip SRAM. No xref query and no `tools/refscan.py`
sweep for `+0xa2` could have found it. **[C]**

Addresses are Digitakt II 1.16.

- `FUN_4002d438(src, track)` reads the machine type at `src + 0xa2`, asks
  `FUN_400da3b0` whether it is permitted, and on yes memcpys `0x9a` bytes
  **starting at that same byte** into SRAM at `0x80003cd0 + track*0x9a`. Byte
  0 of each SRAM row is therefore the track's machine type. **[V]**

  ```
  4002d458  712a 00a2       mvs.b (0xa2,A2),D0      ; the type byte
  4002d45e  4eb9 400d a3b0  jsr FUN_400da3b0.l      ; permitted?
  4002d4aa  486a 00a2       pea (0xa2,A2)           ; source = the type byte
  4002d4b8  0680 8000 3cd0  addi.l #-0x7fffc330,D0  ; dest = 0x80003cd0 + track*0x9a
  4002d4c0  4e93            jsr (A3)                ; memcpy, 0x9a bytes
  ```

- The vector-191 handler `0x4002dd0c` loads that byte into D7 and stores it,
  sign-extended to a word, into the TX frame: **[V]**

  ```
  4002eb4a  1e28 0990       move.b (0x990,A0),D7b   ; A0 = 0x80003340 + i*0x9a,
                                                    ; so +0x990 is 0x80003cd0 + i*0x9a
  4002eb80  4887            ext.w D7w
  4002ebe0  3747 0094       move.w D7w,(0x94,A3)    ; A3 = 0x80005348, the TX buffer
  ```

  Nothing between `0x4002eb80` and `0x4002ebe0` writes D7: the four
  `FUN_401360ac` copies in between take stack arguments and do not touch it.
  The `tst.b D7b` at `0x4002eb68` is a second, independent use of the same
  register, not its only one. Reading only that first use is what made an
  earlier pass conclude the type was reduced to a boolean. **[C]**

- Measured, not only read. `tools/sharcframe.py` on
  `out/snapshots/dt2-1.16/boot400M.snap` with `--open-gate --passes 3`, poking
  track 0's type byte at `0x80003cd0` and diffing the captured 2050-byte frame
  against a type-0 baseline, changes exactly two bytes -- reproducibly across
  passes 1 and 2, and across three values: **[V]**

  | poked type | frame `0x95` | frame `0x73d` |
  | ---------- | ------------ | ------------- |
  | 4          | `00` -> `04` | `00` -> `01`  |
  | 5          | `00` -> `05` | `00` -> `01`  |
  | 6          | `00` -> `06` | `00` -> `01`  |

  `0x95` is the low byte of the big-endian word at `0x94 + 2i`: the type
  verbatim. `0x73d` is the low byte of the word at `0x73c + 2i`, a derived
  flag. The static read and the measurement were made by different agents from
  different evidence and agree exactly.

- So a new machine is not a ColdFire-side concern only. The DSP is told, per
  track and every frame, which of the seven types a track is. What the SHARC
  does with the value was the next question: find the reader of receive-buffer
  offset `0x94 + 2i` in the DSP program. **[C]**

### The SHARC reads and change-tests the `0x94 + 2i` machine word **[V][O]**

The 1.16 SHARC image has a byte-backed reader in `FUN_001c2b24`. The relevant
chain is:

- caller `0x1c7719` copies `I5` to argument `R8`, then `0x1c771e` calls
  `FUN_001c2b24` at `0x1c2b24`;
- `0x1c2ccb` adds `0x94` to `I1`, and `0x1c2cd4` preserves that pointer in
  `I10`; the same function also forms offsets `0x75c` at `0x1c2cc1` and
  `0x73c` at `0x1c2cd7`, independently matching two other fields in the
  ColdFire's `0x802`-byte frame map;
- reset initialization sets `M7=-1`; `0x1c2ca6` copies `M7` to `R11`,
  `0x1c2cff` computes `R5 = LSHIFT R15 by R11`, and `0x1c2d02` copies `R5`
  to modifier `M0`. This establishes the divide-by-two index transform, but
  not yet the natural runtime provenance of `R15`. **[D]**
- `0x1c33c4` copies `I10` to `I0`; `0x1c33d2` loads `R0` with
  `DM(I0,M0) (SWSE)`;
- SHARC+ scaled address arithmetic multiplies `M0` by the short-word width,
  so a track modifier `i` addresses byte offset `0x94 + 2i`, not
  `0x94 + i`;
- `0x1c33c7` loads a cached per-track short word into `R1`, `0x1c33d7`
  compares `R1` with received `R0`, and `0x1c33df` branches on `EQ`. Thus the
  field is behaviorally consumed and change-tested, not merely copied or
  stored. A second byte review reproduced the image hash, exact instruction
  bytes and fields, scaled-address rule, compare and branch. **[V]**

`tools/sharc_interface_probe.py` verifies the exact image hash, instruction
bytes and fields, and runs bounded symbolic slices for tracks 0, 1 and 15.
Those slices reach byte addresses `spi_rx+0x94`, `spi_rx+0x96`, and
`spi_rx+0xb2`, followed by the compare and `EQ` branch. The slices seed
`I10=spi_rx+0x94` and `M0=track`, so they are discovery evidence and
explicitly `qualifying: false`; they do not establish natural pointer/index
provenance or runtime reachability.

A target-bounded follow-up crosses the first changed-value path without
expanding startup. The `EQ` branch at `0x1c33df` has two delay slots:
`0x1c33e2` performs the documented `R2 = ASHIFT R2 by -8`, and `0x1c33e5`
stores the shifted `R2` to `DM(I5+0xc4)`. If the comparison is not equal,
fall-through instruction `0x1c33e7` immediately overwrites that same word with
`M14`; the equal branch skips this overwrite and both paths meet at
`0x1c33e9`. Thus the first concrete changed-machine effect is
`DM(I5+0xc4) = M14`. The meaning of that field and `M14`, and the multifunction
compute at the join, remain open. **[D][O]**

The same experiment extracted the final `0x802`-byte TX buffers from the four
accepted calibrated A2 snapshots. Track 0 is type 2 only after machine change
plus TRIG 1; machine+PLAY, TRIG-1-only and PLAY-only controls retain type 0.
Track 2 remains type 2 in all four frames and is therefore a useful unchanged
within-frame control. Artifacts are under
`out/experiments/sharc-interface-reader/`. **[D]**

This closes the static semantic join from the ColdFire TX field to a SHARC
reader. It does **not** yet prove the physical DSPI2 transport, the SHARC DMA
buffer owning `I5`, natural execution from strict entry, cadence, or the
non-`EQ` downstream machine-selection behavior. Those remain open. **[O]**

The two strongest candidate receive states are now concrete but still not
aliased to `I5`. Callers `0x1c7dae` and `0x1c7c14` pass selectors 1 and 2 with
`R12=0x261b18` and `R12=0x261a10`, respectively, to `0x1c9fd5`.
`0x1c9fed` copies `R12` to `R14` and `0x1c9ff3` copies `R14` to state base
`I3`. At `0x1ca03d`, `I4=I3+0x94`; `0x1ca040` copies that pointer to `R4`;
`0x1ca042` stores it at state offset `+0x20`; `0x1ca044` sets `I12=32`; and
`0x1ca046` stores 32 at state offset `+0xb0` before calling `0x1c9f9c` at
`0x1ca048`. The resulting candidates are:

| selector |      state | `state+0x20` points to | count at `state+0xb0` |
| -------: | ---------: | ---------------------: | --------------------: |
|        1 | `0x261b18` |             `0x261bac` |                    32 |
|        2 | `0x261a10` |             `0x261aa4` |                    32 |

No byte-backed reference or pointer constant yet equates either candidate
buffer with the frame-reader's runtime `I5`, so these remain descriptor/buffer
candidates rather than established ownership. The reader-side `I5` is now
bounded more tightly: `0x1c76e5` computes `I5=I6-14` normal words, or
`I5=I6-0x38` under the established 32-bit normal-word model, before
`0x1c7719` copies it to `R8`. Equality with `0x261bac` or `0x261aa4` would
therefore require runtime frame pointer `I6=0x261be4` or `I6=0x261adc`,
respectively. No static path establishes either frame value. The smallest
decisive runtime observation is now `I6/I5` at `0x1c76e5`, not a broad buffer
watch. The surrounding task setup does not close the gap: `0x1c7770` loads
callback `0x1c7749`, with `R8=0x25f7c0` and `R12=1000`, before the
task-create-shaped L2 call at `0xb8615d`. That service reserves its own frame
and calls deeper L2 code; its resulting task stack/TCB and the later reader
`I6` are runtime products. The callback begins by calling `0xb86b1e`, whose
supported chain reaches application `0x1c0ee8`. The formerly unsupported
instruction at `0x1c0eed` is documented Type9a:
`IF TF JUMP(PC,+7)(DB), R2=R2+1`. Its two delay slots at `0x1c0ef0` and
`0x1c0ef3` converge with the false path at `0x1c0ef4`. The following
conditional Type2a at `0x1c0ef7` is documented `IF NOT AV R7=SAT MRF
(MOD2)`; because the tracer does not model the full multiplier accumulator,
it preserves the result as unknown rather than fabricating saturation.

A target-guided calibrated callback replay now crosses both documented forms
and stops before `0x1c0efa` after 18 instructions and three loaded calls.
Bytes `00 00` at `0x1c0efa` and `06 00` at `0x1c0efb` remain the provisional,
firmware-only `Type21p_undoc16` form: no public source gives them semantics,
so they remain a hard boundary and the replay does not reach `0x1c76e5`.
New repeatable `--break-pc`/`--watch-dm` snapshots record the register file
and selected DM words before a target instruction. Artifacts:
`out/experiments/sharc-runtime-probe/callback-break-c0eed-001-summary.json`,
`callback-break-c0efa-001-summary.json`, and
`callback-target-c76e5-001-summary.json`. These runs seed the callback frame,
modifiers and circular state, so they are exploratory and do not qualify A2;
their `I6/I7` values and zero candidate-buffer reads are not runtime ownership
evidence. A qualifying run still needs the reader observation at
`0x1c76e5`; an additional breakpoint at `0xb8615d` can attest which task
allocation preceded it but does not itself prove the later frame identity.
**[V][O]**

### The per-track TX frame map **[D]**

The frame is 2050 bytes at `0x80005348`, sent by
`FUN_400cd2bc(0x802, 0x80005348, 0xabc, 0x8000488c)`. Sixteen tracks; each
field is a big-endian word at `offset + 2i`. Traced from the handler's
disassembly. Only `0x94` and `0x73c` are confirmed by measurement.

| TX offset | source                                                                          |
| --------- | ------------------------------------------------------------------------------- |
| `0x02`    | low word of `*(long *)(0x800047fc + 4i) >> 8`                                   |
| `0x34`    | word at `0x800047dc + 2i`                                                       |
| `0x54`    | sign-extended byte at `0x80003cd0 + i*0x9a + 2`                                 |
| `0x74`    | word at `0x80005b50 + 2i`                                                       |
| `0x94`    | sign-extended byte at `0x80003cd0 + i*0x9a + 0` -- **the machine type** **[V]** |
| `0xb4`    | sign-extended byte at `0x80003cd0 + i*0x9a + 1`                                 |
| `0x73c`   | `0` if the type is 0 and the word at `src + 0x60` is 0, else `1` **[V]**        |
| `0x75c`   | constant `0`                                                                    |
| `0x77c`   | low word of `*(long *)(0x47db41d0 + i*0x14 + 8)`                                |
| `0x79c`   | high word of the same long                                                      |
| `0x7bc`   | word at `0x47db41d0 + i*0x14 + 0x12`                                            |

There is also a per-track `0x60`-stride sub-block: when the type is 6, slice
boundaries go to `0xee`, `0xf0`, `0xf2`, `0xf4`, `0xf6` and `0xf8`, each
`+ i*0x60`. That type-6 test is at `0x4002ec18` (`moveq #6,D1`) and reads the
same SRAM byte. **[D]**

The single `FUN_400cd2bc` call is at `0x4002dd74`, at the **top** of the
handler, before the per-track loop that fills the buffer. Each firing sends
the frame built by the previous firing and then rebuilds it: a
one-cycle-delayed double buffer, not build-then-send. **[D]**

### Corrections to the SRAM layout **[C][O]**

- The `0x9a`-stride per-track table is at **`0x80003cd0`**, not `0x80003340`.
  The handler addresses a row as `0x80003340 + i*0x9a` plus a `+0x990`
  displacement, and `0x80003340 + 0x990 = 0x80003cd0`; the base register, not
  the table, sits at `0x80003340`. `tools/framelink.py`'s `TABLES` entry
  `(0x80003340, 0x9a, 16, 'track_9a')` therefore named the wrong 2,464 bytes:
  the rows run `0x80003cd0`-`0x80004670`; the tool now uses the corrected base.
  **[C][O]**
- `FUN_4002d438`'s `0x8e`-byte copy goes to `0x80003362 + track*0x8e`, from
  `src + 0x14`. Whether that is the same structure as the
  `0x80003340 + i*0x8e` rows `FUN_400d92a2` reads -- the two bases are `0x22`
  apart with equal stride -- is unresolved. **[O]**
- `FUN_400d92a2` has two loops, and only the second writes
  `0x80005b50 + i*0x8e`: a verbatim copy of three longs from
  `0x80003340 + i*0x8e`, at `+0x2a`, `+0x3a` and `+0x4a`. The EMAC-saturating
  loop reads `0x8000dd40` and writes the adjacent `0x80005b4c`, a different
  region. **[D][C]**
- The type == 4 or 6 compare that gates `FUN_400d907e` is at `0x4002e5f8`, not
  at `0x4002eb4a`. Both read the same byte; `0x4002eb4a` feeds the `0x94` and
  `0x73c` frame fields. **[C]**
- That branch is gated on `*(long *)(local_68 + 0x64) > 0` (`ble.b` at
  `0x4002e5e4`), a slice count carried in the machine-set message. On a stock
  boot with no sliced sample the branch is never taken: confirmed in the
  emulator, where `0x800033a0` was never written in any run, patched or not.
  Testing it needs a track carrying a sample with slices. **[V][O]**
- The permission mask table is 7 longs of which only the high word is read
  (`mvz.w (0,A0,D0*4),D0`), and every high word is `0xffff`: `0x401fbda6` on
  1.15C and `0x4020ecbe` on 1.16, byte-for-byte identical. An earlier reading
  of that table as consecutive small integers came from using load base
  `0x40000000` instead of `0x40000400`, and is withdrawn. Both images load at
  `0x40000400`. **[V][C]**

### The SRAM row refreshes only when the track's source pointer changes **[V]**

Step 1a of the 2026-09-16 handover asked whether a machine commit reaches
`FUN_4002d438` or whether the row only refreshes on a pattern load. The front
half of the chain is now mapped. Three agents read it independently, by
decompile, by disassembly and by a raw-image scan, and agree.

`FUN_4002d438(src, track)` is a wholesale resync, not an incremental one, and
it caches the pointer it last synced from: **[V]**

```
*(int *)(&DAT_80003340 + (track + 0x4f3) * 4) = src;          /* 0x4002d47a */
FUN_401360ac(track * 0x8e + -0x7fffcc9e, src + 0x14, 0x8e);   /* the 0x8e mirror */
FUN_401360ac(&DAT_80003cd0 + track * 0x9a, src + 0xa2, 0x9a); /* the DSP row */
FUN_400d9000();
```

`0x80003340 + 0x4f3*4 = 0x8000470c`, so the cache is sixteen longs at
`0x8000470c`-`0x8000474b`. The compiler materialises that address directly, as
`lea (-0x7fffb8f4).l`, in the two places that clear the whole array, which
confirms the arithmetic independently of the decompiler. **[V]**

The vector-191 handler calls `FUN_4002d438` only on a cache miss, and takes a
different path on a hit (`decomp/4002dd0c_vector_191_handler.c`, around
`0x4002e50c`-`0x4002e574`): **[V]**

```
if (iVar4 != *(int *)(&DAT_80003340 + (uVar17 + 0x4f3) * 4)) {
LAB_4002e572:
    FUN_4002d438(iVar4,uVar17);           /* pointer identity changed */
    goto LAB_4002e578;
}
if (*(int *)(&DAT_80003340 + (uVar17 + 0x4f3) * 4) == 0) {
    iVar4 = _DAT_80004704 + uVar17 * 0x450 + 0x34;
    goto LAB_4002e572;                    /* slot zeroed: forced refresh */
}
FUN_400d90ac(uVar17,&DAT_80003340,*(int *)(&DAT_80003340 + (uVar17 + 0x4f3) * 4));
```

A zero in a slot is therefore an **invalidate**: it forces a full row refresh
on the next frame.

The steady-state path `FUN_400d90ac` cannot change the machine type. It walks a
per-track dirty bitmask at `0x47db461c + track*0xc` (three words, up to 96
bits) and, for each set bit, copies one short from `src + 0x14 + idx*2` into
the `0x8e` mirror at `0x80003362 + track*0x8e` and into a DSP fixed-point table
at `0x8000dd40`. It never addresses `0x80003cd0` and never reads `src + 0xa2`:
its whole domain is source offsets `0x14`-`0xa1`, and the machine type is at
`0xa2`. Its dirty bits are set by `FUN_400d9204`, which the same handler calls
twice per interrupt when a queued parameter-edit event is present. **[V]**

Every writer of the cache, found three ways -- Ghidra `data_refs`, a `disasm/`
grep for both `8000 470c` and the `addi.l #0x4f3` displacement, and
`tools/refscan.py` over the raw image at 96.78% coverage. Only six functions in
the image hard-code the `0x4f3` displacement. **[V]**

| address      | function              | writes                                                                            |
| ------------ | --------------------- | --------------------------------------------------------------------------------- |
| `0x4002d47a` | `FUN_4002d438`        | the pointer                                                                       |
| `0x4002e052` | vector-191 handler    | zero, all 16 slots, when the live track base `_DAT_80004704` changed              |
| `0x4002e516` | vector-191 handler    | zero, one slot, after a refresh from an explicit override pointer                 |
| `0x4002e640` | vector-191 handler    | zero, one slot, in the slice branch                                               |
| `0x4002da8a` | `FUN_4002da7a`        | zero, all 16 slots, unconditional on entry, right after `_DAT_80004704 = param_1` |
| `0x4002da48` | `FUN_4002da38(track)` | zero, one slot, unconditional -- the invalidate helper                            |
| `0x4002d7fa` | `FUN_4002d7a4`        | zero, one slot, only when the cached pointer already differs from the live one    |

`FUN_4002d7a4(value, track, index)` is the per-parameter apply: it writes one
short into the `0x8e` mirror, and its callers are `SoundParameterSet::vfunc_13`,
`SoundParameterSet::vfunc_31`, `FxParameterSet::vfunc_13` and
`FxParameterSet::vfunc_31`. Its invalidate is conditional on a pointer mismatch
the handler would catch on its own, so it is a consistency fixup, not a
machine-type path. **[V]**

`FUN_4002da38` is the unconditional invalidate, and Ghidra lists **no** caller
for it. That is an artifact: `tools/refscan.py` finds two real
`jsr $4002da38.l`, at `0x4004319c` and `0x400431d4`, with the bytes
`4eb9 4002da38` at both. They lie in `0x40042fe2`-`0x4004335c`, a range no
entry in `function_ranges` covers, so Ghidra never built a function there and
its call table is silently empty -- a textbook case of the rule that an empty
Ghidra caller list is not evidence. **[V][C]**

### The notification dispatcher at `0x40042fe2` **[V][O]**

`0x40042fe2` is a real entry: it opens with the movem prologue
`lea.l -$18(a7),a7` / `movem.l d2-d3/a2-a5,(a7)`, and Ghidra knows the address
only as `LAB_40042fe2`. It is a `DataChangeInfo` notification dispatcher in the
same style as `FUN_40042eaa`, running its third argument through five RTTI
checks in order, each one a
`FUN_401e2d66(info, &DataChangeInfo::typeinfo, &X::typeinfo, 0)`: **[V]**

| check | at           | class                            | typeinfo     |
| ----- | ------------ | -------------------------------- | ------------ |
| 1     | `0x40042ffc` | `MultipleSoundParamsChangedInfo` | `0x401f2970` |
| 2     | `0x40043050` | `SoundParamChangedInfo`          | `0x401f2964` |
| 3     | `0x400430fe` | `SoundConfigChangedInfo`         | `0x401f3fdc` |
| 4     | `0x40043132` | `SoundSlicesChangedInfo`         | `0x401f3fe8` |
| 5     | `0x4004316c` | `SoundConfigPlayModeChangedInfo` | `0x401f3ff4` |

The two invalidate calls sit on: **[V]**

- `0x4004319c`, reached when check 5 matches `SoundConfigPlayModeChangedInfo`;
- `0x400431d4` at `LAB_400431d0`, the fallback, reached either when the info
  pointer is null (`beq.w $400431d0` at `0x40042ff8`) or when all five checks
  fail.

`SoundParamChangedInfo`, check 2, takes its own branch and reaches neither. It
calls `FUN_4002d7a4` per field, then `FUN_40035cca` twice, `FUN_400506ac` /
`FUN_4005063e` and `FUN_400da23e`, sends message code 5 through
`FUN_400d3418`, and exits. **[V]**

The dispatcher is registered, not virtual: `vtables` has no target in
`0x40042eaa`-`0x4004335c`. Instead `FUN_40044852` writes `FUN_40042eaa` and
`0x40042fe2` into the same record, at record offsets `0x54` and `0x5c`, sixteen
times in a loop over keys 0-15 (`0x40044ad6`-`0x40044b50`), and `FUN_40044e28`
references `0x40042fe2` twice more. It is installed deliberately, alongside a
dispatcher already known to be live. **[V]**

### The real panel setter reaches the unconditional invalidate **[V]**

`FUN_40051712` is the in-place machine-type setter. It writes the new value to
`+0xa2` of the track object through the accessor at vtable `+0x28` and, on a
change, constructs a `SoundParamChangedInfo` and calls the notify vfunc at
`+0x10`: **[V]**

```
400517b0  jsr $40050b14(pc)    ; SoundParamChangedInfo::ctor_dtor(obj, 1, old, 0)
400517b4  movea.l (a2),a0
400517b6  clr.l -(a7)          ; push 0
400517b8  move.l a2,-(a7)      ; push obj
400517ba  movea.l $10(a0),a0   ; the notify vfunc
400517be  jsr (a0)             ; vfunc(obj, 0)
```

The concrete class and the fate of that literal are now resolved from the
qualified 1.16 runtime object and real panel replay. In the immutable parent
checkpoint, setter object `0x44fb9da0` has vtable `0x401f53d4`; slot `+0x10`
is `0x401aa95c`,
`ValueWithMirror<Digisharc::sound_struct,...>::vfunc_4`. Its mirror callback
slot `+0x44` is `0x40051aa4`, `Sound::updateMirror`, and its base
`Value<...>::vfunc_4` observer record names callback `0x40042fe2`. The literal
zero remains the callback's third argument. The null test at `0x40042ff8`
therefore takes `LAB_400431d0` and calls the unconditional per-track
invalidate `FUN_4002da38` at `0x400431d4`. **[V]**

The exact qualification is
`out/experiments/a2-notification-invalidation/qualify-001/report.json`. Two
clean and two narrow-traced runs restore the same checkpoint, use the same
64M-instruction real-panel gesture with `--exact --no-unblock`, touch no fault
pages, and finish with byte-identical panel and snapshot hashes. Each traced
run records one commit, one setter and the following live sequence:

```
0x40051712  setter, object 0x44fb9da0, new type 2
0x401aa95c  ValueWithMirror notify, info 0
0x40051aa4  Sound::updateMirror, info 0
0x40042fe2  registered dispatcher, source 0x426532ec, info 0
0x4002da38  unconditional invalidate, track 0
```

The parent snapshot contains `0x426532ec` at cache slot `0x8000470c`; the
panel run writes source byte `0x4265338e` from 0 to 2 and leaves that slot
zero. An independent check re-read the instruction bytes from MAIN OS using
its correct `0x40000400` load base and decoded the parent snapshot page before
this result was marked verified. **[V]**

The one setter invocation occurs in a burst with eight downstream
notify/update/dispatch/invalidate hits. That count is observer activity, not
eight setters or eight panel events. The bounded run still records zero hits
at `FUN_4002d438` and zero writes to row byte `0x80003cd0`: invalidation is
now proven, but invalidation alone does not make the queue-draining vector-191
handler refresh a row. Natural vector-191 delivery remains blocked on the
external SSI RX synchronization and cadence described below. **[V][O]**

This constrains the design. The DSP row is refreshed only
wholesale, only from `src + 0xa2`, and only on a cache miss or an invalidate;
nothing incremental writes it. So type substitution (handover step 3) has
exactly one place to act: `FUN_4002d438`'s copy, or the byte that copy
reads. **[V]**

### The frame handler drains a queue; it does not sweep sixteen tracks **[V][C]**

This corrects the reading above, and it corrects the premise of handover step
1a. The vector-191 handler does **not** walk tracks 0-15 refreshing rows. It
drains a linked list of change records and touches only the tracks those
records name. Found after a headless run of `tools/machinecommit.py` on
`out/snapshots/dt2-1.16/boot400M.snap` reached `FUN_4002d438` zero times in
three conditions, including one that zeroed the sync-cache slot on purpose.
**[V][C]**

The queue module is `0x4013a3c4`-`0x4013a7f4`: **[V]**

| function                        | role                                                                                |
| ------------------------------- | ----------------------------------------------------------------------------------- |
| `FUN_4013a408`                  | init: zeroes the head, lays the two static pools out as free lists                  |
| `FUN_4013a3f4`                  | `return _DAT_44e6b488` -- peek the outer head                                       |
| `FUN_4013a3fc`                  | `_DAT_44e6b488 = p` -- pop / advance                                                |
| `FUN_4013a3c4`                  | outer-node alloc, pops `_DAT_44e6b490`                                              |
| `FUN_4013a52a`                  | record alloc, pops `_DAT_44e6b494`, zeroes words `[0]`, `[0xf]`, `[0x10]`, `[0x15]` |
| `FUN_4013a560` / `FUN_4013a5d8` | free a record / an outer node                                                       |
| `FUN_4013a6b0(rec, key)`        | enqueue, sorted into a bucket by `key`                                              |
| `FUN_4013a78a(rec)`             | enqueue onto the front bucket (tag `[0] == 1`)                                      |

Outer nodes chain through `+0x10`; each node's `+8` is the head of its record
list, and records chain through `+0x68`. The handler's `local_68` is a record
pointer walked down that inner list, not a stack buffer. **[V]**

A record reaches the per-track apply path only if its tag `[0]` is outside
`{2,3,4,5,6,7,8}`, `[1] == 1`, `[4]` (the track) is not `0x10`,
`DAT_47db4310[track] <= [5]`, the byte selected by `[7]`/`[8]` is
non-negative, and `([0xe] & 0x81) != 1`. Then: **[V]**

- `[0x10]` non-zero is passed straight to `FUN_4002d438([0x10], track)`, and
  the sync-cache slot is zeroed afterwards at `0x4002e516`;
- `[0x10]` zero falls back to `[0xf]`, and `[0xf]` zero falls back to the live
  track object `_DAT_80004704 + track*0x450 + 0x34`.

So `[0xf]` and `[0x10]` are per-record **source-object overrides**. That is the
shape of a sound lock, and it explains why the row is refreshed wholesale from
`src + 0xa2` rather than incrementally: each record can name a different source
object for the same track.

The producer is `FUN_40139878`, called only from `FUN_4011fe12`. It fills a
template in a static per-(track, slot) array, allocates a record, memcpys
`0x6c` bytes over it and enqueues it: **[V]**

```
iVar3 = param_1[0xc];
puVar7[0xf] = 0;
puVar7[0x10] = iVar3;          /* the source-object override */
...
iVar4 = FUN_4013a52a();                        /* alloc */
FUN_401360ac(iVar4, iVar9 + iVar6 + 0x414, 0x6c);  /* template -> record */
FUN_4013a78a();                                /* enqueue */
```

In `FUN_4011fe12` the value that becomes `[0x10]` is `local_14`, and it is set
three ways: zero; a cached pointer (`_DAT_44e08970`, else
`DAT_44e08930[track]`); or, when its sixth argument is below `0x80`, a direct
`param_6 * 0x450 + 0x4291377a`. Seven of the eight call sites pass the sentinel
`0xffffffff` and take the cached path. **[V]**

The eight callers are UI and playback sites -- `PatternGridView::vfunc_2`,
`TrackSwapMenuView::vfunc_2`, `KeyboardView::vfunc_2` (through
`FUN_4005c5e8`), `SoundManager::vfunc_32`, `KitActiveSettingsChangedInfo::
ctor_dtor` at `0x400d4460`, and three more. **[D]**

That the queue is therefore the trig path -- that a track's machine type
reaches the DSP when the track next sounds a note, carrying whatever source
object the trig resolves to -- is the natural reading of those callers and of
the per-record override, but it is inference from names and shape, not from a
trace. **[O]**

### Why the first headless run said nothing **[V]**

`tools/machinecommit.py` resumes the snapshot three times and, per track,
compares the object's type byte, the SRAM row byte and TX frame offset
`0x94 + 2i` across three conditions: nothing poked; the track object's `+0xa2`
poked; and that poke plus the sync-cache slot zeroed. On
`out/snapshots/dt2-1.16/boot400M.snap`, track 0, type 5:

| condition | obj type | row type | frame `0x94` | hooks hit |
| --------- | -------- | -------- | ------------ | --------- |
| base      | 0 -> 0   | 0 -> 0   | 0, 0         | none      |
| inplace   | 0 -> 5   | 0 -> 0   | 0, 0         | none      |
| invalid   | 0 -> 5   | 0 -> 0   | 0, 0         | none      |

Every pass reported `returned` and a 2050-byte frame, so the handler ran to
completion; the pokes landed, so the harness wrote what it meant to. But
`FUN_4002d438`, `FUN_4002da38`, `FUN_4002d7a4` and the dispatcher at
`0x40042fe2` were reached zero times **in the control as well**, so the run
does not show the mechanism working at all and says nothing about the
question. The tool prints `INCONCLUSIVE` for exactly this case. **[V]**

The cause is the correction above: with the queue empty on a stock resumed
boot, the handler's outer `while` never executes, and nothing downstream of it
can run however the cache slot is set. A sweep-based reading of the handler
would have called this run a clean negative result. **[C]**

### The smallest qualified 1.15C panel gesture commits STRETCH **[V]**

`out/experiments/panel-machine-commit/qualify-1.15c-001/report.json` records
two clean and two narrowly traced repetitions of the same raw panel sequence:
FUNC+SRC opens Machine Selection, one DOWN tap selects the next entry, and YES
commits it. FUNC uses button channel 2 (`22xx`); DOWN and YES use channel 1
(`21xx`). Requested feed counts are deterministic lower bounds serviced at the
next 400,000-instruction loop boundary, not exact delivery counts. **[V]**

All four runs exited zero, touched no fault page, and produced the same
1024-byte panel (`8ba3d66b…`) and endpoint snapshot (`b467d62f…`). Each traced
run entered the commit setter `0x40035e90` exactly once with track 0 and type
2, then wrote `0x4263b1a2` from PC `0x40050d62`, `00 -> 02`. The final panel
highlights `STRETCH`. This qualifies the gesture and its real UART8/eDMA panel
path on 1.15C. It does not complete behavioral A2: these runs used the
historical global-unblock mode, and the target setter/back half is the
relocated 1.16 build. **[V]**

### Faithful 1.16 panel input reaches the relocated setter, not refresh **[V]**

`out/experiments/panel-machine-commit/qualify-1.16-001/report.json` repeats
the same FUNC+SRC, DOWN, YES sequence twice clean and twice with narrow hooks
from one immutable 1.16 user-screen snapshot. Its retained command provenance
uses `--exact --no-unblock`, never enables `--weakptr`, and the source snapshot
has a wholly counted three-stage lineage from `boot400M.snap` through
`explore-1.16-pit3-004`, `-005`, and `-013`. All lineage stages and all four
qualification runs touched zero fault pages. **[V]**

The port required three hardware/model corrections rather than generic wait
satisfaction: the image-resolved UART8 TX state is `0x40964d74`; PIT3 alone
runs during the intro while DTIM remains held; and eSDHC CMD18/CMD25 transfers
now complete channel 59, post its resolved completion semaphore, and preserve
the card's sparse write overlay in checkpoints. The final user screen is
therefore reached with `unblock=False` and `weakptr=False`. **[V]**

All four qualification runs delivered the same eight feeds at the same actual
lower bounds, latched the panel at `53,047,610`, and saved at `80,004,410`.
Their 1024-byte panel hash is `8ba3d66b…`, and their endpoint snapshot hash is
`4327aebe…`; clean and traced endpoints are byte-identical. Each traced run
entered commit `0x40036798` and setter `0x40051712` exactly once, with track 0
and type 2, then wrote `0x4265338e`, `00 -> 02`, at PC `0x4005179e`. The raw
1.16 image bytes independently place the commit call into the setter and the
byte write at those addresses. **[V]**

The equivalent shorter qualification at
`out/experiments/panel-machine-commit/qualify-1.16-fast-001/report.json`
keeps the opening and DOWN timings but asks for YES at 46.4M rather than
52.4M and ends at 64M rather than 80M. Two clean and two traced exact runs
again have byte-identical endpoints, the same `8ba3d66b…` panel hash, zero
fault pages, and one type-2 commit/setter pair in each traced run. The panel
latched 5,616,000 instructions earlier and the actual final save was
15,849,359 instructions earlier. This is the preferred repeat recipe; it
shortens the scenario rather than increasing per-instruction emulator speed.
**[V]**

`FUN_4002d438` was reached zero times in both traced runs. Thus this closes the
real-panel provenance only through the setter; it does not connect the
setter's indirect notification to invalidation, the SRAM row, or the frame.
Behavioral A2 remains open. **[O]**

One follow-up control narrows the next missing event. From the committed 1.16
snapshot, a real TRIG 1 press/release in
`out/experiments/panel-machine-commit/explore-1.16-pit3-012/` reaches
`FUN_4011fe12` once and the record producer/enqueue pair twice, but still hits
`FUN_4002d438` zero times. At the time of that run SSI0-paced eDMA48/50,
vector 170, and `INTFRCH1` software-force delivery were unmodelled. The new
narrow model closes those mechanics, but without external RX data it remains
in the generic vector-170 synchronization handler and still does not provide
a natural refresh. This run identifies only the queue side dynamically.
**[C][D][O]**

An earlier proposed front-half control was to poke a type byte into a source
object and call `FUN_4011fe12` through
`emu/harness.py`'s
`call()` with a sixth argument below `0x80` so the record carries `[0x10]`
directly, then raise vector 191 and read the row and the frame. Confirm what
`0x4291377a + idx*0x450` actually is first -- the base is odd-aligned, which
is not the shape of a `0x450`-stride object array, so it may be a decompiler
artifact rather than a real address. The real TRIG path now reaches the
producer, so this is optional calibration, not the next behavioral-A2 test.
**[C][O]**

### Direct-refresh control establishes source byte -> SRAM row -> TX frame **[V]**

`tools/machinecommit.py` now tests the narrow back half directly on Digitakt
II 1.16. It does **not** pretend that the unresolved setter notification fired:
each condition starts from a fresh restore, optionally changes the live source
object's type byte, optionally calls `FUN_4002d438(src, track)` through
`emu/harness.py`, then enters vector 191 three times. The final accepted report
is `out/experiments/a2-machine-provenance/a2-real-005/report.json`. **[V]**

This is calibration/control evidence for behavioral A2, not completion of A2.
The run used track 0 and source object `0x426532ec`; the selected addresses were
`src + 0xa2`, row `0x80003cd0`, and cache slot `0x8000470c`. Its immutable
inputs were: **[V]**

- SysEx SHA-256 `278541e466edcd77d6b3e018a91fb90185932d3c7de224dd3e68294dddf3a9ec`,
  equal to `out/sections/dt2-1.16/.source-sha256`;
- MAIN OS SHA-256 `57bb4dfa8df07d846adc72fdb4fb0d3cd3c5680c524bf498338460207e008e7d`;
- snapshot SHA-256 `c23b733dfb11eb0239e44b517102c9719d966e5cd344fa3363c9a16af920a9b2`.

The build contract was `unblock=False`, `weakptr=False`, no GUI fast mode,
and state-only `softfloat=True`, `bitmap=True`, `dsp=True`. Four conditions
were each repeated clean and with the bounded refresh-entry/full-row-write
observers. Every handler entry returned, every captured frame was 2050 bytes,
and clean/instrumented state, row hashes, frame hashes and stop reasons were
identical in all four pairs. **[V]**

| condition         | source type after poke | direct refresh | row type after | frame type words |
| ----------------- | ---------------------: | -------------: | -------------: | ---------------- |
| baseline          |                      0 |             no |              0 | `0, 0, 0`        |
| source only       |                      5 |             no |              0 | `0, 0, 0`        |
| unchanged refresh |                      0 |            yes |              0 | `0, 0, 0`        |
| changed refresh   |                      5 |            yes |              5 | `0, 5, 5`        |

In each instrumented refresh condition the entry hook fired exactly once at
`0x4002d438`, with stacked arguments `[0x426532ec, 0]`. The row observer saw
40 writes; it retained the first 32 by design and reported truncation rather
than silently losing the count. The complete row after-images provide the
discriminator: changed refresh versus unchanged refresh differs at exactly
row offset 0, `00 -> 05`. The corresponding complete-frame comparison differs
nowhere in pass 0 and, in passes 1 and 2, at exactly `0x95: 00 -> 05` and
`0x73d: 00 -> 01`. Thus the big-endian word at `0x94` becomes `0x0005`; the
`0,5,5` sequence is a measured one-cycle delay, not a claim about local static
send/rebuild ordering. **[V]**

The unchanged-refresh control also corrects an over-strong first acceptance
gate. A wholesale refresh legitimately changes other stale row bytes, so its
whole frame need not equal a no-refresh baseline. The discriminating A/B is
changed refresh versus unchanged refresh: those conditions execute the same
copy and differ in only `src + 0xa2`. `a2-real-001` was rejected solely by the
old whole-frame-equality gate; `a2-real-005` uses the corrected controlled
comparison and fails closed on duplicate/missing conditions, address drift,
input-state drift, inactive hooks or clean/instrumented disagreement. **[V]**

This establishes only the direct 1.16 chain
`source + 0xa2 -> FUN_4002d438 -> row byte 0 -> TX 0x94`. It does not establish
that `FUN_40051712`'s indirect notify vfunc invalidates the cache or schedules
that refresh, and it does not replace a future faithful 1.16 UI commit trace.
That front-half link remains open. **[O]**

## The frame link on Digitakt II 1.16 **[V][O]**

- Every 1.15C address of the frame link has a 1.16 counterpart, found from
  the Version Tracking map and checked instruction by instruction in both
  images:

  | 1.15C                           | 1.16           | evidence                                                                    |
  | ------------------------------- | -------------- | --------------------------------------------------------------------------- |
  | installer `FUN_4002ce4a`        | `FUN_4002d4f2` | `move.l #handler,dN; move.l dN,$400002fc` and `move.b #5,$fc04c07f` in both |
  | vector-191 handler `0x4002d652` | `0x4002dd0c`   |                                                                             |
  | driver call at `0x4002d6ba`     | `0x4002dd74`   | after `pea $8000488c`, `pea $abc`, `pea $80005348`, `pea $802` in both      |
  | DSPI2 driver `FUN_400cf9c4`     | `FUN_400cd2bc` |                                                                             |
  | pacing counter `0x4028ac90`     | `0x402a1488`   | one read and one write each, in the handler                                 |
  | gate `0x4094e4f4`               | `0x409664f4`   | 7 references each                                                           |
  | countdown `0x4094e4f0`          | `0x409664f0`   | 4 each                                                                      |
  | mode `0x4094e4f8`               | `0x409664f8`   | 7 each                                                                      |
  | stop flag `0x4094e4ec`          | `0x409664ec`   | 2 each: the writer, and `tst.l` at `0x4002d6ce` / `0x4002dd88`              |
  | stop-flag writer `0x4002d632`   | `0x4002dce2`   | `moveq #1,d0; move.l d0,stop; rts`, outside any function                    |
  | `FUN_4002d602`                  | `FUN_4002dcb2` | nine sites each, below                                                      |
  | MIDI flag writer `FUN_4002ed16` | `FUN_4002f3da` | `lea $80003340,a0`, indices `0x4d1` and `0x4e1`                             |
  | `FUN_400db9aa`                  | `FUN_400d92a2` | `lea $80005b50,a2`, row step `0x8e`                                         |

  The gate variables moved by `+0x18000`, and their references are the
  same instructions in the same order. The frame tables did not move: TX
  `0x80005348` (`0x802`), RX `0x8000488c` (`0xabc`), `0x800047fc + 4*i`,
  `0x80003340 + i*0x9a`, `0x80005b50 + i*0x8e`, and the MIDI flags
  `0x80004684 + 4*i` and `0x800046c4 + 4*i`, each with 16 rows. The
  instruction pattern of the stop-flag writer occurs 9 times in each image;
  the one above is the one that writes the stop flag. **[V]**

- The nine sites of `FUN_4002d602`, with the argument pushed before each:

  | 1.15C        | 1.16         | in                                   | argument |
  | ------------ | ------------ | ------------------------------------ | -------- |
  | `0x400323dc` | `0x40032c3c` | trampoline outside functions         | 1        |
  | `0x400323f2` | `0x40032c52` | `FUN_400323e2` / `FUN_40032c42`      | 0        |
  | `0x400330f6` | `0x4003395c` | task `FUN_40032f5a` / `FUN_400337ba` | 0        |
  | `0x400407d8` | `0x400410f8` | trampoline outside functions         | 0        |
  | `0x400407ee` | `0x4004110e` | trampoline outside functions         | 0        |
  | `0x40043692` | `0x40043fb2` | `OnScopeExit::ctor_dtor`             | 1        |
  | `0x40046008` | `0x4004694a` | `OnScopeExit::ctor_dtor`             | 1        |
  | `0x400fa6d8` | `0x40106c9c` | trampoline outside functions         | 0        |
  | `0x400faa98` | `0x4010705c` | `FUN_400faa86` / `FUN_4010704a`      | 1        |

  **[V]**

- Two things differ. The 1.16 installer also writes the frame's first word,
  `moveq #1,d0` then `move.w d0,$80005348`; the 1.15C installer does not.
  And the handler reads both MIDI flag arrays itself, `lea $80004684,a1` and
  `lea $800046c4,a0`, at `0x4002d70c` and `0x4002d720` in 1.15C and at
  `0x4002ddc6` and `0x4002ddda` in 1.16. **[V]**
- `tools/framelink.py` holds these addresses per image, looked up by the
  SHA-256 of the MAIN OS image. `tools/sharcframe.py` takes its addresses
  from it and stops on an image without a profile; `--open-gate` writes 0 to
  the profile's gate. On `snapshots/boot400M.snap` (1.15C) with three passes,
  the tool before the change with `--poke 0x4094e4f4=0`, and the new tool
  with `--open-gate` or with the same poke, capture the same frames: pass 0
  `382e008e…`, passes 1 and 2 `5cc9772c…`. **[V]**
- `tools/dspmap.py` lists the instructions that reference the frame tables,
  the gate variables and the pacing counter, with their functions and callers
  two levels up (`out/maps/`). It finds 186 sites in 19 functions on 1.15C
  and 187 in 20 on 1.16; the only new site is the installer's write. Its 115
  sites from the raw sweep on 1.16 match an independent sweep. Two sites are
  not table accesses: the DSPI2 driver's `move.w a0,(a1,d0.l*4)`, which
  Ghidra attributes to `0x80003340` while `a1` holds `0x80001bc0`, and
  `cmpa.l #$800047fc,a2` in `FUN_400dbaac` / `FUN_400d93a4`, the end of a
  loop over the 16 words at `0x800047dc`. Of the other 10 functions that use
  `0x80003340`, 8 load the base and index it by track; `FUN_4002cd38` and
  `FUN_4002d166` use fixed rows 14 and 15. **[V]**
- None of the eight Digitakt `*::updateMirror` methods reaches these
  functions within three callee levels of Ghidra's call graph, including its
  resolved indirect calls, and none of those callees references the tables.
  A method can still hand the handler data through shared state, which a
  call graph does not show. **[V][O]**

## The frame capture runs; the frame build is switched off **[V][D][O][C]**

- `tools/sharcframe.py snapshots/boot400M.snap --passes 3` takes 0.5 s.
  Vector 191 holds `0x4002d652`. Each pass returns through the handler's
  `rte` and calls `FUN_400cf9c4(0x802, 0x80005348, 0xabc, 0x8000488c)` once,
  from `0x4002d6c0`. All 2050 TX bytes are zero in every pass. **[V]**
- The handler builds the frame only when the long at `0x4094e4f4` is 0
  (test at `0x4002d91a`). In `boot400M.snap` it is 1, `0x4094e4f8` is 1,
  and the per-track tables are zero. `FUN_4002d5b2` writes exactly those
  two values and is called from the SHARC boot routine `FUN_400cef6c`.
  **[V]**
- `FUN_4002d5c4` clears `0x4094e4f4`; its only caller is the console
  command parser `FUN_400cd594` on `#EXIT_TEST_MODE` (`0x400cda98`). **[V]**
- `FUN_4002d602(n)` has two branches. With n != 0 it writes 2 to
  `0x4094e4f8` and 5 to `0x4094e4f0`; the handler counts `0x4094e4f0` down
  once per pass (`0x4002ecaa`-`0x4002ecbe`) and writes 1 to `0x4094e4f4`
  when it reaches 0. With n == 0 it clears `0x4094e4f4` and `0x4094e4f0`
  with interrupts masked (`0x4002d61c`-`0x4002d630`). **[V]** An earlier
  reading here, that only `FUN_4002d5c4` clears the gate, was wrong. **[C]**
- `tools/refscan.py` finds nine call and jump sites to `FUN_4002d602`
  (96.75% of the image decoded); the argument is the last value pushed.
  Argument 1, stop after five passes: `0x40043692` and `0x40046008` in the
  two `OnScopeExit::ctor_dtor` functions (`0x40043654`, `0x40045fc4`), and
  `0x400faa98` in `FUN_400faa86` (`Waiting for SysEx`) **[V]**; and the
  trampoline `0x400323d6` **[D]**. Argument 0, open now: `0x400330f6` in
  `FUN_40032f5a` and `0x400323f2` in `FUN_400323e2` **[V]**; and the
  trampolines `0x400407c8`, `0x400407de` and `0x400fa6c2` **[D]**. Ghidra's
  call graph lists five of the nine: the trampolines are not in functions.
- The two `OnScopeExit` functions load `0x400407c8` and `0x400407de` as
  pointers (`0x400436b8`, `0x4004602e`) next to their argument-1 call, and
  `OsUpgradeMenuView::ctor_dtor` loads `0x400faa86` and `0x400fa6c2`
  (`0x400fb07c`, `0x400fb0b2`). This suggests the frame build stops while a
  kit or project change, a sample reload or an OS upgrade runs, and a
  callback reopens it afterwards. Where the callbacks are invoked was not
  traced. **[D][O]**
- `FUN_40032f5a` is a task body: `FUN_400329ee` pushes `$40032f5a(pc)` at
  `0x40032a16` and calls `0x400012c8` with a `0x28000` size and 6. **[V]**
  It references `Factory reset`, `Migrate presets`, `Update MMC Caches`,
  `MAINTENANCE MODE` and `MMC NOT IN SLC MODE`. Its `FUN_4002d602(0)` at
  `0x400330f6` follows the `Update MMC Caches` step with no branch in
  between. **[D]** It is the only argument-0 call that is not a callback, so
  it is the best candidate for opening the gate in normal use. Whether a
  normal boot runs it is not known. **[O]**
- `0x4094e4ec` is a stop flag: when it is non-zero, the handler writes 1 to
  `0x4094e4f4` and `0x4094e4f8` (`0x4002d6ce`-`0x4002d6de`). Its only
  writer, `0x4002d632` (`moveq #1,d0; move.l d0,$4094e4ec.l; rts`), has no
  static caller and no copy of its address in the image, and is not in a
  Ghidra function. **[V]**
- In `boot60M`, `boot120M` and `boot200M.snap`, `0x4094e4ec`, `0x4094e4f0`,
  `0x4094e4f4` and `0x4094e4f8` are all 0. In `boot280M` and
  `boot400M.snap`, `0x4094e4f4` and `0x4094e4f8` are 1 and the other two
  are 0. **[V]** `FUN_4002d5b2` or the end of a countdown could each
  produce that. A write watch during a cold boot answers it: the SHARC boot
  routine calls `FUN_4002d5b2` at instruction 257,643,827, and nothing else
  writes 1 to either variable (see "The emulator boots Digitakt II 1.16").
  **[V]**
- Forcing `0x4094e4f4` to 0 before a pass enters the build path and ends in
  the firmware's exception dump `FUN_4010fcae` (vector 64 at PC
  `0x400db9e0`), which executes `halt`; Unicorn reports that as exception 257. **[D]** The instruction at `0x400db9e0`, in `FUN_400db9aa`, is
  `a891 00c6`, `mac.w D6u,D0u,(A1),D4,ACC0`: a MAC with load. **[V]** On
  those two words alone Unicorn 2.1.4 (CFV4E) stops with `UC_ERR_EXCEPTION`
  and leaves PC, A1 and D4 unchanged, where the CPU adds D6u\*D0u to ACC0
  and loads `(A1)` into D4. **[V]** So the exception dump came from the
  emulator, not the firmware. **[C]**
- QEMU's MAC translation, which Unicorn 2.1.4 uses, has four faults in the
  load forms: it faults when the Ry number has bit 0 or 1 set, reads a
  data-register Rx from D2, adds for MSAC, and applies MASK to every load
  address. `patches/unicorn-2.1.4-m68k-emac-mac-load.patch` fixes them, and
  `tests/test_unicorn_emac.py` checks four load forms against the CFPRM.
  **[V]** A sweep of the two code ranges finds about 160 MAC and MSAC with
  load, about 60 of them with such an Ry; the sweep may count some data.
  **[D]**
- With that patch, `tools/sharcframe.py snapshots/boot400M.snap --passes 3
--poke 0x4094e4f4=0` runs all three passes to the handler's `rte` with one
  driver call each and no exception; `0x400db9e0` runs 1836 times. **[V]**
  Pass 0 sends 2050 zero bytes. Passes 1 and 2 send the same bytes, 46 of
  them non-zero: `0002` at `+0x00`, `3840` at `+0xd8`, 16 slots of `0x60`
  bytes starting at `+0x11c`, `+0x17c`, ... `+0x6bc`, each `0200 0000 0200`
  then zeros, and near
  the end `4000 1130` at `+0x736`, `0008` at `+0x7dc`, `0012` at `+0x7e0`,
  `0002` at `+0x7e8`, `7fff ffff` at `+0x7f0` and `0001` at `+0x800`. **[V]**
- After those passes, rows 1 to 15 of `0x80005b50 + i*0x8e` are non-zero and
  each starts `02 00 00 00`; row 0 and the tables at `0x800047fc` and
  `0x80003340` stay zero. **[V]** Why pass 0 sends zeros, and what the slot
  words mean, is not known. **[O]**

## The SHARC side of the SPI frame link **[V][O]**

- `tools/sharcimm.py` lists the immediates in DSP code. It decodes one
  instruction at every even offset and marks each hit with the longest run
  of decoded instructions that ends there (depth) and with whether the
  linear sweep that steps past undecodable words reaches it. `--words` scans
  the 32-bit words of every block of a boot stream instead. Peripheral names
  are from the ADSP-2156x SHARC+ Processor Hardware Reference Rev 1.0
  (Appendix A; Table 27-2 for the DMA channels). **[D]**
- No instruction in either main program carries an SPI
  (`0x3102e000`-`0x31030fff`), SPI DMA (`0x3102d000`-`0x3102d2ff`) or SEC0
  (`0x31089000`) address. The values in `0x30000000`-`0x31ffffff` on the
  sweep in 1.16 are: 17 in the DAI0 page at SW `0x1cb28e`-`0x1cb2da` and 17
  in the DAI1 page at `0x1cb2dd`-`0x1cb31a` (the 34 SPORT/DAI setup writes;
  these pages also hold the ASRC, SPDIF and PCG registers); `14a` writes to
  PORTA+`0x30`, PORTB+`0x30`, PORTA and PORTB at SW `0x1cb25d`-`0x1cb26c`
  and to PADS0+`0x60` and +`0x64` at `0x1cb320` and `0x1cb323`; a `14a`
  write to RCU0+`0x2c` (reset control unit) at `0x1c1414`; and `17a`
  R12=`0x30c6d751` at `0x1c673e`, which is not a register. 1.15C has the
  same, `0x6c` words lower after `0x1c7781`. `14a` with d=1 writes the
  register to memory (Core Programming Reference, Type 14a). **[V]**
- `docs/sharc/structure-1.16.md` section 3b gives `0x31004000` and
  `0x3108c000` as DMA or interrupt controller candidates. They are PORTA and
  RCU0. **[C]**
- A decode at every offset finds 32 instructions with a value in
  `0x82a00000`-`0x82a001ff`. The two at SW `0x1c3806` and `0x1c4562` lie
  inside real instructions that start at `0x1c3805` and `0x1c4560`, so the
  count of 30 above stands. **[V]**
- The SPI base addresses are data. Loader block 35 (1.16: target
  `0x28269250`, 1,576 bytes; 1.15C: `0x28269240`) is not part of the main
  program. At data pointer `0x2694a0` (1.15C: `0x269490`) it holds one
  40-byte entry per SPI instance: the SPI base, the DMA TX and DMA RX channel
  bases, the SEC ids of TX DMA, RX DMA, status, error, TX DMA error and RX
  DMA error, and a zero word. The ids match the SEC table of the Hardware
  Reference (Table 6-5). Each base occurs once as a 4-aligned word in the
  stream. **[V]**

  | entry            | SPI base     | DMA TX       | DMA RX       | SEC ids                  |
  | ---------------- | ------------ | ------------ | ------------ | ------------------------ |
  | SPI0, `0x2694a0` | `0x3102e000` | `0x3102d000` | `0x3102d080` | 85, 86, 87, 88, 158, 159 |
  | SPI1, `0x2694c8` | `0x3102f000` | `0x3102d100` | `0x3102d180` | 89, 90, 91, 92, 160, 161 |
  | SPI2, `0x2694f0` | `0x31030000` | `0x3102d200` | `0x3102d280` | 69, 70, 71, 72, 156, 157 |

- The same block holds SPORT0A-SPORT7B entries from `0x26955c` (stride
  `0x28`, each with the SPORT and DMA channel base) and LP0/LP1 entries at
  `0x269468`. Block 48 holds PORTA-PORTC, PINT0-PINT2 and PADS0 register
  addresses from `0x2d6ea0`, and block 2 holds the CGU0 and CGU1 bases at
  `0x242c88` and `0x242c98`. **[D]**
- The code that receives 2748 selects the SPI2 entry. In 1.16, SW `0x1c80a0`
  is `17b` R4=`0xabc` (2748, the bytes the ColdFire receives per frame),
  then `25a_direct` to `0x1c7bd4`. That function sets R12=`0x261a10`
  (`17a`, `0x1c7c0f`) and R4=2 (`17b`, `0x1c7c12`) and jumps to `0x1c9fd5`
  (`0x1c7c14`). There: `2c` R13=R4 (`0x1c9fe7`), `17b` R2=`0x28`
  (`0x1c9fe8`), `4a` R2=R13\*R2 (`0x1c9fea`), `5a` I4=R2 (`0x1c9ff0`) and
  `19a` I1=I4+`0x2694a0` (`0x1c9ffb`), so I1=`0x2694f0`. 1.15C does the same
  from SW `0x1c8034` with R12=`0x261a00`, `0x1c9f69` and the table at
  `0x269490`. `0x1c9fd5` is also reached from `0x1c7dae` with the index in
  M6 and R12=`0x261b18`, so it serves any instance. **[V]**
- What happens to 2748 after the call is not known. One reading has
  `0x1c7bd4` copy R4 to R13 before it loads 2, and `0x1c9fd5` save and
  restore R13 without using it. No write to an SPI or DMA register has been
  found yet. **[D][O]**
- 1.16 SW `0x1c1496`-`0x1c149a` is `9b_abs` (a delayed jump), `17b`
  R0=`0xabc` and `25c_rframe`. The two instructions after a delayed jump
  execute before the jump (Core Programming Reference, Table 4-7), so this
  function returns 2748. The bytes are the same in 1.15C. **[V]** Ghidra
  starts the function at `0x1c136a` and finds one caller, SW `0x1c0027`,
  outside the main program. **[D]**
- 1.16 SW `0x1c7e4f`-`0x1c7e51`, and again `0x1c7ee3`-`0x1c7ee5`, write
  `0x401` (1025) to DM(`0x26822c`), offset `0xc` of a structure at
  `0x268220`. **[V]** The structure is then passed in R8 to `0x1c834a` and
  to `0x1c83ff`. **[D]** Whether 1025 is half of the 2050-byte frame is
  not known. **[O]**
- No 4-aligned word in either loader stream equals `0x802`, `0xabc`,
  `0x401` or `0x55e`. The `0x802` in the `4a` at SW `0x1cb423` is part of
  its compute field, not a value. **[V]**

## Workstream D kickoff: the generic parameter -> mirror-index resolver **[D][O]**

Tracing MANUAL SLICE's (type 6) LEV parameter end-to-end (2026-09-20). LEV was
chosen over SLICE/LEN: it is the generic level control on the mainline
parameter-apply flow with no `type==N`-gated branch. SLICE's SRC page shows
`LEV`, `SLICE`, `LEN` and a dash (docs/findings/02-machines-and-parameters.md,
"Type 7 did not stick: a permission check was the sixth bound" [V]).

Static path (sharpens the existing apply chain): a parameter edit goes
`SoundParameterSet::vfunc_31` (generic apply) / `vfunc_13` (coarse/fine tune) ->
`FUN_4002d7a4(value, track, index)`, which writes one short to the live `Sound`
object at `src + 0x14 + index*2` (guard `-1 < index < 0x47`, so 0x47 shorts) and
to the `0x8e`-stride mirror `0x80003362 + track*0x8e + index*2`. The `index` is
resolved from the parameter code by **`FUN_400d9ed8`** (read from disasm, the
decompiler's `*0xf` was misleading):

```
D0 = param_code
D1b = (D0 < 0x113) ? 0xff : 0        ; clamp out-of-range to entry 0
D0 = D0 & sign_extend(D1b)
return *(long*)(0x4020f18c + 4 + D0*0x3c)   ; table @ 0x4020f18c, 0x3c stride, idx field at +4
```

So the parameter table at `0x4020f18c` (stride `0x3c`, 0x113 entries) carries each
param's mirror `index` at offset +4 (a `-1` there means the param is not
mirror-indexed). From the mirror the value propagates (dirty bit `FUN_400d9204`
-> vector-191 `FUN_400d90ac` -> DSP table `0x8000dd40`; `FUN_400d92a2` ->
`0x80005b50 + i*0x8e` at `+0x2a/0x3a/0x4a`) to **TX frame offset 0x74 =
word at 0x80005b50 + 2\*track** (offset 0x74 is doc-traced [D], not yet measured).

**Open [O]:** LEV's specific `param_code` (and thus its table entry / `index`)
was not pinned -- the bare string "LEV" at `0x40226eac` sits in mixer/track-level
rodata (param_code `0x0a`, `+4` field = -1), likely NOT the SRC-page LEV, so it
was not attributed. The `0x4020f18c` table's meaning by entry is otherwise
undecoded. Save/project representation untraced.

**Blocked [O]:** the emulator A/B that would measure LEV -> mirror -> TX-0x74
cannot run -- every snapshot in `snapshots/` is 1.15C and no 1.16 `.syx` is in the
tree, so a 1.16 snapshot ladder must be built first (`DT2_SYX` -> the 1.16 `.syx`,
`emu.checkpoint make`). This is the standing gate for any 1.16 emulator
measurement, not just this trace.

## SRC-page parameters do not travel in the vector-191 DSP frame **[V][C][O]**

> **Superseded.** The headline claim of this section is wrong: it treated
> `mirror_index` as a word offset from `0x80005b50` and missed the 17-word
> (`0x22`-byte) row header, which shifts every index by 17. SRC-page
> parameters *are* in the frame, in the first of the four block copies. See
> "The mirror index to TX frame map, and the 17-word header" below. The
> pointer strides and the frame-field inventory recorded here are correct.

Pointer strides in the vector-191 frame builder, pinned from the instructions
rather than the decompiler's element arithmetic
(`disasm/4002dd0c_vector_191_handler.s`, loop `LAB_4002eb2a` .. `0x4002ecf4`):

| decomp var | reg | init                                                                                         | stride    | evidence                              |
| ---------- | --- | -------------------------------------------------------------------------------------------- | --------- | ------------------------------------- |
| `puVar8`   | A2  | `&DAT_80005b50` (smoothed mirror; `FUN_400d92a2` returns it unconditionally at `0x400d9346`) | fixed     | `0x4002e8d4`                          |
| `puVar27`  | A5  | `= A2`                                                                                       | **+0x8e** | `0x4002ecee lea (0x8e,A5),A5`         |
| `puVar31`  | A3  | `&DAT_80005348`                                                                              | **+2**    | `0x4002ecda addq.l 0x2,A3`            |
| `puVar26`  | A4  | `= A3`                                                                                       | **+0x60** | `0x4002ecea lea (0x60,A4),A4`         |
| `local_88` | —   | `= &DAT_80005b50`                                                                            | **+2**    | `0x4002eb32 addq.l 0x2,(local_88,A6)` |

`FUN_401360ac` is a byte-count memcpy, so the four block copies map mirror word
indices to frame bytes as:

| dest (F + t\*0x60 + ...) | len  | source word indices |
| ------------------------ | ---- | ------------------- |
| `+0xda`                  | 0x14 | 42-51               |
| `+0xfa`                  | 0x1c | 52-65               |
| `+0x116`                 | 0x1a | 66-78               |
| `+0x130`                 | 0x0a | 81-85               |

The row is 71 words (`0x8e` B), so copies 3 and 4 provably read past the end of
track `t`'s row into track `t+1`'s (displacements `0x84`/`0xa2` exceed `0x8e`).
Whether that spillover is intentional packing or a bug is **[O]**; for t=15 it
reads past the 16 legitimate rows entirely.

**None of the four copies, and none of the eleven per-track scalars
(`0x02/0x34/0x54/0x74/0x94/0xb4/0x73c/0x75c/0x77c/0x79c/0x7bc`), touch mirror
indices 25-34.** The scalars come from four other per-track arrays
(`0x800047fc + t*4`, `0x800047dc + t*2`, `0x80003cd0 + t*0x9a`,
`0x47db41d0 + t*0x14`). So the machine's own parameters reach the DSP by some
other route, or not at all: this frame carries the filter/amp/FX/LFO bands.

`FUN_400cd2bc` (the DSP send) has exactly two call sites in the whole 1.16 image
(`xrefs.sqlite` `calls`): `0x4002dd74` in the frame builder above, and
`0x400ceccc` in a **second, structurally different `vector_191_handler` at
`0x400cec70`**. That second one is not a frame builder -- it runs a per-voice
loop (`0x400ced08`) doing fractional-pitch resampling on the ColdFire's own EMAC
(`msac.l D0,A0,ACC0` / `movclr.l ACC0,D0`) against an SDRAM sample-cache window
(`0x4fe58100`/`0x4fe57100`). That is the most likely home of SRC-page
consumption -- sample playback resolved locally rather than shipped to the DSP --
but it is **[O]**: the table feeding its per-voice start/length was not traced.
One genuine SRC-page write path does exist outside the TX loop: the per-track
dirty-message loop writes raw-mirror index 31 (STRT) for machine types 4/6 at
`0x4002e606`-`0x4002e618` and calls `FUN_400d907e(t, val, 0x1f)`.

**[C] correction to "Workstream D kickoff" above.** That section states the LEV
path ends at "TX frame offset 0x74 = word at `0x80005b50 + 2*track`" and cites
`FUN_400d90ac` as the producer. The first half is literally what the code does
but it is not LEV, and the second half conflates two loops:

```
0x4002eb2e  movea.l (local_88,A6),A1
0x4002eb32  addq.l  0x2,(local_88,A6)   ; +2 BYTES per track, never +0x8e
0x4002eb3a  move.w  (A1)+,(0x74,A3)
```

`local_88` starts at row 0's base and advances one word per track, so TX `0x74`
for output slot `t` receives **word `t` of track 0's row** -- indices 0..15, the
trig/LFO band -- not track `t`'s LEV (mirror 34). Mirror index 34 never reaches
TX `0x74` for any track. And the cited `FUN_400d90ac` call (`0x4002e588`) is in
the earlier per-message dirty loop, operating on a per-track `0x450`-byte UI
page-context struct at `_DAT_80004704 + t*0x450 + 0x34`, not on the TX frame.
The semantics of the row-0 word walk are **[O]**.

Consequence for machine building: wiring `0xcc`/`0xfa` into a machine's field 2
would activate the ordinary param_code-generic UI and mirror plumbing, so the
value really would start landing at mirror index 27. But no reader of index 27
has been found, and index 27 is not in the DSP frame, so it is **not** yet a
cheap win. The gating question is now "what consumes mirror indices 25-34",
most plausibly inside `0x400cec70`.

## `FUN_400cec70` is a debug-console sample player, not the track engine **[V][C]**

`FUN_400cd2bc`'s second call site was the leading candidate for a ColdFire-side
sample engine. It is not one. **[C]** corrects the working hypothesis that the
ColdFire might fetch and resample track audio itself.

`FUN_400cec70` is installed as the handler for **exception vector 191**
(`VBR + 0x2fc`) at `0x400ceee8` inside `FUN_400ceeb4`:

```
400ceee8  203c 400c ec70   move.l #0x400cec70,D0
400ceeee  23c0 4000 02fc   move.l D0,(DAT_400002fc).l
```

`refscan` over the whole image finds exactly one reference to that address, so
the previously noted second install site at `0x400d15f6` is spurious — it falls
mid-instruction inside `FUN_400d13ae` **[C]**. Vector 191 is INTC1 source 63,
which the MCF5441X reference manual Table 17-16 (p. 349) lists as "Not used":
it is **software-forced**, and the handler's prologue clears its own force bit
(`400cec86 move.l #0x7fffffff,D0` / `400cec8c and.l D0,(DAT_fc04c010).l`, where
`0xfc04c010` is `INTFRCH1`). Who forces it is **[O]**.

The resampling is real: a 32-iteration outer loop around an inner loop of
exactly **2** whose base pointer is reloaded to a fixed address each outer pass
(`400ced00 lea (0x47db4600).l,A2`, _inside_ the loop), so it is 32 samples for
2 fixed channel slots -- not 64 voices. Per sample it does an indexed PCM fetch
and an EMAC multiply-accumulate against a 19-bit-shifted phase, i.e. fractional
linear interpolation:

```
400ced2c  movea.l (0x0,A0,D7*0x4),A0
400ced30  msac.l D0,A0,ACC0
400ced34  movclr.l ACC0,D0
400ced36  asr.l #0x8,D0
400ced38  move.l D0,(A3)
```

But its control path is an **engineering debug console**. The only function that
ever writes a non-zero start position is `FUN_400cea94`, which has exactly one
caller: `0x400cb6e8` inside `FUN_400cae8c`, a text command parser matching
`"#PLAY_START"`, `"#PLAY_STOP"`, `"#RECORD_START"`, `"#RECORD_STOP"` (strings at
`0x4024161c`/`1628`/`1637`/`1642`) with `"%s %d %d %d %d"` formats, passing the
parsed ASCII integers straight through as channel/slot/start/length/rate. No
part of that chain touches `0x80003362`, `0x80005b50` or any track mirror. Its
`FUN_400cd2bc(0x802, &DAT_42948b3c, 0, 0)` send is a **zero-length** sync tick,
not bulk PCM.

Every write to the voice-control structures (`0x47db45ac`, `0x47db45e0`,
`0x47db4600`) comes from inside `FUN_400cec70`'s own cluster
(`0x400cead2`-`0x400ceebe`); nothing in the real `vector_191_handler`
(`0x4002dd0c`) writes them. The ISR is armed unconditionally at boot (from the
init-table walker `FUN_400cc864`), so the machinery is always live but idle.

By contrast `vector_170_handler` (`0x400d2f98`) is a genuine hardware ISR --
eDMA channel 50 completion (`EDMA_CINT = 0x32`, INTC1 source 42) -- and its
298-byte body contains **no EMAC or multiply instructions at all**: it scans the
`0x4fe57100` ring for a `0x7fffff` sentinel and nudges DMA transfer sizes
between `0x40` and `0x3e`, i.e. ±2-sample clock-drift compensation.

There is no voice, oscillator, engine or synth C++ vtable anywhere on the
ColdFire. Taken together: the ColdFire marshals parameters and sequences DMA;
synthesis is the SHARC's.

## No ColdFire consumer of SRC-page mirror indices 25-34 **[V][O]**

> **Partly superseded.** The negative result is right but the reason was
> missed: there is no ColdFire consumer because these parameters go to the
> SHARC. The "index 29 is clobbered with raw index 12" claim below is also
> wrong -- `A1` advances before the second and third reads, so all three are
> same-index copies. See the correction section below.

An exhaustive search from the write side found no audio-path consumer of mirror
indices 25-34 on the ColdFire. The write formula was re-confirmed at instruction
level from `FUN_4002d7a4` (the decompiler hides a pointer-type trap here):

```
4002d7b4  lea (-0x7fffccc0).l,A0     ; 0x80003340
4002d7c8  moveq 0x47,D2
4002d7ca  muls.l D0,D2               ; track*0x47
4002d7ce  lea (0x10,A1,D2*1),A1
4002d7d8  move.w D1w,(0x2,A0,A1*0x2) ; 0x80003362 + idx*2 + track*0x8e
```

Everything that touches 25-34 is UI plumbing (`FUN_4002d7a4` from
`SoundParameterSet::vfunc_13/31`), p-lock storage (bound-checked `idx < 0x47`,
generic), or propagation (`FUN_400d9204`). Two exceptions, both propagation
rather than consumption:

**Index 29 is clobbered every tick.** After its generic 612-iteration one-pole
filter, `FUN_400d92a2` overwrites three smoothed slots with _unsmoothed_ copies
of three different raw indices:

```
400d931c  lea (-0x7fffa486).l,A0        ; 0x80005b7a = smoothed idx21
400d9322  move.w (0x2a,A1),(A0)         ; smoothed[21] = raw[4]
400d9326  lea (0x8e,A1),A1
400d932a  move.w (-0x54,A1),(0x10,A0)   ; smoothed[29] = raw[12]
400d9330  move.w (-0x44,A1),(0x20,A0)   ; smoothed[37] = raw[20]
```

So anyone reading smoothed index 29 gets raw index 12. `Sound::updateMirror`
(`0x40051aa4`) independently special-cases the same trio -- `if (((idx &
0xfffffff7) == 4) || (idx == 0x14))` -- applying a nonlinear remap via
`FUN_400dad5e`. Indices 4/12/20 are evidently a deliberate pitch-like trio; the
semantics are **[O]**.

**Index 31 (STRT) has a type-gated direct feed.** In the per-track dirty loop,
for machine types 4 and 6 only:

```
4002e604  bne.b LAB_4002e634        ; skip unless type==4 or type==6
4002e612  lea (-0x7fffcc64).l,A1    ; 0x8000339c
4002e618  move.w D1w,(0x4,A1,D0*0x1) ; raw_mirror[idx31], direct literal store
4002e61c  pea (0x1f).w              ; idx = 31
4002e62a  jsr FUN_400d907e
```

sourced from a live per-track playback struct (`local_68+0x64`), not from the
Sound object. `refscan` confirms this is the _only_ literal-address write
anywhere in track 0's raw-mirror 25-34 byte range. The trail ends at the
smoothing target.

Worth recording as a method note: `Sound::updateMirror` has **zero** entries in
Ghidra's `calls` and `data_refs` yet is live -- its literal address appears once,
at `0x401f5418`, inside the `Sound` vtable at `0x401f53cc`. Virtual dispatch is a
blind spot for both Ghidra's tables and `refscan`'s operand regex, distinct from
the trampoline blind spot already recorded.

## `DAT_8000dd40` is a ColdFire-internal smoothing buffer **[V]**

Fully mapped, and a closed negative for the transport question. Layout
`0x8000dd40 + (idx + (track*0x8e + 0x22 >> 1))*4` -- one 32-bit fixed-point
(`value << 16`) slot per mirrored short, covering **all** 71 indices including
25-34. Per-track base `0x8000dd84 + track*0x11c` (`0x11c` = 71 longs), confirmed
independently by `FUN_400d9000`. Extent from the smoothing loop's own
disassembly (`A1 = 0x8000dd40`, `D3 = 0x4c8` decrementing by 2, 2 longs per
iteration = 612 passes = 1224 longs): **`0x8000dd40`-`0x8000f060`**.

Exactly seven functions reference it (`FUN_400d903a`, `400d907e`, `400d90ac`,
`400d914e`, `400d91e4`, `400d9204`, `400d92a2`), all ColdFire-side. It is the
one-pole filter's previous-state store, updated in place, and its only outputs
are `0x80005b50` and the four 16-entry scalar arrays at
`0x8000f060`/`f080`/`f0a0`/`f0c0` (read by `FUN_400d93a4` to build the frame's
per-track scalars). **No eDMA descriptor anywhere references the range.**

## eDMA and DSPI transfer inventory **[V][D][O]**

TCD base `0xfc045000`, channel = `(addr - 0xfc045000) / 0x20`.

| ch  | TCD          | driver                    | destination                    | source                                 | notes                                                                                                                                                                                                                                                         |
| --- | ------------ | ------------------------- | ------------------------------ | -------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 28  | `0xfc045380` | `FUN_400cd2bc`            | `0xec038034` = **DSPI2** PUSHR | `0x80001bc0` staging, rebuilt per call | the SHARC link. `NBYTES=2`, `CITER` = word count. Armed with `EDMA_SERQ=0x1c` **[V]**                                                                                                                                                                         |
| 14  | `0xfc0451c0` | `FUN_400cd48a`            | `0xfc03c034` = **DSPI1** PUSHR | `0x4fe79340` SDRAM staging             | a _different bus_. Completion vector at `0x40000158`; `INTC0_CIMR=0x16`. Sole caller `FUN_400124a0` under 9 wrappers implementing a command/response protocol: 4-byte command out, then a `0x300`/`0xb40`/`0x10`-byte read-back. Peer unidentified **[D][O]** |
| 50  | `0xfc045640` | `FUN_400d30c2` **[C]**    | —                              | —                                      | **[C]** not RX and not unwritten: SADDR `0x4fe58100`, DADDR `0xfc0bc000` = SSI0_TX0, ping-pong via scatter-gather. The writer stages the TCD in RAM and blits it with a generic memcpy, so no literal store exists to find. See the correction section below **[V]**                                                                        |
| 59  | `0xfc045760` | several unrelated drivers | —                              | —                                      | likely USB or storage, not traced **[D]**                                                                                                                                                                                                                     |

Channel 50 having no visible programming site is the substantive open item.
Channels 14 and 28 are programmed through literal addresses; 50 never is, which
favours a generic `setup_tcd(channel, ...)` helper using a runtime-computed
register address -- invisible both to Ghidra's `data_refs` and to `refscan`,
which is documented as blind to `(An,disp)` and indexed forms. Settling it needs
a search for the TCD _field-offset pattern_ (`+0x00/+0x04/+0x06/+0x08/...` off
some register) rather than any literal base.

## The `0x9a`-byte machine row is almost entirely unread **[V][O]**

`refscan` over `0x80003cd0`-`0x80004672` (2466 bytes, 96.78% coverage) finds six
references image-wide: the wholesale writer `FUN_4002d438` (`0x4002d4b8`,
`0x4002d838`), the three known TX-frame bytes at row offsets +0/+1/+2, one
decode-desync artifact at `0x402f4910` (no function range contains it), and one
genuinely new field:

**Row offset +0x68**, read once per track at `0x4002e88c` inside the real
`vector_191_handler`: `cmp.l (A0),D1` with `D1 = 3`; on equality, and if the
track's bit is set in a mask, the code clears that track's bit before calling
`FUN_400d92a2` (smoothing) and `FUN_400d93a4` (frame scalars). It gates whether
a track is smoothed that tick and never reaches the frame. Plausibly a "no
sample loaded" / "track inactive" enum; not confirmed **[O]**.

So 151 of the row's 154 bytes have no confirmed static reader. The row is copied
wholesale from `Sound + 0xa2` and almost entirely unconsumed on the ColdFire.

## Open: how SRC parameters and note-on reach the SHARC **[O]**

Still unresolved after eliminating the periodic frame, `DAT_8000dd40`, the
`0x9a` row and the debug player. Ranked candidates:

1. **eDMA channel 50 / the `0x4fe57100`-`0x4fe58100` SDRAM rings.** A live
   hardware channel with a TCD nothing visibly programs. Settle it by finding
   the boot-time or computed-address TCD write (see the field-offset-pattern
   search above), then reading its `SADDR`/`DADDR`.
2. **DSPI1 (channel 14) command/response protocol.** Confirmed real, distinct
   from the DSPI2 SHARC link, peer unidentified -- no strings, no RTTI. Settle
   it by identifying the 9 command opcodes' callers' domain, or from the
   teardown's chip list for DSPI1's physical lines.
3. The real `vector_191_handler` is 5796 bytes and only about 1150 have been
   read. It reads channel 50's bank-select word at its own entry; what it does
   with that is unexplored.

## The mirror index to TX frame map, and the 17-word header **[C][V]**

This corrects the central claim of "SRC-page parameters do not travel in the
vector-191 DSP frame" above. They do. The error was reading `mirror_index` as a
word offset from `0x80005b50`, missing a 17-word (`0x22`-byte) header at the
start of every row. Everything downstream of that -- "the frame is fully
accounted for and the SRC page is not in it", and the search for another
transport that followed -- was chasing a gap that does not exist.

The header is explicit in `FUN_400d907e`, which writes the smoothing target:

```
400d907e  move.l (Stack[0x4],SP),D0    ; track
400d9082  move.l #0x8e,D1
400d9088  lea (0x8000dd40).l,A0
400d908e  muls.l D1,D0                 ; track*0x8e
400d909a  addi.l #0x22,D0              ; + the header
400d90a0  lsr.l  #0x1,D0               ; /2 -> word index
400d90a2  add.l  (Stack[0xc],SP),D0    ; + idx
400d90a6  move.l D1,(0x0,A0,D0*0x4)
```

`0x22` is exactly where a row's parameter array starts: the raw mirror row base
is `0x80003340` and its parameters begin at `0x80003362`. So a row is 71 words,
of which the first 17 are header and **only indices 0..53 are parameters**.

`FUN_400d92a2`'s filter loop is software-pipelined, which is where the second
trap sits: `A2` is pre-decremented to `0x80005b4c` and each iteration stores the
*previous* iteration's result. The one-iteration delay and the pre-decrement
cancel exactly -- verified two ways, arithmetically and by the final flush
address (`0x80005b4c + 612*4 = 0x800064dc` = `0x80005b50 + 4*611`). Net skew is
zero, so the smoothed buffer's row layout is byte-identical to the raw mirror's:

```
row_offset(idx)          = 0x22 + 2*idx                 (0 <= idx <= 53)
addr_raw(track,idx)      = 0x80003340 + track*0x8e + row_offset(idx)
addr_smoothed(track,idx) = 0x80005b50 + track*0x8e + row_offset(idx)
```

The bypass loop confirms this independently and without any pipelining: its
source and destination row offsets are identical (`0x2a`, `0x3a`, `0x4a` on both
sides), which pins the two buffers to the same layout directly.

Applying `idx = (offset - 0x22)/2` to the four block copies' source offsets:

| copy | frame offset | len | mirror indices | page |
|---|---|---|---|---|
| 1 | `+0xda` | `0x14` | **25-34** | **SRC -- the machine's own parameters** |
| 2 | `+0xfa` | `0x1c` | 35-48 | filter |
| 3 | `+0x116` | `0x1a` | 49-61 | amp and FX sends |
| 4 | `+0x130` | `0x0a` | 64-68 | FX |

So, per track, with `A4 = 0x80005348 + track*0x60`:

```
idx 25-34: frame_offset = 0xda  + 2*(idx-25)
idx 35-48: frame_offset = 0xfa  + 2*(idx-35)
idx 49-61: frame_offset = 0x116 + 2*(idx-49)
idx 64-68: frame_offset = 0x130 + 2*(idx-64)
```

The four destination ranges tile one contiguous `0x60`-byte block per track
(`0xda` to `0x13a`), so tracks do not overlap.

The confirmation that matters: four memcpy ranges derived only from `pea`
operands land exactly on four page boundaries -- 25, 35, 49, 64 -- derived only
from the parameter table's `mirror_index` distribution. Boundary for boundary,
with no fudge. Indices 62-63 (Portamento) are skipped between copies 3 and 4.

Anchor check: the type-4/6 literal `FUN_400d907e(track, val, 0x1f)` at
`0x4002e61c` writes index 31, which for those types is Slice Select. It lands at
frame offset `0xda + 2*(31-25) = 0xe6`, inside copy 1 -- an audibly load-bearing
parameter, provably in the frame.

**CFADE reaches the DSP.** Mirror index 27 maps to frame offset
`0xda + 2*(27-25)` = **`0xde`** (absolute `0x80005348 + track*0x60 + 0xde`). The
copy is an unconditional 20-byte memcpy with no machine-type test, so slot 2
transits to the SHARC for every track on every tick regardless of whether the
track's machine exposes it. Today it always carries whatever the UI never wrote.
Wiring `0xcc`/`0xfa` into a machine's descriptor field 2 would put a real user
value there through the ordinary plumbing.

Two further corrections fall out:

**[C]** The bypass loop is a *same-index* copy, not the cross-index copy recorded
above. `A1` advances a full row before the second and third reads, so
`(-0x54,A1)` and `(-0x44,A1)` resolve back into the same track's row:

```
400d9322  move.w (0x2a,A1),(A0)
400d9326  lea (0x8e,A1),A1             ; advances FIRST
400d932a  move.w (-0x54,A1),(0x10,A0)  ; = +0x3a of the old A1
400d9330  move.w (-0x44,A1),(0x20,A0)  ; = +0x4a of the old A1
```

Row offsets `0x2a`/`0x3a`/`0x4a` are mirror indices **4, 12 and 20** -- the LFO1,
LFO2 and LFO3 destination selectors. They are written raw, after the filter loop
has already passed over them, because interpolating a destination index would be
meaningless. Nothing is clobbered.

**[O]** The row overrun survives the correction and is a separate fact. A row
holds only indices 0..53, so copy 3's source range runs `0x10` bytes past the row
end and copy 4's lies entirely past it -- for track `t` those bytes come from
track `t+1`'s *header*. Where parameters with `mirror_index >= 54` actually live
for a track is unresolved; the `FUN_400d914e` lead is dead (a raw 4-byte literal
scan of the whole image finds zero references to it, so it is unreferenced in
1.16), and `FUN_400d91e4` turns out to be a MIDI-CC writer. The smoothing sweep
is 1224 words, not a multiple of the 71-word row, which is a loose end pointing
at another structure after the 16 rows.

## eDMA channel 50 is SSI0 transmit, and its TCD writer hides behind a memcpy **[C][V]**

Verified independently, field by field. `FUN_400d30c2` (`0x400d30c2`-`0x400d333c`),
whose sole caller is the boot init chain `FUN_400cc864` at `0x400ccc32`, stages
two descriptors in RAM and blits each into the hardware TCD table:

```
400d32ea  pea (0x20).w
400d32ee  move.l A3,-(SP)              ; src = 0x4fe57080 (staged TCD50)
400d32f0  pea (DAT_fc045640).l         ; dest = channel 50's TCD
400d32f6  jsr (A4)                     ; A4 = FUN_401360ac, a plain memcpy
400d32fc  lea (EDMA_SERQ).l,A0
400d3302  move.b #0x30,(A0)            ; arm channel 48
400d3306  move.b #0x32,(A0)            ; arm channel 50
```

Channel 50: SADDR `0x4fe58100`, ATTR `0x0202` (32-bit both sides), SOFF 4,
NBYTES `0x20`, DADDR **`0xfc0bc000` = SSI0_TX0**, CITER/BITER `0x40`, DOFF 0,
DLAST_SGA `0x4fe570a0`, CSR `0x0012` (E_SG + INT_MAJOR). The scatter-gather
partner at `0x4fe570a0` carries SADDR `0x4fe58900` and points back -- a true
ping-pong ring. `NBYTES * BITER = 0x800`, matching the buffer size used
throughout. Channel 48 is the mirror image: SADDR `0xfc0bc008` (SSI0_RX0), DADDR
`0x4fe57100`, CSR `0x0010` (no INT_MAJOR, hence no separate ISR).

`0xFC0B_C000` = SSI0_TX0 and `0xFC0B_C008` = SSI0_RX0 per the MCF5441X reference
manual (`out/refs/MCF5441XRM/all.txt` lines 59252 and 59258, SS35.3.1/35.3.4,
pp. 35-8/35-11).

This is a **third static-analysis blind spot**, distinct from trampolines and
vtable dispatch: the hardware address appears only as a `pea` argument to a
generic helper, which Ghidra tags `DATA` rather than `WRITE`, and the actual
stores inside the memcpy are register-pointer moves with no displacement
immediates. No literal-address search and no TCD-field-offset pattern search can
see it. The way in was to enumerate writers of `EDMA_SERQ` (`0xFC04_4018`) and
read backwards.

**[C]** The TCD field layout is ATTR `+0x04`, SOFF `+0x06`, CITER `+0x14`, DOFF
`+0x16`, BITER `+0x1c`, CSR `+0x1e` -- i.e. three pairs swapped relative to the
Kinetis ordering. Confirmed from the manual's own per-register address formulas
(`all.txt` lines 20802, 20848, 20978, 21029, 21070, 21124; pp. 385-390). This
resolves the open question flagged in
`docs/refs/dspi2-edma-blocker-and-register-sources.md` S7 -- MQX's Kinetis TCD
layout is **not** byte-compatible with ColdFire eDMA. Nothing currently relies on
the wrong ordering: `emu/edma.py`, `docs/contracts/mcf5441x-reference-v1.json`
and the emulator findings already use the ColdFire ordering. Reading the same
bytes under the Kinetis layout yields an incoherent descriptor (CITER 0, E_SG
clear despite a built scatter-gather chain), which is an independent check.

## The `DAT_4031b264` node list is USB audio streaming, not voices **[V]**

A 16-slot pool of `0x40`-byte nodes, drained every tick by `FUN_40003376` into
the SSI0 TX ring, looked like a voice list. It is not. Two independent passes
agree:

- **Activation is a USB control request.** `FUN_400030b0` (builds the list) and
  `FUN_400030e6` (tears it down) are called only from `FUN_40005ff6`, a USB EP0
  SETUP dispatcher decoding `_DAT_47db4198` as bmRequestType/bRequest. The value
  that reaches `FUN_400030b0` is `0x010b0000` -- **SET_INTERFACE** -- with an
  interface selector of 6 and alternate setting 2. Its only caller is
  `vector_134_handler`. Nothing in the sequencer, note-on or UI reaches it.
- **Nodes are submitted to the USB controller.** `FUN_40002eac` ends with
  `FUN_40005a86(3, node)`, which drives registers at `0xfc0b014c`/`0140`/`01b0`/
  `01b8`. `0xFC0B_0000` is the USB On-the-Go controller (`all.txt:2578`). Every
  node is a USB endpoint-3 buffer descriptor.
- **No synthesis.** `FUN_401360ac` is a pure block move that overwrites rather
  than accumulates; `FUN_40135ad8` is a zero fill (silence insertion). Grepping
  `FUN_40003376`, `FUN_40002eac`, `FUN_401360ac` and `FUN_40135ad8` for
  `mac|movclr|acc0|emac` returns **zero matches**. It walks one singly-linked
  FIFO from one head and writes non-overlapping output positions -- a drain, not
  a mixer. Clock skew is handled by skip/silence accounting, not resampling.

So this is the Digitakt acting as a USB audio interface in the speaker
direction: host PCM in over USB, out through SSI0 to the DAC.

**[C]** `0x8000cb40 + n*0x100` is not page-aligned (`0xb40` low bits); it is a
256-byte-stride DMA buffer array with a mid-page base. Node `+0x30`, the pointer
`FUN_40003376` actually dereferences for data, is written by neither the
allocator nor anything else found -- presumably the USB RX-complete handler
**[O]**.

Two unnamed handlers at `0x40002cb8` and `0x40002db6` sit in a gap between
Ghidra functions and are invisible to any function-based search -- the
trampoline blind spot again. They maintain the USB-clock/audio-clock drift
state, masking a frame counter with `0x7ff`.

## `FUN_400cec70` confirmed as the debug-console player **[V]**

Independently verified. Installed at `0x400ceee8` into `0x400002fc` (vector 191);
the normal handler `FUN_4002dd0c` is installed separately at `0x4002d51e`. The
sole caller of `FUN_400cea94` is `0x400cb6e8` inside `FUN_400cae8c`, confirmed by
both the `calls` table and `refscan` at 99.63% coverage. The four command strings
sit at `0x4024161c`, `0x40241637`, `0x40241642`, `0x4024165c` and are referenced
only from within that parser.

The "zero-length send" is more precisely a **hardcoded zero destination
pointer**: `FUN_400cec70` passes `(0x802, &DAT_42948b3c, 0, 0)`, and inside
`FUN_400cd2bc` a `tst.l`/`beq` on argument 4 unconditionally skips the staging
copy.

**[C]** confirmed: the apparent second install site at `0x400d15f6` is spurious.
`400d15f4: b4 af 00 30` is a four-byte `cmp.l (local_c,SP),D2`, and `0x400d15f6`
is its third byte. A whole-image `refscan` finds exactly one literal reference to
`0x400cec70`, at `0x400ceee8`.
