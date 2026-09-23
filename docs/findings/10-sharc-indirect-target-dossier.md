# SHARC indirect-target dossier: `FUN_1c642a`

This is a bounded static dossier for the non-delayed Type9b transfer at
short-word PC `0x1c6579` in DT2 1.16 section 7 (blob SHA-256
`0f514a12a2255f5c081e292c47f1f29462003177658da4bbae0a22fd737fffa2`). It
uses short-word PCs; the Ghidra SQLite addresses below are doubled. All claims
here are single-review results and therefore **[D]** or **[O]**.

## Bounds and transfer

- `FUN_1c642a` / `blk93@0x1c642a` spans `0x1c642a` through its return/delay
  completion at `0x1c71ec` (1458 instructions). It is the 5366-byte Ghidra
  function at `0x38c854`. **[D]**
- The exact four bytes at `0x1c6579` are `3f 08 3f 28` (the stream's
  little-endian parcel order); they decode as a `9b_abs` unconditional,
  non-delayed indirect `JUMP`. The successor PC is selected immediately:
  there are no delay-slot instructions to preserve. **[D]**
- The Type9b fields select DAG2 `I(8 + pmi) = I12` and `M(8 + pmm) = M13`.
  Thus its short-word effective target is `(I12 + M13) & 0xffffff`, not the
  `PM(I4, M5)` spelling currently rendered by `sharcfn.py`'s listing. **[D]**
  This is distinct from the known `MODIFY` renderer issue: a Type7/19 MODIFY
  destination is `Is XOR idis` (plus its DAG bank), not simply `Is`. **[D]**

## Bounded state and raw target

Starting `sharc_trace.py` at `0x1c642a` with the handover inputs
`R8=0x2506ec` and `R4=0x2412c8`, bounded at 200 instructions, forks on
unresolved predicates. Three paths stop in external calls; two paths reach
`0x1c6579` after 149 and 151 steps respectively and stop exactly with
`unknown 9b_abs indirect target through I12/M13`. Two other paths stop on
unsupported form `7b` after 155/157 steps. These stops are tracer limitations,
not dead-path evidence. **[D]**

On each path reaching the transfer, the preceding slice is:

```
0x1c6569  I4  = 0x8055c840
0x1c656c  I12 = DM(I4, M4)       ; normal-word table access
0x1c656e  I4  = I14
0x1c6570  R1  = 0x463b8400
0x1c6573  R0  = 0x3f800000
0x1c6576  R8  = 0x42700000       ; 60.0f bit pattern
0x1c6579  JUMP (M13, I12)        ; non-delayed
```

The table-read effective address is `0x8055c840 + 4*M4` under the
normal-word assumption; it is not based on the later `I4=I14` move. The
tracer leaves `M4` and hence `I12` symbolic in this setup, so it does not by
itself select a table element. Startup documentation records `M13=0`; with
that state the transfer target is exactly the fetched `I12` value. **[D]**

The raw loaded bytes independently identify code-pointer entries beginning at
the literal table base `0x8055c840`: bytes `bd 65 1c 00` are the little-endian
normal word `0x001c65bd`, followed by `0x1c6715`, `0x1c6782`, and
`0x1c686e`. Under the 32-bit normal-word assumption, the `M4=0`, `M13=0`
case therefore resolves to short-word target **`0x1c65bd`**; `M4=1` selects
`0x1c6715`, and so on. A minimal transfer trace seeded only with
`I12=0x1c65bd`, `M13=0`, and a breakpoint at that PC records one executed
non-delayed branch directly to `0x1c65bd`; its preserved seeded context
includes `R4=0x2412c8` and `R8=0x42700000`. The natural value of `M4` and the
runtime dispatch choice remain **[O]**.

The deterministic `tools/sharc_discover.py` composition harness checks the
blob hash and final loader marker, applies final loader-memory provenance to
non-fill pointer words, and expands the manifest's all-entry `M4=0..19`,
`M13=0` hypothesis sweep directly from its in-memory dispatch/table join. It
records 20 resolved structural target contexts, each requiring a unique strict
trace terminal state at that entry's expected target. This is not an
observation of a natural runtime selector: the natural `M4` value and dispatch
choice remain **[O]**; the all-entry structural result is **[D]**.

At `0x1c65bd`, the selected code begins `R12=pass(R9)`, then establishes
`R11=0x38aec33e` and `R14=0x463b8000`; the transfer itself does not establish
live `R9`/`R11` values. The earlier `R4` and overwritten `R8` are live
register-context facts, not a complete calling convention. **[D][O]**

## Static context

SQLite identifies one direct caller of `FUN_1c642a`: `FUN_1c2b24` at doubled
site `0x386106` (`0x1c3083` short-word), and records 28 direct call edges with
this function as caller. The function is consequently the large per-frame
orchestrator context, while this Type9b edge is indirect and absent from the
direct-call table. **[D]** Ghidra's disassembly retains the preceding
`I4=0x8055c840` literal at doubled site `0x38cad2`. Its `data_refs` rows for the
load at `0x38cad8` instead name `0x8055c7e0` and `0x8055c820`; because neither
is the exact literal base, those generated xrefs are not used as confirmation
of the pointer table. **[D]**

## Hook ranking

1. **`0x1c65bd` table-selected internal leg — provisional best internal hook
   candidate.** It is reached at per-orchestrator granularity after the table
   dispatch for the unresolved `M4=0` selection, has a concrete raw table
   entry, and preserves the useful context `R4` plus the `R8=60.0f` setup. It
   ranks above an entry hook at
   `0x1c642a` for narrower granularity. Risks: it is an internal, table-selected
   address rather than a stable function entry; `M4` chooses other entries;
   `R9` is consumed immediately; and any replacement must retain the
   non-delayed transfer semantics and the downstream register state. **[D][O]**
2. **`0x1c642a` entry — fallback only.** Its sole known direct caller and
   per-frame placement make entry stability better, but its 1458-instruction
   scope is much coarser and its prologue saves extensive state. A hook there
   must preserve the stack/circular-address state and all later dispatch
   register setup. **[D][O]**

Neither candidate is established as a machine-specific or per-voice hook.
Finding the runtime producer of `M4` and independently reviewing the pointer
table bytes are required before treating the ranking as implementation-ready.
**[O]**

## Selector provenance boundary

The deterministic discovery manifest now starts an additional strict trace at
`0x1c6553`, rather than seeding `M4` after the copy. The loaded instruction
encoding `707f1200b062` there is traced as the unconditional register copy
`M4=R6`. In each target-reaching forced trace, the audit retains that copy,
finds no later `M4` write before the `0x1c656c` table load, and records the
`0x1c6579` branch. This establishes a local, forced-dataflow chain
`R6 -> M4 -> DM(0x8055c840 + 4*M4) -> I12 -> JUMP(I12+M13)`. **[D]**

The manifest seeds `R6=0..19` and `M13=0`; it preserves all terminals rather
than collapsing them. For each seed, one terminal reaches its corresponding
table target, but unresolved conditional routing also leaves non-target
terminals. The report consequently classifies each as existential
`target-reached`, with `selector_origin: manifest-seed`,
`runtime_occurrence: not-observed`, and `natural_runtime_observed: false`.
In particular, values 15--19 are only counterfactual-reachable (targets
`0x1c7395`, `0x1c73b0`, `0x1c73cb`, `0x1c73e3`, and `0x1c742a` respectively);
they are not demonstrated live-audio selections. **[D][O]**

## Candidate IVT region

Loader block 70 begins at loader byte address `0x28240000`. Applying the
`sharcldr.sw_to_byte` inverse maps that address to short-word PC `0x120000`,
not `0x1c2000`; the manifest candidate scan now starts at `0x120000` and
validates that its declared start maps to block 70's loader-final bytes. With
no processor-specific entry count, it does not decode candidate slots. This
only corrects the address-space conversion. The candidate base, vector identities, core
semantics, SPORT semantics, and audio semantics remain unverified or unknown;
it is not an active-IVT claim. **[D][O]**

## Natural-selector frontier at `0x1c351a`

**[C]** The prior statement that the two tail dependencies were not
loader-final was an address-space error. Low-DM literals use the loader alias
`0x28000000 + address`: `DM(0x2560c4)` is loader address `0x282560c4`, a
zero-initialized word from loader block 18; `DM(0x256c98)` is
`0x28256c98`, where loader block 21 supplies the four little-endian normal
words `0x1c351c`, `0x1c352f`, `0x1c353e`, and `0x1c354d`. The loaded code at
`0x1c3507`, `0x1c3515`, `0x1c3518`, and `0x1c351a` establishes
`M4=R4`, `I4=0x256c98`, `I12=DM(I4,M4)`, then a non-delayed `JUMP(I12+M13)`.
The `M4` offset is a normal-word offset (four bytes). **[D]**

Those four entries are **loaded target candidates**, not an exhaustive runtime
target set. They begin wrappers which directly call `0x1c349b`, `0x1c347e`,
`0x1c3474`, and `0x1c3464` respectively. The persistent index now generates a
tri-state `R6` effect result for each table-entered wrapper and its declared direct callee.
`preserved` requires complete supported control flow with no `R6` writer;
`written` requires a concrete decoded writer; an indirect edge, unsupported
form, external call, decode gap, or truncated path yields `unknown`. Current
report values must be read from that generated audit and are not runtime
observations. The current incomplete paths remain **[O]** rather than a
calling-convention claim.

The deterministic discovery report records this frontier with both DM
spellings, source loader blocks, the exact tail decode, candidate legs, and
incomplete writer coverage. Its bounded `R4` target probes start at the loaded
setup boundary `0x1c3507`, seed counterfactual `M13=0`, then sweep `R4=0..3`
without seeding `I12`. Under the manifest's 64-step/16-state bounds, each case
retains a return-without-followed-call terminal and an existential breakpoint
terminal at its corresponding loaded target (`0x1c351c`, `0x1c352f`,
`0x1c353e`, or `0x1c354d`); the forced traces include the `R4` copy at
`0x1c3507`, table load at `0x1c3518`, and branch at `0x1c351a`. These are
counterfactual reachability facts, not seeded-selector, guarded-return, or
natural-runtime claims. The range guard condition, an upstream `R4` bound, and
table mutation remain unproven. The writer census has no exact hit but retains
7,456 unresolved stores and does not model external writers; the `0x1c306f`
bulk-copy range is also not yet bounded. **[D][O]**

The loaded tail is no longer an unknown *loader initialization* boundary, but
its runtime target set and `R6` effect remain unproven. No natural selector
value or runtime occurrence is observed. A minimal future capture would record
`DM(0x2560c4)`, `DM(0x256c98..0x256ca7)`, `R4`/`R6` at `0x1c351a`, and the
selected tail target. **[O]**
