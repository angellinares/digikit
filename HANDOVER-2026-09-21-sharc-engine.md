# Handover 2026-09-21: reading the SHARC audio engine

Target is **DT2 1.16 only**. MAIN OS `out/sections/dt2-1.16/section_3_MAIN_OS.bin`
sha `57bb4dfa…e008e7d`; SHARC `out/sections/dt2-1.16/section_7_BLOB.bin` sha
`0f514a12…737fffa2`. Results live in `docs/findings/` (indexed from
`docs/FINDINGS.md`); per-function notes in `docs/findings/functions/`.
This file is state and next steps only.

## Where this stands

The goal is a custom track machine. The ColdFire side is fully mapped; the work
is now **reading the SHARC's DSP library**, because that is what a machine is
built from.

**How much is left.** Of 1229 SHARC functions, only **170** do any float
compute, and the real audio engine is **51** (≥10 float mul/MAC and ≥60
instructions). **21 are read — 41%. 30 remain.** The other ~1060 are copy, glue,
driver and DMA. This is a finite, nearly-half-done job, not an open field.

## The engine model

A machine is a **ColdFire-side parameter recipe** over a shared SHARC library.
Descriptor field *n* always maps to the same mirror index, so a machine picks
what each of eight fixed slots *means*, not where it goes. Both descriptor tables
and all 275 parameter descriptors are extracted (`tools/machinedescr.py`,
`tools/paramtable.py`).

Parameters reach the DSP: mirror index → `0x8000dd40` smoothing → `0x80005b50` →
TX frame `0x80005348` → **SHARC DM `0x2558dc`**, byte-identical (all eleven
per-track scalar offsets match). Per-track parameter block at
`0x2559b6 + track*0x60`; SRC page at block bytes `0x00`–`0x13`.

**Named library primitives so far:** shared reciprocal `0x1c06ba` (RECIPS + 3
Newton-Raphson, 30 callers) · `base^x` evaluator `blk88@0x1c0d68` (`R4` selects
2.0 pitch / 10.0 dB / π) · log primitive `0x1c1284` (`ln(10)`) · polynomial
envelope `x ← x·(2−|x|)` · one-pole gain smoothing (0.8465/0.1534, √10) ·
two-tap interpolated table lookup · **6-tap polyphase resampler** (64-bit phase
accumulator) · **note→frequency** `blk93@0x1cbe19` (12-TET, `f = 220·2^((n−69)/12)`,
clamped 20–22000 Hz). Sample rate is **96 kHz**.

**Render chain:** `FUN_1c2b24` (per-frame orchestrator) → 32 per-track calls to
`0x1c24e9`, then `0x1c642a` → `0x1c18a6` → `0x1c207b` → `0x1c14e7`, threading a
shared context pointer `0x252d3c`. The resampler hangs off `0x1c642a` via
`0x1c4ecf`.

**Settled negatives** (do not re-litigate): there is **no FFT** anywhere — every
checklist item fails across all 13 dual-add/subtract functions, and there is no
bit-reversed addressing in the image. `FUN_400cec70` on the ColdFire is a
debug-console player. The `DAT_4031b264` node list is USB audio. CFADE reaches
the DSP at frame `+0xde` but nothing reads it.

## Next steps

1. **Add opcode tests.** The last opcode pass (`Type10a_rel`, ALU `0xe0`,
   multifunction `0x1a`/`0x1e`/`0x1f`, the `…by RY` convert family, ALU
   `0x05`/`0x06`, Type 2b) is manual-cited and the suite passes, but **no
   opcode-specific tests were added** — the agent ran out of time. Fill this in
   the style of `FloatComputeTest` before the next opcode pass.
2. **Read the remaining 30.** Use the pre-generated dossiers; start with
   `0x1cbdea` (the unread third of three back-to-back orchestrator calls) and
   `0x1c4ecf` (the resampler's real entry). One agent per function, each writing
   `docs/findings/functions/<block>-<addr>.md` and returning ≤15 lines — this
   keeps the main context from becoming the bottleneck.
3. **Find what consumes the SRC page.** Still open, and it is the last link from
   a knob to the audio. `I4` in the resampler was tested and **refuted** (every
   per-track block starts at frame byte `0xda` ≡ 2 mod 4, so a 4-aligned read can
   never hit TUNE). A per-voice DSP struct is the better hypothesis.
4. **Build a content-loaded 1.16 snapshot.** Every remaining question is a
   runtime question. `snapshots/dt2-1.16/` is cold-boot only, so nothing carries
   live parameter data.
5. **If a deliverable is wanted sooner:** exposing CFADE is scoped at two
   length-preserving 6-byte `jmp` edits plus a 32-byte cave stub
   (`docs/findings/02-machines-and-parameters.md`). Every gate but one is
   satisfied; the open one is whether the DSP does anything with it.

## Tools

`tools/sharcfn.py <blob> <addr>` — **start here**; one-command function dossier
(bounds with both hazards flagged, call graph, annotated listing with resolved
operands and named regions, feature vector). `tools/sharcinv.py` — inventory and
feature vectors for all 1229. `tools/sharcflow.py` — calls/returns. `tools/sharc_trace.py`
— symbolic tracer, now models float compute. `tools/sharcldr.py` — blocks, RAM vs ROM.

Tests: `uv run --with pytest python -m pytest tests -q` (593 passed, 5 skipped).

## Traps that have cost real time

- **Address spaces differ by block.** blk93/blk1/blk88 use `byte = 0x28000000 | (2*sw)`.
  **blk69 does not** — target `0x20000000`, `base_sw = 0xb80000`.
- **Immediate scaling is per instruction form.** `19a` adds raw bytes;
  `19a_scaled` scales by normal-word; `dm(K,I)` scales K by **4** under 32-bit
  normal words; M-indexed scales by 2 if short-word tagged else 4.
- **Boundaries lie two ways.** A call target can sit inside another span; and a
  span can contain a branch target that is *not* a function (`0x1cddff`). A
  function's live exit can jump **backward** into a preceding span's epilogue
  (`0x1c4f81`).
- **Decoder gaps cause wrong conclusions, not just gaps.** Type 8a calls were
  invisible until this session and made 51 blk93 calls vanish, which produced
  several false "no caller" claims. Still open: `cu=2` op `0x10`, `cu=1` op
  `0x48`, form `1a`, Type 9b indirect targets, shifter `0xb0` (absent from both
  manuals).
- **An empty result is not proof.** Say what was scanned and what the method
  misses. A `[V]` needs a second agent's byte-check.
- blk69 and blk93 are **one library** — live cross-block calls, not shared bytes.

## Repo state

Branch `machine-ideas-menu`, ahead of `main`, all work committed. Firmware and
derivatives are never committed. `docs/findings/functions/` has 12 notes.

**Untracked, foreign — leave alone:** `scratch/`,
`docs/refs/dspi2-edma-blocker-and-register-sources.md`.

**Note:** `Untitled.mmon` and `Untitled2.mmon` were present at session start and
are gone; they were untracked so git cannot restore them. Cause unknown — flagged
to Em.
