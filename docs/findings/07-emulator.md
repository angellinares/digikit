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
