# Emulator

## Native loader and host checkpoint (2026-09-30) **[V]**

`native/loader` decodes the SysEx 8-in-7 transport, ELE3 entries and ELZ
sections without filesystem APIs. Its decoded DT2 1.16 and DN2 1.11 sections
byte-match `dt2/elz.py`; its library also compiles for `wasm32-unknown-unknown`.
The native host's `--syx` input selects section 3, the MAIN_OS at
`0x40000400`, rather than section 2, which is the bootstrap/version record.

The bounded DT2 1.16 Oracle host checkpoint accepts only recorded force/feed
events and `--timer oracle`. With 68 TX frames and three offline renders per
TX it matched the recorded wire stream, rendered 204 clean SHARC frames with
zero stops, and produced the same PCM hash through `--main` and `--syx`.
It is not cold boot evidence, autonomous Device cadence, sustained GUI proof,
or a browser runtime result.

Bringing the ColdFire emulation up: display, DSP bring-up, making it run, the performance work, correct-speed playback, the serial console and boot.

## The emulator boots Digitakt II 1.16 **[V][D][O]**

- `emu/symbols.py` resolves all seven required symbols on 1.16, and 57 of
  72 in all. Its `transport` signature matches the wrong function there,
  `0x40134200`; Version Tracking maps 1.15C's `0x40128c7c` to `0x40136268`.
  `call_sites` therefore lists 25 sites instead of 4; the cold boot only
  logs them. **[D]**
- `DT2_SECTIONS=out/sections/dt2-1.16 DT2_SNAPSHOTS=out/snapshots/dt2-1.16
  uv run python -m emu.checkpoint make
  60000000,120000000,200000000,280000000,400000000
  out/snapshots/dt2-1.16/boot Digitakt_II_OS1.16.syx` builds a 1.16 ladder
  in about 6 minutes:

  | rung | 1.15C tasks | 1.16 tasks | 1.16 PC |
  |---|---|---|---|
  | 60M | 5 | 5 | `0x401382aa` |
  | 120M | 5 | 5 | `0x4011e8bc` |
  | 200M | 5 | 5 | `0x400dc4f6` |
  | 280M | 9 | 5 | `0x401e55e0` |
  | 400M | 9 | 9 | `0x40182e0e` |

  The four late tasks, among them the Main OS task `FUN_400337ba` (1.15C
  `FUN_40032f5a`), start at about instruction 291.4M in 1.16 and 257.6M in
  1.15C. **[D]**
- `tools/bootwatch.py --frame-gate` boots from reset with write watches on
  the stop flag, countdown, gate and mode of the image's profile. In both
  versions there are six writes. The startup code clears all four at
  instruction 1,752,605-1,752,609 (`0x400004d2`, a loop in
  `FUN_400004b2`). Then the SHARC boot routine calls the helper
  `moveq #1,d0; move.l d0,gate; move.l d0,mode; bra.w <vector-191
  installer>` once: in 1.15C `FUN_4002d5b2`, called from `0x400cf320` in
  `FUN_400cef6c` at instruction 257,643,827; in 1.16 `FUN_4002dc62`,
  called from `0x400ccc18` in `FUN_400cc864` at instruction 291,415,290.
  That call site is the helper's only caller in each image. The stop flag
  and the countdown stay 0. So a normal boot leaves the gate at 1, which
  keeps the frame build off, and installs the handler right after. **[V]**
- `tools/sharcframe.py out/snapshots/dt2-1.16/boot400M.snap` (with
  `DT2_SECTIONS=out/sections/dt2-1.16`) enters the 1.16 handler
  `0x4002dd0c`, which returns after one driver call with TX `0x802` bytes at
  `0x80005348` and RX `0xabc` bytes at `0x8000488c`. With the gate closed,
  both passes send `eabdd389…`, the same as pass 0 with `--open-gate`;
  passes 1 and 2 with `--open-gate` send `d674f76c…`. Against 1.15C, pass 0
  and pass 1 each differ in one byte, offset 1: `0x01` and `0x03` in 1.16,
  `0x00` and `0x02` in 1.15C. The 1.16 installer's `move.w` of 1 to
  `0x80005348` sets that byte. **[V]** The bytes that change from pass 0 to
  pass 1 are at the same 41 places in both versions. **[D]**
- The frame capture does not use the 1.15C addresses still written as
  literals in the emulator: `PRINT` and `SWITCH_TO` in `emu/longrun.py`,
  `TX_STATE` `0x4094cd74` and `WAIT_LOOP` in `emu/edma.py`, the weak-pointer
  patch, and the addresses in `emu/panel.py`, `emu/screen.py`,
  `emu/hle.py`, `emu/serial.py` and the terminal hook in `emu/gui.py`. A GUI
  run on 1.16 would. Why pass 0 sends no track data, and what the slot words
  mean, are still open. **[O]**

## Emulation

Function-level works well and is the practical path. Full boot was pushed as far
as it would go; four blockers were cleared, **two of them Unicorn defects**:

| # | blocker | nature | distinct addrs after |
|---|---|---|---|
| 0 | no exception dispatch / `rte` / interrupts | emulator | 444 |
| 1 | `FF1.L` at `0x401112de` — undecodable by Capstone, unimplemented in Unicorn | **emulator** | **36,474** |
| 2 | `USR8` bit 2 (TXRDY) poll, `0xEC070004` — **UART8** | hardware | 37,395 |
| 3 | `movec d0,Rc=0x009` at `0x400cf7e4` — aborts the Unicorn process (SIGABRT) | **emulator** | — |
| 4 | `DSPI0_SR` RXCTR poll, `0xFC05C02C` — **DSPI0** | hardware | 38,193 |

Peripherals identified from NXP *MCF54418RM* Rev. 5 (Table 1-4; ch. 40 §40.3.4–
40.3.7; ch. 41). Only eight distinct peripheral registers are read in the whole
boot, and peripheral setup is read-modify-write that works fine against zeros. **[V]**

### The SPI flash needs no physical dump — correction

An earlier reading of stall 4 concluded the firmware wanted "real bytes off an
SPI device we have no image of", implying a hardware dump was required. **That
was wrong.**

`0x401296fe` is the SPI NOR read routine, signature `read(offset, len, dest)` —
confirmed by `0x84020003 -> DSPI0_PUSHR`, whose low byte `0x03` is the NOR READ
command. Its callers scan the ELE3 section table at flash offset `0x80020`. The
flash content it wants at boot **is the staged OS container**, which is exactly
what `dt2.container` decodes out of the `.syx` we already have.

High-level-emulating that one function and backing it with the container (see
`emu/flashboot.py`) makes boot progress immediately, and the reads it issues
confirm the model is right: **[V]**

```
off=0x080000 len=32      -> 0x44e4d67c    ELE3 header, into the exact address
                                           MAIN OS compares at 0x40128b8c
off=0x080020 len=16  x5                    the five section-table entries
off=0x19be60 len=184844  -> 0x45020a90    section 7 = the SHARC DSP blob
```

Coverage 36,483 -> 37,627 distinct addresses, and the firmware is now loading the
DSP image. The next frontier is the ColdFire<->SHARC link (DSPI2/eDMA), not
another storage problem.

Note the UI draws into a `Bitmap` object (`SoundBrowser::drawMain(Bitmap&)`),
so rendering the screen is a matter of locating that buffer once boot gets far
enough — not of reverse-engineering a display controller. **[O]**

What a physical dump *would* still be needed for: the +Drive contents (samples,
projects), which live elsewhere in flash and are not required to boot.

Also unresolved: `m68k` `SR` must be written **before** `A7`, or the stack
pointer lands in the banked register the CPU is about to stop using. Cost an
hour; noted in `emu/harness.py`.

## Open questions

- ~~Does MAIN OS's USB upgrade path reject a same-version image?~~ **ANSWERED
  -- no.** The comparison is a hard-coded **build-number floor**, not a
  comparison against what is running, so a same-version image passes. It was not
  found by searching for the version string because it reads the **build**
  string at ELE3 `+0x08`, through a register rather than an absolute address.
  Read on 1.16 (`0x400d9e4c`, floor `"006/"` = build >= 0060); see "MAIN OS has
  a second gate" above, which includes a byte pattern for confirming it on
  1.15C. **[V on 1.16, [O] on 1.15C]**
- Is the bootstrap rewrite atomic once triggered? **[O]**
- Sample/project/preset on-flash layout. `MmcFs`, `/factory`, and the manager
  classes are visible; the partition and directory format is not mapped. **[O]**
- `ERROR_headerVersion_wrong` and friends in MAIN OS are the **LZ4 frame error
  enum**, not OS versioning — a false lead worth recording.

## Display

The panel is **128 x 64, 8 bits per pixel**, read straight out of the firmware's
own structures rather than guessed. **[V]**

`intro_dither::px_copy_to_bitmap(PixelData&, Bitmap&)` at `0x400d315e` — the
binary carries the *demangled* signature as an assert string at `0x401f7ef0`,
which makes it an unusually good anchor. From its argument handling and copy
loop:

```
Bitmap      +0x04  width
            +0x08  height
            +0x0C  stride -- 32-bit words per COLUMN
            +0x10  pixel data pointer
PixelData   +0x00  width
            +0x04  height
            +0x08  8bpp row-major source buffer
```

Pixels are **column-major, 1 bit per pixel**, 32 rows packed per big-endian
word, MSB = lowest y:

    word_index = x * stride + (y >> 5)      bit = 0x80000000 >> (y & 31)

recovered from `Bitmap::setPixel` at `0x40104eb4`. An earlier draft of this
document called `+0x0C` a setPixel function pointer; that was wrong -- it came
from a different routine at `0x400d3244` where the register did not hold a
Bitmap. The panel is 1bpp, not 8bpp: the 8bpp `PixelData` is a greyscale source
thresholded on the way in.

The intro's `PixelData` instance lives at `0x4028ae98` and reads
`width=128, height=64, buffer=0x43139290`. The loop bound `cmpi.l #$2000`
(8192 = 128x64) at `0x400d3640` corroborates it, as does the runtime struct,
which reads back `0x80, 0x40, 0x43139290` under emulation.

**Not yet captured: actual pixels.** The buffer is allocated at runtime and is
still all zeros after 80M instructions — boot parks in DSP init before the intro
renders. Two ways forward, neither attempted:

1. Progress boot past the ColdFire<->SHARC handshake so the UI runs naturally.
2. Call the intro renderer at `0x400d3372` directly. It has no direct callers
   (`jsr (a2)`, `jsr (a5)` — invoked via lambda/vtable), so a harness would have
   to supply the allocator and callback registers. **[O]**

Note the drawing path is pure ColdFire — `MainScreenView`, `SoundBrowser::drawMain(Bitmap&)`,
and ~140 item-renderer lambdas of shape `(int, Bitmap&, int, int, bool)`. Nothing
in it needs the DSP, so route 2 should not require the SHARC at all.

### Rendering firmware graphics without booting **[V]**

`emu/screen.py` runs the device's own drawing code and captures the output. The
mechanism, and one correction to the note above:

`px_copy_to_bitmap` does **not** dispatch through `Bitmap+0x0C`. It loads a
fixed address into `a4` and calls that: `0x40104eb4` is the real
`Bitmap::setPixel(Bitmap*, x, y, value)`. Intercepting that address captures
every pixel without needing to know how `Bitmap` stores them. (`+0x0C` is used
by a *different* renderer at `0x400d3220`, so the harness intercepts both.)

The source is thresholded to 1 bit on this path — `cmpi.l #$80` then `shi.b` at
`0x400d31d8` — so an 8bpp greyscale `PixelData` becomes a monochrome Bitmap.

Validated against ground truth rather than by eye: feed a synthetic 40x16 source
through the firmware routine and the captured pixels match the thresholded input
exactly, 640 setPixel calls for 640 pixels. `./venv/bin/python emu/screen.py selftest`

This matters because a wrong `PixelData` renders as *plausible dither* rather
than failing — several structs found by heuristic scanning are false positives.
Locating genuine static image assets is still open. **[O]**

### Text rendering — still open **[O]**

Not found yet, and the obvious routes came up empty:

- 27 distinct functions call `setPixel`; **none walk a string** (no `move.b (aN)+`
  over a char buffer), so text must go through a glyph blitter one character at
  a time, called from a higher-level loop.
- No standard 5x7 or 6x8 bitmap font table is present (searched for the
  distinctive `'!'` glyph `00 00 5F 00 00` and variants).
- `PopupWindow(const Bitmap*, ...)` and `VerticalMenuView(const char*, int,
  std::string, const Bitmap*, int)` show `Bitmap` is used as an icon type, but
  scanning for static instances in the recovered layout finds none - icons are
  constructed at runtime, so the glyph/icon data is probably stored compressed
  or generated.

The rendering harness itself is done and verified, so once a text routine is
located it can be driven immediately.

### Why full boot stalls — the real reason **[V]**

The RTOS task created at boot (entry `0x400cef6c`, stack `0x4000`, priority 1)
**is the DSP bring-up task**. Walking the call tree upward from the ColdFire<->SHARC
transport lands exactly on it:

```
0x40128c7c   transport: write arg -> DSPI 0xFC074004, kick 0x841b -> 0xFC074000,
             then block on a semaphore the completion ISR would signal
  <- 0x400cf000, 0x400cf928, 0x400cfd8a, 0x4012d46e   (4 call sites)
    <- 0x400cef6c   the task entry itself
```

The transport installs an ISR at vector 97 (`0x40000184`), enables INTC sources
`0x21`/`0x1d`, starts the transfer and waits. Firing the completion interrupt by
hand gains only ~330 addresses; stubbing the transport outright gains nothing and
just moves the stall to `0x400cf956`. Four call sites means DSP bring-up is a
**stateful conversation**, not one transfer.

This is a genuinely different wall from the SPI flash. There, the data we needed
already existed in the `.syx`. Here the ColdFire is waiting on replies from a
processor with no open emulator, so the responses would have to be *synthesised*
from a protocol nobody has documented. Boot cannot complete without that, and the
UI task presumably never starts because DSP init never finishes.

**Update (next session): the "do not pursue" call above was too pessimistic.**
The DSP handshake did not need real SHARC replies synthesized -- it needed the
*ColdFire-side* synchronization primitives satisfied, which turned out to be
inspectable and fakeable without modeling the SHARC at all. See "DSP bring-up
-- session 2" below: `task_create` sites reached went from 4/16 to 10/16 this
way. The capability table remains accurate for what *doesn't* need booting:

| capability | status |
|---|---|
| CRC-32 oracle (`0x80001bd0`) | works, byte-exact |
| aPLib depacker (`0x80000432`) | works, validates a 3.1 MB repack |
| firmware graphics rendering (`emu/screen.py`) | works, pixel-exact vs ground truth |
| Bitmap framebuffer encode/decode | works, verified two independent ways |
| full boot to UI | in progress -- 10/16 `task_create` sites reached, see below |

## DSP bring-up -- session 2

Starting point: 4/16 `task_create` (`0x400012c8`) call sites reached, ~37,627
distinct code addresses, stuck inside the DSP-transport wait at `0x40128c7c`.
Ending point: **10/16 `task_create` sites, 58,337 distinct addresses.** New
tooling: `emu/dspboot.py` (instrumented boot harness, reusable) and a scoped-hook
speedup added to `emu/harness.py`. All of it verified by running, not inferred.

### Blocker 1 (cleared): the transport is a mutex+semaphore wrapper, not an RPC

Re-reading `0x40128c7c` disassembly line by line (not just skimming) shows it
is **not** "send a request word, wait for a reply word" as the earlier session
guessed. It is:

```
lock mutex @0x44e4d6a4                          (0x400015a0)
  (first call only: install ISR @0x40128c4c at vector 97,
   enable INTC sources 0x21/0x1d, init completion sem @0x44e4d69c to 0)
write timeout(!) -> 0xFC074004
write control word 0x841b -> 0xFC074000          (kicks the transfer)
jsr 0x4000141a   (sem_pend on 0x44e4d69c)        <-- blocks here
unlock mutex (tail call into 0x400016d2)
```

The value each of the 4 call sites pushes (`0xF4240`, `0x3E8`, `0x64`,
`0x2DC6C0` = 1,000,000 / 1,000 / 100 / 3,000,000) is a **microsecond timeout**
written to hardware, not a command/payload word -- it is never read back by the
ColdFire side. The real completion signal is the semaphore at `0x44e4d69c`,
which the would-be completion ISR at `0x40128c4c` posts to via
`0x4000155c -> 0x400011ee`.

Critically, `0x4000141a` (sem-pend) has a fast, non-blocking path: if the
semaphore's count field is already `>0`, it clears it and returns immediately
without ever calling the scheduler (`trap #0`) -- and **every one of the 4
callers discards its D0 return value** (overwritten immediately after the
call), so nothing downstream ever checks "did the transfer really succeed."

**Patch**: at `PC == 0x40128d08` (the `jsr 0x4000141a` instruction itself),
write `1` into the 4 bytes at `0x44e4d69c` before it executes. No interrupt
firing, no scheduler re-entry, no `rte` -- just pre-satisfying the flag the
very next instruction is about to check. This is different from, and more
surgical than, the earlier session's attempts (firing vector 97/33/29 by hand,
or blanket-stubbing the whole transport function), which is presumably why
those only gained ~330 addresses or moved the stall without progress.

Result: the one transport call site actually reached at this point in boot
(`0x400cf928`, timeout `0x3E8`) goes through. Two more polled hardware status
registers immediately downstream needed the same treatment, discovered by
running and reading what changed at the new stall PC:

- `0xEC03802C` bit 31 (`0x400cf956`, a byte-at-a-time TX loop unrelated to the
  already-known UART8/DSPI0 mocks -- a *second* status register pair,
  `0xEC094018`/`0xEC03802C`, spent uploading what is very likely the SHARC ADI
  loader blob byte-by-byte after the handshake succeeds)
- `0xFC05C02C` bit 28 / RFDF (`0x40129da2`) -- same DSPI0 status register
  already mocked for RXCTR, but a *different* bit, tested by a second,
  synchronous SPI0 read routine reached only after the transport unblocks

Both mocked the same way as the pre-existing UART8/DSPI0 mocks: force the
polled bit permanently set in `Machine.mmio`.

### A red herring that turned out to be correct behavior, not a bug

Past the above, boot hit a second embedded aPLib-style depacker at
`0x4012ab70` (distinct from the bootstrap's `0x80000432`, and from the
flash-section-table depacker used by MAIN OS's own boot-time decompression --
this one runs on in-memory buffers during DSP bring-up). One invocation
(source `0x402489b4`, a *static address inside the already-loaded MAIN OS
image*, not flash- or DSP-reply-dependent) appeared to run away: 2.6M+ hits at
the same 3 addresses (`0x4012acba/bc/be`, its copy loop) with zero new code
coverage for tens of millions of instructions -- classic infinite-loop
signature.

It is not one. Register tracing (dump D2/D3/A1 on every entry to the copy
loop) showed match lengths never exceeding ~2KB; the "stall" was thousands of
small, legitimate tokens through a tight loop, which the coverage-based stall
heuristic cannot distinguish from a hang because it only tracks *new* PCs, not
forward progress within a loop. Independently ruled out a Unicorn MVZ/MVS
decode bug (the specific opcode class flagged as broken in Capstone) by
testing `mvz.b`/`mvs.b` in isolation, register and `(a0)`/`(a0)+` addressing --
all matched 68k semantics exactly. Given more instruction budget (400M+) this
depacker completes normally and boot proceeds. A defensive safety valve was
added anyway (clamp the copy count if it ever exceeds 64K, `DEPACK_COPY` in
`emu/dspboot.py`) but it has never actually fired -- included for whatever
comes next, not because it was needed here.

**Lesson for next time**: before concluding a repeating-PC "stall" is a hang,
dump the actual loop-bound register(s) a few times. A slow-but-finite loop and
an infinite one look identical to a coverage-only heuristic.

### Blocker 2 (cleared, and generalized): idle spins need timer ticks too

With the above fixed, boot progressed in a large burst: task_create sites went
4 -> 5 -> 9 -> 10 as longer instruction budgets were tried (47M, 257M, 416M).
Between two of those bursts, boot hung again -- this time truly, 139M+
instructions with zero new coverage, at `0x400cf3e0`.

`0x400cf3e0` disassembles to `bra.b $400cf3e0` -- a literal self-branch. This
is the **exact same idiom** as the already-known `HALT` idle loop
(`0x400ceeb6`, also `bra.b $self`), which the harness already fed periodic
timer ticks (vector 32) to keep the RTOS scheduler moving. But the harness only
ever ticked *that one hardcoded address* -- a second thread/task's own
idle-wait-for-scheduler point at a different address got no ticks at all, so
once execution reached it, nothing could ever preempt it.

**Fix, generalized rather than special-cased**: scanned all of MAIN OS for the
opcode `0x60FE` (`bra.b -2`, i.e. branch-to-self) -- 13 occurrences total --
and feed periodic timer ticks to *all* of them, not just the one instance
someone happened to hit first (`find_idle_spins` in `emu/dspboot.py`). This is
exactly the kind of fix the diverging/converging distinction in the task brief
calls for: a class of blocker, not a single address.

This got two of the three known idle points working correctly (`0x400ceeb6`
and `0x400cf3e0` both now receive ticks and both did unblock at least once,
confirmed by `spin_by_addr` counters saturating at clean multiples of
`tick_every`).

### Where it stands now: a new, different kind of stop

After the burst that reached 10/16 (last new task at instruction ~416M,
`0x400f1a6c` -> entry `0x400f1eb6`, prio 2), execution parked at `0x400cf3e0`
and stayed there for the rest of a 1.4B-instruction run -- ~48,000 further
timer ticks, zero new coverage.

Traced precisely (not just inferred from the address repeating): `0x400cf3e0`
sits between a one-shot guard and a permanent idle trap in the *same* function
that produces 3 of the burst's 4 tasks:

```
400cf3d6  moveq #$40,d0
400cf3d8  and.l $40288190.l,d0
400cf3de  beq.b 400cf3e2                 ; bit clear -> do the work (it was clear: confirmed 0x40288190=0x04 at runtime)
400cf3e0  bra.b 400cf3e0                 ; <-- idle trap, same idiom as HALT
400cf3e2  jsr 0x4011311c                 ; wrapper containing task_create site 0x401131d2 (prio 7)
400cf3e8  jsr 0x4014635e                 ; wrapper containing task_create site 0x4014638e (prio 5)
400cf3ee  jsr 0x400329ee                 ; wrapper containing task_create site 0x40032a20 (prio 6)
400cf3f4  bra.b 400cf3e0                 ; done -- park here forever by design, same as HALT
```

So this is **not** a guard repeatedly failing -- it passed once, did its
one-shot job (matching the 3 near-simultaneous hits at instructions
257710408/257710485/257710555), and then deliberately loops to the same idle
trap as its designed terminal state, exactly like `HALT`. There is nothing
further for *this* thread to do; it is functioning correctly. This resolves
what looked like an open question in an earlier draft of this note.

The real open question is why **no other** thread creates any of the
remaining 6 `task_create` sites even after tens of thousands of scheduler
ticks. The newly-created prio 2/5/6/7/8 tasks are themselves candidates to be
the ones that would create more (or not -- they may simply be leaf worker
tasks). Two live hypotheses, neither confirmed:

1. One of the **other three** DSP transport call sites (`0x400cf000` timeout
   `0xF4240`, `0x400cfd8a` timeout `0x64`, `0x4012d46e` timeout `0x2DC6C0`) is
   what some other task is blocked on -- all session, only one of the four
   (`0x400cf928`) has ever been exercised (`transport calls: 1` in every run).
   If a task is parked in a *real* semaphore wait (`trap #0`, correctly
   descheduled by the RTOS) rather than a self-branch idle loop, our idle-spin
   fix does not apply to it -- it needs the same treatment as blocker 1
   (satisfy whatever it is actually waiting on), not more timer ticks.
2. The remaining 6 sites are in code that is reachable only through a
   different subsystem-init path this cascade never calls into at all under
   this configuration (not blocked -- just not on the current call graph).

**Checked, and it points at hypothesis 1.** For each of the 6 tasks created
after the first burst (prio 7/8/7/5/6/2), coverage tracking shows the RTOS
scheduler *did* switch into every single one of them -- their entry addresses
are all in `seen`, each followed by a small additional cluster of newly-hit
addresses (6 to 31 distinct addresses within a few hundred bytes of its entry
point). So `task_start` is not merely called on all 10 tasks
(`task_start hits: 10`, already known) -- the scheduler genuinely gave CPU
time to the 6 newest ones, each ran a handful of real instructions, and then
every single one went quiet with no further coverage growth for the rest of
a 450M/1.4B-instruction run. That is the signature of each one reaching its
own short init sequence and then hitting a genuine blocking wait very
quickly -- not of the scheduler failing to reach them (hypothesis 2, now
effectively ruled out) and not of them running unboundedly (they are not
CPU-bound). **Concrete next step**: for each of these 6, disassemble the
handful of instructions right past where its coverage cluster ends -- that
boundary is exactly where each one blocks, and is a small, bounded amount of
code to read per task (nothing like the earlier multi-hundred-instruction
transport functions).

### Convergence assessment

Up to 10/16: **converging**. Each fix (semaphore fast-path, two MMIO bit
forces, generalized idle-spin ticking) unlocked either the next blocker or a
burst of several `task_create` sites at once, and the depacker "stall" that
looked alarming turned out to be a false alarm resolved by patience, not a
patch. Past 10/16: **stalled, not diverging** -- one clearly-identified
address, no new blockers appearing, but the fix used for the last two blockers
(generic timer ticking) is confirmed insufficient here and the next fix needs
actual tracing of `0x40288190`'s producer(s), most plausibly tied to one of
the three still-unexercised transport call sites.

### Performance: a 2x+ harness speedup, reusable

`emu/harness.py` and `emu/dspboot.py`'s original hot path ran a single global
`UC_HOOK_CODE` callback on *every instruction*, which for the FF1/MOVEC
patches did a `mem_read` + `struct.unpack` unconditionally to check "is this
one of the two rare opcodes" -- on every single instruction of the run, not
just the rare ones. `Machine.install_isa_patches_scoped` (new) pre-scans the
image once for the actual FF1 (`0x04C0`-`0x04C7`, 232 hits) and MOVEC
(`0x4E7A`/`0x4E7B`, 7 hits) opcode addresses and registers a Unicorn hook
scoped to each exact address (`begin=addr, end=addr`) instead. `emu/dspboot.py`
does the same for its own instrumentation points (`fast=True`, the default;
`fast=False` keeps the original global-hook path for cross-checking). Measured
on identical 60M-instruction runs: 146s -> 73s wall-clock, same result
(verified byte-for-byte identical task_create hits, addresses, priorities).
This matters because runs at the scale needed here are 400M-1.4B instructions
(9-25+ minutes each even with the speedup).

### Reusable artifacts from this session

- `emu/dspboot.py` -- the instrumented DSP bring-up harness. Reports
  `task_create` sites reached (with entry/priority/tcb), transport call sites
  hit, semaphore-satisfy count, depack-clamp count, idle-spin addresses found
  and hit counts, distinct-address coverage curve, and stall PCs. Run directly:
  `./venv/bin/python -m emu.dspboot <instruction_limit> <patch_sem 0|1>`.
- `emu/harness.py` -- added `Machine.install_isa_patches_scoped`, a drop-in,
  much faster alternative to `install_isa_patches` for long runs.

### Next blockers, named **[V]**

After reaching 10/16 tasks, the remaining ones start and then go quiet. Measured,
not guessed:

1. **They are not blocked on semaphores.** There are two counting-semaphore pend
   primitives, both with a fast path when count > 0: `0x4000141a` and
   `0x400013a6`. Logging every call to both across 150M instructions finds
   **exactly one** — the already-patched transport wait at `0x40128d0e`. So the
   stalled tasks are not waiting on anything; they are not being scheduled.
   (`emu/blockers.py` does this logging.)
2. **The scheduler is hand-cranked.** Injecting `trap #0` (vector 32 — the
   task-switch trap) is the only thing that helps. Compared at 60M instructions:

   | injected vector | tasks | distinct addrs |
   |---|---|---|
   | 32 (trap #0) | 5 | 38,247 |
   | 66 | 2 | 329 |
   | 221 | 2 | 260 |
   | 222 | 2 | 260 |

   The candidate hardware ISRs installed during init (vectors 221/222/66 →
   handlers `0x400019bc`/`0x400019e6`/`0x40001a10`) are **not** the system tick.
   So we are forcing context switches rather than running a real scheduler.

**The prize is identified.** Task `0x400d3fb6` (prio 7, created at instr 47M) is
the **intro/animation task**: it calls a RNG at `0x40144bd8`, compares results
against `0x7fdf` and `0x3ffe`, and selects among static structs at
`0x4028ae5c`/`0x4028ae6c` — the same neighbourhood as the known intro
`PixelData` at `0x4028ae98`. If that task runs, it draws, and `emu/screen.py`
can capture the result.

**So the next blocker to attack is the scheduler itself**, not another
peripheral: find the real tick source, or drive the context-switch path directly
against the ready-list/TCB structures so tasks round-robin properly. **[O]**

### Snapshots — stop replaying boot **[V]**

Chasing each blocker meant re-running from the entry point: ~32M instructions of
identical setup (the memory-clear loop alone is ~8M) before reaching anything
new, then 15M more to the next event. Every experiment paid that toll.

`emu/snapshot.py` + `emu/checkpoint.py` remove it:

```
./venv/bin/python -m emu.checkpoint make 40000000 snapshots/boot40M.snap   # once
./venv/bin/python -m emu.checkpoint resume snapshots/boot40M.snap 5000000  # thereafter
```

Checkpoint creation: **42 s**. Resume + 5M instructions: **6.9 s**. Only non-zero
pages are stored (22 of 134 mapped), so 23 MB of live memory compresses to
1.8 MB on disk.

**Resume must happen inside `dspboot.run`, not onto a bare Machine.** The first
attempt restored state onto a fresh `Machine` with only the base hooks, which
silently dropped the flash HLE, the semaphore patch and the scheduler tick — and
diverged by ~50 addresses over 5M instructions while looking plausible. Fixed by
`restore_into()`, which loads onto an already-hooked Machine. Verified: snapshot
at 40M + 5M resume gives **coverage identical** to a straight 45M run (37,616
addresses both ways).

Snapshots are firmware-derived state, so `snapshots/` and `*.snap` are gitignored.

`make` takes a comma-separated ladder and saves them all in **one** pass, since
saving only reads state and emulation continues afterwards:

```
./venv/bin/python -m emu.checkpoint make 60000000,120000000,200000000,280000000
```

One 5-minute pass produced:

| checkpoint | distinct addrs | tasks | on disk |
|---|---|---|---|
| 60M  | 38,247 | 5 | 1.9 MB |
| 120M | 39,244 | 5 | 2.0 MB |
| 200M | 42,234 | 5 | 2.3 MB |
| **280M** | **47,335** | **9** | 2.4 MB |

Resuming from 280M reaches 9 tasks in **10 seconds**.

Two things that immediately became visible once iteration was cheap:

- The apparent "stall" at `0x40175288` is **`__mulsf3`** — a softfloat multiply
  (23 shift-and-add iterations = float mantissa). The stall metric flags any hot
  address after a window with no *new* coverage, so ordinary hot arithmetic looks
  like a hang. Not a blocker.
- Boot is **still progressing** past 280M, just slowly: 47,335 -> 47,637 distinct
  addresses over 60M further instructions, arriving in bursts. Not deadlocked.

### The scheduler was never running **[V]**

With cheap iteration, the actual state at the 280M checkpoint turned out to be
much simpler than "many blockers":

- **Zero context switches in 20M instructions.** The tick only fired at idle-spin
  addresses, and a *busy* task never reaches one. So one task held the CPU
  outright and the other eight never ran.
- **Preemption must respect the interrupt mask.** An earlier attempt at periodic
  `trap #0` injection crashed the machine (`pc=0`). The cause was injecting
  regardless of `SR`; a maskable interrupt cannot fire at IPL 7. Skipping
  injection when `(SR & 0x0700) == 0x0700` makes it stable — 100 injections,
  99 switches, no crash.
- **Scheduling is priority-based, not round-robin.** The ready-list cursor at
  `0x4094c914` points at a node whose `->next` is itself, i.e. a single ready
  task. Lower-priority tasks starve until the running one blocks — and our mocks
  are precisely what stop it blocking.

**The task holding the CPU is the intro task** (`tcb=0x43135210`, prio 7). It is
not stuck: outside the softfloat library it sits at `0x400d3900`, inside
`intro_dither`, doing per-pixel float work with the constants `0x3c000000`
(= 1/128) and `0x3f800000` (= 1.0) — consistent with normalising x across the
128-pixel-wide panel. It is generating the boot animation, just very slowly:
software floating point, per pixel, at ~300k emulated instructions/sec.

The known intro framebuffer at `0x43139290` is still all zeros after +100M, so
either a frame has not completed or the output goes to a different buffer.
Finding it is the next step — watch writes issued from the `0x400d3xxx` code
range. **[O]**

### Where the intro writes its pixels **[V]**

Watching writes issued from the `0x400d3xxx` code range (via the new `pre_start`
hook on `dspboot.run`) gives two destinations:

| destination | writes | what |
|---|---|---|
| `0x43135000` | 161,621 | the intro task's own stack (`tcb=0x43135210`) — not output |
| **`0x44f52000`** | 1024 per 4KB page | **the pixel work buffer** |

1024 longword writes per 4KB page means every word is written. The footprint is
**128 x 64 x 4 bytes = 32,768 bytes** — a float per pixel, matching the softfloat
work and the `1/128` normalisation constant.

Reading it back after +120M from the 280M checkpoint: **7,925 of 8,192 floats
non-zero**, and rendering with a relative-intensity ramp shows clear structure
with mirror symmetry — a smoothly varying field, consistent with `intro_dither`
generating a dither/noise field rather than a finished logo.

So the pipeline appears to be: generate a float field at `0x44f52000` -> combine
with a source image -> threshold into a `Bitmap`. The values are very small in
absolute terms (both min and max print as 0.0000 at 4dp), so this is an
intermediate, not the final image. **[O]** The `Bitmap` at `0x43139290` is still
zero, so the threshold/copy step has not run yet in emulation.

### The intro buffer is a particle array, not a framebuffer **[V]**

The 32,768-byte buffer at `0x44f52000` is **not** floats, despite sitting next to
heavy softfloat use. Only byte 3 of each 32-bit word is ever non-zero, so these
are small big-endian integers. Splitting them by parity settles it:

| | range | meaning |
|---|---|---|
| even indices | 0..127 | **x** — panel width |
| odd indices | 0..63 | **y** — panel height |

It is an array of **4,096 (x, y) particle positions** — the state of the boot
animation, which is what `intro_dither` animates. Rendering the captured buffer
plots all 4,096 in range and shows clear left-right mirror symmetry.

So the chain is: animate particles at `0x44f52000` -> rasterise into the 8bpp
`PixelData` at `0x43139290` -> `>>2` into `0x43137290` (loop at `0x400d3628`)
-> `px_copy_to_bitmap` -> `Bitmap`. Both 8bpp stages are still zero in emulation,
so the rasterise step has not run yet. **[O]**

Note this corrects the previous entry, which read the buffer as floats and
described it as a dither field. The values that made it look like a smoothly
varying field were coordinates.

### The firmware's draw path has not executed — timeboxed negative **[V]**

Two traces from the 280M checkpoint, 100M instructions each:

- **No reads of the particle array** at `0x44f52000` — so nothing has consumed
  the animation state yet.
- **No writes to either 8bpp buffer** (`0x43139290`, `0x43137290`).
- **`Bitmap::setPixel` (0x40104eb4): 0 calls. `px_copy_to_bitmap` (0x400d315e):
  0 calls.**

So the intro task is still in its *compute* phase — animating particles — and the
rasterise/draw stage begins later, or waits on something not yet satisfied. No
callable entry point for it was found, so per the agreed timebox this stops here
rather than becoming another grind.

What we do have: the animation state itself is readable and renderable
(4,096 particles plotted on the real 128x64 geometry), and the final stage
(`px_copy_to_bitmap` -> `Bitmap` -> decode) is independently verified pixel-exact
by `emu/screen.py`. Only the middle link — particles to 8bpp raster — is missing,
and it is missing because it has not *run*, not because it is not understood.

### Emulation is 9x faster than it was **[V]**

The per-instruction Python hook used for coverage tracking capped throughput at
~300k instr/sec. Almost none of it was necessary: every HLE side effect lives at
a known address, and Unicorn hooks registered with `begin == end` cost nothing
between hits. The one thing that appeared to need a global hook — the preemption
tick, fired on an instruction count — doesn't: `emu_start(pc, 0, count=N)`
returns after N instructions, so the tick can be driven by *chunking* instead.

`emu/fastrun.py`: **2.72M instr/sec**, a 9x improvement. A billion instructions
is now ~6 minutes rather than an hour.

### The firmware has a serial command console **[V]**

MAIN OS carries a command protocol, dispatched by a `strcmp` chain at
`0x400cd93e` onward:

`#HELLO` -> `HOW DO YOU DO?`, plus `#BREAK`, `#UPGRADE`, `#FULL_UPGRADE`,
`#WRITE`, `#DUMP_AUDIO`, `#RECEIVE_AUDIO`, `#PLAY_STEREO`, `#VERIFY_SAMPLES`,
`#ENTER_TEST_MODE`, `#EXIT_TEST_MODE`, and status replies `READY FOR OS`,
`READY FOR BOOTSTRAP`, `READY FOR SAMPLE DATA`.

Key handles:
- **`0x400054b4` is the print function.** Hooking it captures all console output
  regardless of transport — no UART modelling needed.
- The console is a **task**, entry `0x400cd594`, priority 2 — one of the 16
  `task_create` sites, and one our boot has not reached.
- UART8 is modelled properly in `emu/console.py` (USR8 `0xEC070004` with real
  RXRDY/TXRDY, data register `0xEC07000C` popping queued input and capturing
  output) rather than pinned to a constant.

Starting the console task manually from a snapshot runs but yields almost
immediately into an idle spin, so it needs more of the system up first. **[O]**

## Making the emulator actually run -- session 3

Marks: **[V]** verified in this session, **[C]** corrects an earlier claim,
**[O]** open.

### The scheduler never worked, and one line explains it **[V][C]**

Every run before this one scheduled exactly **one task**. The stated ceiling in
`docs/NEXT.md` -- "1 new task per ~250M instructions, and the gaps are
widening" -- was not a property of the firmware. It was this bug.

`harness.raise_vector` pushed the *current* PC into the exception frame. The
RTOS yields with `trap #0`, and Unicorn reports a trap with PC still pointing
**at** the trap instruction. So every task that blocked in `sem_pend` got a
stack frame that resumed onto its own `trap #0`. The instant the scheduler
restored it, it trapped again. Tasks could block but could never wake.

The evidence is direct: `emu/tasks.py` decodes each TCB's parked PC, and
before the fix all eight blocked tasks sat at `0x40001486` / `0x40001414` --
the `trap #0` instructions themselves. After it they sit at `0x40001488` /
`0x40001416`, the `move.w d0,sr; rts` that follows.

`raise_vector` now takes `from_instruction=True` from the interrupt hook and
advances the pushed PC by 2 when the faulting word is `0x4E40-0x4E4F`.
Asynchronous injections still push the interrupted PC, which is correct.

**Distinct TCBs scheduled: 1 -> 5.** `emu/oracle.py` and
`emu/screen.py selftest` both still pass.

### TCB layout, from the context switcher **[V]**

`0x40000410` gives it away:

    movea.l $47d9adb4,a0        ; current TCB
    movem.l d0-d7/a0-a7,$c(a0)  ; registers at TCB+0x0C
    move.l  -4(a7),$2c(a0)      ; => a0 at +0x2C, a7 at +0x48
    movea.l $4094c914,a1        ; ready-list cursor
    movea.l (a1),a0 ; movea.l (a0),a0   ; TCB+0x00 = next pointer

A parked task's PC is on its own stack: ColdFire pushes two longwords,
`[a7]` = format/vector/SR and `[a7+4]` = PC. `emu/tasks.py` prints the whole
table plus the ready list from any snapshot.

**Priorities run low-number = low priority.** prio 0 and 1 are the init/idle
tasks; the real work is at 5-10.

### `0x400cf3e0` is not an idle spin needing ticks **[V][C]**

`emu/dspboot.py`'s comment calls it "a different task/thread's idle point"
that was blocking progress. It is actually where the prio-1 init task **parks
after finishing its work**, reached by the `bra.b` at `0x400cf3f4` at the end
of its main loop:

    400cf3e2  jsr $4011311c
    400cf3e8  jsr $4014635e
    400cf3ee  jsr $400329ee
    400cf3f4  bra.b $400cf3e0     ; -> bra self

Feeding it timer ticks does nothing, because it is a *ready* task at priority
1 and the scheduler correctly keeps choosing it. It parks there because
everything above it is blocked.

### A boot-mode flag word at `0x40288190` **[V]**

Two bits of it gate real behaviour in the init task, and its value in every
snapshot is `0x00000004`:

| bit | test site | effect when set |
|---|---|---|
| 5 (`0x20`) | `0x400cf386` | creates and starts the **serial console task** |
| 6 (`0x40`) | `0x400cf3d8` | falls into `bra self` at `0x400cf3e0` -- deliberate halt |

Bit 5 clear is why the console task never existed. Setting it before the init
task reaches `0x400cf384` creates it:
`TASK entry=0x400cd594 prio=2 tcb=0x40383e58`.

Note bit 6 is a *halt*, not a hang: `beq` past it is the normal path. The
earlier reading of `0x400cf3e0` as an idle spin conflated the two.

### The six task_create sites that never fire **[V]**

`0x400cd594` has no absolute reference anywhere in MAIN OS -- it is pushed
PC-relative (`pea.l $400cd594(pc)`), which is why searching for the address
found nothing. Reading the entry operand out of each unreached site:

| site | entry | prio | |
|---|---|---|---|
| `0x400ced72` | `0x400cd594` | 2 | serial console |
| `0x401135a8` | `0x401136ee` | 3 | |
| `0x401279e8` | `0x40127c78` | 4 | |
| `0x40127a7c` | `0x40127d9e` | 4 | |
| `0x40127960` | `0x40127b24` | 5 | |
| `0x40125fde` | `0x4012606a` | 6 | |

### Every task waits on a device event that never happens **[V]**

With the trap fix in, tasks block properly -- and then all of them block, on
semaphores that only real hardware would post. `dspboot` already force-satisfies
one such semaphore (the DSP transport completion sem). Generalising that to
*any* pend whose count is <= 0 is `longrun.build(unblock=True)`.

Sweeping all ~20 installed device ISRs and injecting each one wakes nothing:
the two that look like timers (`0x400cf424` vec 65, `0x400cf450` vec 68)
dispatch a one-shot callback pointer that is null, so they are timeout slots,
not the event source.

### The panel draws **[V]**

With `unblock=True` from `boot400M`, `Bitmap::setPixel` executes for the first
time in this project: **688,128 calls = exactly 84 frames of 128x64**, all into
one Bitmap object at **`0x4313b298`** -- the panel framebuffer instance, which
was previously unknown. The rendered frame is the Elektron logo.

`emu/frame.py` captures it and writes a PNG. This is firmware code drawing
through the firmware's own `setPixel`; nothing about the raster is
reimplemented.

Note this also settles the older open item: the intro's rasteriser does run,
and reaching it needed no new entry point -- only a scheduler that works.
`unblock=True` does change semantics (nothing ever really waits), so
inter-task ordering under it is not the hardware's.

### Resume fidelity: the chunk-boundary tick was corrupting runs **[V][C]**

`longrun.spin` injected a vector-32 trap at every 500k-instruction chunk
boundary. Vector 32 *is* `trap #0`, the scheduler yield, so this forced a
reschedule in the middle of arbitrary code. A run resumed from `boot200M` then
never reached the init task's own flag test at `0x400cf384`, while
`dspboot.run(resume_from=...)` reproduced the from-entry timeline exactly
(task creations at n=257642531, 257710409, 257710486, 257710556, 416346994).

`spin()` no longer ticks by default. `build()` instead installs the two
behaviour hooks it had been missing -- the depack copy clamp and the idle-spin
ticks -- so it now matches `dspboot.run` while staying ~2x quicker.
This is trap 4 in a subtler dress: the hook set, not just the Machine, has to
match the run that produced the snapshot.

### Emulation is 3.2x faster again **[V]**

`longrun.build` was calling `install_isa_patches` (a Python callback on every
instruction) where the snapshots had been made with
`install_isa_patches_scoped`. Switching to scoped: **0.79 -> 2.80M instr/sec**,
with byte-identical state (same PC, `ff1=120821`, `movec=4`, same particle
count). `isa='global'` remains available.

### The console blocker, named exactly **[V][O]**

With bit 5 set the console task starts, runs **22 instructions**, and blocks --
never reaching the UART read or the `strcmp` dispatcher. The last instruction is

    400cd5e6  pea.l $40388eac.l
    400cd5ec  jsr   $40001928.l      ; queue-receive on the console input queue

`0x40001928` is a ring-buffer queue receive. Reading it:

    a2 = queue (0x40388eac)
    d2 = a2 + 8                 ; the semaphore, 0x40388eb4
    loop: if 4(a2) == 0 { pend(a2+8); repeat }
    ...  head/tail at 0x1c(a2), mask at 0x10(a2), buffer at 0x14(a2)

So the console needs an *item enqueued*, not merely a semaphore post -- posting
`0x40388eb4` via the firmware's own post primitive (`0x4000148c`, driven from a
synthetic ISR) lets the pend return, but `4(a2)` is still 0 so it loops
straight back. Confirmed: 0 instructions of console-task code execute.

`0x40388eac` has only three static references, all inside the console task and
its own creation, so the producer reaches the queue through a pointer --
most likely the object at `0x40303e50` registered at `0x400cd5a8`
(`jsr $40110592`) right before the receive loop. **That registration is the
thread to pull next.** **[O]**

Also worth noting for whoever picks this up: the console protocol words are
`#HELLO`, `#BREAK`, `#UPGRADE` and friends -- not `help`.

## Why the emulator was slow: 93% of it was soft-float **[V]**

The GUI ran at ~1.3 firmware frames/sec. Profiling found the cause is not the
harness at all -- a minimal machine (scoped ISA patches + mmio + exceptions)
runs at 2.16M instr/sec and the full hooked machine at 2.19M, so **every hook
in this project is free**. Chunk size makes no difference either.

> **Corrected 2026-09-13.** This section used to end "~2.2M instr/sec is
> simply what Unicorn's m68k core does here." That is wrong, and it steered
> later decisions. ~2.2M is what the core does *when `count=` is passed to
> emu_start*, which Unicorn implements by counting every instruction and
> which breaks TB chaining. The same machine with the identical hook set and
> the identical stop mechanism runs at **15.5M instr/sec uncounted -- a 7.6x
> tax**, and a cProfile of the running configuration puts **99.5% of wall
> time inside emu_start** with every Python callback in this project under
> 0.5% combined. The hooks really are free; the ceiling was never the core.
> `longrun.spin(fast=True)` buys it back for interactive use, at the cost of
> timers landing on a block boundary rather than an exact instruction. The
> paragraph below -- "the only way to go faster is to execute fewer
> instructions" -- followed from the wrong premise.

So the only way to go faster is to execute fewer instructions. An exact PC
histogram over the intro says where they go:

| region | share |
|---|---|
| `0x40174000-0x40176000` (soft-float) | **93.2%** |
| everything else | 6.8% |

This ColdFire build has no hardware FPU, so every float operation is a
libgcc-style routine, and the particle animation is float-heavy.

### Identifying the routines, rather than guessing

Entry points were found by watching which addresses execution *enters* the
region at (transitions from outside it), then identified by calling each one
with known values and comparing against real arithmetic:

| entry | routine | share |
|---|---|---|
| `0x40175204` | `__mulsf3` | 46.3% |
| `0x40174f1c` | `__subsf3` -- `bchg.b #$1f,$8(a7)` then falls into add | 22.1% |
| `0x40174f22` | `__addsf3` | |
| `0x40175346` | `__divsf3` | 4.5% |
| `0x40175a94` | `__fixsfsi` (float -> int, truncate) | |
| `0x40174134` | `fabsf` | |
| `0x40175834` | float compare -> -1/0/1 | |

The other hot addresses in the region (`0x40175644`, `0x4017550a`, ...) are
internal helpers of these, so intercepting the entries removes them too.

### The firmware's float routines are not IEEE-754 **[V]**

Comparing a native implementation against the firmware's own code found
systematic disagreement, all at the edges: the add returns **-0.0 on exact
cancellation** where IEEE gives +0.0, and it gets **infinity signs wrong**
(`-1.0 - inf` yields `+inf`). `__fixsfsi` returns `0xFFFFFFFF` for
out-of-range input, which is undefined behaviour in C.

Rather than replicate those quirks, `emu/softfloat.py` intercepts **only the
fast path** -- finite arguments producing a finite, normal, non-zero result --
and falls through to the real routine for everything else, which then defines
the answer by construction. Same shape as the sem_pend patch, which takes the
primitive's own fast path instead of reimplementing it.

For normal values this is not an approximation: computing in float64 and
rounding once to float32 gives exactly the correctly-rounded float32 result
for +, -, * and /, since 2*24+2 = 50 <= 53 bits. `uv run python -m emu.softfloat`
checks all seven routines against the firmware's: **1,774 intercepted cases,
0 mismatches**, 1,154 edge cases deferred.

### Then setPixel became the bottleneck **[V]**

With the float work gone, the soft-float region fell to 1.9% and the top cost
became `Bitmap::setPixel` (`0x40104eb4`) plus `getPixel` (`0x40104f80`) at
~54% combined -- the rasteriser touches all 8,192 pixels per frame and reads
many back. Both are small, fully understood bit-twiddlers, HLE'd in
`emu/hle.py`. Two details matter: the bounds comparisons are **signed**, and
the value is tested with `btst.b #0`, so **val=2 clears a pixel**. The HLE
writes the same bits into emulated memory, so anything reading the bitmap back
sees identical state. Verified: 680 cases, 0 mismatches.

### Result

| configuration | fps | instructions for 12 frames | |
|---|---|---|---|
| all emulated | 1.32 | 20,750,000 | 1.0x |
| + soft-float HLE | 2.37 | 9,250,000 | 1.8x |
| + bitmap HLE | 4.06 | 3,750,000 | **3.1x** |

Frames are **pixel-identical** across all three, which is the gate that makes
the optimisation trustworthy.

What is left is mostly the rasteriser itself (`0x400d3d7e` 39%, `0x400d3bea`
12%) -- the firmware logic the whole exercise exists to watch, so HLE'ing it
would defeat the point. Another ~17% is scattered math worth maybe 1.2x more.

Both HLEs are **off by default** in `longrun.build`. They are bit-exact so
program state evolves identically, but instruction *counts* change, and too
much here depends on a resumed run matching the run that made its snapshot.
`emu/frame.py` and `emu/gui.py` opt in; `FAST=1` turns them on for the
`emu.longrun` CLI.

The GUI's own redraw was measured at 0.94ms (~2% of a core) and was never the
problem; it now skips redrawing when no pixel changed, and reuses one zoomed
image instead of allocating per frame.

## The intro is meant to run at 15.00 fps, and the bus clock is 132 MHz **[V]**

> **[C] 2026-09-29.** The rates in this section use a PIT prescaler of
> 2^(PRE+1). The divisor is 2^PRE, so the intro runs at **30 fps**, the RTOS
> tick is 100 Hz and PIT2 is 120 Hz; the 132 MHz bus clock stands. See
> "Timers at the device rate" at the end of this file.

"How fast should it be?" is answerable exactly, not by eye.

**The draw loop is paced by a semaphore, not by how fast it can go.** The task
at `0x400d3fb6` ends in:

    400d402a  jsr (a2)              ; render one frame  (a2 = 0x400d3e94)
    400d402c  tst.b d0
    400d4030  pea.l $43131200
    400d4036  jsr (a3)              ; a3 = 0x400013a6, sem_pend
    400d403a  bra.b $400d402a

and `0x43131200` is posted by the ISR at `0x400d2d70`, installed at vector 208
(`move.l #$400d2d70,$40000340` at `0x400d3a5a`), which acknowledges PIT3 and
calls sem_post. **One PIT3 interrupt = one frame.**

PIT3 is configured at `0x400d3a7a`: `PCSR = 0x0936` (PRE=9, so prescaler
2^10 = 1024), `PMR = 0x2191` = 8593, then `PCSR |= 9` (EN|PIE). One frame is
therefore `(8593+1) * 1024 = 8,800,256` bus cycles.

**The bus clock comes from the UART, not a guess.** The serial init computes
its baud divider at `0x400024a4`:

    4000245a  lsl.l  #5,d0          ; baud * 32
    400024a4  move.l #$07de2900,d2
    400024b0  divs.l d0,d2          ; divider = f_bus / (32 * baud)
    400024ce  move.b d2,$ec07001c   ; UBG2

`0x07DE2900` = **132,000,000**, and the ColdFire UART divider is exactly
`f_bus / (32 * baud)`, so that constant is f_sys/bus clock.

It cross-checks against all four PITs landing on round rates, which is what
makes 132 MHz trustworthy rather than merely plausible:

| timer | PMR | prescaler | bus cycles | period | rate |
|---|---|---|---|---|---|
| PIT0 (RTOS tick) | 41249 | 64 | 2,640,000 | 20.0000 ms | **50.0000 Hz** |
| PIT2 | 17187 | 128 | 2,200,064 | 16.6672 ms | **59.998 Hz** |
| PIT3 (intro frame) | 8593 | 1024 | 8,800,256 | 66.6686 ms | **14.9996 Hz** |

So the boot animation runs at **15 fps** on hardware, the RTOS tick is 50 Hz,
and PIT2 is a 60 Hz something. PIT1 is set up at `0x40128d34` in the DSP
transport path.

### What that says about the emulator

The GUI reaches ~4.2-4.7 fps, i.e. **~30% of real time**, and the status line
now reports it that way instead of leaving it to the eye.

Arithmetic for closing the gap: real hardware runs ~1.73M instructions per
frame; with both HLEs on we execute ~312k. At Unicorn's ~2.2M instr/sec that
is ~7 fps of headroom before hook overhead, and we measure 4.2-4.7. Reaching a
true 15 fps needs ~147k instructions per frame. The remaining scattered math
(~17%) is worth maybe 1.3x; past that the cost is the rasteriser itself
(~51%), so matching real time would mean reimplementing the very thing the
emulator exists to watch.

### unblock=True removes the pacing -- and distorts boot **[V]**

Because `unblock=True` satisfies *every* wait, it satisfies the frame
semaphore too: the animation runs unpaced rather than at 15 fps.

Excluding `0x43131200` and driving vector 208 from a modelled PIT3 was tried
and **does not work on its own**: the ISR fires and the semaphore count climbs
(observed reaching 33), but the draw task never runs, because with every other
wait satisfied the prio-6 task never yields and the scheduler never
reschedules. Faithful pacing needs cycle accounting so the 50 Hz RTOS tick can
preempt as well. `longrun.build` now takes `unblock_except` for whoever picks
this up.

Worth noting: leaving the frame semaphore unsatisfied changed the boot path
and created **two further tasks**, including `0x4012606a` (prio 6) -- one of
the six that never appear under blanket unblock. That is more evidence that
blanket unblock distorts boot, and a hint for reaching the remaining tasks.

## Correct-speed playback **[V]**

Emulating at the real 15 fps needs ~3x more throughput than we have, but the
frames themselves are correct and pixel-identical to a fully emulated run --
so the animation can be *shown* at its true speed even though producing it is
slower. `emu/gui.py` keeps every completed frame and its **Replay 15fps**
button plays them back at `FRAME_HZ`, self-correcting for drift. Measured 83
frames cycling at ~14-15 fps against the 14.9996 target.

That separates the two things that were conflated: emulator throughput (30% of
real time, and bounded by the rasteriser) versus what the animation actually
looks like on the device (now viewable).

## The serial console works **[V]**

    #HELLO            -> HOW DO YOU DO?
    #BREAK            -> OK
    #UPGRADE          -> READY FOR BOOTSTRAP
    #ENTER_TEST_MODE  -> OK
    #EXIT_TEST_MODE   -> OK
    #NOPE             -> (no reply, correctly rejected)

`uv run python -m emu.serial console '#HELLO'`.

The last piece was realising **what the console queue actually carries**. It
does `sscanf(item, "%s", buf)` (format `'%s'` at `0x4022A912`, via
`0x400CC93A`) and then strcmps `buf` against its command table using strcmp at
`0x4017C300`. So a queue item is a **pointer to a NUL-terminated string**.

That is why routing the raw serial stream at it did not work. The chain from
DMA does deliver messages -- pointing the sink at the console queue made the
console wake and run its dispatcher twice -- but those messages are the
timestamped 16-byte records built at `0x40110D40` by what is really a
MIDI-style router (8 ports, a timestamp from `0xFC07000C` at `+0x0C`), not
text. The dispatcher ran and matched nothing, exactly as it should.

Two things found along the way:

- **The serial sink is a global**: the producer pushes the destination queue
  from `[0x4029D864]`, and `0x401109E0` is `set_serial_sink(queue)` (its only
  caller is `0x40033130`). By default it points at `0x47D9ADC0`, a queue that
  **no `queue_receive` call site in the firmware reads** -- there are only
  three such sites in the whole image, for queues `0x4094EF3C`, `0x40388EAC`
  (console) and `0x44E0C290`.
- The console task at `0x400CD594` is created only when **bit 5 of
  `0x40288190`** is set (see the boot-mode flag section), and it registers
  `(0x80008, 0x40303E50)` into `0x44DADD0C`/`0x44DADD10` at `0x400CD5A8`.

`emu.serial.send_command` enqueues through the firmware's own `queue_send`
(`0x40001896`), so the semaphore is posted and the task woken exactly as it
would be normally; output is captured by hooking `print` at `0x400054B4`.

> **Note 2026-09-28 [C].** The addresses in this section and in "A boot-mode
> flag word at `0x40288190`" are 1.15C. For 1.16 see "The 1.16 serial
> console" at the end of this file.

**`#UPGRADE` answering `READY FOR BOOTSTRAP` matters for work item B**: the
firmware-upload path is now drivable under emulation, so a patched image can
be pushed at the device's own acceptance logic without touching hardware.

## Boot reaches the main OS: the stall was eDMA, not a semaphore **[V]**

The previous session's conclusion -- that `unblock=True` was needed for the
intro and poisoned everything after it, and that `0x400d404a` was unreachable
in 900M instructions -- had the right symptom and the wrong cause. The intro
was not failing to *exit*; it was failing to *finish*. It stopped rendering at
exactly frame 88 of 175, every time, and never got near its exit path.

### The intro's own termination condition

`0x400d3e94` (the render call at `0x400d402a`) returns 0 when the intro is
over, and it is driven purely by call count, not by time:

| | |
|---|---|
| scene count `[0x4313b290]` | 1 |
| scene table `[0x4313b294]` | `0x4028ae2c` |
| draw fn / frames | `0x400d3ab6` / **175** |

So the intro is 176 render calls and nothing else. It is not waiting for a
timer, and there was never a reason it could not finish.

### Where it actually stopped

`0x4000220c` is the firmware's "queue bytes for the console" routine. It opens
with a spin loop waiting for room in a 4096-byte ring at `0x4FE1B000`:

    4000221c  d2 = [0x4094cd90] + len          ; bytes wanted
    40002232  d1 = w[0xFC045474]               ; TCD35.CITER
    40002238  d3 = w[0xFC04547C]               ; TCD35.BITER
    40002244  d1 = (d1 - d3) + ([cd88] - [cd94])
    40002246  d1 &= 0xfff                      ; -> bytes still in the ring
    40002252  if 0x1000 - d1 < d2: goto 4000221c

Nothing advanced that channel, so the ring never drained. The intro draw task
span there at priority 7 and starved everything -- which looked exactly like
the priority-7 busy-spin `unblock=True` was blamed for.

### Channel 35 is UART8 transmit

TCD35 at `0xFC045460`, in the **ColdFire** eDMA layout where CITER is at +0x14
and BITER at +0x1C (not the Kinetis order):

    SADDR  = 0x4FE1B000   ring; ATTR = 0x6000 -> SMOD 12, source modulo 4096
    NBYTES = 1            one byte per request
    DADDR  = 0xEC07000C   UDR8, DOFF = 0

`0xFC044018` is EDMA_SERQ (start), `0xFC044019` CERQ (stop). Vector **155**
points at `0x40001e7c`, the channel-35 completion ISR -- ch34 (RX) is 154, so
the vectors are contiguous. The ISR clears EDMA_CINT, sets `[cd94] = [cd88]`,
and either parks the channel or chains the next transfer.

`emu/edma.py` runs the whole major loop on a SERQ write, advances SADDR with
the ring modulo, reloads CITER from BITER as hardware does at major-loop
completion, and raises vector 155 so the firmware's own ISR does the
bookkeeping. The completion is queued rather than raised inside the write
hook: the enqueue routine writes SERQ with SR = 0x2700, so hardware could not
deliver it there either.

The firmware ring fields, all confirmed against the enqueue routine and the
ISR: `cd74` state (0 idle / 1 running / 2 draining), `cd7c` ring base, `cd88`
head, `cd8c` write index, `cd90` bytes queued but not yet handed to DMA,
`cd94` offset fully drained.

### Result

Intro runs all 175 frames, then reaches `0x400d404a`, `0x400d4058` and
`0x400d4060`. Six previously-missing tasks spawn (`0x400f1eb6` prio 2,
`0x4012606a` prio 6, `0x400f1fce` prio 3, `0x40127b24` prio 5, `0x40127c78`
and `0x40127d9e` prio 4) and `0x40000e82` formats real parameter values:
`'%s: %.16s' 'ONE'`, `'FWD'`, `'OFF'`, `'0.00'`.

PIT3's PCSR goes `0x093f -> 0x0000` across the intro, by the firmware's own
`move.w d0,$fc08c000` -- so a PCSR-gated PIT model stops delivering frames
after the intro without being told to. PIT0 (50 Hz, vec 205) and PIT2
(59.998 Hz, vec 207) stay enabled; PIT1 is off.

### `unblock` had to be narrowed, not removed **[V]**

Blanket-satisfying every pend hid a second copy of the same mistake.
`queue_receive` (`0x40001928`) pends on the queue's own semaphore at queue+8
and then **re-reads `queue->count`**. Satisfying the semaphore without also
enqueuing an item turns a sleep into an infinite spin: 8.9M iterations, ~92%
of all post-intro cycles, on one queue.

The fix is caller-based, not semaphore-based, so it generalises to every queue
in the system: never satisfy a pend whose return address is `0x40001946`. The
same shape appears again at `0x401260c2` in the prio-6 task, which pends on
`0x44e2d148` then re-checks a flag at `0x44e2d5cc`.

Caller-based discrimination also removes the intro handoff entirely. The intro
loop pends the frame semaphore from `0x400d4038` and must be satisfied; the
park loop pends the *same* semaphore from `0x400d4068` and must not be. Two
different callers, one rule, no state to hand over -- and it survives a
snapshot taken after the intro.

Post-intro pends satisfied: **8.9M -> 1.** Nine tasks reach clean blocking
waits instead of spinning.

## What actually limits speed: our hook layer, not Unicorn **[V]**

Superseded by measurement. An earlier version of this section reported
"Unicorn m68k ceiling here 2.90M instr/s" and concluded that live 15 fps was
out of reach for Unicorn plus Python hooks. **The ceiling figure was
mis-attributed.** It is the speed of *this workload with our hooks*, not
anything Unicorn imposes.

| measured on this machine | |
|---|---|
| hook-free m68k loop, `count=` on | **250.8M instr/s** |
| our workload, both HLEs on | 1.3-2.5M instr/s |

Two orders of magnitude sit between those, and all of it is ours.

### `count=` on emu_start costs 1.84x

Passing `count` makes Unicorn install an internal per-instruction hook to
decrement the budget, which defeats its fast dispatch path. Over the same 40
rendered frames:

| | |
|---|---|
| `count=250_000`, chunked loop | 8.07s |
| uncounted, stop from a hook at frame completion | 4.39s |

The cost is `count` itself, not the number of emu_start calls: over the same
100 frames, `count=20k` (1308 calls), `count=500k` (53 calls) and `count=1e9`
(1 call) all land within 3% of each other. Chunking was never the price.

`emu/longrun.py:run_until` is the uncounted form. Stop only from a hook that
has already advanced PC past the current instruction -- the setPixel HLE
writes PC = return address, so it qualifies. Stopping from a plain code hook
leaves PC on the hooked address and the resume re-enters the same hook
immediately: the run then spins making no progress while appearing to
iterate, which is how an early attempt at this measured a fictitious 118x.

`emu/gui.py` now stops per completed panel frame instead of every 250k
instructions: **~8.7-9.3 fps during the intro, ~58-62% of the real 15.00 Hz,
against ~30% before.**

A hook-only stop condition also needs a wall-clock floor, or the caller hangs
the moment the firmware stops meeting it -- the end of the intro does exactly
that. **A timeout is free, unlike `count`.** Same 40 frames, identical 327,681
setPixel calls each way:

| | |
|---|---|
| `count=250_000` | 8.03s |
| uncounted | 4.45s |
| uncounted + 0.5s timeout | 4.43s |

`count` installs a per-instruction hook; a timeout only arms a timer thread.
Bound wall-clock time freely; bound instruction counts only when something
genuinely has to happen per fixed number of instructions.

### Where the remaining time goes

cProfile over 10M instructions, both HLEs on:

| | share |
|---|---|
| `emu_start` -- Unicorn actually executing m68k | 34% |
| Unicorn's Python ctypes binding | ~54% |
| our own handler logic | ~12% |

1.02M of 10M instructions cross into Python. Per crossing we pay a ctypes
`create_string_buffer` allocation for every `mem_read` (1.94M of them) and a
separate FFI call for every `reg_write` (1.85M). The dominant cost is the FFI
boundary, not our logic and not the chip model -- so the next wins are fewer
crossings and cheaper crossings, not a better peripheral model. Real time is
no longer ruled out.

### A measurement mistake worth recording

An earlier attempt to split "hook dispatch" from "handler work" gave dispatch
4% and handler bodies 93%, which looked like large headroom in our Python.
**That reading was wrong.** With no-op handlers the firmware still executes all
the soft-float code, so the two runs cover completely different amounts of
firmware work per instruction -- the comparison was not apples to apples.

Acting on it produced only 8% (4.10 -> 4.43 fps): precompiled `struct.Struct`
codecs and unpacking arguments directly as `>f` instead of bits-then-convert
(the old `b2f`/`f2b` each cost a pack *and* an unpack). Those are worth
keeping. The third change in that batch -- caching Bitmap geometry per pointer
-- was **a bug**, not a win, and has been reverted: the firmware mutates the
fields of an existing Bitmap. See the note in `emu/hle.py`.

The lesson matches the earlier `install_mmio` one, and the fictitious 118x
above, and the "2.90M ceiling" this section replaces: only trust an A/B where
the two sides do the same work.


## Digitakt II 1.16 and Digitone II 1.11 reach the main screen **[D][O]**

Four harness bugs kept 1.16 from getting past boot dialogs. All were in the
harness, none in the firmware. **[D]**

- `emu.gui` refused the 1.16 `.syx`: `devices/digitakt-ii.toml` listed only
  the 1.15C sha256. Both 1.16 and Digitone II 1.11 are now listed. The panel
  code mapping was measured on 1.15C and has not been re-measured on 1.16.
- `display_frame_post` was `Fixed(0x40125f4e)`, a 1.15C address, so on 1.16 it
  and `display_sem` resolved to `None`, `unblock` faked the display semaphore,
  and "INITIALIZING +DRIVE..." never cleared, the stall 1.15C had before
  `display_sem` was excluded. The display module's PIT3 handler masks to the
  same bytes as the intro's, so it is now found by position: the new `SigAt`
  rule matches a masked signature 0x174 bytes before `display_wait`. It
  resolves on DT2 1.15C (`0x40125f4e`, sem `0x44e2d148`), DT2 1.16
  (`0x4013352a`, sem `0x44e460d8`), DN2 1.10E and DN2 1.11.
- `unblock` faked the wait on `worker_done_sem` (`display_sem+8`), which a
  background worker's teardown posts, releasing its caller about 250M
  instructions early.
- `unblock` faked a producer/consumer pair, `bq_free_sem`/`bq_ready_sem`
  (`0x44e1dc88`/`0x44e1dc90` on 1.16), both posted from task code. With it,
  "FACTORY PROJECT >> +DRIVE..." froze: the Factory-reset worker disables PIT3
  itself when done (`FUN_40133626`), then no follow-up job was submitted in
  2.5B instructions.

`give` (`0x4000148c`) and `give_b` (`0x400014fc`) are byte-identical RTOS
entry points on all four builds. A scan of every post site with an immediate
semaphore argument, classified by whether the poster is task code or an ISR
of a modelled source, gave the semaphores `unblock` now never fakes:
`display_sem`, `worker_done_sem`, `bq_free_sem`, `bq_ready_sem`, and the
three eSDHC semaphores, which `emu/esdhc.py` posts from host code. The scan
was run once (`scratch/semscan.py`, gitignored, from the Ghidra dumps); each
semaphore resolves per image, but a new firmware's semaphores are not
discovered automatically. **[D][O]**

On first boot the emulated eMMC is blank. The firmware finds no `0xBEEFBACE`
header at byte 0 and runs its own "Factory reset" worker: 2,120 erase groups
(CMD35/36/38), then writes a header sector at 0 and a table at `0x800`. The
card overlay is saved in snapshots. +Drive holds no samples. **[D]**

From `snapshots/dt2-1.16/boot400M.snap` the main screen is up by about 300M
instructions, and from `snapshots/dn2-1.11/boot400M.snap` (a new ladder)
likewise. `tools/emucheck.py` passes both at 700M. The only pends still faked
are 175 in the intro's exit park (`0x400d1930` on 1.16, `0x400d4038` on
1.15C). `tests/test_symbol_audit.py` fails if any symbol the harness uses is
`None` on DT2 1.16 or DN2 1.11; eight trace-only symbols are still fixed 1.15C
addresses. **[D][O]**

## Speed: measured on 1.16, and QEMU is not worth porting **[V]**

`tools/speedab.py` times a fixed guest window from a snapshot (three fresh
repeats; exact mode checks that every repeat ends on the same instruction
count and PC), and has `--crossings` (every Unicorn callback counted by hook)
and `--profile` (cProfile split into Unicorn, binding, our handlers). Run on
`snapshots/dt2-1.16/boot400M.snap`, 50M instructions, on a quiet machine:

| | instr/s | vs `INSTR_PER_SEC` (4.68M) |
|---|---|---|
| exact mode | 1.19M | 0.25x |
| fast mode | 2.0M | 0.43x |
| Unicorn, hook-free two-instruction loop (`tools/qemuceiling`) | 1.09G | 232x |
| QEMU `mcf5208evb`, same loop | 1.48G | 316x |

Unicorn's ceiling is far above real time and QEMU's is only 1.4x higher, so
by the handover's rule a port is not worth it. The loss is in Python
crossings: 43% of time is Unicorn's Python binding (mostly `mem_read`
buffer allocation and per-call `reg_write`), 14% our handlers, 38% native
execution. The top crossing sources per 50M instructions: the bitmap HLE
getPixel/setPixel (2.43M), then softfloat (0.49M). Removing the duplicate
setPixel counter hook made no measurable difference (within repeat noise);
it is kept as a cleanup. This corrects the earlier A/B in
`out/speed-ab/`, whose floor used `count=` and idled.

## Two task counters **[V]**

`emu.dspboot.run` (cold boot, and the checkpoint ladder) counts creates at a
fixed list of boot sites; `emu.longrun.build` (emucheck, guirun) hooks the
RTOS `task_create` and counts only creates after the resume point. They are
different metrics: a stock 1.16 cold boot creates nine boot tasks by
~291M, and a resumed run from 400M creates six different, dynamic workers.
Do not compare one against the other.

## Bounded 1.16 boot-window speed recheck; parameter-traffic fixture remains open **[D][O]**

The local `Digitakt_II_OS1.16.syx` SHA-256
`278541e466edcd77d6b3e018a91fb90185932d3c7de224dd3e68294dddf3a9ec`
matched `out/sections/dt2-1.16/.source-sha256`. From
`snapshots/dt2-1.16/boot400M.snap`, `tools/speedab.py` with
`--sections out/sections/dt2-1.16 --syx Digitakt_II_OS1.16.syx
--snapshot snapshots/dt2-1.16/boot400M.snap --instrs 10000000`
ran three fresh repeats per mode on this machine:

| mode | median wall time | reported guest instructions | median reported rate | final PC |
| --- | ---: | ---: | ---: | --- |
| `--mode exact` | 7.334 s | 10,000,000 each | 1.364M/s | `0x4018446a` each |
| `--mode fast` | 4.449 s | 10,999,956 estimated each | 2.473M/s estimated | `0x400d1690` each |

Exact repetitions agreed on count, PC, and `limit` stop. Fast mode has a
different stream, an estimated count, and a different endpoint: its rate is
an approximate interactive-mode ceiling, **not** an A/B speedup on identical
work. Separate untimed `--crossings --mode exact` and
`--profile --mode exact` over the 10M window recorded 717,756 scoped code
callback firings (459,008 Bitmap get/set callbacks), 836 other memory hooks,
and 418 interrupt hooks. The *instrumented* cProfile tottime split was
45.0% Unicorn Python binding, 35.2% native `emu_start`, 14.2% project
handlers, 5.6% other. Its percentages cannot be directly applied to the
uninstrumented 7.334-s median. Records: ignored
`out/speedab/dt2-116-{exact10m,fast10m,crossings10m,profile10m}.json`.
**[D]**

An attempted more relevant event fixture reached the main screen after
360,662,834 bounded instructions, sent 16 stock machine-selection messages,
and saved a post-selection state. But forced vector-191 TX-frame capture did
not return cleanly (one driver call, zero frame words). From that checkpoint,
encoder channel 2 `+30` produced no observed mirror/panel change; `-30`
produced a panel-only difference at 2M that disappeared by 5M, and neither
case changed any of the 16 raw track-mirror rows inspected. The fixture does
**not** establish parameter traffic, so none of the speed numbers above is
labelled a machine-selection/parameter-throughput result. Ignored diagnostic
scripts and snapshot are under `out/speedab/`; do not treat them as a
validated fixture or commit them. A future benchmark must first demonstrate
a persistent ColdFire parameter or DSP-frame difference against an
uninjected control, then time that same bounded path. **[D][O]**

## Cold-boot ladder speed: the `cover` hook costs 1.94x; ladders skip it by default **[D]**

`emu/dspboot.py:run(fast=True)` (cold boot, and `emu.checkpoint.make`'s
ladder) installs one GLOBAL `UC_HOOK_CODE` hook, `cover`, alongside its
scoped (begin==end) per-address hooks. `cover` only counts instructions,
tracks coverage (`seen`/`stall_pcs`/`curve`), and calls `checkpoint.make`'s
`extra_hook` to notice when a requested rung is reached -- it never touches a
register or memory. A/B on a true cold boot to 60M instructions (no card
image, `sdgate=True`, `esdhc=True`, 3 reps each, strictly alternated,
identical final PC and `TASK_CREATE` timings on every rep): median wall time
49.894s (`coverage=True`, 1.203e6 instr/s) vs 25.682s (`coverage=False`,
2.336e6 instr/s) -- **1.94x**.

`emu.checkpoint.make` now defaults to `coverage=False`: instead of one
`db.run()` call driven by `cover`, it calls `db.prepare()` (the same hook set
minus the global one) and steps itself with one exact
`uc.emu_start(pc, 0, count=delta)` per rung -- the same shape
`emu/longrun.py`'s `extend()` uses on the resume side. Verified on a 30M/60M
ladder and a 60M ladder with a +Drive card image
(`out/plusdrive/dt2.img`): registers, every mapped memory page, mmio,
ctlregs and the FF1/MOVEC counters are byte-identical between
`coverage=True` and `coverage=False` at every rung (`tools/snapeq.py`).
Timed on the same 60M window: 56.0s median (`coverage=True`) vs 26.9s median
(`coverage=False`), consistent with the 1.94x figure above (other jobs were
running on the measuring machine; treat these two as noisy, not a precise
re-measurement). `coverage=True` (`--coverage` on the CLI) still exists for
when `seen`/`stall_pcs`/`curve` are actually wanted; the `.ladder.json`
sidecar records which mode built a given ladder.

## SHARC voice-render tooling: post-init snapshot, watchpoints, survey **[D]**

Support used to reach the "one voice renders correctly" result in finding
06: run `FUN_1c15e3` (init) to its return, then apply setup, to get a
post-init snapshot to start a render from, instead of a full cold boot.
Register/memory watchpoints and `diagnose_unknown` (both in
`tools/sharc_harness.py`) narrow a stuck or wrong-value run to the
instruction that produced it. `tools/sharc_survey.py` runs a bounded batch
of these probes over a function or region in one pass. `Image.cfg`,
`callgraph`, `defuse` and `slice` (`tools/sharc.py`) give the control-flow
graph, static call graph, def/use chains and a backward slice for a
register at an address, used here to trace the record fields and the
past-limit `R12` doubling back to their writers before trusting the
execution numbers.

## Booting with an already-formatted card stalls before `running` **[V][O]**

Every prior 1.16 boot result in this file (the "reaches the main screen"
section above, `tools/bootcheck.py`, `tools/dt2_reach_running.py`'s existing
ladders) was run with **no card image** (`emu.esdhc.Card()`, blank/all-zero
backing) or a card the firmware itself formats fresh during that same boot.
That path is DT2 1.16's **factory-reset branch**; it is not the only boot
path, and it turns out to be the *easy* one.

**Discriminator.** A control image was built containing *only* the two
blocks the firmware itself writes when formatting a blank card -- block 0
(the `0xBEEFBACE` header) and block `0x800` (the pool-occupancy table, all
zero on an empty card) -- extracted byte-for-byte from
`snapshots/dt2-1.16/running.snap`'s `esdhc` card overlay (`emu/snapshot.py`
`_load_blob`, `components['esdhc']['card_overlay']`; see
`docs/findings/14-plus-drive-format.md` for the exact bytes). No +Drive
sample filesystem content at all. Cold-booted with
`emu.checkpoint.make(..., card_image=...)` (`snapshots/dt2-1.16-control/`)
and continued with `tools/dt2_reach_running.py` to **1,000,051,798**
instructions -- 27% past the 785,113,600 instructions a blank-card boot
needs to reach `running.snap`. Result: `PIT3 firing: False`, `vector 208
handed to display: False`, `main_os_running: False` throughout;
`format_driver_hits`, `factory_reset_hits`, `record_resolve_hits`,
`readdir_hits` all stayed 0, and the eSDHC command log never grew past the
two initial CMD18 reads (blocks 0 and `0x800`) the whole run. **This proves
the stall is not specific to +Drive sample content** (task item 1's
discriminator): a card with nothing but a *valid, already-formatted* header
stalls identically to one with a full `tools/plusdrive.py` image.

**Root cause, traced statically to a specific branch.** The Main-OS task
(`FUN_400337ba`, `out/ghidra/dt2-1.16-emac/decomp/400337ba_FUN_400337ba.c`,
spawned at instruction ~32.4M as the priority-6 task, entry `0x400337ba`)
decides among three first-run actions right after its own start:

```c
iVar6 = FUN_4012eb08(mmcfs);              // header state: 0=unread, 1=INVALID, 2=valid
if ((iVar6 == 1) || (DAT_4029e9b0 & 0x10)) {          // (A) blank/invalid header
    FUN_40133586();                                    // <-- starts PIT3 + the display task
    ...spawn "Factory reset" BgWorker (FUN_400334bc)...
} else {
    iVar6 = FUN_4012eae4(mmcfs);          // header flags: byte[8]/[9], see finding 14
    if (iVar6 == 0) {                                   // (B) needs preset migration
        FUN_40133586();                                 // <-- starts PIT3 + the display task
        ...spawn "Migrate presets" BgWorker...
    }
    // (C) neither -- falls through unconditionally:
    ...spawn "Update MMC Caches" BgWorker (completion FUN_40032eaa)...
    // FUN_40133586() is NEVER called on this path.
}
```

`FUN_4012eb08` (`out/ghidra/dt2-1.16-emac/decomp/4012eb08_FUN_4012eb08.c`)
reads the header buffer at `ctx+0x1de70` (finding 14's block-0 layout) and
returns 1 iff the magic is *not* `0xBEEFBACE`. `FUN_4012eae4`
(`.../4012eae4_FUN_4012eae4.c`) reads header bytes `+0x08`/`+0x09` (finding
14's two format flags, both `01` on any correctly-formatted header,
including ours) and returns nonzero when both are set. **A valid header
(byte[8]=1, byte[9]=1, magic correct) makes both checks fail, so branch (C)
is always taken** -- and `FUN_40133586`
(`out/ghidra/dt2-1.16-emac/decomp/40133586_FUN_40133586.c`) is the *only*
place in the whole 1.16 image found to write vector 208, arm `PIT3_PMR`/
`PIT3_BASE` and spawn the priority-6 display task
(`profile.display_start` = `0x401335e0`, inside this function, confirmed
against the live vector-208 check). It is confirmed as `FUN_40133586`'s
sole caller via `out/ghidra/dt2-1.16-emac/xrefs.sqlite`'s `calls` table.

> **Corrected 2026-09-28 [C][V].** `FUN_40133586` does not start the
> display. It is the progress screen ("INITIALIZING +DRIVE...") used by the
> factory-reset and migrate jobs: a prio-6 task `FUN_40133646` draws a
> spinner and a bar and flushes through `FUN_4013390e`, and `FUN_40133626`
> tears it down. Branch (C) draws through the main UI's own flush
> (`0x40032992` -> `FUN_4013390e`). The +Drive boots were stuck behind the
> intro, which the ladder builder does not advance. See "Branch (C) draws
> through its own flush" at the end of this file.

This can't be the whole story for a shipping device -- branch (C) ("Update
MMC Caches") is what an **ordinary second boot** takes on real hardware, and
the screen obviously does come on then. So either the display starts from a
different, not-yet-located path specific to branch (C) (most likely inside
or after the "Update MMC Caches" `BgWorker`'s own completion callback,
`FUN_40032eaa`, or one of the unconditional calls right after the
if/else -- `FUN_400c14dc`/`FUN_400f03e8`/`FUN_4002dcb2`/`FUN_401339aa`/
`FUN_40133e22`/`FUN_40133ac8`, none read yet), or that path depends on a
semaphore `unblock`'s narrowing (the "Four harness bugs" list earlier in
this file) doesn't yet cover, the same way `bq_free_sem`/`bq_ready_sem` had
to be excluded to stop `unblock` from faking the factory-reset worker's own
follow-up job. **Every 1.16 boot validated in this project to date has
exercised only branch (A); branch (C) -- the normal, non-first-boot path,
which is what any +Drive image needs -- is untested territory and its
display-start mechanism is not yet found.** **[O]**: locate branch (C)'s
real display-start call and confirm whether the gap is a missing/miswired
completion semaphore in the emulator or a genuinely slow (not stuck) path
needing a larger instruction budget.

Also confirmed while tracing this (item 1(b) of the +Drive task): the real
filesystem's mount routine, `FUN_4015a450` (called from this same task,
`FUN_400cc864`, not `FUN_400337ba`), requires an on-disk superblock at
absolute sector `0x5D8000` that neither this file nor
`docs/findings/14-plus-drive-format.md` had documented before now -- see
that file's new section for the full layout and checksum.

**Update, a later session:** that superblock is now implemented
(`tools/plusdrive.py`'s `hashlittle()`/`build_superblock()`, checked against
44 real firmware executions) and confirmed self-consistent in a rebuilt
`out/plusdrive/dt2.img`. `FUN_400cc864` gates `FUN_4015a450`/`FUN_4015a424`
behind `_DAT_42940a48 == 0`, set from an eMMC-identity whitelist check
(`FUN_4012dc80`→`FUN_4012db90`→`FUN_4012dbe0`→`FUN_4012da2c`×2 against
`MAIN_OS`'s own 7-entry manufacturer/product-name table) that this superblock
work alone did not satisfy.

**That whitelist check is now fixed and verified, in a later session still.**
Disassembling (not decompiling) the chain found the decompiler had misread
one field's width (the manufacturer-ID read is a 16-bit load then the word's
low byte, not a byte mask on the raw 32-bit CID word the decompile showed --
`emu/esdhc.py`'s existing `self.cid=[0,0,0,0x00110000]` already happened to
place the byte correctly despite that). `emu/esdhc.py`'s `Card.cid`/`csd`/
`ext_csd()` now encode a complete, valid whitelist entry (manufacturer
`0x11`, product name `"004GE0"`, that entry's own two auxiliary fields, and
its capacity through a CSD-1.0 fallback rather than EXT_CSD's `SEC_COUNT` --
encoding the capacity there hung a real boot outright, confirmed by
bisection; see `docs/findings/14-plus-drive-format.md` for the full
derivation and why). Verified two ways: a bounded, isolated call to the real
`FUN_4012dc80` returns 0 (`tests/test_esdhc_identity.py`), and an
instrumented live cold boot shows the *exact same* real values reaching that
function and it returning 0 for real, right after a real CMD9/SEND_CSD
exchange.

**Passing that check for the first time exposed a second, unrelated
blocker immediately behind it -- now fixed.** A corrected, TCB-filtered
instrumented trace (disassembling `FUN_400cc864` directly, not estimating
addresses) showed the boot task legitimately blocking forever in
`sem_pend` on `sd_dma_sem`, inside `FUN_4012e0c0` (the CMD25/multi-block
-WRITE primitive), reached via `FUN_400f0628` (an always-executed,
identity-independent step) right after the identity check. Root cause:
`emu/dspboot.py`'s own `Esdhc(...)` construction (used only for the
*initial* cold-boot pass -- exactly where this hang occurs) never passed
`dma_sem`, unlike `emu/longrun.py`'s construction of the same class, which
always has. `Esdhc._post(None)` is a documented no-op, so the real
eDMA-channel-59-completion `give` this model performs on a write's behalf
never landed; reads (which pend on `data_sem` instead) were unaffected,
which is why only writes hung. **Fixed** by passing
`dma_sem=profile.sd_dma_sem` there too, with a new regression test
(`tests/test_dspboot_esdhc_wiring.py`) -- the `Esdhc` model's own
CMD25-posts-`dma_sem` behavior was already covered by `tests/test_esdhc.py`.

With that fixed, a cold ladder now reaches 400M instructions with 10 tasks
(including the priority-6 Main-OS task) and confirms `FUN_4015a450` (mount)
should be genuinely called (`_DAT_42940a48 == 0` at the boot task's own
stable terminal idle loop). A **third**, previously-undocumented on-disk
structure at sector `0x458000` (a "MaGj"-tagged, two-stage-CRC-32-checked
32 KiB record `FUN_4015a124` reads via `FUN_4002cd6a`/`FUN_4002ccd0`) was
fully pinned down by disassembly (not the decompile, which had misread two
of its fields as one overlapping 32-bit read) and a valid record built
(`tools/plusdrive.py`'s `build_factory_table_record()`) -- confirmed
against the real firmware with a bounded isolated call
(`tests/test_factory_table.py`), same technique as the eMMC-identity
fix's own test.

**That did not, on its own, get a live boot to mount.** With the new
record written and the card ladder rebuilt, the boot task is already
parked in its known terminal idle loop after 1.6B total instructions, the
mount flag stays `0`, and the RAM buffer `FUN_4002ccd0` reads the sector
into (`DAT_47e203cc`) reads back all zero -- meaning either
`FUN_4015a450`/`FUN_4002cd6a`/`FUN_4002ccd0` were never actually reached in
this live run (which a straightforward read of `FUN_4015a124`'s own
unconditional call chain doesn't explain), or the live `CMD18` read of this
unusually large (32 KiB / 64-sector) single request didn't deliver the
image's bytes the way the isolated unit test's direct RAM write did.
Branch-(C)'s own display-start question (`vector208 == intro_pit3_isr`,
still true after 1.6B instructions) also remains exactly as documented
below -- this run never got far enough past mount to newly test whether
the dma_sem fix incidentally helped it. **[O]**: needs scoped-hook tracing
(not a global per-instruction hook, too slow for a run this long) on the
three functions above, resumed from shortly before the expected call
rather than from a cold start -- see
`docs/findings/14-plus-drive-format.md`'s matching section for the full
trace and the CRC derivation.

**Update, a later session: the scoped-hook trace above was done, from a true
cold start (`emu.dspboot.run`, not a `--card-image` resume through
`emu.longrun.build`/`tools/dt2_reach_running.py` -- that resume path was
tried first and gave a non-reproducing, divergent trace: no eSDHC traffic
and no boot-task-entry hit for hundreds of millions of instructions after
resuming a mid-ladder snapshot, most likely because it drives scheduling
through `unblock` (force-satisfy every pending semaphore) where a native
cold boot drives it through `emu.dspboot.run`'s own idle-spin/vector-32 tick
-- the two are not equivalent for this code path and should not be assumed
interchangeable for anything past the point a card-image boot starts real
SD traffic). A native cold boot with `--card-image out/plusdrive/dt2.img`,
code hooks on `FUN_400cc864`/`FUN_400ccb6a`(call site of
`FUN_400f0628`)/`FUN_400ccbde`(pre-mount check)/`FUN_4015a43a`/`FUN_4015a450`/
`FUN_4015a124`/`FUN_4002cd6a`/`FUN_4002ccd0`, and `Esdhc.command_log_enabled`
turned on, shows: the boot task (entry `0x400cc864`) is created at
n≈32.36M, calls `FUN_400f0628` at n≈47.1M (returns at n≈52.66M -- it does
return; it is not an infinite job-pump loop as an earlier pass through this
same investigation, using the divergent resume path, mistakenly concluded
from an unrelated later worker task that happens to reuse the same generic
`BgWorker` job-loop code), reaches the pre-mount check at n≈82.911M with
`DAT_42940a48==0` and `DAT_4029e9b0==0` (both consistent with proceeding),
calls `FUN_4015a43a` then **`FUN_4015a450` is genuinely entered** at
n≈82.911127M -- contradicting this section's own earlier "never reached"
framing, which was built on the divergent resume trace. `FUN_4015a450`
issues its CMD18 for sector `0x5D8000` (confirmed: the stack argument at
its own `FUN_4012deda` entry is exactly `0x5D8000`, length `0x200`, dest
`0x46f4dcb0`), but **`Esdhc.command_log_enabled`'s own log has no entry for
this call at all** -- no XFERTYP write ever reaches the eSDHC MMIO
registers for it, unlike every other CMD18 in the same log (449 entries by
this point, all with a normal `armed=59`/`dma_bytes` record). `DAT_46f4dcb0`
reads back all zero both before and after, `FUN_4015a450` returns
`0xFFFFFFFF` (magic check fails against zero), and the boot task falls
through the rest of `FUN_400cc864` to its terminal spin by n≈83.003M.
**Root cause, pinned down by disassembly of `FUN_4012deda`
(`out/ghidra/dt2-1.16-emac/disasm/4012deda_FUN_4012deda.s`, not the
decompile): before ever touching `EDMA_SERQ`/`ESDHC_XFERTYP`, it does
`tst.l D4` (the requested sector, signed) then
`cmp.l (DAT_44e3fea0).l,D4; bcc.b <error return, D2=-1>` -- an unsigned
bounds check against the card's own believed capacity in sectors, and bails
out (no hardware access at all, buffer untouched) if the sector is `>=`
that capacity.** `DAT_44e3fea0` is the exact same global this file's eMMC-
identity section already named: the CSD-1.0-fallback capacity `FUN_4012d4b2`
computes and this project deliberately set to `0x3B0000` sectors (not the
card's real, larger size) because encoding the correct, larger capacity
through EXT_CSD's own `SEC_COUNT` field "made a real cold boot hang
outright" in that earlier session. **`0x5D8000` (the +Drive superblock
sector) is `0x1FD000` sectors past `0x3B0000`** -- i.e. every sector
`tools/plusdrive.py`'s real filesystem uses (superblock at `0x5D8000`,
records from `0x5D8180`, content from `0x5EE180`) sits entirely outside the
capacity this build's identity fix tells the firmware the card has, so
`FUN_4012deda` rejects every read of it, unconditionally, regardless of
whether the image bytes at that file offset are correct (they are --
verified directly against `out/plusdrive/dt2.img`: the exact documented
superblock bytes are there) or whether the superblock/MaGj-record work is
otherwise right. **This, not a display-start gap or a format-writer bug, is
why no card-image boot has ever mounted +Drive's real filesystem in this
project.** **[O]**, one static pass, not yet independently re-checked: the
fix is very likely to re-test whether the "`SEC_COUNT=0x760000` hangs boot"
result from the eMMC-identity session still reproduces now that the
dma_sem wiring fix (a later fix in the *same* investigation chain, for a
hang inside a CMD25 write) is in place -- that earlier hang was bisected
before the dma_sem fix existed and may well have been the very same bug,
in which case reporting the card's real, larger capacity through
`SEC_COUNT` (or any row/encoding that both passes the whitelist and covers
at least `0x5EE180`+ sectors) would remove this bound entirely, without
needing to touch `tools/plusdrive.py` or the mount code at all.

**[C]: fixed, in a later session -- confirmed by execution, not just
re-reading the decompile.** The re-test above was right that the dma_sem fix
mattered, but the earlier "`SEC_COUNT=0x760000` hangs boot" hang was never
actually the same bug: `SEC_COUNT`'s own second consumer this project had
flagged as "not yet located" is `FUN_4012dca0` (found via `xrefs.sqlite`),
which busy-waits on `PRSSTAT` bit `0x18` after issuing raw CMD6/SWITCH
commands whenever EXT_CSD's `SLC_OK` byte isn't `1` -- a real hang risk this
model doesn't implement, but only reachable, on the evidence gathered
(`tools/refscan.py` over the whole image; see
`docs/findings/14-plus-drive-format.md`'s new section), from the debug
console, not the normal boot path. Separately, the CSD-1.0 fallback formula
(the mechanism this project used for the *smaller* constant) turns out to
have a genuine firmware arithmetic-shift-overflow bug for any capacity at
or above 2 GiB, so it can never reach the larger constant either way --
`SEC_COUNT` was always the only correct path there. `emu/esdhc.py` now sets
`SEC_COUNT=0x00760000` and `SLC_OK=0` together
(`emu.esdhc._CAPACITY_PARAMS`); a live cold boot confirms `DAT_44e3fea0`
reads back `0x00760000` correctly, `FUN_4012dbe0` accepts it (`D0=0`), the
superblock CMD18 at `0x5D8000` is issued and accepted, and **the mount flag
`_DAT_44f2bd68` reads `1`** -- the first successful `FUN_4015a450` mount
this project has observed. See `docs/findings/14-plus-drive-format.md`'s
"The capacity bound fixed" section for the full trace. This does **not**
reach "running" on its own -- see the branch-(C) section immediately below,
which remains open and is a separate gap.

### Follow-up: branch (C) never hands vector 208 to the real display ISR; the six unconditional calls and `FUN_40032eaa` are not it **[V][O]**

> **Corrected 2026-09-28 [C][V].** `FUN_400d18ae` is the intro task, not a
> display task; `0x40133518` is the progress screen's ISR. Branch (C) needs
> neither after the intro. The "`FUN_400337ba` is not the blocked task"
> result below holds for the control-image ladder; on the +Drive snapshots
> Main OS is parked on the intro's semaphore `0x43149550`. See "Branch (C)
> draws through its own flush" at the end of this file.

Read all six unconditional calls named above (`FUN_400c14dc`, `FUN_400f03e8`,
`FUN_4002dcb2`, `FUN_401339aa`, `FUN_40133e22`, `FUN_40133ac8`) and
`FUN_40032eaa`: none of them is display/PIT3-related. `FUN_40032eaa` is a
generic type-erasure "manager" function (get-default/copy/allocate/free by a
mode argument) shared by *every* `BgWorker` parameter block in this
function, branches A/B/C alike -- not specific to "Update MMC Caches". The
six calls touch internal-flash calibration, a couple of RAM mode flags, and
LED/knob-grid clearing. **None of them writes vector 208, `PIT3_PMR` or
`PIT3_BASE`.**

The "Update MMC Caches" job body itself (`LAB_400333e6`, a small trampoline
at `0x400333e6`-`0x400333fa` between two registered functions -- Ghidra
does not give it its own symbol; read directly from
`sections/section_3_MAIN_OS.bin` via `dt2.coldfire.disasm`, per CLAUDE.md's
warning that Ghidra misses small trampolines) is just:

```
400333e6  jsr FUN_4019d8fe.l   ; MmcFs singleton accessor
400333ec  move.l d0,-(sp)
400333ee  jsr FUN_4012faea.l   ; FUN_4012f184 + FUN_4012fa8c (pool-bitmap
                                ; rescan) + FUN_401328d2
400333f4  addq.l #4,sp
400333f6  clr.l d0
400333f8  rts
```

Pure region-1 (`MmcFs` pool) housekeeping, no display/PIT3 code anywhere in
it either.

**A second PIT3-arming function exists, and it is the real red herring.**
`data_refs` for vector 208's slot (`0x40000340`) has exactly two writers:
`FUN_40133586` (branches A/B, writes the real display ISR `0x40133518`) and
`FUN_400d12e4` (called unconditionally, early, from `FUN_400cc864` at
`0x400cc82e`/`0x400d133a` -- present on *every* boot, confirmed by
task-create logs firing at instruction ~47M even on the control-image
ladder). Reading `FUN_400d12e4`'s raw disassembly (not the decompiled C,
whose `vector_208_handler` symbol name is reused by Ghidra for both this
function and `FUN_40133586` even though they load *different* literals):

```
400d1352  move.l #0x400d0668,d1      ; = profile.intro_pit3_isr, NOT the
400d1358  move.l d1,(0x40000340).l   ;   real display ISR (0x40133518)
...
400d137c  move.w #0x2191,d0w         ; a DIFFERENT PMR than FUN_40133586's 0x4323
400d1380  move.w d1w,(PIT3_BASE).l   ; EN|PIE set, same as FUN_40133586
```

So on **every** boot, `FUN_400d12e4` re-arms PIT3 (with the intro's own
rate, `0x2191`) but re-points vector 208 right back at the intro's own ISR
(`0x400d0668`) and spawns the dedicated display-refresh task (entry
`FUN_400d18ae`). Verified live: at `snapshots/dt2-1.16-control/boot400M.snap`
(400M instructions into the control-image ladder), `PIT3_BASE`
PCSR=`0x093f` (EN|PIE both set) and `PIT3_PMR`=`0x2191` -- already armed --
while vector 208's slot is still exactly `0x400d0668`. `emu.pit.intro_running()`
therefore (correctly, given the real firmware state) returns `True` even at
400M+ instructions, since its check (`vector 208 == intro's ISR AND PIT3
enabled`) is genuinely satisfied by this branch-independent early call, not
by the intro actually still running.

**Net effect on branch (C): PIT3 keeps ticking, into the intro's own
(harmless, ack-only) handler, forever. The dedicated display task
(`FUN_400d18ae`) is created but never gets its frame semaphore posted,
because only the real ISR (`0x40133518`, written solely by `FUN_40133586`,
branches A/B only) posts it. Two independent exhaustive searches (the
`calls`+`data_refs` xrefs tables, and a raw 4-byte-literal scan of the whole
`section_3_MAIN_OS.bin` for `0x40133586`) found no third caller and no
third reference to either address anywhere in the image.**

**What this session ruled out, with live evidence, as the actual blocker:**

- Live task-profiling (`tools/guirun.py --trace-tasks` on the control-image
  boot) shows `FUN_400cc864`'s own task (`tcb=0x42944aac`) consuming
  **70-75% of every instruction budget** sitting in its own designed
  terminal `bra.b $-2` self-loop at `0x400cccd8` (confirmed against the raw
  disassembly of `FUN_400cc864`'s tail) -- a real, if independent,
  inefficiency worth fixing for anyone trying to reach `running` on a
  larger instruction budget, but not itself the display blocker.
- `--trace-tasks`'s stack-scan diagnostic showed `FUN_400337ba` (the
  Main-OS task) apparently blocked via a chain running through the
  "Update MMC Caches" job body and an async-comm-interface registration
  callback -- **this turned out to be a false lead**: a follow-up probe
  hooking `profile.sem_pend`/`profile.pend_b` directly (ground truth, not a
  stack scan that can pick up stale/leftover stack bytes) and filtering to
  `FUN_400337ba`'s and `FUN_400cc864`'s own TCBs recorded **zero** pend
  calls from either task across a full 280M-680M-instruction window on the
  control-image ladder. Whatever `--trace-tasks` was reporting was not a
  live call chain. `MainScreenView`'s constructor (`0x4019ab40`), separately
  hooked live, fires continuously (376 hits in the first 100M instructions
  after resuming from 400M, still climbing at 800M) -- so the Main-OS task
  is not stuck at all; it reaches and repeatedly touches the main screen
  view normally on branch (C). **Corrects this file's own earlier framing above: `FUN_400337ba` is not the blocked task; only the dedicated display task is.**
- Injecting panel input (`tools/guirun.py --input WHEN:press:2` for SRC,
  then `:17` for FUNC, at instruction counts well after intro handover and
  task setup) did not trigger `FUN_40133586` or change vector 208 either --
  `display_start_real hits=0` and `pit3=0` throughout a 300M-instruction
  window that included both presses. This doesn't rule out some other
  input/menu sequence, but a plain key press alone does not wake the real
  display path.

**Net: this is very likely a genuine property of DT2 1.16's boot code on
the "existing/already-formatted card" path, not (or not only) an emulator
peripheral-model gap** -- `unblock` and the eSDHC/PIT/DTIM models are not
implicated by any of the evidence gathered this session. **[O]**, not
resolved: either (a) there is a real display-start call for branch (C)
this session's static search still missed (the exhaustive checks were for
literal references to `FUN_40133586`'s address and the `0x40133518`
literal specifically -- a computed/indirect reference, e.g. through a
vtable slot, would not show up in either), or (b) real DT2 1.16 hardware
genuinely needs some other trigger (a specific menu navigation, not a bare
key press) to wake the display on this path, which would need reproducing
against a real device to confirm. No emulator fix was applied this
session; applying one without a verified root cause would not be
trustworthy per this repo's own verification rule.

**Re-tested, in a later session, now that +Drive's own mount succeeds
(above): the gap is unchanged and clearly not caused by the mount stall.**
A true cold boot with `--card-image out/plusdrive/dt2.img` (the same run
that confirmed the mount fix) continued to 780M total instructions: zero
new eSDHC traffic and zero new tasks created past `n≈260M`, `vec208` still
`0x400d0668` (the intro's own ISR), and the boot task still parked at
`0x400cccd8`. Since this run's `FUN_4015a450` genuinely mounts (mount flag
`1`) where every earlier run in this section's history never did, this
rules out "waiting on the +Drive mount" as a contributing explanation for
branch (C)'s missing display-start -- the two are independent gaps.

> **Corrected 2026-09-28 [C][V].** That run never left the intro. In
> `snapshots/dt2-1.16-drive2/boot400M.snap` and
> `snapshots/dt2-1.16-drive/running.snap` the intro is on frame 1
> (`0x43153a04 = 1`), the intro task is parked on `0x43149548`, and Main OS
> (TCB `0x40966ee8`) is parked in `sem_pend(0x43149550)` returning to
> `0x400337e2`. It only measured the intro gate.

## Opening the sample-pool list or the +Drive browser panics the UI task: a null `std::string` construction inside `SampleManager::vfunc_40`, not a resource cache **[V][C][O]**

**Corrects this file's own earlier framing below (originally recorded in
this session before the mechanism was fully identified): `DAT_44f37030`/
`DAT_44f37034` are not an application-level "resource/glyph decode cache".
They are libgcc's DWARF2 unwinder object-registration lists** (the
`seen_objects` splay tree and `unseen_objects` queue from libgcc's
`unwind-dw2-fde.c`), and the crash is an uncaught C++ exception, not a
missing resource archive. Reproduced headless with `tools/guirun.py` from
`snapshots/dt2-1.16/running.snap` (also from
`snapshots/dt2-1.16-card/boot400M.snap` with
`--card-image out/plusdrive/dt2.img`, same result), replaying the panel
feed for SRC, an encoder press to open the sample-pool list, FUNC, YES:

```
DT2_SYX=Digitakt_II_OS1.16.syx uv run python tools/guirun.py \
  snapshots/dt2-1.16/running.snap \
  --feed 54674406:2508 --feed 63719310:2500 --feed 72770933:2104 \
  --feed 81815375:2100 --feed 91072796:2104 --feed 100058398:2100 \
  --feed 109271476:2104 --feed 118324259:2100 --feed 127367540:2104 \
  --feed 136420786:2100 --feed 145463608:2104 --feed 154720798:2100 \
  --feed 163706398:2104 --feed 172919707:2100 --feed 181974109:2104 \
  --feed 191015771:2100 --feed 347640343:2120 --feed 356896798:2100 \
  --feed 365882397:2102 --feed 375096444:2100 --feed 384155990:2102 \
  --feed 393192508:2100 --limit 450000000
```

### Identification: this is libgcc's unwinder, not a resource cache **[V]**

Re-reading `out/ghidra/dt2-1.16-emac/decomp/40184e44_FUN_40184e44.c` with
this hypothesis in mind, the "compressed resource decode" is unmistakably
DWARF CFI parsing: it tests the CIE augmentation string byte-for-byte
against `'z'`(0x7a)/`'L'`(0x4c)/`'P'`(0x50)/`'R'`(0x52)/`'S'`(0x53), and the
legacy `"eh"` augmentation (`pcVar22[9]==0x65 && pcVar22[10]==0x68`), and
decodes fields with the standard ULEB128 loop (`uVar13 = (byte & 0x7f) <<
shift | uVar13; while (byte < 0)`) -- exactly `extract_cie_info()` in
libgcc's `unwind-dw2.c`. `FUN_40187c5c` walks a sorted tree
(`_DAT_44f37030`) then pops from a linked queue (`_DAT_44f37034`),
classifying each popped entry and inserting it into the tree -- exactly
`_Unwind_Find_FDE`'s `seen_objects`/`unseen_objects` two-list scheme.
`FUN_401879dc`/`FUN_40187a20`/`FUN_40187b08` (previously read as "register
a resource group") are the `__register_frame_info`/`__register_frame`/
`__register_frame_info_table` family: they allocate or take a caller-owned
24-byte `struct object` and push it onto `_DAT_44f37034`, guarding on `*fde
!= 0` exactly as libgcc's real implementations do. `FUN_40184e44`'s
return value `5` is `_URC_END_OF_STACK` (the standard `_Unwind_Reason_Code`
enum), returned when the search runs out of registered objects with no
CIE/FDE covering the target PC.

**Confirmed live**, by resuming a checkpoint saved just before the crash
(`tools/guirun.py --save-at 440000000:PATH`, then a small script using
`emu.longrun.build`/`spin` directly with a code hook at `0x40185f9c`) and
reading guest memory at the hook: the object passed as the exception
argument begins with the bytes `47 4e 55 43 43 2b 2b 00` = **`"GNUCC++\0"`**,
the literal, well-known GNU C++ `_Unwind_Exception.exception_class` magic
value. This is conclusive: `FUN_40185f9c` is (a thin wrapper immediately
around) `_Unwind_RaiseException`, called from `FUN_401866c2` (== `__cxa_throw`,
confirmed by its caller `FUN_401e8d20` filling in exactly the
`__cxa_exception`/`_Unwind_Exception` header fields -- the `"GNUCC++\0"`
class, a `handlerCount`-style refcount, and an `exceptionDestructor`
function pointer -- before calling it), and `FUN_40184e44` is
`_Unwind_RaiseException`'s per-frame search step. `__register_frame_info`
and friends being unreferenced anywhere in the compiled 1.16 `MAIN_OS`
image (the three-method dead-code proof kept below, now correctly
understood) means **this firmware's C++ runtime never registers any unwind
frame information, so a real DWARF unwind can never find a handler and any
C++ exception thrown anywhere in this firmware is unconditionally fatal**
-- consistent with `FUN_4013a8e6` (a bare `bra.b self` reached with no
`rte`, i.e. `abort()`) having 37 unrelated static callers across the image.
This is very likely a deliberate embedded-firmware choice (many C++
embedded builds ship without functional stack unwinding and treat any
`throw` as a bug that should hard-fault) rather than a boot-model gap, and
explains why finding "who registers the unwind tables" was a dead end: on
this firmware, on real hardware too, nobody does.

### What actually throws: `std::string(nullptr)` inside `SampleManager::vfunc_40` **[V]**

With the unwinder correctly identified, the real question is what raises
the exception. Reading backward from the `_Unwind_Exception` header
(`unwind_hdr`) at the live crash: the bytes at `unwind_hdr-48..-4`
(the `__cxa_exception` header fields preceding it) contain `0x402260dc` at
the `exceptionType` slot, which resolves directly in
`out/symbols/dt2-1.16-rtti.json`'s `typeinfos` table to **`std::logic_error`**.
The thrown object's vtable is `0x40225b78`, and its `what()` string (the
COW `std::string` member right after the vtable pointer, with a
length/capacity/refcount header at `-12`: `length=41 cap=41 refs=0`) reads:

```
basic_string::_S_construct null not valid
```

This is libstdc++'s own diagnostic, verbatim, from `basic_string.tcc`'s
`_S_construct`, thrown when a `const char*` range constructor is given a
null `begin` with a non-null `end`. Confirmed in the disassembly:
`FUN_401e75b6` *is* `_S_construct` (returns the shared empty-string rep at
`0x44f37244` when `begin==end`; calls
`FUN_401e44c4(s_basic_string___S_construct_null_n_4025de2f)` -- i.e.
`std::__throw_logic_error("basic_string::_S_construct null not valid")`,
the exact same string, at `0x4025de2f` -- when `begin==0 && end!=0`).
`FUN_401e7a64` *is* the `std::string(const char*)` constructor: if its
`const char*` argument is null, it deliberately calls
`FUN_401e75b6(0, 0xffffffff, ...)` (a null begin with a nonzero sentinel
end, not a real length) purely to trigger this throw -- matching
libstdc++'s actual `basic_string(const char*)` implementation, which is
well known to reject a null argument exactly this way.

The caller supplying that null pointer is `SampleManager::vfunc_40`
(`out/ghidra/dt2-1.16-emac/decomp/400266bc_SampleManager__vfunc_40.c`):

```c
void SampleManager::vfunc_40(int *param_1)
{
  if (param_1[0x7f] != 0) {
    ...
    if (param_1[0x81] == param_1[0x7f]) {
      FUN_4009020e(param_1,1);
      uVar1 = FUN_4015996e(param_1[0x81]);   // "get display name", may be NULL
      FUN_401e7a64(auStack_8,uVar1,&uStack_9);  // std::string(uVar1) -- no null check
      ...
```

`FUN_4015996e` (`out/ghidra/dt2-1.16-emac/decomp/4015996e_FUN_4015996e.c`)
is a "get display name" accessor with **two** failure modes but only
**one** safe fallback:

```c
undefined *FUN_4015996e(int *param_1)
{
  cVar2 = (**(code **)(*param_1 + 0x30))(param_1);   // vtable+0x30: "is valid"
  if (cVar2 == '\0') {
    puVar1 = (undefined *)0x0;              // <-- no fallback: raw NULL
  } else {
    puVar1 = FUN_4015778c(param_1[0x44], param_1 + 1);
    if (puVar1 == (undefined *)0x0) {
      puVar1 = &DAT_4023f364;               // safe fallback: "" (a bare NUL byte)
    }
  }
  return puVar1;
}
```

`&DAT_4023f364` is a plain `'\0'` byte sitting just before the
`"SEND SYSEX\0"` string literal in `.rodata` -- an intentional empty-string
default the author clearly meant to use whenever a name can't be produced.
The *first* branch (the object reports itself invalid at all) has no such
guard and returns a bare `NULL`, and `SampleManager::vfunc_40` passes that
straight into `std::string`'s constructor with no null check -- the actual
firmware defect.

### Why the object reports itself invalid: an unmounted `FileSystemDirectory` **[V]**

Hooking `SampleManager::vfunc_40`'s entry live (same checkpoint) and
reading `param_1[0x7f]`/`param_1[0x81]` (both equal, `0x450ef250`) shows the
"currently selected" object's vtable is `0x40225854`, which is
`out/symbols/dt2-1.16-rtti.json`'s `FileSystemDirectory` vtable
(`0x4022584c`) plus 8 -- the standard Itanium offset from a vtable's start
to the `vptr` value objects actually store. **The item SampleManager has
selected when this screen opens is a `FileSystemDirectory`** -- the real
on-card filesystem directory object this project already partially reverse
engineered in `docs/findings/14-plus-drive-format.md`. Its vtable+0x30
method (`FileSystemDirectory::vfunc_12`,
`out/ghidra/dt2-1.16-emac/decomp/401592ac_FileSystemDirectory__vfunc_12.c`)
is a one-line flag getter: `return *(byte *)(this + 0x120);` -- and that
flag is false on this object in both reproductions tried
(`snapshots/dt2-1.16/running.snap`, no card at all, and
`snapshots/dt2-1.16-card/boot400M.snap` with
`--card-image out/plusdrive/dt2.img`). No writer of
`FileSystemDirectory+0x120` was found among `FileSystemDirectory`'s own
named methods in `out/ghidra/dt2-1.16-emac/decomp/` (only the getter
references it); it is very likely set once a real mount actually succeeds
and reads at least one directory entry.

**This ties directly to this file's own still-open finding two sections
below** ("Booting with an already-formatted card stalls before `running`")
and to `docs/findings/14-plus-drive-format.md`'s undocumented real-FS
superblock at sector `0x5D8000` required by the mount routine
`FUN_4015a450`: this project has never gotten a card image through a real,
successful mount in this emulator, with or without `--card-image`. A
`FileSystemDirectory` that never became valid is exactly what an
unsuccessful (or never-attempted) mount would leave behind, and it being
the object SampleManager selects even with **no card connected at all**
(`running.snap`) suggests real firmware may gate ever showing/selecting a
`FileSystemDirectory` on card presence in a way this emulator's eSDHC/card
model does not yet reproduce (open below).

### What this rules out, and what remains open **[O]**

No code change was made to the emulator or a snapshot this session. Two
real candidates remain, not yet distinguished:

- **(a) Emulator/setup gap, most likely candidate.** The real filesystem
  mount never succeeds in this emulator (the pre-existing, still-open
  `FUN_4015a450`/superblock gap), so `FileSystemDirectory+0x120` never
  becomes true before `SampleManager::vfunc_40` runs. If real hardware
  either (i) always has a genuinely mounted, valid `FileSystemDirectory`
  by the time a user can reach this screen, or (ii) gates ever
  selecting/showing one on a successful card-detect+mount that the
  emulator's `emu/esdhc.py` model does not perform, then fixing the
  existing mount gap (or, more narrowly, making card-absence correctly
  avoid ever selecting an invalid `FileSystemDirectory`) would fix this
  crash too. Neither was attempted this session: the mount-format side is
  a separate, larger, already-partially-investigated task
  (`docs/findings/14-plus-drive-format.md`), and confirming the
  card-absence-gating hypothesis needs tracing what constructs/selects
  `param_1[0x7f]`/`param_1[0x81]` in `SampleManager`'s own setup path,
  which this session did not chase further.
- **(b) Latent firmware defect.** `SampleManager::vfunc_40`'s missing null
  guard is a real bug regardless of cause -- `FUN_4015996e` already has a
  safe empty-string fallback for the *other* null case three lines away,
  and simply doesn't use it here. If real hardware can ever reach this
  exact "selected item reports itself invalid" state (e.g. a card that
  fails to mount, or is removed mid-browse), it would hit the same
  abort there too. Confirming this needs a real device test (browse
  SampleManager/+Drive with no card, or a card that fails to mount) that
  this project cannot run.

Given the repo's own verification rule, no fix was applied without
distinguishing these. The immediately actionable next step is static: read
whatever constructs `SampleManager`'s `param_1[0x7f]`/`[0x81]` fields (its
constructor or the view-open path leading to this screen) to see whether it
is unconditional (selects a `FileSystemDirectory` regardless of card
presence -- pointing at (a)) or itself guarded on a card-detect/mount check
that the emulator's eSDHC model fakes or skips.

### The libgcc-unwinder dead-code proof (unaffected by the correction above) **[V]**

The three-method proof that `FUN_401879dc`/`FUN_40187a20`/`FUN_40187b08`
(the `__register_frame_info` family) and the two globals
`_DAT_44f37030`/`_DAT_44f37034` have no writer or caller anywhere in
`section_3_MAIN_OS.bin` stands unchanged under the corrected identification
-- it is *why* no unwind ever finds a handler, not evidence of a resource
cache:

1. `xrefs.sqlite`'s `calls` table: zero rows targeting any of the three.
2. `tools/refscan.py` (96.78% byte coverage) over the whole image: 0 hits
   on any of the three function addresses, or on `0x44f37030`/`0x44f37034`,
   outside the six functions implementing the unwinder itself
   (`0x401879b4`-`0x40187d58`).
3. A raw byte-for-byte 4-byte-literal scan of the entire 3.1 MB image
   (covering data/vtables/jump tables too, not just instructions): 0 hits
   in the same sense; the identical method correctly finds
   `_Unwind_Find_FDE`'s (`FUN_40187c5c`'s) two real call sites, confirming
   the method works.

Also reconfirmed **not an eSDHC/+Drive command bug** in the narrow sense:
re-running the identical feed sequence with `--card-image` omitted
reproduces byte-for-byte the same crash at the same instruction count, and
`emu/esdhc.py`'s opt-in command log (`--esdhc-log`) records zero commands
issued either way -- the SD driver's `XFERTYP` write is never reached
before the throw. That is now explained: the `FileSystemDirectory`
object's invalidity is a state left over from an *earlier*, already
completed (and already failing) mount attempt or its total absence, not
something this exact screen's own code tries to read from the card live.

## Branch (C) draws through its own flush; the +Drive boots were stuck behind the intro **[V][D][C]**

2026-09-28. Corrects "Booting with an already-formatted card stalls" and
its follow-ups above. Marks: **[V]** re-read against the image bytes and
snapshot memory by a second agent (verification lane, 2026-09-28);
**[D]** read or run once. Static sources: `out/ghidra/dt2-1.16-emac`
(`tools/cf.py`), raw `section_3_MAIN_OS.bin` (loaded at `0x40000400`),
snapshot memory through `tools/snapread.py`.

**Vector 208 [V].** VBR = `0x40000000` (`0x400019b0`/`0x400019b6`);
`FUN_40001992` fills the 256 slots with `0x40001252`. The literal
`0x40000340` (vector 208's slot) occurs only at `0x400d135a` (intro,
`FUN_400d12e4`) and `0x401335dc` (`FUN_40133586`). `0x40133586` is
referenced only by the two `jsr` in branches A and B (`0x4003382e`,
`0x400338a0`).

**`FUN_40133586` is the progress screen [V].** It creates task
`FUN_40133646` (prio 6), points vector 208 at `0x40133518` (acks PIT3,
gives `0x44e460d8`) and arms PIT3 with PMR `0x4323`. The task draws a
spinner and a bar from the bitmaps at `0x402b4b80`/`0x402b4cf8` and flushes
with `FUN_4013390e`. `FUN_40133626` (PIT3 off, `INTC2_SIMR = 0x10`, post
`0x44e460e0`) ends it, called at `0x400335b6`, `0x40033618` (factory reset
job) and `0x400334a0` (migrate job). `emu/symbols.py` already names it the
progress screen.

**The intro is the same pattern [V].** `FUN_400d12e4` (called at
`0x400ccb8e` on every boot) creates the intro task `FUN_400d18ae` (prio 7),
points vector 208 at `0x400d0668` and arms PIT3 with PMR `0x2191`. The task
plays one frame per PIT3 tick; at the end (`0x400d1934`) it turns PIT3 off
and posts `0x43149550` (`0x400d1950`). **Main OS waits for that post
first**: `0x400337dc` calls `FUN_400d139e` = `sem_pend(0x43149550)`, before
any branch.

**Branch (C) [V].** After the intro, `FUN_400337ba` queues "Update MMC
Caches" (`0x4003390c` invoker `0x400333e6`, `0x40033912` manager
`0x40032eaa`, `0x40033934` `jsr FUN_400f0ab8`, non-blocking), clears the
framebuffers (`0x40033962` `FUN_401339aa`), does the first UI render
(`0x4003398a` `jsr 0x40032992`, which flushes through `FUN_4013390e`) and
enters the event loop (`0x40033d06`, queue `0x40966f3c`). The main UI never
uses PIT3. The job body at `0x400333e6` is `jsr 0x4019d8fe` (MmcFs), `jsr
FUN_4012faea` (three scans under the MmcFs lock), `clr.l d0`, `rts`.

- **[C]** `FUN_40032eaa` is the job functor's std::function manager, not a
  completion routine; "Update MMC Caches" has no completion callback.
- **[C]** The GUI's `jobs` figure counts entries to the worker loop
  `FUN_400f0958` (one per worker thread; it pends on worker+0x14 at
  `0x400f0986`), not queued jobs.
- The prio-3 SampleLoaderBgWorker (ctor `0x400f0d30`) runs the same loop
  **[D]**.

**Snapshot evidence [V].** `snapshots/dt2-1.16-control/running.snap`
(branch C) draws the full main page with the progress task never created
(TCB `0x44e46564` saved sp = 0 **[V] (P0 2026-09-29: the TCB's first three
words are 0; in `snapshots/dt2-1.16/running.snap`, a formatting boot that
did run the progress screen, they are set and vector 208 is `0x40133518`)**),
and its vector-208 slot is still
`0x400d0668`. `snapshots/dt2-1.16-drive2/boot400M.snap` has vector 208 =
`0x400d0668` and the intro on frame 1 (`0x43153a04 = 1`), with Main OS
parked on `0x43149550` **[V] (P0: in drive2/boot400M.snap and
drive/running.snap the Main OS stack holds the argument `0x43149550` at
`0x4098f6a4` and the return address `0x400337e2` at `0x4098f6a8`; the intro
task's stack holds `0x43149548` at `0x4314d548`)**. The same intro-at-frame-1
state is in every `emu.checkpoint make` rung read, so the ladder builder
(`emu/dspboot.py`) does not deliver PIT3 **[D]** (P0: also true of
`drive3/boot400M.snap`).

**Confirmed by a run [D].** `tools/guirun.py` from
`snapshots/dt2-1.16-drive2/boot400M.snap` with `--card-image
out/plusdrive/dt2.img --intro-timers pit3` and markers, 300M instructions
(68 s): intro done at about 51M (`0x400d1934` 1 hit), Main OS past the intro
(`0x400337e2` 1), no branch A or B (`0x4003382c`/`0x4003389e` 0), queue
"Update MMC Caches" (`0x40033934` 1), job start and end (`0x400333e6`,
`0x400333f6` 1 each), first UI render (`0x4003398a` 1), event loop
(`0x40033d06` 383), panel flush (`0x4013390e` 303). No fault. This is the
first branch-(C) boot seen on a +Drive image, and it draws. The black
screen in earlier GUI runs was the intro gate, not missing firmware work.

## A +Drive cold boot loads a sample end to end **[D]**

2026-09-28. With the native-format image (finding 14) the firmware loads the
boot project's samples on a cold boot, with no UI.

    uv run python tools/plusdrive.py build samples -o out/plusdrive/native/dt2.img
    DT2_SYX=Digitakt_II_OS1.16.syx uv run python tools/plusdrive_check.py out/plusdrive/native/dt2.img
    DT2_SYX=Digitakt_II_OS1.16.syx uv run python -m emu.checkpoint make \
      280000000,400000000 snapshots/dt2-1.16-drive3/boot \
      Digitakt_II_OS1.16.syx --card-image out/plusdrive/native/dt2.img
    DT2_SYX=Digitakt_II_OS1.16.syx uv run python tools/guirun.py \
      snapshots/dt2-1.16-drive3/boot400M.snap \
      --card-image out/plusdrive/native/dt2.img --intro-timers pit3 \
      --trace-tasks --esdhc-log --limit 400000000 \
      --at 0x40154540=load_sample --at 0x400cd638=page_send \
      --flexbus-log FLEXBUS.raw \
      --save-at 390M:snapshots/dt2-1.16-drive3/loaded.snap

- The ladder took 5:55 wall; the guirun run 88.5 s (4.52M instr/s), no
  HALT, FAULT or STALL.
- One hit each of "Load all samples" queue (`0x400311d0`), its invoker
  (`0x40030ce8`) and `reloadAllSamples` (`0x4004e8be`); 297
  `FUN_40154540` calls (one real load, 296 aliases); 1940 `FUN_400cd638`
  calls; 1321 `FUN_40153b90` (mono and reset headers) and 297 `FUN_40153b28`
  (stereo) calls. The wire content is in finding 04 ("Sample data crosses
  FlexBus to link port 0 on 1.16"); the PCM matches the file.
- eSDHC: 3698 commands (CMD18 3250, CMD23 224, CMD25 224).
- `pends force-satisfied by unblock: 13`, none at the 100 us sleep, so the
  per-page DTIM1 sleep needs no model under guirun's default `unblock`.
- `loaded.snap` (99 MB, holds the sample RAM) is the source of the first
  listen ([15](15-sharc-sample-path.md)). Its UI has two modal windows open
  (finding 03, "Modal windows take every key").
- `--intro-timers pit3` has no effect when resuming past the intro;
  guirun's "intro handover at 52M" line then reuses a cold-boot heuristic
  against the resumed run's relative count.

## Fractional EMAC was wrong in patched Unicorn **[V][D][C][O]**

2026-09-28. Everything here was executed or read once; the fix is built and
tested in worktree `.claude/worktrees/agent-a35ef1ffe626acbe5` but **not
installed in the shared venv** and not merged.

> **[C] 2026-09-29 (P0).** The fix is merged (commit `95d92de`:
> `patches/unicorn-2.1.4-m68k-emac-fractional.patch`, sha256 `8f497c93...`,
> pinned third in `tools/install-patched-unicorn.sh`) and installed in the
> shared venv: `uv run python -m emu.unicorn_compat` reports
> `emac_fractional` pass (0.5 x 0.5 = `0x20000000`, -0.5 x 0.5 =
> `0xE0000000`, the mode switch keeps `0x12345678`). The install steps at
> the end of this section are done. A capture made with it exists:
> `out/captures/drive3/dt2-1.16-drive3-trig1-emac.dt2cap`.
>
> P0 checked rows 1, 2 and 5 twice: against MCF54418RM p.5-17 (PDF p.159:
> `product[63:0] = (operandY * operandX) << 1`, and the zero-fill for
> `0x8000_0000 * 0x8000_0000`, row 4's manual column), and by running
> `emu/unicorn_compat.py`'s own case on the stock `unicorn==2.1.4` wheel in
> an isolated environment: 0.5 x 0.5 = `0x10000000`, -0.5 x 0.5 =
> `0x30000000`, and the mode switch reads `0x00123456`. Rows 1, 2 and 5 are
> **[V]**; the Unicorn column of rows 3, 4 and 6-8 stays **[D]** (only the
> patched build's tests exercise them). The seven MACSR immediates were
> found again by a raw scan for `a93c` (seven code hits, three more in data
> past `0x40220000`) and in the Ghidra listing, which also shows two `move.l
> A0,MACSR` restores (`0x4002f398`, `0x400cee02`).

The ColdFire uses fractional EMAC (`MACSR = 0x20`, F/I = 1, truncating, no
OMC) in `vector_191_handler` (the parameter smoother `FUN_400d92a2` and the
`mac.l` that writes `0x800047fc` at `0x4002e9f6`), the second vector-191
routine `0x400cec70`, and `FUN_40138ff8`. The image's only `move.l
#imm,MACSR` immediates are 0x00 and 0x20 (`0x4002dd38`, `0x4002dd52`,
`0x4002f384`, `0x400cec94`, `0x400cecb0`, `0x400cedee`, `0x4013900c`)
**[V]**.

Unicorn 2.1.4's `qemu/target/m68k/helper.c` (the same in QEMU master)
against the manuals (MCF54418RM section 5, PDF p.145-159; CFPRM chapter 6):

| # | behaviour | Unicorn | manual | fixed |
|---|---|---|---|---|
| 1 | product shift | none: 0.5*0.5 -> `0x10000000` | `(Y * X) << 1` (RM p.5-17): `0x20000000` | yes |
| 2 | operand sign | unsigned multiply: -0.5*0.5 -> `0x30000000` | signed: `0xE0000000` | yes |
| 3 | R/T rounding | one bit off | rounds `product << 1` at [23:0]/[24] | yes |
| 4 | -1 * -1 | 0.5 | +1.0 | yes |
| 5 | MACSR mode change (`set_macsr`) | re-encodes with the old MACSR (no-op); signed/unsigned swapped | ACCn/ACCext bits kept | yes |
| 6 | accumulation saturation (OMC) | 48-bit limit with the opposite sign | `0x007f_ffff_ff00` / `0xff80_0000_0000` | yes |
| 7 | fractional EV | tests ACC[47:40] | ACC[47:39] | yes |
| 8 | store with OMC (`get_macf`) | returns 0 for negatives; 16-bit never saturates | saturate on Temp[47:39] | yes |
| 9 | store without OMC, MOVE to ACC, operand extraction, MOVCLR, ACCext layout | right | -- | unchanged |
| 10 | OMC with PAVn already set | still accumulates | ACC unchanged | no (firmware never sets OMC) |
| 11 | integer-mode EV and saturation sign | looks wrong | RM p.5-15/16 | no, out of scope |

Effect on the firmware **[V]** (P0: the numbers below follow from row 1
alone; see finding 04, "The smoother"): the smoother is a unity-gain one-pole
(finding 04, "1.16 frame fields"). With the old library it settles at
0.029 x: running `FUN_400d92a2` directly on `loaded.snap`'s SRAM, track 1's
raw `[15614, 768, 7, 30720, 28458]` (TUNE, PLAY, SAMP, LEN, LEV) settles at
`[455, 22, 0, 895, 829]`; with the fixed library it holds the raw values
(slot state exactly 7.0). The old numbers match every existing capture.

The fix (in the worktree): `patches/unicorn-2.1.4-m68k-emac-fractional.patch`
(sha256 `8f497c93...`), a third patch pinned in
`tools/install-patched-unicorn.sh`; `emu/unicorn_compat.py` case
`emac_fractional` (the emulator refuses to start on an old library once this
is merged); `tests/test_unicorn_emac.py` 11 `FRACTIONAL_CASES`. In a scratch
venv the compat check fails on the old build and passes on the new one, and
104 tests pass.

What it invalidates:

- Every `out/captures/*.dt2cap` made before the fix (all except
  `dt2-1.16-drive3-trig1-emac.dt2cap`): the smoothed bands
  (`0x02`, `0x74`, `0xda`-`0x139` per track) are wrong; `0x94` is right.
  Tests that read captures (`test_sharc_replay.py`, `test_sharc_inputs.py`,
  `test_sharc_armpath.py`, `test_sharc_calltrace.py`, `test_sharc_memdiff.py`,
  the capture part of `test_sharc_framemap.py`) keep passing until the
  captures are remade.
- `snapshots/dt2-1.16/running-audio.snap` holds smoother state at the old
  steady value. Other DT2 snapshots have smoother output 0 (not run yet),
  including `drive3/loaded.snap`. Whether `FUN_40138ff8` or `0x400cec70`
  left state in boot snapshots is **[O]**.
- SHARC golden hashes do not depend on it (no capture input).
- **[O]** what `0x400cec70` and `FUN_40138ff8` compute, before and after.

Install (Em, between runs; running processes keep the old library; done
by 2026-09-29, and the installer now runs from any tree at `95d92de` or
later):

    cd /Users/em/src/digi/digitakt2/.claude/worktrees/agent-a35ef1ffe626acbe5 && tools/install-patched-unicorn.sh

Then `uv run python -m emu.unicorn_compat` and `uv run python -m pytest
tests/test_unicorn_emac.py tests/test_unicorn_compat.py -q`.

## The count-stop BTST flag bug and the flush-flags patch **[V]**

2026-09-29 (P0). Checks the root cause in `patches/README.md` ("What the
flush-flags patch fixes") and `emu/unicorn_compat.py`'s `btst_flush_z`.

- **The site [V].** `0x400cd2f4` is `btst.l #28,d0` and `0x400cd2f8` is
  `beq.w 0x400cd47c`, in `FUN_400cd2bc`, after `move.l (0xec03802c).l,d0`
  (Ghidra listing and capstone agree).
- **The mechanism [V], from Unicorn 2.1.4's `qemu/target/m68k/translate.c`
  (tag commit `8028ec43`).** `update_cc_op()` stores `s->cc_op` to the CPU
  state only when `cc_op_synced` is 0. `gen_flush_flags()` handles
  `CC_OP_ADD*`, `SUB*`, `CMP*` and `LOGIC` in line and then sets
  `s->cc_op = CC_OP_FLAGS` without clearing `cc_op_synced` (only the helper
  cases, which also write `env->cc_op`, set it to 1). `bitop_im` (BTST)
  calls `gen_flush_flags()`. Stock Unicorn stores CC_OP only at block end,
  where the flag is still 0 from the producer's `set_cc_op`, so it stores
  `CC_OP_FLAGS`. The hook CCR patch adds an `update_cc_op()` before each
  code hook, which stores the producer's op (`CC_OP_LOGIC` after
  `move.l`) and sets the flag to 1; after BTST the state still says LOGIC,
  and a later reader recomputes Z from N. The flush-flags patch clears the
  flag in the four in-line cases.
- **By execution [V].** On the stock `unicorn==2.1.4` wheel (isolated
  environment) `btst_flush_z` passes (both shapes, D1 = 2) and
  `count_boundary_cmp_z` fails (SR = 1, not 4): stock has the lazy-CCR
  count-stop bug but not the BTST one, so the BTST bug comes from the hook
  CCR patch, as the README says. The installed library passes all six
  compat cases. A build with the hook patch but without the flush-flags
  patch was not run.
- **[D]** that the 1.16 run hit it after 69.87M instructions, 49
  instructions after an `rte` (a run observation).

## The 1.16 serial console **[V][D][C]**

2026-09-28. Corrects the console addresses above, which are 1.15C.

- **[V]** `FUN_400cc63a` creates the console task, entry `0x400cae8c`
  (`pea` at `0x400cc660`), priority 2. `FUN_400cc864` calls it only when
  bit 5 of `0x4029e9b0` is set (1.15C: `0x40288190`). Task TCB `0x4039be58`,
  line queue `0x403a0eac` **[V] (P0: `FUN_400cc63a` creates the queue with
  `FUN_40001834(0x403a0eac, ...)` and the task with `FUN_400012c8(0x4039be58,
  0x400cae8c, 2, 0x4039beac, 0x4000)`; the gate is `moveq #$20,d0; and.l
  0x4029e9b0,d0; beq` at `0x400ccc7c`, and the only call is `0x400ccc98`)**.
- `emu/serial.py` (`CONSOLE_QUEUE 0x40388EAC` etc.) still has the 1.15C
  addresses and needs porting **[V]** (P0: `emu/serial.py:50`,
  `CONSOLE_QUEUE = 0x40388EAC`).
- Commands **[D]**: `#RECEIVE_AUDIO`, `#PLAY_START`/`#PLAY_STEREO`/
  `#PLAY_STOP`/`#RECORD_*`/`#DUMP_AUDIO` (the vector-191 debug player,
  finding 04; bypasses the SHARC); `#SAMPLE_UPLOAD n` (writes eMMC sector
  `0x458000 + n*0x800`, the factory-content region); `#VERIFY_SAMPLES`;
  `#READ_SAMPLE_STATUS`; `#FORMAT_FS`; `#PLAY_PATTERN` (depacks the built-in
  project `0x4027989c`, `loadProjectFromMemory`, queues "ReloadAllSamples"
  and starts the sequencer; factory sample ids only); `#STOP_PATTERN`. None
  loads a +Drive file into sample memory.

## Timers at the device rate: prescaler, held ticks, idle skip **[V][C]**

2026-09-29. Measured with `tools/cfrealtime.py` from
`snapshots/dt2-1.16-drive3/loaded.snap` (SSI0 model at 96 kHz,
`RxHandoverPeer`, +Drive image), 0.3 s of device time after a 0.1 s
warm-up, three alternated repeats, medians. Every repeat of a
configuration ended on the same instruction count, PC and TX CRC.

- **[V][C] The PIT prescaler divides by 2^PRE, not 2^(PRE+1).** MCF5441XRM
  Table 38-3 (p.1156) and Eqn. 38-1 (p.1159): timeout = 2^PRE x (PM+1) /
  (fsys/2). The firmware agrees: `0x40136310` writes PCSR1 = `0x0033`
  (PRE 0) and PMR1 = `0x83`, then counts PIF (bit 2) events, so one event
  is 132 bus cycles = 1 us only under 2^PRE. The rates the firmware
  programs are PIT0 100 Hz (PCSR `0x053f`, PMR 41249, `0x40001290`), PIT2
  120 Hz (`0x0636`/17187, `0x40002c5e`), PIT3 15 Hz for the display
  (`0x0936`/17187, `0x401335f8`) and 30 fps for the intro
  (`0x0936`/8593, `0x400d137c`). This corrects the 15 fps, 50 Hz and
  60 Hz above. Checked against the bytes by the time-base lane and again
  here. `emu/pit.py` now uses 2^PRE.
  **P0 2026-09-29 [V][C].** Re-read in both passes (Ghidra listing and
  capstone on the raw image) with the manual text (p.1156 PRE table 2^0 ..
  2^15, p.1159 Eqn. 38-1). At 132 MHz: PIT0 2^5 x 41250 = 10.000 ms
  (PMR at `0x40001290`, PCSR at `0x400012bc`); PIT2 2^6 x 17188 = 8.333
  ms; PIT3 2^9 x 17188 = 66.67 ms; intro 2^9 x 8594 = 33.33 ms; PIT1 2^0
  x 132 = 1 us (`0x40136310`, outside any Ghidra function, then a PIF
  count loop with write-1-clear). **[C]** The 15 Hz PIT3 rate at
  `0x401335f8` is `FUN_40133586`'s, the +Drive progress screen (see
  "Branch (C) draws through its own flush"), not the display's; the main
  UI does not use PIT3.
- **[V] A refused tick is held.** PIF (PIT) and REF (DTIM) stay set until
  the handler write-1-clears them, so `Pits`/`Dtims` keep a refused tick
  pending and offer it at every later `emu_start` boundary. A tick due
  while one is pending is lost (`missed`); a guest write of 1 to PIF or REF
  clears a pending one (`cleared`: the context switcher's `0x053f` write to
  PIT0's PCSR does this). Before, PIT0 and PIT2 lost almost every tick at
  every rate: their periods are whole numbers of audio blocks, so their
  deadlines kept falling inside the audio interrupt. `(3, 2, 0)` is now
  just the INTC's order within a level (higher source first, MCF5441XRM
  Table 17-19), not a starvation trade-off.
- **[V] The idle loop is skipped exactly.** At a boundary with PC on a
  `bra.b *` site, `spin` credits the passes to the clock without running
  them, up to the next boundary or one pass short of the next idle
  reschedule (`emu.longrun.IdleSpin`). Exact runs with and without it, to
  the same device time (1.1 s, 145M instructions): `tools/snapeq.py`
  identical, and the 7,241 host-raised vectors (3,987 idle reschedules,
  1,500 each of 170 and 191, 100 PIT0, 120 PIT2, 30 DTIM3) identical in
  order, PC and SP. A skip that also skips the reschedule pass differs
  (negative control). Exact runs skip only steps that start in the loop:
  a stopped `emu_start` cannot say how many instructions it ran. With the
  SSI0 model there is a boundary every ~1,375 instructions, and 95% of
  idle passes are skipped. `spin(fast=True)` counts blocks anyway, so there
  the first pass stops the step and the rest is skipped.
- **[V] The "1,370 vector-191 a second" cap was the hand-over window.**
  After a resume, vector 170 runs the generic handler `0x400d2f98` until it
  has seen the RX marker in 64 major loops (finding 04, Lane J2); that
  handler does not force vector 191. It takes 65 blocks (43 ms), which is
  130 per second over a 0.5 s window. After it, vector 191 runs 1,500 times
  a guest second with a gap of exactly one block.

| from loaded.snap, 132M instr/s | wall s | real-time factor | executed instr/s | v191 per wall s | IPL >= 5 | PIT0 / PIT2 / DTIM3 taken |
|---|---:|---:|---:|---:|---:|---|
| before, exact | 16.02 | 0.019 | 2.47M | 28 | 38% | 0/15, 0/18, 9/9 |
| + 2^PRE prescaler | 15.94 | 0.019 | 2.48M | 28 | 38% | 0/30, 18/36, 9/9 |
| + held ticks | 16.28 | 0.018 | 2.43M | 28 | 38% | 30/30, 36/36, 9/9 |
| + idle skip, exact | 1.38 | **0.217** | 12.8M | 326 | 38% | 30/30, 36/36, 9/9 |
| + idle skip, SSI0 coalescing | 16.05 | 0.019 | 2.47M | 28 | 5% | 12/30 (18 cleared), 36/36, 9/9 |
| before, fast | 8.57 | 0.035 | 4.62M | 53 | 19% | 0/15, 0/18, 9/9 |
| + all, fast | 1.24 | **0.241** | 7.3M | 362 | 36% | 30/30, 36/36, 9/9 |
| + all, fast, coalescing | 1.06 | 0.282 | 8.5M | 424 | 2% | 30/30, 36/36, 9/9 |
| GUI config (fast, no SSI0), before, 18.72M | 1.09 | 0.274 | 5.1M | - | 0% | 15/15, 18/18, 9/9 |
| GUI config, after, 132M | 0.17 | **1.81** | 8.9M | - | 0% | 30/30, 36/36, 9/9 |

"Executed" excludes skipped idle passes; the guest clock counts them. In
fast mode it is `_FastStepper`'s estimate. IPL >= 5 is sampled at
boundaries, so coalescing (few boundaries) under-reads it.

- **Time base.** At 4.68M and 18.72M the audio state starves: IPL >= 5 at
  every boundary, no PIT or DTIM tick taken in a guest second, the main
  loop never runs, and vector 191 runs 123 and 551 times a guest second.
  At 132M everything is taken and the main loop runs 30 times a second.
  `emu/gui.py` and `tools/guirun.py` now switch to
  `DEVICE_INSTR_PER_SEC` when the intro hands over (was 4 x 4.68M), and a
  checkpoint saved at another rate is rescaled (`Timers.rescale`,
  `Ssi0Dma.rescale`). `INSTR_PER_SEC` stays 4.68M for the
  instruction-budgeted tools, whose budgets were calibrated on it. The
  `dt2gui` sample load from `boot400M.snap` at the new default: the same
  15,908,000 FlexBus bytes and 3,698 eSDHC commands, idle by 200M
  instructions, 44.9 s wall against 84.6 s.
- **Coalescing stays opt-in.** In exact mode a coalesced span almost never
  starts in the idle loop, so it defeats the idle skip, and held ticks
  wait for a span end, where the RTOS has often cleared them.
- **[O] Real time in the audio state.** The best is 0.28x (fast,
  coalesced); exact is 0.22x. The device runs ~55M non-idle instructions a
  guest second here, and Unicorn with our hooks runs 7-13M a second, so
  this is the ceiling of this core, not of the timer model. The rules here
  (2^PRE, held ticks, skipping the idle loop to the next deadline) carry
  over unchanged to a native or WASM core, which can also stop at idle
  entry because it knows its own instruction count. SSI0 coalescing only
  works around Python boundary costs and would not.

## Native ColdFire real-mix gate (2026-09-29)

- **[D]** The pure Rust interpreter (`native/coldfire`) now uses 256 tagged,
  direct-mapped decode pages instead of a per-hit `HashMap` lookup. With the
  same `boot280M.cfdump` (100M instructions, no idle passes), uninstrumented
  `cfrealmix` rose from 47.0M to 80.8–82.2M useful instructions/s; an
  independent final run measured 81.9M/s. All runs ended with state hash
  `0x0bb544e65d49266b`. This passes the plan's 62M/s **interpreter floor**;
  it does not yet establish 1.0x for a wired machine, or browser performance.
  DT2 boot280M lockstep compared 30,000 instructions and both-image seed-42
  fuzz agreed on all supported cases (DT2 1,927/2,000; DN2 1,937/2,000).
  CPU-originated writes invalidate matching cached code; writes from external
  DMA still need explicit invalidation when the machine bus is integrated.

- **[V]** `snapshots/boot280M.snap` has word `0x7381` at `0x401768a6`
  (independently read with `tools/snapread.py`; the drive3 boot280M snapshot
  instead has `0x4e75` there). `0x7381` decodes as `mvz.b d1,d1`;
  CFPRM p.125 specifies that MVZ **always clears N**. **[D]** At this
  instruction patched Unicorn returned SR `0x2008`, while the Rust core
  returned `0x2000` and both returned D1 `0x00000085` from `0xffffff85`.
  `tools/cf_lockstep.py` now corrects only this opcode's isolated N-bit
  oracle discrepancy before continuing. This is a flag defect, not the
  MVZ/MVS *decode* bug ruled out earlier in this finding. The DT2 boot280M
  window then compared 30,000 instructions; the DN2 window stopped after
  1,477 at the harness's existing unmapped/exception boundary, so it does
  not establish longer DN2 lockstep parity.

## Native GPIO SD gate, bounded oracle replay (2026-09-29)

- **[D]** The existing Python `emu/gpio.py` and `emu/esdhc.py` remain the
  oracles. `native/periph/src/gpio.rs` implements only the board continuity
  loopback: write D4 high through `PPDSDR_D` (`0xec09401b`), clear it through
  `PCLRR_D` (`0xec094027`), and sense C3 through `PPDSDR_C` (`0xec09401a`).
  The rest of GPIO remains a register file. The late boot/ready MMIO traces
  miss this gate, so new *ignored*, firmware-derived traces were recorded
  from 24M-instruction checkpoints: DT2 for 30M more instructions and DN2
  for 80M more. On **each** trace the focused `mmio-replay TRACE
  --gpio-gate-only 20` checks 20 guest gate writes, 20 read-hook memory
  writes, and 20 guest sensed reads, with zero GPIO mismatches and a clean
  recorder END. Recorded and unrecorded runs of the same window have
  identical guest state under `tools/snapeq.py` (DT2: 134 mapped pages;
  DN2: 110). Rust peripheral tests pass; the core builds for
  `wasm32-unknown-unknown` with default features disabled.
- **[O]** Whole-trace replay still returns nonzero: DT2 has two, DN2 three
  *unexpected vector-207 IRQ predictions* in these earlier windows. Focused
  GPIO mode explicitly reports but does not gate on unrelated IRQ mismatch
  totals. This is **not** full peripheral parity. eSDHC/card, other GPIO,
  UART, panel, and display remain native-model work; an isolated full-card
  trace/replay gate has not been established.

## Native eSDHC and bounded eMMC card slice (2026-09-29)

- **[D]** The pure `native/periph/src/esdhc.rs` controller models early
  command/register effects with a card-port interface. The explicit Oracle
  register policy matches `emu/esdhc.py`'s plain-RAM guest writes to IRQSTAT;
  Device policy implements the RM's write-one-to-clear behavior. On each
  24M-checkpoint early DT2/DN2 trace, `mmio-replay TRACE
  --esdhc-early-only 74` checks 74 guest register accesses (26 returned
  reads), 111 predicted host register writes, zero read mismatches and zero
  unmodeled eSDHC operations, with a clean recorder END. This focused mode
  reports but excludes the existing 2/3 vector-207 replay mismatches.
- **[D]** `native/card/` carries the card's synthetic command/identity state,
  sector-addressed reads and sparse write overlay. Its random-access backing
  is supplied by the caller, not opened or copied by the core; transfers
  require an explicit size of at most 1 MiB or fail before allocation. The
  early controller-to-card `CardPort` is connected for command responses and
  the bus-test word; tests cover that connection, synthetic EXT_CSD bytes and
  overlay precedence. Both crates' tests pass and their pure library builds
  check for `wasm32-unknown-unknown`.
- **[O]** The controller does not yet feed CMD18/CMD25 payloads through eDMA
  channel 59, post card/dma/data semaphores or deliver interrupts. The
  transfer contract will be fallible and chunked when machine wiring adds it;
  early register replay alone is not a cold-boot or real-time audio gate.

## Live SR trace and bounded channel-59 transfer effects (2026-09-29)

- **[C]** The early trace's 2/3 unexpected vector-207 predictions did not
  establish a PIT defect. The v1 trace records an RTE returning with SR
  `0x2700` to `0x40001416`, whose decoded instruction is `move D1w,SR`;
  the next interrupt frame reports SR `0x2004`. Replay could not observe
  that direct guest SR write. The opt-in v2 recorder samples **live** SR
  immediately before async and timer service; replay applies it before
  predicting delivery. This is not a vector-specific exemption or a value
  inferred from the IRQ under test. Existing v1 traces still use their
  explicitly reported legacy scheduler-trap exemption, or retain their
  original mismatches where that exemption does not apply.
- **[D]** Fresh, ignored v2 traces recorded for 30M more DT2 and 80M more
  DN2 instructions from the 24M checkpoints have respectively 52 and 159
  service-boundary SR samples. Recorded/control snapshots compare identical
  under `tools/snapeq.py`. Both **whole early traces** now pass `mmio-replay`
  with zero mismatches and no SR exemption; on each trace the focused GPIO
  gate (20) and eSDHC gate (74) also pass. Original v1 ready-state traces
  continue to pass with one explicit legacy SR exemption each. This only
  establishes parity for the captured early windows, not late +Drive boot.
- **[D]** `native/periph` now supplies `transfer_dma59`, a pure CMD8/CMD18
  card-to-guest and CMD25 guest-to-card helper. It uses caller-provided
  windows bounded to 1 MiB, validates addresses before mutating payloads,
  returns a detached TCD effect, and restricts TCD completion writeback to
  the three oracle-written registers. Tests exercise synthetic 512-byte and
  32-KiB transfers and a real `Card` media/overlay round-trip. The first
  512-byte `_dma_out` at clock 70562053 in the DT2 late trace is **CMD18**
  (XFERTYP `0x123a0036`), not evidence of CMD8 merely because of its size.
- **[D]** The new `native/card/src/dma.rs` adapter takes a caller-owned guest
  RAM window, reusable bounded staging buffer and live eDMA register bank.
  It selects real card CMD8/18/25 data, transfers via the pure helper, and
  applies the TCD writeback only after a successful operation. A synthetic
  backed read, overlay write/readback, failure atomicity and zero-CITER
  tests pass on the real `Card`; its library also checks for WASM.
- **[O]** A native machine still needs to arm channel 59 on SERQ, select the
  already-armed adapter on the firmware's command, map its guest RAM window,
  and deliver completion. There is no independent full late boot DMA payload
  replay or native ISR parity gate yet. Old v1 late traces have separate
  vector-208 timing residues; do not call that timer parity.

### Late v2 DMA: native board and bounded CPU stepping

- **[C]** Correct the preceding **[O]** only at Board/synthetic CPU scope:
  `native/machine` Board now arms channel59 on SERQ, dispatches eSDHC XFERTYP
  through live `Esdhc<Card>`, sparse guest RAM and TCD. Oracle policy posts
  configured semaphores directly; Device only queues events. Bounded
  `Machine::step` executes actual ColdFire synthetic guest MMIO instructions,
  invalidates decoded code after successful external DMA, and accepts an
  explicitly HOST-CONFIGURED synthetic Device vector/level on the next
  instruction boundary; it does not know firmware INTC vector or timers. Do
  not claim a live firmware ISR or full boot.
- **[D]** Local ignored late v2 peripheral replay reached DT2 220,166,405 and
  DN2 100,614,927 instructions with zero peripheral mismatches and zero
  live-SR exemptions. Both recorded vs no-record control final snapshots
  compare identical (DT2 134 mapped pages, DN2 110, plus
  registers/MMIO/control/counters). Legacy v1 late vector208 timing residue
  is not evidence of v2 timer parity.
- **[D]** Explicit ignored native board-gate invocation `cd native/machine &&
  cargo test --release --features trace --test late_dma -- --ignored` with
  the operator's own ignored DT2/DN2 traces and DT2 card image. Bounded
  windows compare guest RAM byte-for-byte against all captured CMD18 output
  payloads, DT2 2,224/DN2 2,050 COMMAND/TRANSFER COUNTS; stage recorded CMD25
  HRD guest input and verify card overlay and TCD source/CITER/CSR writeback
  for DT2 222/DN2 2. This is replay against recorded peripheral writes, not
  independent CPU boot; no derived artifact is committed. The correlated
  trace gate is now integrated and reviewed; this remains
  **[D]**, not **[V]**, and does not establish an independent cold boot.
- **[D]** `Card` overlay now stores written-byte masks in sparse 512-byte
  sectors rather than a map node per byte; backing remains visible where not
  written and explicit zero writes remain visible. Bounded 1MiB sector test
  and release/WASM build gates pass. Synthetic Machine Device IRQ test uses
  an arbitrary host-supplied vector and RTE, not firmware ISR path.
- **[O]** P5 live firmware CPU loop with timers/INTC, eDMA request
  enable/disable gating, cross-product ISR/semantic parity, real-time audio,
  and browser runtime still need independent gates; do not claim full native
  machine parity.

### Native portable state and bounded checkpoint smoke (2026-09-30)

- **[C]** The preceding open item now has a **narrow** real-firmware CPU
  execution gate, not a full native boot. `native/machine` imports portable
  MSTATE v1 through a bounded parser and restores CPU registers, selector-keyed
  control state, mapped-zero and nonzero RAM pages, and Oracle forced-read
  words. Python alone converts trusted local `.snap` pickles; Rust does not
  parse pickle. The clock defaults to checkpoint-relative zero. A timer facade
  can route PIT/INTC MMIO and Oracle PIT delivery, but the early checkpoint
  smoke run does **not** attach it; nonempty host components are rejected.
- **[D]** `uv run python tools/checkpointprep.py gate --dt2-syx
  Digitakt_II_OS1.16.syx --dn2-syx Digitone_II_OS1.11.syx` checks each local
  source SHA-256 against its pinned product value and extracted sections'
  `.source-sha256`, hashes the MAIN OS image, and compares **all** its loaded
  bytes against the corresponding trusted 24M snapshot before converting it
  under ignored `out/native/checkpoint-gate/`. This full-image check matters
  because those early snapshots have no source manifest. The wrapper then
  reported `source-verified products=dt2,dn2`; each native child completed
  exactly **1,000 instructions** and stopped at its budget. The ignored Rust
  test alone is explicitly `smoke-unverified`, not a provenance check. It
  fails before unsupported MOVEC forms and on any unexpected completion event.
  This is execution progress only: no per-instruction, MMIO, timer, snapshot,
  ISR, full-boot or audio parity was compared.
- **[D]** The integrated release gates passed 33 machine tests (two ignored
  checkpoint tests), 76 peripheral tests, 11 focused Python tests, four
  ignored late Oracle DMA trace tests, and the no-default-features WASM build
  check. Device eDMA request controls and D_REQ persistence have synthetic
  tests; Oracle SERQ/CERQ behavior stays separate. The late trace gate still
  proves captured transfer effects, not independent late firmware execution.
- **[O]** Late checkpoints carry timer, eSDHC, eDMA and UART host components;
  their restoration is deliberately rejected. Native MOVEC register reads and
  unmodelled selector writes still lack parity semantics; MSTATE native export,
  reverse `.snap` conversion, equal-clock `snapeq` comparison, live Device
  timer/INTC delivery, full ISR parity, real-time audio and browser execution
  remain unproven. Do not extrapolate the 1k checkpoint smoke gate to them.

- **[D]** Follow-up CPU-state differential: `uv run python
  tools/checkpointprep.py diff --dt2-syx Digitakt_II_OS1.16.syx --dn2-syx
  Digitone_II_OS1.11.syx --limit 1000` repeats both source/image checks,
  restores each trusted 24M checkpoint in the Python Unicorn oracle, and
  takes exactly one bounded instruction per trace boundary. An ignored native
  Machine test checks **initial state and every subsequent boundary** through
  1,000 instructions for D0–D7, A0–A7, PC, SR and checkpoint-relative clock,
  stopping on the first differing field, unsupported MOVEC or completion
  event. DT2 and DN2 both passed; a seven-step run and a deliberate corrupted
  register detector also passed. The trace JSON is firmware-derived and stays
  ignored under `out/native/checkpoint-gate/`; direct Rust test invocation is
  unverified, whereas the Python wrapper verifies provenance before generating
  fresh traces. This establishes only these **reached CPU-state boundaries**:
  it does not compare memory writes, peripheral registers, timer scheduling,
  interrupt behavior or later firmware execution.

### Bounded guest writes and timer-component handoff (2026-09-30)

- **[D]** The new, separate `tools/checkpointprep.py effects --dt2-syx
  Digitakt_II_OS1.16.syx --dn2-syx Digitone_II_OS1.11.syx --limit 1000`
  repeats the source, section and complete loaded-MAIN-OS checks above before
  generating fresh, ignored v2 effect traces and running native `Machine`.
  The original `diff` command retains its v1 CPU-state trace and was rerun
  through 1,000 instructions for both products after integration. In the
  effects run, **DT2 and DN2 each matched 1,000 ordered guest RAM writes**
  (address, width and value) at their reached instruction boundaries. Both
  windows contained **zero FC/EC MMIO reads and zero FC/EC writes**: MMIO-read
  parity is explicitly *unexercised*, not established by empty comparisons.
  Synthetic tests detect a corrupted write value and keep CPU instruction
  fetches out of guest-data-read records. Bus-access capture is opt-in and
  disabled for ordinary long-running stepping to avoid an unbounded log.
  These checks do not compare final
  whole-RAM contents, host/DMA writes, peripheral state or later execution.
- **[D]** `native/machine::timer_state::import_timers` exposes a separate,
  synthetic-tested **component-only** Python v1 `Timers` importer. It checks
  ordered PIT/DTIM source configurations, rebases finite deadlines against
  the checkpoint-relative clock, retains pending/held/counters and DTIM stale
  history, and rejects nonempty DTIM arm state because its scheduling effect
  is not implemented. The Python MSTATE converter narrowly normalizes timer
  tuple channels and integer counter keys; it does not relax other host
  component validation. Seven focused native tests, five Python converter
  tests and the machine WASM check passed. No real late timer checkpoint was
  imported through `Machine` or source-verified by these synthetic tests.
- **[O]** `Machine::apply_state` still rejects nonempty host components, and
  the attached `Time` facade currently routes PIT/INTC rather than a restored
  PIT/DTIM pair. Timer register pages, storage/card overlay, eDMA and UART
  component state, Device interrupt delivery and a provenance-verified late
  CPU/effects gate remain to be integrated and independently checked. Do not
  extrapolate this 1,000-step RAM-write result to those effects or full boot.

### Local checkpoint chain at the first MMIO window

- **[D]** `tools/checkpointchain.py` roots local captures in a source-hash and
  complete-loaded-image-checked 24M snapshot. Each ignored receipt records a
  private copied parent snapshot, source/section hashes, trace/snapshot hashes,
  and the *actual* completed instruction count; `--limit` is a floor, with a
  120-second subprocess cap. Changed code in MAIN OS is accepted only with
  `--control`: a second no-record replay from the same private input must
  match guest state, host components, manifest and completion count. The
  receipts provide **local integrity and reproducibility, not authentication**
  of arbitrary pickle snapshots or independent hardware verification.
- **[D]** Reproduce a window near the first observed MMIO activity by running
  `anchor dt2 --syx Digitakt_II_OS1.16.syx`, then `capture <returned-chain.json>
  --syx Digitakt_II_OS1.16.syx --limit 8000000 --control`; for DN2 use
  `anchor dn2 --syx Digitone_II_OS1.11.syx` and `capture <returned-chain.json>
  --syx Digitone_II_OS1.11.syx --limit 2000000 --control`. Prefix each command
  with `uv run python tools/checkpointchain.py`. Check `done` in each receipt:
  a timer interval can overshoot either requested floor. A one-million-step
  recorded *next* window from each local pre-event capture observed **91 guest
  MMIO reads, 238 guest MMIO writes and six IRQ records** on each product.
  These are oracle captures, not native parity. All snapshots, traces, receipts
  and derived portable states stay ignored under `out/native/checkpoint-chain/`.
- **[D]** `portable <returned-chain.json> --syx <same-source>` validates the
  chain and copies/re-hashes the local snapshot before conversion to MSTATE.
  The converter narrowly maps `manifest.unblock_except` address tuples to
  arrays and sparse eSDHC overlay integer offsets to decimal string keys;
  unrelated non-JSON values remain rejected. DT2 +8M and DN2 +2M local
  checkpoints converted, but **native `Machine::apply_state` still rejects
  their nonempty host components**. A native CPU/MMIO first-divergence result
  has not yet been produced.

### Native import of the local pre-MMIO host state

- **[C]** The earlier claim that `Machine::apply_state` rejects **all**
  nonempty components described the previous implementation. The native
  machine now accepts a sole validated Python-v1 `timers` component when a
  matching Oracle `Time` facade is attached. It also accepts the exact
  four-component longrun set (`timers`, `esdhc`, `edma_tx`, `uart_in`) when
  UART input and TX state are dormant and the eSDHC DMA byte count is zero.
  Active, unmodelled UART/TX state and unknown components still fail closed;
  this does **not** establish full late-state restoration.
- **[D]** The `Time` facade now routes PIT, DTIM and INTC register pages and
  offers due Oracle vectors to the CPU while retaining declined DTIM ticks.
  A DTIM REF host write bypasses guest W1C dispatch and updates the backing
  page. The card importer restores RCA, selection and sparse absolute-byte
  overlays, including written zeroes that mask backing media. Tests cover
  component mismatches, timer topology, refused-vector retry, card capacity,
  and malformed/active host fields. **Device timer delivery is still
  unsupported** and no Python schedule is treated as proof of hardware timing.
- **[D]** After re-running `checkpointchain.py portable` for both local,
  source-checked pre-event chains, an ignored native test parsed and imported
  the resulting DT2 +8M and DN2 +2M MSTATE files. Rust's direct test alone
  does not authenticate its caller-supplied paths; the local Python source
  checks preceded it. This proves only that these **particular dormant-host
  checkpoints load**, not by itself that subsequent CPU instructions, MMIO
  effects, interrupts or audio match the Oracle. The later bounded checks
  below cover only their named windows; restoration of active UART/TX/card
  host state remains **[O]**.
- **[D]** A bounded **Oracle first-MMIO gate** now starts at those imported
  checkpoints. Run `uv run python tools/checkpointchain.py first-mmio PARENT
  DERIVED --syx SOURCE --count 6` with a direct-child instruction-clock
  capture: the wrapper rechecks the source, loaded MAIN OS, parent and trace
  receipts, then privately converts the snapshot and extracts ordered events
  before invoking the ignored native test. Both local DT2 1.16 and DN2 1.11
  gates passed their **first six ordered guest accesses** (one read, five
  writes, including instruction offset, address, value, size and PC); their
  last compared offsets were 360079 and 242626 respectively. A direct Rust
  invocation has no provenance checks. These receipts demonstrate local
  integrity/reproducibility, **not authentication** of pickles or Device
  behavior. The gate compares neither all later accesses nor guest RAM.
- **[D]** The initial native pre-MMIO exception on DN2 was isolated with a
  locally verified, ignored Oracle CPU-sample probe: the first sampled
  discrepancy was at offset 54785, where the native core raised access error
  on a MOVEM store to an absent SDRAM page. Python's `Machine._fault` lazily
  zero-maps that page. The native Board now has an **opt-in, Oracle-only,
  SDRAM-range, 16-new-page-limited** first-touch mapping for this gate; it is
  not a claim about device memory. The focused sample at offsets 54000–55000
  then passed DN2 D/A/PC/SR comparisons, but that one-off sample is not bound
  to the source-verified gate and does not prove CPU-state parity over the
  whole window. The bounded late CPU/RAM comparison below does not cover
  later timer/IRQ execution; integrated DSP playback remains **[O]**.
- **[D]** An ignored **DSP-only headless audio gate** in
  `native/live/src/sharc_capture_tests.rs` takes explicit absolute
  `LIVE_SHARC_PACK` and `SHARC_NATIVE_LIB` paths under ignored `out/`. It
  checks native core source compatibility **only for v3 packs** (older local
  packs lack that field), renders 160 frames (5120 stereo samples) through
  `LivePlayer::offline` twice from fresh cores, requires clean and non-silent
  frames, and compares canonical interleaved signed-Q31
  PCM FNV-1a fingerprints. It prints each run's elapsed time and bounds the
  test's wait on a separate render worker to 30 seconds (the native core
  cannot be cancelled in-process); this is not a real-time throughput claim.
  The original local capture passed twice using a legacy v1 pack, without a
  core-source compatibility check. A fresh v3 pack was then built into ignored
  `out/native/live/v3/` from the same captured frames and LP0 log with
  `tools/sharc_transpile_run.py live-pack --limit 2000000`; that limit bounds
  initialization, while LP0 callbacks are separately bounded. The selected
  DT2 sections' source SHA-256 matched the local `.syx`, the pack's image
  bytes matched those sections, and the rebuilt pack body was byte-identical
  to the legacy pack body. Its v3 core-source hash matched both the current
  `tools/sharc_core` and the selected generated native library. Two fresh
  v3 debug renders again each produced 160 clean frames and 5120 stereo
  samples with FNV-1a64 `e89d808cd7de9585`. These checks establish local
  source compatibility and repeatability, **not authentication** of the
  firmware/capture, Device audio, ColdFire-generated frames, or integrated
  playback **[O]**.
- **[D]** `tools/checkpointchain.py first-cpu-ram PARENT DERIVED --syx
  SOURCE --count 6` now re-verifies the local source/receipt chain, privately
  copies and hashes the parent snapshot and firmware source, and rejects an
  event window that crosses a timer STEP or IRQ. Its separate bounded Unicorn
  probe samples D/A/PC/SR (including both interval ends) and records one
  interleaved stream of guest writes **below 0x80000000** and MMIO reads/writes
  in the recorder's ranges. It verifies MMIO markers against the checked trace
  before using its read values. The native gate compares this unified order
  and stops at the first observed difference. ColdFire's
  lazy NZV state is resolved before SR comparisons. The local DT2 gate passed
  353 sampled CPU boundaries, 359992 ordered guest writes and six MMIO
  accesses through instruction offset 360079; DN2 passed 238 boundaries,
  242540 guest writes and six accesses through offset 242626. These bounds
  stop **before** the first timer IRQ in each window. The unchanged ignored
  trace artifacts and local source receipts remain under `out/`; this is
  Oracle agreement only, not unobserved CPU boundaries, all address ranges,
  ISR/Device behavior, integrated playback, or authentication of arbitrary
  pickle files.

### Auto-ready host counter and portable overlay limit (2026-09-30)

- **[D]** The locally checked DT2 1.16 auto `ready.snap` has no pending
  `edma_tx` completion and an empty `uart_in` deque. Its historical counters
  are nonzero: TX `bytes=47114`, `transfers=14220`, eSDHC
  `dma_bytes=42312704`. Python increments these on transfers and restores
  them independently of queued work. Native host import now retains them,
  while still rejecting pending TX completions and queued UART input. The
  local section source marker, MAIN OS digest, and card sidecar matched the
  supplied files; these checks establish local integrity, not authenticity.
- **[D]** The source-checked `first-cpu-ram --count 8` DT2 and DN2 gates now
  cross one **synchronous TRAP #0** at relative instruction 360082 / 242629.
  The Python Oracle writes its exception frame from the host, outside the
  guest-write hook; native ColdFire emits two frame writes through the bus.
  The gate separately checks those writes against the source-checked vector
  and pre-exception PC/SR/A7, rather than omitting them silently or treating
  them as guest-instruction effects. DT2 matched 369 sampled CPU boundaries,
  360010 guest RAM writes, two exception-frame writes and eight MMIO accesses
  through step 360093. DN2 matched 254 sampled CPU boundaries, 242558 guest
  RAM writes, two exception-frame writes and eight MMIO accesses through
  step 242640. These are **post-TRAP**, not post-asynchronous-timer, Device
  timer, or integrated playback gates. The checked DT2 trace's first later
  asynchronous IRQ is an idle-spin Oracle injection of vector 32 at relative
  step 380150; a further timer boundary still needs a separate gate.
- **[D]** A narrower source-scheduled **Oracle idle-credit** gate now accepts
  exactly `emu.longrun.IdleSpin.on_spin` vector 32 with no level, from the
  checked trace, and rejects other asynchronous sources (including timer and
  eDMA IRQs). It checks both exception-frame writes separately and accounts
  for the code-hook's one guest-clock credit before the handler executes.
  DT2 `first-cpu-ram --count 12` matched 390 sampled CPU boundaries, 360053
  guest RAM writes, four exception-frame writes and 12 MMIO accesses through
  step 380162; DN2 matched 275 boundaries, 242601 guest writes, four frame
  writes and 12 MMIO accesses through step 262709. The first source-recorded
  idle injections were at DT2 step 380150 and DN2 step 262697. This is an
  Oracle trace-replay normalization, **not** an implemented Device timer/INTC
  scheduler, a native audio path, or proof about a later vector-155 eDMA IRQ.
- **[C] [D]** MSTATE v1 cannot import this auto-ready card overlay: its
  14,522,880 byte entries become ~218 MB of JSON, above the 1 MiB header cap.
  The opt-in MSTATE v2 sector-bitmap representation retains a mask for written
  zeroes without lifting that cap. The local `ready.snap` converted to a
  2,583,342-byte ignored MSTATE (SHA-256
  `cb0f69bf102f27df8bc65b9c2396ec67027e09ca20d0f62e014f3f4155eae547`):
  28,365 sectors, 14,522,880 written bytes. The ignored native import test
  passed with matching retained counters, overlay byte count, and masked
  values from the first/last sectors. This is local Python-to-native state
  import, **not** native ColdFire playback or Device parity. It does not
  authenticate the fixture or establish the LP0 input's origin.
- **[C] [D]** The imported auto-ready DT2 state produced its **first native
  ColdFire DSPI2 wire frame**: 2,748 bytes after **30,464 actual native
  instructions**. `tools/wiretrace.py --prefix 1` matched its wire-order bytes
  against frame 0 of the accepted 68-frame **Python-ColdFire** DTFR. A
  source-checked `tools/autoirq.py` reference also matched 33 CPU boundaries
  and 34 ordered guest RAM/MMIO effects over the first 32 instructions after
  the first GUI-scheduled vector-191 IRQ. The Python fast-stepper's first
  force was at relative clock 20,014; native reached the corresponding
  boundary at 20,015 (one idle-code-hook credit of skew). An earlier probe
  forced a frame at clock zero and observed one at 10,449 instructions; that
  was **not** the accepted GUI's scheduling policy. This is an **Oracle
  playback smoke test**, not autonomous Device timing or passing full-order
  native parity. Its
  ignored `native/machine/tests/auto_wire.rs` probe replays the accepted GUI
  policy: open the frame gate, clear its counter, offer vector 191 at its
  configured level after GUI-like idle-entry/timer boundaries when due, and
  yield vector 32
  every 20,000 passes over source-scanned `BRA.B -2` sites. The Python build
  re-applies the DSPI2 polled-idle bit (bit 28 at `0xec03802c`) **after**
  restoring the checkpoint; the native probe must do the same or the driver
  is reached but programs no eDMA TCD. Python's checkpoint omits ColdFire
  EMAC registers: its freshly constructed Unicorn MASK reads zero at the
  handler, whereas the native CPU starts at the documented reset value
  `0xffffffff`. The probe explicitly seeds zero for **Oracle** parity;
  this cannot recover the Device's EMAC state. The local firmware source
  marker, MAIN OS hash, card sidecar and MSTATE digest matched before the
  run; these checks do not authenticate the fixture. This first-frame gate
  alone says nothing about later IRQs, button-delivery clocks, sustained
  playback or SHARC rendering/stops.
- **[C] [D] [O]** Replaying both accepted panel-input **delivery** clocks
  (3,516,425 and 10,552,659) through Python's UART8 ring-write/eDMA-34
  pointer/vector-154 stimulus, the native run emitted **68 ordered frames**
  after **13,908,723 actual instructions**. All **first 51 frames** match
  the accepted Python-ColdFire DTFR, including the TRIG in frame 18. The
  full comparison **fails**: frames 51 and 53 differ at bytes `0x25` and
  `0x29` (Python has `0x01` at 51; native has `0x01` at 53); the other 66
  frames are byte-identical. The earlier 18-frame/19th-frame mismatch was
  caused by incorrectly forcing at exact multiples of 200,000 starting at
  zero; the GUI offers a due frame only at a fast idle-entry or timer
  boundary. Even with that correction the native and Python force clocks
  are not established equal. Native frame-forces 51 and 52 were at relative
  clocks 10,318,163 and 10,522,540, both **before** the accepted release
  delivery at 10,552,659; its release words appear in frame 53. Python's
  accepted frame 51 already contains those words, but its corresponding
  frame-force clocks were not recorded, so the specific scheduler skew is
  not yet established. The two displaced one-shot words remain an
  **open timing/guest-state parity gap**, not Device parity. No native
  SHARC render or zero-stopped-render gate has passed for these frames.
- **[D] [O]** Extending the first frame-IRQ reference to 15,000 guest
  instructions matched **15,001 CPU boundaries and 671 ordered guest
  effects**. A 60,000-step diagnostic first reported a D0 difference at
  step 44,204, just before `FF1 D0` at `0x40138c8e`: Python's scoped
  Unicorn code hook changes D0 and advances PC while sampling the preceding
  counted instruction, then reports the following PC twice. Native executes
  `FF1` as an instruction; its D0 catches up at the next boundary. Simply
  subtracting an instruction clock for each native FF1 was tried and
  **failed** at the following D1 boundary, so it was not adopted. This
  identifies a trace-observation normalization problem, not a demonstrated
  incorrect native FF1 result or an explanation for the two displaced
  release words. A longer CPU/RAM/MMIO gate needs an explicit hook-boundary
  normalization before calling it CPU parity or divergence.

### Host-event replay isolates the 68-frame release displacement (2026-09-30)

**[C] [D]** The preceding note's *unrecorded Python frame-force clocks*
are now available for a **new, source-checked diagnostic run**; the earlier
native **autonomously scheduled** 68-frame failure remains valid. With the
locally checked ready snapshot, card, OS image and compact state,
`tools/autoevents.py --stepping fast-observed --frames 68 --limit 20000000`
uses fixed guest-clock requests for `3516425:2301` and `10552659:2300` and
delivers them at existing outer GUI-style boundaries, without a polling
thread. Its 68-frame `fast-offers-sixtyeight.dtfr` has SHA-256
`e21cb015102bd6c1222b7c733238ddf7402a725ee7e63b0fb8313d00fd1f8e8b`
and **matches the previously accepted** Python-ColdFire DTFR byte for byte,
after **14,365,479 fast-mode *estimated/credited* instructions**. The input
delivery clocks equal both requested clocks. This integrity match is not
source authentication or a deterministic exact-counted clock gate: the GUI
fast stepper estimates counts and can stop at a block boundary. The separate
`counted` mode is an exact-counted **new** Oracle reference; a bounded
four-frame run took 2,897,012 counted/credited instructions and matched the
first four accepted bytes, but its frame-force clocks differ substantially.

The fast-observed Python **force** offers at ordinals 50–54 were
10,404,273; **10,617,827**; **10,826,104**; 11,035,489; and 11,243,883.
The earlier native self-scheduler forced ordinals 50–54 at 10,113,640;
**10,318,163**; **10,522,540**; 10,732,139; and 10,937,324. Ordinal
52's native offer is **before** release delivery at 10,552,659 while the
corresponding Python offer is **after** it: this accounts for the one-shot
words moving from Python frame 51 to native frame 53 without requiring a
release-word encoding defect. Native offer 52 led by 303,564 clock units;
Python and native delivery clocks were identical. `tools/wireevents.py`
reports the first differing wire byte at frame 51 offset `0x25`, with
the frame-51/53 release windows and local force-clock deltas. This is an
observed host scheduling separation, **not** an explanation for all of its
underlying clock drift or a Device timer model.

**[D]** In a second, distinct **partial host-event replay** mode,
`native/machine/tests/auto_replay.rs` injects exactly the recorded Python
frame-force and panel-delivery events (bounded by `DT2_NATIVE_LIMIT` and
`DT2_NATIVE_FRAMES`), retaining *native* timer and idle-yield scheduling.
From the same locally checked ready state, a release-built replay emitted
**all 68 wire-order frames byte-identically** to the accepted Python DTFR
after **14,167,528 actual native CPU steps**, with 68 explicit force
stimuli and 534 native idle yields. The counted-mode four-frame replay also
matched all four ordered frames, after **2,466,249 actual native steps**.
The full replay pass isolates this particular release displacement to the
force-offer schedule, but does **not** establish native autonomous scheduling,
per-instruction ColdFire parity, Device input/timer/INTC parity, or SHARC
rendering. The first replayed host force already has a different native
pre-IRQ PC (`0x40000458`) from Python (`0x400cccd8`); matched wire bytes do
not erase that difference. Python SR is intentionally *not* sampled solely
for this report because extra Unicorn SR reads can disturb condition codes;
native pre-SR is present in its JSON telemetry. All derived artifacts remain
under ignored `out/native/integrated-auto-smoke/`; local SHA-256s prove
consistency only. A passing native SHARC **zero-stopped-render** gate for
these frames is still open.

**[D]** A separate, bounded **native SHARC-only** diagnostic now passes a
zero-stopped-render gate for the same 68 native-produced wire frames, using
the locally checked state pack `out/native/live/state-82cf380735390258438540a4.pack`
and a **synthetic** queue schedule (one take per ordered frame and one
one-shot-cleared repeat after each third take). Its 90 exact post-queue,
post-halfword-swap DMA inputs are stored with take/repeat ordinals, raw-byte
SHA-256s, end state, stop PC and native instruction deltas at
`out/native/sharc-rendered-lane/native-replay68-plus-repeats.ndjson`
(SHA-256 `1c0b5413cc115fa2d68718f0bad22c168703bdd7441d1e50b64720540d752847`).
The release appears at rendered ordinal 68; all **90** rendered inputs ended
cleanly, with **0 stops, 0 DMA failures and 19,402,398 actual native SHARC
instructions**. This is a **synthetic** replay, not the integrated desktop
audio callback's actual repeat cadence or proof that the intermittent
`0x1c1cd7` stop is fixed. The prior intermittent stop remains **[O]**.

**[C] [D]** `tools/sharc_render_replay.py` then imported the same checked
state-pack SHRD bytes into the **independent Python SHARC interpreter**,
verified the packed image against the locally loaded SHARC section, and
replayed the 90 **exact recorded post-swap DMA byte sequences** without a
second swap. With a 21M-instruction cap it finished all 90 frames after
**19,402,398 actual Python SHARC instructions**; each frame's ordinal,
take/repeat source, raw-byte SHA-256, clean/returned terminal and **per-frame
instruction delta** matches the native SHARC log. The bounded agreement
report is `out/native/sharc-rendered-lane/python-ninety-checked.json`
(SHA-256 `a1adba75b01f4c24e7ee60b14d053478073067d6a65ee3017eb945a3520cf3f0`).
This establishes a 90-input Python/native **SHARC execution/stop** gate for
that particular synthetic sequence, **not** PCM/audio-sample parity,
desktop callback timing, or freedom from intermittent stops on other input
sequences. No failed-frame native read/write trace was captured.

**[D] [O]** A Python interpreter watch over the suspect architectural
`DM(0x254d94)` records a *real writer* at **SHARC PC `0x1c2c64`** on 89
of 90 rendered frames, before the subsequent reads at `0x1c1ca8`.
`0x1c2c64`'s explicit disassembly is `DM(0x254d90) = R12`; in SIMD mode,
`tools/sharc_core/forms_move.py` writes its PEy companion at `+4`, i.e.
`0x254d94`. This accounts for the previously empty **resolved static
store** list: the current SHARC DB indexes the explicit operand but not
this SIMD companion. The watch's canonical address is `0x28254d94`.
The writer's values vary and none in this 90-frame run equals the old
post-trap value `0x31049452`. This is **Python-path** dynamic provenance,
not proof that a stopping **native** path wrote the same value: a native
pre-trap read and actual writer event on a reproducible stopping input
sequence remain **[O]**. Post-trap `M1/I0` alone is not provenance.

**[D]** A bounded native self-scheduling diagnostic (ignored
`diagnose_first_autonomous_force_clock_drift` in
`native/machine/tests/auto_replay.rs`) reproduced six forced-vector offers
over **1,200,000 actual native ColdFire instructions**, using locally
source-checked DT2 1.16 inputs. Its first meaningful difference is offer 2:
native actual clock `224382` versus Python **fast-observed credited** clock
`228381` (difference −3999). Both offers have pre-PC `0x400cccd8`,
pre-SR `0x2000`, post-PC `0x4002dd0c`, post-SR `0x2500`. Offer 2 followed
an **idle entry** after 180,000 observed idle passes and nine Oracle vector-32
yields; its next native timer deadline was `925816`, not due at the offer.
Offer 1 differs by +1 from a separately stepped vector entry. The JSON
under `out/native/force-clock-lane/first-drift.json` records these numbers
and its source/profile/MSTATE main SHA-256 checks. This identifies the
earliest *observed scheduling divergence*, not its cause: fast-observed
credits are not actual native steps or Device time. No `Board`/`Time`
clock-credit policy changed. The lane's prose handoff gave incorrect hex
PCs; the checked JSON values above are the probe's actual integer PCs.

**[D]** An opt-in `live_open_frames_with_rendered_input_log` offline ABI
now captures actual `LivePlayer` queue-consumer inputs after take/repeat
and halfword swap, with a 1..256 record cap, ignored-path restriction,
per-frame flush and an unchanged ordinary playback ABI. A 68-take plus
22-*synthetic*-repeat ABI check reproduced the earlier 90-input diagnostic
byte for byte (SHA-256 `1c0b5413cc115fa2d68718f0bad22c168703bdd7441d1e50b64720540d752847`)
with **19,402,398 actual native SHARC instructions** and zero stops. This
remains synthetic, not integrated GUI cadence. The earlier Python
ColdFire fast-observed `14,365,479` is an **estimated/credited** count,
not an actual native ColdFire instruction count.

**[D] [O]** A new opt-in, bounded **actual Python-ColdFire GUI + native
SHARC** run (not native-ColdFire wire production) through
`tools/live_gui_check.py --rendered-inputs-out` reproduced the intermittent
native stop: first **rendered ordinal 198** is a real *repeat* and stops at
`0x1c1cd7` (`native-trap: unmodeled MMR`). Its exact post-queue/post-swap
bytes, SHA-256, source, and native per-frame instruction delta are in ignored
`out/native/sharc-integrated-lane/gui-trig.ndjson` (local SHA-256
`1c870636146452a949409ffaf99cb72785f414aab2e819d8a1cf79d92b82812c`).
The cap retained only ordinals 0..255: **256 / 1,453** rendered frames,
**one captured stop**, two stops in total, 14 taken and 1,439 repeated,
and 316,986,729 actual native SHARC instructions. The accepted TX wire
log is `gui-trig.dtfr` (14 frames, one trig at TX index 2); local source
`.syx` matches `sections/.source-sha256`. The GUI's `2,897,012`
ColdFire count is **Python fast-mode estimated/credited**, not actual
native ColdFire steps. The GUI zero-stop gate correctly **fails**; this is
not audio parity or a complete 1,453-frame capture. The run required an
explicit `--syx` matching its checkpoint flash digest; a first run without
it failed the checkpoint provenance gate, and was not accepted. Post-trap
`I0=M1=0x31093de7` is **not** writer evidence. Native *pre-trap* memory
read and writer provenance, and the cause of the second uncaptured stop,
remain **[O]**.
The first stop is reproducible without GUI timing: re-import the same
locally checked state pack (`state-82cf380735390258438540a4.pack`,
SHA-256 `cbb6de7e9e732edf085dc3ba47b7760fc2db52c7595827027266d71f8e6f9bfc`)
through offline `tools/live_audio.LiveAudio`, verify each recorded
`bytes_hex` hash, swap each adjacent byte pair *back* to queue wire order,
then `push_frame`/`render(1)` once for each of ordinals 0..198. These
199 **actual native SHARC** renders take **41,661,645 instructions**, with
198 clean and exactly one stop at ordinal 198 (`0x1c1cd7`); the per-frame
instruction sum of the captured artifact agrees. The standalone queue
takes every supplied frame (including bytes originally produced by a
repeat); it reproduces **SHARC inputs and stop**, not the GUI's queue
take/repeat timing. It enables a targeted native pre-trap watch without
rerunning the GUI.
An independent Python SHARC replay of all 199 recorded inputs was
interrupted after about five minutes and ~5 GB RAM, before producing a
comparison; it is **not** a parity result.

**[C] [D]** A subsequent, unintegrated force-credit diagnostic's first
reported Python "counted" offer clocks (`925816`, `1135800`) must **not**
be treated as an exact-counted reference. The diagnostic enabled
`IdleSpin.stop_on_entry` during counted execution; `IdleSpin.on_spin`
then stops Unicorn early, but `emu.longrun.spin`'s counted branch still
credits the entire requested `step`. Thus those numbers include unexecuted
work and do not establish a counted-versus-fast observation-boundary cause.
The diagnostic is being corrected before integration. The earlier native
actual/fast-estimated offer-2 difference remains an observation, not a
proven clock-policy defect. No `Board`/`Time` scheduling change is justified
by the rejected counted result.

**[D]** A disposable, **unchanged-fast-trajectory** shadow ledger under
ignored `out/native/force-credit-lane/` subsequently passed its narrow
control-versus-observed gate. Both runs preserve force clocks `20014` and
`228381`, callback PC `0x400cccd8`, post-force PC `0x4002dd0c`, the single
available ordered TX frame's bytes/digest, and terminal PC. The diagnostic
stops immediately after force 2's original callback returns; it does **not**
render the second TX frame. Instrumentation adds no SR reads and delegates
each wrapped fast run, idle skip, and vector request exactly once. Its
**44,417 instruction-entry callbacks** are observed Unicorn entries, **not**
retired/native instructions. Neither the 1.2M-entry, 4,096-record nor the
90-second-per-run watchdog fired; the ledger has 39 records. The checked
report `shadow-ledger.json` has local SHA-256
`5ca8434442208af0319cf6684772d9b305012cdf66366bb37b3783326e04d625`.

At the Python yield-9-associated idle return, the existing fast callback
clock is **208,368**, below due `220014` by **11,646 credits**. The preceding
actual fast-run return values sum to **28,386**, and idle-skip return values
to **179,982**; their sum is `208368`. Idle-only phases add no stale block
credit. This establishes Python accounting at that observed phase, not
architectural alignment with the native idle-entry boundary, CPU semantic
parity or Device time. Corrected stock-counted callback clocks `925816`
and `1135800` remain a separate-cadence observation; the original method
was invalid, not proof those numeric clocks can never occur. The growing
permanent force-credit probe candidate was **not integrated**: its bounds
and stale fast-block telemetry were insufficient to support its claims.

**[D] [O]** The first native pre-trap instrumentation candidate was also
**not integrated**. It disabled block dispatch for instruction-accurate
PC/M1 observation, but its lane-local generated library stopped at rendered
ordinal **0**, `0x1c77b4` (`no decoded instruction`), instead of the accepted
original library's ordinal-198 stop. A matching generated core/image hash
alone did not establish equivalent execution coverage. No read/writer event
from that engine proves failing-path provenance. The original-library
blocks-on/off and late full-overlay restart gates below subsequently passed;
the failed instrumentation library remains rejected. A Python suffix would
diagnose execution from supplied native state, not establish independent
Python prefix or Device parity.

**[D]** No-rebuild gates using the original native library (local SHA-256
`13d3788d6988f7218232da5cdab3a5563e533bd6d21298a9492fd8b36d188514`)
reproduced all checked inputs 0..198 with block dispatch both enabled and
disabled. Both runs have 198 clean frames, the same ordinal-198 stop at
`0x1c1cd7`, the same **147,896** failing-frame instruction delta, and
**41,661,645** cumulative native instruction-stat counts. Every captured
per-frame delta and input hash agrees. Disabled-mode `block_entries` and
`block_instructions` are both zero. This excludes a difference between those
dispatch modes for the measured input/terminal/count sequence, not a shared
native-semantics defect, memory/PCM divergence, or a firmware/input problem.
Both modes share generated semantics; neither is an independent Python or
Device oracle. The complete mode measurement took 4.33 seconds, peak RSS
564,019,200 macOS bytes. Its ignored report is
`out/native/sharc-modes-lane/run-parent-fixes.json`, local SHA-256
`a19e0b4fa32e32d14acedda780f1f854b31b94b6ec918e5bdae00c047ad1648d`.

**[D]** The original engine also passed a bounded native/native restart
gate at **41,513,749** native instructions, immediately before ordinal-198
DMA injection. Full-overlay SHRD export (option 2) is canonical under the
Python codec. Fresh native import, reapplication of all pack configuration
options, actual special-presence mask **1**, and the same host/input context
re-export the identical pre-frame bytes. Running input 198 then matches the
uninterrupted terminal, PC, instruction delta, and full-overlay final SHRD
bytes. Ignored artifacts in `out/native/sharc-checkpoint-lane/`:

| SHRD pair | Bytes each | Local SHA-256 |
|---|---:|---|
| `checkpoint-before-198.shrd`, `restored-before-198.shrd` | 7,153,256 | `0e73bfc084d96193a995b5a29a3f646741e0d8c1ac63634caed605e6e9a68879` |
| `uninterrupted-after-198.shrd`, `restored-after-198.shrd` | 7,153,284 | `97733b2f58591166647429b0f675000d0a97724b98ac992c1e2534cf666e4fa2` |

`checkpoint-before-198.json` records the phase, next ordinal, pack options,
special presence, host constants, original-library identity, input/source
digests and raw-prefix digest. The complete measurement took 4.36 seconds,
peak RSS 1,126,825,984 macOS bytes. Parent artifact/hash/canonical-state checks
accept this checkpoint for **supplied-native-state suffix diagnostics only**.
This is not general SHRD/configuration losslessness, independent Python-prefix
agreement, Device state, or a fix for the stop; the zero-stop gate still fails.

The diagnostic's initial parser and ordinal-0 DMA failures are excluded:
trailer hashes are length-prefixed ASCII hex, and importing
`tools.sharc_core.state.Const` supplied a different class from the native
wrapper's `tr.st.Const`. Its setter silently selected Unknown for R8, causing
an invalid first-frame fork. Corrected harnesses assert the native R8 readback
before executing. Those failures were parent-recipe errors, not evidence of
an original-library execution defect. No production semantics, clock policy,
or generated library changed to pass these gates. Native failing-path writer
provenance and the supplied-state Python suffix remain **[O]**.

### Yield-9 fast-observed force attribution (2026-09-30)

**[D]** A scratch-only, unchanged-trajectory state ledger at
`/private/tmp/dt2-codex-timing-resume/out/native/force-entry-lane/entry_ledger_state.py`
(SHA-256 `a3ef64f33721dd4e3bec75f6db3bda72464faa49982a3f5e3437499a829ae9f8`)
captured the ninth Python idle return in
`/private/tmp/dt2-codex-timing-resume/yield9-state-ledger.json` (SHA-256
`c1d064f312b88ea4f3f4606c48e6024639dd0bb0cf57681e27eaa63ed4ab053e`).
Removing its diagnostic state fields exactly matches the accepted entry ledger
(SHA-256 `f7eb83178ed0227a07a9875bd6c1b01e643d1cdd46e05df448fd45b7f3e27c61`):
force clocks `20014` and `228381`, terminal PC `0x4002dd0c`, and the ordered
TX-frame SHA-256 are unchanged. At yield 9, PC is `0x400cccd8`, SR `0x2000`,
force 1 has completed, and the clock is `208368` with force 2 due at `220014`.

The observed second offer is host chunk cadence, not a CPU or Device-time
claim. `spin` calls `IdleSpin.skip` before advancing `pits.now` or invoking
`on_chunk` (`emu/longrun.py:1096-1128`); the next idle skip uses the next
reschedule boundary (`emu/longrun.py:797-807`) and adds `19998`, so
`208368 + 19998 = 228366`, already past due. Its following 15-credit fast run
makes `228381` before `FrameForcer.on_chunk` can offer force 2
(`emu/livesharc.py:125-138`). This explains that Python fast-observed boundary
without aligning it to native actual cycles or validating a native
`TimerPolicy::Device` contract. No regression test or clock/CPU semantic
change was added.

### Native suffix diagnostic PC accessor (2026-09-30)

**[D]** `native/sharc` now exports the read-only
`sharc_native_get_pc(handle) -> i64` diagnostic accessor. It returns the
architectural next `pc_sw`, rather than the separately modelled UREG `PC`;
null returns `-1`. `tools/sharc_transpile_run.NativeCore.pc()` binds it only
when present, so the accepted original library remains loadable and a request
against that legacy library fails explicitly without a full-state-export
fallback. A scratch diagnostic rebuild used the accepted generated directory
`out/native/opt/gen-final`, without regeneration; its SHA-256 was
`450a2d270ae71dc2706a991d172e46476af20f6d3c44bb7579ba534398fd5dac`.
The original library remains SHA-256
`13d3788d6988f7218232da5cdab3a5563e533bd6d21298a9492fd8b36d188514`.

Focused ABI probes passed: null maps to `-1`; an imported checkpoint's getter
equals its canonical `pc_sw`; repeated reads leave native counters unchanged;
and the accepted original library loads while `pc()` reports the missing
accessor. The one authorized suffix completed in 40.06 seconds (peak RSS
2,149,416,960 macOS bytes). Its scratch report and final SHRD are
`/private/tmp/dt2-codex-sharc-resume/native-suffix.json` and
`/private/tmp/dt2-codex-sharc-resume/native-after-198.shrd`. The diagnostic
and original libraries exported identical checked pre-state bytes before
execution. With blocks disabled, it completed the six-instruction DMA callback
and 147,890 handler instructions, then stopped at the accepted unmodelled-MMR
terminal `0x1c1cd7`; the 147,896 completed-instruction delta and final full
canonical SHA-256 `97733b2f58591166647429b0f675000d0a97724b98ac992c1e2534cf666e4fa2`
match the accepted uninterrupted state. Its 147,898 one-step requests include
the non-credited callback return trap and terminal MMR trap.

**[D]** The watched word at byte-space `0x28254d94` changed once, from
`0x3120b419` to `0x31093de7`, immediately after native prePC `0x1c2c64`
(form `14a`). The pre-instruction state had `R12=0x25f680`, `I0=0x25f190`,
`M5=0`, and `MODE1=0x00200000`; this is the observed changed-value writer,
not evidence about same-value writes. It is the same counterpart reported by
the supplied-state Python suffix, not an earlier `0x1c2f04` attribution.
Before the terminal form `3c` at `0x1c1cd7`, `I0=0x31093de7`, `M5=0`,
`R12=0`, `MODE1=0x00200000`, and the watched word is `0x31093de7`.
`forms_move._type_3c` takes its address from the old `I0` and advances `I0`
by `M5 * 4` under `assume_nw32`; the reconstructed address is therefore
`0x31093de7`, the unmodelled MMR in the terminal. This is a pre-execution
register/rule reconstruction, not a post-trap probe or a native memory trace.
This closes the failing-path writer-provenance question for the supplied-native-
state suffix. Independent Python-prefix agreement, the semantic cause of the
MMR stop, and the zero-stop gate remain open.

### Type15b SIMD companion transfer (2026-09-30)

**[D]** *SHARC+ Core Programming Reference* (SC58x/2158x Rev. 1.5), printed
pp.16-8--16-12 (extracted pp.0391--0395), requires a non-(LW), normal-word Type15b DAG
transfer in `MODE1.PEYEN` mode to perform both halves of the transfer. The
explicit `R12 = DM(I2 + 8)` at `0x1c2c5a` reads `0x25f680` from `0x2fffe8`;
the implicit PEy half reads `S12` from the next normal word, `0x2fffec`, whose
value is `0x25f700`. The prior Type15b scalar path updated only R12. Type15a
already used the common companion-transfer rule; the fix extends that same
rule to Type15b's non-(LW) DM load/store path, preserving the existing `(LW)`
register-pair path and non-SIMD behavior.

**[D]** Updating the Type15 load metadata changed only the DT2 1.16
`trace_voice` golden: its SHA-256 is now
`207907476dcc3f3f7bb67ec885609e190a2d00cd71a6a7e047d2f1723286f3b5`.
The structured trace difference contains 24 added `space: "DM"` fields in
`last_events`; states, event values, and the other five golden hashes are
unchanged.

Focused Python and scratch-native counterexamples cover the indexed
normal-word load/store pair, PEYEN off, unchanged I register, and an
uncomplementary load. The corrected native checkpointed ordinal-198 suffix
writes the watched PEy word `0x25f700` and no longer reaches the old
`0x1c1cd7` MMR stop; it returns through the existing block-handler boundary
at `0x1c75d3` after 219,827 handler steps. The full 256-input recorded GUI
replay is still incomplete: its first new stop is ordinal 187 at unmodelled
MMR `0xb829eb` after 190,499 instructions. This is a bounded emulator
regression result, not autonomous GUI, boot, timing, or Device validation.

**[D]** `0xb829eb` is the trapping software PC, not the attempted MMR
address. A checkpoint immediately before ordinal 187 has canonical SHA-256
`3537bd2d59fecf35bba0b499801ec8379226a594fcf9da298046e7a28be9348a`.
Sparse pre-trap capture decodes the instruction there as confident Type3a,
`R2 = DM(I2, M6)` with post-modify (`u=1`), normal-word DM access, `I2 =
0x3106fc64`, and `M6 = 1`. Type3a uses the old I register as its post-modify
address, so the unmodelled access is `0x3106fc64`; the post-modify value is
not needed to explain the trap. The instruction belongs to
`FUN_00b82994`. Its pointer provenance remains unmodelled and no additional
ISA or memory semantics were added to continue this replay.

### Fresh SHARC pack and firmware-free WASM gate (2026-09-30)

**[D]** The current frozen core hash is
`4b25379c9ea67fc5933d9e194ba1d4d13ace66c57c5b5e1919a57fd66f34fd7f`.
The exact fresh library is SHA-256
`7e1bf5897ed4634df2d02fcee3c6dbf28576a9677a4f70f5ae22ca4b488b402a` and
its fresh initial pack is SHA-256
`52ff0b9bf05a5efaae963a8a83b4afc395b7a37902d739ee1379600ca018fa39`.
They are scratch artifacts under `/private/tmp/dt2-sharc-completion/`.
The 204-input native replay completed at the accepted bounded return
`0x1c75d3`, with 43,828,934 instructions and final canonical SHA-256
`794efcf929c23633dda872cc82b5c7fbcd52262fef361e739638bbf6a96ab83d`.
The 256-input replay completed with 54,247,701 instructions and final hash
`4b840c3c…a891`. These are state/terminal gates, not full instruction-parity
or realtime claims. Rebuild the initial pack after any semantic change.

**[D]** A generic transpile produced only `core_g.rs`, `core_i.rs`, `syms.rs`,
and `tables.rs`; it contains no `image.rs`. The resulting 579,724-byte WASM
runtime (SHA-256 `98669a1372d8bcfb325af20d9f2d0d461543f8635aff33e7bfa5e082c7194742`)
reports image `none` and decodes the SHFP pack image at runtime. Node replayed
the exact 204 rendered inputs in 4.404 seconds, with 43,828,934 instructions,
no JIT requests, and the same canonical final SHA. This is a firmware-free
interpreter gate under Node, not browser or realtime execution.

**[D]** The replay recipe enters the DMA callback, sets R8 to the host event,
then steps the callback before entering the handler. A scratch runner that set
R8 after stepping DMA produced idle handlers; the corrected ordering matched
the native checkpoint states and counts. That was a runner-recipe error, not a
core defect.

**[D]** Type3c manual attribution is *SHARC+ Core Programming Reference*
(SC58x/2158x Rev. 1.5), extracted pp.0323--0324. The old checkpoint-198 and
old-pack ordinal-187 trap reports remain historical diagnostics: in the latter,
`0xb829eb` is PC while `0x3106fc64` is the attempted address.

### Native host and remaining integration boundary (2026-09-30)

**[D]** Fresh host `--main` and `--syx` checkpoint runs each produced 68 TX
frames, 204 clean SHARC renders, zero stops, and zero DMA failures. They share
the exact wire SHA `e21cb015102bd6c1222b7c733238ddf7402a725ee7e63b0fb8313d00fd1f8e8b`,
PCM SHA `b7f12d308007e0d4b746cae09136d758cb808dfdbd666d0f1e098befaec73b97`,
and WAV SHA `4771ca619239a277cffcb8008095d6c087321607d83cc5e58fad3f33494a0f9e`;
outputs include `host-main.wav` and `host-syx.wav`. The host
writes its reports before returning failure for stops, DMA failures, incomplete
work, or wire mismatch. Its optional `--rendered-input-log` accepts only
rendered records of at most 4096 bytes. This remains an Oracle checkpoint,
not cold boot, autonomous timing, or browser evidence.

**[D]** Rust GPIO's hook is optional. Oracle INTC preserves Device reset with
zero masks; the `+14` offset applies to INTFRCL, not IMRL. Board capture owns
pending/outbound TX through `take`, and both TX35 and DSPI use SERQ-all. The
current full-machine test set has 83 passing and 13 ignored tests, plus a WASM
compile check. These gates do not establish Device scheduling.

**[O]** Cold-boot scratch work uses explicit DTIM1 `[3,1]`, Oracle 132M IPS,
zero-page compatibility, flash HLE, forced UART/DSPI/ZeroPeer, and idle
handling. The historical 1B run without UART service left the console full
ring stuck, with zero main-loop/job progress and DTIM3 despite PIT3 delivery
and the early real-97 handler returning to sleep. A subsequent UART TX35
diagnostic-service run reached the captured main UI described in the next
section. Full Device/autonomous DT2/DN2 boot, UART RX, audio, realtime JIT,
and browser runtime are unfinished.

**[D]** `native/boot` is the promoted bounded Oracle diagnostic runner. It
uses the embedded firmware registry and image-resolved marks before creating
an output directory; `--diagnostic-services` explicitly enables its scratch
compatibility bundle. Its JSON/progress artifacts describe a bounded
observation and do not claim a complete checkpoint or hardware boot.

### Native completed main-panel frames (2026-10-01)

**[D]** Bounded Oracle diagnostic boot captures completed post-intro panel
frames at the image-resolved panel-diff return boundary and attributes them to
the owning main-loop TCB. Its shared readiness predicate requires a nonzero
completed main-TCB frame, intro completion, main-loop and job-pump marks, and
DTIM3; PIT3 and the progress/vector-208 handoff are diagnostics only. This
permits the ordinary Digitone main-UI path, which need not use the
maintenance-only progress handoff.

The DT2 run in `/private/tmp/boot-dt2-panel-20261001/report.json` reached
ready at instruction 825,549,963 and captured a completed main-TCB frame with
SHA-256 `9afbef4eea20d8660ffc6d439a6c0dd5397ab1599985c8be28526553a57456c1`;
its visible UI says `MMC NOT IN SLC MODE`. The corresponding DN2 run reached
ready at instruction 842,205,907 with frame SHA-256
`19f8d7cdd7db23c03c80e86b79f3fdb09a497288fbabf4aa53b12707cc3d73f0` and a
LIGHTHOUSE main page without that warning. Both use the diagnostic Oracle
bundle with a blank card and ZeroDSP. They prove the captured main UI under
that bounded compatibility setup, not full hardware, audio, or interactive
boot behavior.

**[D]** The portable Rust +Drive builder matched Python byte-for-byte for the
14-WAV PCM, float, and extensible fixture set: sample-only image SHA-256
`44e68112018aa1b95c14d0148d554989915ccd46ccc6de6453dbca82a62e6613`;
the DT2 1.16 seeded-project image SHA-256 was
`1caf07515a81ae2f8fcf023f1d9b316359b809cffba165b76f7e1a3a4c44a4e3`.
Both have logical length 3,959,422,976 bytes. The seeded record replaced 297
table references and 1,471 other copies. These are format-builder parity
results, not a claim about firmware storage behavior.

**[D]** With a mounted generated image, the generic SD gate and image-resolved
MMC completion words changed the bounded runs from zero storage commands in
the earlier panel captures to real eSDHC traffic: DT2 reached ready at
830,371,724 instructions with 3,151 commands and 34,287,616 DMA bytes; DN2
reached ready at 847,307,251 with 2,983 commands and 26,898,432 DMA bytes.
The runner's synchronous Oracle completion shortcut is not a Device ISR
model. The first structural main UI still shows DT2's VERIFYING FILE SYSTEM
modal and DN2's SLC advisory, so neither run establishes a fully usable or
final boot. Full SHARC, audio, autonomous Device behavior, and browser
execution remain unproven.

**[D]** Readiness is profile-owned: DT2 1.16 uses a typed
main-panel-fs-check-v1 contract whose startup checker entry and unique
completion stores are image-resolved; a successful done-byte store and a later
completed main-TCB frame are required. The earlier 40-byte
0x400caac6 marker was sample verification, not filesystem verification.
DN2 1.11 uses main-panel-v1, which does not claim unresolved
filesystem-verification work.

**[D]** Final bounded `mise run boot` observations reached `VERIFIED_READY`.
The seeded DT2 run in
`/private/tmp/boot-dt2-startup-fs-final-20261001/report.json` stopped at
914,896,867 instructions: the scanner started and completed once,
`success_result` was true, its last completion was at 914,789,367, and the
later completed main-TCB frame at 914,873,310 has SHA-256
`e6ee4f9326852a09ace5c899b103e3c951a774b13cbe43143bdeceb1368c699f`.
It used the seeded image whose known full SHA-256 is
`1caf07515a81ae2f8fcf023f1d9b316359b809cffba165b76f7e1a3a4c44a4e3`.
The DN2 sample-only run in
`/private/tmp/boot-dn2-contract-ready-20261001/report.json` stopped at
847,307,251 instructions; its completed frame at 842,990,780 has SHA-256
`52979c6ce5d3f29344f9699df61a22c071eb0bcd5cf13e9789610f691fc3d38d` and
shows `MMC NOT IN SLC MODE`. DT2 evidence is limited to filesystem checking
and the main panel. DN2 evidence is main-panel-only; filesystem-verification
completion remains unresolved and is not claimed. Both remain Oracle
diagnostic observations: full hardware, audio, input, GUI, and WASM bridge
work remain pending.

**[D]** The bounded portable runtime produced identical native and WASM JSON
snapshots and packed OLED frames at ready, two `NO` presses, and a subsequent
encoder turn for DT2 and DN2. DT2 reached ready at 914,736,053 instructions
and DN2 at 847,486,347; each run delivered five RX interrupts and drained its
input queue. This checks the shared diagnostic runtime and raw-WASM boundary,
not DSP, audio, hardware timing, or a complete GUI bridge.

### Opt-in Softfloat ABI substitution prototype (2026-10-01)

**[O]** The portable runtime has an opt-in `softfloat-abi-v1` execution policy
for guarded `addsf3`, `mulsf3`, and `divsf3` ABI-result substitution on only
registry-verified DT2 1.16 and DN2 1.11 images. Default construction and the
existing raw ABI entry retain reference execution. A successful substitution
uses host binary32-rounded arithmetic only for finite normal operands (or
signed zero) and a finite nonzero normal result, then writes D0, pops only the
return address, and transfers to that return address. It intentionally does
not claim scratch-register, stack-scratch, exit-CCR, instruction-count, or
interrupt-timing equivalence.

**[D]** The prototype records separate legacy CPU, interpreted-instruction,
flash-HLE, softfloat-HLE, and synthetic Oracle-tick counters. An accepted
atomic softfloat call consumes one Oracle scheduling tick without incrementing
the legacy CPU counter; failed guards execute firmware normally. It checks
the complete arithmetic code region against verified image bytes before each
substitution, using allocation-free backing-RAM comparison rather than copying
1 MiB pages. Division rounds directly in binary32. Per-routine firmware
differential, WASM, browser, and full architectural equivalence remain untested;
the bounded native observations below do not establish those claims.

**[O]** The shared chunk runner also evaluates verified `BRA.B`-to-self idle
passes analytically after one ordinary pass, stopping strictly before the next
Oracle timer deadline, the 20,000-pass rescheduling boundary, or chunk end.
It retains the exact guest clock/pass count and unchanged CPU state, and reports
`idle_fast_forwarded_instructions` separately from actually interpreted work.
Queued input, trace/non-running CPU state, changed code, or exceptional state
disable this shortcut. This mirrors Python's `IdleSpin.skip` approach, not a
timer hold or an estimated instruction count. The DT2 sequence below has been
reproduced; DN2, current WASM, and wider firmware parity remain pending.

### Performance and real-time audio checkpoint (2026-10-01)

**[D]** Removing PIT/DTIM service-loop channel clones, then retaining timer
ownership in `Option<Box<Time>>`, reduced observed native DT2 full-machine QA
wall time from about 119 to 66 seconds. The final scoped box-only pair was
89.995 to 66.466 seconds; matched intro throughput increased 32.8%. These are
single observations, not statistical or real-browser/audio results. Both
devices' native/WASM reference captures matched. Evidence:
`/private/tmp/digi-box-timer-main-ujmZRJAi/{RESULTS.md,summary.json}`.

**[D]** The softfloat-only native DT2 pilot, before idle advancement, reached
first nonblank output at 6.647 seconds, first nonblank main output at 52.992,
and readiness at 60.831. It accepted 1,009,206 arithmetic calls; one button
packet was delivered and drained. This did not resolve the minute-long boot
and was not a matched reference comparison. Evidence:
`/private/tmp/digi-acceleration-1yDSzAfs/dt2-native-pilot/report.json`.

**[D]** With idle advancement in reference mode, the complete DT2 ready/two
NO/encoder sequence reproduced all five binary captures and every legacy
ready/final JSON field. Additional accounting fields are intentional. Final
guest count remained 974,985,741: 629,900,776 interpreted instructions,
345,084,958 analytically evaluated idle instructions, and seven flash calls.
There were five input IRQs, no pending input/error, and verified filesystem
readiness. Wall time was 51.872 seconds, not a fresh paired timing result.
Evidence: `reference-idle-isolated-receipt.json` under the same temporary
directory. Thirteen boot-library tests and the new backing-RAM comparison
test passed; active LSP checks of the changed Rust paths reported no errors.

**[D]** Source inspection found the existing Python live-audio route feeds
ColdFire DSPI2 frames to the native SHARC renderer, but uses forced SSI0
interrupts, zero DSP RX replies, and repeated command frames. Audio-buffer
pacing is not a shared CPU/DSP clock. The new Rust boot/desktop/browser paths
have no SHARC/audio integration. `native/live` does execute real SHARC frame
work via its native library; its 32-sample, requested-48-kHz output gives a
nominal 0.667-ms render budget, not demonstrated full-device timing.

**[O]** A 100-frame device-free DSP benchmark refused its stale live pack
before rendering. Pack core hash `124ec4db...` differs from current native/core
hash `4b25379c...`; do not bypass that guard or merely retag the pack. The
current library's core/generator check passed and its image hash matched
`sections/section_7_BLOB.bin`; the sections source SHA matched DT2 OS 1.16.
No DSP timing, underrun, or synchronized real-time result was obtained.
Evidence: `sharc-100-receipt.json`, `sharc-100.log`, and
`meta/sharc-baseline-provenance.json` under the same temporary directory.

### DSP decode allocation and first-block performance follow-up (2026-10-02)

**[D]** The stale pack's 1,114 DMA frames, starting at capture frame 74,
matched `out/captures/drive3/dt2-1.16-drive3-trig1-emac.dt2cap` byte for byte
after the existing halfword swap. Rebuilding from that capture and
`out/captures/drive3/flexbus-drive3.raw`, with `--rebuild --limit 2000000`,
produced v3 pack key `2cf652cf46d0d2b45bab69a3` in 77.55 seconds. Its embedded
image exactly matches the current loader image. The actual loaded
`out/sections/dt2-1.16/section_7_BLOB.bin` hash is `0f514a12...`, matching the
native library's image hash; DT2 1.16 SYX SHA is `278541e4...`, matching the
device contract. Pack and native library both name core `4b25379c...`.
No stale guard was disabled or pack state retagged.

**[D]** The existing native library rendered 100/100 frames cleanly with no
DMA failures or stops. The initial observation was median 457.3, p99 709.4,
max 934.1 and first-frame 14,107.5 microseconds. Interpreter fallback was
6,044 of 21,948,991 counted instructions (0.0275%). Block profiling showed
distributed AOT work; its top entry `0x1c06ba` accounted for about 6.4% of
profiled block time. Profiling adds overhead and does not provide comparable
unprofiled deadline measurements.

**[D]** `native/sharc/src/canon.rs` now stores the 103,289 decoded
instructions, their field descriptors and field entries in three contiguous
process-lifetime arrays, replacing roughly three heap allocations per
instruction. Lookup pages, field order, signed values, duplicate-PC behavior
and unknown-PC handling are preserved. Parsing completes before publishing
the arrays, so a truncated decode does not leak partial instructions.
`native/sharc/src/lib.rs` initializes this host-only table during engine
construction, before rendering starts. This executes no guest instructions
and changes no guest counters; it moves initialization cost into library/core
loading. Code-page faults and other first-use costs can still affect rendering.

**[D]** Five serial alternating baseline/final pairs each rendered 100 frames,
using the same fresh pack and `live_play` executable, interactive QoS, default
release flags and the pinned Rust 1.98.1 toolchain. Median across the five
per-run statistics (microseconds):

| Statistic | Baseline | Final |
| --- | ---: | ---: |
| First frame | 6,971.4 | 1,184.1 |
| Steady median, excluding first frame | 446.6 | 445.4 |
| Steady p99, excluding first frame | 526.2 | 522.2 |

First-frame ranges were 6,868.2–7,129.5 and 1,101.7–2,435.3 microseconds.
The first final run also incurred first-use costs for its new library file;
these are fresh-process runs with an uncontrolled OS file cache, not a cold
machine experiment. The 83.0% reduction describes the first **render** stall,
not overall startup or sustained throughput. An intermediate arena-only build
reduced cached-library first frames to about 4.5 ms before eager initialization.
Steady throughput has no material demonstrated improvement. All five final
first blocks still exceeded the explicitly nominal 666.7-microsecond budget
for 32 samples at requested 48 kHz; no steady blocks missed it in these final
pairs. Earlier observations did miss it, so these short runs do not establish
worst-case deadlines. A ThinLTO/16-codegen-unit experiment preserved parity
but showed inconsistent median changes; default build flags remain unchanged.

**[D]** The final build matched all 100 baseline canonical state hashes and
all dumped f64 PCM bytes. Every paired run retained 21,948,991 counted
instructions and 6,044 fallback instructions. PCM SHA-256 was
`4744e837e67720ebbf79dd5157ab2bcff18dffaeadb869426d5cfa5b94593e64`.
Twenty SHARC runtime tests passed, including field-arena/PC-boundary and
every-prefix truncation checks. The real wire-order live-source/capture-source
parity test passed. The DSP-only offline gate rendered two fresh 160-frame
runs, each with 5,120 stereo samples, non-silent output and Q31 PCM FNV-1a64
`e89d808cd7de9585`. The generated AOT wasm32 frame module passed locked/offline
Cargo check; this is a compilation gate, not browser execution. Independent
review found no issues. Formatting was checked on changed files only.

Evidence: `/private/tmp/digi-audio-performance-yed29asm/`, particularly
`provenance.json`, `summary.json`, `final-comparison.json`,
`final-parity-receipt.json`, `final-parity.log`, `offline-gate-serial.log` and
`wire-parity.log`. The matching pack and final library were also retained in
ignored `out/native/audio-perf-20261002-yed29asm/`. Firmware-derived artifacts
are not commit candidates. Reproduce the bounded device-free render with:

```sh
native/live/target/release/live_play \
  --pack out/native/audio-perf-20261002-yed29asm/2cf652cf46d0d2b45bab69a3.pack \
  --lib out/native/audio-perf-20261002-yed29asm/libsharc_native.dylib \
  --bench 100 --times /private/tmp/digi-audio-100.times
```

Rebuild the library with `SHARC_GEN_DIR` naming the current matching generated
sources (`out/native/opt/gen-final` here), an external `CARGO_TARGET_DIR`, and
`rustup run 1.98.1 cargo build --release --locked --offline --manifest-path
native/sharc/Cargo.toml --lib --bin sharc-frames`. Regenerate those sources
first if their core/generator provenance differs.

**[O]** This is captured-state DSP rendering of voice work-buffer taps.
ColdFire/SHARC shared virtual time, SPI reply semantics, SSI0 delivery,
representative polyphony, sustained coupled deadlines and actual device/browser
audio remain unvalidated. The next integration step needs an evidenced reply
and timing contract; sending captured frames with fabricated zero replies
does not establish it. The boot/desktop/browser runtime still does not connect
to this DSP renderer.


### Shared boot runtime performance, intro publication and warning audit (2026-10-02)

**[O]** The user's actual desktop DT2 and DN2 boots were slow before the main
UI, and the Elektron intro flickered. Main page buttons were responsive after
boot. This is a different path from the captured-state SHARC renderer above:
the Tauri host and browser WASM both use `native/boot::Emulator`, which still
has no SHARC/audio integration.

**[O]** A bounded, fresh-firmware native QA harness sampled with macOS `sample`
identified expensive host work at every interpreted instruction: timer-bank
service/deadline scanning and the frame-completion observer's current-TCB RAM
read. The matching baseline was built before these changes from the existing
runtime, including analytic idle advancement. Changes preserve the reference
execution policy and instruction boundaries:

- `machine::Time` caches the earliest possible timer service boundary when no
  PIT/DTIM IRQ is pending. Every timer/controller write, page load, component
  restore and explicit deadline-arm operation invalidates it. Refused/pending
  IRQs still get an offer on every instruction; SR seeding remains unchanged.
  The cache uses the original floating-point deadlines and disables itself for
  NaN deadlines. Device-policy errors remain immediate.
- `FrameTracker::complete_at_return` checks pending return PC and A7 before
  reading the current TCB. It retains the original owner and completion checks
  when a return can match.

**[O]** The initial native ready/two-NO/encoder sequence took 56.17 -> 33.72 s
for DT2 and 43.47 -> 27.53 s for DN2. Before the intro fix, both ready/final
JSON files and all five framebuffer captures were byte-identical on each
device. The DT2 baseline was sampled for ten seconds, so its wall-time result
is not an uncontaminated speed measurement. A later unsampled DT2 baseline
completed in 50.77 s. Final timings and frame parity are recorded below.
These are bounded single-run observations, not real-time audio or UI latency
benchmarks. Logical `icount` includes analytic idle and flash substitutions;
never report it as interpreted MIPS.

**[O]** Intro probes through 400M logical instructions showed 74 complete DT2
raster ends and 70 DN2 raster ends. Each observed cycle visited all 8192
coordinates, ending at `(127,63)`; the next `(0,0)` entry already saw a cleared
bitmap. DT2 had 73 observed next-cycle boundaries and DN2 had 70, all with a
zero bitmap. Chunk-boundary publication alternated zero, partial and complete
images. This establishes why capturing at either a chunk boundary or the next
cycle's first pixel causes flicker. Pixel order must not be assumed from the
ending coordinate: an initial strict column-order implementation failed the
full replay by withholding the intro and was replaced before delivery.

**[O]** `IntroFrameTracker` now starts at `(0,0)`, tracks distinct coordinates
on the same bitmap with a 128-word bitset, and waits for coverage of all 8192
pixels. It captures the real bitmap only after the last setPixel call returns
with the expected PC and stack pointer. Live dimensions, stride and storage
are validated at capture. Partial/duplicate cycles cannot publish; the last
complete image remains available. A complete black image is still published.
Main task/return completion and the main-frame latch retain their previous
behavior. This observes existing guest execution; it adds no pixel HLE,
synthetic display or skipped guest work.

**[O]** A scout traced the ten shared-library `dead_code` warnings to the
same `common.rs` being compiled independently by the portable library and
CLI binary. CLI-only constants, instruction/task/context records and exit
checks moved to `diagnostics.rs`, included only by the CLI. Shared telemetry
and frame metadata that the CLI actually prints have narrow, documented
`allow(dead_code)` annotations; there is no crate-wide warning suppression.
The CLI now uses the same `LoggingBus` constructor as the runtime. Fresh
native QA, WASM, desktop release builds and boot tests emit no Rust warnings.
These warnings did not identify unfinished boot/display functionality. The
missing CPU/DSP/audio integration remains a separate, documented gap.

**[O]** Verification: all 30 boot tests pass, including publication after the
last pixel return, both raster traversal orders, retaining a completed image
during partial drawing, rejecting a duplicate, and publishing a complete
black raster. The timer library/integration selection passes 43 tests; its
new differential test compares cached versus uncached service at every
boundary with blocked/refused IRQs, MMIO changes, page loads, deadline arming,
fractional clocks and counts beyond 2^53. A NaN-clock regression is covered.
An independent reviewer reported no remaining findings for the timer cache,
frame guard, warning cleanup and corrected coverage-based intro observer.

Evidence is in `/private/tmp/digi-desktop-performance-5jtmamth/`: canonical
`qa/src/main.rs`, pre-change `target-baseline`, final `target-final`,
`target-wasm`, `target-desktop`, sample, probe logs, build/test logs, receipts
and framebuffer captures. Firmware contracts remain DT2 1.16 and DN2 1.11.
Builds use pinned Rust 1.98.1, `--locked --offline`, external targets and
explicit `RUSTC`/`RUSTDOC`/sysroot library paths. Each timed run is serial,
limited to 300 seconds and at most 1.1B logical instructions, stopping at
ready plus 60M instructions after two NO presses and an encoder turn. The
Node WASM harness instantiates the actual external build, with no substitutions
or extra imports. Temporary evidence can be OS-cleaned; no new commits were
made and no owner's app/server was stopped.


**[O]** Final complete-raster native sequence: DT2 34.45 s versus the 50.77 s
unsampled baseline (32.1% less time); DN2 27.74 s versus 43.47 s (36.2% less).
Final Node WASM sequences completed in 45.21 s and 36.10 s respectively;
there is no new WASM baseline or browser UI wall-time comparison here.
All four runs reached ready without a fault and accepted the bounded input
sequence. Native baseline versus final status differs only in the intentional
frame-revision count: DT2 ready 797 -> 98, final 801 -> 102; DN2 ready
794 -> 88, final 795 -> 89. Ready occurs at the same logical instruction
counts (914,736,053 DT2; 847,486,347 DN2), with the same interpreted, idle,
flash and timer behavior. All four main/input frame captures match baseline.
The intro capture intentionally changes to the last complete image.

**[O]** Final native/WASM comparison matches every ready/final status value,
all five framebuffer captures, and all intro revision instruction counts and
pixel counts on both devices. Each device emits 87 changed intro images;
first publication is a complete 367-lit-pixel logo at 204,749,605 DT2 or
222,249,605 DN2 logical instructions. The minimum observed complete-image
pixel count is 135; the unit test, rather than this firmware sample, verifies
a complete black image. The last complete intro capture on both devices has
SHA-256 `fc6c0df3c364515e4f62c1904b77f01494c8a5c8a6df1e66ea7bce935d98abad`.
The new WASM SHA-256 is
`bf3902913fd5f22c487ff13af15103507f1481db772d3f223dc95a225fef218d`;
the tested external build was explicitly copied to
`packages/web/public/emulator-core.wasm`. An existing browser worker retains
its instantiated module, so refresh the page to use the new core. Relaunch
via `pnpm --filter @digi/desktop dev` to rebuild/use the changed native core;
the existing desktop process was not replaced. The external desktop release
is `target-desktop/release/digiemu-desktop` in the evidence directory above.
No actual GUI visual/input session was exercised in this follow-up; native
host release compilation and shared runtime/ABI replay are the evidence.


**[O]** Final full `native/machine` suite: 90 passed, 13 existing fixture tests
ignored; `native/boot`: 30 passed. Changed-file rustfmt checks and the Astro
static build pass. The static output and public WASM hashes match. Curated
logs, native/WASM captures, harness sources and receipts are also retained in
ignored `out/native/desktop-perf-20261002/`; large target directories and the
external desktop executable remain in the temporary evidence directory.


**[O]** Subsequent user GUI verification: the boot animation is smooth, there
is no flickering, and the UI is responsive after loading. The user still
observes roughly 200M logical instructions before a bootscreen appears.
This validates the visible publication fix; it does not measure audio,
explain the pre-display guest work, or establish real-time hardware pacing.


### Retained boot diagnostics and audio integration audit (2026-10-02)

**[O]** The user reports that a physical DT2 immediately shows the Elektron
logo and OS version 1.16, followed by the animation. The portable runtime
executes section 3 MAIN at `0x400004e8`; it does not execute section 2
bootstrap at `0x80000400`. Host setup supplies selected Board/Oracle state
(SR/A7, RAM, timer/interrupt policy, forced status values and flash image).
Existing evidence establishes MAIN intro code, but not which boot stage owns
the physical immediate splash. Adding bootstrap would add earlier guest work;
it cannot by itself make MAIN's execution faster. Its compatibility benefit
should be established by tracing an actual missing handoff or state dependency.

**[O]** New retained diagnostics are observational and export on demand through
`Emulator::diagnostics()`, the `digi_diagnostics` WASM ABI, and the session-
checked Tauri `emu_diagnostics` actor request. **Export diagnostics** in the
faceplate downloads JSON, including after a runtime fault. Report requests do
not step the guest, consume a pending frame or read MMIO. The schema includes
firmware MAIN hash, execution policy, separate logical/interpreted/idle
counters, first milestones, timer delivery counts, storage/DMA/UART/DSPI
counters, fault, enabled profiling and event history. SHARC and PCM connection
flags truthfully remain false for the current boot core.

- Always-on milestones retain the first observation only in a fixed 15-slot
  array. They cover entry, first task/pixel/complete raster/published intro,
  intro exit, display start/main publication, filesystem start/completion,
  ready, input-ready, UART TX, DSPI exchange and fault. Each records its
  observation boundary: instruction, chunk end or snapshot. The DSPI first
  milestone is an observation at chunk end, not its exact transfer timestamp.
- `diagnostic-profile` samples once per 16,381 interpreted instructions into
  three fixed 1024-bin (4 KiB PC bucket) histograms: before intro, intro and
  main. Analytic idle advancement has its own counter and is not reported as
  interpreted samples. It is a sample estimate, not exact instruction/call
  attribution. Code/storage are absent when this feature is disabled.
- `diagnostic-events` retains the newest 256 events in a preallocated ring
  and reports how many older entries were dropped. Entries include first
  milestones, timer-delivery batches, input/encoder requests, input IRQs,
  storage completions, DMA writes, UART TX and DSPI exchanges observed at
  chunk end. It reuses existing successful execution/completion observations;
  it does not probe devices again. Code/storage are absent when disabled.

**[O]** The host separately measures load/step response wall time, response
counts, maximum response duration and first-frame/main/ready response times.
Browser measurements cover WASM ABI execution/decoding; desktop measurements
include IPC and actor response time. These are not firmware cycle times or
physical display-present measurements. Host clocks are sampled per chunk,
never per guest instruction. No timer policy, guest pacing or defaults change.

**[O]** The normal portable bus now specializes `LoggingBus<false>`, compiling
out the per-fetch/read/write record collection. The diagnostic CLI retains
`LoggingBus<true>` and its original observations. RAM/compatibility fault
mapping, PPMCR behavior, interrupt checks and device accesses remain active
in both specializations. `diagnostic-trace` restores detailed portable bus
collection for focused debugging; it is separate from lightweight PC/event
sampling and carries a larger cost. Default builds have none of these three
optional features, while retaining milestones and export.

To enable PC/event diagnostics for a native diagnostic build, use
`--features diagnostic-profile,diagnostic-events` on `native/boot/Cargo.toml`.
For the desktop, use `pnpm --filter @digi/desktop dev --features diagnostics`;
the desktop feature forwards PC/event features without detailed bus tracing.
For a browser diagnostic asset, run `tools/native_wasm.sh --diagnostics`, then
reload the page. Running it without the flag restores the default build.
The WASM build script now copies from the actual `CARGO_TARGET_DIR` when set,
resolving the previous stale external-target copy trap. All Cargo work still
uses the pinned toolchain, offline/locked dependencies and external targets.

**[O]** Audio audit: neither current host attaches a SHARC engine or consumes
PCM. Board DSPI2 transport currently uses the captured-trace-compatible
`ZeroPeer`; the FlexBus FIFO models ready/bookkeeping and discards transferred
words. `native/periph::ssi` is not connected to the active renderer. The native
SHARC/live infrastructure can execute a matching captured state and tap voice
work buffers; the Python live path forces SSI0/vector 191, returns zero RX and
repeats frames under independent pacing. This is not validated coupled device
execution or final DAC output. Silence in the new frontends is therefore an
integration gap, not evidence of a muted functioning audio engine.

The next defensible audio slice is to trace actual ColdFire DSP load/DSPI TX
activity from this same boot runtime, then supply matching real DSP state and
establish CPU/DSP virtual time, RX causality and SSI0 cadence. Coupled PCM
should first pass a bounded offline render/nonzero/hash check, followed by
native audio-device and browser AudioWorklet sinks with underrun/backlog
metrics. A fresh boot must not silently substitute a captured-state benchmark
or call voice-buffer taps the final DAC/master-FX mix.


**[O]** Retained-diagnostics replay evidence is in temporary
`/private/tmp/digi-diagnostics-ff4s2up9` and curated ignored `out/native/diagnostics-20261002/`.
All default native, PC/event native and default Node WASM runs match the
committed `40731a0` runtime's complete ready/final JSON and all five binary
captures for DT2 and DN2. All three modes match the 87 intro publications and
all base diagnostic fields. A PC/event WASM DT2 replay also matches the native
PC/event report in full, including histograms and the bounded event history.
These are deterministic replay checks, not audio or unrestricted hardware proof.

| Canonical ready/input replay | Native default | Native PC/events | Node WASM default |
| --- | ---: | ---: | ---: |
| DT2 | 30.20 s | 30.88 s | 41.65 s |
| DN2 | 24.65 s | 25.23 s | 34.82 s |

A freshly rerun committed DT2 baseline took 32.76 s: disabling unused detailed
portable-bus tracing reduced this single comparison by 7.8%. PC/events added
2.3% (DT2) and 2.4% (DN2) versus the new default in this batch. This is one
serial run per mode, not a distribution or a real GUI timing. The optional
PC/event WASM DT2 replay took 42.34 s. Export/hash/serialization cost occurs
only on request; bounded milestones still carry a small observation cost.

**[O]** Before the DT2 first complete raster there were 12,467 PC samples:
56.94% in `0x40182000` and 11.04% in `0x40183000`, 4 KiB buckets containing
software floating-point routines/helpers. Another 15.85% falls in
`0x40000000`, including the early RAM clear. DN2's `0x40178000` bucket contains
62.64% of its 13,545 pre-intro samples. This localizes expensive firmware work;
it does not attribute every sample to a specific routine or validate the
experimental arithmetic substitutions. Keep `SoftfloatAbiV1` opt-in until
its ABI/state/interrupt contract has stronger differential evidence.

DT2 first UART TX is observed at 32,979,092 logical instructions; first
setPixel at 203,458,677; first complete raster at 204,520,827; first intro
publication at 204,749,605. Earlier UART traffic does not prove an OLED splash.
A focused early display-transfer/handoff trace is preferable to adding the
entire bootstrap or drawing a substitute logo. Both canonical boot/input
replays report zero observed DSPI2 exchanges/TX bytes. That is a finding for
this scenario, not proof that every guest/audio scenario has no transfers.
Investigate actual DSP-load/TX production before assuming PCM plumbing alone
will make this runtime audible.

**[O]** Final boot tests pass with default features (33 tests) and all
`diagnostics` features (35 tests). Desktop default release build and PC/event
feature check pass without Rust warnings. Astro check reports zero errors,
zero warnings and its existing async hint; static build passes. The native
adapter probe covers normal and post-fault report export plus rejecting an
in-flight report during firmware replacement, as well as prior lifecycle/input
checks. Independent source review found that session race; it is corrected.
Changed Python build-script lint/format and diff whitespace checks pass.
The default tested WASM copied through the repaired external-target script is
`5292e9b154c1b7adc53f9f12315047ddbfaff67dc91995c6d64fb733f21438a9`; public and static assets match. The desktop release was rebuilt after
the shared static assets. No owner process was stopped and no new actual GUI
or audio-device playback test was performed. The user subsequently requested
committing these source/docs changes; `git log` records that checkpoint.


### Guarded reset RAM-clear acceleration (2026-10-02)

**[O]** After committing retained diagnostics as `07b2f01`, the shared runtime
batches verified reset-clear loop iterations on both supported firmware images.
A full 54-byte routine signature is resolved (only start/end operands vary)
and checked against live RAM before each batch. DT2 clears
`[0x40312000,0x47e28470)` in 8,066,631 iterations; DN2 clears
`[0x402fc000,0x466b74d0)` in 6,536,013. Each iteration represents four
instructions and 16 zero bytes.

A batch requires the matching loop PC and pointer/count relation, zero
D4–D7, running supervisor CPU at IPL7 with tracing disabled, no pending panel
input, and inactive/pending-free Oracle timers. It stops at the chunk budget,
the current 1 MiB backing-page boundary and before the final iteration.
`Board::zero_mapped_sdram` rejects unmapped pages, cross-page spans, MMIO,
address overflow and guest-access capture; it never allocates RAM. Original
MOVEM steps still handle first-touch page allocation and Oracle/fallback
limits. The last loop iteration and setup/restore/return remain interpreted.
The CPU decode cache is invalidated for the filled range. Positive non-final
SUBQ/BNE effects preserve registers/CCR and all original logical guest ticks.

The separately reported `ram_clear_fast_forwarded_instructions` counter is
not interpreted work. DT2 skips 32,266,036 CPU steps; DN2 skips 26,143,244 in
the canonical 250k-chunk sequence. Logical counts, Oracle ticks and chunk
positions remain equal to reference. PC sampling records only actual CPU
steps, so these batches are absent from its histogram. Detailed bus-trace
builds disable the shortcut; `--features reference-ram-clear` provides a
portable reference build. No arithmetic execution policy is activated.

**[O]** Evidence: `/private/tmp/digi-ram-clear-g5rbt10g/`, curated into ignored
`out/native/ram-clear-20261002/`. Short native probes to the first chunk at
35M guest instructions, excluding firmware construction, took 1.717 ->
0.144 s (DT2) and 1.631 -> 0.398 s (DN2). Full canonical native results:

| Wall-time observation | DT2 reference | DT2 batched | DN2 reference | DN2 batched |
| --- | ---: | ---: | ---: | ---: |
| First visible frame | 9.02 s | 7.64 s | 9.52 s | 8.50 s |
| Ready | 27.82 s | 26.55 s | 22.65 s | 21.98 s |
| Complete boot/input replay | 29.89 s | 28.64 s | 24.37 s | 23.74 s |

These are one serial paired full replay per device, not timing distributions.
Each excludes firmware construction; subprocess receipts include construction
and export/exit and consequently differ slightly. The first visible frame
still occurs at 204,749,605 DT2 / 222,249,605 DN2 logical instructions:
this accelerates host execution without removing guest work.

Native reference/batched/default Node WASM match ready/final status, diagnostic
milestones/counters and all five frame captures on both devices. Comparisons
normalize only the new availability flag and add batched instructions back to
the interpreted count. Every other value matches, including all 87 intro
publication positions/pixel counts and the previous `07b2f01` native status.
Node WASM replays completed in 39.78 s DT2 / 33.47 s DN2; these are Node tests,
not real browser display measurements. Independent source review found no
issues. Default and PC/event tests exercise chunk/page boundaries, final CCR,
CPU/register/memory parity and rejection of altered code/state/active timers.


**[O]** Final verification: boot default 37 tests, PC/event build 39 tests and
all diagnostics 37 tests passed. The machine suite passed 91 tests with its
13 existing ignored fixtures. Rust formatting, warning checks and whitespace
checks pass. Default WASM/static build and a desktop release rebuilt after the
static assets pass. Public/static/tested WASM SHA-256 is `99aca7f2c2da64120b3558e56e6269bfef70f191823759431f939afa03495958`. An already
running worker/app keeps its old core; refresh/relaunch to use this build.
No owner process was stopped, actual GUI session or audio playback tested, or
additional commit made.

**[O]** A short, separate fresh-MAIN arithmetic canary prepares the next
optimization without enabling the prototype. Both firmware images yield the
first accepted add/mul/div calls before 72M guest instructions (53.55M DT2,
71.20M DN2). Their real isolated callees execute 24/241/106 CPU instructions
respectively. D0 matches the existing HLE result in all six calls. Full state
does not: add/div differ in D1/address scratch registers and CCR; multiply
differs in CCR. Add/div also change stack scratch bytes. Each observed call
writes zero to a global library word (`0x4030c418` DT2 / `0x402f6f20` DN2);
that word's value is unchanged in these particular preimages. This is not
proof of equivalent global effects for other inputs.

The temporary probe is `/private/tmp/digi-softfloat-canary-20261002`; source, pinned dependency lock, reports and
receipts are curated under `out/native/ram-clear-20261002/softfloat-canary/`.
It executes an isolated, IRQ-free raw callee from a real fresh reference
preimage, snapshots all touched already-mapped SDRAM pages (max eight), and
restores them plus the CPU before normal reference discovery continues.
Non-RAM instruction/data accesses, executed PCs outside MAIN and unbounded
returns fail the probe. Independent review identified the missing fetch guard;
it was added and both guarded reports reproduced identically. Timers are
not serviced inside the isolated canary: it establishes observed arithmetic
state differences, not scheduling or general ABI equivalence. Keep
`SoftfloatAbiV1` experimental; the next proof must cover full state/code
coverage and virtual time, rather than validating D0 alone. Parent completed
this probe directly after stopping the scoped preparatory worker.


### Outer timer-service gate (2026-10-02, after `d68306c`)

**[O]** The shared boot runner now checks the existing timer service boundary
before detaching/restoring its boxed timer facade. It still seeds the live SR
at every boundary. Only Oracle policy may skip; pending IRQs, invalidated or
NaN deadlines and undrained DTIM host writes keep the full path. The gate uses
the same floating-point comparison as `Time::service_with`; no new device
clock or instruction substitution is introduced. Independent review caught
the missing policy guard, which was added. The existing differential test now
exercises the outer gate across timer reconfiguration, refused IRQs and large
clock values; a new test verifies queued REF writes cannot be hidden.

**[O]** Complete native baseline/current and default Node WASM replays match
ready/final status, diagnostics, all five captures and all 87 intro publication
positions/pixel counts exactly on both devices. Guest counts are unchanged.
One paired native observation, excluding firmware construction:

| Wall time | DT2 baseline | DT2 gated | DN2 baseline | DN2 gated |
| --- | ---: | ---: | ---: | ---: |
| First frame | 7.56 s | 6.39 s | 8.52 s | 7.76 s |
| Ready | 26.31 s | 23.52 s | 21.78 s | 20.05 s |
| Full boot/input replay | 28.41 s | 25.46 s | 23.58 s | 21.62 s |

Short 100M probes took 3.074 -> 2.680 s DT2 and 3.227 -> 2.834 s DN2.
These are observations, not timing distributions; compilation could contend
with the beginning of the DT2 baseline. A harness initially compared native
and WASM intro log labels literally; corrected numeric comparisons passed
without repeating the already completed DT2 runs. The second receipts mark
those runs as reused; use per-run `timings.json`, not their near-zero reuse
process time, for native measurements.

Evidence is curated under ignored `out/native/service-gate-20261002/` from
`/private/tmp/digi-service-gate-yrm9gdpz/`. Boot default/PC-event tests pass
(37/39); machine tests pass (92, with 13 existing ignored fixtures). The
default public/static/tested WASM hash is
`ba037b2af3843a4745fbac3bec34e21a351616bd362ba77c493accde94929f8b`.
Web static build and a desktop release rebuilt after those assets pass. No
owner process was stopped or actual GUI session tested. This is the final
bounded boot optimization round before audio work.


### Fresh DSP traffic, native startup frontier and speaker proof (2026-10-02)

**[O]** A bounded temporary copy of the shared runtime records only direct
DSPI2/FlexBus writes during a fresh DT2 1.16 boot without a card. The default
frontend/runtime has no added tracing cost. Direct PUSHR writes span guest
counts 40,470,807..45,931,399: an initial `03`, 321,016 loader bytes, then
`0x18000000`. Those loader bytes match section 7 exactly, SHA-256
`0f514a12a2255f5c081e292c47f1f29462003177658da4bbae0a22fd737fffa2`.
There are 321,018 PUSHR writes total. The FIFO records 565,800 status writes
and 70,725 latch writes; falling-edge decoding yields 282,900 LP0 bytes,
69 complete 0x401-word transfers, all with header tag `0xffffffff`. This
no-card scenario sends slot headers and no sample pages. Zero **DMA frame
exchanges** therefore never meant zero DSP program/sample-port traffic.

**[O]** The existing matching AOT library lacks reset entry `0x1c1338` in its
instruction table. The program database omits its small loader block 89
(20 bytes), subsequent zero fill and other startup entry PCs. The generator
now accepts repeatable `--decode-range START:END` short-word ranges, using the
existing loaded-memory decoder and recording the ranges in provenance. Default
generation remains unchanged; no firmware bytes enter source control.

For startup diagnosis, generate **no AOT blocks**, with explicit startup/L2
coverage. This took 10.8 s to generate and 4.48 s to build, rather than the
36 s generation / 76 s build of a broad AOT probe. It has 108,074 decoded
entries. It is an interpreter diagnostic build, not an audio throughput
optimization or a replacement for the existing hot AOT library. Reproduce:

```sh
.venv/bin/python tools/sharc_rsgen.py dt2-1.16 \
  --decode-range 0x1c0000:0x1ce400 --decode-range 0xb80000:0xb8d5c0 \
  --out out/native/fresh-reset-20261002/interpreter-gen
SHARC_GEN_DIR="$PWD/out/native/fresh-reset-20261002/interpreter-gen" \
  CARGO_TARGET_DIR=/private/tmp/digi-reset-core \
  rustup run 1.98.1 cargo build --manifest-path native/sharc/Cargo.toml \
  --release --locked --offline --lib
.venv/bin/python tools/sharc_reset_check.py dt2-1.16 \
  --lib /private/tmp/digi-reset-core/release/libsharc_native.dylib --steps 100000
.venv/bin/python tools/sharc_reset_check.py dt2-1.16 \
  --lib /private/tmp/digi-reset-core/release/libsharc_native.dylib \
  --steps 7543 --compare --report /private/tmp/digi-reset-comparison.json
```

**[O]** Pure native execution from fresh `sharc_run.make_state` reference
reset defaults completes 7,543 instructions in 0.838 ms, then stops on an
unknown EQ predicate at `0x1c1447`. All 7,543 completed instructions match
Python canonical state in an instruction-by-instruction comparison (16.7 s).
No captured SHRD starting state, Python instruction fallback, provisional form
or approximate reciprocal mode is used. The tool bounds native work to 1M
steps and per-instruction comparisons to 10k, checks core/generator staleness
and rejects mismatched loader/library image hashes before execution.

The value comes from `DM(0x10000000)` at `0x1c1443`; its absence makes R0 and
then ASTATX.AZ unknown. This reproduces the documented boot-source frontier
in findings 06, rather than discovering an arithmetic mismatch. Section 7
also calls INIT at `0x120230` before booting the final FIRST entry `0x1c1338`.
The ADSP HWR (local p1799, p1821 and boot termination section) requires ROM
boot-config context for INIT and updates the application vector at handoff.
Flattening all loader records into a final memory image does not reproduce
that execution/context, and later writes overwrite most initcode bytes.
A subsequent primary-source check identifies `0x10000000..0x17ffffff` as
DMC0 normal-word space, mapping to byte-addressed DDR from `0x80000000`.
See [ADSP-2156x data addressing evidence](../refs/adsp-2156x-data-addressing.md).
The loader fills DDR at that byte address; the strict reference's generic
memory helper does not translate this normal-word alias. This supersedes
"no source establishes the address mapping," but does not qualify a full ROM
handoff. Fix addressing with explicit byte/normal-word access context and
correct DAG modifier units, including the native fast paths; a blanket alias
in the shared untyped memory helper could misroute byte/short accesses.
Independent review confirmed that risk. Internal stack ACONV transitions
also need the product map, rather than treating every conversion as a shift.
Do not synthesize a zero or infer physical startup state from reference seeds.

**[O]** Expanded interpreter-table rendering also matches all 100 existing
capture state hashes, with the same 21,948,991 instructions. This separate
captured-state regression is not fresh-boot audio proof. The fresh runtime
still has no SHARC/PCM or shared CPU/DSP clock integration.

**[O]** A three-second speaker check uses the existing verified capture pack
and hot AOT library, at gain 0.25. The first attempt exposed a CPAL 0.18.2
`Device::to_string()` panic when its Display formatter could not query the
macOS device name. `default_output` now uses the fallible description API
and a fallback label. Afterward, sandbox execution returns the actual
CoreAudio configuration error (OSStatus 560947818) instead of panicking.
The authorized test with normal OS audio access succeeds on MacBook Pro
Speakers, 48 kHz stereo: 4,561 clean blocks, zero underruns, zero DSP stops
and zero DMA failures. Median/p99/max render times are 443.2/520.1/908.6 us;
first render is 8,666.7 us, covered by prefill. This short buffered playback
checks DSP rendering plus the device sink, not worst-case real-time behavior,
a final DAC/master-FX mix or fresh frontend sound. Live-library tests pass
66 tests, with three existing opt-in fixtures ignored.

Evidence is curated under ignored `out/native/fresh-audio-20261002/` from
`/private/tmp/digi-fresh-audio-20261002/`, including loader/LP0 bytes, fresh
reset reports, frontier trace, capture parity and speaker metrics. The raw
full MMIO probe remains scratch-only; its bounded capture dropped no writes.
Temporary demand-decode experiments were replaced by the complete table and
full canonical comparison; the reusable probe has no such fallback.

**[D]** A separate bounded reference-only pre-INIT calibration materializes
loader records 0..6 before later records overwrite initcode. With the existing
explicit-memory policy, it stops after 40 instructions on DMC0 PHY lane
control. HWR reset-table seeds, an assumed top-of-ROM-stack frame and one
EMUCLK tick per instruction let it advance to 100k instructions, polling
CGU0_STAT at `0x120bc3..0x120bcf` after writing CGU0_PLLCTL=2 at `0x120bbf`.
That command requests bypass clear (HWR Table 2-16); a static reset register
map cannot perform the corresponding status transition. EMUCLK must count
core cycles (PRM Emulation Counter Register), whereas the current reference
leaves it static. This is calibration with explicit assumptions, not native
startup qualification or a verified ROM call context. HWR preboot already
enters Full-On clock mode; reset values alone are not the INIT handoff state.
Artifacts `init-calibration.py` and `init-calibration-frontier.json` identify
these remaining clock/ROM dependencies without changing production defaults.

## Runtime SHARC decoding and startup semantics follow-up (2026-10-02)

**[D]** The shared Python core and native memory boundaries now distinguish
normal-word transfers from byte/short transfers. Product-specific private L1,
public L1, L2, SPI and DDR normal-word windows use the public ADSP-2156x map
in [the addressing evidence](../refs/adsp-2156x-data-addressing.md).
DAG modifier units and ACONV use the same map. Host pokes/peeks remain byte
addressed. Synthetic tests cover alias coherence, window endpoints, absent
memory, modifier units and native instruction rollback; numeric addresses
alone no longer imply a normal-word transfer.

**[D]** `tools/sharc_decode_rsgen.py` generates an ISA-only Rust decoder from
our public-manual decode table. An engine can select runtime decoding with
C ABI option 4, rather than requiring a firmware-specific instruction table.
Instruction fields and cached metadata are owned. Cache hits revalidate the
loaded shortwords used for decoding and successor confidence, so executable
writes cannot silently keep stale metadata. Enabling runtime decoding clears
that engine's AOT dispatch/table-loop assumptions. Unmapped, truncated,
ambiguous and unsupported instructions fail closed. Generator version 2
invalidates libraries using the older instruction-field representation.

**[D]** Startup-driven shared semantics fixes include conditional SISD Type4a
transfers, coherent-memory L1 cache maintenance, modular low-word integer
MACs when upper MR words are unknown, Type8a loop-abort bookkeeping, PUSH
PCSTK and explicit guest PCSTK restore, normal PM reads from unified physical
memory, and PX transfers from the maintained PX1/PX2 halves. PCSTK preserves
its 26-bit word while return targets use its low 24 bits. Unknown reserved
stack entries and unsupported PCSTKP writes stop. This does not implement
complete stack/CEC behavior, PM LW or implicit PM SIMD reads, or 40-bit data
registers. Normal PM reads make the explicit result known; the unmodeled
implicit SIMD result remains unknown.

**[D]** A Digitone II startup stop at `0x1c0716` additionally exposed Type5b's
conditional SIMD register transfer using the old SISD-only EQ predicate.
Type5b now evaluates each processing element independently, including paired
sources and shared-source broadcasts. A destination with no complement uses
PEx only. Unknown relevant predicates stop before either write. Type5a's
conditional parallel-compute extension is not qualified by this change.
The following stop at `0x1c0720` exposed the same predicate problem in Type2a
compute-only instructions. These now select each PE from the pre-instruction
flags, preserving the registers and flags of an unselected PE. An unknown
relevant condition stops before either PE changes. Synthetic native/reference
checks cover all four independent EQ predicate combinations.

**[D]** The retained startup tool accepts runtime decoding, public HWR reset
rows, interval comparisons and bounded wall time. C ABI option 5 explicitly
provides one diagnostic EMUCLK tick per completed instruction; option 6
supplies a continuation base. Neither is a hardware cycle clock. Canonical
state does not contain instruction counters: a continuation host must supply
its clock origin deliberately. Reset-table parsing rejects conflicting rows
and models values only, not peripheral side effects. Reports explicitly keep
`loader_init_executed` and `qualified_fresh_boot` false: a flattened final
loader still omits ROM/INIT handoff. Approximate reciprocal seeds remain an
explicit diagnostic option.
Interval reports count completed instructions at agreeing comparison
boundaries, excluding a requested interval's unexecuted remainder after a halt.

A reproducible bounded invocation is:

```sh
.venv/bin/python tools/sharc_transpile.py --out out/native/runtime-core/gen
SHARC_GEN_DIR="$PWD/out/native/runtime-core/gen" \
  CARGO_TARGET_DIR=/private/tmp/digi-runtime-core \
  rustup run 1.98.1 cargo build --manifest-path native/sharc/Cargo.toml \
  --release --locked --offline --lib
.venv/bin/python tools/sharc_reset_check.py dn2-1.11 \
  --lib /private/tmp/digi-runtime-core/release/libsharc_native.dylib \
  --runtime-decode --mmr-resets out/refs/adsp-2156x-hwr/all.txt \
  --approx-recips --instruction-clock --steps 100000 \
  --compare --compare-every 1000 --seconds 60 --report /private/tmp/dsp-start.json
```

**[O]** An explicitly unqualified DT2 final-entry diagnostic ran through
256M instructions without a native halt. At that boundary IRPTL had software
interrupt 3 pending, TCOUNT remained static, and the core had no CEC delivery.
A new instruction budget completing is not a completed DSP boot. Interrupt
entry/RTI/CI, alternate register banks, clocks and coupled peripherals remain
material gaps; additional execution of a task loop does not prove progress.

**[D]** Independent static/MMIO audit associates DMA10 DSCPTR_NXT
`0x282620c8` with ring-A list head `0x2620c8`; the public HWR maps DMA10 to
SPORT4A. Captured CFG `0x44225` selects enabled descriptor-list memory-read
transfers, 4-byte peripheral/memory sizes and no DMA interrupt request.
SPORT4A CTL `0x021119f2` selects transmit/32-bit words but leaves both data
paths disabled. Its bit clock and frame sync are external; DIV `0x20` does
not establish a sample rate. Zero current descriptor/count registers provide
no transfer proof. This is a diagnostic setup observation, not PCM output or
physical connector qualification (HWR DMA/SPORT chapters, local lines
47322–47323, 50043–50302, 59200–59392).

**[O]** The first audio scenario is a freshly generated Digitone II oscillator
trigger. Existing DT2 captured-state players tap voice work buffers and cannot
qualify this scenario. DN2's common task/command/ring symbols resolve, but the
DT2 initializer and sample-voice harness do not. DN2's command-3 path calls
its renderer and converts the planar master buffer to the output ring. A
fresh command producer and valid initialized DSP/task state must precede host
playback integration; no fresh nonzero master PCM or frontend audio result is
claimed here.

## Guest-driven SSI command producer follow-up (2026-10-02)

**[D]** `Emulator::enable_ssi_diagnostic(request_hz)` opts into a bounded
SSI0 request source before guest execution. Default desktop/browser construction
keeps it disabled. The request clock uses the existing 132M Oracle scheduling
ticks per second and an explicitly supplied frequency; it does not establish
the physical clock. SSI0 channels 48/50 now transfer mapped guest RAM through
their live TCDs, advance minor/major loops, reload scatter/gather descriptors,
honor D_REQ and route byte SERQ/CERQ/CINT writes. Unsupported descriptor shapes
or unmapped payload/descriptor ranges are rejected before TCD/RAM mutation.
RX carries the existing documented diagnostic handover marker at major-loop
start. This marker is an explicit external-peer assumption, not recovered
SHARC audio.

**[D]** SSI work precedes timer delivery at an instruction boundary. The
guest's vector table, ICR levels, INTC mask and CPU IPL decide whether an owed
170/191 interrupt can enter. The lane offers one eligible SSI interrupt per
boundary; accepted entries are recorded in the existing diagnostic counters.
They are excluded from the runtime's unexpected-exception fault check. Idle
and RAM-clear acceleration respect active SSI deadlines. Tests cover invalid
SG/strides without mutation, D_REQ, NOP CINT, non-byte SERQ rejection and
SSI/timer ordering with retained timer delivery.

**[D]** A bounded native DN2 1.11 MAIN-only diagnostic, with fresh construction,
the empty synthetic +Drive and explicit 96k requests/s, reached six 2,748-byte
DSPI frame exchanges at logical instruction 435,499,044. SSI counters were
316,726 requests, 4,948 RX and TX major loops, and zero rejected requests.
The guest's handover counter reached 64; diagnostic IRQ counts were 1,570 for
vector 170 and 1,506 for vector 191. No vector, frame gate or countdown was
patched by the host. The six retained initial payloads were identical command
1 frames (SHA-256 `8d9342e6b2f7b8a3e5f04c3d7d4ecddafd901af8f5129accad3e258547cdeb33`).
The DSPI peer still replied with zeros; neither SHARC execution nor PCM was
connected. This proves guest command production under the diagnostic assumptions,
not an oscillator trigger or completed audio boot.

**[D]** Validation of this checkpoint: the full Python suite including slow
tests completed with 2,152 passes, 29 skips, one expected failure and 274
passing subtests. Its sole failure was the subset linter not declaring the
new pure normal-word stride helper; the declaration was corrected and the
subset/lint checks rerun. Native SHARC tests (30), peripheral tests (87),
SSI board tests (5) and boot runtime tests passed. The generic SHARC core
produced identical native/WASM canonical state after a 100k-instruction
diagnostic. With SSI disabled, fresh DN2 native/WASM boot and panel QA
produced identical status, diagnostics and all framebuffer captures.

**[O]** The unqualified DN2 flattened-entry diagnostic has different progress
from DT2: IRPTL remains zero at 250M and 300M instructions while it constructs
tables. At 325,496,893 it stops on conditional Type6a at `0x1c0e18`.
Saved canonical state permits bounded continuation without repeating startup.
An isolated SISD conditional-shift extension advances one instruction, then
stops at aligned Type1a `0x1c0e1b` with ALUOP `0x20`. The public PRM fixed
ALU table lists PASS as `0x21` and does not explain `0x20`; no alias is
assumed. Neither table progress nor this isolated extension proves DSP boot
completion or oscillator output.

**[C]** The apparent ALUOP `0x20` startup stop above was caused by an
earlier width error, not evidence that DN2 requires an undocumented ALU
operation. At `0x1c0e13`, the eight-instruction successor check retained
32-bit Type2b where the helper requires a 16-bit Type2c ADD. Its successor
loads the high half of a 64-bit random-generator increment. Correcting this
single width in an isolated native continuation executes only supported
operations and returns normally to `0x1c4bc8`. For seeds 0, 1, `0xffffffff`,
`0x123456789abcdef0` and `0xffffffffffffffff`, both stored state words equal
`(seed * 0x5851f42d4c957f2d + 0x14057b7ef767814f) mod 2^64`; the returned
value equals the high word masked to 31 bits. This is an independent
arithmetic check, not merely agreement between two decoders.

**[D]** Python and Rust width resolution now keep the existing eight-step
policy and retry only unresolved, confident Type2b/Type2c pairs through
at most 16 instructions. A common boundary reached by both confident
prefixes establishes a rejoin; uncertainty after that shared boundary does
not invalidate it. A public-format synthetic ADD/load/NOP fixture exercises
late rejoin followed by uncertainty. Generated libraries are version 3 and
the instruction database is version 15, invalidating prior decode artifacts.
The corrected native runtime automatically passes the same five arithmetic
checks without a PC-specific override. Conditional SISD Type6a support has
separate reference/native predicate and parallel-effect tests; its earlier
DN2 sighting was in the misaligned stream and does not qualify a real DN2
instruction boundary.

**[D]** Corrected-width bounded continuations pass the table constructor and
reach peripheral initialization. Two calls contain the same finite software
delay: a counter increments to `0xffffff` in a seven-instruction loop. A
private generated block matched interpreter canonical state after 10,007
instructions. It then executed the first wait's remaining 68,596,700 guest
instructions in about 2.1 seconds, and the second wait's remaining
107,441,345 in about 3.3 seconds. These are generated execution of the loop,
not counter patches or skipped guest instructions. The diagnostic instruction
clock was disabled only inside the inspected loop, which does not access it,
and its logical continuation base was restored afterward. These host timings
describe isolated DSP diagnostic runs, not desktop/browser boot or audio.

**[O]** After those waits, the legacy unified-register continuation reaches
SHARC instruction 561,611,502 with `MODE1=0x39010c80`,
`IRPTL=0x80000000`, and `IMASK=0x80408018`. Both alternate RF halves are
selected and SFT3I is pending, but MODE1.IRPTEN is clear, so the interrupt
is not yet eligible. The next architectural dependency is alternate-register
banks, followed by interrupt entry/return handling. The continuation already
ran instructions after changing bank selectors; it cannot be imported into a
bank-aware core as a qualified state. Restart initialization with the bank
model rather than retagging that capture. No fresh DN2 oscillator PCM or
native/browser sound has been demonstrated.

**[D]** The corrected-width checkpoint passed the full Python suite including
slow tests: 2,165 passed, 29 skipped, one expected failure and 274 passing
subtests. The native SHARC tests (31) and a 100k native/WASM canonical-state
comparison also passed; six existing SHARC golden outputs stayed unchanged.

## Opt-in SHARC register banks and interrupt-frontier probe (2026-10-02)

**[D]** `State.bank_model`, native option 19 and `sharc_reset_check.py --banks`
enable the primary/alternate register files. MODE1.SRRFL/SRRFH select both
PEx/PEy halves; SRD1L/H and SRD2L/H select the four I/M/L/B quarters.
Guest register transfers, system bit operations and existing PUSH/POP STS
MODE1 writes request a switch after the following completed instruction.
Separate requested/pending selectors preserve back-to-back writes. Native
traps roll back requests. Canonical state v2 preserves the inactive registers
and selector pipeline; v1 imports disable banking with unknown inactive
values. Native AOT blocks are disabled while banking is enabled until their
register-file path can preserve both banks. This is a correctness limitation,
not an audio-performance result. Diagnostic call/render clones copy inactive
registers, and state comparison checks them and the selector pipeline.

**[D]** Integration checks passed: 82 focused checks (four skips), 147
runner/render/golden/lint/type checks (five skips), and 36 native SHARC tests.
Instruction-level native/reference cases cover banked moves, back-to-back
selectors, MODE1 bit operations and both delayed-branch slots. Each selector
has independent isolation coverage. A bank-enabled 100k-instruction DN2
diagnostic produced identical native/WASM canonical states. Existing golden
outputs are unchanged. The full slow suite recorded above belongs to the
preceding decoder checkpoint; the bank follow-up has not yet run that suite.

**[D]** `sharc_reset_check.py --stop-software-interrupt` (native option 7)
stops before executing another instruction when a concrete software interrupt
is pending with global/per-source masks enabled and priority permits it. It
defers during branch delay slots and hardware loops. This observes an
interrupt candidate, not cycle-accurate eligibility or interrupt delivery;
core-control effect latencies and ISR entry/return are still unmodeled.
It is disabled by default and suppresses AOT dispatch when enabled so every
instruction boundary is inspected. It neither patches masks nor pushes an
interrupt frame. Coarse host sampling had repeatedly observed IRPTEN clear
inside critical sections; that sampling cannot exclude a brief enable window.
Use the opt-in boundary stop to locate the actual next dependency. No fresh
DN2 PCM or native/browser playback has been established.

**[O]** A restarted bank-aware DN2 diagnostic, with the boundary stop enabled
and the same memory policy across continuations, reached 589,544,942
instructions without finding an unmasked software-interrupt candidate.
The final observation was PC `0xb85ab8`, MODE1 `0x39010c80`, IRPTL
`0x80000000`, and IMASK `0x80408018`; global IRPTEN remained clear.
This run did not execute loader INIT and is not a qualified hardware boot.
Missing interrupt delivery alone has not been established as the reason
initialization remains in its task/event paths.

**[D]** An independent review against the public SHARC+ programming manual
confirmed that the core conflates followed-call bookkeeping with the
architectural PC stack. Type25a CJUMP is a JUMP plus compiler frame-register
transfers, but `forms_flow.py` marks it as a call and `sequencer.py` pushes
its return address into guest-visible PCSTK/PCSTKP. The corresponding
compiler JUMP-return also pops that stack. Neither operation is an
architectural CALL/RTS. Separating those stacks is required for correct
guest context restoration and interrupt work. This defect is confirmed;
its causal role in the observed initialization loop remains unproven.

## Physical task stacks, ISA vectors and software IRQ delivery (2026-10-02)

**[D]** Opt-in `State.stack_model`, native option 20 and
`sharc_reset_check.py --stacks` separate the architectural PC stack from
followed-CALL observations. CJUMP and compiler computed JUMP-return no longer
push/pop hardware return addresses. CALL reserves its physical entry at issue,
including before delayed slots; RTS consumes that entry at issue. Guest PCSTK
writes replace the occupied top, and PCSTKP truncation takes effect after the
following completed instruction. Growth and unknown targets fail closed.
MODE1STK reads/writes the actual top status-stack MODE1, including guest task
context restoration. Canonical v3 preserves independent physical entries and
the pointer pipeline; v1/v2 imports disable that model. Native AOT remains
disabled while bank or physical-stack models are enabled. Native option 8 is a
default-off PC boundary stop; it leaves guest bytes and state unchanged.

**[O]** Restarting DN2 1.11 with banks and physical stacks, documented reset
MMRs, approximate reciprocal mode and the diagnostic instruction clock reaches
the first task restore at instruction 561,596,597. Two finite software waits
were executed by the previously validated generated counter-loop block, with
10,007 agreeing interpreter/generated instructions checked at each wait. The
restore's PUSH STS / guest MODE1STK write / RTS / POP STS changes MODE1 to
`0x39011cf8`. After 723 more instructions, SFT3 is demonstrably eligible at
PC `0xb896f7`: IRPTL `0x80000000`, IMASK `0x80408018`, IMASKP zero.
This establishes the causal role of the earlier stack/MODE1STK defect; the
bank-only run's repeated IRPTEN-clear observations were not sufficient.

**[D]** Native option 9 and `--software-interrupts` opt into functional L1
software IRQ delivery. They require banks and physical stacks. Entry preserves
PC and status, applies MMASK, clears the accepted latch, sets IMASKP and settles
bank selection before the vector executes. RTI restores status/PC and clears
that interrupt's latch/priority bit; decoded guest IRPTL writes keep the current
active source clear. The supported IVT window uses fixed 48-bit ISA words and
normal-word PCs, then branches into the main VISA program with both delay slots
preserved. This is standard public ISA encoding, independently audited for the
DN2 SFT3 target; see the correction in findings 06. Generator version is 6.
Delivery is disabled by default. Active hardware loops and delayed transfers
are deferred; hardware IRQ generation, external IVTs, overflow traps, CI and
cycle-accurate control/pipeline timing remain outside this bounded model.

**[O]** A continuation from the eligible boundary now enters SFT3's
`0x9007c` vector and reaches VISA handler `0x1c0a70` after two ISA delay slots.
It initially failed 41 instructions later at `0xb8a947` reading the saved PC.
An independent manual/address audit confirmed another core defect: W2B was
multiplying an already-byte-space L1 stack pointer by four. PRM Table 6-4
requires identity in that case. Both engines now preserve known destination
address spaces; unknown spaces retain the legacy likely-shift/ILAD limitation.
The corrected continuation completes interrupt/task restoration and executes
another ten million instructions without a trap, ending at `0xb88aaf` with
MODE1 `0x39011cf8`, IRPTL/IMASKP zero. This repeated task/event path still needs
peripheral events and genuine ColdFire/DSP coupling; it produces no sound by
itself.

**[D]** Focused verification passed: 123 Python/native checks (five skips),
40 native SHARC unit tests, unchanged six SHARC goldens, strict translation of
all 300 core functions, and native/WASM builds. A 100k-instruction continuation
from the eligible SFT3 boundary also produced identical native/WASM canonical
states, including interrupt entry, task restoration and both register banks. Synthetic public fixtures
cover ISA vector delay slots, RTI status and bank restoration, active-latch
writes through decoded instructions, default-disabled delivery, rejected-entry
rollback, and committed entry followed by missing-vector fetch. The full slow
suite has not yet run for this follow-up.

**[O]** These diagnostics start at the flattened loader FIRST entry. They do
not execute loader INIT/ROM handoff, are explicitly unqualified as complete
hardware boot, and do not establish fresh DN2 oscillator PCM or native/browser
playback. Private canonical captures are troubleshooting boundaries, not
redistributable running-state fixtures. The genuine audio integration seam is
the shared board's DSPI2 peer, currently ZeroPeer, followed by SHARC-produced
master output into the existing native audio ring and a browser PCM consumer.


## Core timer and physical loop reservations (2026-10-02)

**[D]** Native option 21 and `sharc_reset_check.py --core-timer` enable a
functional core timer, using one clock per completed instruction. MODE2.TIMEN,
TCOUNT and TPERIOD drive countdown/reload; expiry latches both TMZHI (11) and
TMZLI (22). Guest count/period writes win over that instruction's clock.
Completed instructions remain committed if a subsequent timer event cannot be
modeled; timer changes are transactional separately. This is an instruction
clock, with no claim of cycle accuracy or architectural TIMEN latency. It is
inactive by default. Enabled timer IRQ delivery requires banks/physical stacks
and uses the same public ISA vector/RTI machinery as software IRQs.

**[D]** The physical stack model now implements inactive PUSH LOOP/POP LOOP
reservations separately from active decoded DO loops. It tracks six retained
(LADDR, CURLCNTR) slots and a depth, preserves popped contents when pushed again,
and returns all ones for empty register reads. Empty writes have no effect.
Reserved-slot CURLCNTR writes and the all-ones LADDR value are supported.
Packed active-loop restoration, PUSH within an active DO, mixed reserved/active
pops, and overflow interrupts stop explicitly. Decoded DO counters are paired
with physical resources, but packed DO LADDR characterization remains unknown.
Canonical v4 stores all six slots, including popped shadows; v1-v3 readers start
with empty resources. Native rejects an older physical-stack capture containing
active loops because it cannot reconstruct those resources. Native undo logs
preserve both depth and slot contents across failed instructions. Generator 8
translates 306 functions with zero unsupported translations. Independent review
against public PRM loop-stack rules found no issues in this bounded contract.

**[O]** Enabling the functional timer in the existing DN2 continuation expires
TCOUNT after one million instructions and enters the genuine enabled TMZLI
handler. The initial stop at `0xb8b1ff` was unsupported PUSH LOOP in a context
restore's delayed slot. With reservations implemented, the handler returns,
subsequent expiry is serviced, and 2,000,300 additional instructions produce
identical native/WASM canonical state (6,663,756 bytes). The continuation stops
31 instructions later at `0xb88aa1`, an unknown Type9b indirect target. This is a
bounded continuation from private guest state, not evidence of complete fresh
hardware boot. No command-3/note protocol, master PCM or frontend sound has been
established.

**[C]** `sharc_diff.import_state` now restores canonical memory ranges keyed by
address, as produced by `export_state`/`unpack_state`; it also accepts the older
explicit-address record form. A public byte round-trip checks this path. Raw
manual parsing of the DN2 continuation previously led to an incorrect claim of
zero memory ranges; its canonical state actually contains 264,256 ranges and
4,451,800 memory bytes.

**[D]** Checkpoint verification: 40 native unit tests, native/WASM release
builds and real continuation parity pass. Full slow-suite coverage completed
in resumed segments: 2,237 passed, 29 skipped, one expected failure and 274
passing subtests. The first segment passed 1,770 checks before interruption,
with one new lambda subset-lint violation. Replacing that initializer with
`State.__post_init__` corrected it; the remaining segment passed 467 checks
(one skip), including the failed lint and interrupted captured-frame replay.
Another 134 affected checks and the PyPy compatibility check pass against the
final initializer; six SHARC goldens remain unchanged.

**[C]** The captured-frame replay initially used a semantics-only diagnostic
library without firmware instruction metadata or enabled runtime decode. Every
instruction therefore fell back to Python, repeatedly exporting/hashing full
DSP state. A one-second process sample found that export path dominating the
replay. Regenerating matching DT2 instruction metadata (103,289 table entries;
zero AOT blocks required) let the remaining 468 checks finish in 150.56 seconds.
DN2 diagnostics explicitly enable runtime decode and still agree native/WASM.
Use matching firmware metadata for the legacy replay harness, or explicitly
select runtime decode; a matching core source hash alone is insufficient.


**[C]** The previous paragraph's diagnosis of the `0xb88aa1` stop was wrong.
DN2 1.11 `0xb88a99` is bytes `fe 4d 3f 0e`, VISA word `0x4dfe0e3f`: Type3b with
l=0, x=1, w=1, a plain normal-word `I12 = DM(M7,I6)` (only l=x=w=1 is `(lw)`,
PRM Type3b encode tables, pages 318-319). The pure-Python core loads it
correctly (I12 = `0xb88ab1`). That epilogue is also not on the failing path.
The 31 instructions after the 2,000,300-step state are a task context restore
(`0xb8b385..0xb8b39a`, then `0x1c0b37..0x1c0b5d`) that ends with `RTI (DB)` at
`0x1c0b5a` (Type11c x=1 j=1). The RTI pops MODE1 `0x39011cf8`, which selects
the secondary DAG and register-file banks. Its delay slots are
`I7 = PM(5,I12)` and `I12 = PM(1,I12)`. The model applied the bank switch
after one following instruction, so slot 2 read through the task's secondary
I12 (`0xb88ab1`) into code space. Python then loaded 0 and native loaded
Unknown, and the task's I12 was lost before its RETURN.

**[D]** Both RTI (DB) delay slots use the interrupt's banks; the restored
selection is first active at the return target. The PRM does not say when a
delayed RTI's status pop takes effect: page 57 and pages 213-214 give only a
flat one-cycle MODE1 bank latency, and nothing covers delay slots. The firmware
decides it. In the context block at `0x26f770`, word 1 is `0x26f770` (a self
pointer) and word 5 is `0x26f7a8`. The slots reload the kernel's own primary
I12 and I7 from the block, and the task's secondary I7/I12/I6/B7
(`0x269458`/`0xb88ab1`/`0x269470`/`0x269170`) stay intact. DT2 1.16 has the
same sequence (RTI (DB) at `0x1c0c08`). The model marks the popped selection
with a transient hold bit in `bank_requested_mask` (`_bank_hold_request`,
`St::bank_complete`), so the canonical format stays v4. A MODE1 write in such a
delay slot fails closed. Explicit MODE1 writes, BIT SET/CLR, POP STS and
interrupt entry keep their latency. Generator version 9.

**[O]** A non-delayed RTI still runs the first target instruction in the old
bank (the one-following-instruction rule). The older PGR says an ISR return's
status pop "results in a one cycle stall" (Table A-11 notes), which suggests
that the target sees the new bank. No firmware case has tested it yet.

**[D]** With the fix, the DN2 continuation from the 2,000,300-step state
returns to `0xb88ab2` and runs to a 10,000,000-instruction cap without a halt
(1.5 s native). MODE1 alternates between task (`0x39011cf8`) and timer-handler
(`0x39010800`) states, and IRPTL bit 11 stays latched and masked. Native and WASM
canonical states at 2,000,300 steps from the IRQ-continued state are identical
(6,663,756 bytes). Strict generation is 306/0, with 40 native unit tests and 75 focused
Python tests. This shows no halt, not forward progress. It is still a private
diagnostic continuation, not a fresh boot, and there is no PCM.

## Opt-in SEC, descriptor DMA and SPI2 frames (2026-10-02)

**[D]** The DN2 continuation now waits in the RTOS idle task. FUN_b88aa6
spins while the priority-0 ready-list count at DM `0x2d0608` is at most 1, and
it touches no MMR. Only an interrupt that readies a task can move it on.

**[D]** DN2 SECI path. Vector `0x9003c` enters `FUN_1c0a6f` at `0x1c0acd`.
- It reads the SID from SHDBG_SECI_ID `0x300eb` at `0x1c0ae1`, acknowledges by
  writing it back at `0x1c0ae4`, and looks up the halfword selector at
  `0x240910+2*SID`, then `{callback, arg}` at `0x240aa0+8*sel`.
- It enters the callback with `RTS (DB)` at `0x1c0b2e`, with R4=SID, R8=sel and
  R12=arg. SEC_END is written at `0x1c0b34`.
- The earlier "JUMP (CI) at `0x1c0af6`" was an unaligned decode artefact; the
  aligned stream has `R12 = 0x240910` at `0x1c0af4`.
- Generic wrapper `0x1c02a4` jumps through `0x2404b0+4*sel`.
- SPI2 handlers (arg = device struct `0x268d30`) are SID69 `0x1cc2eb`, SID70
  `0x1cc3f5`, SID71 `0x1cc5ca` and SID72 `0x1cc520`. Their DMA completion
  helper `0x1cc1d6` W1C-acknowledges DMA_STAT, advances the descriptor and
  calls `0x1ca04a`, which toggles the ping-pong bit at DM `0x2c0450`.
- SPI2 completions ready no task.

**[D]** The Audio Task (entry `0x1c9fe7`) is woken only by event `0x100`.
- The event reaches the task through this chain: SID191
  DAI1_GBL_SPORT_INT0, handler `0x1cd73c` (arg `0x2690e0`), callback
  `0x1ca136`, then task_notify `0xb88e41`.
- Each block is paced by SPORT4A/B (DMA10/11): 2-descriptor rings of 0x40
  words, clocked by PCG C through DAI1.
- In this state DAI1_GBL_SP_EN bit 0 and SPORT4 SPEN are clear. No static
  writer of them was found.
- The block handler `0x1c9d6b` reads its command from the SPI2 RX buffer.
  `0x1ca020` returns the ping-pong buffer pointers; the command dispatch is
  the indirect jump at `0x1c9dbf` through `0x268a68`.

**[D]** `State.peripheral_model`, native option 22, models two blocks (sources
in tools/sharc_core/periph.py, twin native/sharc/src/rt/periph.rs):
- The SEC core interface: SCTL.SEN/IEN, SSTAT PND/ACT, CSID/CSTAT.SIDV,
  acknowledge by a SHDBG_SECI_ID or CSID write, END and RAISE.
- Descriptor-list DMA for DMA26/27/10/11: the fetch order is NXT, ADDRSTART,
  CFG, XCNT, XMOD (NDSIZE+1 words). An X-count completion sets IRQDONE and
  raises the SEC source. DMA_STAT is W1C, and CFG.EN 0->1 resets RUN.

All state lives in `State.mmrs`, so the canonical format stays v4. SECI is a
level request from issue to acknowledge. It latches IRPTL bit 15 at an
instruction boundary unless SECI is active (the PRM rule that SECI is not
stored during an SEC ISR), so a request issued inside the ISR is taken after
its RTI.

Not modeled:
- the SEC preemption stack and CPMSK/CGMSK;
- re-assertion of a level source after END;
- DMA timing, autobuffer, array and 2D flows.

Unsupported cases raise or trap.

**[C]** PM-bus scalar and long-word data loads and stores now use the unified
memory, as normal-word UREG loads already did ("single, unified address
space"). Previously PM loads were Unknown and PM stores wrote nothing. The DN2
dispatcher's `R8 = PM(I12+0) (sw)` depends on this. The SIMD PEy transfer over
PM remains an explicit stub.

**[D]** Feeding ten genuine DT2 1.16 ColdFire frames
(`dt2-1.16-idle-fulltx.dt2cap`) to the idle DN2 state:
- Each frame takes two SECI entries (SID69, then SID70 after END), runs the
  genuine handlers, flips the ping-pong bit and leaves the SEC state clean.
- There are no halts and the replies are all zero.
- Python, native and WASM agree on frames 0-7 (ping-pong, SEC state, end PC).
  Native and WASM agree on all ten. Frames 8-9 end at different idle-loop PCs
  in Python, whose run options differed (a periodic path at `0xb8afe4` ran in
  Python frame 8) **[O]**.
- Native takes 0.18 s for the ten frames and WASM 0.30 s.
- These are DT2 frames driving DN2 firmware, so the protocol meaning is
  unverified. No task woke and no DAI1 or SPORT4 enable was written. DN2
  frames are needed next.

## DAI1 group SPORT interrupt and the SPORT4 audio block (2026-10-02)

**[D]** Public-manual semantics (ADSP-2156x HWR "Grouping of SPORTs" ch. 23,
DAI_GBL_INT_EN and DAI_GBL_SP_EN ch. 22):
- SPORT4A/B are the DAI1 "SP0A/SP0B". DAI1_GBL_INT_EN `0x310CA2EC` bit 16+n
  is GRPn_INT_EN and bit 8n+x selects member x. DN2 idles at `0x10003`: group
  0, both halves.
- The group source is the AND of its members' DMA interrupts (each channel
  needs DMA_CFG.INT set). There is no status register of its own: it is
  cleared by W1C of the members' DMA_STAT.IRQDONE. SEC SID 191 is
  DAI1_GBL_SPORT_INT0 (sensitivity "None" in the table).
- DAI1_GBL_SP_EN bit 0 is GBL_SP_EN; the HWR says it is reserved for DAI1,
  so the real start bit may be GBL_SPEN_DAIX (bit 1, already set at idle)
  **[O]**. The model follows the lead's reading: bit 0 plus both SPEN.

**[D]** The handler `0x1cd73c` touches one MMR kind: a W1C write of 1 to each
member channel's DMA_STAT (`+0x30`). The branch at `0x1cd88f` writes R13 to a
channel's DMA_CFG; it is not on the callback path **[O]**.

**[D]** Model (periph.py `_dma_irq`, `_sport_running`; host
`sharc_periph_host.sport_block`; native `Engine::sport_block`,
`sharc_native_sport_block`, `NativeCore.sport_block`): while the SPORTs run,
a block returns the DMA10 work unit bytes, writes the input (default zeros)
into the DMA11 unit, completes both, and the second completion raises SID 191
(the channel SIDs 53 and 55 stay quiet). Not running returns None.

**[D]** Diagnostic, not genuine audio: from the idle DN2 state with the
enables poked, one block takes SID 191: `0x1cd73c` ran 223 instructions after
the block, then `0x1ca136` and task_notify `0xb88e41`. The scheduler then
halts at `0xb8ac2e` (`LADDR` restore, "guest packed loop restoration is not
modeled") before the Audio Task entry runs. The output block is all zero.

**[C]** SPORT4 runs once DAI1_GBL_SP_EN holds GBL_SPEN_DAIX and both
primary selects (`0x52` within the DN2 value `0x5e`, written by group-open
`0x1cd1c3` at `0x1cd2e4` during boot init) and both DMA channels are enabled.
The firmware never sets SPORT CTL.SPEN, and GBL_SP_EN bit 0 is the cross-DAI
strobe. The earlier gate (bit 0 plus SPEN) treated genuinely running audio as
stopped.

### Packed loop restore and DO above PUSH LOOP slots (2026-10-02)

**[C]** The halt at `0xb8ac2e` was not an active loop. The frame restored
there (DN2 `0x2d1604`: loop count 1, CURLCNTR `0xffffffff`, LADDR `0`, one
PC entry `0x011c0e68`) was written by an earlier model that did not
synchronize LADDR for a pushed empty slot; every capture after
`digi-audio-dn2-irq-progress.bin` carries it. The current model saves LADDR
`0xffffffff` there (re-run from `digi-audio-dn2-task-restored.bin`), and that
restore already worked. LADDR `0` is code 0 (EQ), type 0, end 0: an
arithmetic loop that the model does not execute, so the stale frame still
stops, now with "termination code".

**[D]** Kernel order (DN2 1.11): the save at `0xb8abd2` pops every loop
entry (LADDR then CURLCNTR, `lpo`), stores LCNTR, then the PC stack. The
restore at `0xb8ac1e` / `0xb8a912` / `0xb8b1f8` pushes `lpu`, loads CURLCNTR,
then LADDR for each entry (deepest first), then LCNTR, then all PC entries
(`ppu`, PCSTK). All loops are restored before any PC entry, unlike the
interleaved example in SHARC+ PRM 4-46/4-47 (all.txt 7117-7186).

**[D]** Model (state.py `_restore_packed_loop`, `_loop_reserved_above`,
`_packed_counter_laddr`; sequencer.py `_advance`, `_start_counted_loop`):
LADDR is 24-bit end address, 5-bit termination code, 3-bit type (SHARC+ PRM
28-40, all.txt 31264; field positions from the public classic manual Table A-4,
`adsp-2136x...rev2.4` all.txt 24869-24885, and code LCE = `01111`, Table 10-4,
20209). A LADDR write that follows PUSH LOOP and a known CURLCNTR makes an
active `Loop` only for LCE (counter) words, directly above active loops,
with CURLCNTR not 0 or all ones. The start is not in LADDR: the Loop gets
the sentinel `0xFFFFFFFF` and the first loop end reads the PC-stack top
(PRM 4-33, all.txt 6438-6441: refetch "from the top-of-loop address stored on
the top of PC stack") and binds it; an ISINT entry or an empty stack stops.
The type bits are kept as written, never interpreted. A DO the model starts
reports a concrete LADDR (`0xE0000000 | 0x0F<<24 | end`, the classic
"counter, length > 3" code; F1/E2 mode and exact length are not modeled
**[O]**) instead of Unknown, so a save round-trips. PUSH LOOP above an
unbound restored loop is allowed (nested restore); above a bound loop it
still stops. Stops kept: non-LCE codes, unknown LADDR, bad counter, restore
over reserved slots, writes to an executing loop's registers, DO or POP
over a half-restored stack. Canonical format v4 and generator 8 are
unchanged (the sentinel is an ordinary u32 start).

**[D]** DO above reserved slots: PUSH LOOP slots lie below DO loops, so the
slot index and CURLCNTR/STKYX stay those of the top slot; the old stop "DO with
reserved loop resources" is gone and loop exit no longer forces CURLCNTR to
all ones. This is what stopped the fresh run next (`0x1c9e31`).

**[D]** Diagnostic (private, not a boot): fresh state = `task-restored` +
22M instructions with the current model, timer on after 10M. 20 SPORT4
blocks at 700k instructions: no halt in 14M instructions; SID 191
handler `0x1cd73c`, Audio Task `0x1c9fe7` and block handler `0x1c9d6b` each
ran 20 times. Output blocks (256 B) are all zero with zero input: no
evidence of synthesis, no note/command-3 protocol yet **[O]**.

## Genuine DN2 ColdFire frames and the first coupled run (2026-10-02)

**[D]** The native ColdFire boot sends no DSPI2 frames unless SSI0 is paced.
`Emulator::enable_ssi_diagnostic(hz)` is off by default. Without it eDMA50
never completes, so vector 170, the forced vector 191 and the DSPI2 driver
never run. The UI still reaches the main page without them.

**[D]** DN2 1.11 runs with `native/boot/examples/dspi2_capture.rs` (opt-in
`Emulator::record_dspi2`, writing .dt2cap files with zero replies) and
`SSI_HZ=96000` (`ssi::AUDIO_SSI0_REQUEST_HZ`). Results:
- Ready at 851,236,347 instructions (847,486,347 without SSI pacing).
- 5,415 frames of 2748 bytes from instruction 434,996,163, exactly 88,000
  ColdFire instructions apart: 1,500 Hz at 132 MHz, one frame per 32-frame
  48 kHz audio block.
- First-word histogram: 0x0001 ×4,604, all byte-identical, then 0x0003 ×811
  (251 distinct).
- 0x0003 frames vary in only 68 bytes: a 32-byte area at 116-147, and byte
  pairs 4 bytes apart in a repeating 146-byte record from byte 334.
- These are frames from a ColdFire that saw zero DSP replies; a real device
  may send a different sequence.

**[O]** Fed one frame per ~667k DSP instructions into a fresh DN2 continuation
(peripheral model on, assumed 1 GHz DSP clock):
- 5,415 frames ran in 3.6G native instructions (7.6M instr/s) with no halt.
- The firmware never set DAI1_GBL_SP_EN bit 0 or SPORT4 SPEN, so no audio block
  ran.
- With those enables forced (diagnostic only), 585 blocks over frames 0-584
  (all 0x0001) ran the Audio Task with all-zero output.
- Neither a genuine audio start nor non-zero PCM has been established. The
  enable path probably needs DSP replies (the ColdFire may wait on a reply
  protocol) or commands this capture does not contain.

### Type15 DAG-register stores in SIMD mode write both words (2026-10-02)

**[C]** The Type15b/15a rule "uncomplementary UREGs behave as in SISD mode"
(PRM p.16-8, TCOUNT example) is wrong for DAG registers (I/M/L/B). DN2 1.11
`FUN_1c1f1e` (sw `0x1c1f4b`-`0x1c1f96`) clears each 64-word voice row with
16 x `DM(I4 - 32) = M12` (Type15b) and `DM(I4, M4) = M12` (M4 = 2) in
`MODE1.PEYEN`; only a two-word store per displaced access covers the row. With
one word the odd lane kept its old value, the voice mix fed it back (gain about
-2.4 per 32-sample block from DM `0x25ba68`), and the master buffer at
`0x268438` overflowed to +-Inf and NaN by block ~4994 of the DN2 audio run.
`_type_nw_companion` now replicates DAG-register stores; TCOUNT-like UREGs and
all loads keep one word. **[D]** The PRM text does not say this; the evidence is
the compiler-shaped clear loop. After the fix the same run gives no NaN
(blocks 4896-4899 are a short finite transient, then 0). Test:
`tests/test_sharc_trace_simd.py::test_type15_dag_register_store_writes_both_words_in_simd`.
The earlier "real cmd3 output from frame 4896" in that run was this blow-up.

## DN2 command-3 synthesis path (2026-10-02)

**[C]** The DN2 1.11 "`R2 = fpack(R3, R14)`" at `0x1c4f4a` (in `FUN_1c4f04`,
a wavetable lookup reached from command 3) was a width-resolution error, not a
missing FPACK.
- The parcels from `0x1c4f46` tile as `M4 = 0x114`, `0x1c4f48` 3b
  `R0 = DM(I3,M4)`, `0x1c4f4a` 2c `R2 = add(R2,R9)`, then shifts, adds, a
  clamp and `float`. This is a 24-bit sample combine that ends exactly on the
  15b at `0x1c4f52`.
- The 2b/2c resolver rejected 2c because its raw successor chain lost phase.
  It now takes 2c when the 2b chain is unclean and the 2c chain is clean once
  each successor is itself width-resolved (one level).
- The Python decoder and the Rust runtime decoder make the same choice.
- Changed sites: DN2 1.11 `0x1c4f48`/`0x1c4f4a`, and DT2 1.16 `0x1c32ab`, where
  a garbage decode becomes clean code. That accounts for the `cov_all` and
  `cov_render` golden updates; the run and trace goldens are unchanged.
- sharcdb DB_VERSION is 16. FPACK/FUNPACK remain unimplemented; their only DN2
  sites are in a data region.

**[D]** Type6b (shift immediate) now executes a conditional SISD predicate as
Type6a does. DN2 `0xb8b03d` `IF NOT TF R0 = bset(R0, bit=16)` needed it.
Unknown predicates and SIMD mode still stop.

**[D]** Command 3 (block handler `0x1c9d6b` → `0x1c9f0f`) copies RX words
through `0x1c9d00` into `0x268538..0x268938` and calls FM `0x1c2712` with
master L/R `0x268438`/`0x2684b8`. `0x1c9d3f` converts them to Q31 in the
SPORT4A buffer as interleaved L/R (even/odd words), 32 samples per block.
Observed sign: Q31 = −master, clipped. Commands 0 and 1 write silence;
command 2 is a copy/loopback.

## Live ColdFire/SHARC+ coupling (2026-10-02)

**[D]** `native/boot` feature `sharc` (off by default; no effect on the
default or WASM build) adds `sharc_peer::SharcPeer`, a `periph::dspi::Peer`
that owns the native SHARC engine. Per 2748-byte DSPI2 frame: SPI2 exchange
(reply returned to the ColdFire), `step(666_667)` in 1024-instruction chunks
(noting the first chunk that ends in the DN2 idle range `0xb88a49..0xb88abb`),
then one SPORT4 block (-Q31 to f32 stereo). Driver:
`native/boot/examples/sharc_live.rs` (needs `SHARC_GEN_DIR`; image blob from
`tools/sharc_pack_image.py`; DSP state is a private canonical blob).

**[D]** DN2 1.11, SSI paced, NOTE_EVENTS=trig, ready+250M (1.1015G ColdFire
instructions, 7,574 frames, 5.05 s audio, 5.05G DSP instructions): no halt,
wall 732 s (145x slower than real time; DSP about 6.9M instr/s, native). The
audio is bit-identical between two runs and matches the zero-reply replay
(peak 0.693, same pin and decay figures).
- The TX stream is byte-identical to the zero-reply capture for all 7,560
  frames both runs cover: the ColdFire does not react to the DSP replies yet.
- Replies are non-zero (7,571 of 7,574) but tiny: 1 to 5 non-zero bytes per
  frame, 2,143 distinct.
- DSP busy before idle: command 1 about 3.4k instructions (max 5k); command 3
  about 335k (50% of a 1 GHz period; assumed clock). Overall idle share 80%.
- The DSP state is a mid-run continuation attached from the first ColdFire
  frame (435M), not from the DSP's own boot **[O]**.

### ColdFire ready snapshot (native)

**[V]** `Emulator::save_state` / `load_state` (`native/boot/src/runtime.rs`,
format `DT2SNP01`) serialize the mutable ColdFire machine at a `step_chunk`
boundary: CPU, board RAM (zero 4 KiB blocks elided, 62 MB for DN2 at ready),
DMA/DSPI2/SSI link, eSDHC and card overlay, PIT/DTIM/INTC, SD gate, input
queue, frame trackers and readiness counters. Construction facts (firmware
signatures, panel profile, card backing, peers) come from building the same
`Emulator` again; the load needs a fresh emulator for the same firmware with
the same SSI diagnostic enabled. Telemetry marks, bus trace vectors and the
DSPI capture recorder are not state.
- Determinism, DN2 1.11, SSI 96 kHz, ready at 851,236,347 + 20M: boot-straight
  (no save), boot-and-save-then-continue, and restore-in-a-fresh-process give
  the same state digest (SHA-256 of the serialized state) and the same
  DSPI2 capture hash; 33 s boot vs 1.9 s restore plus run.
- `CF_SNAPSHOT=path` in `examples/dspi2_capture.rs` restores the file if it
  exists, else boots and saves at ready, before the QA inputs.
- Coupled snapshot **[V]** (`examples/sharc_live.rs`): the DSP is attached from
  the first frame, and at ready the ColdFire state goes to `path` and the
  SHARC engine's canonical state (`Engine::export`, memory as explicit ranges,
  6.5 MB) to `path.dsp`, with the instruction-clock tick and the DSP
  instruction counter. A restore imports it, continues the clock from the
  saved tick, and attaches at once, so the DSP has seen the boot-time
  command-1 frames. (The canonical export without ranges drops memory; a
  restore from it halts at pc 0. `DSP_ATTACH=ready` keeps the old uncoupled
  mode.) Determinism, DN2 1.11, ready+50M: boot-and-save and restore in a fresh
  process give the same ColdFire state digest, the same SHA-256 of the DSP
  export and the same SHA-256 of the PCM since ready (`CF_DIGEST=1`).
- `sharc_live` from the coupled snapshot (NOTE_EVENTS=trig, ready+250M, idle
  skip on): 162 s wall, 85.6x slower than real time on the 1.90 s of audio
  after ready (the straight run from boot took 732 s without the skip). The
  PCM words after ready are identical to the straight coupled run, word for
  word (181,952 words). The earlier uncoupled attach at ready gave different
  samples **[C]**.

**[D]** Idle-loop skip (native engine options 23 head PC / 24 and 25 inclusive
PC range, -1 turns off; `SharcPeer` enables it for DN2 with head `0xb88aab`,
range `0xb88a49..0xb88abb`, `DSP_IDLE_SKIP=0` disables). Exact by
construction, per `step()` call: at the head the engine snapshots registers,
banks, loop/call/PC/status stacks, specials and pending state, runs one
iteration through the normal interpreter, and returns to the head. If the
state is unchanged (apart from the instruction count, EMUCLK, TCOUNT and
the `steps` counter, which is advanced by its own per-iteration delta), no
MMR was written, and every memory byte written has its old value back, the
iteration is a fixed point, so M whole iterations are replayed by adding
`icount`, EMUCLK and `steps`. M is limited by the step budget and by the
core timer: only ticks before the next TCOUNT==0 are skipped, so the latch
and interrupt entry happen at the same instruction as before. Any host event
(SPI2, SPORT, pokes) lands between `step()` calls, so each call re-verifies.
Not covered: side-effecting MMR *reads* in the loop (the PC range plus the
absence of EMUCLK/TCOUNT use in `0xb88a49..0xb88abb` stands in for that
check). The Python Runner keeps stepping every instruction: this is a host
scheduling change with identical results, and the native and Python engines
are already compared on canonical state.
- Note capture replay (frames 4600..6400, 1.2G DSP instructions): export_state
  hashes at frames 4700, 5000, 5399, 5400, 5900, 6399, 6400 and the PCM hash are
  identical with and without the skip. 599M of 1,201M instructions (50%, the
  note section is busy) were replayed. Frames 5400..6400: 101 s -> 56.7 s.
- Unit test `idle_skip_matches_plain_stepping_across_timer_events` compares
  all registers, `steps`, icount and EMUCLK after odd step sizes across timer
  periods 0, 1, 2, 7, 100 and 1000.


## AOT blocks with the models on (2026-10-03)

**[C]** The earlier statements that native AOT blocks stay disabled while the
bank, physical-stack, core-timer, software-IRQ or peripheral models are on
(sections "Opt-in SHARC register banks", "Physical task stacks", "Core timer
and physical loop reservations") no longer hold: generated blocks now run
with all of them on, behind a generator classification and a run-time gate.
Block code is the partial evaluation of the same core, and the core reads the
model switches (`s.cfg.stack_model`, ...) at run time, so a block already
does what the interpreter does for an instruction. What it does not do is
what the *engine* does between instructions, and that is the whole list of
blockers, per model:

- **Banks**: `St::bank_complete` swaps the visible and alternate registers
  one instruction after a MODE1 write. Blocks never call it. Gate: no bank
  selection pending or requested at entry; classification: no instruction
  that names MODE1 (or MODE1STK) in a register field, so none can request.
- **Physical stacks**: `pc_stack_complete` truncates the PC stack after a
  PCSTKP write; CALL/RTS/RTI/CJUMP/RFRAME and PUSH/POP act on the physical
  entries. Gate: none pending or requested. Classification: no PCSTK/PCSTKP/
  LADDR/CURLCNTR/MODE1STK field, no call, return (RTS, RTI, CJUMP, RFRAME,
  the `(DB)` return idiom), loop-abort or CI jump, no system form (PUSH/POP,
  bit operations on system registers, IDLE, ...). DO loops and the
  instruction at a loop's last address stay in: block code runs the core's
  own loop logic (including the `loop_slots` and PC-stack updates), and the
  block now saves `loop_depth` and puts it back if that instruction traps,
  the one scalar the undo log lacks (`rollback_log` kept the changed value).
- **Software IRQs**: the engine checks the latches at every boundary. A
  block is entered only when no source it delivers (software 28..31, timer
  11/22, SECI 15) is latched, enabled and not priority-blocked, *ignoring*
  the delay-slot and active-loop deferral (`latent_interrupt`): that
  deferral can end at a boundary inside the block. Classification excludes
  writes of IRPTL, IMASK, IMASKP, MMASK, MODE1 and RTI, so nothing inside can
  make a source deliverable. SEC SECI latching (`sec_line`) is idempotent
  within a block because SEC state changes only through stores (below).
- **Core timer**: one tick per completed instruction. The block's `s.limit` is
  `icount + TCOUNT - 1`, so it cannot reach expiry (TCOUNT < 2 or a reload at
  0 is left to the interpreter); afterwards TCOUNT is reduced by the
  instructions run. Classification excludes TCOUNT/TPERIOD/MODE2 fields.
- **Instruction clock**: EMUCLK is set at every instruction start. After a
  block the engine sets it as the last instruction saw it (`base + icount -
  1`), and before the interpreter's next instruction as usual; blocks that
  name EMUCLK/EMUCLK2 are excluded. This found a real bug in the first
  version: after a budget exit the interpreter instruction ran with a stale
  clock and without the boundary's interrupt checks; the step loop now goes
  back to its top whenever a block completed instructions.
- **Peripheral model**: SEC/DMA state changes only through stores. A block
  store that `periph_write` would act on (`rt::periph::write_acts`: SECI_ID,
  CSID, END, RAISE, SCTL status, DMA STAT/CFG) traps out of the block with
  `TRAP_BLOCK_MODEL` before any change and the interpreter re-runs it. Reads
  are pure and stay in the block.
- **Idle skip**: no block runs while a probed iteration is open or while the
  PC is inside the idle range (the skip needs the interpreter's write log);
  blocks run up to and after it.
- **Runtime decode (option 4)**: it used to clear the dispatch table. Blocks
  now survive it but run only while the loaded code is what they were
  generated from: `image.rs` carries `CODE_RANGES` (short-word ranges, with
  six look-ahead words, and the SHA-256 of their loaded bytes). `Mem` watches
  the pages of those ranges (`code_gen` counts stores), and the engine
  re-hashes after a store to one, so a library made for another image (DT2
  blocks on DN2 memory) or a patched code word never runs. Loops started by
  runtime-decoded code are re-checked against `LOOP_ENDS` when the loop stack
  changes.
- **Memory model**: block code folds `explicit_memory_model`; the DN2
  diagnostics run with it off, so `rsgen --explicit-memory-model 0` generates
  for that and `Cfg::refresh` compares with `GEN_EXPLICIT_MEMORY_MODEL`.

Tools: `sharc_rsgen.py` classifies (`model_unsafe_reason`) and `--model-safe`
cuts each block before its first unsafe instruction (the interpreter runs
that one and a block resumes at the next entry; `--entries` supplies those
entries); without the flag each entry is flagged in `MODEL_SAFE` and regions
never mix safe and unsafe bodies. `sharc_dn2_replay.py` is the replay runner
(state hashes, PCM hash, wall time, block and idle counts, the profile for
`--coverage/--entries/--transitions`: native option 26,
`sharc_native_profile`). Generator version stays 9: the translator's output
is unchanged, and libraries without the new statics do not build.

**[D]** DN2 1.11 note capture, `digi-audio-loop2-fresh.bin` state, frames
4600..5600 (667.7M DSP instructions, 333.8M of them replayed by the idle
skip), idle skip and all models on, native, one machine:
- Export-state SHA-256 at frames 4700, 5000, 5399, 5400, 5500, 5599, 5600 and
  the PCM SHA-256 are identical in five runs: blocks off/on with the skip,
  blocks off/on without it, and a repeat. The first four hashes also equal the
  earlier idle-skip run's.
- Frames 5400..5600 (133.4M instructions, 67.0M not skipped): 11.06 s without
  blocks (6.06M non-idle instr/s), 1.08-1.25 s with (54-62M instr/s), 8.9x
  to 10.3x; whole range 56.7 s -> 6.8-7.5 s. Without the skip, 100.3 s ->
  20.4 s (4.9x; the idle loop is interpreted). 93.6% of the non-skipped
  instructions run in blocks (312.4M of 333.8M); 967 blocks, 6.7M block
  calls, 18.1k refused for a latched interrupt, 2.0k for the timer, 8.0k
  peripheral-store exits, 0 code mismatches.
- What is left is interpreted: 64% is code whose operands are Unknown in this
  diagnostic state (blocks need known registers), 8% return idioms, 15%
  CJUMP/RFRAME, 8% system forms, 3% MODE1 writes (instruction counts by the
  first block-level reason, from the replay's own profile).

**[O]** Not done: calls, returns, CJUMP/RFRAME and MODE1 writes in blocks
(about 25% of what is still interpreted); a differential check of one block
call against the interpreter for every entry (the replay hashes are the only
end-to-end evidence, over this one capture); a unit test that takes a real
interrupt after a timer-limited block (the replay crosses several hundred timer
periods, but no unit test takes one); an independent audit of the classification against the core
before this is marked **[V]**; the cost of re-hashing when a data store shares a
64 KiB page with generated code (not measured; `code_mismatch` stayed 0). The
one-command pipeline and the WASM and `sharc_live` builds of a DN2 library are
done (below).

**[D]** `tools/sharc_dn2_aot.py` builds the DN2 block library from scratch in
one command: strict transpile, a core-only build, `--cycles` (3) profiling
replays (cycle 0 interpreter-only, then rsgen + build + replay with blocks),
the final rsgen on all cycles' profiles merged (counts summed per key,
sorted), the build, optionally the wasm32 `--features sharc` core, the
five-way replay gate and optionally the coupled `sharc_live` run (threaded and
ColdFire-only). Everything goes under `--out` (refused inside a git tree);
`manifest.json` holds input, profile, gen-tree and library hashes;
`--compare` and `--expect` make it fail on a difference. The native library
is built with the gen path remapped and a fixed macOS install name, so its
bytes do not depend on the output directory. Two runs from scratch in
different directories: identical gen tree (`5cc4b4a0...`, 931 blocks),
profiles, native library (`a6336cae...`) and wasm (`bf106458...`), 0 manifest
differences. Gate: the replay hashes above (PCM `581339b3...`) in all five
runs; coupled ColdFire `fbace0f0...`, DSP export `05ac2ac2...`, PCM
`38d2a322...` (72,896 samples); ColdFire-only `4b569189...`. Its replay speed
equals the earlier hand-built 967-block library (frames 4600..5600, blocks and
skip: 5.96/5.78 s against 5.99/6.27 s back to back).

### Threaded coupling and native playback (2026-10-03)

**[D]** `sharc_peer::ThreadedPeer` (`DSP_THREAD=1` in `sharc_live`) moves the
SHARC engine into a worker thread (built there, so it need not be `Send`).
Frame N: the ColdFire thread sends it; the worker, which is one thread working
in order, finishes period N-1, does the SPI2 exchange, returns the reply, then
renders period N and the SPORT4 block while the ColdFire runs frame N+1. Frame
logic is one `Core` shared with the synchronous `SharcPeer`; `Shared` gets
each frame in order when the ColdFire thread next looks (`ThreadedHandle::sync`
/ `export` wait for the worker). DN2 1.11 coupled snapshot, trig, ready+100M
(1,139 frames): ColdFire state digest, DSP export sha256, PCM sha256, wav, q31
and per-frame csv are byte-identical to the synchronous run. Wall 68.4 s sync,
63.6 s threaded; the ColdFire alone is 5.3 s, so the DSP is the bound and
threading hides only the ColdFire share (about 7%).
- `--features play`: `pcm_play::PcmPlayer` feeds `native/live`'s `SpscRing` and
  cpal output from a feeder thread (linear resampling to the device rate
  outside the callback; the producer never blocks). `AUDIO=1` plays as
  produced (silence on underrun); `AUDIO_BUFFER=SECONDS` holds the audio back,
  then plays it in real time and plays out the rest at the end. No device:
  a message and the run goes on.

### ColdFire interpreter and machine speedups (native, exact) **[D]**

Time, board, SSI, PIT and DTIM paths of the Rust machine and the boot runtime
were made cheaper (batched timer/peripheral deadlines, fewer per-instruction
checks, soft-float and idle-loop fast paths) without changing any guest-visible
state. DN2 1.11 coupled snapshot, trig, ready+100M ColdFire instructions:
ColdFire-only (`DSP_PERIOD=1`) 5.3 s before, 1.7 s now (about 3x, 58M logical
ticks/s); coupled `DSP_THREAD=1` 14.8 s before, 5.8 s now; headless WASM
coupled core 21.3 s before, 13.5 s now. State digests are unchanged: coupled
ColdFire `fbace0f0...`, DSP export `05ac2ac2...`, PCM `38d2a322...` (WASM PCM
identical); ColdFire-only digest `4b569189...`. `sharc_live` prints a
`cf_clock` line (logical ticks, interpreted and idle-skipped instructions,
ticks per wall second) after the run. Not re-checked against the device.

### DSP block path: wider model-safe blocks and cheaper boundaries (2026-10-03)

**[D]** Exact speedups of the native SHARC engine with AOT blocks and all models on
(DN2 1.11). Where the time went first (native replay of the note capture,
macOS Time Profiler, leaf frames resolved through inlining): blocks ran
312M of 334M non-skipped instructions but the interpreter took as long as
all of them (about 1,700 host instructions per interpreted SHARC
instruction against about 130 in a block); inside blocks the loop-register
sync (`_sync_empty_loop_registers`, every instruction) and the
normal-word address test (`normal_word_to_byte`, twice per access) were
about 40% of block time; the runtime decode cache re-read every cached
word on every interpreted instruction. Changes:

- **Runtime**: cached runtime decodes stay valid while no write reaches a
  page holding one of their words (`Mem::watch_sw`, `dec_gen`; only for the
  engine's own `Mem::read_sw`); the boundary skips `begin`/`commit_host`
  when the SEC line is idle (`periph::sec_line_active`); `bank_codes` is
  static; `normal_word_to_byte` brackets its ranges first (also in
  `tools/sharc_core/addressing.py`, same mapping); `Stk::index` has a
  one-compare path.
- **Generator** (`GENERATOR_VERSION` 10): model-safe bodies now hold calls,
  the (DB) return idiom, RTS (Type 11a/11c with x=0; RTI stays out), CJUMP,
  RFRAME and the 21a/21c no-ops: their stack pushes and pops are the core's
  own steps, and only a PCSTKP write requests the PC-stack completion the
  engine does between instructions. Instructions that name a model register
  are decided by what their variant reads and writes: reads are safe except
  EMUCLK/EMUCLK2/TCOUNT; a write of MODE1, IRPTL, IMASK, IMASKP or MMASK
  ends the body ("terminal"), after which the engine completes the bank
  request as `St::commit` would and checks interrupts at the next boundary
  (the body saves `bank_requested_mask` for a trap); other model-register
  writes stay interpreted. A body skips the loop-register sync after an
  instruction that is not a DO or at a loop end (`loop_synced` fact).
  STKYX/STKYY keep run-time masks like ASTATX/ASTATY. Model-safe bodies
  check the limit before each instruction when the whole body does not fit
  (the core timer's limit cut them often in the coupled run). `--chain` is
  used; `--exclude 0xb88a49:0xb88abc` keeps bodies out of the idle-skip range
  (a chained call or route would otherwise run the idle loop in block code).
- **Engine**: a pending bank selection that changes no bank is completed
  ahead of a block (put back if the block completes nothing).
- **Coupling**: `SharcPeer` no longer steps the busy part in 1,024-instruction
  chunks: `Engine::step_until_in` stops at the first boundary in the idle
  range, then the chunk ends from there on are looked at as before, so
  `busy` (and the per-frame csv) is unchanged; blocks cut short at every
  chunk end ran their tails one instruction at a time (+26% host
  instructions on the replay window).
- **Tools**: `native/sharc/examples/dn2_replay.rs` (the replay without
  Python; `SAVE_AT`/`CLOCK` continue from a saved state, `CHUNK` imitates the
  old peer); `sharc_live` `DSP_PROFILE=PREFIX` (synchronous runs: coverage,
  entries, transitions for `sharc_rsgen.py`) and `DSP_EXPORT=PATH`.

Recipe of the measured library: `sharc_transpile.py --strict`, then
`sharc_rsgen.py dn2-1.11 --model-safe --explicit-memory-model 0 --chain
--exclude 0xb88a49:0xb88abc` with the replay's no-block coverage plus the
coupled run's interpreter profile (`--coverage` both files concatenated),
`--entries` from the replay and coupled profiles plus the PC after every
flagged instruction and the coupled run's hot interpreted PCs (3,000+ over
the run, outside the unknown-data loop), and `--transitions` from the same
profiles: 1,751 blocks, 20,722 instructions.

Results, same machine, runs alternated (the machine was shared, so
instructions retired from `time -l` back the wall times):
- Gates: the 5-way replay (`tools/sharc_dn2_replay.py`, frames 4600..5600)
  matches every state hash and PCM `581339b3...` in all five
  configurations; the coupled run (snapshot m5, trig, ready+100M,
  `DSP_THREAD=1` and synchronous) gives ColdFire `fbace0f0...`, DSP export
  `05ac2ac2...`, PCM `38d2a322...` (72,896 samples), and wav, q31 and the
  per-frame csv are byte-identical to the old peer's; ColdFire-only
  `4b569189...`.
- Replay window 5400..5600 (67.0M non-skipped instructions, native, from a
  saved state, three alternated pairs): 0.820-0.837 s before (80-82M
  instr/s), 0.325-0.332 s after (202-206M instr/s), 2.5x; host instructions
  15.5G -> 6.3G, cycles 3.1G -> 1.2G. Python replay with blocks and skip:
  5.9 s -> 3.3 s for the whole 1,000 frames.
- Coupled `DSP_THREAD=1` (HEAD built with the same block set as before, two
  alternated pairs): 5.8 s (7.6x slower than real time) -> 2.3 s (3.0x
  slower), 2.5x; synchronous 4.0 s. The DSP is still the bound (ColdFire
  alone 1.7 s).
- Still interpreted in the coupled run (5.8M instructions, 5k per frame):
  half of it is the loop at `0x1c253f` (5 instructions x 496 per call), which
  loads data that is not in the state (Unknown) every iteration; blocks need
  known registers, so it stays in the interpreter. The rest is mostly
  interrupt returns into the middle of bodies.
- With `DSP_IDLE_SKIP=0` the idle loop is now interpreted (no blocks in its
  range): the 5-way no-skip replay takes 27 s instead of 14 s.

**[O]** Blocks for registers known to be Unknown (the `0x1c253f` loop); the
interpreter's cost per instruction (`bnd::_field` scans are 29% of it);
entries for interrupt return points without listing hot PCs by hand; a
one-command pipeline; the WASM build of a DN2 library.
### ColdFire interpreter round 2: compact decode, split handlers, fused loops (2026-10-03) **[D]**

Exact speedups of the native ColdFire path (`native/coldfire`, `machine`,
`periph`, `boot`). No guest-visible state changes; every gate below is
byte-identical to the previous build.

- `Insn` is 32 bytes (was 92): `Ea`/`Operand` are 8 bytes (`Ea::PcIdx` stores
  `base + d8`), three stored operands, and the MAC/MSAC register, scale, mask
  and accumulator fields packed in one word; `Insn::operands()` rebuilds the
  full list. Decoder text (`cfdis`) is identical to the previous build at every
  halfword of DN2 1.11 and DT2 1.16 MAIN and for all 65,536 opwords with 16
  extension-word patterns.
- `Cpu::step`: privileged/FPU checks from one per-form table; the cache-miss
  path is out of line; `Exc` packs into one non-zero word, so the hot handlers
  return `Result<(), Exc>` in a register. The 25 most frequent forms run in
  their own small functions (`x_*`), the rest in `execute_slow`.
- Lazy flags: `cond` must still resolve N/Z/V into `sr`. A version that only
  read the resolved value changed the ColdFire state digest: the timer model's
  IPL tracker records the raw `sr` (`Time::seed_sr`), so the resolve points
  are observable state.
- Bus: `LoggingBus` returns the board result directly when untraced (zero-page
  and logging tails are cold), `Board` reads/writes SDRAM through an in-page
  fast path, and `owned_mmio` rejects addresses below the end of SDRAM (a
  const assertion checks every device slot starts above it).
- `run_fast` adds the interpreted-instruction count and formats an error once
  per exit instead of per instruction.
- Fused loops (`coldfire::fused`): five loops found by their exact encoding at
  load (any address): a word interleave (`move.w #0x8001,(a0)` + source word;
  source register a2 on DT2, a3 on DN2), a 16-byte MOVEM copy, a 16-byte clear
  and two EMAC filter loops (DN2 0x400db160, 0x400db1f4; DT2 0x400d92d8,
  0x400d936c). Their heads are watched; at a head `run_fast` runs whole
  iterations while (a) the loop branch is taken, (b) every access is plain
  mapped RAM (`Bus::plain_ram`) and no write hits a decoded page, (c) every
  body instruction is already in the decode cache, and (d) the batch ends
  before the timer/SSI tick limit. The first three are written out; the EMAC
  loops call the interpreter's own handlers on the decoded instructions. The
  instructions count as interpreted, and the bus/capture bookkeeping is left
  as the last instruction sets it. `tests/fused.rs` compares fused against
  single-stepped iterations (whole `Cpu` incl. lazy flags and decode cache,
  and memory) over random registers, MACSR modes, overlaps, non-plain holes
  and writes into decoded code; mutating one flag update or one EMAC step
  makes it fail. In 300M ticks from the DN2 coupled snapshot, 7.04M
  interleave/copy/clear iterations (21% of instructions) were fused.
- Tried and dropped: a generic short-loop runner from pre-decoded bodies (84
  host instructions per guest instruction against 104; no net gain after its
  entry checks), and register-only leaf fast paths (no gain).

Measurements (this Mac, otherwise idle, `sharc_live` from
`digi-audio-m5.snap`, NOTE_EVENTS=trig, four alternating runs, median):

| run | before | after |
|---|---|---|
| CF-only (`DSP_PERIOD=1`), ready+100M | 56.7M ticks/s, 1.8 s | 127.3M ticks/s, 0.8 s |
| CF-only, ready+300M | 75.2M ticks/s, 4.0 s | 173.3M ticks/s, 1.7 s |
| coupled `DSP_THREAD=1`, ready+100M | 6.3 s wall | 6.3 s wall (DSP bound; CPU cycles -11%) |
| DT2 1.16 cold boot to ready+20M (`dspi2_capture`) | 10.0 s | 5.8 s |
| `cfrealmix` boot280M.cfdump, 100M | 78.4M instr/s | 132.9M instr/s |

Real time is 132M ticks/s: the CF-only run is at 0.96x in the first 100M after
ready (note-on) and 1.31x over 300M.

Gates, all identical to the previous build: CF-only digest `4b569189...`
(100M) and `bd39b037...` (300M); coupled ColdFire `fbace0f0...`, DSP export
`05ac2ac2...`, PCM `38d2a322...` (72,896 samples); DT2 cold boot capture
`33e7918c...` and state `78dba198...`; `cfrealmix` hash `0x0bb544e65d49266b`;
`cf_lockstep` fuzz (DT2/DN2, seed 42, 20k) without divergence, `emac` output
byte-identical to the previous build (its known oracle exemptions), `snap`
boot280M 20k clean; `cargo test --release` in coldfire, periph, machine,
card, boot (with and without `sharc`, firmware present), including
`batched_chunks_match_single_steps_from_cold_boot`; boot builds for
wasm32-unknown-unknown. Not re-checked against the device.

### Merged speed streams: integration notes (2026-10-03)

**[D]** The ColdFire speed-up (fused loops, 32-byte `Insn`), the DSP block
speed-up (generator 10, `step_until_in`) and `tools/sharc_dn2_aot.py` were
merged on `e5f61f0`. The merged generator, run with the DSP stream's coverage,
entries and transitions inputs, reproduces that stream's DN2 gen tree
byte for byte (`0e73c5c1...`, 1751 blocks, 20,722 instructions; sha256 of the
sorted `*.rs`/`*.bin` hashes). Merged tree, back to back on one machine: replay
frames 4600..5600 with blocks and skip 2.95 s (busy window 0.31 s, 217M
instr/s), against 5.9 s before; threaded coupled run 2.0 s for 0.757 s of audio
(2.7x slower than real time), ColdFire-only 0.8 s, synchronous 2.8 s. All
five replay runs and the coupled and ColdFire-only digests match the
references above.

**[D]** A DN2 library used with `SharcPeer` must be generated with `--chain`
and `--exclude 0xb88a49:0xb88abc` (the idle loop). `Engine::step_until_in`
stops at the first idle-range boundary only if no block or route enters that
range; nothing records or checks the exclusion in the library (build info and
`GENERATOR_VERSION` do not carry it). Without it only the busy and per-frame
diagnostics can change, not state or PCM. `tools/sharc_dn2_aot.py` therefore
takes `--chain` and `--exclude` (both recorded in the manifest settings); its
earlier 931-block gen (no chaining) predates generator 10 and is stale.
**[O]** carry the excluded ranges in the library build info and make
`NativeDsp`/`SharedDsp` fall back to chunked stepping when the idle range is
not covered.

**[D]** A block or chain re-checks `blocks_code_ok` only at dispatcher entry,
so a store inside a region that rewrites code the same region (or a chained
one) runs later is seen one dispatch late. Not reachable in the DN2 capture and
not exercised by any gate. **[O]**

## DN2 audio triage: launcher, stage baselines and sink health (2026-10-03)

**[D]** `mise run emu` was reproduced with the pinned Rust 1.98.1 toolchain:
the plain WASM/web build succeeded, then Cargo rejected the stale desktop lock
under `--locked`. An offline update of the local `elektron-native-boot` package
added its direct `periph` dependency reference and selected the already-locked
`syn 2.0.119` for `cssparser-macros`; no registry package versions/checksums
were added or changed. Locked/offline metadata then passed. A second bounded
launch compiled the desktop in about 49 s and kept the application process
alive for eight seconds before intentional SIGTERM. No firmware was loaded in
that launch; this is not a desktop audio or rendered-UI gate. A non-fatal
`rust-objcopy`/`libLLVM.dylib` stripping warning remains.

**[D]** Serialized, bounded DN2 1.11 stage baselines reused existing local
release artifacts, the paired ready snapshot and `NOTE_EVENTS=trig`, with
playback off. Native/headless WASM windows were ready+100M ticks; DSP replay
used frames 4600..5600, timing [5400,5600). Single-run observations, not a
performance distribution:

| stage | wall time | audio window / interpretation |
|---|---|---|
| CF-only diagnostic (`DSP_PERIOD=1`) | about 0.8 s | about 0.759 s nominal; little CF headroom |
| DSP replay busy window | 0.314 s | 200 blocks = 0.133 s; about 213M non-idle instructions/s |
| coupled native, threaded | about 2.1 s | 0.759 s PCM, about 2.8x slower than real time |
| coupled native, synchronous | about 3.0 s | same PCM, about 3.9x slower |
| headless Node WASM | about 5.8 s | same PCM, about 7.6x slower |

All seven replay state-hash prefixes and PCM `581339b332e9...` matched the
previous gate. Threaded/synchronous native CF `fbace0f0...`, DSP export
`05ac2ac2...`, and PCM `38d2a3224d10...` matched; headless WASM produced the
same PCM (72,896 interleaved values). CF-only digest was `4b569189...`.
No coupled run halted or missed a SPORT block. This supports a native DSP
throughput bottleneck, not proof that every workload or link behavior is correct.
The extra WASM cost still needs CF/DSP host-profile attribution.

Artifact provenance is bounded: the measured native example SHA-256 starts
`cfb4bbe0`, native library `70e69f17`, and WASM `b53c378d`. The generated tree
currently hashes to `e0e97d72...`, not the handover's `0e73c5c1...`; no fresh
source-bound build/provenance claim is made. The old pipeline-c manifest is
generator 9 and is not a manifest for these artifacts. Input hashes matched
the recorded firmware/capture/snapshot hashes, including DN2's own extracted
section source marker. The root `sections/` marker belongs to DT2, not DN2.

**[D]** An actual headless Chromium worker-to-AudioWorklet smoke used the same
WASM hash and snapshot, scripted menu/note input, a 0.1 s diagnostic buffer,
and a 15 s observation. AudioContext was running; the worklet acknowledged its
port, received and consumed PCM. It reported 15 underrun events and about
12 s of starvation/rebuffer silence after first playback. The worker produced
1.825 s PCM over 15.538 s session wall time (0.117 audio s/wall s), with 2,738
frames, no missing block, no DSP halt and no runtime error. This establishes
pipeline activity and production starvation, not speaker audibility, waveform
quality or sustained real time. Sink reports are periodic, so core/sink counter
snapshots are not simultaneous.

**[D]** The browser coupled UI now shows PCM production rate, queue high-water
mark and underrun duration, and exports an `audio-health.json` containing core
and sink observations. Worker diagnostics include the audio report; host
response timings use eight fixed buckets. Initial buffering silence is
separate from starvation after playback begins. Production duration comes
from newly emitted PCM, not cumulative DSP instructions restored from a
snapshot. The elapsed production clock excludes load but includes pause and
worker-yield time. Scheduling, buffer defaults and firmware semantics did not
change. Four Node audio-accounting/worklet tests, Astro check (zero errors and
warnings; one existing hint), locked offline desktop metadata and diff checks
passed. The full firmware/Python suite was not rerun for these host-only edits.

**[C] [D]** Ordinary Tauri remains CF-only, but an explicit local
`coupled-audio` desktop seam now restores the paired DN2 inputs, enables SSI
pacing, owns a threaded SHARC peer and optionally connects the existing player.
Its diagnostics expose `native_audio`, including sink state and post-start
device errors. A CPAL callback error after a successful start is shared with
the feeder, latched in health and terminates draining rather than hanging it.
This is software integration evidence, not sustained real-time, speaker
audibility or fidelity validation. Next work must preserve the state/PCM gates,
not lower instruction budgets or hide starvation with a larger buffer.

The private continuation, image, paired ready snapshot and note capture have
hash-identical backups in ignored `snapshots/dn2-audio-ready-2026-10-03/`.
Triage logs and the local Chromium smoke harness are under
`/private/tmp/dn2-audio-triage-20261003/`; they are not durable distribution assets.

### Native coupled acceptance and cross-link profiling (2026-10-03)

**[D]** Focused acceptance covered live (66 tests), PCM player (9), threaded
peer (6), and ignored private coupled fixtures, including missing device. Four
current ready+100M workloads (one default and three profiled, playback off)
preserved CF `fbace0f0...`, DSP `05ac2ac2...`, PCM `38d2a322...`, 72,896
interleaved samples and zero missing SPORT blocks. The fixture emits 1,139
sent/completed frames and 0.759333333333 s source audio. It is headless and
bounded; it does not validate speakers, GUI, browser/WASM, sustained real time
or waveform fidelity.

`DN2_PROFILE_LINK=1` opts into cumulative, window-delta link timing under
`native_audio.link_timing`; timing is off by default, stores no histories and
adds no worker-shared lock. Units are nanoseconds. The worker replies before
rendering the current DSP frame, so host reply wait commonly overlaps prior
rendering; queue/render/CF and enclosing poll/sync durations are non-additive.
Across the latest three workloads, elapsed wall was 2.121-2.189 s; worker DSP
2.092-2.159 s, reply wait 1.390-1.455 s, SPI 9.358-9.565 ms, enqueue
181-242 us, collect 502-542 us, poll 213-254 us, sync 1.600-1.927 ms and PCM
handoff 167-198 us. DSP work sets the pace and the host waits for its worker;
arithmetic versus memory-model versus generated-core CPU cost remains open.

The standalone measured window has 66,212,132 generated and 778,452
interpreted busy instructions, plus 66,409,416 idle; 98.8% of busy instructions
are generated, not a CPU-time attribution. Ordinary DSP-only timing was
0.32-0.35 s for about 0.133 s source audio with hashes verified every run.
ThinLTO paired medians were 0.320 s ordinary and 0.322 s ThinLTO with
overlapping ranges, so it was not adopted. The generation fingerprint was
`e0e97d72ebbdfa97963772ef39d3519e0ef9a474da94488fc87d05e3ecad088a`.
Evidence is under `/private/tmp/dn2-audio-restart-20261003/{cross-link-profile,dsp-profile,dsp-thinlto}`; separate binaries and instrumented timings are distinct baselines.

Reproduce only with the private fixture and generated-tree environment used by
the handover: pinned Rust 1.98.1, offline/locked Cargo, `SHARC_GEN_DIR`,
`NOTE_EVENTS=trig`, `DIGI_COUPLED_FIXTURES` and `DIGI_COUPLED_SYX`, then run
`cargo test --release --offline --locked --manifest-path packages/desktop/src-tauri/Cargo.toml --features coupled-audio desktop_runtime::coupled::tests::coupled_ready_exactness -- --ignored --nocapture --test-threads=1`.
No hardware is involved.

### DSP replay CPU sampling and rejected inlining experiments (2026-10-03)

The 35 s, 1 ms native sample is a source-bound profiling observation, not a
firmware verification or a real-time result. Build an optimized symbol-bearing
replay artifact with `CARGO_PROFILE_RELEASE_DEBUG=1`, use the matching source
and generated-tree hashes, set `DN2_REPLAY_REPEATS=25`, and run
`/usr/bin/sample PID 35 1 -file measured-window-35s-1ms.sample.txt`. The
durable artifact is
`/private/tmp/dn2-audio-investigation-20261003/long-profile/measured-window-35s-1ms.sample.txt`.
Its `measured_window_step` marker accounts for 4,937 of 29,193 raw samples.

Direct-child self accounting under that marker is disjoint: generated blocks
are 3,268 samples (66.19%), `exec_insn` ancestry 1,334 (27.02%),
`Engine::step` dispatch 265 (5.37%), and other bookkeeping 70 (1.42%). The
window counter has 778,452 interpreted instructions among 66,990,584 busy
instructions (1.16%); that instruction fraction is not the 27.02% sampled CPU
share. The marker wraps only the measured `Engine::step` workload, so it
excludes import, SPI and SPORT and is not a universal CPU-cost attribution.

Three narrowly source-bound inlining experiments preserved the replay hashes
and exact window counters but did not show a repeatable gain. The empty-journal
`St::old_of` guard had seven-pair candidate/base median 1.0063 (range
0.9755-1.0219). Forcing `forms_move::_type_3a_transfer` inline had median
1.0065 (0.9777-1.0192). Forcing `compute::_apply_compute_simd` inline removed
its five direct calls and made the binary 96 bytes smaller, but its default
seven-pair median was 1.00325 (0.9936-1.0358). Its repeat-five batches, with
all 70 replay instances passing PCM/state/counter gates, had summed-window wall
median 0.99936 (0.99037-1.01887; four wins, three losses) and CPU median 1.0.
None was adopted.

Global direct `memcpy` call count stayed 236 before and after the inlining
experiments. The sampled 424-byte copies concern compute-result `Option`
tuples, not `Fields`; a 200-byte `Fields` prologue also exists, but broad borrow
changes are not ready. The next conservative lead is the compute-output `Tup8`
representation: usual compute output is three items, yet the shift FIFO has
four destinations and float flag lists reach seven. A global cap of three or
four is unsafe and `from_slice` truncation would be silent. Any follow-up needs
per-type capacity bounds, a default-eight fallback, and a consistent full
generation rather than substituting core-only tables or symbols.

For that future full generation, use the project database
`out/sharcdb/dn2-1.11.sqlite` and section blob
`out/sections/dn2-1.11/section_7_BLOB.bin` with a fresh work and output
directory:
`tools/sharc_rsgen.py dn2-1.11 --coverage /private/tmp/digi-r1-pipeline-a/prof/merged-final --entries /private/tmp/digi-r1-pipeline-a/prof/merged-final.entries --transitions /private/tmp/digi-r1-pipeline-a/prof/merged-final.trans --model-safe --explicit-memory-model 0 --chain --exclude 0xb88a49:0xb88abc --region-insns 120 --region-regs 36 --work FRESHscratch --out FRESHscratch`.
The current canonical generated-tree report records 1,751 blocks and 20,722
block instructions. It was made from a different accumulated profile set, so it
is not a baseline for the controlled fresh generation below.

### Controlled fixed-arity compute-result trial and raw-PC cost map (2026-10-03)

**[O]** A fixed-arity (two/three/four) compute-result payload trial was
source-bound and fully reversible. It passed its focused type, translator and
compute-family checks, then a fresh, same-input baseline/candidate generation
showed a material coverage difference: 1,277 baseline blocks versus 1,265
candidate blocks. The candidate's fixed tuple union stopped block specialization
at `_apply_compute`'s dynamic register-index write because `static_zip` does not
project a union of fixed tuples. This is a code-generation coverage change, not
evidence that the payload representation itself is faster.

One controlled default replay per generated tree kept the PCM hash
`581339b...889de6d`, state hash `eeeeda3a...ddf3e5b8`, 133,400,000 instructions,
66,409,416 idle instructions and 66,990,584 busy instructions. The baseline
measured 0.424 s CPU; the candidate measured 1.504 s CPU (3.55x slower), with
about 2.0M versus 12.0M windowed interpreted steps. The candidate was rejected
and all seven owned source/test paths were restored byte-for-byte. The result is
the net outcome of the type and generated-coverage change; it is not a storage
optimization measurement and does not establish real-time audio.

An own-process Xcode Time Profiler capture of the existing symbol-bearing replay
artifact exported 29,478 unaggregated 1 ms user-stack samples over 29.744 s. The
separately supervised 25-repeat replay completed afterward with the same PCM,
state and per-window counter gates on every repeat. Its raw PC and full-stack
records are under
`/private/tmp/dn2-audio-investigation-20261003/strategic-profile/`. The raw
table contains two duplicate `Stackshot` rows at one timestamp; the decoded
Time Profiler table contains 29,476 running rows and omits those two rows.

The raw record's current-PC field is distinct from its unwound stack-vector
addresses. The trace records the replay image load address as `0x102230000`;
using it puts `measured_window_step` at `0x102230bd4..0x102230cb0`. That range
selects 5,187 raw rows, all of which join the decoded table by timestamp. DWARF
attributes the wrapper's body to `step_workload`, so the wrapper name is absent,
but 5,183 selected stacks retain `Engine::step`; the other four are generated or
interpreter tails whose `Engine::step` frame was not retained. This is a valid
sampled scope for the 200-frame `5400..5600` workload window, repeated 25
times, rather than a full-replay ranking.

Within those 5,187 selected samples, generated block leaves account for 3,363
(64.835%) and generated core helpers for 1,023 (19.722%); `Engine::step` is 298
(5.745%), `exec_insn` 188 (3.624%), and `platform_memmove` 93 (1.793%). The
remaining 222 leaves (4.280%) are other functions. The reproducible
`scope_marker_profile.py` classifier emits all six mutually exclusive categories
in `marker-scoped-symbolicated-costmap.json`, asserts that their counts total
5,187, and checks that selected timestamps are unique. The
largest leaves are generated block `r_1C399A` (391, 7.538%), `Engine::step`
(298, 5.745%), generated `r_1C3862` (199, 3.837%), generated `r_1C364F` (197,
3.798%), and generated `__compute` (196, 3.779%). No selected leaf is hashing,
state import or state export. These are sampled leaf rankings, not cycle counts,
and their overlapping stack ancestry must not be added. They can prioritize
measured-window investigation, but do not establish an audio CPU saving,
sustained real time, or a particular optimization. The symbolicated export
contains no `_OUTLINED_FUNCTION_*` or anonymous frame, so it supplies no
evidence that LLVM machine outlining is a dominant cost.

The earlier 27.02% interpreter figure groups sampled self costs by an
`exec_insn` ancestor; the 3.624% above counts only leaves named `exec_insn`.
Shared helpers called by the interpreter belong to its ancestry cost but have
their own leaf names. These percentages use different accounting and do not
show an interpreter speedup.

The same window's interpreter coverage narrows the largest fallback workload
to five PCs, `0x1c253f`, `0x1c2542`, `0x1c2545`, `0x1c2548`, and `0x1c254b`:
each runs 99,200 times, together 496,000 of 778,452 interpreted instructions
(63.716%). This is instruction frequency, not a CPU percentage. Existing
generated code for their loop returns without progress at entry 96,000 times.
An option-26 register-state histogram now explains those entries: all have
MODE1 `0x39003cf8`, no pending transfer, and unknown R0 and R15 masks. Both
registers are required to be fully known by that region's first guard. The
diagnostic replay retained the original PCM/state hashes, measured instruction
counts, and windowed Stats/ModelStats. Its instrumentation time is not a
throughput result.

Option 26's new profile kind 4 records zero-progress entry bails as
`pc MODE1 unknown-register-bitset pending count`; `dn2_replay` writes it as
`.entry-bails.tsv`. It snapshots register masks at the bail, rather than
asserting that every unknown register caused it. Recording occurs inside the
existing optional exit-profile branch; normal playback does not collect it.
Artifacts are in `strategic-profile/bail-window.*` under the scratch root above.


### Unknown-value DSP loop fallback: measured CPU saving (2026-10-03)

**[O]** The entry-bail histogram above identified a specific lost AOT path,
rather than suggesting a general helper rewrite. Region `r_1C253F` already
contains the five-instruction loop, but its known-register guard rejects R0
and R15. A selected fallback now executes the same generated instruction
bodies with runtime known-bit masks. The ordinary fully-known path remains;
`Rf.allow_unknown` permits full `V` register writes and unknown memory loads in
the fallback. Pending transfers, per-instruction budgets, special-register
restrictions, instruction rollback and logged-memory rollback remain guarded.
The first pilot only relaxed register writes and still trapped on unknown
loads; it showed no gain and was superseded by the complete fallback.

Seven alternating baseline/candidate pairs used the same original 1,751-block
cache, replay inputs, release settings and measured `5400..5600` frame window.
All 14 replays retained PCM
`581339b332e936fa9204926f5087b66d9495f530b71272bf0aa24c7ff889de6d`, state
`eeeeda3a581d3eec5f7d7f35f3ad8ec03b8d744d59c15e2c66ea8947ddf3e5b8`, and
133,400,000 measured instructions (66,409,416 idle; 66,990,584 busy).
The median paired candidate/baseline CPU ratio was 0.78443: **21.6% less CPU,
1.275x throughput**. Every pair improved. Median CPU times were 0.321 s and
0.255 s respectively. Windowed interpreted instructions fell from 778,452 to
298,452; generated instructions rose from 66,212,132 to 66,692,132. Traps stayed
zero and block traps stayed 12,932. The 480,000 instructions moved to AOT are
not skipped work. This window represents about 0.133 s audio, so its improved
DSP CPU time is still about 1.9x too slow for real time.

The native coupled fixture also preserved the existing ColdFire, DSP and PCM
hashes (`fbace0f0...1a2cb2c`, `05ac2ac2...f304e19`, `38d2a322...3ff1f8`),
72,896 interleaved samples, 1,139 frames and zero missing SPORT frames. Its
0.759333 s audio took 1.816159 s elapsed; DSP work took 1.786057 s, SPI
0.009498 s and SPORT draining 0.005716 s. The final version-11 rebuild passed
both fixtures; its first workload measured 2.161473 s. Three subsequent runs of
that built exactness fixture measured 1.814975, 1.785510 and 1.818857 s (median
1.814975 s), preserving all exactness gates. This variation reinforces the need
for paired controls before quoting an integrated improvement percentage.
The reply wait overlaps DSP work and
must not be added to it. This integrated run remains about 2.4x slower than
real time. Its historical baseline is not a contemporaneous paired control,
so the 21.6% saving applies to the DSP replay window, not a measured integrated
percentage. Browser/WASM and Windows have not been performance-tested for this
change; the implementation uses portable Rust.

`tools/sharc_rsgen.py` and `tools/sharc_dn2_aot.py` expose
`--unknown-fallbacks 0x1c253f`. It selects regions containing the named block;
it is opt-in, not a global removal of knownness guards. The AOT manifest records
this choice. Generator version 11 identifies the new runtime register-file
mode. Focused checks passed: 47 runtime unit tests and 17 generator/model-safe/
AOT tests, including execution of emitted Rust with known, unknown and partial
values, strict-path rejection, pending-transfer rejection, budget exits and
register/memory rollback. Both ignored private native coupled fixtures passed, including a final
version-11 rebuild. A separate replay check with 1,024-instruction stepping
also preserved baseline/candidate PCM, state and measured instruction counts.

Reproduction artifacts are in
`/private/tmp/dn2-audio-investigation-20261003/unknown-loop-fallback/`:
`paired-7.json`, individual replay logs, the failed register-only pilot,
`emitter-equivalence.json`, `cache-provenance.json`, selected-region generation
command/report, `chunk-1024-fidelity.json`, and coupled logs/repeats. The provenance manifest distinguishes the
measured overlay from inherited generation reports; version-11 metadata changes
no instruction code. The measured cache is a private derived copy in `candidate-gen`;
only the selected region and generator-version constant differ from the
immutable `/private/tmp/digi-r1-int-gen-dn2` baseline. Source-emitter equivalence
checks match both the original region and the measured fallback. The separate
`selected-gen` output has only one AOT block and is a generation check, not an
application cache. Future full AOT generation must carry the new option.

Next attribution should profile the improved artifact on the same marker
window, recompute the remaining interpreter and generated-body cost, and choose
one change from that evidence. Do not reuse the old cost map as though it
profiles the optimized binary. Sustained real-time audio remains open.

### Improved DSP profile and default audio startup (2026-10-03)

**[O]** After commits `4660aee` (native audio integration) and `dc6863f`
(unknown-value loop fallback), the version-11 candidate was profiled again.
The symbol-bearing release replay used the same `5400..5600` marker window,
repeated 25 times. Binary UUID `22548728-F14C-325C-9A7E-DF5C107CF9C7`, its
trace load address and marker PC range reconcile with the raw and symbolicated
tables. All 6,415 marker-scoped samples join uniquely; 6,408 retain
`Engine::step`, and all retain `step_workload`. The decoded wrapper name is
absent because of inlining, so selection uses the raw marker address range.
An independent evidence audit checked the scope, binary identity, category
totals and exact instruction counters.

The disjoint sampled leaf costs are generated blocks 82.245%, generated-core
helpers 6.516%, other 5.066%, `Engine::step` 3.601%, `exec_insn` 2.089%, and
`memmove` 0.483%. The largest generated regions are `r_1C399A` (9.930%),
`r_1C3862` (4.832%) and `r_1C364F` (4.801%). The retained unknown-value
fallback has only 0.826% leaf cost. Generated-core helpers can also be called
from the interpreter; these categories are not an AOT ancestry decomposition.
Exact counters still show 66,692,132 generated and 298,452 single-step
instructions out of 66,990,584 busy instructions (99.5545%/0.4455%). These
instruction shares are not CPU shares. The 19,465 zero-progress entry bails
also do not establish costly remaining fallback candidates.

Whole-scope innermost source attribution groups numeric/address/bit-conversion
helpers at 12.814%, memory helpers at 11.473%, and status/ASTAT helpers at
8.839%. These are name-token families of sampled leaves, not causal cost
decompositions or projected gains. In particular, address translation and
`f32::from_bits` are included in the first family; it is not a measurement of
expensive floating arithmetic. Memory attribution does not prove bandwidth
or cache pressure. The next performance experiment should target a demonstrated
cost within the generated bodies and repeat the paired fidelity/performance
gates, rather than ranking optimizations by entry frequency alone.

Private reproduction records are in
`/private/tmp/dn2-postgain-profile-20261003/results/`, including
`marker-scoped-symbolicated-costmap.json`, `counter-summary.json`,
`trace-attribution-reconciliation.json`, `hot-r_1C399A-inline-operations.json`
and `whole-scope-inline-operations.json`. The later generated-cache hash
inventory is explicitly retrospective, not a pre-build source attestation.
The measured private candidate was copied byte-identically to the stable
application location `out/native/dn2-audio/gen`; generated code and firmware
remain ignored and local.

**[O]** `mise run emu` now attaches the locally available matching DN2 ready
session and live audio by default. `--no-audio` mutes playback while retaining
the DSP; `--cf-only` selects the CF diagnostic path. Automatic setup compares
the selected firmware bytes with the independently configured ready profile,
so selecting different firmware does not reuse its DSP continuation. Explicit
manual coupled inputs remain trusted local configuration. Native and Tauri
frontend builds invoke Astro directly, avoiding the browser package's WASM
prebuild. The normal browser build includes SHARC when its local generated
core is available; visible setup still requires the user to select private
image/state inputs and use a browser audio gesture. No private assets are
bundled into the repository.

Two startup/display regressions were corrected. A saved frame's serialized
delivery revision described the old observer; restore now uses a separate,
nonserialized replay flag to deliver the saved LCD once while preserving
load/save byte equality. The desktop actor also retains the initial CLI load
snapshot for the frontend startup request, since the CLI consumes that reply
before the window exists. Tap retains the latest display frame across its
bounded hold. The optional audio row had occupied the faceplate's sole sized
grid row; a flex column now gives the panel the remaining height regardless
of optional rows. Native GUI inspection confirmed visible LCD/panel and
continued panel visibility after pad and audition interactions.

Focused checks passed: restored-frame byte equality and once-only delivery,
private actor startup/nonzero LCD, existing private 100M coupled exactness,
Tap holding across chunks, seven launcher tests, six browser worker/audio
tests, Astro check and independent review. A default-speaker CPAL smoke passed
outside the sandbox; a sandbox CoreAudio failure did not establish device
unavailability. Follow-up native builds took about 14–17 s and reused SHARC.
The first new default build compiles roughly 77 MiB / 2.08 million generated
Rust lines that the former CF-only default did not compile; Cargo's crate
counter does not indicate progress inside that compilation. Native and WASM
artifacts have separate caches. These build timings do not measure playback
speed, and sustained real-time audio remains open.

### Live native sound investigation (2026-10-03)

**[O]** The user reported mostly noise from a fresh firmware selection and
Audition Trig 1, with no intervening controls. A restored desktop-only test
holding Trig 1 for 40M guest instructions immediately produced 29,056 f32
values / 0.302667 s of exact silence; a delayed press and keyboard-mode trial
also produced silence. The immediate hash is
`a50586e86b2d0246bd05181dd1f877b1b2570f2c1eb74b7613c9b17dbd4eb75a`.
Those short runs do not reproduce the later live GUI state. The existing
QA-plus-note fixture produces nonzero PCM, RMS 0.2950, peak 0.6930,
no clipping and a dominant frequency near 263 Hz; its canonical PCM hash
remains `38d2a322...3ff1f8`. It runs two NO presses and an encoder turn that
desktop startup does not run. Neither this reference nor an UNTITLED display
proves hardware factory-init patch fidelity.

An opt-in native pre-sink capture now uses `DIGI_EMU_PCM_F32LE=PATH`. It opens
or truncates the explicit local path on each new coupled session, writes
interleaved f32LE batches before the player, and stops at 960,000 values
(10 source-audio seconds). With the variable unset it creates no recorder,
file or capture buffer. File work does not run on the real-time callback;
errors latch in conditional `native_audio.pcm_capture_path/count/error`
diagnostics independently of output-device errors. Byte order, limit and
write-error tests passed; the coupled desktop suite passed 7 tests with
5 expected private-fixture skips. Capture is diagnostic, not a throughput
benchmark.

A fresh native GUI run selected the DN2 firmware through the picker and used
only Audition Trig 1. Its bounded 10 s pre-sink capture contains finite,
nonzero PCM, peak 0.6930, no clipping, and whole-capture RMS 0.08646. SHA-256:
`b50148499af2b3f3c27d5243f3c05dab34f04ec36a3bdf05ef4ed94335fc7b16`.
The output device opened as MacBook Pro Speakers while underruns accumulated.
The callback explicitly substitutes silence for missing frames; its underrun
counter counts callbacks with any shortage, not missing-frame duration. A
raw-signal spectrum shows a clean, sine-like tone: active source interval
8.39–9.49 s, active RMS 0.21863, near-zero DC, no clipping. A stable in-note
window has a fundamental near 263 Hz with 99.9903% of non-DC power in its
±20 Hz band, and 0.00969% inharmonic energy. There is no repeated discontinuity
at the 32-frame DSP block boundary (largest boundary jump 0.02698 versus
0.02821 elsewhere; none above 0.1). These measurements reject pre-sink
broadband noise for this actual GUI reproduction. Playback starvation/host
output is now the appropriate investigation boundary; they do not by
themselves measure physical output or prove that gaps explain every audible
artifact. The unnormalized `gui-pre-sink-active-1p6s.wav` provides a gap-free
comparison. The user listened to that WAV and confirmed a clean tone while
live emulator playback remained noisy. This independently supports the
live delivery/output boundary; it does not yet quantify which host delays
produce the audible artifacts. Private captures and analysis live under
`/private/tmp/dn2-audio-listening-20261003/`.


**[O]** A fresh default coupled browser WASM build completed in 276.69 s in its
separate cache. The normal `emulator-core.wasm` exports `digi_load_coupled`,
restored the ready fixture and delivered the initial LCD. The bounded QA-plus-
note ABI smoke produced 72,896 PCM values with canonical SHA
`38d2a3224d10e0b59cafa602ba1b8f8be31562efa07491b963ff4e127f3ff1f8`,
1,139 frames and no missing blocks. Artifact SHA:
`4b1321270c853c8c2667cef166c3562fdb29d369a65161d3df2145b9eddc169b`.
This validates the optimized default artifact's ABI/PCM correctness, not
real-time browser playback. The focused native boot restored-frame test also
passed with the current byte-equality and once-only delivery assertions.

**[O]** Source inspection found a separate host observation cost: the native
audio UI polls `emu_diagnostics` every second. On the actor, that call waits
for outstanding DSP work via `audio.sync()` and recomputes SHA-256 over the
3,192,192-byte DN2 main image before returning audio health. Full profile/event
collection is disabled in the default launcher feature set. The cost and
audible impact of periodic sync/hash remain unmeasured; source inspection
does not establish that they dominate the already DSP-bound workload. Existing
`host` metrics measure whole IPC step responses. Their wall residual includes
load, pauses and user time, so only an uninterrupted run with load subtracted
can estimate off-step overhead. No effective timer-clamp cost was measured.
A native GUI export attempt with `DN2_PROFILE_LINK=1` timed out in Computer Use
after Export diagnostics; it yielded no report and is not performance evidence.


### Underrun work: retained flag-update improvement (2026-10-03)

**[O]** The clean pre-sink/live-noisy comparison identifies the delivery
boundary, but the existing integrated DSP deficit remains the primary
throughput problem. An interactive-QoS probe on the coupled DSP worker reused
the native live helper. Five off/on pairs all preserved exactness and all
requests succeeded, yet both settings still needed about 2 s for 0.759333 s
audio. Variation and fixed off-before-on ordering limit attribution. The
probe was removed; it is not a real-time fix. Logs are private under
`/private/tmp/dn2-qos-20261003/`.

The improved profile identified repeated `_astatx_define` work. Its three
value-shape cases were replaced with one known-mask merge and bit merge, with
the zero-merged-mask case preserving the old value verbatim. This retains
known, partial and unknown values, including noncanonical public `V` fields
and wide `i128` inputs that truncate to zero. CACC and forget behavior remain
unchanged. An independent review and an old-branch reference differential
test accepted the equivalence.

Seven baseline/candidate pairs alternated run order and passed the existing
ColdFire, DSP and PCM hash assertions. Each completed 1,139 frames with zero
missing SPORT blocks. Every pair improved DSP wall, workload wall and
whole-process CPU. Median paired ratios were 0.978344 DSP wall (2.166% less),
0.978559 workload wall (2.144% less) and 0.986885 process user+system CPU
(1.311% less). The last measurement covers the whole test process, not DSP
thread CPU alone. Candidate median workload elapsed was 1.757835 s versus
1.793367 s baseline for the same 0.759333 s audio: still 2.315x too slow.
The profile family's 8.839% share was an investigation lead, not the gain.
The change is retained as a modest measured improvement, not an underrun fix.

Private records under `/private/tmp/dn2-astat-20261003/` preserve the actual
14-run order, executable SHA-256 values, stable generated-file inventory,
per-run workload/DSP wall and POSIX process CPU, and exactness logs.
`summary.json` contains raw observations; `paired-analysis.json` contains
computed ratios. No firmware/generated payloads or profiles are committed.

**[O]** Secondary host cleanup adds a session-validated `emu_audio_status`
endpoint for the periodic UI query, using nonblocking poll/report without
full CF diagnostics, firmware hashing or a DSP barrier. Explicit diagnostic
export retains its original synchronization. Bounded native pump-gap metrics
exclude load/pause/resume and queued input completion, including an input
enqueued during an outstanding step. They are omitted from WASM reports,
which do not share that native invalidation contract. Node checks passed
9/9; actor no-runtime, stale-session and CF-only-null checks passed. Independent
review found no residual correctness issue. Astro check had zero errors and
warnings (one style hint); frontend and native release builds passed. These
checks do not establish sustained live playback or a host scheduling speedup.

### Underrun work: measured native PGO candidate (2026-10-03)

**[O]** Profile-guided optimization of the integrated native root reduced DSP
wall time in all seven alternating control/use pairs. A copied Homebrew-built
baseline was rejected as a compiler-distribution confound; the accepted control
and candidate use pinned Rust 1.98.1 commit 48a229cea / LLVM 22.1.8, explicit
aarch64-apple-darwin target, identical source/generated core and release flags
except profile-use and missing-function warnings. Instrumented timings were
excluded. Four uniquely named QA profiles were merged; the separate audition
paths were not training inputs. All three previously hottest generated regions
have nonzero profile counts, and no missing-profile warning identifies a
generated image block. Untrained runtime/framework functions still warn.

Median paired use/control ratios were 0.791387 workload wall (20.861% less),
0.789116 DSP wall (21.088% less), 0.857616 process user+system CPU (14.238%
less), 0.923736 retired instructions (7.626% less) and 0.857875 elapsed cycles
(14.212% less). The hardware counts cover the entire process, including CF and
fixture setup, and cannot establish DSP-only IPC, CPU-core placement or an
instruction-cache bottleneck. Median workload elapsed was 1.754423 -> 1.386955 s
for exactly 0.759333 s audio: 20,775 -> 26,279 sample frames/s, still below the
48,000/s requirement and headroom needed for uninterrupted playback.

Every pair passed the fixed CF/DSP/PCM exactness assertions, 1,139 SPORT blocks
and zero missing blocks. An enhanced ignored audition test separately records
CF/DSP hashes and clocks, PCM hashes/counts and held-window timing after draining
prepress work. Short and delayed 1.2B-before-press/100M-held control/use runs
matched all those values, but both were silent. The optional nonzero gate
correctly failed on the delayed control, so these holdouts do not validate the
user's audible held-note scenario. No input-parameter sweep was used. Running
without link profiling also passed with optional timing absent. Independent
review found no residual issue in the test enhancement.

The profile and executables remain private under
`/private/tmp/dn2-pgo-20261003/`; `summary.json`, `bench.tsv` and `logs/bench-*`
record actual order, per-run measurements and artifact hashes. PGO has not yet
been installed in the normal launcher. These are native Mac measurements, not
Windows/WASM performance or a completed underrun fix.

**[O]** A matched-binary lookup in the existing marker-scoped DSP replay found
360 stack-slot-access samples across `r_1C399A`, `r_1C3862` and `r_1C364F`,
5.61% of the full 6,415-sample window. This supports a controlled register-
pressure experiment; it does not establish removable spill cost. Current cache
metadata lacks the original full-generation invocation. Available commands
produce materially different trees, so a cap comparison requires its own
reproducible regenerated baseline rather than silently treating the old cache
as a one-variable control.
