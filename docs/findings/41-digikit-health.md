# 41. DIGIKIT > HEALTH: CPU load, DSP block time, RAM (core/health)

Lane r1, 2026-10-17, native emulator (coupled daemon), all four images (DT2 1.16, 1.17, DN2 1.11, 1.12). Everything here is **[D]**: measured
here once, no second agent has checked it against the image bytes. **Nothing ran on a device, and emulator timing is not device timing:**
every number below proves a mechanism (the counter counts, the figures follow the load, the page refreshes), not a value of a Digitakt II or
Digitone II. Plan: `docs/plans/digikit-runtime.md` ("Later": health module). Menu: findings 28, 34, 38. Boot thread: finding 36 section 1. DSP
channel and record: finding 31.

## What the page shows

SETTINGS > DIGIKIT > HEALTH (third row, after MODULES and ABOUT). Eight rows of at most 18 characters (the list shows five, the rest scroll; the two STORE rows were added in Phase 8, below):

    CPU 35%               load of the CPU time the interrupts leave (below)
    DSP 50% MAX 51%       DSP block handler time per block, average and maximum of the last second
    RAM FREE 29.3 MB      free bytes of the firmware's 32 MiB heap
    LARGEST 16.0 MB       its largest free block
    CPU FREE 512 KB       the runtime's module arena (CPU memory)
    DSP FREE 320 KB       the DSP code area (core/dsp-link); `--` where an image has none
    STORE USED 2 KB       the module store: sectors the current header says are used (header, table, files), in KB
    STORE FREE 11.9 MB    what a new file may still take (the room for its table entry kept back); `--` for a card with no store

`--` replaces a figure that is not valid yet (the first windows after boot) or that the image cannot give; without `core/health` the page
adds `NO HEALTH MODULE`. A `>` before the free RAM means the free-list walk stopped at its bound (4096 blocks): the figure is a lower bound.

## How each figure is measured

**CPU: an idle counter.** The boot thread (DT2 `FUN_400cc864`, DN2 `FUN_400cec98`, the same code in both versions of each) creates every task
and ends in an empty loop (finding 36). A `hook` op replaces its last call (the `jsr` that creates the main-OS task; anchor `health.hook.idle`,
6 bytes) with a jump to `health_idle_hook`, which makes that call itself (`health.fw.create_main`, a `cf_ptr` anchor) and then loops
`addq.l #1,health_idle_line; bra.s`. The DSP block interrupt (1500 per second) closes a window every 1500 blocks and keeps the iterations
counted in it, so the time base is the audio clock. Load = 100 % - 100 * (window's iterations) / (the highest window since boot), valid from
the third window. **[D]** The idle context is the lowest priority: with a task that wants more than the CPU the counter reads 0 and the load 100 %
(table "burn" below).

The reference is the best window of this boot (the early windows, when the tasks are still blocked on the card), so the figure is the share of the
time the interrupts leave that the tasks use, not of the whole CPU: in the emulator a quiet main screen is 40.2 % idle by instruction count
(DN2) and 46.2 % (DT2) and reads 34 % and 20 %; the block interrupt handler and the firmware's own background work are the rest. **[O]** An
absolute calibration (idle iterations per second with nothing else running) needs the device: the loop's speed is a property of its memory and
caches. The page shows the relative figure and `DkrtHealth` carries `idle_win` and `idle_ref` for a later calibration.

The RTOS keeps no run-time statistics that I found: `task_create` (DT2 `0x400012c8`) stores a stack pointer at +0x48 and clears +0x4c and
+0x50 of the task record; what uses them was not traced **[O]**. The scheduler is a trap-0 handler (`0x40000410`) that rotates a ring and
reloads the PIT0 time slice. Per-task stack high-water marks were not added: the task table is a patch-time result (`tools/coldfire/task_stacks.py`),
not something the runtime can enumerate **[O]**.

**DSP: the DSP already measures itself.** The DSP's block handler (DT2 `FUN_1c74cd`, the function core/dsp-link hooks at its tail) reads
`EMUCLK` at its start (`0x1c74e9`) and again before its end (`0x1c75a9`) and stores the difference, rotated by 16 bits, in the first word of the
record it sends back to the ColdFire. The ColdFire has that record at the address the block interrupt handler pushes first (DT2 `0x8000488c`, DN2
`0x800053a4`; anchor `health.rec_lit`, the `pea` literal at +0x56 of the handler). `EMUCLK` is the 32-bit emulation clock counter that "counts clock
cycles" and can be read in user space (SHARC+ core manual `sc58x-2158x-prm`, "Emulation Cycle Counting", PDF page 291); the emulator
counts one tick per instruction. So **no DSP patch is needed**: a `hook` at the entry of the block interrupt handler (`health.hook.block`, 8 bytes:
`link.w; movem.l`, the same hashes core/dsp-link uses) calls `health_block`, which reads that word once per block and keeps the window's sum and
maximum. **[D]** The word is the DSP's: the module's window average and a direct read of the record agree (DT2 208 422 and 208 423, DN2 333 850
and 334 069 cycles), and they match the headroom counts of finding 06 (DT2 208 600, DN2 334 500 busy instructions per block). A percentage divides by
`DSP_BLOCK_CYCLES` = 666 667, one block at 1 GHz. The clock word in the start-up code decodes to 1 GHz **[D]** ([finding 100](100-module-limits.md); the write of the control register is not traced). The handler is the whole of the DSP's per-block work in the emulator
(the average equals the whole-DSP busy count); whether that holds on a device is **[O]**.

**RAM: the buddy allocator's free lists.** The C++ heap is a 32 MiB buddy allocator with 16-byte blocks and order 21 (finding 01; `operator new` =
`FUN_4011e674`, DT2 1.16, the same hash on all four images). Descriptor words (`0x402b406c` on DT2 1.16): `[0]` initialised, `[1]` base, `[2]` bitmap,
`[3]` the array of free-list heads (one block index per order, negative = empty), `[4]` highest order (21), `[5]` block count (`0x200000`), `[6]` live
allocations. A free block holds its previous index at +4 and its next at +8 (negative = none). `health_heap` walks the lists under the allocator's own
mutex (`FUN_40001608` lock, `FUN_4000172a` unlock) and sums `16 << order`; the largest is the highest non-empty order. Five literals are read from the
operator's code at run time (anchors `health.heap.{desc,mutex,lock,unlock}_lit`, offsets +0x1e, +0xe, +0x14, +0x92 in the function): no firmware address is
in the module. The walk runs only when the page refreshes (twice a second), is bounded to 4096 nodes (about 12 instructions each: 0.4 ms in the worst case,
with the mutex held, which other tasks only feel when they allocate; interrupts are not masked), and stops with a flag on a bad index. It covers the
firmware's heap only, not the other RAM (stacks, the boot-record arena, display and DMA buffers). **[O]** the cost of the mutex under contention on a
device. The arena and DSP-area figures are the runtime's own (`arena::stats`, `dsp::area_stats`).

## Interfaces

- `core/health` (`modules/core/adapters/health`, requires `core/runtime`): `health_read(HealthRaw *)`, `health_heap(HealthRaw *)`; exported state `health_hs`,
  `health_idle_line`, `health_off` (the proof's A/B switch).
- Runtime C ABI, additive (`dkrt-sdk`, docs/design/digikit-runtime.md): `void dkrt_health(DkrtHealth *)` (24 words; flags `HEALTH_MODULE/CPU/DSP/HEAP/
  HEAP_CUT/HEAP_BAD`), `u32 dkrt_blocks(void)` (DSP blocks since boot: a 1500 Hz clock, 0 without core/health), `void dkrt_poll_set(fn)` (the UI-loop tick calls
  `fn()` once per pass; one caller), mailbox command `CMD_HEALTH` (6) into the exported `DKRT_HEALTH`. The runtime links `health_read`/`health_heap` as optional
  symbols (0 when the module is not in the image). `RUNTIME_VERSION` stays 2.

## The page refreshes by itself

**[D]** No key press needed: the menu has no timer (finding 34), and I did not trace a timer or per-view update in the firmware, so the page uses the runtime's
UI-loop tick (`dkrt_tick`, once per pass of the UI task's event loop, about 32 passes per emulated second here: 0.24 per million instructions). Opening
the page registers `dk_poll` with `dkrt_poll_set`. `dk_poll` waits 750 DSP blocks (0.5 s, `dkrt_blocks`), then calls `dkrt_health`, rebuilds the six rows with the
firmware's own rebuild idiom (selection kept, `View::invalidate`) and counts `DK_REDRAWS`. When the page is closed it removes itself: the View's parent pointer at
View + 0x2c (the field `View::invalidate` tests) is 0 once the view has left the stack (read: 0x44f5de80 open, 0 closed). Without the block clock (no core/health)
it refreshes every 60 passes.

## Proof

`uv run python -m tools.coldfire.digikit_health_proof run --image IMAGE [--quick] [--no-cost]`: the four images, `--quick` (3 A/B pairs instead of 5), one coupled
daemon per image with the write watch (all of `cf.expansion` but the card buffers; the module's own code may store) and the flush check on. Everything is read from
guest memory, never from pixels (OLED PNGs go to `out/runs/lanes/r1-shots/` for a look). Results (JSON per image: `out/runs/lanes/r1-health/IMAGE.json`):

| check | dt2-1.16 | dn2-1.11 |
| --- | --- | --- |
| windows close, blocks = 1500 per window | 8 windows by the first read, exact | same |
| mailbox `CMD_HEALTH` figures: flags module, CPU, DSP, heap | all valid | all valid |
| independent walk of the free lists (this tool, over `peek32`): buddy invariants (order alignment, previous links, totals) | hold, 34 free blocks | hold, 13 free blocks |
| module's heap free / largest / free blocks equal the walk (allocation count equal before, after the command and after the walk) | 28 118 448 / 16 777 216 / 34 | 30 740 288 / 16 777 216 / 13 |
| arena and DSP area | 524 288 free of 524 288; 524 288 of 524 288 | 524 288; 327 680 of 327 680 |
| DSP count = word 0 of the record copy | 208 422 / 208 423 | 333 850 / 334 069 |
| page: refreshes with no key press | 3 to 7 redraws in 2 s | same |
| the firmware drew the view (its dirty flag consumed) | yes | yes |
| lines equal this tool's formatting of `DK_HEALTH` | `CPU 21%`, `DSP 31% MAX 31%`, `RAM FREE 26.8 MB`, `LARGEST 16.0 MB`, `CPU FREE 512 KB`, `DSP FREE 512 KB` | `CPU 35%`, `DSP 50% MAX 50%`, `RAM FREE 29.3 MB`, `LARGEST 16.0 MB`, `CPU FREE 512 KB`, `DSP FREE 320 KB` |
| leaving the page stops the polling (`DKRT_POLL` 0, no more redraws) | yes | yes |
| 0 stray writes over `cf.expansion`, 0 flush hits | 0 / 0 | 0 / 0 |

The 1.17 and 1.12 columns are the same to the digit in every figure above (the DSP blobs, the code and the anchors are the same). The tool needs `dsp-link`, `dyndsp`, `dynreg` in the image (the boot rig reads the voice rows); `core/health` does not need them.

**Idle versus a busy pattern.** Grid recording on, all sixteen TRIG keys pressed together on each of tracks 1 to 8, PLAY (every step of eight tracks trigging):

| | CPU (tenths of a percent, four windows) | DSP average / maximum (cycles) |
| --- | --- | --- |
| DT2 idle | 204, 206, 205, 212 | 208 422 / 208 423 |
| DT2 playing | 208, 209, 209, 208 (mean +0.2 points) | 203 082 / 214 431 (average -2.6 %, maximum +2.9 %) |
| DN2 idle | 342, 344, 343, 343 | 333 850 / 334 210 |
| DN2 playing | 347, 347, 347, 347 (+0.4 points) | 334 716 / 342 575 (average +0.3 %, maximum +2.5 %) |

The CPU rises by a small amount only: in the emulator a playing sequencer costs the ColdFire little next to the 1500 Hz block interrupt (the check is the mean
of four windows each, at least 0.1 point higher). The DSP maximum rises when voices trigger; the average on DT2 is *lower* while playing (voices that play cost less
than the idle path of a silent voice?) **[O]**, not investigated; finding 06 already found the DSP load nearly independent of the voice count. The page opened while
the pattern plays shows the same figures (`DSP 30% MAX 32%` on DT2, `DSP 50% MAX 51%` on DN2).

**Burn: the counter counts what the tasks leave.** The host writes a 18-byte routine into the runtime's staging buffer (`move.l #N,d0; subq.l #1,d0; bne.s; addq.l #1,counter; rts`)
and registers it as the poll function, so the UI task burns 3 N instructions once per pass. DN2 1.11 (idle share = 2 x iterations / instructions, over 2 s):

| N | burn share of the instructions | idle loop's share | load (page) |
| --- | --- | --- | --- |
| quiet | 0 | 40.2 % | 34.2 % |
| 150 000 | 10.9 % | 33.0 % | 46.2 % |
| 600 000 | 43.6 % | 11.1 % | 81.8 % |
| 2 000 000 (more than the CPU has) | 61.4 % (27 runs in 2 s) | 0 | 100 % |
| removed | | 40.2 % | 34.3 % |

The idle loop gives up at least half of what the routine takes (it gives up 7.3 of 10.9 points and 29.1 of 43.6: other low-priority work gives way as well, which I did not
trace **[O]**), reads 0 under overload (the idle context is the lowest priority, so it is the spare time and nothing else) and returns to its old rate. DT2 1.16 behaves the same
way: idle share 46.2 %, then 38.9 % (burn 10.9 %), 17.1 % (43.6 %), 0 (70.5 %, 31 runs), back to 46.2 %; load 20.4, 32.9, 70.6, 100, 20.4 %.

## Cost of the measurement

- **Idle loop:** none that a task could use: it runs only in spare time.
- **Block hook:** the stub (`lea; movem.l; jsr; movem.l; lea; jmp`: 6 instructions) and `health_block` (about 22 instructions on the usual path, 45 more once per second when it
  closes a window; `m68k-elf-objdump` of the object) = about 28 instructions per block = 42 000 per second = **0.03 %** of the emulator's 132 million instructions per second.
  Measured A/B (`health_off` poked 1 or 0, alternating 2-second windows, DN2 1.11): idle loop's share of the instructions 40.257 % with the hook, 40.278 % with `health_off`
  (the stub and the call still run, the body does not): the difference 0.02 points is inside the spread of the windows (0.024 points); DT2 46.23 % and 46.25 %. The A/B cannot resolve
  less than about 0.03 points, so the claim is "under 0.1 %", the instruction count gives 0.03 %.
- **Poll:** `dk_poll` runs once per UI pass (about 32 per second) and returns after a few instructions until half a second has passed; a refresh is `dkrt_health` (with the walk:
  about 12 instructions per free block, so a few hundred at 13 to 34 blocks), 6 menu items rebuilt by the firmware's own idiom, once per 0.5 s while the page is open. Nothing
  runs when the page is closed (the poll pointer is 0).
- **Size:** `core/health` is 710 bytes of code and 84 bytes of bss; the runtime grew by `dkrt_health`, `dkrt_blocks`, `dkrt_poll_set` and the optional links.
- **Emulator speed (not a device cost):** the emulator's idle fast-forward recognised only the stock `bra.b self` of the boot thread (`60 fe`), so with `core/health` the counting loop was interpreted and the DT2 cold boot took 107 s of wall time instead of about 25 s. **[C]** Fixed in the model: the fast-forward also recognises the counting loop by its instruction pattern (`addq.l #1,(counter).l; bra.s` back, `52 b9 <counter> 60 f8`, learned from the PC at chunk starts, so code placed at run time is found), skips whole passes (2 instructions each, up to the next timer or SSI event) and adds the skipped passes to the counter in memory and to the flags of the last `addq`. The result equals the interpreted loop exactly (unit test `counting_idle_loop_matches_reference`: CPU state, counter, guest clock). **[D]** DT2 1.16 with `core/health`, same machine, one run each: boot 107 s before, 41 s after; the proof's idle share (0.462) and load readings (204 / 329 / 706 / 1000 tenths of a percent, CPU 21 %) are identical before and after. Images without `core/health` are unchanged.

Runs (each image once, on the committed objects): `dt2-1.16`, `dt2-1.17`: 27 of 27 checks plus `--sites-only` (2 of 2); `dn2-1.11`, `dn2-1.12`: 29 of 29 (the site check is part of the run).
`--sites-only` patches the image without `core/health` and checks, from the patch plan's writes, that nothing touches the two hook sites, and that with the module exactly it does.
Tests: `tests/patchkit/test_health.py` (10: object current, anchors on all four ports, module.toml against the object, the runtime's exports, the idle counter's own cache line, the
formatting, the burn routine's bytes), `test_digikit_menu.py` and the runtime's two object tests still pass; `patchkit gen --check` and `pk_build build-all` are clean.

## Images without the hooks stay as they were

`core/health` is its own module: `core/runtime` only links its two functions as optional symbols and `core/dsp-link` is not touched (no DSP patch at all). An image patched without
`core/health` has neither hook; the runtime's added code (`dkrt_health` and two small functions) changes only the runtime object, not any firmware byte of such an image. The DIGIKIT
menu's new row and view change the menu's frames: `tests/golden/digikit_menu_frames.json` is regenerated (`digikit_menu_proof run --image IMAGE --quick --update` on all four; the
DIGIKIT main page has three rows now). That proof's boot part (APPLY & RESTART, safe mode) no longer started at this commit: its image lacked `core/dynreg` and `core/dyndsp`, and the boot rig
reads the voice rows (`no symbol core/dsp-voice:voices.rows`); the image is now built with the runtime set of `pk_dkrt_boot.RT`, and the boot part passes on all four.

## Open

- **[O]** The device. Whether EMUCLK counts cycles from the first instruction of the handler to its end as it does here, the real core clock (the DSP percentages scale with it), the idle
  loop's real speed (an absolute CPU calibration), the cost of the mutex walk on the real ColdFire.
- **[O]** The reference of the CPU figure is the best window of the boot; a device that is never quiet reads low. An absolute calibration would need the idle loop's speed alone.
- **[O]** DT2: the DSP average falls while a pattern plays. Per-task stack marks (the task table is not enumerable at run time). Why the burn displaces more than the idle loop (other low-priority work).
- **[O]** One poll function only (`dkrt_poll_set`); a second user needs a list. The page's own stack use was not measured separately (it is the chain of `fill_about` plus `dkrt_health` and
  `health_heap`, about 200 bytes more).
- **[O]** The firmware's own timer or per-view update was not looked for beyond the UI event loop; the UI-loop tick is what the page uses.

## The module store's used and free space (lane r4, Phase 8 Safety, 2026-10-18) **[D]**

`DkrtHealth` grew at its end by `store_total`, `store_used`, `store_free` (bytes) and the flag `HEALTH_STORE` (64); its `version` is 2. The module
`Api` that modules see is unchanged, so `RUNTIME_VERSION` stays 2 (the struct is read by the menu, which is built into the same image as the
runtime). The runtime fills them from the two header sectors alone (`store::peek_header`: the usable copy with the higher `seq`, no table read), and
reads them **once**, again only after an upload wrote a header sector (`STORE_DIRTY`, set by `wr_header`): `store_total` is the sectors a store may use (`LIMIT`, up to header B,
12,581,376 bytes), `store_used` the header's `used_sectors`, `store_free` = `dkrt_sdk::store::free_bytes(used, count)`, the same formula as the
upload's INFO. **[C]** The first version read the two sectors on every refresh (twice a second): the proof's own check "CPU load is higher while a busy pattern plays" then failed deterministically (idle windows 204, 216, 207, 213 against busy 208, 210, 210, 209 tenths of a percent; before and after the cache 204, 207, 206, 212 against 208, 209, 209, 208), because the card reads of the page's own refresh are CPU time of the UI task that the page then reports as load. A page that shows a load must not read the card to show it. A card with no store has no usable header: the flag is clear and both rows read `--`. The menu page now has eight rows
(`DK_LINES` has nine lines for the `NO HEALTH MODULE` note); `STORE USED 2 KB` and `STORE FREE 11.9 MB` fit the 18-character row (OLED frame of the scrolled
page checked by eye: the longest row, `STORE FREE 11.9 MB`, is not clipped).

Proof (`tools.coldfire.digikit_health_proof`, dt2-1.17): the card has one 700-byte file (header 1 + table 1 + data 2 = 4 sectors): the check
`store figures` reads `used` 2,048, `total` 12,581,376 and `free` 12,578,816 from `DKRT_HEALTH` and expects exactly those, and the page's lines
equal the proof's own formatting (`STORE USED 2 KB`, `STORE FREE 11.9 MB`). No menu frame is pinned for HEALTH, so `tests/golden/digikit_menu_frames.json`
did not change (the pinned frames are the other pages).
