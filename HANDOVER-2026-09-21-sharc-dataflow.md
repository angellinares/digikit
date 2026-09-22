# Handover 2026-09-21: SHARC dataflow and runtime-state writers

The goal is still to map machine IDs and parameters to their actual SHARC DSP
implementations, so existing machines can be changed or augmented and new
machines may eventually be added. This handover supersedes the dataflow status
in `HANDOVER-2026-09-21-sharc-engine.md`; that file remains useful for the
broader engine inventory and named DSP primitives.

Results belong in `docs/findings/`, indexed from `docs/FINDINGS.md`. Firmware,
extracted sections, Ghidra projects, and everything under `out/` remain ignored
and must never be committed.

## Repository state

Branch: `machine-ideas-menu`.

Latest commits:

- `3d663f2 Trace SHARC runtime state writer chain`
- `a684f0f Document SHARC runtime state forwarding`
- `e1eb34a Add headless SHARC dataflow queries`
- `83ab9b5 Map SHARC engine paths and add deterministic queue`

The tree was clean immediately after `3d663f2`. This handover file is
intentionally uncommitted, per Em's request.

Current validation at `3d663f2`:

- `624 passed, 5 skipped, 187 subtests passed`
- `git diff --check` clean
- deterministic engine queue: 51 candidates, 12 documented, 39 undocumented
- queue SHA-256:
  `86a3124524167d37046f62b1b3ddba0cda153162d55de48bf672848c7d0ce8a9`
- independent documentation review found no inaccurate claims or overclaims

## What was added

`tools/ghidraq.py` now has deterministic headless queries:

- `pcode PC`
- `slice PC SELECTOR`
- `stores TARGET [LO HI]`
- `loads TARGET [LO HI]`

Selectors include `store:value`, `store:address`, `load:value`,
`load:address`, `input:N`, `output`, and `reg:NAME`. SHARC short-word
coordinates are explicit as `sw:...`; unprefixed external addresses remain
Ghidra byte offsets. Exact memory matching includes address-space identity and
normalizes direct-memory COPY operations.

Targeted SLEIGH semantics were added only for the forms that blocked this work:

- Type14a scalar direct DM load/store
- exact Type3b reader `493e0e3f`, `I12 = DM(I4,M4) u=0`

Tests are in `tests/test_ghidraq.py` and `tests/test_sharc_pcode.py`; usage is
documented in `docs/TOOLS.md`.

## Verified DT2 writer chain

In both DT2 1.15C and DT2 1.16, `blk93@0x1c18a6` contains identical raw
instructions:

```text
0x1c18ed  100200252658  R2 = DM(0x252658)
0x1c18f3  100400254d9c  R4 = DM(0x254d9c)
0x1c191d  110200254d9c  DM(0x254d9c) = R2
0x1c1928  110400254d98  DM(0x254d98) = R4
```

Canonical section-replay decoding and independent byte review found no
intervening R2/R4 destination or control transfer in this bounded straight-line
interval. Therefore the byte-proven assignments are:

```text
DM(0x254d9c) <- DM(0x252658)
DM(0x254d98) <- old DM(0x254d9c)
```

This is consistent with a two-word state/history shift, but that interpretation,
natural execution, ownership, and any machine-type relation remain open.

Ghidra HighFunction independently proves the R4 path as
`ram 0x4a9b38 -> value -> ram 0x4a9b30`. The R2 store exists in raw P-code,
but its HighFunction slice returns `no-seeds`; do not turn that limitation into
a negative dataflow claim.

The prior open question “who writes `0x254d9c`?” is closed for the direct local
writer: `0x1c191d` writes R2. The new highest-value unresolved source/owner is
`0x252658`.

## Exhaustive direct census for `0x252658`

For both DT2 versions, an exhaustive even-offset canonical Type14a `g=0`
census found exactly one direct literal access to `DM(0x252658)`:

```text
0x1c18ed  R2 = DM(0x252658)
```

There is no direct Type14a writer in either image. This is verified only for
that instruction form and exact address. It does **not** exclude:

- Type3/indexed stores
- immediate-offset or dual-memory stores
- an I-register-built address
- loader/startup initialization
- ColdFire/host writes

Ignored evidence:

- mapping audit:
  `out/ghidraq-dataflow/writer-batch/mapping-audit/mapping-audit.json`
  SHA-256
  `24441e37f2933e1a8c561d5182b98f59812549e70df0b1251057f4b09e9f10d9`
- DT2 1.15C census:
  `out/ghidraq-dataflow/writer-batch/upstream/dt2-1.15C/dm-252658.json`
  SHA-256
  `1bbb5eaed7ec762d4a3692e851ae8b319e212b9f37ad8134c90c76f07b259b91`
- DT2 1.16 summary:
  `out/ghidraq-dataflow/writer-batch/upstream/dt2-1.16/summary.json`
  SHA-256
  `ba1e830b04c8b79e283ff4305990d6bbf20d24b049297a2b52f97167400305ca`

## Caller context

The documented static call is:

```text
0x1c308d  R4 = caller frame[-17], R8 = caller frame[-18]
0x1c3090  call 0x1c18a6
```

It is in `blk93@0x1c2b24` and is unconditional only within an already reached
caller. This does not prove natural execution or exhaustive absence of indirect
callers.

## DN2 shifted candidate

An exhaustive DN2 1.11 canonical scan found no direct Type14a access in the DT2
`0x254d00..0x254dff` range. That is a bounded encoding result, not architectural
absence. DN2 instead has a structurally similar candidate at shifted addresses:

```text
0x1c19d7  10040025d010  R4 = DM(0x25d010)
0x1c1aaf  11040025d00c  DM(0x25d00c) = R4
```

The source/destination delta is again -4. The containing function is
`blk83@0x1c1738`, bounds `0x1c1738..0x1c1e1f`, 659 instructions. Its vector is
close to DT2: 3 calls, 204 loads, 204 stores, 33 MACs, one nested loop; DT2 has
654 instructions.

Treat this only as structural analogy. At the correct DN2 PCs, HighFunction's
semantic location is inconsistent with canonical bytes and does not prove that
the same runtime R4 value flows between these instructions. Do not claim R4
preservation, object identity, or machine identity.

DN scan:
`out/ghidraq-dataflow/writer-batch/upstream/dn2-1.11/canonical-replay-type14a-g0-scan.json`,
SHA-256
`fe05c06f323b41595e155d7238b92444c784e22e56ce04ed7a36420584cb5cb7`.

## Cross-image batch and projects

The first cross-image batch is under
`out/ghidraq-dataflow/batch/combined/`:

- `machine-map.json`
- `machine-map.sqlite`
- `summary.md`

It covers 3 images, 24 normalized queries, 72 results, 26 P-code operations,
4 roots, 2 exact matches, and 0 query errors. Per-image repeats were
byte-identical.

Existing isolated Ghidra projects:

- `~/ghidra-projects/sharc-batch-dt2-115c`, project
  `sharc-batch-dt2-115c`, program `/dt2-1.15C_SHARC`
- `~/ghidra-projects/sharc-batch-dt2-116`, project
  `sharc-batch-dt2-116`, program `/dt2-1.16_SHARC`
- `~/ghidra-projects/sharc-batch-dn2-111`, project
  `sharc-batch-dn2-111`, program `/dn2-1.11_SHARC`

One JVM per project at a time. Chain queries with `--then` rather than starting
multiple JVMs against one project.

Source blob SHA-256 values:

- DT2 1.15C:
  `6d4316cddd41edef7a136c10d270313882028a59b96cc8fe97a716b949a7d551`
- DT2 1.16:
  `0f514a12a2255f5c081e292c47f1f29462003177658da4bbae0a22fd737fffa2`
- DN2 1.11:
  `336e340aa0cdcd34e314cfa44849f709a3134f6bd4cd57dfc7e15702c83115e2`

## Address-mapping trap

Do not infer flat-image offsets from a guessed main-program base. Use
`sharcldr.LoadedMemory`/canonical section replay. The audited mapping is:

```text
flat_offset = (2 * PC + 0x28000000) - main_program_loader_byte_base
```

A review that assumed `2 * (PC - 0x1c1338)` incorrectly read zeros at
`0x1c1928`; the mapping audit proved that both DT2 stores are present and that
all three `*-main.bin` files exactly match `sharcldr.main_program()`.

## Other still-open dataflow blocker

The three computed readers around external addresses `0x8055c840`,
`0x8055c858`, and `0x8055c874` remain unresolved. Raw instruction P-code shows
the Type3b memory load, but HighFunction can omit or mislocate it, so
`load:address`/`load:value` may return `no-seeds`. `I6+124` identity is also
open. These are decompiler/control-flow limitations, not absence evidence.

`0x254d98` is an argument/state location, not the jump-target table. The actual
indirect targets are fetched from runtime structures around the three
`0x8055c8xx` addresses.

## Status of the previous next batch

Steps 1-3 are done and the answer changed. `tools/ghidraq.py` was dropping
every HighFunction `COPY` with direct memory on both ends, which is exactly
how Ghidra folds `R2 = DM(0x252658); DM(0x254d9c) = R2`. That, not a
decompiler limit, is why the R2 store returned `no-seeds`. With the classifier
fixed (`COPY_MEM_TO_MEM_WRITE`/`COPY_MEM_TO_MEM_READ`, `tests/test_ghidraq.py`)
Ghidra dataflow now proves the R2 path, and an unbounded whole-image `stores
sw:0x252658` still finds no genuine writer in either `sharc-batch-dt2-116` or
`elektron-sharc`.

`0x252658`, `0x254d98` and `0x254d9c` are all inside one zero-FILL loader
block (DT2 block 18 at `0x282412c0`, 86216 bytes, fill value 0; DN2 block 16
at `0x28241290`), second-agent byte-verified in all three images, with no
later block re-covering them. All three words are zero at boot. So the loader
supplies nothing and no *direct* SHARC store writes the address. The Ghidra
sweep cannot see indexed stores at all; see the next section.

Two mapping traps cost time here and are now recorded in
`docs/findings/06-sharc-engine-and-startup.md`: for data blocks the loader
byte address is the Ghidra displayed address plus `0x28000000`, and
`sharcldr --addr` used to print "not covered by any loaded block" for an
address inside a FILL block. `fill_block_for_address` now reports fill
coverage separately.

## Highest-value next batch

**[C]** The Ghidra sweep did not bound the owner as far as the previous
version of this section claimed. Every indexed DM form (`15b`, `3c`, `4a`,
`3a`, most `3b`) has an empty SLEIGH semantic body, so it emits no p-code, and
neither the decompiler queries nor an emulator can see a store made through
one. The owner is still one of: an indexed SHARC store, a ColdFire/host write,
a DMA path, or code outside Ghidra's function boundaries.

Semantics are now the shared bottleneck for both routes. Only 22% of the
DT2 1.16 main program lifts to any p-code; the ranked worklist is
`out/sharc-semantics/worklist.md`. Ghidra's `EmulatorHelper` does run this
language, but a form with no semantics executes as a silent no-op instead of
faulting. Results are in `docs/findings/05-sharc-isa-and-decoding.md`.

1. Done: `tools/sharcemu.py` runs Ghidra's `EmulatorHelper`, faults on any
   instruction that lifts to zero p-code ops (except Type21a and
   Type9a/9b_abs with `b==1`), and reports writes to watched addresses by
   address, so same-value writes count. Its positive control reports the
   store at `sw 0x1c191d`.
1a. Done: DM byte addresses are translated to `ram` units in the language,
   and external blocks sit at their own address
   (`docs/findings/05-sharc-isa-and-decoding.md`, last section). Use the
   fresh project `~/ghidra-projects/sharc-dm-dt2-116` (project name
   `sharc-dm-dt2-116`); `sharc-batch-*` and `elektron-sharc` were imported
   with the old language. Pass data addresses as plain byte addresses. New
   memory-form semantics must build their address with
   `dm_byte_addr_to_ram_unit` in `gen_sleigh.py`. DN2 1.11 and DT2 1.15C have
   not been reimported.
2. Done for memory: the DM cases of 15b, 4a and 3a have semantics
   (`docs/findings/05-sharc-isa-and-decoding.md`, last section), with
   `compute`, `condition` and `circular` as explicit unimplemented ops.
   Coverage is 51% of the image. `condition` cannot be implemented until the
   language models the ASTAT flags, and the flags need compute semantics, so
   the next language work is the compute field (ALU, multiplier, shifter),
   mirroring `tools/sharc_trace.py`'s `_compute` and `_astatx_*` helpers. It
   is now the emulator's main fault (139 of 301 in `FUN_001c18a6`). Measure
   each language change with `tools/sharcpcode.py measure` and `compare`, and
   re-run `tools/sharc_worklist.py`.
3. `tools/sharcwriters.py 0x252658` censuses all 12,634 DM stores and finds
   no writer among those it resolves, but 7,492 stay unresolved and 224
   depend on caller registers (`docs/findings/06-sharc-engine-and-startup.md`,
   "A zero-initialiser also writes `0x254d9c`"). Next, in order of yield:
   (a) the stack context switch: every writer of I6/I7/B6/B7 keeps them in
   the stack range except `blk69@0xb8853a`, which sets `I7 = I11 + 0x204`
   (sw `0xb885f5`/`0xb885f7`) from a context structure reached through the
   global pointer `DM(0x2ca3e0)`. Until that is resolved, all 4,389 stack
   exclusions are conditional (`out/sharcwriters/252658-v3.json`,
   `excluded_stack_depends_on_unproven_entry_assumption`). `0x2ca3e0` lies
   in loader payload, so read its initial value, find its writers, and bound
   the context structures and their stacks; if they are a fixed table far
   from `0x252658`, widen S with that evidence and the exclusions become
   unconditional. This is also the first sign of multiple execution
   contexts on the SHARC, which matters for how machines are scheduled;
   (b) Type16b semantics in the language, so the new `0x254d9c` writer is
   checked live; (c) tracer support for Type11a and compute opcode `0xa5`,
   and confirmation of the 627 uncertain forms; (d) chase the 224
   caller-relative stores through their callers. Only if nothing survives,
   take the ColdFire side: the machine type reaches the
   SHARC at TX frame offset `0x94 + 2i`
   (`docs/findings/04-coldfire-dsp-link.md`), and `FUN_001c2b24`, the
   documented caller of `0x1c18a6`, reads it at `0x1c33d2` and writes the
   neighbouring words `0x252650`/`0x252654`.
4. Then return to `I6+124` and the `0x8055c840/58/74` table-reader control
   flow.

Verify any finding against image bytes with a second agent before marking it
`[V]`. Extend SLEIGH form by form for forms that block this work -- the
indexed DM forms now do -- and measure each change. Do not write a custom SSA
engine.

## Useful commands

```bash
# Tests
uv run --with pytest python -m pytest tests -q

# Raw instruction and slices
uv run python tools/ghidraq.py SHARC_PROGRAM pcode sw:0x1c191d
uv run python tools/ghidraq.py SHARC_PROGRAM slice sw:0x1c191d store:value
uv run python tools/ghidraq.py SHARC_PROGRAM stores sw:0x252658 LO HI

# Function dossier
uv run python tools/sharcfn.py \
  out/sections/dt2-1.16/section_7_BLOB.bin 0x1c18a6

# Deterministic queue
uv run python tools/sharcfn.py \
  out/sections/dt2-1.16/section_7_BLOB.bin \
  --engine-queue \
  --sqlite out/sharcpcode/l2-cross-002/dt2-1.16.sqlite \
  --notes-dir docs/findings/functions \
  --json /tmp/queue.json
```

## Read first in the next context

1. `CLAUDE.md`
2. this handover
3. `docs/findings/functions/blk93-1c18a6.md`
4. `docs/findings/06-sharc-engine-and-startup.md`, especially the latest
   `[C] Bounded cross-image forwarding at 0x1c18a6` section
5. `tools/ghidraq.py` and `docs/TOOLS.md` before changing query behavior

Do not commit this handover unless Em explicitly asks later.
