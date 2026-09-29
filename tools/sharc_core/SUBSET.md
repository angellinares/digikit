# The translatable subset of tools/sharc_core

The SHARC+ core is written so that one source serves three uses: the
symbolic driver (`tools/sharc_trace.py`), the concrete driver
(`tools/sharc_run.py`), and a future translator that emits a native
concrete core (Rust or C) from the same Python. This file defines the
Python subset that translator may assume. `tools/sharc_subset_lint.py`
checks the parts of it that the syntax tree shows, and
`tests/test_sharc_subset_lint.py` holds the core to
`subset_allowlist.json`, which only shrinks.

    uv run python tools/sharc_subset_lint.py            # counts per module and rule
    uv run python tools/sharc_subset_lint.py --detail   # every violation with its line
    uv run python tools/sharc_subset_lint.py --update   # lower the allowlist

## 1. One source, two drivers

Semantic functions compute over the value lattice in `values.py`:
`Const` (a known 32-bit value), `Affine` (symbolic address arithmetic),
`Unknown`, and `PartialConst` (bit-level knowledge, ASTATX/ASTATY only),
plus `MR` in `state.py` (the 80-bit accumulators, known bit by bit). Both
drivers run the same Python. The difference is only which values reach
it: `sharc_run.py` seeds `Const`, `sharc_trace.py` seeds `Affine` and
`Unknown`.

The split between concrete and symbolic execution is the lattice
boundary, not a second copy of the semantics:

- The **boundary** is the code a concrete translation replaces with a
  small hand-written native runtime instead of translating (list in
  section 2). It may use any Python.
- Everything else is **semantic code** in the subset. It sees values only
  through the boundary: `isinstance(v, Const)` / `isinstance(v, MR)` (is
  the value known), `.value`, the lattice operations (`_add`,
  `_subtract`, `_multiply`, `_bitwise`, `_not`, `_is_unknown`,
  `_astatx_known_bit`, `_apply_flag_update`), and `Unknown(reason)`.

A concrete specialisation maps the lattice like this:

| Python | Concrete native core |
|---|---|
| `Const(x)` | `(known=1, x & 0xFFFFFFFF)` |
| `Unknown(reason)` | `known=0` (the reason is erased) |
| `isinstance(v, Const)` | `v.known` |
| `_is_unknown(v)` | `!v.known` |
| `Affine` | never constructed by the concrete driver; a trap if it appears |
| `PartialConst` (ASTATX/ASTATY) | `(mask, bits)` |
| `MR` | `(mask, bits)` as 128-bit integers |
| `FlagUpdate` | the same four integers, applied by `_apply_flag_update` |
| a fork (`_copy` of an unresolved predicate) | a trap; the concrete runner halts there too |

## 2. The boundary

Declared in `BOUNDARY` in the lint (the rules skip these):

- `values.py`: the whole lattice, including `FlagUpdate` and its
  builders (`_flags_define`, `_flags_forget`, `_flags_put`,
  `_flags_from_pairs`, `_flags_then`, `_flags_or`), `_astatx_define`,
  `_astatx_forget`, `_apply_flag_update`, and the named integer
  operations `_op_and`, `_op_or`, `_op_xor`, `_op_andnot`.
- `state.py`: `MR` and `_mr_*` (the 80-bit lattice); `_ureg`,
  `_ureg_raw` (register reads that collapse `PartialConst`);
  `_snapshot_uregs` (the pre-instruction register file every form reads
  its operands from); `_pey_view` and `_pey_special` (PEy's view of the
  register file and accumulator, a register-number offset natively);
  `_copy` (a fork); `_note_provisional` (a per-run report); and the
  observability functions `_event`, `_stop`, `_render`, `_json_value`.
- Generation time: `sequencer.decode_at`,
  `encoding._split_compute_fields`, and every `_build_*` table builder.
  They depend on the image and the decoded fields only, so a generator
  evaluates them before it emits code.
- `memory._dossier`: the external-call report.

## 3. Rules for semantic code

Each rule names the lint rule that checks it, if any.

**Functions.** Module-level `def` only: no `lambda` (`lambda`), no
nested `def` (`closure`). A function value is always a reference to a
module-level core function or `abs`: the handler tables (`FORMS`,
`ALU_OPS`, ...), the operation argument of `_bitwise` (`_op_*`) and of
`_float_binary`/`_float_unary`/`_double_*` (`_f_add`, `_f_sub`, `_f_mul`,
`_f_avg`, `_f_neg`, `_f_pass`, `_float_min`, ...). No `*args`/`**kwargs`,
`getattr`/`setattr`, `global`, `yield` or `with` (`dynamic`); `**` in a
call is allowed only inside an observability call.

**Type tests.** Only `isinstance(v, Const)` and `isinstance(v, MR)`
(`isinstance-symbolic` for `Affine`/`Unknown`/`PartialConst`,
`isinstance-type` for anything else). One declared exception: a compute
result's destination is a tagged union (`ComputeDest`: register number,
special-register name, or a tuple of those; a Rust enum), and only
`compute._apply_compute` and `compute._apply_compute_pey` match on it
with `isinstance(rn, str | tuple | int)` (`DEST_MATCH`).

**Records.** Fixed-shape data is a `NamedTuple` (`FlagUpdate`,
`MultSpec`, `MultifnOperands`) or a frozen dataclass of the lattice. A
translator emits a struct. Records carry no behaviour except
`FlagUpdate.__call__`, kept for callers written against the closure form.

**Integers.** Python integers are unbounded; the width is the author's
job and the translator's type choice, so every value keeps to one of
these and the code masks explicitly:

- 32-bit register values: `Const` masks to 32 bits on construction; a
  signed view is `_signed32(x)` or `_signed(x, bits)`.
- 64-bit products: fixed and fractional multiplies (`_mr_product_raw`,
  `_multiply_fractional`) form up to 64-bit signed products.
- 80-bit accumulators: `MR` and `compute_mult` keep `raw & _MR_MASK`
  (`(1 << 80) - 1`); signed views subtract `1 << 80`. Native: `i128`.
- `>>` on a negative Python int is arithmetic, the same as `>>` on a
  signed native integer of enough width.
- `//` and `%` appear only on non-negative operands (`length_bytes // 2`),
  where floor and truncation agree.
- Shift counts can exceed the width (`1 << position` for a 6-bit
  position, a 64-bit bit FIFO). Python gives 0 or the sign; the
  translator must emit a checked shift, not a raw native shift.
- Floats convert to integers only through `_trunc_int` and
  `_round_even_int`, and the callers only range-check the result against
  int32 before masking it; a saturating `as i64` keeps that exact.

The lint cannot see widths or shift counts; the translator checks them.

**Floats.** Only the float primitives in `floats.py` touch `struct`,
`math` or `try` (`struct-math`, `try`):

| Primitive | Exact native equivalent |
|---|---|
| `_f32_from_bits(b)` | `f32::from_bits(b) as f64` |
| `_float32_bits(x)` | `(x as f32).to_bits()`; overflowed = result infinite and `x` finite |
| `_f64_from_words(hi, lo)` | `f64::from_bits(hi << 32 \| lo)` |
| `_double_pair_bits(x)` | `x.to_bits()` split into halves |
| `_ldexp(x, n)` | `scalbn(x, n)` (C `ldexp`); infinity on overflow |
| `_trunc_int(x)` | `x.trunc() as i64` |
| `_round_even_int(x)` | `x.round_ties_even() as i64` |
| `_isnan`, `_isinf`, `_isfinite`, `_copysign`, `_sqrt` | `is_nan`, `is_infinite`, `is_finite`, `copysign`, `sqrt` |

Float arithmetic (`+ - * /`, `abs`, comparisons, `float(int)`) is on
Python floats, which are IEEE binary64, so it is `f64` arithmetic
natively. The SHARC rules as the core implements them today, which a
translation reproduces rather than fixes:

- A 32-bit float operation computes in double from the two float32
  inputs, then rounds once to float32 with round-to-nearest-even
  (`_float32_bits`). Native code keeps the double step; computing in
  `f32` would round differently.
- Overflow of that rounding gives signed infinity and reports AV (MV for
  the multiplier).
- A NaN input gives the all-ones result `0xFFFFFFFF` and reports AI; a NaN
  made by the operation from ordinary inputs (infinity minus infinity)
  keeps the pattern the host computes and rounds, so it depends on the
  host's default NaN and on how the Python version packs a NaN into
  float32. A translation should pick one pattern and the core should
  then name it; until then such results are not bit-exact across hosts.
- Denormals are not flushed, except where an operation's page documents
  it and the helper does it: `scalb` and `float ... by`, `copysign`,
  `rnd`, `mant`, `logb`, the recips/rsqrts seeds. The 40-bit extended
  format and MODE1.RND32 are not modelled.
- `fix` rounds to nearest even unless MODE1.TRUNCATE; `trunc` truncates;
  out-of-range results saturate or give all ones according to
  MODE1.ALUSAT; an unknown mode bit gives an Unknown result.
- 64-bit operations compute in double directly, with no flush.

**Flags.** A compute returns its ASTATX/ASTATY effect as a `FlagUpdate`
(define mask and bits, forget mask, and the CACC bit of a known compare),
never as a function. `flags.py` builds them; `_apply_flag_update` applies
them. Combining two is `_flags_or` (dual add/subtract) or `_flags_then`
(one after the other).

**Containers.** The machine state lives in the `State` containers
(`uregs`, `special`, `overlay`, `mmrs`, `loops`, `call_stack`,
`status_stack`). Constant tables are module-level `dict`/`tuple`
literals. Inside a function, no `dict` or `set` is built (`dict-build`);
the state containers are read and written with literal or computed keys
(`state.special["MRF"]` is a struct field natively). The special
registers may be absent: `special if special is not None else NO_SPECIAL`.
A short sequence (a register pair's two addresses) is a list built by
`append` in a `for` loop over `range` or a tuple; no comprehensions
(`comprehension`). No `sorted`, `round`, `pow`, `divmod`, `hash` or `**`
on a non-literal (`host-builtin`).

**Traps.** `raise` (usually `ValueError` for an unsupported encoding, or
`UnmodeledMMR`) is a trap. The only `try` is the trap-to-stop form: every
handler catches `ValueError` or `UnmodeledMMR` and just returns
(`return [_stop(...)]`, or the error text to a caller that stops).
Natively a trap returns to the Python core, which re-executes the
instruction. `assert` is erased.

**Observability.** `_event`, `_stop`'s reason, `Unknown`'s reason, the
trace log (`state.trace[...]`), and the string parameters that only feed
them (`expression`, `label`, `rendered`) are erased by a translator.
Their arguments must therefore have no effect (`observability-effect`):
a store whose result is logged is made first (`wrote = _dm_write(...)`)
and the log gets the result.

## 4. Survey: violations per module

Counted by `tools/sharc_subset_lint.py` with the rules above. "Before" is
the core as copied from the main tree on 2026-09-28 (before this
conversion); "after" is now. Rule columns: obs = observability-effect,
lam = lambda, clo = closure, it = isinstance-type, is = isinstance-symbolic,
sm = struct-math, try, dict = dict-build, comp = comprehension,
dyn = dynamic, hb = host-builtin.

| module | obs | lam | clo | it | is | sm | try | dict | comp | dyn | hb | before | after |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| compute.py | | 1 | 1 | | | | | 5 | 1 | | | 8 | 0 |
| compute_alu.py | | 15 | | | | 3 | | 3 | | | | 21 | 0 |
| compute_mult.py | | 5 | 5 | | | 7 | | 9 | | | | 26 | 0 |
| compute_multi.py | | 10 | 2 | | | | | | | | | 12 | 0 |
| compute_shift.py | | 10 | | | | | | 12 | | | | 22 | 0 |
| flags.py | | 1 | 12 | 1 | 2 | 2 | | 14 | 1 | | | 33 | 0 |
| floats.py | | | | | | 60 | 5 | 3 | | | 2 | 70 | 0 |
| forms.py | | | | | | | | 3 | | 1 | 1 | 5 | 0 |
| forms_compute.py | 1 | | | | | | | 5 | | | | 6 | 0 |
| forms_dag.py | | | 1 | | 1 | | | 1 | | | | 3 | 0 |
| forms_flow.py | | | 3 | | | | | 3 | | | | 6 | 0 |
| forms_move.py | 13 | | 5 | | | | | 18 | 14 | | | 50 | 0 |
| forms_system.py | | 6 | | | | | | 2 | 3 | | | 11 | 0 |
| memory.py | | | 2 | 1 | | | | 1 | 3 | | | 7 | 2 |
| sequencer.py | | 3 | 1 | | | | | | | | | 4 | 0 |
| state.py | | 2 | | | | | | | | | | 2 | 0 |
| **total** | 14 | 53 | 32 | 2 | 3 | 72 | 5 | 79 | 22 | 1 | 3 | **286** | **2** |

(The "before" numbers of flags.py include `_astatx_define` and
`_astatx_forget`, which moved into the `values.py` lattice.)

## 5. What remains

- `memory.py` (allowlisted, 2): `_concrete_address(value: Value | int)`
  tests `isinstance(value, int)` because callers pass either a register
  value or an already concrete address; `_load_normal_ureg` returns a
  `{"PX1": ..., "PX2": ...}` summary for a combined-PX load. Both need a
  signature change across callers outside the core (`sharc_harness`,
  `sharc_lp0`, `sharc_replay`).
- Not checked by the lint and left to the translator: integer widths and
  shift counts (section 3), that string-typed values only feed erased
  observability, and list lengths.
- The concrete runtime for the boundary (section 2) is not written yet.
