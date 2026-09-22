# blk93@0x1ccfa4 — multi-table two-tap interpolated coefficient synthesis, state written back to caller

- **Bounds**: entry `0x1ccfa4`, return (`9b_abs`) at `0x1cd132`, delay slots
  (`15b` + `25c_rframe`) finish at `0x1cd136`. 180 instructions, exactly
  matching `sharcinv.py`'s boundary (`entry=0x1ccfa4 exit=0x1cd137
  n_insns=180`). **[V]** — independently re-derived by linearly
  disassembling the exact byte range with `tools/sharc_disasm.py`'s
  `disassemble()` (script in scratchpad, not `sharcflow.py`'s return list):
  it decodes cleanly, with no desync, from `0x1ccfa4` through the `9b_abs`
  at `0x1cd132` and its two delay-slot instructions, landing exactly on
  `0x1cd136` as the last instruction and `0x1cd137` as a clean new function
  entry (confirmed separately as `blk93@0x1cd18b`'s second callee, see
  Callers below) — a real boundary, not a truncation artifact.
- **Callers**: **two**, matching the inventory (`sharcinv.py`'s fresh run,
  `tools/sharcinv.py out/sections/dt2-1.16/section_7_BLOB.bin`):
  `blk93@0x1c642a` (the per-frame orchestrator named in the task) and
  `blk93@0x1cd18b` (a small 61-instruction, all-integer glue function, no
  float ops at all, itself called once from `blk93@0x1c6154`). Both call
  sites verified directly against the raw bytes (see "Both callers" below).
  **[V]**
- **Callees**: **six real calls, not two.** `sharcinv.py`/`sharcflow.py`
  report `calls: 2`, both `25a_direct` to `0x1c1284` — correct as far as it
  goes, but **it misses four Type 8a `CALL`s** (`b=1`) to the shared
  reciprocal `0x1c06ba`, exactly the gap the task brief warns about. Found
  by linearly disassembling the function's own byte span (not the classic
  25a_direct/16a detector) and checking every `8a_rel` instruction's `b`
  field: `0x1cd053`, `0x1cd05e`, `0x1cd0a3`, `0x1cd0ca` are all `8a_rel,
  b=1`, and all four resolve (`pc + sext24(reladdr) = 0x1c06ba` for every
  one, verified arithmetically) to the shared reciprocal documented in
  `docs/findings/functions/blk93-1cb4b2.md`. **[V]**. `0x1c1284` is **not**
  a `sharcinv.py` function entry — it is a call target landing inside
  `blk88@0x1c127c`'s return-delimited span (entry `0x1c127c`, exit
  `0x1c12b3`), i.e. exactly the "function sitting inside another's span"
  case the README's conventions section calls out. See "The `0x1c1284`
  helper" below.
- **Label**: inventory label "envelope or gain", confidence 0.4, reason
  "float ALU ops dominate a moderate multiply count -- polynomial/ramp
  shape". **Partially agrees**: this is a coefficient-producing function
  with heavy float ALU/multiply traffic (38 float multiplies, 52 float ALU
  ops, matching the task brief), but the concrete mechanism is a **table
  lookup**, not a ramp or polynomial saturator. It is a new shape for this
  engine's catalogue — see "What it computes".

## What it computes

**No hardware loop anywhere in the function** — `loop_literal=0,
loop_register=0` in the inventory vector, and a full scan of the 180-word
span found zero `12a_imm`/`12a_ureg` instructions. The entire body, apart
from the six calls, is **one straight-line basic block**: no `9a_*`
conditional branch, no `18a`, nothing that alters control flow inside the
function. **[V]** (exhaustive instruction-type census of the whole span,
counted from the linear disassembly: 42× `2a_short`, 22× `15b`, 17× `4a`,
15× `17a`, 14× `2c`, 13× `3a`, 12× `5a_move`, 11× `3c`, 9× `5b_move`, 7×
`3b`, plus the 6 calls and the fixed prologue/epilogue forms — no branch
form present). This rules out any per-sample or per-iteration loop shape;
whatever this computes, it computes once per call, unconditionally.

**The first ~39 traced steps (`tools/sharc_trace.py --blob --start
0x1ccfa4 --concrete-memory --assume-32bit-normal-words
--follow-loaded-calls --json`, before it stops on an unsupported `2b`
opcode — see "Tracer gap" below) show a textbook two-tap interpolated table
lookup, twice over, against two different small RAM tables:**

1. **Table 1 (128 entries).** `R10 = 0x43000000` (**128.0**) is loaded, then
   a `float-multiply` scales an input value by it (`0x1ccfbe`). `R0 =
   0x42FE0000` (**127.0**) is loaded and used in a `float-max`/`float-min`
   clamp pair. The incoming argument **`R4` is copied into `I5`**
   (`ureg-copy R4 -> I5`, `0x1ccfd6`) before `R4` itself is immediately
   reloaded with the plain integer **127** (`0x1ccfd9`, `ureg-write R4 =
   127`) — the clamp bound for the second index. The scaled value is
   `trunc`ed (`0x1ccfd0`) into an index, copied into `M4`
   (`ureg-copy R2 -> M4`), incremented and `min`-clamped against 127 into
   `M3` (`0x1ccfe2`/`0x1ccfe5`) — **the classic `idx = trunc(x*128); idx2 =
   min(idx+1, 127)` two-tap index pair.** `I4` is then loaded with the
   literal RAM address **`0x2c2cc0`** (`0x1ccfea`), and two loads follow:
   `R1 = DM(I4 + M4*4)` (`0x1ccfed`, pre-modify) and `R9 = DM(I4 + M3*4)`
   (`0x1ccfef`) — table[idx] and table[idx+1], gathered from adjacent
   entries of a **128-entry float array at `0x2c2cc0`**, then combined with
   `float-multiply`/`float-add` (the interpolation blend, `0x1ccff2`-
   `0x1ccff9`). **[V]** (every step named directly by the tracer's
   operation log).
2. **Table 2 (1024 entries), same shape.** Right after, `R2 = 0x44800000`
   (**1024.0**, `0x1cd009`) and `R11 = 0x447FC000` (**1023.0**,
   `0x1cd00e`) appear, then `R8 = 0x3ff` (**1023** as a plain integer,
   `0x1cd01f`) — the identical `scale-by-N / clamp-to-N-1` pattern, one
   order of magnitude up. `I4` is then loaded with a second literal RAM
   address, **`0x2c2018`** (`0x1cd027`), and the same
   index/increment/clamp/gather/interpolate shape repeats (`0x1cd02a`-
   `0x1cd049`, three `3a`-form DM accesses in a row — a two-tap gather
   again). This part was read from the raw instruction stream, not the
   semantic tracer (which had already stopped) — **[D]**, structural
   evidence (the 1024.0/1023.0/0x3ff triple and the repeated two-tap
   instruction shape) is strong but the individual float ops were not
   re-confirmed by the tracer's operation names the way Table 1's were.

This is the same **two-tap interpolated table lookup** mechanism already
named in this engine (docs/findings for the wavetable stages), but applied
to **short, RAM-resident coefficient/curve tables** (128 and 1024 entries)
rather than a periodic waveform, and driven by a value computed fresh each
call rather than a running phase accumulator. It is explicitly **not** the
`x <- x*(2-|x|)` polynomial saturator (no `float-abs` appears anywhere in
the traced operation list, and no repeated abs/subtract/multiply triple
exists in the instruction census) and **not** the six-dual-MAC blend network
from `0x1cb4b2`'s richer path (`dual_add_sub: 0` in the inventory vector —
this function contains zero dual add/subtract instructions). Combined with
the four calls to the shared reciprocal and the two calls to the `0x1c1284`
helper (see below), and the result being written back into a small
caller-supplied struct (see Interface), the best one-line description is:
**a coefficient-synthesis routine that interpolates two small runtime
tables, normalises the result through the shared reciprocal and a
log-flavoured helper, and stores the outcome into persistent per-call
state.**

### Tracer gap: unsupported Type 2b at `0x1cd002`

`tools/sharc_trace.py` stops at `0x1cd002` with `"unsupported form 2b"`.
This is a **new gap**, distinct from the two the task brief names
(multifunction `0x1e`/`0x1f`, float ALU `0xd9`/`0xda`). Decoding the raw
compute field by hand (`compute[22:16]=0x2b, compute[15:0]=0x186` ->
23-bit field `0x2B0186`) against `tools/sharcspec/compute_table.json`'s
`single_function_selector` layout (`mf[22], cu[21:20], opcode[19:12],
rn[11:8], rx[7:4], ry[3:0]`): `mf=0, cu=0b10` (**Shifter unit**, 32-bit
fixed format), `opcode=0xB0`, `rn=1, rx=8, ry=6`. **`0xB0` does not appear
in `shiftop_shiftimm`'s 25 documented rows** (PRM Table 18-9) — the table's
own notes flag a documented gap around here ("PGR also omits BITDEP
entirely"), so this may be `BITDEP` or another undocumented shifter op.
**[O]** — not resolved further; flagging it is more valuable than guessing.
Everything after this point in the note comes from the raw instruction
census and address literals, not the semantic tracer.

### The `0x1c1284` helper

Called twice (`0x1cd068`, `0x1cd0b5`), both via the classic
`25a_direct` + `16a`-return-push convention (so `sharcflow.py`/`sharcinv.py`
do see these two). `0x1c1284` sits mid-body inside `blk88@0x1c127c`'s
return-delimited span, reached both by fallthrough from a `JUMP` at
`0x1c127c` and directly by these two `CALL`s — a second, independent entry
point inside that function's span, per the README's stated convention.
Disassembled directly (`0x1c1270`-`0x1c12c0`): loads the float constant
**`0x40135D8E` = 2.3025851 = ln(10)** exactly (`0x1c127f`) and later
**`3.0`** (`0x1c1292`), has one internal conditional branch pair (`9a_rel`
at `0x1c128a`/`0x1c128d`) selecting between two near-identical short
float-compute stretches, and returns via `9b_abs` at `0x1c12ae`. No literal
address is loaded in its span. Neither call site in `0x1ccfa4` sets a fresh
literal argument immediately before the call (unlike the reciprocal calls'
implicit-1.0 pattern documented in `0x1cb4b2`) — its input is whatever the
preceding table-interpolation arithmetic just produced. The `ln(10)`
constant makes a **log/dB-style conversion** ("multiply by 1/ln(10)" or
"exp(x*ln10)", i.e. base-10 log or its inverse) the natural reading, but
this is **not** the already-documented `base^x` evaluator at
`blk88@0x1c0d68` (different address, different body, no `2.302585`
constant recorded there) — a second, smaller, previously-undocumented
shared primitive. **[O]** — not traced or fully decoded; flagged as a
follow-up target in its own right.

## Structure

```
0x1ccfa4  prologue: I7 -= 12*4 bytes (19a_scaled), save M1/M2/M3/I3/I5 and
          R7/R9/R10/R11/R13/R14/R15 to the I6 frame (~10 registers)
0x1ccfbb  R10 = 128.0
0x1ccfbe  float-multiply (x * 128.0) -> R2
0x1ccfc5  float-max clamp
0x1ccfc8  R0 = 127.0
0x1ccfcb  float-min clamp; R4(arg) copied -> I5 (state-struct pointer)
0x1ccfd0  trunc -> R2 (index)
0x1ccfd3  R15 = 1.0
0x1ccfd6  R4 reloaded = 127 (int clamp bound); R2 -> M4 (idx)
0x1ccfd9  increment -> R1
0x1ccfdc  R1 = 512.0
0x1ccfdf  R1 min-clamped against 127 -> M3 (idx+1, clamped)
0x1ccfe2  float-convert -> R2
0x1ccfe5  float-subtract (fractional part)
0x1ccfe8  float-multiply
0x1ccfea  I4 = 0x2c2cc0                         ; Table 1 base (128 floats)
0x1ccfed  R1 = DM(I4 + M4*4)                    ; table[idx]
0x1ccfef  R9 = DM(I4 + M3*4)                    ; table[idx+1]
0x1ccff2..0x1ccfff  interpolation blend (float-multiply/float-add x2)
0x1cd002  2b -- unresolved Shifter opcode 0xB0 (tracer gap, see above)
0x1cd004  1a -- dual DM+PM access + compute (mem_dual=1, the function's only one)
0x1cd009  R2 = 1024.0
0x1cd00e  R11 = 1023.0
0x1cd01f  R8 = 1023 (int)
0x1cd027  I4 = 0x2c2018                         ; Table 2 base (1024 floats)
0x1cd02a..0x1cd049  second two-tap index/gather/interpolate (same shape)
0x1cd053  CALL 0x1c06ba  (8a_rel, b=1 -- shared reciprocal, missed by static tools)
0x1cd05e  CALL 0x1c06ba  (8a_rel, b=1)
0x1cd068  CALL 0x1c1284  (25a_direct + 16a push -- ln(10) helper)
0x1cd0a3  CALL 0x1c06ba  (8a_rel, b=1)
0x1cd0b5  CALL 0x1c1284  (25a_direct + 16a push)
0x1cd0ca  CALL 0x1c06ba  (8a_rel, b=1)
0x1cd0cd..0x1cd117  ~6 near-identical write blocks (3c address-setup + 4a
          compute+store pairs, dreg cycling through R0/R1/R4/R8/R11/R12) --
          plausibly one write per state-struct slot; matches the 6-slot
          local struct the 0x1cd18b caller pre-builds (see Interface)
0x1cd11a..0x1cd130  epilogue: restore R2/R7/R9/R10/R11/R13/R14/R15,
          M1/M2/M3/I3/I5 from the I6 frame (loads, d=0)
0x1cd132  9b_abs return
0x1cd134  15b (delay slot: restore R15)
0x1cd136  25c_rframe (delay slot: deallocate frame)
```

Instruction-type census for the whole 180-word span (from the linear
disassembly, exact counts): 42× `2a_short`, 22× `15b`, 17× `4a`, 15× `17a`,
14× `2c`, 13× `3a`, 12× `5a_move`, 11× `3c`, 9× `5b_move`, 7× `3b`, 4×
`8a_rel` (all calls), 4× `17b`, 3× `19a_scaled`, 2× `25a_direct` (both
calls), 2× `16a`, 1× `9b_abs`, 1× `4b`, 1× `2b`, 1× `25c_rframe`, 1× `1a`,
1× `19a`. **[V]**

## Memory

Every literal RAM address loaded in this function (from the `17a`
instructions carrying a full 32-bit immediate into an `I`-register):

| address | role | loader-covered? |
|---|---|---|
| `0x2c2cc0` | Table 1 base, 128-entry gather (`I4`, `0x1ccfea`) | no — RAM |
| `0x2c2218` | second `I4` load, `0x1cd009` region | no — RAM |
| `0x2c1018` | third address literal, `0x1ccfff` region | no — RAM |
| `0x2c2018` | Table 2 base, 1024-entry gather (`I4`, `0x1cd027`) | no — RAM |
| `0x2c0008` | fourth address literal, `0x1cd002` region | no — RAM |

Checked with `tools/sharcldr.py out/sections/dt2-1.16/section_7_BLOB.bin
--addr <addr> --addr-space byte`: all five report *"not covered by any
loaded block"* — every buffer this function touches is runtime RAM, not
loader-initialised ROM. **[V]**

**None of the task's named tables are touched by a literal address**: not
the cosine pair (`0x8055c440`/`0x8055c640`), not the 1024-float pair
(`0x8055c890`/`0x8055d890` — despite this function also working with a
1024-entry table, it is a *different*, RAM-resident array at `0x2c2018`),
not the 829-float exponential (`0x8045a6c8`), not the 32-float table
(`0x8045c3c0`), not `0x26bb68`, not the 128-float pair (`0x256388`/
`0x256588` — again, despite a 128-entry table being read here, it is a
different address, `0x2c2cc0`), not the shared context struct (`0x252d3c`),
not any audio ring (`0x262138`/`0x262938`/`0x263138`/`0x263938`). **[V]**
for the literal scan — this does not rule out one of `I5`/`I3`/`M`-indexed
accesses aliasing a named table through a caller-supplied pointer, which
this analysis cannot see without a concrete caller.

Does **not** touch the parameter frame (`0x2558dc` base, `0x2559b6 +
track*0x60` per-track block) by literal address either — no `17a`/`17b`
immediate in this function's span falls in `0x2558xx`-`0x255axx`. **[V]**
for the literal scan, **[O]** for the SRC page's consumer generally (this
function is not it, at least not via a hard-coded literal).

The `0x2c0000`-`0x2c3000` region these five addresses sit in is not one of
the task's named tables and, as far as this note found, not previously
documented elsewhere in `docs/findings/`. Worth a dedicated look: it may be
a per-voice or per-track RAM working set distinct from the wavetable
engine's fixed tables. **[O]**

## Interface

- `R4` (arg) -> **copied into `I5`** at `0x1ccfd6` (`ureg-copy R4 -> I5`,
  confirmed by the tracer), then `R4` itself is immediately reloaded with
  the plain integer 127 and used purely as scratch for the rest of the
  function. `I5` is one of the registers saved/restored across the call
  (prologue/epilogue), so it is very likely the **persistent state-struct
  pointer** — matching the "state persisted to a caller struct" shape named
  in the task brief, here arriving through `R4`/`I5` rather than the
  `R4`/`I4` pairing seen in `0x1cb4b2`. **[V]** for the copy itself,
  **[O]** for exactly which fields of the struct are read/written (the
  ~6-block write sequence at `0x1cd0cd`-`0x1cd117` was read structurally,
  not semantically — see Structure).
- `R8` (arg): at the `0x1cd18b` caller, `R8` is set to the **literal float
  1.0** immediately before the call (`0x1cd1f9`, `17a ureg=0x8
  data=0x3f800000`) — a plain scalar parameter, not a pointer, at least for
  that call site. What role `1.0` plays (a mix/gain target, an initial
  ratio) is **[O]**.
- `R12`: no `ureg-copy` from `R12` was seen in the traced portion (the
  tracer stopped at step 39, before any `R12` use would show). Not
  resolved — **[O]**.
- No return value register: the epilogue restores callee-saved registers
  and returns without a distinguished result register, consistent with
  "output goes through the state-struct pointer" like `0x1cb4b2`.

## Both callers

**`blk93@0x1cd18b`** (small glue function, calls `0x1ccfa4` then
immediately calls the sibling function `blk93@0x1cd137` that starts right
where `0x1ccfa4` ends): before the call at `0x1cd1fc` (`25a_direct addr =
0x1ccfa4`), the function spends `0x1cd18e`-`0x1cd1f7` building a **6-entry
local struct** via an identical unrolled block repeated 6 times — each
repetition stores three zero floats and one `0x3F7EB852 = 0.995` float at
stride `0x20` (32 bytes) through `(I4, M6)`-indexed writes. Immediately
before the call, `R8 = 0x3F800000` (**1.0**) is loaded (`0x1cd1f9`). The
`I4` pointer to this freshly-built struct is the natural candidate for what
becomes `R4`/`I5` inside `0x1ccfa4` — a **6-slot state block initialised
with a near-unity constant (0.995)**, strongly suggestive of one-pole
smoothing state being primed before the first call. **[D]** (the I4-to-R4
argument link at the call site itself was not bit-decoded; inferred from
proximity and from the struct shape matching the callee's ~6-block write
pattern).

**`blk93@0x1c642a`** (the giant per-frame orchestrator): the call is at
`0x1c6e30` (`25a_direct addr = 0x1ccfa4`, confirmed by byte-pattern search
— see Method). In the ~20 instructions immediately before it
(`0x1c6dfa`-`0x1c6e2e`) the orchestrator: sets `M4 = -1.0`
(`0x1c6dfa`)/`M5 = 1.0` (`0x1c6e09`) inside what looks like a small
conditional (two `8a_rel` branches, `b=0`, at `0x1c6df7`/`0x1c6e03`,
i.e. an if/else, not calls), then rebuilds `I4` twice via `5a_move`
(`0x1c6e1b`, `0x1c6e27`) interleaved with several `3b`-form DM stores
(`0x1c6e12`, `0x1c6e2a`, `0x1c6e2e`) that look like the same
"build-a-small-struct-then-call" shape as `0x1cd18b`'s prologue, just less
regular. **[O]** — the exact `R4`/`R8`/`R12` values were not bit-decoded
here; establishing them would need either resolving the `5a_move`/`3b`
compute fields precisely or a concrete/traced run. What is confirmed is
that this is a genuinely different call site from `0x1cd18b`'s (different
setup shape, inside the big per-frame orchestrator rather than small
dedicated glue), consistent with the same coefficient-synthesis routine
being invoked from two different parts of the per-frame pipeline with
different live state.

## Method

- Bounds, instruction census and the Type 8a call scan were all produced by
  linearly disassembling the function's exact byte range with
  `tools/sharc_disasm.py`'s `disassemble()` (via `sharcldr.LoadedMemory` +
  `sw_to_byte` for the loader-aware byte lookup) — a script in the session
  scratchpad, not saved to the repo. It decoded the whole 180-instruction
  span, both caller regions, and the `0x1c1284` helper's ~30-instruction
  span with **zero desyncs**. A full linear sweep of the whole giant
  orchestrator (`0x1c642a`-`0x1c71ec`, 1458 instructions) *did* desync
  after 565 instructions (`"no form matches"` at `0x1c69dc`) — expected for
  a function that large with embedded branch/jump-table structure; the
  call site inside it was instead located by an exact byte-pattern search
  for the `25a_direct` encoding of target `0x1ccfa4` (`0x1804001ccfa4` as a
  48-bit frame, byte-swapped per 16-bit-little-endian-word rules), which
  found exactly 2 hits image-wide, matching both known callers exactly (no
  false positives).
- `tools/sharc_trace.py --blob --start 0x1ccfa4 --concrete-memory
  --assume-32bit-normal-words --follow-loaded-calls --json` ran 39 steps
  before stopping on the unsupported `2b` form; its operation-naming
  (`float-multiply`, `float-max`, `trunc`, etc.) is the source for every
  claim marked **[V]** in "What it computes" above. Nothing past
  `0x1cd002` was traced semantically.
- Image sha confirmed: `shasum -a 256
  out/sections/dt2-1.16/section_7_BLOB.bin` =
  `0f514a12a2255f5c081e292c47f1f29462003177658da4bbae0a22fd737fffa2`,
  matching the task brief.

## Open

- **The `0x2b`/opcode-`0xB0` Shifter instruction at `0x1cd002`** is a real
  gap in both `tools/sharc_trace.py` (form `2b` entirely unsupported) and
  `tools/sharcspec/compute_table.json` (`shiftop_shiftimm` has no row for
  `0xB0`). Resolving it would let the tracer run substantially further
  into this function.
- **What the ~6-block write sequence (`0x1cd0cd`-`0x1cd117`) actually
  writes**, field by field, and its correspondence to the 6-slot struct
  `0x1cd18b` pre-initialises with `0.995` — read structurally (repeated
  `3c`+`4a` shape, cycling destination registers) but not semantically.
  Resolving the `4a`/`3c` compute fields bit-exact, or a concrete/emulated
  run, would settle this and likely confirm or refute the "one-pole
  smoothing state" reading.
- **The `0x2c0000`-`0x2c3000` RAM region** (5 literal addresses touched
  here) is not one of the task's named tables and was not matched to any
  other documented buffer. Worth a `tools/refscan.py`-style search for
  other static references into this range, or a runtime check of what
  populates it.
- **`0x1c1284`** (the ln(10)-using helper) was disassembled but not traced
  or fully decoded — its exact function (base-10 log? its inverse? a dB
  conversion?) is inferred from the `2.302585`/`3.0` constants alone.
- **`R12`'s role** was not established at all (the tracer stopped before
  any `R12` use; the raw disassembly does not by itself identify which
  ureg feeds `R12` without deeper compute-field decoding).
- **The exact `R4`/`R8`/`R12` values `0x1c642a` passes** were not
  bit-decoded — only the general "build a small struct via 5a_move + 3b
  stores" shape was established, by analogy with `0x1cd18b`'s clearer
  setup.
- The odd float constant `162.8155059814453` (`0x4322D0C5`, loaded at
  `0x1cd040`) was not matched to any recognisable ratio or documented
  table entry.
