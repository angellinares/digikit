# Handover 2026-09-24: machine selection, DN2 voices, then a custom DT2 sound

This file gives the current state and the next steps. Results are in
`docs/findings/`. Read `docs/findings/06-sharc-engine-and-startup.md` first,
from the section "The per-frame static chain" to the end.

## State

- Branch `work/machines-and-dsp-hooks`. All work is committed and pushed.
- The device runs OS 1.16. Targets are DT2 1.16 and DN2 1.11.
- The SHARC database is at `DB_VERSION` 11. Build it with
  `uv run python tools/sharcdb.py build out/sections/*/section_7_BLOB.bin`
  (about 15 s). The ColdFire databases come from Ghidra dumps:
  `uv run python tools/sharcdb.py import-ghidra out/ghidra/dt2-1.16-emac --name dt2-1.16-cf`.
- Function notes for the 160 DT2 audio-path functions are in
  `out/sharcdb/dt2-1.16.notes.sqlite` (not in git). `img.card(fn)` shows them.

## What is verified (finding 06)

- The audio task, its four commands, and the ring conversions (ring A out,
  rings B and D in, ring C to the 16-channel pack).
- The per-frame render: `FUN_1c2b24` -> `0x1c24e9` (per-track unpack) ->
  `FUN_1c642a` (per-slot dispatch, voices, effect stages) -> master
  `0x1c207b`.
- Machine selection on the DSP: selector = `0x2567c0[0x255970[track]]`, stored
  at slot record word 19 (`+0x4c`). The remap table is
  `[0, 1, 2, 3, 4, 0, 5]` for machine types 0-6.
- The voice record contract: 32 records at `0x2412cc`, stride `0x1d8` bytes;
  step, positions, phase, flag bytes, work buffer and declick bytes. See
  "The voice record contract".
- The interrupt vector table (loader block 70) and the SECI dispatch.

## How to work fast

- Ask the database first. In one script:
  `import sys; sys.path.insert(0, "tools"); import sharc; img = sharc.load("dt2-1.16")`,
  then `img.card`, `listing`, `callers`, `writers`, `readers`, `last_def`,
  `reach`, `refs`, `sql`, `trace`. Do not run `sharcfn.py` in a loop.
- For many functions, use a workflow: calibrate on 6 known functions with a
  strict scorer, then annotate callee-first, then summarise. Verify important
  claims with two independent skeptics (one checks bytes, one checks
  dataflow). Record only what both confirm as [V].
- Agents change tools in a scratch copy. Apply the tested files with `cp`,
  check them with `cmp`, bump `DB_VERSION` if the decode changes, rebuild,
  then commit. Run tests only when semantics change.
- Give agents the facts in the prompt. Tell them not to read finding 06 in
  full (it is long).

## Next 1: the ColdFire side of machine selection

Goal: know how a machine type goes from the user interface to the DSP
selector, so that a new machine can be selected.

Known:

- ColdFire machine dispatch at `0x400caf48`: types 0-5 index a table, 6 and
  higher use MANUAL SLICE (finding 02).
- `FUN_4002d438` copies the machine type (`src + 0xa2`) into the SRAM row
  `0x80003cd0 + track * 0x9a` (finding 04).
- The vector-191 handler (`0x4002dd0c`, call at `0x4002dd74`) builds the DSPI2
  frame with `FUN_400cd2bc`. The machine type is the frame word
  `0x94 + 2 * track`.
- On the DSP, `FUN_1c2b24` compares the live frame word with the cache
  `0x255970` at `0x1c33c1`. The cache has no literal writer.

Questions:

1. Who writes the DSP cache `0x255970[track]`, and from which frame word?
2. What does the DSP do with machine type 7 (the XSLICE clone from finding
   02)? The remap table has 7 entries; word 7 (`0x2567dc`) already belongs to
   another structure.
3. Which ColdFire code limits the machine list to 7 types, and what must
   change for an eighth type to reach the DSP as a new selector?

Tools: `sharc.load("dt2-1.16-cf")` (functions, calls, data references,
decompiled C; no control flow inside functions), `tools/ghidraq.py` for live
queries, `tools/machinecheck.py` in the emulator to read the TX frame.

## Next 2: the DN2 voice engine

Goal: map the Digitone II 1.11 voice engine (FM) as well as the DT2 one.

Known (finding 11):

- DN2 shares the DT2 effect stages (byte-identical or relocated) and the
  reciprocal/divide helper at `sw 0x1c06ba`.
- DN2 has the same dispatch shape: a large function `sw 0x1c8ef1` jumps into
  `sw 0x1c9b73` (`JUMP IF SZ` at `0x1c99a8`). Its stage A bound is 5, not 6.
- `sw 0x1c2712` has a 16-track loop (like `FUN_1c2b24`). `sw 0x1c9e76`
  matches DT2's `FUN_1c75d8` by relocation-tolerant hash.

Plan:

1. Find the DN2 audio task root and its reach set with `sharc.load("dn2-1.11")`.
2. Run the annotation workflow on that set. Calibrate on DN2 functions that
   match known DT2 functions (`img.match`).
3. Verify the voice record layout and the FM operator code with two
   skeptics per claim. Record the results in finding 11 or a new finding.

## Next 3: plan a custom DT2 sound

Do this with Em before any build work. Questions to agree:

- Which machine type or selector the new sound uses (see Next 1).
- Where the new render code is called. The render choice in `FUN_1c642a` is
  binary (`btgl` at `0x1c6ae8`, branch at `0x1c6aeb`, calls at `0x1c6af1` and
  `0x1c6b00`). The trigger array at `0x8055c840` cannot grow in place: stage
  B's table starts at its entry 6.
- What the new code must write: the voice record contract (64 floats at
  record `+4` for the decimator `0xb80000`; the amplitude envelope runs
  after).
- How to test it offline with `tools/sharc_trace.py` before any device test.
  A device test is Em's decision.

Candidate first sounds, simplest first:

1. A phase-driven oscillator (saw or pulse, band-limited with polyBLEP). It
   uses the existing Q31 pitch step as its phase increment, needs no sample
   memory, and its output is easy to predict and check in a trace.
2. A plucked string (Karplus-Strong): a noise burst into a short delay line
   with a lowpass in the feedback. Delay length comes from the pitch step. It
   needs a small per-voice buffer in free L2.
3. A noise percussion voice (hi-hat or snare style): filtered noise with its
   own short envelope. No pitch tracking is needed.

All three keep the rest of the chain: the amplitude envelope, the per-track
filter and effects, the sends and the master.

## Open items

- `0x1c4a31`'s caller: does it cancel the factor of 2 in the step?
- The decimator state range (`+0x104..+0x14b`, or only `+0x114..+0x14b`).
- The amplitude envelope array base (`0x24e8cc`, one reading only).
- Who writes the 32 source words at `0x24ef2c` (the calls to `0x1c7442` in
  boot are the lead).
- The 48-bit ISA decode of the IVT jump targets in `tools/sharc_isa.py` is
  wrong. The database reads the IVT from raw bytes instead.
- The tracer does not execute multiplier opcode `0x48` (fixed-point high
  word).
