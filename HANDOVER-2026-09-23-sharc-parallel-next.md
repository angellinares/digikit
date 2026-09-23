# Handover 2026-09-23: SHARC semantic convergence and next parallel wave

This handover supersedes the execution state in older root handovers. It is a
continuation guide, not a findings document. Durable results are under
`docs/findings/` and indexed by `docs/FINDINGS.md`.

Read `CLAUDE.md`, this file, `docs/FINDINGS.md`, and findings 05, 06, 10, 11,
and 12 before changing SHARC semantics or evidence.

## Goal

Understand and reverse engineer the SHARC+ DSP shared by Digitakt II and
Digitone II well enough to disassemble and lift it reliably, execute meaningful
DSP paths, identify machine/audio boundaries and safe hooks, and eventually
run project-owned machine/DSP code patched into firmware.

Every semantic change must be public-manual-supported, checked against real
bytes when it makes a firmware claim, measurable, deterministic, and explicit
about unknown hardware behavior.

## Repository state

- Branch: `work/machines-and-dsp-hooks`
- HEAD: `834152f` (`Cover bounded Type12a loop counts`)
- This handover is intentionally untracked and uncommitted.
- Expected status: only this file is untracked.

Recent commits:

```text
834152f Cover bounded Type12a loop counts
4f00dfd Lift pure SHARC ACONV operations
5e5d8b2 Propagate symbolic SHARC ACONV values
1d52337 Add incremental SHARC writer fact cache
777e959 Extend SHARC writer and cross-image analysis
```

Latest full validation:

```text
874 passed, 7 skipped, 206 subtests passed
```

Run it with:

```sh
uv run --with pytest python -m pytest tests -q
```

## Non-negotiable constraints

1. Never commit firmware or derived artifacts (`*.syx`, `sections/`, `out/`,
   snapshots, generated databases/reports).
2. Never name, copy, or quote the prohibited private DSP toolchain. Use only
   public manuals listed in `docs/sharc/SOURCES.md`.
3. Findings belong under `docs/findings/`, not in handovers.
4. A firmware claim becomes `[V]` only after a second agent checks image bytes
   and provenance independently.
5. Empty Ghidra callers/references are not absence evidence; use raw scans for
   important negatives.
6. The typed project ISA is authoritative. Generated SLEIGH is an adapter.
7. Keep encoding, architecture, tracer approximations, p-code, firmware
   interpretation, and runtime evidence distinct.
8. A manual's “likely” semantics is not hardware equivalence.
9. One JVM per Ghidra project at a time.
10. Avoid emulator runs when static/synthetic work is sufficient; otherwise
    verify the source hash and use `--limit`.
11. Do not commit or push unless Em asks.

## Completed work

### Per-function writer cache (`1d52337`)

`tools/sharc_index.py` now uses schema/contract v5 and stores writer facts per
recovered function. Facts carry identity, store shape, digest/size, completion,
forms/blockers, handler revisions, policy, and stable ordering.

Selective invalidation distinguishes core changes, per-form semantic changes,
and provisional-policy changes. Corrupt/incomplete rows fail closed;
publication is transactional and deterministic. This is the primary speedup
for future semantic iterations.

See `docs/findings/12-sharc-writer-function-cache.md`.

### Native Type7d symbolic ACONV (`5e5d8b2`)

The native tracer now propagates:

- affine W2B by multiplying by four;
- B2W only when all affine parts are four-aligned; and
- uncertain B2W as a stable opaque source-derived symbol.

Unknown/partial sources still stop. Address-map lookup, retain-on-miss, and
ILAD are intentionally not modeled. Type7d has a dedicated handler revision
for selective cache invalidation.

The bounded worker run reported 239 direct Type7d blockers progressing to
later blockers; this does not mean 239 functions became fully resolved.

### Generated pure Type7d p-code (`4f00dfd`)

`tools/sharcspec/ghidra/gen_sleigh.py` emits 512 pure-selector constructors:

```text
2 DAG banks × 2 register classes × 2 directions × 8 sources × 8 idis
```

They cover I/B classes, both DAG banks, B2W/W2B, and XOR destination mapping.
Conditional/compute-bearing Type7d remains excluded.

Across DT2 1.15C, DT2 1.16, and DN2 1.11, Type7d changed from zero to one
semantic p-code op per pure instruction, adding eight ops per image with no
reported comparison regressions. Focused p-code/backend validation ended at
49 passed and 56 subtests.

### Exact ACONV limitation

For real DT2 bytes `bf0480c00000` at short-word PC `0x1c1460`, the generated
likely-shift p-code computes:

```text
0x26f7f0 >> 2 = 0x09bdfc
```

The documented observed conversion is:

```text
0x26f7f0 -> 0x09be7c
```

The public manual labels shifts “likely,” says exact behavior depends on the
address map, and specifies retain-input plus ILAD when no equivalent exists.
The reviewed public pages do not provide a complete model-specific map or
precise interrupt timing. Firmware observations cover only one aligned
reversible segment. Do not infer a general map from it.

Defer exact ACONV until a public model-specific map or controlled hardware
evidence exists. Prefer an explicit opaque/fail-closed semantic over p-code
that falsely claims exactness.

### Type12a concrete boundary (`834152f`)

Production code already handled immediate and concrete UREG loop counts. This
phase added boundary tests/documentation for UREG count 1, maximum immediate
`0xffff`, retained mode values, and conservative zero/nonconcrete stops.

No production semantic source changed. E2/F1 pipeline timing, zero-count
behavior, and symbolic loop counts remain open.

## Image provenance

| image | SHARC blob SHA-256 |
| --- | --- |
| DT2 1.15C | `6d4316cddd41edef7a136c10d270313882028a59b96cc8fe97a716b949a7d551` |
| DT2 1.16 | `0f514a12a2255f5c081e292c47f1f29462003177658da4bbae0a22fd737fffa2` |
| DN2 1.10E | `174b391822bbe33a99e5f42bd02ef3ea75c0e79351caae6ba90e4cd2c4de5350` |
| DN2 1.11 | `336e340aa0cdcd34e314cfa44849f709a3134f6bd4cd57dfc7e15702c83115e2` |

Recompute local hashes before relying on an ignored artifact.

## Current function-level blocker frontier

The last inspected post-Type7d v5 cache contained 1,059 complete function
facts. Keep these counts separate from instruction counts, target rows, and
p-code operations.

| blocker | owning functions |
| --- | ---: |
| Type12a | 107 |
| Type9b absolute | 63 |
| Type6a | 34 |
| Type7a | 22 |
| Type9a | 7 |
| Type6b | 2 |
| Type20a | 1 |
| Type3a | 1 |
| Type9a absolute | 1 |

The inspected ignored cache was:

```text
out/sharc-index/type7d-aconv-after-final-worker.sqlite
```

Type12a is not a quick concrete-loop win: its frontier is primarily
nonconcrete UREG counts and needs sound symbolic reasoning or better constant
propagation. Never choose an arbitrary loop bound and call it resolved.

## Type11a preparation

Read-only research established the public control matrix sufficiently to plan
fixtures: `x=0` RTS, `x=1` RTI, `j` delayed return, `e` ELSE compute, and `lr`
loop reentry for RTS. Delayed returns execute two delay slots. Optional compute
must use the existing compute decoder. RTI must not be modeled as an ordinary
return without interrupt-state handling.

No Type11a semantic implementation landed. Extract and verify every real
DT2/DN2 Type11a word before editing.

## Next parallel wave

Parallelize read-only evidence and fixture work, then serialize one writer.

### Lane A: Type9b absolute control flow

- enumerate stop subtypes and owning functions;
- extract representative verified words;
- separate proven return idioms from general indirect branches;
- map delay-slot, call-stack, and PC-stack requirements; and
- propose one narrow subcase with function-level before/after measurement.

Never follow an unknown target merely because it resembles a return.

### Lane B: Type6a shift plus memory

- derive predicate, shift, transfer, width, and ordering semantics from public
  manuals;
- inventory exact DT2/DN2 variants and predicates;
- separate native-tracer and generated-p-code gaps;
- prepare exact synthetic and real-byte fixtures; and
- decide whether one used variant is safely implementable.

This is the favored quick implementation target if reconnaissance confirms a
straightforward variant.

### Lane C: Type11a returns

- extract all real Type11a words and verify hashes;
- group RTS/RTI, delayed/non-delayed, ELSE, LR, and compute combinations;
- map existing branch, delay-slot, loop, and interrupt helpers; and
- prepare the smallest correct subset and tests without editing shared code.

### Lane D: tracer/p-code/backend parity dashboard

For each high-impact form, report separately:

```text
typed decode status
native tracer semantics
generated p-code semantics
concrete backend executability
manual evidence status
real-byte fixtures
function-level blocker count
```

Use existing SQLite and `tools/sharcpcode.sql`; do not create another
pyghidra script.

### Selection and integration barrier

Choose exactly one subset using manual certainty, verified bytes, failing-test
feasibility, measured function impact, risk, and emulator relevance.

Preference:

1. a bounded Type6a variant if exact and straightforward;
2. a verified narrow Type9b idiom;
3. the smallest fully supported Type11a subset.

Use one writer, then independent manual/byte and metric/regression reviewers.
Only the writer applies accepted fixes. Stop after one semantic family.

## Parallelism/resource policy

Safe in parallel:

- manual research;
- raw-byte fixture extraction;
- SQLite queries;
- cross-image comparison;
- pypcode-only probes in separate output directories;
- test design; and
- independent review.

Serialize:

- shared semantic edits;
- generated-language installation;
- Ghidra JVM/project access;
- CPU-heavy discovery;
- final cache/report publication; and
- findings promotion.

Do not run several `--jobs 8` discovery jobs concurrently. The source checkout
is intentionally dirty only because this handover is untracked, so managed
worktree allocation may reject it. Do not silently stash/delete the handover;
use shared read-only scouts plus one writer unless Em explicitly handles it.

## Validation contract

For the next semantic phase:

1. capture a failing focused test/baseline first;
2. implement one semantic family/subset;
3. regenerate deterministically when applicable;
4. run focused tracer and p-code/backend tests as relevant;
5. build/compare a fresh v5 cache for affected functions;
6. keep count units distinct;
7. run `git diff --check`;
8. obtain independent manual/byte review before `[V]`;
9. obtain independent impact review; and
10. run the full suite once at the final barrier.

Commands:

```sh
uv run --with pytest python -m pytest tests/test_sharc_trace.py -q

uv run --with pytest,pypcode python -m pytest \
  tests/test_sharc_pcode.py tests/test_sharcemu_pypcode.py -q

uv run --with pytest python -m pytest \
  tests/test_sharc_index.py tests/test_sharcwriters.py -q

uv run --with pytest python -m pytest tests -q

git diff --check
```

Generated-language measurement:

```sh
uv run python tools/sharcpcode.py measure --out OUT_DIR
uv run python tools/sharcpcode.py compare OLD_DIR NEW_DIR
```

Add `--ghidra` only when needed and never overlap JVMs.

## First actions in the next context

1. Confirm HEAD `834152f` and that only this handover is untracked.
2. Read the files listed at the top.
3. Launch parallel read-only Type9b, Type6a, Type11a, and parity lanes.
4. Do not repeat broad ACONV-map research without genuinely new evidence.
5. Select one bounded semantic subset.
6. Give one writer an exact file/test/measurement contract.
7. Run independent evidence and impact checks.
8. Apply accepted corrections, run the full suite, and stop with a concise
   achievement/blocker update.

The likely quick win is Type6a. Type9b has greater potential impact but higher
control-flow risk. Type11a is the best prepared fallback and matters for
eventual emulator completeness.
