# Emulator

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
