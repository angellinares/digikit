# Handover 2026-09-22: emulator on current firmware, speed, and the machine goal

Supersedes the next-step sections of `HANDOVER-2026-09-21-sharc-dataflow.md`
and `HANDOVER-2026-09-20-machine-build.md`; read those only for history.
Results live in `docs/findings/`; this file carries state and next steps.

## The goal

Map how machines use the DSP so existing machines can be changed or improved,
and new ones built, on Digitakt II **1.16** and Digitone II **1.11**. 1.15C is
kept only as a reference. Two tiers:

- **Tier 1, no new DSP code.** A machine is a ColdFire-side parameter recipe:
  eight fixed engine slots, each meaning whatever the machine's descriptor says
  (`docs/findings/02-machines-and-parameters.md`, "A machine is eight fixed
  engine slots"). Slot 2 (mirror index 27) is used by no machine: an engine
  input nobody exposes. A clone plus a new recipe is the fastest path to a
  custom machine on the device.
- **Tier 2, new DSP behaviour.** Blocked on the hook, not on space: where in
  the SHARC render path new code could be called from.

Stop the `0x252658` writer hunt from the 09-21 handover. It never connected to
machines.

## Repository state

Branch `machine-ideas-menu`. Tests: `uv run --with pytest python -m pytest
tests -q` gives 749 passed, 6 skipped, about 4 s. Recent commits, newest first:

```
3f2a6b5 Add Digitone II 1.11 and a headless milestone check
89e8f60 Stop unblock faking semaphores the firmware posts itself
3944970 Keep a worker's completion semaphore out of unblock
fa11d1f Report eSDHC activity at the end of a guirun
7aaabbe Resolve the display semaphore on Digitakt II 1.16
554fd4a Let the emulator identify Digitakt II 1.16
0534090 Record 1.16 space budgets for a machine clone and new DSP code
```

This handover and the new section at the end of
`docs/findings/07-emulator.md` are not committed yet.

## Emulator: where it stands

DT2 1.16 and DN2 1.11 boot to the main screen, and both pass the milestone
check (`docs/findings/07-emulator.md`, last section).

```sh
# GUI (Em's usual command; no new snapshot needed)
.venv/bin/python3 -m emu.gui snapshots/Digitakt_II_OS1.16/boot400M.snap --syx Digitakt_II_OS1.16.syx

# Headless pass/fail, per firmware
uv run python tools/emucheck.py --device dt2 --syx Digitakt_II_OS1.16.syx \
    --sections out/sections/dt2-1.16 --snapshot snapshots/dt2-1.16/boot400M.snap --instrs 600000000
uv run python tools/emucheck.py --device dn2 --syx Digitone_II_OS1.11.syx \
    --sections out/sections/dn2-1.11 --snapshot snapshots/dn2-1.11/boot400M.snap --instrs 600000000
```

- Ladders: `snapshots/dt2-1.16/`, Em's identical `snapshots/Digitakt_II_OS1.16/`,
  `snapshots/dn2-1.11/` (new), all `boot{60,120,200,280,400}M.snap`.
- `sections/` holds whichever firmware was extracted last (currently 1.16).
  Always pass `DT2_SECTIONS=out/sections/<fw>` and the `.syx` explicitly, and
  check shas before trusting a run. Tests now read the pinned
  `out/sections/dt2-1.15C/` copy.
- Firmware shas: DT2 1.16 `278541e466edcd77d6b3e018a91fb90185932d3c7de224dd3e68294dddf3a9ec`;
  DN2 1.11 `2af43e65e3d8390b41c9f66222620f8cce027d73ed87db00c80b440f628472e0`.
- The emulated +Drive is formatted empty by the firmware on first boot. There
  are no samples, so a machine still cannot be *heard* in the emulator.

Open, in order of value:

1. **The never-fake list is hand-kept.** It came from a one-off scan
   (`scratch/semscan.py`, gitignored, using the Ghidra dumps). Move the scan
   into `emu/symbols.py` as a runtime rule: find every `pea IMM32; jsr give`
   or `give_b` site in the image and never fake a semaphore with a task-code
   poster. Then a new firmware needs no investigation. The one non-trivial
   part is telling ISR posters from task code without Ghidra; the vector table
   and the handler entry points are in the image.
2. The panel code mapping in `devices/*.toml` was measured on 1.15C. Re-check
   on 1.16 with `tools/panelsweep.py` if a button misbehaves.
3. Eight trace-only symbols are still fixed 1.15C addresses
   (`tests/test_symbol_audit.py` lists them). They matter only with
   `--trace-ui`.

## Task A: settle QEMU with numbers (small; do first, alongside B)

**What is already established.** `docs/findings/07-emulator.md`, "What
actually limits speed: our hook layer, not Unicorn" **[V]**:
- A hook-free m68k loop runs at **250.8M instructions/s** in this Unicorn,
  about real-hardware speed (200-264M/s).
- The firmware workload runs at 1.3-2.5M/s headless, and the GUI shows 5-12M/s.
  So the gap is two orders of magnitude, and it is in our layer.
- A cProfile of 10M instructions: 34% Unicorn executing, **~54% the Unicorn
  Python ctypes binding** (a buffer allocation per `mem_read`, an FFI call
  per `reg_write`), 12% our handlers. 1.02M of 10M instructions cross into
  Python.

**Today's A/B** (`out/speed-ab/report.md`, `results.json`) concluded that hooks
cost under 10% and QEMU is not warranted. Do not rely on its numbers:
- Its "floor" of **2.89M/s** was the `isa_faultmap_exceptions` configuration.
  That still dispatches exceptions in Python, used `count=` (1.84x by itself,
  per the finding above), and spent the window in the idle spin at
  `0x400cccd8`.
- Its own `strict` configuration, with no Python machinery, ran at **76.9M/s**
  and was set aside as "not meaningful".
- It did find one free fix: `emu/longrun.py build()` installs plain
  `setpixel`/`pxcopy` counter hooks at `profile.set_pixel`/`profile.px_copy`
  even when `bitmap=True` installs its own hooks at the same addresses. That is
  two Python crossings per pixel on the hottest path (about 1.3M per 100M
  instructions). Skip the counters when `bitmap=True`.

**What to do.** Use one metric that needs no instruction counting: **guest time
per wall second**, taken from PIT/DTIM tick counts. This is the "% of real
time" the GUI already prints. It avoids `count=` and compares configurations
that do the same work.

1. Apply the duplicate setPixel fix. Re-measure with the same snapshot,
   budget and shortcuts, 3 repeats each.
2. Count Python crossings by kind over a fixed guest-time window: code hooks,
   memory hooks, MMIO callbacks, exception and interrupt delivery, and idle-
   spin yields. Include `Machine` MMIO callbacks and exception dispatch, which
   the A/B's hook inventory may have missed. Get a cProfile split like the
   finding's (Unicorn / ctypes binding / handlers).
3. Take the top two crossing sources and cut them: batch MMIO reads, avoid a
   `mem_read` allocation per crossing (read into a reused buffer), replace
   per-instruction hooks with block hooks or single-address hooks, and move
   idle-spin handling so it doesn't cross on every pass. Report guest-time per
   wall second before and after.
4. As a direct comparison, not a port: `brew install qemu`, run a tiny
   bare-metal ColdFire loop on `qemu-system-m68k -M mcf5208evb` and the same
   loop in Unicorn, and compare MIPS. That gives QEMU's ColdFire ceiling on
   this machine in minutes.

**Decision rule.** QEMU is worth porting only if Unicorn's raw ceiling on this
workload is well below real time *and* QEMU's is well above it. If the gap is
crossings, fixing crossings keeps all the Python tooling: `addrtrace`,
watches, `unblock`, snapshots, symbol hooks. A QEMU port means rebuilding
~15 peripheral models in C and all of that tooling as TCG plugins. Record the
result in `docs/findings/07-emulator.md`, correcting the A/B's conclusion.

## Task B: a first custom machine (the main line)

State (`docs/PATCHING.md`, "Digitakt II 1.16"; `docs/findings/02-machines-and-parameters.md`):

- `tools/machinepatch.py` clones SLICE into a new eighth machine (type 7).
  Eight of its nine parts plan cleanly on 1.16: 484 bytes, 407 of them in
  `cave_b`.
- The ninth part, `clone`, **does not run on 1.16**. `CLONE_EQ_SITES` and
  `CLONE_MASK_SITE` hold 1.15C addresses and ignore `profile['clone_sites']`.
  `tools/machineprofile.py` `DT2_116['clone_sites']` already has the six
  correct 1.16 sites, byte-identical to 1.15C's. Wiring it is mechanical. Full
  clone: about 714 bytes, 952 of `cave_b`'s 16,647.
- 1.16 caves: `cave_a` `0x40311c14` (1,004 bytes) and `cave_b` `0x4031be5c`
  (16,647 bytes) are clean by the immediate scan and Ghidra references. The
  runtime check was weak then; it can be redone now that 1.16 reaches the
  main screen: `tools/memdump.py --range` after an `emucheck`-length run.
- `machinepatch` so far only patches a live emulator snapshot. It produces
  nothing flashable.

Steps:

1. Wire `clone` to `profile['clone_sites']`, and plan the full nine parts on
   1.16 with `plan_b` against `machineprofile.DT2_116`.
2. Redo the cave runtime check on 1.16 as above.
3. Make a flashable image. Apply the plan to a copy of
   `out/sections/dt2-1.16/section_3_MAIN_OS.bin` with `tools/patchimg.py`,
   then repack the container (`docs/findings/01-container-and-patching.md`:
   ELE3 format, integrity, the version gate).
4. Prove it in the emulator. Extract the patched image, boot it to
   `emucheck` PASS, then open machine select (FUNC+SRC; panel codes FUNC=17,
   SRC=2) and confirm the new machine appears and can be selected. A headless
   input run with `tools/guirun.py --input` plus `--png-at` gives
   deterministic evidence.
5. Give the machine something new, which is the reason to clone. Start with
   the machine-ideas menu the branch is named for: each machine's SRC-page
   parameters, the per-track TX-frame fields the SHARC reads
   (`docs/findings/04-coldfire-dsp-link.md`), and the unused slot 2. A first
   concrete idea is a SLICE or SAMPLE variant that exposes slot 2. Then
   characterise STRETCH (type 2), the natural base for a stretch variant; its
   `type == 2` sites have to be found the way SLICE's six were.
6. Hardware. The device is on 1.16. Flashing a patched OS is Em's decision;
   confirm the container and version-gate behaviour first.

A non-SLICE clone needs that machine's own `type == N` sites found. That is new
reverse engineering, not a table swap.

## Tier 2 leads (DSP), parked until Tier 1 lands

- **Space** (`docs/findings/06-sharc-engine-and-startup.md`, "Where new DSP
  code could live"): the L2 tail past the loader, about 922 KB, is the only
  large candidate, and weakly checked. The six-stage wavetable pipeline is
  2,956 bytes, so space is not the limit.
- **The hook**, in order:
  - Read the context-switch writers `FUN_00b85af7` and `FUN_00b85f6c`.
    `DM(0x2ca3e0)` is filled at runtime, and `blk69@0xb8853a` switches stacks
    with `I7 = I11 + 0x204`. This is the best lead for where the render loop
    runs.
  - Work forward from the machine type the SHARC caches (`DM(I5+0xc4)`,
    written at `0x1c33d2` in `FUN_001c2b24`), to every reader.
- **DSP tooling built on 09-21/22:**
  - `tools/sharcemu.py`: Ghidra p-code emulator; faults on missing semantics.
  - `tools/sharcwriters.py`: which stores can write an address.
  - `tools/sharc_worklist.py`: semantics coverage.
  - SLEIGH semantics cover 51% of the image; the next language work is the
    compute field.
  - Use the Ghidra project `~/ghidra-projects/sharc-dm-dt2-116`; the older
    SHARC projects use the old language. Pass DM data addresses as plain byte
    addresses. Loader byte address = on-chip byte address + `0x28000000`.

## Rules that bit this session

- **Fixed addresses fail silently.** A `Fixed(<1.15C address>)` in
  `emu/symbols.py` resolves to `None` on 1.16, and the harness quietly loses a
  behaviour. When 1.16 or 1.11 hangs where 1.15C did not, first compare
  `symbols.resolve()` across images. `tests/test_symbol_audit.py` now guards
  the used symbols.
- **Faked software semaphores cause hangs.** `unblock` faking a semaphore the
  firmware posts itself makes callers race ahead. Check `ev['satisfied_by_sem']`.
- **Speed A/Bs must do the same work.** Only trust one where both sides do the
  same work, and avoid `count=` in timing runs.
- **Verification rules.**
  - A second agent must check a finding against image bytes before it is
    marked `[V]`.
  - A null result from a method is not evidence until a positive control
    passes: a `stores` sweep once missed a byte-proven store.
- **Environment.** zsh, no `timeout`; use `uv run python` (bare `python3` lacks
  pyghidra). Run one JVM per Ghidra project.
