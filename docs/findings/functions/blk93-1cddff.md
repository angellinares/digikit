# blk93@0x1cddff — the "else" branch of blk93@0x1cdd8b, not a standalone function

- **Bounds**: `tools/sharcinv.py` records entry `0x1cddff`, exit `0x1cdecb`
  (91 instructions), with `tail_split_into: blk93@0x1cdecb` and a
  `boundary_note` saying the split is because `0x1cdecb` is itself an
  interior call target. That accounting is byte-range-correct but
  functionally misleading (see below): **[V]** for the 91-instruction byte
  span (cross-checked against `tools/sharcflow.py`'s return list — nearest
  prior return `0x1cddfa`/after `0x1cddff`, nearest following return
  `0x1cdf33`/after `0x1cdf38` — and against a fresh disassembly of the same
  span with `tools/sharc_disasm.py`, 91 instructions, no desync); **[C]** for
  treating `0x1cddff` as an independent function with its own entry and exit.
  It is not one. It is the out-of-line "else" body of **`blk93@0x1cdd8b`**
  (inventory: entry `0x1cdd8b`, exit `0x1cddff`, 55 instructions, label
  "block copy / move", caller `blk93@0x1c642a`). `0x1cdd8b` runs a
  register-save prologue, then at `0x1cddb5` a **Type 8a conditional JUMP**
  (`b=0` so JUMP not CALL, `cond=0x12` = GT, `j=1` delayed) tests
  `comp(F2, F1)` (set two instructions earlier at `0x1cddb2`, a
  `5a_move`-packaged float-compare) and, on GT, jumps to `0x1cddff`. The
  short (fall-through) path does a `leftz`/shift check and falls into a
  13-register restore-and-return sequence at `0x1cdde0`–`0x1cddfe`
  (`9b_abs` return at `0x1cddfa`). `0x1cddff`'s own last instruction, at
  `0x1cdec8`, is an **unconditional, non-delayed Type 8a JUMP** (`b=0`,
  `cond=0x1F`=TRUE, `j=0`) back to that same `0x1cdde0` restore sequence —
  so both branches of `0x1cdd8b` share one prologue and one epilogue, and
  `0x1cddff` is the long branch body, physically laid out after
  `0x1cdd8b`'s return purely as a cold-path relocation. **[V]** (target
  arithmetic validated against the already-documented `0x1cb4ee -> 0x1c06ba`
  call in `docs/findings/functions/blk93-1cb4b2.md` before trusting it here;
  the branch-into-`0x1cddff` site was found by extending that same Type 8a
  scan to `b=0` JUMPs, not just `b=1` CALLs).
- **Callers**: none call `0x1cddff` as a subroutine (0 of 51 Type 8a CALL
  sites in blk93 target it — the 51 CALLs are the 50-to-`0x1c06ba` + 1
  target already found for `0x1cb4b2`; also 0 `25a`/indirect calls). Its
  *only* control-flow entry is the conditional JUMP from `0x1cddb5` inside
  `blk93@0x1cdd8b`, which is itself called once, from `blk93@0x1c642a`
  (the render orchestrator — the same function that calls `0x1cdecb`; see
  "family" below). **[V]** for the exhaustive Type 8a scan (both CALL and
  JUMP, `b=0` and `b=1`) over blk93's full depth-filtered aligned
  instruction stream (22593 instructions); **[D]** that no other SHARC code
  block (blk1/blk69/blk88/etc.) reaches it — not checked, out of scope for
  a single-function note.
- **Callees**: none. No `25a`/Type 8a CALL, no indirect `9b_abs` call inside
  `0x1cddff`'s 91 instructions — every branch instruction in the span is
  the one exit JUMP at `0x1cdec8`. **[V]**
- **Label**: inventory label "envelope or gain", confidence 0.4, reason
  "float ALU ops dominate a moderate multiply count". Feature counts match
  exactly (float_alu 41, float_mul 18, mac 8, one literal hardware loop of
  14). The label's mechanism is closer to a **MAC-heavy polynomial/scale
  evaluation with clamp-to-[0,127] output**, not a simple gain multiply —
  see below. **[D]**

## What it computes

**[D]**, traced with `tools/sharc_trace.py --blob --start 0x1cddff
--concrete-memory --assume-32bit-normal-words --follow-loaded-calls
--summary` (stops after 44 steps at the first unmodelled multifunction
category, a known tracer gap — see Method) and cross-checked with a manual,
per-instruction decode driving `sharc_trace.py`'s own `_compute()` on
symbolic registers (scratchpad `pseudo2_1cddff.py`), which decodes every
instruction in the span except 7: 3 multifunction ops in unmodelled
categories `0x1a`/`0x1e`, and 4 single-function float-ALU ops with the
unmodelled opcode `0xda`. Both gaps are filled by hand from
`tools/sharcspec/compute_table.json` (built from the public ADI manuals,
never the vendor toolchain): `aluop_32_40bit` row `11011010` is
**`FN = float RX by RY`** (register-scaled fixed→float convert) and row
`11011001` is `RN = fix FX by RY` (the inverse); `multifn_mul_alu` row
`011010` is **`FM = F(0-3)*F(4-7), FA = float RXA by RYA`** and row `011110`
is **`FM = F(0-3)*F(4-7), FA = max(FXA, FYA)`**.

Counting all multiply+parallel-ALU forms (the 5 decoded
`float-mulalu-add`/`float-mulalu-subtract` pairs plus these 3 hand-filled
`0x1a`/`0x1e` multiply+max / multiply+scaled-convert pairs) accounts for
exactly the **8 MACs** the inventory vector reports. The function is a
chain of these MAC pairs interleaved with `float-min`/`float-max`/`trunc`
clamps and the scaled `float X by Y` / `fix X by Y` conversions, ending in
a 14-iteration literal hardware loop (`12a_imm` at `0x1cde80`, data=14,
`reladdr`=43 → loop body `0x1cde83`–`0x1cdea8` inclusive, i.e. exactly the
13 instructions before the loop-closing boundary at `0x1cdeab`) whose body
is itself two more MAC pairs, two `trunc`s, a `float-max`/`float-min` clamp
pair, and one `dm(I6, -6)` load (`15a`, `d=0`) — a fixed frame slot read
every iteration, not an address that advances with the loop.

This does **not** match any of the shapes already named in this engine:
- Not the `x <- x*(2-|x|)` polynomial envelope (no `abs` op anywhere in the
  span; that idiom's repeats are 3-4x identical short sequences, this loop
  body is 13 distinct instructions run 14x with two different MAC pairings).
- Not the one-pole gain-blend pair (0.8465/0.1534) or a sqrt(10)/+10dB
  ratio — those constants do not appear; the only float immediates loaded
  are 127.0 (twice, `R0`/`R12`), 1.0 (twice, `R13`, later `R12` reloaded),
  and 0.5 (twice, `R7`, `R15`), plus one plain 16-bit integer 127 (`R8`).
- Not a call to the shared reciprocal `0x1c06ba` or the `base^x` evaluator
  `blk88@0x1c0d68` — zero calls total.
- **Not related to `0x1cdecb`** (see "family" below) despite the physical
  adjacency the task flags: this function's only exit is the unconditional
  JUMP at `0x1cdec8` to `0x1cdde0`, and it never falls through into
  `0x1cdecb`.

Best reading: a **MAC-heavy, clamp-to-[0,127] scale/polynomial evaluation**
that produces one or more fixed-point-ish output values (via `trunc` after
clamping against the `127.0`/`127` constants), gated behind a float `GT`
compare in its parent (`0x1cdd8b`) rather than being unconditional. This is
closest in spirit to the task's "128-entry coefficient table scaled by 128,
clamped to 127" idiom (same clamp-to-127 output shape) but the trip count
here is a literal **14**, not 128, and no named coefficient table
(`0x256388`/`0x256588` etc.) is touched (see Memory) — so it is either a
different, smaller table, or the "scaled by 128" idea maps onto the
register-scaled `float X by Y`/`fix X by Y` conversions rather than a
literal ×128 multiply. **[O]** — not enough to name definitively; a second
reader should try to pin the exact recurrence (which register carries the
MAC chain's accumulator across iterations — `F1`/`F2`/`F10` are the most
reused) against a specific published gain-curve or coefficient formula
before this label is upgraded past [D].

## Structure

- **Entry** `0x1cddff`–`0x1cde50`ish: no register-save prologue of its own
  (it inherits one from `0x1cdd8b`, see "family"). Loads constants
  `127.0->R0/F0`, `127.0->R12/F12` (`17a`), `127(int)->R8` (`17b`,
  16-bit form, integer not float), `1.0->R13/F13` (`17a`), and a literal DM
  address `I3 = 0x2c73d0` (`17a`, ureg 19 = I3). Sets `MODE1` bit 21 (the
  bit this codebase's own tracer uses as the SIMD-mode flag, per
  `_predicate`'s `mode1.value & (1<<21)` check) with a Type 18a `bop=0`
  (SET) at `0x1cde4c` — and **clears the same bit** with `bop=1` (CLEAR) at
  `0x1cdec5`, two instructions before the exit jump. The function's entire
  body runs with this MODE1 bit set and it is restored before the shared
  epilogue. **[V]** for the bit position and set/clear pairing (decoded via
  `sharc_trace.py`'s own Type18a handler, `UREG_CODES["USTAT1"]+sreg` with
  `sreg=2` -> `MODE1`); **[O]** for what MODE1 bit 21 controls beyond this
  codebase's own SIMD-detection convention — not independently confirmed
  against the ADI manual bit table in this session.
- `I3` is modified once (`3b`, `I3 += M4` at `0x1cde1f`) but **never
  dereferenced** anywhere else in the 91 instructions — no `4a`/`15a`/`15b`
  in the span uses `i=3`. Either it is prepared for a consumer this note
  did not find, or it is dead by the time control leaves the function.
  **[O]**
- Straight-line MAC/clamp code from `0x1cde50` to `0x1cde7e` (about 20
  instructions): several `float-mulalu-add`/`-subtract` pairs and the two
  hand-filled `0x1a`/`0x1e` multiply+ALU pairs, `trunc`, `float-min`,
  `float-max`.
- **Loop** at `0x1cde80` (`12a_imm`, literal trip count **14**, not a
  register count — no `12a_ureg` anywhere in this function): body
  `0x1cde83`–`0x1cdea8`, 13 instructions, 2 MAC pairs + 2 `trunc` + a
  clamp pair + one `dm(I6,-6)` load. **[V]** (loop bounds computed the same
  way `sharc_trace.py`'s `_start_counted_loop` does: `end_sw = pc_sw +
  signed(reladdr,23)`, `start_sw = pc_sw + insn_length`).
- Post-loop straight-line code `0x1cdeab`–`0x1cdec3`: more MAC pairs,
  `float-add`/`float-subtract`, one `4b` instruction this session's decoder
  could not classify (field-access error, not a semantic gap — `4b`'s
  `compute` field layout differs from `4a`'s and the shared helper assumed
  the wrong one; worth a tools fix, out of scope here).
- **Exit** `0x1cdec5`–`0x1cdec8`: clear the MODE1 bit, then the
  unconditional non-delayed JUMP to `0x1cdde0`. No return instruction of
  its own.

## Memory

- 11 DM loads / 7 DM stores per the inventory vector; this session
  confirmed the shape (spill-style stores to an `I6`-relative frame, one
  fixed-offset frame load inside the loop) but did not walk every offset.
  **[D]**
- One literal DM address in the body: **`I3 = 0x2c73d0`**, loaded but never
  used as an address in this span (see above). This does **not** match any
  of the named tables (cosine `0x8055c440`/`0x8055c640`, the 1024-float
  pair, the 829-float exponential, `0x8045c3c0`, `0x26bb68`, the 128-float
  pair `0x256388`/`0x256588`, the context struct `0x252d3c`, or any audio
  ring) — it is either an unlabelled table or was meant for a consumer not
  in this span. **[O]**
- The other two `dm_0x2xxxxx`-range literals the inventory vector counts
  are **not addresses**: they are the `0x200000` (bit 21) mask used by the
  two Type 18a MODE1 set/clear instructions above, which only numerically
  resembles a DM address. **[V]**
- No touch of any audio ring (`0x262138`/`0x262938`/`0x263138`/`0x263938`)
  and no touch of the parameter frame (`0x2558dc` base,
  `0x2559b6 + track*0x60` per-track) anywhere in the 91 instructions.
  **[V]** (none of the literal loads or `I`-register bases land in either
  range).

## Interface

No evidence of the `R4`/`R8`/`R12` state-struct-pointer convention seen in
`blk93-1cb4b2.md` (`R4->I4`, `R8->I5`, `R12->I3`) — this function is not
independently called, so there is no caller-supplied argument set to
begin with. What is live on entry is whatever `blk93@0x1cdd8b`'s own
prologue left in place before the `0x1cddb5` branch: `F1`/`F2` (the GT
compare operands, set at `0x1cddb2` just before the branch), and the block
of registers `0x1cdd8b` spilled to its `I6` frame (`M3`, `I3`, `I5`, `R3`,
`R5`–`R7`, `R9`–`R11`, `R13`–`R15`) — restored by the shared epilogue this
function jumps into, not by code of its own. **[V]** for the register list
(read directly off the `15b` save/restore pair at `0x1cdd8b`'s prologue and
`0x1cdde0`'s epilogue, same offsets 109–121 on both sides); **[O]** for
what specifically feeds `F1`/`F2` before the branch — not traced back
further than `0x1cdd8b`'s own entry in this session.

## How is it entered (resolves the task's open question)

**[V]** Not a RAM-backed pointer-table target: the function's short-word
address `0x1cddff` does not appear anywhere in
`out/sections/dt2-1.16/section_7_BLOB.bin` (313500 bytes) as a 32-bit LE
byte address (`0x2839bbfe`), a 32-bit LE raw short-word value, or a bare
3-byte LE/BE short-word value — all three patterns searched, zero hits.
**[V]** Not called by any Type 8a CALL, `25a` CJUMP, or indirect `9b_abs`
call anywhere in blk93 (exhaustive scan, see "Callers"). **[V]** It *is*
reached, exactly once in the static image, by the Type 8a conditional JUMP
at `0x1cddb5` inside `blk93@0x1cdd8b`, itself called once from
`blk93@0x1c642a`. So the question "how is it entered" has a definite
answer for blk93: it is the cold branch of a function on the
`0x1c642a` render/dispatch call tree, not an independent, indirectly-called
routine.

## Family: is this stage 2 of the `FUN_1c71ec` wavetable pipeline?

**No.** `0x1cdecb` (stage 2, "gated block copy / linear-interpolation",
callers `blk93@0x1c642a` and `blk93@0x1c71ec` per the inventory) sits
immediately after `0x1cddff` in the byte stream only because nothing
between `0x1cddff`'s last instruction and `0x1cdecb` is a `9b_abs` return —
the boundary heuristic's `tail_split_into`/`interior_call_target` marks
are about *byte layout*, not control flow. This session's decode shows
`0x1cddff`'s only exit is the unconditional JUMP at `0x1cdec8` to
`0x1cdde0` (`0x1cdd8b`'s shared epilogue); execution never falls through
into `0x1cdecb`. The two functions share a common ancestor in the call
graph (`blk93@0x1c642a` calls both `0x1cdd8b`, whose branch reaches
`0x1cddff`, and `0x1cdecb` directly) but are otherwise unrelated siblings,
not pipeline stages of each other. **[V]**

## Open

- What `I3 = 0x2c73d0` is for (loaded, incremented once by `M4`, never
  dereferenced in this span).
- The exact MAC recurrence / which shape it evaluates (polynomial,
  coefficient scale, or something not yet named in this engine).
- What feeds `F1`/`F2` before the `comp(F2,F1)` GT test in `0x1cdd8b` that
  selects this branch, and what the short (fall-through) branch computes
  instead — both belong to a note on `blk93@0x1cdd8b`, not opened here.
- The `4b` instruction at `0x1cdec3` this session's decode helper could not
  classify (a tools bug, not a content gap).
- Whether any SHARC block outside blk93 reaches `0x1cddff` — not checked.
