# blk69@0xb819bd — polynomial envelope shaper over a pointer-chained struct chain

Address convention for this whole note: **blk69**, loader target `0x20000000`,
`base_sw = 0xb80000` (the `byte = 0x28000000 | 2*sw` alias used for
blk93/blk1/blk88 does **not** apply here). All addresses below are
short-word (`sw`) addresses in that space unless marked "byte".

- **Bounds**: entry `0xb819bd`, exit `0xb81bae` (the `25c_rframe` return
  frame instruction; the return itself commits two instructions earlier, at
  the `9b_abs` indirect jump at `0xb81baa` — SHARC+ delay slots continue to
  `0xb81bae`). **212 instructions**, matching `tools/sharcinv.py`'s count
  exactly. **[D]** — established by direct disassembly
  (`tools/sharc_disasm.py` via the scratchpad's `blk69_list.py`,
  `SW_BASE=0xB80000, POFF=83412`) from `0xb819bd` through `0xb81bae`, and
  cross-checked against `tools/sharcflow.py --base-sw 0xb80000` run on the
  blk69 payload slice (`blk.json` block 69: `payload_offset=83412,
  payload_len=109436`, sha256 of `section_7_BLOB.bin` matches
  `0f514a12a2255f5c081e292c47f1f29462003177658da4bbae0a22fd737fffa2`). The
  instruction immediately after (`0xb81baf`) is a fresh `19a_scaled`
  prologue (`I7 += -40`, a different function, itself later called by our
  caller — see below), confirming the boundary. Not yet checked by a second
  agent.
- **Callers**: exactly one call site in the whole block targets `0xb819bd`:
  `0xb82187` (a `25a_direct` absolute-call form, `addr = 0xb8<<16 | 0x19bd =
  0xb819bd`, linked, delay slots `3c`+`16a`, returns to `0xb8218e`). **[D]**
  — from `tools/sharcflow.py`'s `calls` list (980 calls found over blk69).
  **Callees**: none. **[D]** — sharcflow's own call list shows none
  originating inside `[0xb819bd, 0xb81bae]`, and (per this task's Type 8a
  caveat) I decoded all ten `8a_rel` instructions in the function's body by
  hand: every one has `b=0` (plain conditional/unconditional branch, SHARC+
  PRM Type 8a table), none has `b=1` (hardware `CALL`). So the "no callees"
  claim survives the check that catches sharcflow's known blind spot.
- **Label**: `tools/sharcinv.py` says "wavetable/indexed lookup (pointer
  table)". **Partially disagrees.** The dominant instruction pattern (see
  below) is the polynomial envelope saturator `x <- x*(2-|x|)`, applied
  repeatedly to fields of a small pointer-chased structure chain — not a
  fractional-phase two-tap table interpolation. The one feature that likely
  drove the auto-label is a single early address computation, `I5 <- I4 +
  0x400004` (raw byte add, `19a` unscaled) — a big fixed offset added to a
  small caller-supplied value, which reads like `base | selector`
  addressing into a fixed table region. That address is data-dependent on
  the caller's `R4` and could not be resolved to a concrete DM address from
  static bytes or from `tools/sharc_trace.py` without a concrete seed for
  `R4` (open, see below).

## What it computes

The function walks a short chain of pointers (`I2`, `I3`, `I4`, `I5`,
threaded together with `i-modify ... M5`, i.e., a fixed stride) and, for
each node, applies the same three-instruction float idiom to one or more
float fields:

```
F = clip(load)
F = |F|
F = 2.0 - |F|      (R8=1.0f=0x3f800000, R9=2.0f=0x40000000, loaded once via
                     17a immediate writes at 0xb81a02/0xb81a05)
F' = F_orig * (2.0 - |F_orig|)
store F'
```

This is byte-identical in shape to the "on-the-fly polynomial envelope"
idiom already documented for blk93 (`docs/findings/functions/README.md`'s
named shapes list, and `docs/findings/functions/blk93-1cb4b2.md`'s
"Method-gap correction" section describing the same `float-clip` /
`float-abs` / `float-subtract` / `float-multiply` sequence). Traced example
(`tools/sharc_trace.py --blob --start 0xb819bd --concrete-memory
--assume-32bit-normal-words --follow-loaded-calls`, events at
`0xb81a15`-`0xb81a3e`):

```
0xb81a15 load  DM R2  <- [I4 + <unresolved>]
0xb81a16 load  DM R1  <- [I5 + 8]      ; float-clip R2
0xb81a19            ; float-clip R1
0xb81a1b            ; float-abs  R12  (|x|)
0xb81a1d            ; float-abs  R4
0xb81a1f            ; float-subtract R12   (2.0 - |x|)
0xb81a21            ; float-subtract R4
0xb81a23            ; float-multiply R2    (x * (2-|x|))
0xb81a24 store DM [I5 + M5*4] <- F2*F12 ; float-multiply R2 (again, second field)
0xb81a27 store DM [I5 + 8]    <- F1*F4  (post-modify)
```

The same three-field treatment (struct offsets **+8, +112, +120 bytes**,
i.e. word offsets 2, 28, 30) repeats for each node in the chain: first for
an initial fixed set of nodes reached via `I5`, `I2`, `I4` (chained by
`i-modify`/`M5`), then — after a register-counted loop setup at `0xb81ace`
(see Structure) — for more nodes reached the same way. 32 float multiplies
and 88 float ALU ops (from the task's static count) is consistent with
~10-11 repetitions of this 3-multiply/8-ALU-op idiom across the chain and
the loop body.

**No RECIPS, no `base^x` evaluator idiom, no cosine-table access, and no
fractional-phase multiply-by-two-neighbours pattern were seen anywhere in
this function.** It does not touch any of the named tables (see Memory).
So of the shapes already catalogued for blk93, this function matches
**the polynomial envelope shaper** specifically, not the interpolated
lookup, the reciprocal, the `base^x` evaluator, or the linear recurrence.

**Same library or separate?** The evidence points at *same library*, and
it is stronger than "byte-identical code": the caller of this function
(span `0xb820b1`-`0xb821ee`, see below) makes eight calls in immediate
succession, five of them into other blk69 addresses and **two of them
directly into blk93's own short-word range**, at `sw=0x1ccbd8`
(`1887192`). blk93's payload occupies sw `0x1c13e6`..`~0x1ce000`
(`target_address=0x283827cc`, `payload_len=104500`, alias
`byte=0x28000000|2*sw`), so `0x1ccbd8` falls inside it. This is a live,
directly observed cross-block call from blk69 code into blk93 code in the
same call cluster that reaches this function — not yet followed to see
what `0x1ccbd8` does, but it settles the "same call graph" question for
this corner of blk69. **[D]**, not independently re-checked.

## Structure

- **Prologue** (`0xb819bd`-`0xb819e7`): `I7 -= 88` (raw byte offset -22
  scaled by 4, `19a_scaled` form — 22-normal-word/88-byte local frame).
  `R4 -> I4` (the one argument register used). Then a block of `5b_move` +
  `15b` stores spilling `I2`, `I3`, `I5`, `R5`-`R7`, `R9`-`R11`, `R13`-`R15`
  to the frame — a standard callee-save spill, **including extended
  DAG/ureg registers** (the `15b` stores use `ureg` codes in the 105-126
  range alongside plain `R`/`I` registers, i.e. this prologue also saves
  L#/B# style extended registers, not just core R/I registers).
- **Branch fan-out** (`0xb819e2`, `0xb819e9`, `0xb819ff`): three `8a_rel`
  branches (`b=0`, real conditional branches, confirmed by hand) gate which
  of several near-identical envelope blocks executes — this looks like a
  small case dispatch (e.g. per-node-count or per-mode selection) rather
  than a single straight-line body.
- **First envelope block** (`~0xb81a15`-`0xb81aae`): applies the
  `x*(2-|x|)` idiom to three fields (+8/+112/+120) of each of a short,
  fixed chain of nodes reached via `I5`→`I2`→`I4` (each hop an `i-modify`
  with `M5`).
- **Register-counted loop** (`0xb81ace`, `12a_ureg`, `ureg=2`/`R2`, `mode=1`,
  `reladdr=81` i.e. body extends to about `0xb81b1f`): trip count comes
  from **`R2` at runtime, not a literal** — this is the `12a_ureg` form,
  not `12a_imm`. **[D]**, confirmed dynamically: with
  `tools/sharc_trace.py --max-states 64`, 2 of 26 explored paths stop with
  `"nonconcrete Type12a UREG loop count" at 0xb81ace` because the tracer
  cannot resolve `R2` without a concrete seed. The loop body repeats the
  same `+112`/`+120` treatment (not `+8`) on further chained nodes via `I3`,
  `I2`, `I5` again.
- **No hardware `DO`/`LCNTR` hardware-loop setup instruction was seen** —
  `12a_ureg` here reads as a manual bounded-iteration construct (SHARC+
  loop-with-register-count form), not the classic `LCNTR=n; DO x UNTIL LCE`
  pair.
- **Epilogue** (`0xb81b90`-`0xb81bae`): restores the extended/ureg
  registers saved in the prologue (mirrored `15b` loads, ureg codes
  105-126), pops `I7` back by +88 (`19a_scaled`, offset `+22` scaled),
  restores `R4`/`R5`/`I2`/`I3` etc from the frame, then the `9b_abs`
  indirect jump (`j=1,ci=0`, `pmi/pmm` selecting an I/M pair — reads as the
  SHARC+ "return via `JUMP(In,Mn)` using the popped return-PC register"
  convention) at `0xb81baa`, with `25c_rframe` closing the frame two words
  later at `0xb81bae`.
- Of 26 explored control-flow paths (`--max-steps 500 --max-states 64`), 24
  reach the return cleanly with step counts from 33 to 180; the remaining 2
  are the register-loop-count stops above. No path hit `max-steps`, so the
  function's control flow (aside from the one register-counted loop) is
  fully enumerable.

## Memory

- All data addresses touched in the traced body are **register-relative**
  (`I2`/`I3`/`I4`/`I5` + small constant or `+M5*4`), not absolute DM
  literals, so **none of the named tables were touched**: no access to the
  cosine table (`0x8055c440`/`0x8055c640`), the 1024-float pair
  (`0x8055c890`/`0x8055d890`), the 829-float exponential (`0x8045a6c8`),
  the 32-float table (`0x8045c3c0`), `0x26bb68`, the 128-float pair
  (`0x256388`/`0x256588`), or the audio rings (`0x262138`/`0x262938`/
  `0x263138`/`0x263938`). **[D]**, from the traced event log's `address`
  fields, all `{"unknown": "In + k"}` register-relative expressions.
- **No peripheral-register-shaped literals** (`0x310c93xx`/`0x310ca3xx`/
  `0x31089xxx`) appear anywhere in the function or its traced events, and
  `tools/sharc_trace.py`'s `peripheral_accesses` list is empty across all
  26 explored paths. This is **not** driver code. **[D]**
- One address computation could not be resolved: `I5 <- I4 + 0x400004`
  (`19a` raw/unscaled add, `0xb819ec`) early in the prologue, feeding a
  later `3c` DM load. Whether `0x400004` selects into a fixed DM region
  (consistent with the auto-labeler's "pointer table" guess) depends on the
  caller-supplied `R4`, which was not seeded. **[O]** — would need a
  concrete `--set R4=...` trace, or reading what the caller
  (`0xb820b1`-`0xb821ee`) actually places in `R4` before the call at
  `0xb82187`, neither of which was done here.
- Struct field offsets actually written: **+8, +112, +120 bytes** (word
  offsets 2, 28, 30) from each chain node.

## Interface

- **R4** in: pointer to the head of the node chain this function walks
  (copied to `I4` in the first prologue instruction). This is the only
  register whose *first* appearance in the trace is a read rather than a
  write, i.e. the only confirmed true input.
- **R8**, **R9**: set internally by the function itself (`17a` immediate
  writes to `1.0f`/`2.0f`), not inputs.
- **I2, I3, I5, I7**, and the extended ureg/DAG registers spilled in the
  prologue: callee-saved, restored before return — not inputs or outputs
  in the caller-visible sense.
- **R12**: used only as scratch inside the `float-abs`/`float-subtract`
  sequences; never read before being written in the trace.
- No I/M register is loaded with a caller-recognisable constant at entry
  (e.g. no fixed M-register stride matching a known table); `M5` is used
  throughout for the chain-walk stride but its value was never resolved
  concretely.
- **Parameter frame** (base `0x2558dc`, per-track block
  `0x2559b6 + track*0x60`): **not read**. Every address in this function is
  I-register-relative to the `R4` argument or to the local stack frame;
  no absolute reference to `0x2558dc` or the `0x2559b6` per-track region
  appears anywhere in the 212-instruction body. **[D]**

## Caller and call chain

Caller span: `0xb820b1`-`0xb821ee` (bounded by `tools/sharcflow.py`'s
return list: previous return closes at `0xb820b0`, this caller's own
return commits at `0xb821ea`/frame-close at `0xb821ee`). It makes eight
calls in sequence:

| call site | target | in blk69? |
|---|---|---|
| `0xb820c9` | `0xb81ef1` | yes |
| `0xb82170` | `0xb81f28` | yes |
| `0xb8217d` | `0xb81baf` (the function immediately after ours) | yes |
| `0xb82187` | **`0xb819bd` (this function)** | yes |
| `0xb8219a` | `0x1ccbd8` | **no — blk93** |
| `0xb821b2` | `0x1ccbd8` (same target again) | **no — blk93** |
| `0xb821c6` | `0xb817fc` | yes |
| `0xb821d3` | `0xb81896` | yes |

All eight are `25a_direct`/`3a`-style linked absolute calls (delay slots
`3c`+`16a` or `3a`+`16a`), i.e. genuine calls, not just literal-address data
builds — `0xb82187` itself is the `25a_direct` instruction encoding
`addr=0xb819bd`, matching our function exactly. **[D]**, from
`tools/sharcflow.py`'s call list filtered to the caller's span. The call
chain from this function therefore **does reach into blk93** two hops up
(via its caller), inside the same short burst of calls that includes this
function — the strongest evidence found in this session that blk69's dense
float code and blk93 belong to one shared library rather than two that
merely share bytes. `0x1ccbd8` itself was not opened or read.

## Open

- What `0x400004` in `I5 <- I4 + 0x400004` resolves to (needs a concrete
  `R4` seed, or reading the caller's argument setup before `0xb82187`).
- What `0x1ccbd8` (blk93) computes, and what the caller
  (`0xb820b1`-`0xb821ee`) is as a whole — it looks like a per-voice/per-tap
  setup or refresh routine that fans out to several sibling envelope
  functions plus two blk93 calls, but that was not read here.
- The exact trip count and per-iteration meaning of the `R2`-driven loop at
  `0xb81ace` (likely a track/tap/voice count, not resolved).
- Not checked by a second agent against the image bytes; all marks above
  are **[D]**, none **[V]**.
