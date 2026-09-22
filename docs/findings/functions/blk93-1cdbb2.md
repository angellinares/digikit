# blk93@0x1cdbb2 — conditional per-track state update (two guarded skips, float MAC body, data-dependent tail loop)

- **Bounds**: entry `0x1cdbb2`, exit `0x1cdcb0`, **115 instructions**, matching
  `tools/sharcinv.py` exactly (`entry=1891250 exit=1891504 n_insns=115`,
  confirmed by an independent linear disassembly of the same byte range with
  `sharc_disasm.disassemble()`, zero desyncs, 115 decoded instructions before
  the return's two delay slots end at `0x1cdcb0`). **[V]**
- **The single return is genuine and unconditional**: the only `9b_abs` in
  the span is at `0x1cdcab` — `b=0, cond=0x1F` (TRUE), `pmi=4, pmm=6`
  (DAG2 `I12`/`M14`), `j=1` — exactly `tools/sharc_trace.py`'s hard-coded
  "verified compiler return" idiom (jump through `I12`/`M14`, two delay
  slots: a `15b` store then `25c_rframe`). No conditional return exists in
  this function, so the return-delimited boundary is not at risk of the
  "conditional returns are missed" failure mode the task warns about.
  **[V]** (`tools/sharc_trace.py --blob --start 0x1cdbb2` reaches this exact
  PC on 3 of 4 explored paths and stops with `"return without followed
  call"`, which for a fresh trace with an empty call stack is the *normal*
  successful exit, not an error).
- **`0x1cdcb0` is not this function** — it is the next return-delimited span,
  `blk93@0x1cdcb0`–`0x1cdcc3` (10 instructions, inventory label also
  "unclassified/mixed", callers `blk93@0x1c6154` and `blk93@0x1c642a`). No
  branch inside `0x1cdbb2`'s 115 instructions targets it (the function's
  only three conditional branches target `0x1cdc93`, `0x1cdc8b` and
  `0x1cdc7d`, all *inside* the span — checked exhaustively, only three
  `8a_rel` and one `12a_ureg` exist in the whole 115-instruction body). So,
  unlike `blk93@0x1cddff`, this boundary is **not** a cold-branch artifact:
  it is a real, independently-called function immediately followed by
  another real, independently-called function. **[V]**
- **Callers**: exactly one, **`blk93@0x1c642a`** (the giant per-frame
  render/dispatch orchestrator). Found by an exact byte-pattern search for
  the `25a_direct` (Type 8a delayed HW **CALL**) encoding of target
  `0x1cdbb2` (`180400 1cdbb2`, word-swapped as stored:
  `04 18 1c 00 b2 db`) — **1 hit, image-wide**, at file offset `0x3d15a`,
  which maps to loader byte address `0x2838dd5a` = **`sw 0x1c6ead`**,
  inside `0x1c642a`–`0x1c71ec`. Disassembling a locally-aligned window
  around it (`0x1c6e71`–`0x1c6eb4`, 24 instructions, zero desyncs — the
  giant orchestrator desyncs on a full linear sweep, so this was walked from
  a nearby alignment point rather than from `0x1c642a` itself) shows the
  call to `0x1cdbb2` is the **third of three back-to-back sibling calls**:
  `0x1c6e88 -> 0x1cbe19`, `0x1c6e9d -> 0x1cbdea`, `0x1c6ead -> 0x1cdbb2`.
  `0x1cbe19` is already catalogued (inventory label "envelope or gain", 100
  insns, 1 caller = this same site); `0x1cbdea` is not yet read. **[V]** for
  the call site and the two sibling targets; **[D]** that they process
  related per-track state (inferred from being emitted back to back with
  the same push/pop bracketing pattern around each call, not from reading
  `0x1cbdea` or `0x1cbe19` in full).
- **Callees**: none — confirmed leaf (`callees: []`, and no `25a_direct`,
  `8a_rel` with `b=1`, or `9b_abs` CALL found anywhere in the 115
  instructions). **[V]**
- **Label**: `tools/sharcinv.py` abstains — "unclassified/mixed", confidence
  0, reason "no rule matched". Feature vector: `float_mul=24 mac=6
  float_alu=30 int_alu=2 shifter=1 loop_register=1 loop_literal=0
  dual_add_sub=0 multifn=0 named_tables_touched=[] float_immediates=[]`.
  The abstention is correct: this function does not match any of the six
  shapes the classifier is tuned for (see "What it computes"). **[V]** that
  the vector is accurate (cross-checked every `float-*`/`mac` op against the
  trace below); **[D]** that "no rule matched" is the right call (it is, but
  only after tracing — a reader going purely off the vector could easily
  mis-guess "envelope or gain" by analogy to the many same-shaped
  neighbours).

## What it computes

**[D]** for the algorithm identification below (traced twice — once via
`tools/sharc_trace.py --blob --start 0x1cdbb2 --concrete-memory
--assume-32bit-normal-words --follow-loaded-calls --json`, which explored 4
paths (44/70/73/94 steps) and reached the real return on 3 of them, one
stopping only at the data-dependent hardware-loop count; once via an
independent manual field decode of every `5a_move`/`5b_move` (register
interface) and `15b`/`4a`/`3a`/`3b` (memory) instruction against the same
byte range, which reproduces every address the tracer resolved
(`I4 + 112`, `I4 + 140`, etc.) exactly). **[O]** for the loop body and the
exact math inside it — never reached in either trace because `R2` (the trip
count) is caller-supplied and this run had no concrete `R4`.

This is **not** one of the confirmed engine shapes: not the two-tap
interpolated table lookup (no `trunc`/`+1`/lerp, no table literal), not the
6-tap polyphase resampler (no 64-bit phase accumulator, only 1–2
`float-mulalu-add`s not 6), not the polynomial envelope `x <- x*(2-|x|)`
(no `abs`, no `2.0 -` pattern), not a two-state linear recurrence, not the
128-entry coefficient table or the 0.8465/0.1534 gain-smoothing pair (no
matching float immediates — `float_immediates=[]`), and it calls neither
the shared reciprocal `0x1c06ba` nor `blk88@0x1c0d68`/`0x1c1284` (it is a
leaf). It also isn't an FFT tell: `dual_add_sub=0`, no bit-reversed
addressing, no power-of-two loop counts (the one hardware loop is
register-counted, not literal at all).

**Proposed new shape: a conditional, per-track state-accumulator with a
caller-supplied element count.** Control flow ([V], from the trace):

1. Two guard branches, each able to skip an increasing amount of the body:
   - `0x1cdbef` (`cond=7`, ASTATX `SV` bit, non-negated — PGR Table 10-4):
     **taken -> jumps straight to the epilogue at `0x1cdc93`**, doing *no*
     per-element work at all. This is the function's fast "nothing to do"
     exit (44-step trace path).
   - `0x1cdc26` (`cond=0`, ASTATX `AZ` bit — "IF EQ", tested on the `R12`
     decrement two instructions earlier): **taken -> jumps to `0x1cdc8b`**,
     skipping the second float block, the whole loop-setup/loop, and the
     MAC block, landing directly at the struct write-back (73-step path).
2. If neither guard fires, two ~10-op chains of `float-multiply`/
   `float-add`/`float-subtract` (`0x1cdbfd`–`0x1cdc23`, then
   `0x1cdc29`–`0x1cdc40`) each consume one array element from `I5` (the
   `R8` argument) via `I5 + M6*4` and combine it with several fields of the
   `I4` (`R4` argument) state struct, each chain ending in a `decrement`
   (`R12` then `R2`) and a conditional branch on the result.
3. `0x1cdc41` (`cond=0`, `AZ`, on the second decrement of `R2`): **taken
   (R2-1 == 0) -> skips straight to the MAC block at `0x1cdc7d`** (94-step
   path, reaches the return cleanly). **Not taken -> `0x1cdc48`, a
   `12a_ureg` hardware-loop setup whose trip count is loaded directly from
   `R2`** — i.e. a **register** count (`12a_ureg`, not `12a_imm`), the
   post-decrement remainder of whatever field seeded `R2`. This is the one
   path the tracer cannot follow without a concrete `R4` (70-step path,
   stops with `"nonconcrete Type12a UREG loop count"`). **[O]**: the loop
   body (`0x1cdc4b`–`0x1cdc7a`, ~50 instructions, more
   `float-multiply`/`float-add`/`float-subtract`/one `5a_move` `R9->R10`)
   was disassembled but not semantically traced.
4. All paths that don't bail at `0x1cdbef` converge at `0x1cdc7d`: two
   `float-mulalu-add` multifunction ops (`compute[22:16]=88` at `0x1cdc7d`
   and `0x1cdc80` — a fused multiply-then-accumulate, each writing both a
   product and an accumulated-sum register, not a "dual add/subtract"
   butterfly — `dual_add_sub=0` in the vector confirms this), then four
   more `float-add`s, then a scalar store to `I3 + M6*4` (the `R12`
   argument's output cursor, `0x1cdc8a`).
5. Epilogue (`0x1cdc8b`–`0x1cdcaf`): writes `R7`/`R2`/`R3`/`R15` back into
   the `I4` state struct at offsets `+196/+192/+12/+16` (see Memory),
   restores 13 registers from the `I6` frame, returns via `I12`/`M14`.

The shape — two independent bail-out guards gating an increasing share of
the work, two fixed 2-element float combines, then an optional
register-counted continuation loop, one or two fused MACs, and a
read-modify-write on two persistent state fields plus two more
write-only fields — most resembles a **per-track smoothing/update step
operating on a variable number of new samples per call** (the register
loop count would then be "how many new elements arrived since the last
call"), in the same functional family as `blk93@0x1cb4b2`'s
"envelope/coefficient-table refresh" (see Interface — identical register
convention) but with the caller supplying a variable, not fixed, amount of
work. This is inference, not a settled identification; naming it more
precisely needs either the loop body traced with a concrete `R4`/`R2`, or
reading the two sibling calls (`0x1cbe19`, `0x1cbdea`) for corroborating
context. **[O]**

## Structure

- **Prologue** (`0x1cdbb2`–`0x1cdbea`): `I7 -= 64` (`19a_scaled`, 16 words
  scaled by 4); spills the incoming `I2`/`I3`/`I5` values (via an `R2`
  staging register) to `I6 - 28/-24/-20`; spills `R3, R6, R7, R9, R10, R11,
  R13, R14, R15` directly to `I6 - 64 .. I6 - 32` (11 registers total) — the
  same callee-saves-on-an-`I6`-frame shape as `blk93@0x1cb4b2`. One `3b`
  load (`I6 + M6*4`) into `R13` immediately after the saves, of unclear
  purpose (**[O]**). A `5a_move`+compute pair at `0x1cdbd7` runs `leftz`
  (count/normalize a bitfield) into `R2` in parallel with `R8 -> I5`.
  Reads eight `I4`-relative state fields (offsets `+12, +16, +48, +56,
  +104, +112, +132, +140, +160` — one, `+104`, combined with a
  `float-add` into `R4`) before the first guard branch.
- **Guard 1** `0x1cdbef` (`SV`, taken -> `0x1cdc93`, epilogue, no work).
- **Block 1** `0x1cdbf6`–`0x1cdc23`: `I7 += 8` (unrelated local, `19a`);
  reads `I2 - 48/-52/-56` and `I5 + M6*4` (one array element); ~11
  `float-multiply`/`float-add`/`float-subtract` ops into
  `R0-R3,R6,R7,R9-R12,R15`; a store to `I2 + 124`; `decrement R12`.
- **Guard 2** `0x1cdc26` (`AZ`, taken -> `0x1cdc8b`, skip to write-back).
- **Block 2** `0x1cdc29`–`0x1cdc40`: reads `I2 - 48/-52/-56` again and
  `I5 + M6*4` (a second array element — `M6` unresolved but the same
  stride register as block 1, consistent with post-modify walking one
  float per access); ~10 more float ALU ops into `R2-R4,R6,R7,R9,R15`; a
  store to `I2 + 124` (same field block 1 wrote — path-dependent value); a
  second `decrement`, this time of `R2`.
- **Guard 3 / loop gate** `0x1cdc41` (`AZ` on `R2`'s decrement, taken ->
  `0x1cdc7d`, skip loop; not taken -> `0x1cdc48` `12a_ureg` loop, count =
  `R2`). **[O]** loop body unresolved.
- **MAC tail** `0x1cdc7d`–`0x1cdc8a`: two `float-mulalu-add`s, four
  `float-add`s, one scalar store to `I3 + M6*4`.
- **Write-back** `0x1cdc8b`–`0x1cdc91`: `R7 -> I4+196`, `R2 -> I4+192`,
  `R3 -> I4+12`, `R15 -> I4+16` (the last two are read-modify-write: `+12`
  and `+16` were also read in the prologue as `R7`/read via the `4a` at
  `0x1cdc00`/`0x1cdbf9` — see Memory).
- **Epilogue** `0x1cdc93`–`0x1cdcaf`: loads `I12` from `I6 + M7*4` (the
  return-linkage slot), restores `I2/I3/I5` and the 8 direct-saved
  registers from the `I6` frame, returns.

## Memory

All addressing in this function is **register-relative** (`I4`/`I2`/`I5`/
`I3`/`I6`, fixed immediate offsets or `M6`-strided array walks) — no
absolute/literal DM address appears anywhere in the 115 instructions
(matches the inventory's `named_tables_touched=[]`). So whether this
function ever touches the parameter frame, an audio ring, or a named table
depends entirely on what pointer values the caller (`0x1c642a`) puts in
`R4`/`R8`/`R12` — which this run did not resolve (see Callers). **This is
exactly the "register-held pointer" case the task flags for the SRC
parameter-frame consumer** — not ruled out, not confirmed. **[O]**

- **`I4` (= `R4` argument, per-track/per-call state struct)**: read at byte
  offsets `+12, +16, +48, +56, +104, +112, +132, +140, +160, +168`; written
  at `+12, +16, +192, +196`. `+12` and `+16` are read early and written
  back at the end with newly computed values (`R3`, `R15`) — a genuine
  persistent-state read-modify-write, confirming state carries across calls
  in the caller-supplied struct (answers Interface Q5 directly). `+192` and
  `+196` are write-only here (no read in this function — could be consumed
  by a sibling, e.g. `0x1cbe19` or `0x1cbdea`, or by a later call). **[V]**
  (every offset independently cross-checked between the automatic trace's
  `expression` field and a manual `data[6:0]` field decode of the `15b`/`4a`
  instructions — they agree exactly).
- **`I2`** (live on entry, *not* one of `R4`/`R8`/`R12` — its incoming
  value is only ever spilled and restored, never reassigned from an
  argument register): read at `-48, -52, -56`; written at `+124` (from two
  different paths, block 1 and block 2 — whichever runs last wins). Not
  identified — could be a second, orthogonal shared-context pointer (the
  task's named `0x252d3c` shared context is a candidate given the pattern,
  but this was not confirmed against a concrete value). **[O]**
- **`I5`** (= `R8` argument, array cursor): read once per block via
  `I5 + M6*4` (blocks 1 and 2 — two fixed elements, *not* loop-driven,
  since both reads happen before the register-counted loop even starts).
  Matches the "array cursor" role in `blk93@0x1cb4b2`'s confirmed
  interface. **[V]** for the role match, **[O]** for what array it points
  at.
- **`I3`** (= `R12` argument, output cursor): written once, `I3 + M6*4`,
  one scalar float, only on the paths that reach the MAC tail. Matches
  `0x1cb4b2`'s "output-array cursor" role. **[V]**/**[O]** as above.
- **`I6`** (frame pointer): the callee-save frame (`-64..-8`) plus one
  early `I6 + M6*4` read (`0x1cdbd5`, purpose unclear, **[O]**) and the
  `I12` return-linkage load (`I6 + M7*4`, `0x1cdc93`).
- **No touch of**: the parameter frame literal range `0x2558dc`-`0x2560de`,
  either audio ring, either cosine table, the `1024`-float pair, the
  `829`-float or `32`-float tables, `0x26bb68`, the `128`-float pair
  `0x256388`/`0x256588`, `0x25d940`, `0x2c2cc0`, `0x2c2018` — none appear as
  literals, and none of the unresolved register pointers (`I2`, `I4`, `I5`,
  `I3`) were pinned down to any of these ranges. **[D]** (absence of a
  literal is evidence but not proof against a register-held alias).

## Interface

- **`R4` -> `I4`**: per-track/per-call state struct pointer (read-modify-
  write at `+12`/`+16`, write-only at `+192`/`+196`, read-only at `+48,
  +56, +104, +112, +132, +140, +160, +168`). **[V]**
- **`R8` -> `I5`**: input array cursor, one float stride (`M6`),
  read-only. **[V]**
- **`R12` -> `I3`**: output array cursor, one float stride (`M6`),
  write-only. **[V]**
- This is the **exact same three-register convention** already confirmed
  in `blk93@0x1cb4b2` ("envelope/coefficient-table refresh": `R4->I4`,
  `R8->I5`, `R12->I3`) — strong evidence the two functions belong to the
  same family of per-track update routines, even though this one is not
  `0x1cb4b2` and does not call the shared reciprocal. **[V]** for the
  register mapping; **[D]** for the family inference.
- **State persists**: yes, confirmed — see `I4 +12`/`+16` read-modify-write
  above. **[V]**
- Return value: none of `R0`/`R4`/`R8`/`R12` is set immediately before the
  return in any traced path; the function communicates only through the
  struct/array writes. **[D]**

## Open

- The `12a_ureg` loop body (`0x1cdc4b`–`0x1cdc7a`) — never traced
  concretely; needs a real `R4`/`R2` from the caller (or a targeted
  `--set` once the state-struct layout is known from a sibling read) to
  see what it accumulates and confirm/refute the "variable element count"
  reading.
- The exact `R4`/`R8`/`R12` values `0x1c642a` passes at `0x1c6ead` — not
  bit-decoded; only `R4`'s *setup instruction* (`0x1c6e82`, `R7 -> R4`,
  10 instructions earlier, not re-touched before any of the three sibling
  calls) was found, suggesting but not proving the same state pointer is
  shared across all three calls in the cluster.
- `I2`'s identity and whether it aliases the shared context `0x252d3c` or
  another named region.
- Whether `blk93@0x1cbdea` (the middle sibling call, not yet read) and
  `blk93@0x1cbe19` corroborate the "per-track state update cluster"
  reading.

## Method

- Image sha confirmed: `shasum -a 256 out/sections/dt2-1.16/section_7_BLOB.bin`
  = `0f514a12a2255f5c081e292c47f1f29462003177658da4bbae0a22fd737fffa2`,
  matching the task and `.coldtrace/Digitakt_II_OS1.16/section_7_BLOB.bin`.
  Note `sections/.source-sha256` in the working tree is a *different*
  (1.15C) hash — the 1.16 blob lives under `out/sections/dt2-1.16/`.
- Inventory: `python3 tools/sharcinv.py out/sections/dt2-1.16/section_7_BLOB.bin
  --blocks 93 --json /tmp/inv93.json`.
- Disassembly: `sharcldr.LoadedMemory` + `sharc_disasm.disassemble()` over
  the loader-byte range for `sw 0x1cdbb2` (115 instructions, zero desyncs)
  and, separately, a locally-aligned window around the call site
  (`sw 0x1c6e71`–`0x1c6eb4`, 24 instructions, zero desyncs — found by
  scanning small alignment offsets around the byte-pattern hit until one
  produced a clean decode landing exactly on the known call).
- Call-site discovery: exact byte-pattern search (word-swapped 48-bit
  `25a_direct` frame `18 04 00 1c db b2` -> `04 18 1c 00 b2 db`) over the
  whole blob — 1 hit.
- Semantic trace: `tools/sharc_trace.py out/sections/dt2-1.16/section_7_BLOB.bin
  --blob --start 0x1cdbb2 --concrete-memory --assume-32bit-normal-words
  --follow-loaded-calls --json --max-steps 200 --max-states 64` (note: the
  positional `source` argument is required even with `--blob`; omitting it
  fails argument parsing rather than defaulting). 4 states explored,
  44/70/73/94 steps; 3 reach the real return, 1 stops on the nonconcrete
  loop count. Every memory `expression` the tracer printed was
  cross-checked against an independent manual field decode of the
  corresponding `15b`/`4a` instruction (offset field × 4 under
  `--assume-32bit-normal-words`) and agreed exactly.
- Did not run the emulator, did not start Ghidra.
