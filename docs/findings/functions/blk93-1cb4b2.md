# blk93@0x1cb4b2 — envelope/coefficient-table refresh: polynomial saturator (4x) + shared reciprocal call

- **Bounds**: entry `0x1cb4b2`, exit `0x1cb647` (return `9b_abs` at `0x1cb642`,
  delay slots `15b` load + `25c_rframe` at `0x1cb644`/`0x1cb646`). Matches
  `tools/sharcinv.py`'s inventory entry exactly (`entry=1881266`,
  `exit=1881671`, `n_insns=183`). **[V]** — cross-checked against
  `tools/sharcflow.py`'s return list on `blk93.bin` (`sharcflow.py` return at
  `sw=0x1cb4ad, after=0x1cb4b2` closes the previous function — this is
  `0x1cb3d8`, stage 3 of `FUN_1c71ec` — and the next return
  `sw=0x1cb642, after=0x1cb647` closes this one, with no call target landing
  inside the span).
- **Callers**: none found — see "How is it entered" below; this is **not**
  fully settled, only checked exhaustively for Type 8a calls within blk93.
  **Callees**: **`0x1c06ba`** (blk88), called once, from `0x1cb4ee`. **[V]**
  — see "Method-gap correction" below; the inventory's `callees: []` for this
  function is wrong.
- **Label**: inventory says "envelope or gain", confidence 0.4, reason "float
  ALU ops dominate a moderate multiply count -- polynomial/ramp shape". Agrees
  with what was found, but the function is closer to a **coefficient/envelope
  table refresh** than a single gain multiply: it can write one value or loop
  to write several, and it opens with a call to a shared math primitive, not
  a table lookup.

## Method-gap correction: `tools/sharcflow.py` and `sharcinv.py` do not see Type 8a `CALL`

The call at `0x1cb4ee` decodes as `8a_rel` with fields
`b=1, a=0, j=1, ci=0` — SHARC+ Core Programming Reference, "Type 8a
ISA/VISA (cond + branch)" JUMP/CALL encode table (`out/refs/sharc-plus-prm`,
page 357 / `all.txt:18829`): `1 0 0 0 = call ADDR (Type 8a) (db)`. This is a
genuine hardware delayed CALL (return address pushed to the PC stack), not
the compiler's manual push-to-`(I7,M7)`-plus-`16a`-store convention that
`sharcflow.py`'s docstring and detector are built around. `sharcflow.py`
only recognises `25a_direct`/`25a_pcrel` (CJUMP) and `9b_abs` (indirect,
`M5`) as calls, so **every Type 8a `CALL` in the image is invisible to it**,
and therefore to `sharcinv.py`'s `calls`/`callees`/`callers` counts too.

Decoding all `8a_rel`/`8a_abs` instructions in `blk93.bin` with `b=1`
(`base_sw=0x1c13e6`) finds **51 real CALL sites**, none of them in
`sharcflow.py`'s `calls` list. **50 of the 51 target `0x1c06ba`**, one
targets `0x1c0f26`. `0x1c06ba` is therefore not a leaf with "no static
caller" (as `sharcinv.py` has it) but one of the most-called routines in the
block. One of the 50 call sites, `0x1cb3ff`, is inside the *already
documented* stage-3 envelope (`0x1cb3d8`, docs/findings/06, "on-the-fly
polynomial envelope") — so stage 3 and this function share the same callee.
`tools/sharc_trace.py`'s tracer (`_field(f, "b")` check, `tools/sharc_trace.py`
around the Type 8/9/25 handling) already models the `b` bit correctly; only
the static call-graph tools miss it. This is worth fixing in
`sharcflow.py`/`sharcinv.py`, but that is out of scope for this note (no
other `docs/` file or tool was edited here).

## What `0x1c06ba` computes (context for this function's callee)

`0x1c06ba`–`~0x1c06fa`, labelled "IIR or recurrence" by the inventory
(confidence 0.5) — **that label is wrong**. Traced with
`tools/sharc_trace.py --start 0x1c06ba`: `float-recips-seed` (RECIPS) on
entry, then three rounds of `float-multiply` + `float-mulalu-subtract`
computing `2.0 - D*R` and multiplying it back in, before returning. This is
the ADI-documented **iterative floating-point reciprocal** idiom verbatim
(SHARC+ Core Programming Reference, "32-bit and 40-bit Operations",
`all.txt:24791`–`24812`: `F0=RECIPS F12` seed, then repeated
`F12=F0*F12; F0=F11-F12` refinement, `F11=2.0`). **[D]** (traced once, not
independently re-verified). It is a shared `1/x` subroutine, not envelope
state.

## What this function computes

**[D]** — traced with `--concrete-memory --assume-32bit-normal-words
--continue-external-calls`; 8 branch paths explored, several reach the
return cleanly (`return without followed call`), two stop on a data-dependent
hardware loop count (expected, see below).

1. **Prologue** (`0x1cb4b2`–`0x1cb4d4`-ish): `I7 -= 72` (`19a_scaled`), saves
   `I3` and `I5` to the `I6` frame, then pushes `R2,R3,R5,R6,R7,R9,R10,R11,
   R13,R14,R15` to `I6`-relative slots (11 `15b` stores) — a normal
   callee-saves prologue on a compiler-managed `I6` frame. Register
   interface (**[V]** from the trace): **`R4` -> `I4`** (state-struct
   pointer, fields read/written at byte offsets `+8,+16,+20,+24,+44,+48,
   +52`), **`R8` -> `I5`** (a second array cursor, walked later as
   `I5 + M6*4` post-modify), **`R12` -> `I3`** (copied in the CALL's own
   delay slot at `0x1cb4f1`; an output-array cursor, written as
   `I3 + M6*4` post-modify). This matches the template's "R4/R8/R12" register
   convention.

2. **State setup** (`0x1cb4d8`–`0x1cb4eb`): `I4+8` (an integer field) is
   `float-convert`ed then conditionally corrected by `+2^32`
   (`R1 = 0x4F800000`, added under `cond=1`) — **the identical "unsigned
   int -> float via +2^32 bias" idiom used by stage 5/6's phase accumulators**
   (docs/findings/06, table base `0x26bb68`). `I4+20` is clamped with
   `float-max` against `1.0` (`R5=0x3F800000`); `I4+48` is clamped with
   `float-min`. `I4`'s field 0 (`*I4`) is copied into `I4+20`, and a
   `float-subtract` (`R10`) is computed alongside.

3. **Call to the shared reciprocal** at `0x1cb4ee` (see above). Its delay
   slots (`0x1cb4f1`, `0x1cb4f4`) run before the callee starts: the second
   one writes `R4 = 0x3F800000` (**1.0**) unconditionally — this precedes the
   function's first branch, so on every dynamic call to `0x1c06ba` from here
   the reciprocal's operand is the literal `1.0` (`1/1.0`, a fixed-point of
   the Newton step). Whether this is a deliberate identity/default path or
   whether the "real" input is a different operand than the one this trace
   attributes to `R4` is **[O]** — I could not fully decode the raw compute
   field of the packed `5a_move` at `0x1cb4ee`'s neighbour to be certain
   which physical register RECIPS reads.

4. **Two envelope-value paths**, chosen by a bit-test (`0x1cb50a`
   `bit-test`, branch at `0x1cb50c`) after a `leftz`-based early-exit guard
   (`0x1cb4fd` `leftz`, branch-not-taken at `0x1cb500` to the epilogue at
   `0x1cb642` — the same "guarded by a zero/leadz test, early return" shape
   noted for stage 4, `0x1cd286`, in docs/findings/06):
   - **Simple path** (`0x1cb50f`–`0x1cb53b`): walks `*R8` (post-modify) once,
     one `float-add`, one `float-multiply`, one `float-clip`, then the
     **polynomial saturator `x <- x*(2-|x|)` applied four times**
     (`float-abs`/`float-subtract`/`float-multiply` triples at `0x1cb521`,
     `0x1cb526`, `0x1cb52b`, `0x1cb530`), then one more
     subtract/multiply/multiply/add, stored through `R12` post-modify, and a
     `compare` that can branch straight to the epilogue.
     **This is the same primitive as stage 3's envelope (`0x1cb3d8`,
     iterates 3x)**, run one iteration deeper (4x) here — same family, not
     the same function.
   - **Richer path** (`0x1cb53e`–`0x1cb62a`, reached either by the bit-test
     or by falling through the simple path): walks a second array through
     `I5 + M6*4` (post-modify), combining loaded values with **six
     `float-mulalu-add`/`float-mulalu-subtract` dual ops** (`0x1cb553`,
     `0x1cb564`, `0x1cb56c`, `0x1cb575`, `0x1cb589`, `0x1cb606`) — the
     "dual add/subtract" multiply-combine network the task brief flags as a
     known shape elsewhere in the engine, here with 6 dual ops rather than
     5. It contains a **register-counted hardware loop**:
     `0x1cb5b3`/(pc `0x1cb5a9` region), form **`12a_ureg`**, `LCNTR = R6`
     (`ureg[6:0]=6`, `mode=1`) — **the trip count is a register value, not a
     literal** (no `12a_imm` form seen), so it is data-dependent and could
     not be resolved statically. The loop body (`~0x1cb5b6`–`0x1cb5fc`)
     applies the same dual-MAC network per iteration. After the loop, the
     **same 4x `x <- x*(2-|x|)` saturator runs a second time**
     (`0x1cb606`–`0x1cb61e`), then a final add and a store through
     `I3 + M6*4` post-modify (one of `0x1cb603`, `0x1cb61f`+`0x1cb629`
     depending on a branch at `0x1cb625`).

5. **Epilogue** (`0x1cb62a`–`0x1cb646`): pops `I12` from `(I6, M7*4)`
   pre-modify (restoring the call-frame index register), then `I3`, `I5`,
   and the 11 saved data registers from the `I6` frame, then `9b_abs`
   return + `25c_rframe`.

**Net reading**: this is not a single "apply gain" op. It is a **state-driven
envelope/coefficient generator**: it normalises a count field with the
phase-accumulator float-convert idiom, calls the shared reciprocal, then
either produces one saturated value (4x `x*(2-|x|)`, same primitive as the
already-documented stage-3 envelope) or, when a flag/array walk demands it,
refreshes a whole run of values through a register-counted loop that blends
a second input array via six dual-MAC ops before saturating again. If a
second opinion is wanted, the open item to chase is exactly which physical
registers back the `5a_move`/`2a_short` compute fields — this note trusts
`tools/sharc_trace.py`'s operation naming and result-register attribution,
not an independent raw-bit decode of every instruction.

## Structure summary

| Region | What |
|---|---|
| `0x1cb4b2`-`0x1cb4d4` | prologue: frame, 11-register push |
| `0x1cb4d8`-`0x1cb4eb` | state setup: +2^32 float-convert, min/max clamp |
| `0x1cb4ee` | **CALL `0x1c06ba`** (Type 8a, `b=1`) — shared `1/x` |
| `0x1cb4fd`-`0x1cb500` | `leftz` guard, early-exit branch to epilogue |
| `0x1cb50a`-`0x1cb53b` | simple path: 4x `x*(2-\|x\|)`, store via `R12`/`I3` |
| `0x1cb53e`-`0x1cb5a9` | richer path setup: `I5+M6*4` walk, dual-MAC network |
| `0x1cb5a9`/`0x1cb5b3` | **register loop** `12a_ureg`, `LCNTR=R6` |
| `0x1cb5b6`-`0x1cb5fc` | loop body: dual-MAC network |
| `0x1cb602`-`0x1cb62a` | second 4x `x*(2-\|x\|)`, final store via `I3+M6*4` |
| `0x1cb62a`-`0x1cb647` | epilogue, pop registers, return |

## Memory

No named table (`0x8055c440`/`0x8055c640` cosine, `0x8055c890`/`0x8055d890`,
`0x8045a6c8`, `0x8045c3c0`, `0x26bb68`, `0x256388`/`0x256588`, or the audio
rings `0x262138`/`0x262938`/`0x263138`/`0x263938`) is touched by a **literal**
address anywhere in this function — every load/store is through the
register-supplied cursors `I4` (from `R4`), `I5` (from `R8`) and `I3` (from
`R12`), all symbolic in a callerless trace. The only literal immediates
loaded are floats: `0x4F800000` (2^32, unsigned-int-to-float bias),
`0x3F800000` (1.0, used twice: as a clamp bound and as the reciprocal call's
argument), `0x40000000` (2.0, the saturator's constant). **[O]** — table
identity is undetermined without a concrete caller providing `R4`/`R8`/`R12`.

The parameter frame (`0x2558dc` base, `0x2559b6 + track*0x60` per-track,
SRC page bytes `0x00`-`0x13`) is **not** read by literal address either,
consistent with docs/findings/06's observation that the wavetable engine's
stage routines all take pointers as arguments rather than reading the frame
directly. `I4`'s touched offsets (`8,16,20,24,44,48,52`) exceed the 20-byte
SRC page window, so even if `I4` turned out to alias a per-track block, this
would be reading past the SRC page into other per-track fields — SRC page's
consumer is still **[O]**.

## Interface

- `R4` (arg) -> `I4`: state-struct pointer. Fields touched: `+8` (uint,
  converted to float with the 2^32 bias), `+16`, `+20` (max-clamped to 1.0,
  also overwritten from field 0), `+24`, `+44`, `+48` (min-clamped), `+52`.
  State persists back into this struct across calls — matches the "state
  persisted to a caller struct" shape from the task brief, here with more
  than two fields.
- `R8` (arg) -> `I5`: second array cursor, read-only in the traced path
  (`I5 + M6*4` post-modify loads).
- `R12` (arg) -> `I3` (copied inside the CALL's delay slot): output-array
  cursor, `I3 + M6*4` post-modify stores (one or two per invocation
  depending on which path runs).
- Return value: none observed written to a return register in either
  explored path; all outputs go through the `I3`/`I4` pointers.

## Open

- **No static caller confirmed only for blk93.** All 51 Type 8a `CALL`
  sites and the existing `25a`/`9b_abs` lists in `blk93.bin` were checked
  against this function's address (`0x1cb4b2`) and none match. A raw
  byte/short-word scan of the whole `section_7_BLOB.bin`
  (`sha256 0f514a12a2255f5c081e292c47f1f29462003177658da4bbae0a22fd737fffa2`)
  for `0x001cb4b2` and `0x28000000 | (2*0x1cb4b2) = 0x28396964`, both
  endiannesses, found **zero hits** — the address does not appear as a
  literal 32-bit value anywhere in the file, which rules out a simple
  absolute function-pointer table entry but says nothing about a
  PC-relative `CALL` from a block outside blk93 (blk1, blk88, blk69), which
  was not scanned. The RAM-backed `JUMP(M13,I12)` dispatch at `0x254d98`
  (docs/findings/06) reads a table populated at runtime, not a literal in
  this static image, so a static scan cannot rule it in or out either.
- Which physical register RECIPS actually reads at `0x1c06ba` (assumed
  `R4`/`F4` here, from the paired `ureg-copy R4->R1` and the `F2*F4` term in
  the first refinement step) is inferred, not bit-decoded.
- The two-path split (`bit-test` at `0x1cb50a`) and the early-exit guard
  (`leftz` at `0x1cb4fd`) are read from the trace's operation names; what
  specific state-struct field or flag they test is not identified.
- The `12a_ureg` loop's real dynamic trip count (register `R6`) was not
  resolved — its provenance earlier in the function (a `decrement` at
  `0x1cb55a`/`0x1cb5a9`) suggests it counts down from a value derived from
  the state struct, but which field was not traced back further.
- `sharc_trace.py`'s known gaps (Type3a predicates, fixed-point
  `RN=min(RX,RY)`) were not hit in the explored paths of this function, but
  8 branch combinations is not exhaustive coverage of all conditions.
