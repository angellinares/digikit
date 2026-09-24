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

The index's v4 query layer now persists target-independent per-function writer
trace facts, so a new writer target reclassifies the same raw events rather
than tracing all recovered functions again. Indexed discovery also rebuilds
only loader-final memory around the stored function inventory instead of
re-running the decoder/inventory analysis. An opt-in `--profile` side channel
writes phase timings to stderr and never enters report bytes. On this machine,
the same jobs=8 cold run fell from 97.84 s to 68.67 s, while a warm run fell
from 5.01 s to 1.25 s; a new writer target reused the 3.34 MB trace-fact row in
0.40 s without changing it. Reusing the integration test's baseline report and
filling its temporary index in parallel reduced the full local suite from
242.12 s to 80.23 s. These are local engineering measurements, not firmware
evidence. **[D]**

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

The generated register-effect audit now accepts the exact terminal
`return without followed call` only for a trace started at a recovered function
entry, as that terminal has reached the entry's top-level architectural return.
With all retained paths supported and uncertainty-free, the audit reports
`R6` preserved for direct entries `0x1c3464`, `0x1c3474`, `0x1c347e`, and
`0x1c349b`, and for table-entered wrappers `0x1c352f`, `0x1c353e`, and
`0x1c354d`. `0x1c3504` remains unknown, but the immediate `source: prm`
stop is now traversed as an explicit calibration rather than silently
qualified. Its manifest query names `14d`; the typed ISA seam resolves that
name to `isa.form.14d.encoding`, status `unconfirmed`, source `prm`, and the
index includes that evidence in both the cache request and report. The trace
executes the decoded short-word load from `DM(0x256a48)` and finds no `R6`
writer. The next frontier is now traversed too: the exact adjacent
`LSHIFT R4 BY -2` at `0x1c350f` and non-delayed `NOT SZ` branch at `0x1c3512`
derive the complete fallthrough domain `R4=0..3`. The index reruns those four
cases with the separately evidenced global-constant seeds (including `M13=0`);
all retained cases reach a top-level return and the former `I12/M13` stop is
gone. The generated result still stays `unknown`, now solely because
`calibration form used: 14d`: a calibration path cannot establish even an
existential writer result. These are bounded static trace results, not a
qualification of Type14d, a natural selector observation, or a calling-
convention claim. **[D][O]**

The writer-trace cache now applies the same narrow, versioned Type14d
continuation policy as the register-effect query. It records
`type14d-continuation/v1`, the admitted form, and every store whose path has
crossed that provisional decode. Such a store remains `UNRESOLVED`; it cannot
become a hit or exclusion merely because the tracer continued. On the full
DT2 census, the immediate `source: prm` stop count falls from 627 to 189
stores. Of the continued results, 171 store sites are explicitly retained as
Type14d-dependent and therefore unresolved; other paths proceed to later
limits or unsupported forms. The census totals do not change: each of the two
current target classifications still has 7,456 unresolved stores. This moves
the analysis frontier without qualifying Type14d or strengthening writer
evidence. Jobs=1 and jobs=8 reports are byte-identical, and a warm read leaves
the index mtime unchanged. **[D][O]**

The loaded tail is no longer an unknown *loader initialization* boundary, but
its runtime target set remains unproven. No natural selector value or runtime
occurrence is observed. A minimal future capture would record `DM(0x2560c4)`,
`DM(0x256c98..0x256ca7)`, `R4`/`R6` at `0x1c351a`, and the selected tail
target. **[O]**

## Type9b stop attribution and a shared indirect helper **[C][D][V][O]**

The post-Type7a DT2 1.16 v5 index
`out/sharc-index/type7a-after-local-06adc1b.sqlite` has 1,059 function
facts. Exactly **65 distinct functions** have at least one `unknown 9b_abs`
indirect-target stop: 37 with `I12/M13`, 31 with `I13/M13`, and 1 with
`I12/M14`, with overlap between groups. These are stopped *function facts*,
not 65 instruction sites. The handover's earlier 63 was from a different
pre-Type7a cache; the continuation exposed additional stops rather than
changing the meaning of Type9b. A substring query for `I12/M13` also matches
one `9a_abs` reason, so it must not be counted as a Type9b stop. **[C][D]**

Hash-checked loader-final bounded traces (`max_steps=400`, `max_states=32`,
global constant seeds, concrete memory, NW32 assumption, followed calls,
external-call continuation, and provisional Type14d) attribute three
distinct Type9b sites in six sampled entries:

| SW PC | decoded operands | source of unknown target | context |
| --- | --- | --- | --- |
| `0x1c351a` | `JUMP(I12+M13)`, non-delayed | I12 load at `0x1c3518` from a symbolic table address | natural-selector tail |
| `0x1c6579` | `JUMP(I12+M13)`, non-delayed | I12 load at `0x1c656c` from the documented M4-selected table | per-frame orchestrator |
| `0xb891b4` | `JUMP(I13+M13)`, delayed | preceding `I13=R1` at `0xb891b2` | followed callee in several samples |

This reproduces the known `0x1c6579` frontier but does not show that all 65
function facts stop at these three PCs. Type9b already models the documented
indirect target when both operands are known; these stops are missing *value*
provenance, not an unimplemented target arithmetic rule. The Type9b `b=0`
field selects JUMP rather than CALL; `j=1` selects the delayed transfer at
`0xb891b4` (see the Type9b return-idiom field correction in finding 06).
**[D][O]**

An independent second reader rehashed the same section-7 blob (SHA-256
`0f514a12a2255f5c081e292c47f1f29462003177658da4bbae0a22fd737fffa2`)
and confirmed loader-final bytes `3e70bf8e` at `0xb891b2` (Type5b,
unconditional UREG copy from code 1 = R1 to code 29 = I13) followed by
`3f083f6c` at `0xb891b4` (Type9b, `pmi=5`, `pmm=5`, `j=1`, `b=0`, `cond=31`).
The public SHARC+ Core Programming Reference Rev. 1.5 describes Type5b's
UREG copy on printed pp. 13-38--13-39 and Type9b's pre-modified I+M
transfer on pp. 14-8--14-9. This verifies the *words and decoded local
transfer*, not their runtime occurrence. **[V][O]**

Four sampled entry traces reach the shared helper through a represented call
at `0xb88da2` and stop at `0xb891b4` with both R1 and I13 unknown as
`R2 + R1`; no sampled state there supplied a concrete target. Missing a
represented R1 writer in those paths is **not** proof that none exists.
Neither the natural R6/M4 value for `0x1c6579` nor the runtime R1 argument
for this helper is established, and the helper's role as machine, voice, or
audio code is unknown. Do not promote either to a safe hook on this evidence.
The next discriminating observation would capture R6/M4/table word/target in
a natural frame at `0x1c6579`, or the call-site R1 and selected target at
`0xb891b4`, with machine/track context. These are **1.16 image addresses**:
the physical OS is currently contradictory in the repo (`CLAUDE.md` says
1.15C remains installed; `README.md` says Em upgraded to 1.16 on 2026-09-20).
Before any hardware capture, confirm the installed version with Em and
byte-check either its identity with the analyzed 1.16 image or a mapping to
the 1.15C equivalent. Do not install firmware to obtain this capture.
**[C][D][O]**
