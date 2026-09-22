# blk93@0x1cbe19 — note/pitch-to-frequency converter with a new 128-entry response-curve table

- **Bounds**: entry `0x1cbe19`, exit `0x1cbf07`, 100 instructions. **[V]** —
  `tools/sharcflow.py --base-sw 0x1c13e6` over `blk93.bin` (re-run fresh this
  session): the return at `sw=0x1cbe14, after=0x1cbe19` opens this function,
  the next return `sw=0x1cbf02, after=0x1cbf07` closes it — exactly the
  boundary the task brief gives for the adjacent `FUN_1c71ec` stage 6
  (`0x1cbf07`), confirming this function's epilogue and that stage's entry
  are the same address. Exact match to `tools/sharcinv.py`'s inventory
  entry (`entry=1883673, exit=1883911, n_insns=100`). No call target lands
  inside `[0x1cbe19, 0x1cbf07)` (checked against the full call list below),
  so this is a genuine single function, not a split span.
- **Callers**: **one**, `blk93@0x1c642a` (the 1458-instruction orchestrator),
  from **two call sites**, `0x1c6db5` and `0x1c6e88` — see "Call sites"
  below. **[V] Callees**: **three, not two** — the inventory undercounts by
  one (see "Method-gap correction"). **[V]**
  - `0x1cbe72` → **`blk93@0x1cc6c4`** (`25a_direct`, resolved by the
    inventory)
  - `0x1cbead` → **`blk88@0x1c0d68`** (`25a_direct`; inventory lists this as
    `unresolved_callees: [1838440]` — `1838440 == 0x1c0d68`, so it *is*
    resolved, just not cross-block-labelled)
  - `0x1cbede` → **`blk88@0x1c06ba`** (`8a_rel`, `b=1` Type 8a CALL,
    `cond=31`/unconditional despite the field name — **invisible to the
    inventory's `calls: 2` count**, the same gap `blk93-1cb4b2.md` already
    flagged for `sharcflow.py`/`sharcinv.py`)
- **Label**: inventory says "envelope or gain", confidence not recorded
  here but reasoning is float-ALU/float-mul dominance. **Disagrees.** The
  traced constants and call target make this a **pitch/note→frequency (or
  frequency-ratio) conversion**, not a gain or envelope: `220.0`, `69.0`,
  `12.0`, `1/12`, `2.0` (the classic 12-TET `f = 220 * 2^((note-69)/12)`
  constant set) all appear on the single straight-line path leading into the
  call to `0x1c0d68`, which `docs/findings/functions/blk88-1c0d68.md`
  independently documents as "shared exponential/mapping primitive (`R4`
  selects a base: `2.0` / `10.0` / π)... reused for both pitch
  (note→frequency-ratio) and what looks like gain (dB-ish) computation
  depending on which constant the caller loads into `R4`" — here `R4=2.0`
  (float bits `0x40000000`) at the call, which is the **pitch** branch of
  that shared primitive, not the gain branch.

## What it computes

**[D]** — traced with `tools/sharc_trace.py --blob --start 0x1cbe19
--concrete-memory --assume-32bit-normal-words`, both with
`--follow-loaded-calls` (to identify the two direct-call targets) and with
`--continue-external-calls` (to see the whole 100-instruction body without
descending into callees). The function has **no hardware loop**
(`loop_literal`/`loop_register` both 0 in the inventory vector, and no
`12a_imm`/`12a_ureg` instruction anywhere in the traced span) and exactly
**one small conditional branch** (`8a_rel` at `0x1cbec3`, 2 dynamic paths
explored, reconverging 18 short words later at `0x1cbed5`).

1. **Table lookup / interpolate** (`0x1cbe19`–`0x1cbe6f`, before the first
   call): the established "128-entry coefficient table, index scaled and
   clamped to 127, two adjacent taps blended" idiom (same shape as
   `docs/findings/06-sharc-engine-and-startup.md`'s `0x1c18a6` and
   `docs/findings/functions/blk93-1ccfa4.md`'s `0x2c2cc0` table) —
   **but against a table this note believes is previously uncatalogued: DM
   `0x26b338`** (loaded as a literal into `I4` at `0x1cbe59`, the vector's
   lone `dm_0x2xxxxx` literal region). The scale/clamp pattern is explicit:
   `R0 = 127.0` (`0x1cbe23`), a `float-multiply` into `R2` (`0x1cbe26`),
   `float-max` then `float-min` (`0x1cbe2b`, `0x1cbe30` — two-sided clamp of
   the scaled value), `trunc` to get index `i` (`0x1cbe35`), `increment` +
   integer `min` against `127` (`R10=127`, `0x1cbe33`/`0x1cbe3e`) to get
   index `i+1` clamped. `i+1` → `M3`, `i` → `M4` (`0x1cbe43`, `0x1cbe4b`).
   Two loads `I4+M3*4` and `I4+M4*4` (`0x1cbe5c`, `0x1cbe5f`) gather the
   adjacent table entries, blended through a `float-mulalu-subtract` dual op
   (`0x1cbe4e`) and two `float-mulalu-add` dual ops (`0x1cbe68`, `0x1cbe6d`)
   against constants `0.2`, `1.0`, `22000.0`, `20.0` — **`20.0`/`22000.0` are
   an audio-frequency-range pair (20 Hz .. 22 kHz), not note constants**, so
   this stage's blend already leans toward a frequency-domain quantity, not
   a plain 0..1 coefficient. Result stored to `*(I5+40)` (`0x1cbe6f`, `I5`
   is `R4` — see Interface).
2. **CALL `0x1cc6c4`** (`0x1cbe72`). Not followed to completion here (see
   "Callees" note below) but its own 20-instruction body loads `π`
   (`0x40490fdb`) and `1.0`, then tail-calls `0x1c12b4` (20 instructions from
   the task brief's cited log/`ln(10)` primitive at `0x1c1284`) before its
   own return — i.e. `0x1cc6c4` is itself a thin forwarder into a shared
   trig/log-family routine, not envelope math. **[O]** — not traced past
   this point (would need a targeted trace of `0x1cc6c4`/`0x1c12b4` on its
   own, out of scope here); flagging it as a plausible **new sibling of the
   documented shared-math primitives** (`0x1c06ba` reciprocal, `0x1c0d68`
   base^x, `0x1c1284` `ln(10)`), not as "gain".
3. **Pitch-to-ratio setup and CALL `0x1c0d68`** (`0x1cbe79`–`0x1cbead`): a
   dense straight-line block of float ops using, in this order, `1/8191`
   (`0.00012208521366119385` — the wavetable's `0x1fff` mask's reciprocal,
   task brief), `8191.0`, `mant` (mantissa extraction), `69.0` (MIDI note
   A4), `1/12` and `12.0` (semitones-per-octave and its reciprocal),
   `2.0` (the exponent base), `220.0` (a reference frequency, A3), and a
   second `mant`. This constant set is the standard **12-TET conversion**
   `f = 220.0 * 2^((note - 69) / 12)` (or an equivalent note-number
   normalisation feeding the shared `base^x` evaluator) — `R4 = 2.0` at the
   call (`opaque_calls` dossier: `{R7: 8191.0, I4: 0x26b338, R4: 2.0,
   R13: 220.0}`), matching `blk88-1c0d68.md`'s documented "`R4` selects the
   base" convention exactly on its pitch branch. **[D]** — the exponent
   operand (the `(note-69)/12` term, presumably `R8`) is not independently
   confirmed since the tracer names only result registers, not source
   operands, for `compute` actions.
4. **Post-call blend and conditional branch** (`0x1cbeb4`–`0x1cbed5`):
   after the call returns (dossier shows `*(I5+48)` reloaded into `R4`, so
   this field is read again rather than trusted across the call — consistent
   with the callee clobbering all data registers), a `float-compare`
   (`0x1cbeba`) followed by `R10 -> R2` under `cond=1`, then a second
   `float-compare` (`0x1cbec2`) feeds the branch at `0x1cbec3`:
   - **Taken** (`0x1cbec3` → `0x1cbed5` directly, delay slots
     `R6=8372.015625`/`float-min R8` always run): stores `*(I5+44) =
     min(F2, F6)`.
   - **Not taken** (falls through `0x1cbecb`–`0x1cbed3`): five extra
     instructions (`R2=3.848484992980957`, two multiplies, a subtract, a
     final `float-add`) compute `*(I5+44) = F10 + F2` instead — a different,
     more elaborate formula for the same output slot. **[O]** — what
     distinguishes the two cases (edge-of-range note? sign of something?)
     was not chased further; the branch's own condition source is a
     `float-compare` two instructions earlier whose operands aren't named
     by the tracer.
   Both paths reconverge at `0x1cbed5`.
5. **CALL `0x1c06ba`** (`0x1cbede`, Type 8a, delay slot is a
   `float-mulalu-subtract` dual op `[R14,R15]`) — the shared reciprocal
   documented in `blk93-1cb4b2.md` (RECIPS + 3 Newton refinements). Dossier
   at the call: `{R1: 0.9, R6: 6.0, I4: 0x26b338}`. **[O]** — which register
   is the actual `1/x` operand is not confirmed (see `blk93-1cb4b2.md`'s own
   open item on this, same limitation here).
6. **Final clamp and store** (`0x1cbee6`–`0x1cbeee`): `float-multiply`,
   `float-subtract`, then a two-sided `float-max`/`float-min` clamp, stored
   to `*(I5+60)`. Given the `20.0`/`22000.0` constants seen in step 1, this
   final clamp is very likely bounding the computed value to an audible
   frequency range (20 Hz .. 22 kHz) before it is handed off — consistent
   with this being the **frequency parameter that feeds the adjacent
   wavetable stage** (`0x1cbf07`, `FUN_1c71ec` stage 6, a two-tap
   `2^32`-accumulator wavetable lookup with an `0x1fff`/8192-entry mask) —
   the `1/8191`/`8191.0` constants seen in step 3 are the same mask domain,
   which is the concrete link the task brief asked to check. **[D]**.

**Net reading**: this is not "envelope or gain". It is a **note/pitch
parameter converter**: one output field (`*I5+40`) comes from a new
128-entry response-curve table gather/interpolate, a second
(`*I5+44`) from a small conditional correction, and the main output
(`*I5+60`) from the shared `base^x` 12-TET pitch formula and shared
reciprocal, clamped to an audio frequency range — plausibly the frequency
(or phase-increment precursor) that stage 6's wavetable lookup consumes.
The inventory's "envelope or gain" label is the same kind of
float-ALU/float-mul heuristic miss `blk88-1c0d68.md` already found for its
own callee.

## Structure

| Region | What |
|---|---|
| `0x1cbe19`-`0x1cbe1e` | prologue: save `M3`, `I7 -= 40` |
| `0x1cbe1f`-`0x1cbe43` | 8-register callee-save (`R15,R10,R3,R11,R13,R6,R14` + old `I5`) interleaved with compute (SHARC dual-issue) |
| `0x1cbe23`-`0x1cbe38` | table-index scale (`*127.0`) + two-sided clamp + `trunc` → index `i` |
| `0x1cbe33`-`0x1cbe43` | index `i+1` = `min(i+1, 127)` → `M3`; `i` → `M4` |
| `0x1cbe45`-`0x1cbe4e` | float-convert of a stack-loaded local (`I6+M6*4`), fractional-weight setup |
| `0x1cbe4e` | `R4 -> I5` (caller's struct pointer becomes the output cursor) |
| `0x1cbe54`-`0x1cbe6f` | table gather (`I4+M3*4`, `I4+M4*4`) + 3 dual `mulalu` blends, store `*(I5+40)` |
| `0x1cbe72` | **CALL `0x1cc6c4`** (→ forwards to `0x1c12b4`, near the `ln(10)` primitive) |
| `0x1cbe79`-`0x1cbeaa` | pitch-ratio setup: `1/8191`, `8191.0`, `mant`, `69.0`, `1/12`, `12.0`, `2.0`, `220.0`, `mant` |
| `0x1cbead` | **CALL `0x1c0d68`** (shared `base^x`, `R4=2.0`) |
| `0x1cbeb4`-`0x1cbec2` | post-call reload of `*(I5+48)`, two `float-compare`s |
| `0x1cbec3` | **conditional branch** (`8a_rel`, 2 dynamic paths) |
| `0x1cbec6`-`0x1cbeda` | taken: `min` path; not-taken: 5-instruction polynomial path; both store `*(I5+44)` |
| `0x1cbede` | **CALL `0x1c06ba`** (shared reciprocal, Type 8a) |
| `0x1cbee6`-`0x1cbeee` | final blend + two-sided clamp, store `*(I5+60)` |
| `0x1cbef0`-`0x1cbf00` | epilogue: restore `M3`, `I5`, and the 8 saved registers |
| `0x1cbf02`-`0x1cbf07` | `9b_abs` return + delay slots |

No hardware loop anywhere in the span (`loop_literal=0`, `loop_register=0`
in both the inventory vector and the exhaustive `--continue-external-calls`
trace's `loop_setups: []`).

## Method-gap correction: third callee invisible to `sharcinv.py`

The inventory's feature vector records `"calls": 2` and
`callees: ["blk93@0x1cc6c4"]` / `unresolved_callees: [1838440]`. A fresh
`tools/sharcflow.py --base-sw 0x1c13e6` run over `blk93.bin` finds **three**
call sites in `[0x1cbe19, 0x1cbf07)`: the two `25a_direct` calls the
inventory already half-credits, plus an `8a_rel` (Type 8a, `b=1`) CALL at
`0x1cbede` targeting `0x1c06ba`. This is the same gap `blk93-1cb4b2.md`
documented for its own function: `sharcflow.py`'s call detector (and by
extension `sharcinv.py`'s `calls`/`callees` counts) only recognises
`25a_direct`/`25a_pcrel` and indirect `9b_abs`/`M5` calls, not the hardware
Type 8a CALL. Confirmed independently by `tools/sharc_trace.py`, which does
model the `b` bit correctly and shows the call as an `opaque-external-call`
to `0x1c06ba` with a normal return.

## Call sites — the highest-value part

Both sites are in `blk93@0x1c642a`, decoded from raw fields in
`sw1c642a_insns.json` (cross-checked against `tools/sharc_trace.py --start
0x1c6d91`/`--start`-adjacent runs for the semantic register-copy
confirmation) and independently against the `srcureg = (high<<2)|(low1<<1)|
low0` field-combination formula (validated: `combined=37` → `M5`,
`combined=11` → `R11`, `combined=20` → `I4`, consistent across both sites).

**Site 1, `0x1c6db5`** (`25a_direct`, target `0x1cbe19` confirmed from the
raw `addr[23:16]=28, addr[15:0]=48665` fields → `(28<<16)|48665 =
0x1cbe19`):
- `0x1c6da9`: `R11 = <int add>` (an integer `add` result, operands not
  named by the raw-field decode alone)
- `0x1c6dab`: **`R4 <- R11`** (`5a_move`, unconditional)
- `R12` is **not** written in this call's setup window; its value going
  into the call comes from a `2c` `pass` op at `0x1c6db4`, the instruction
  immediately before the call.

**Site 2, `0x1c6e88`** (same target, `addr` fields identical):
- `0x1c6e7b`: **`R12 <- M5`** (`5a_move`, unconditional)
- `0x1c6e82`: **`R4 <- R7`** (`5a_move`, unconditional)
- `0x1c6e85`: **`R12 <- R10`** (`5a_move`, unconditional) — **overwrites**
  the `M5` copy 3 instructions later, so the value that actually reaches
  the call in `R12` is `R10`, not `M5`.

**What differs and what it implies**: the only register both sites
definitely set is `R4`, from a different source each time (`R11` at site 1,
`R7` at site 2) — consistent with `R4` being the function's one real
formal parameter (which becomes `I5`, the output-struct cursor), fed a
different per-call value (plausibly a per-voice, per-track, or per-note
struct pointer) by the orchestrator's loop body at each site. **Neither
traced path inside this function reads an incoming `R12`** — `R12` is only
ever written by *this* function's own compute, never loaded as an argument
— so site 2's extra `R12 <- M5 <- R10` setup looks like **caller-side
register scheduling that this callee does not consume on the paths
explored**, not a genuine second parameter. This is worth flagging rather
than asserting: it is possible an unexplored path (there are only 2 dynamic
paths total, both already covered by the branch at `0x1cbec3`) does read
`R12`, but nothing in the traced 100-instruction body does. If a second
opinion is wanted, this is the item to check first.

## Its two (really three) callees

- **`blk93@0x1cc6c4`** (20 instructions, `0x1cc6c4`-`0x1cc6f2`): loads `π`
  and `1.0`, then calls `0x1c12b4` (near the task brief's `ln(10)` primitive
  at `0x1c1284`) and returns. Inventory labels it "block copy / move"
  (confidence 0.4, "loads roughly match stores, little compute between
  them") — a weak label for what looks like a thin forwarder into a
  shared trig/log routine. It has **two callers**: this function and
  `blk93@0x1cc6f2` (its own immediate successor in address space), so it is
  a genuinely shared helper, not private to pitch conversion. **[O]** — not
  traced to completion; a good next target.
- **`blk88@0x1c0d68`**, documented in `blk88-1c0d68.md`: "shared
  exponential/mapping primitive", 15 distinct callers image-wide, `R4`
  selects the base (`2.0`/`10.0`/π). Called here with `R4=2.0` — the pitch
  branch.
- **`blk88@0x1c06ba`**, documented in `blk93-1cb4b2.md`: the shared
  iterative float reciprocal (RECIPS + 3 Newton refinements).

## Memory

**One literal absolute DM address touched: `I4 = 0x26b338`** (`0x1cbe59`,
float bits `0x26b338` as a plain integer literal, not the code-space `byte =
0x28000000|2*sw` alias — this is a data pointer). This does **not** match
any previously catalogued table in the task brief's list (`0x2558dc`-
`0x2560de` frame, audio rings `0x262138`/`0x262938`/`0x263138`/`0x263938`,
cosine `0x8055c440`/`0x8055c640`, the 1024-float pair, 829-float
`0x8045a6c8`, 32-float `0x8045c3c0`, `0x26bb68`, 128-float pair
`0x256388`/`0x256588`, shared context `0x252d3c`, RAM tables `0x25d940`,
`0x2c2cc0`, `0x2c2018`), nor any address in the existing `docs/findings/
functions/*.md` notes (checked with `grep`). **This is a new table.**

- **Base**: `0x26b338` (DM word address; byte alias `0x2826b338`).
- **Storage**: `tools/sharcldr.py out/sections/dt2-1.16/section_7_BLOB.bin
  --addr 0x2826b338 --addr-space byte` resolves it to file offset `0x66c8`
  inside **blk37** (`target_address=0x2826a2f8, cnt=12404`, **not** a FILL
  block) — real payload bytes baked into the image, i.e. **ROM**, not a
  zeroed RAM scratch table like `0x25d940`/`0x2c2cc0`/`0x2c2018`. **[V]**
  (read directly from the image; sha256 matches the task's stated
  `0f514a12a2255f5c081e292c47f1f29462003177658da4bbae0a22fd737fffa2`).
- **Stride**: 4 bytes (32-bit float), matching the `I4+M3*4`/`I4+M4*4`
  addressing.
- **Entry count**: at least 128 (indices `0`..`127`, matching the
  scale-by-127/clamp-to-127 index computation). Read directly (Python
  `struct.unpack` over the raw file bytes at offset `0x66c8`): index `0` =
  `0.0`, rising **monotonically and concavely** to index `127` = `1.0`
  (indices 126/127 both read ~`1.0`, i.e. the curve saturates at its last
  entry). Bytes immediately after index 127 (indices 128/129 = `0.0`, index
  130 = `-1.0`, then a fast-decaying negative series) look like a
  **second, separate table** starting nearby, not a continuation of this
  one — consistent with this being one 128-entry table among several
  packed contiguously in blk37, the same way `0x256388`/`0x256588` are a
  contiguous pair.
- **Shape**: 0→1 over 128 entries, concave (fast initial rise, saturating
  near the top) — the general shape of a **response/shaping curve over a
  7-bit (0..127) domain** (velocity curve, knob-position curve, or similar
  nonlinear MIDI-range LUT). **[O]** — the exact closed form (e.g. `x^(1/n)`
  for some `n`, or a lookup with no simple formula) was not solved; a few
  candidate power-law fits were tried by hand and none matched cleanly
  across multiple sample points, so this is left as "a monotonic 0..127
  response curve", not a named function.

No other named region (parameter frame, audio rings, cosine, 1024/829/32-
float tables, `0x26bb68`, `0x256388`/`0x256588` pair, `0x252d3c`,
`0x25d940`/`0x2c2cc0`/`0x2c2018`) is touched by literal address anywhere in
this function.

## Interface

- `R4` (arg) → `I5`: the output/state struct pointer. Fields written:
  `+40` (table-interpolated value), `+44` (branch-selected blend), `+60`
  (final clamped value, likely the frequency handed to the wavetable
  stage). Field `+48` is *read* (once before the first call, once again
  after — consistent with the intervening call clobbering registers, not
  the struct).
- `R8` (arg, presumed): copied to `R15` early (`0x1cbe2b`, unconditional)
  but **not observed being read again** afterward in the traced path —
  **[O]**, could be a genuine second parameter that only matters on an
  unexplored branch, or dead.
- `I6+8` (stack slot, presumed caller-frame local): loaded once
  (`0x1cbe54`) and folded into the index-scaling arithmetic. **[O]** — not
  traced back to its origin in the 1458-instruction orchestrator; could be
  a per-voice or per-frame constant set up once and reused across many
  calls, not specific to this call site.
- No parameter-frame (`0x2558dc`-`0x2560de`) literal access — this function
  does not itself read the SRC page. Whether the values arriving via `R4`/
  `R8`/`I6+8` were themselves derived from the frame by the caller is
  **[O]**, unresolved by this note (same open status `blk93-1cb4b2.md`
  left the SRC-page-consumer question in).
- Return: `9b_abs` at `0x1cbf02`, standard `15b`+`25c_rframe` delay slots.

## Open

- The exact algebraic form of the pitch computation (which register holds
  the raw note number, whether `220.0` is a multiplied-in reference or a
  subtracted one, what `0.25984251499176025` and `8372.015625` are for).
- `blk93@0x1cc6c4` and `0x1c12b4` — traced only one level deep here; a
  dedicated pass would settle whether `0x1cc6c4` is a real trig/log
  sibling worth cataloguing on its own.
- Whether `R8` (this function's putative second argument) is read on any
  path — only 2 dynamic paths exist and neither reads it, so either it's
  unused here or the callers never rely on it.
- The closed-form shape of the new `0x26b338` 128-entry curve table.
- Confirming (by reading more of `0x1c642a`) what `R11`/`R7` and the
  `I6+8` stack slot actually represent at each call site — this is the
  concrete next step if the goal is to name this function's parameter
  precisely (e.g. "voice index" vs. "raw MIDI note").
