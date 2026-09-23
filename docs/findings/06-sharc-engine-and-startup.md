# SHARC engine and startup

The bounded SHARC startup probe, the frontier sprint, the machine-type consumer on the SHARC side, and the audio engine's located ingredients.

## The apparent stream-activation lead is USB, not the SSI/DSP peer **[C][V]**

The earlier interpretation of `FUN_40005ff6`, vector 134 and logical channels
3/7 as a possible missing DSP-peer stream was wrong. `FUN_40005ff6` is a USB
Chapter 9 setup-request dispatcher: `0x80060000` is setup bytes `80 06`
(`GET_DESCRIPTOR`), while `0x010b0000` is `01 0b` (`SET_INTERFACE`). The code
returns device/string descriptors and configures endpoint queues under the
MCF5441x USBOTG register block at `0xfc0b0000`; SSI0 is separately based at
`0xfc0bc000`. Vector 134 consumes the USB setup packet and dispatches endpoint
completion work. **[V]**

Consequently `_DAT_40965a60 == 6`, the `SET_INTERFACE` alternate-setting
branch, and the USB endpoint queues do not explain the observed A2 refresh.
The trigger trace instead reaches `FUN_4002d438` through vector 191 after
local record construction in `FUN_40139878`; no evidence here connects the
USB control dispatcher or its endpoint configuration to that refresh.
`FUN_40003376` remains unclassified by this correction. USB endpoint transfer
type also remains unlabelled until its endpoint descriptor attributes are
decoded. **[C]**

## Bounded static marker and cadence gates remain blocked **[V][O]**

Two bounded, independently reviewed gates closed their specified A2 slices
without relaxing the evidence rules. Neither passed, so a natural SSI peer
model and final qualification must not be implemented from the current
evidence. This result did not by itself prove that every possible static or
emulator experiment had been exhausted. **[V]**

The DMA10 marker gate found only immutable SPORT record `0x0a` at DM
`0x26968c` (loader alias `0x2826968c`), containing SPORT4A `0x31002400` and
DMA10 `0x31023000`. The supported calibrated consumer slice loads these words
at `0x1ca6c1` and `0x1ca6c3` and copies them into an object whose address and
ownership remain unknown. It then stops at aligned provisional words
`0x1ca6e9: 3e02` (`Type23p_undoc16`) and `0x1ca6ea: 3100`
(`Type21p_undoc16`). No supported path establishes DMA direction, descriptor
counts, application buffer, a writer of `0x007fffff`, or placement at stream
word `0 mod 512`. The loader contains no other direct DMA10-window word, the
SHARC main contains none, and normalized reference/immediate queries found no
producer edge. The nine `0x7fffffff` immediates remain unrelated. **[V][O]**

The PCG-C cadence gate confirmed table `0x2d7158` contains only the four
handler pointers. Handler `0xb8b706` reaches a CTLC0 load at `0xb8b70e`, then
immediately stops at aligned `0xb8b711: 3e02`. The known stores to SYNC2,
CTLC1, CTLC0 and PW2 prove reachability only: no supported dataflow supplies
executed values, source selection, or source frequency. The only justified
relation remains symbolic:

```
SSI request rate = bit clock / (24 * 8) = bit clock / 192
64 requests * 32 bytes = 0x800 bytes per major loop
```

It does not justify a numeric rate. Consequently 1,000 Hz, 48,000 Hz, or any
other convenient value remains disqualifying for final A2. **[V][O]**

The gate artifacts are under workflow
`296e0a13-5d01-4406-b5a6-451d30905086`. Final A2 is now blocked on a newly
approved supported observation source: for example, runtime PCG-C register and
edge/request timing plus DMA10 descriptor/application-buffer observation, or
public documentation that supplies the currently unsupported semantics. Broad
ISA expansion, guessed marker conversion, and guessed cadence are rejected
pivots. **[O]**

### A target-backed descriptor slice reaches the marker candidate **[C][V][O]**

The earlier statement that all nine `0x7fffffff` immediates were unrelated is
superseded. Address-aware decoding and a bounded replay now establish an exact
store from one of them into a buffer named by a concrete descriptor-list
template. This
does not yet establish that the list belongs to DMA10, nor that the serial
boundary converts the value to the ColdFire's `0x007fffff`. **[C][V][O]**

At the aligned, even-register Type-14a forced-long-word DM sites used here, the
documented form writes the explicit UREG and its next neighbor at `address` and
`address+4`. (The tracer conservatively stops on odd explicit registers; these
sites do not use them.) With that form modeled, four bounded setup slices
reconstruct cyclic two-descriptor templates and pass their heads in `R8` to
`0x1ca7e4`: **[V]**

| list head | descriptors | start buffers | `CFG, XCNT, XMOD, YCNT, YMOD` | bytes/buffer |
| ---: | --- | --- | --- | ---: |
| `0x2620c8` | `0x2620c8` ↔ `0x2620e4` | `0x261cc8`, `0x261dc8` | `0x100000, 64, 4, 0, 0` | `0x100` |
| `0x262100` | `0x262100` ↔ `0x26211c` | `0x261ec8`, `0x261fc8` | `0x100000, 64, 4, 0, 0` | `0x100` |
| `0x264138` | `0x264138` ↔ `0x264154` | `0x262138`, `0x262938` | `0x100000, 512, 4, 0, 0` | `0x800` |
| `0x264170` | `0x264170` ↔ `0x26418c` | `0x263138`, `0x263938` | `0x100000, 512, 4, 0, 0` | `0x800` |

All four slices calibrate `R11=0` across an unresolved caller path; lists 2 and
4 additionally seed register values written before unresolved call boundaries.
Their rows are verified calibrated reconstructions, not proof that intervening
calls preserve the registers or that natural execution constructs the same
result. All four probe records disclose common and slice-specific seeds and
remain `qualifying: false`. **[V][O]**

The field names above follow the public descriptor-list register order:
`DSCPTR_NXT`, `ADDRSTART`, `CFG`, `XCNT`, `XMOD`, `YCNT`, `YMOD`. The two
large list geometries exactly match one ColdFire eDMA48/50 `0x800`-byte major
loop, but equal byte counts are not physical ownership or wiring evidence.
The same setup family has four candidate software objects and global slots;
the runtime join from any one object through the three-level SPORT/DMA chain
to record `0x0a` and DMA10 remains unproved. `0x1ca7e4` may also transform the
templates before any hardware fetch, so these are not live DMA descriptors.
**[V][O]**

The first large list does have a byte-backed producer for its first word.
At `0x1c7578` the code loads selector word `DM(0x25f780)` into `R1`;
`0x1c757b` computes `R2 = LSHIFT R1 by 11`; `0x1c757e` copies that byte
offset to `I4`; `0x1c7580` loads exact constant `0x7fffffff` into `I12`;
`0x1c7583` adds base `0x262138`; and `0x1c7586` stores `I12` through
`DM(I4,M5)`. Global setup writes `M5=0` at `0x1c0f44`. With the loader's
zero selector, the calibrated replay therefore writes `0x7fffffff` at
`0x262138`; selector value one would address peer buffer `0x262938`. Those
are exactly the two `ADDRSTART` words in descriptor list `0x264138`.
This is an application-buffer/descriptor-template join, not yet a DMA10/object
join. The instruction path and conditional address expression were checked
independently against the image bytes; the replay is direct-entry and therefore
non-qualifying. **[V][O]**

No exact supported path currently changes `0x7fffffff` into
`0x007fffff`. ColdFire SSI0 is configured for 24-bit words, but the executed
SPORT4A control value, transmitted word length, bit selection, and ownership
of list `0x264138` are still missing. Inferring truncation from the matching
low 24 bits would violate the evidence gate. The smallest useful natural
breakpoint set is now: `0x1c7588` after the marker store, watching
`0x25f780`, `0x262138`, and `0x262938`; `0x1ca7e4` on descriptor submission,
recording `R4/R8`; and `0x1ca6d6` on DMA-base installation, recording the
full `P/L1/L2/L3` chain. A qualifying join needs one natural run to correlate
those observations and the eventual DMA10 register writer. **[O]**

The deterministic report is
`out/experiments/sharc-interface-reader/dma-descriptors-001.json`. Each
calibrated slice is marked `qualifying: false`; its exact bytes, descriptor
words, complete seed disclosure and classification, and residual ownership
gaps are included.
**[V]**

The two concrete static hypotheses left after reopening that conclusion were
then tested. First, a documented 32- or 48-bit predecessor does **not** consume
the aligned `0x023e` parcel. At all four PCG-C sites in each of DT2 1.15C, DT2
1.16 and DN2 1.11, documented 48-bit `Type14a` bytes
`02100c3100a3` end immediately before `3e02`; the parcel then falls through to
documented 48-bit `Type1a` bytes `103822800211`. At the SPORT homolog in each
image, documented 32-bit `Type15b` bytes `0896060a` end immediately before the
same parcel. Identical direct branches target those `Type15b` starts, fixing
their alignment independently. Every apparent documented covering decode
starts inside one of those established predecessors. Thus `0x023e` is a
standalone unsupported parcel at these sites, not a width error; its execution
semantics remain unknown. **[V][O]**

Second, fresh loaded-L2 measurements were made in separate JVM runs under
`out/sharcpcode/l2-cross-002/`. The independently located four-pointer tables
are:

```
DT2 1.15C  DM 0x2d7148: b8b12a b8b3d7 b8b698 b8b945
DT2 1.16   DM 0x2d7158: b8b198 b8b445 b8b706 b8b9b3
DN2 1.11   DM 0x2dd3d0: b8d01a b8d2c7 b8d588 b8d835
```

After relocation, ordered decoded bytes for each table target are identical in
all three images. Their byte lengths and SHA-256 hashes are respectively:

```
148  2e88738b0453ce1f840e267b1b62a3b2b16ed16440ecb1a6ce23e9ac15889dca
  2  15c69a0025b994074ea78f06a5e2f3101b78bc193aaa6f6d044e4e5f679727fc
150  2ba282e3e830b6e631df4a3e832065b2b69680379dcbc5267897206139d5ca0a
158  99752fd431c399854488bb1946158048ce9172c33c89a016cf28d649dacb01db
```

The 64 bytes before each table and 136 bytes after it are also byte-identical;
the latter contain a diagnostic PCG memory-size assertion, not clock
configuration. No alternate image substitutes a documented operation or an
immutable default that yields CTLC0/1, PW2, SYNC2, source selection or source
frequency. Exact identity does not exclude configuration elsewhere or runtime
arguments, but it closes this comparative-substitution hypothesis. **[V][O]**

Names such as `rpc_dispatch_table` and `rpc_handler_00` through
`rpc_handler_03` are mechanically generated by `tools/sharc_import.py` from a
user-supplied label-table address. They are not firmware symbols and do not
establish an RPC role or a ColdFire-facing edge. The tables above must be
described by their byte-backed locations and targets unless an independent
consumer edge is recovered. **[V]**

Independent byte review reproduced the table locations, target hashes and
instruction boundaries. Both reopened hypotheses therefore close negatively:
neither natural DMA10 marker production nor numeric PCG-C/SSI cadence is
recovered, and final A2 qualification remains blocked. This is a bounded
negative result, not a claim that future public evidence or a genuinely new
runtime observation source is impossible. **[V][O]**

## The bounded SHARC startup probe now reaches core initialization **[V][O]**

`tools/sharc_trace.py` can now replay the real section-7 loader stream from a
short-word PC, seed a bounded set of public-manual reset values, follow loaded
calls, and report concrete core/peripheral MMR accesses. The new instruction
slice implements documented Type18a system-register bit operations, including
`TST`/`XOR` updates of ASTAT BTF and the `TF`/`NOT TF` predicates, counted
Type12a loops, and the Type20a status-stack operations and stack-empty polling
used by startup. Unsupported instruction forms still stop explicitly. **[V]**

The strict reset-state run starts at `0x1c1338`. Its first documented Type14a
instruction reads the public reset value zero from `SHBTB_CFG` at `0x31400`.
Execution then stops after one instruction at the standalone `0x023e` parcel,
decoded provisionally as `Type23p_undoc16`, at `0x1c133b`. The zero is supplied
by the public reset table through `--core-reset-state`, not by a section-7
loader record. Artifact:
`out/experiments/sharc-runtime-probe/strict-entry-009-summary.json`. **[V][O]**

An explicitly exploratory restart at `0x1c1365`, after the first unsupported
island, branches through the loaded helper at `0x1c0000`, writes zero to
`SHL1C_CFG`, and executes six concrete zero-fill loops with counts
`1024, 1024, 4096, 256, 256, 128`. It then reads concrete reset value zero from
`SHBTB_CFG` and stops at the next standalone `0x023e` parcel at `0x1c13b7`.
Artifact: `out/experiments/sharc-runtime-probe/post-shbtb-009-summary.json`.
The restart seam makes this discovery evidence, not a qualifying continuous
boot trace. **[V][O]**

A second exploratory restart at `0x1c13c3`, with the already observed MODE1
state, now executes the firmware's three stack-empty polling ladders. Type18a
tests STKYX LSEM, SSEM and PCEM; Type20a pops the loop, status and PC stacks
until those read-only indicators become set. The path writes the real main
entry `0x1c1338` to `RCU0+0x2c` at instruction `0x1c1414`, enters loaded call
`0x1c0f26`, initializes DAG and mode state, loads public reset value zero from
`CMMR_SYSCTL` at `0x30024`, and stops at another standalone unsupported
`0x023e` parcel at `0x1c0f8b`. Artifact:
`out/experiments/sharc-runtime-probe/post-shbtb2-005-summary.json`. Independent
review reproduced the instruction bytes, decodes, loader-backed control flow,
MMR addresses and trace events. **[V][O]**

This is the first firmware-backed dynamic reach into SHARC core initialization,
but it has not yet reached a concrete PCG-C, SPORT4A or DMA10 configuration.
The strict path remains blocked at `0x1c133b`, while every later observation
above crosses an explicit unsupported-instruction restart seam. Therefore it
does not establish marker production, DMA10 descriptors, SSI cadence or final
A2 qualification. **[O]**

## The capped SHARC frontier sprint found no supported route to the SSI peer **[V][O]**

A final bounded public-source search found no semantics for aligned parcel
`0x023e`. The current SHARC+ programming reference and the earlier public VISA
reference both mark Types 23–24 reserved. Historical classic-core `IDLE16` is
device-specific, has a different documented opcode prefix, and cannot name or
define the SHARC+ parcel. Public emulator, assembler, LLVM, patent and forum
searches supplied no exact encoding or architectural effects. “Reserved” is
negative documentation evidence, not proof that the target silicon treats the
parcel as illegal or inert. **[V][O]**

The existing loaded-image graph also supplies no alternate supported route.
The strongest PCG-C candidate loads CTLC0 at `0xb8b70e` and immediately reaches
`0xb8b711: 3e02`; corresponding paths encounter the same provisional family.
The SPORT4A/DMA10 consumer at `0x1ca58a` still requires the calibration-only
`I4=-0x13c` selection and stops at `0x1ca6e9: 3e02`. Every ranked path therefore
requires an unsupported parcel, arbitrary direct-entry state, or unknown
runtime arguments before it can establish executed PCG, SPORT or DMA state.
**[V][O]**

`tools/sharc_frontier.py` now automates exploratory continuation without
changing strict tracer semantics. A versioned manifest names each predecessor
stop PC, successor PC, exact skipped byte range, SHA-256 and assumptions. The
driver hard-limits a run to three islands, 4,096 bytes per island and 50,000
instructions; reports and restart events are always `qualifying: false`.
`optimistic-preserve` carries state across an island, while
`conservative-clobber` invalidates registers, stacks and pre-island data/MMR
state but retains the immutable loader code map. Compact reports cap peripheral
samples while full traces remain optional artifacts. **[V]**

The startup manifest under
`out/experiments/sharc-runtime-probe/frontier-startup-001-manifest.json`
declares three independently byte-verified ranges:

```
0x1c133b..0x1c1365   84 bytes  b678736eb48034a335d831eef9c5f9f6f8fc3384bf343dedd3fdcaf35cd2a68d
0x1c13b7..0x1c13c3   24 bytes  3a239ba23af24b22909a98aebeb110fbdbb71794f17faf8a345cc9f4f48f9f19
0x1c0f8b..0x1c0f8e    6 bytes  1260410df141425e99ed75249a74463e83e70caae55a6ed30f37fd21999b6bf0
```

The optimistic run crossed all three, executed 6,932 instructions, reached no
PCG-C, SPORT4A or DMA10 target, and stopped at unsupported documented form
Type11c at `0x1c0fbc`. The conservative run crossed two islands, made the
post-island status predicates unknown, and reached its state bound before a
target. Artifacts are
`frontier-startup-001-{optimistic,conservative}-{summary,trace}.json` in the
same directory. These results satisfy the sprint's stop criterion: increasing
the island or instruction budget would produce more discovery-only execution,
not qualifying evidence. **[V][O]**

Static 1.15C/1.16 comparison is insufficient to substitute runtime values.
DAI routing and immutable SPORT association are compatible, but PCG source and
divisors, DMA descriptors/buffers, marker words and external SSI cadence remain
unknown runtime inputs. No 1.15C cadence or payload may therefore be promoted
to the 1.16 target by relocation or handler identity alone. **[V][O]**

## `0x023e` is the first parcel of a 48-bit immediate shift **[C][V][O]**

The earlier standalone-`Type23p_undoc16` interpretation is wrong. A public
SHARC+ VISA decoder (`js216/selache` revision
`2b26d3b75c53063575bc5c820fa0d38879335187`) selects a 48-bit instruction for
first byte `0x02`. For the observed words, the stricter fixed bits and fields
match the public programming reference's Type6a no-memory ShiftImm layout after
substituting that VISA prefix. The low 23-bit field then decodes entirely
through the public ShiftImm opcode table. The apparent following
Type1a/Type15b instructions began inside
the same 48-bit instruction; predecessor alignment alone did not fix the
successor width. `Type23p_undoc16` has therefore been removed. This correction
supersedes every earlier paragraph that calls `0x023e` standalone, reserved,
or a strict/frontier restart boundary, including the bounded-gate, startup and
frontier conclusions above. **[C][V]**

Reversing each little-endian loader parcel into architectural bit order gives
these exact selected-path instructions:

```
PC          architectural word  documented operation
0x1c133b    023e00300000        R0 = BSET R0 BY 0
0x1c13b7    023e00310000        R0 = BCLR R0 BY 0
0x1c0f8b    023e00300200        R0 = BSET R0 BY 2
0xb8b711    023e38108022        R2 = FEXT R2 BY 0:30
0xb8b723    023e3810c022        R2 = FEXT R2 BY 0:31
0xb8b7c5    023e7800f611        R1 = LSHIFT R1 BY -10
0x1ca57f    023e00300022        R2 = BSET R2 BY 0
0x1ca692    023e20100022        R2 = FEXT R2 BY 0:16
0x1ca6e9    023e00311922        R2 = BCLR R2 BY 25
```

An independent reviewer reproduced the loader bytes, per-parcel byte order,
all nine architectural words, the public decoder results and the corresponding
public ShiftImm operation descriptions. These exact unconditional operations
are verified. The generic form remains documented rather than fully verified:
the emulator does not implement its other predicates/opcodes, SIMD companion
effects, or documented ASTAT SS/SZ/SV updates, and the secondary decoder allows
one high field bit which the stricter PRM-derived mask fixes to zero. None of
those differences affects the nine words above. **[V][D][O]**

The tracer implements only the documented immediate operations encountered on
these selected paths. A strict reset-state run now crosses all three former
startup restart islands without a skip, performs the previously observed
BTB/cache/RCU/core-control accesses, and passes documented Type11c/Type12a
control flow. Its eight conservative predicate branches execute at least 6,952
instructions before stopping on a non-concrete register-counted loop or an
external-call boundary. Artifact:
`out/experiments/sharc-runtime-probe/strict-entry-type6b-004-summary.json`.
The old frontier restart hashes remain reproducible byte facts, but the
frontier's unsupported-island rationale and its resulting qualification limit
are superseded. **[C][V][O]**

Correct parcel sizing also changes the PCG-C slice. The sequence now reads
CTLC0 at `0xb8b70e`, extracts its low 30 bits at `0xb8b711`, and stores the
result back at `0xb8b714`; it reads CTLC1 at `0xb8b720`, extracts its low 31
bits at `0xb8b723`, and stores it at `0xb8b726`. The four previously selected
stores are ordinary documented read/mask/write operations and no longer cross
an unknown instruction effect:

```
0xb8b71d  SYNC2 = old SYNC2 & 0xfffffff9
0xb8b75a  CTLC1 = old CTLC1 & 0xfff00000
0xb8b7bc  CTLC0 = old CTLC0 & 0xc0000000
0xb8b801  PW2   = old PW2   & 0xffff0000
```

These formulas still do not provide the old values, subsequent set fields,
runtime handler selection, source clock or numeric cadence. A direct symbolic
entry at `0xb8b706` now crosses both former `0x023e` stops and records the
resulting PCG read/mask/write events, but remains non-qualifying because its
entry and branch state are not established from reset. Artifact:
`out/experiments/sharc-runtime-probe/pcg-type6b-001-summary.json`. **[C][V][O]**

Likewise, calibrated SPORT4A record selection now crosses the complete
`0x1ca698..0x1ca71b` suffix and reaches its return using documented semantics.
In addition to `0x1ca6e9: R2 = BCLR R2 BY 25`, the supported slice now covers
fixed-point AND/OR/XOR, variable and immediate BSET/BCLR/BTGL, Type6a's
parallel ShiftImm plus post-modify memory access. Selected MR/MRF data-move
and multiply-accumulate forms were also added for the earlier `0x1ca58a`
entry path, but do not establish the runtime inputs to this suffix. With
calibration-only `I4=-0x13c`, `0x1ca6c1` reads SPORT4A control base
`0x31002400` from `0x26969c`, and `0x1ca6c3` reads DMA10 base `0x31023000`
from `0x2696a0`.
No path state performs a concrete peripheral access: the store at `0x1ca6f9`
is `DM(I5,M5)=R9` in parallel with `R2=BCLR R2 BY 11`. It is not the DMA10
base store: `0x1ca5c9` loads its `I5` destination base from frame slot
`DM(I6+0x10)`, while `0x1ca5d1` copies incoming `I3` to its `R9` value.
The DMA10 base instead flows through `R0`: `0x1ca6c9` and `0x1ca6d1` load a
two-stage linked destination through `I4`, and `0x1ca6d6` stores `R0` through
that `I4/M5` address.

The compiler frame removes the earlier uncertainty around `0x1ca6f9`.
Type25a `CJUMP` executes `R2=I6, I6=I7`; its two delay slots save the prior
frame and return address. Replaying the four exact caller push sequences with
the documented 32-bit normal-word scaling makes the callee loads at
`0x1ca5c7` (`I3=DM(I6+0x8)`) and `0x1ca5c9`
(`I5=DM(I6+0x10)`) concrete:

| call | `I3`, copied to `R9` | `I5` | `0x1ca6f9` effect when reached |
| ---: | ---: | ---: | ---: |
| `0x1c78f6` | `0x2618d0` | `0x261960` | `DM(0x261960)=0x2618d0` |
| `0x1c798b` | `0x261918` | `0x261964` | `DM(0x261964)=0x261918` |
| `0x1c7a5f` | `0x261970` | `0x261a00` | `DM(0x261a00)=0x261970` |
| `0x1c7b12` | `0x2619b8` | `0x261a04` | `DM(0x261a04)=0x2619b8` |

`M5` is initialized to zero at `0x1c0f44` and is not reassigned on these
caller/callee paths, so `0x1ca6f9` installs four software-object pointers in
four global slots; it is not a DMA10 register or descriptor store. The DMA10
destination remains a different chain. If entry `I2` is `P`, then
`0x1ca6a6` gives `L1=DM(P+0x14)` and `0x1ca6c9` gives
`L2=DM(L1+0x14)`. With `M5=0`, `0x1ca6ce` installs SPORT4A base
`0x31002400` at `DM(L2)`. Then `0x1ca6d1` gives `L3=DM(L2+0x14)` and
`0x1ca6d6` installs DMA10 base `0x31023000` at `DM(L3)`. `P`, `L1`, `L2`,
and `L3` remain runtime inputs. Seeding `I5` with the DMA10 base would
therefore fabricate the wrong ownership join rather than discover it.
Artifacts:
`out/experiments/sharc-runtime-probe/sport4a-consumer-supported-002-summary.json`
and `sport4a-consumer-supported-002-trace.json`; the deterministic compiler
frame replay and exact-byte assertions are in `tools/sharc_interface_probe.py`.
This removes the remaining
decoder stop in the calibrated suffix but does not establish natural
`I4=-0x13c`, DMA10 ownership/descriptors, an application buffer, a first-word
writer, or production of `0x007fffff`. The natural marker chain and numeric
SSI cadence therefore remain open, and final A2 is still unqualified.
**[C][V][O]**

## The first strict-startup PM/PX loss is concrete; the next PM source is absent **[C][V][O]**

The earlier strict trace made every PM data load unknown, including the first
load in the helper at `0x1c0fb4`. Public SHARC documentation distinguishes the
combined 64-bit `PX` register from its 32-bit `PX1` and `PX2` halves. A
non-`LW` memory transfer through combined `PX` moves 48 bits into PX bits
63--16, so the upper two 16-bit memory parcels form `PX2` and the final parcel
occupies the upper half of `PX1`. The L1 block-3 normal-word and short-word
aliases begin at `0xe0000` and `0x1c0000`; 48-bit normal words occupy three
16-bit columns. `tools/sharc_trace.py` now models only this bounded,
loader-backed combined-PX case rather than treating arbitrary PM reads as
concrete. Independent review checked the register split and both DM- and
PM-bus handling against the public reference. **[V]**

In the 1.16 loader image, the direct Type14a PM read at `0x1c0fb4` addresses
normal word `0xe00e0`. It maps to loader/system byte address `0x28380540`,
covered by loader block 87; its exact six bytes are `00 00 00 00 00 00`, so
both PX halves are zero. The following documented moves therefore copy
`PX2=0` through `R0` to `I8`. The next Type3b PM read at `0x1c0fbd`
consequently addresses absolute normal word zero. That address is outside the
bounded block-3 mapping; direct loader checks find no record at byte addresses
`0` or `0x28000000`. (The record at `0x28380000` is block-3 normal word
`0xe0000`, not normal word zero.) The tracer therefore invalidates `PX`,
`PX1`, and `PX2`; it does not preserve the earlier zero or invent contents.
The six downstream Type12a paths still stop on non-concrete register loop
counts, while two sibling paths stop at the same external-call boundary as
before. Artifact:
`out/experiments/sharc-runtime-probe/strict-entry-pmpx-005-summary.json`.
Independent review reproduced the raw loader block and instruction bytes, and
an independent execution of the same bounded command reproduced all eight
terminal states and the absence of target-peripheral accesses. **[C][V][O]**

This narrows the initialization gate: Type12a loop semantics are not the first
loss, and the loader-backed `0xe00e0` read is no longer unknown. The next
missing input is the PM source at normal-word address zero, plausibly outside
the section-7 image but not yet identified as a specific ROM or other memory
provider. The run still reaches no PCG-C, SPORT4A, or DMA10 initialization and
does not derive `I4=-0x13c`. **[V][O]**

ASTAT work is also deferred: the public tables document PASS/COMPARE arithmetic
flags, but the currently implemented strict
predicates consume `TF`, so adding inferred shift flags would not move this
frontier. **[D][O]**

The separate Type25 PC-relative audit found no arithmetic defect: the 24-bit
displacement is sign-extended, added to the instruction's own short-word PC,
and remains in short-word units. A Type25-specific negative synthetic
regression now checks this convention. No loaded firmware Type25 negative
target was found, so this closes a test gap rather than authorizing a new
loaded-call edge. **[D][O]**

## Type12a concrete-count tracer boundary **[D][O]**

The tracer admits Type12a counted loops only when the instruction supplies a
16-bit immediate or its UREG count is a concrete value. It records the
23-bit signed PC-relative end, loop count, and mode, then follows the loop
until the concrete counter expires. A zero count and `Unknown`, `Affine`, or
partially known UREG counts remain conservative stops; they are not converted
into bounded symbolic iterations. The public SHARC+ Core Programming Reference
pp. 14-23--14-25 documents the immediate/UREG forms, counter-stack setup, and
E2-active (mode 0)/F1-active (mode 1) distinction. This trace abstraction
preserves the mode but does not claim pipeline-precise E2/F1 timing or
zero-count behavior.

## PASS/EQ removes the low-PM false path, and Type25 wraps at 24 bits **[C][V][O]**

The preceding PM-address-zero blocker and ASTAT deferral are retracted. At
`0x1c0fb4` the loader-backed combined-PX read still produces concrete zero;
`0x1c0fb7` copies PX2 to R0, and `0x1c0fb9` performs documented fixed-point
`PASS R0` while also copying R0 to I8. PASS clears AC/AI/AS/AV and sets AN/AZ
from the 32-bit result, so zero sets AZ. The `IF EQ RTS` at `0x1c0fbc` is
therefore taken in concrete SISD execution. The feasible strict path returns
before `0x1c0fbd`: it never reads PM normal-word address zero, and the six
downstream Type12a failures disappear. Unknown PASS inputs invalidate ASTATX
rather than preserving stale flags. **[C][V]**

The preceding Type25 arithmetic conclusion is also retracted. Sign extension
and addition to the instruction's own PC were right, but the sequencer target
must then be reduced to the architectural 24-bit short-word PC. At
`0x1c0fa5`, storage bytes `44 18 9c 00 3d 84` normalize to
`0x1844009c843d`; its signed displacement is `-6519747`, and the wrapped
target is `0xb893e2`, not a negative external address. That target maps through
the loader's L2 fallback to byte address `0x200127c4` in block 69, interval
`[0x20000000,0x2001ab7c)`. The strict trace now follows this as its third
loaded call. **[C][V]**

## Enhanced Type19a `(NW)` advances strict startup to `0xb893fb` **[C][V][O]**

The earlier `Type19p_undoc48` classification and every `-257` / `I7 -= 0x404`
interpretation are retracted. At loaded SW `0xb893e2`, exact bytes
`87 15 ff ff fe ff` are three little-endian 16-bit parcels in
most-significant-parcel order, producing logical word `0x1587fffffffe`.
The SHARC+ PRM documents `sc=01` as enhanced Type19a address scaling, and the
public Selache encoder/decoder independently confirms fields `w=1`, `g=0`,
`idis=0`, `is=7`, signed immediate `-2`. The instruction is therefore
`I7 = MODIFY(I7,-2)(NW)`. **[C][V]**

`tools/sharcspec/build_table.py` now emits this prefix as confident
`Type19a_scaled`, while preserving the ordinary classic `0x16` Type19a form.
The strict tracer scales `(NW)` by four only in its explicit byte-address
model, applies the same scaling to the active circular length, and leaves B/L
unchanged. The observed entry state is `I7=0x26f7ee`, `B7=0x26f000`,
`L7=0x1fd`; the documented operation produces `I7=0x26f7e6` without wrapping.
In normal-word address space the architectural update remains `I7 -= 2`.
Synthetic tests cover both interpretations and a negative circular wrap.
An independent loader-aware review reproduced the image hash, L2 fallback
mapping `0x200127c4`, source block 69, exact six bytes, parcel normalization,
fields, and both trace endpoints. **[V]**

A new strict run begins at real entry `0x1c1338` against exactly
`out/sections/dt2-1.16/section_7_BLOB.bin` (SHA-256
`0f514a12a2255f5c081e292c47f1f29462003177658da4bbae0a22fd737fffa2`),
with loader-backed concrete memory, public core reset state, 32-bit normal
words and loaded-call following. It naturally executes the enhanced MODIFY,
then ends in two states after 6,987 / 6,989 instructions at the same next
boundary: Type2a at `0xb893fb`, whose full-compute field has `cu=0`, opcode
`0x00`. The public PRM's ALUOP table has no `0x00` operation, and the independent
Selache decoder also leaves it as an unknown ALU opcode, so no semantics are
invented for it. Both states record three loaded calls and no PCG-C, SPORT4A or
DMA10 access. Artifacts and SHA-256:

```
out/experiments/sharc-runtime-probe/strict-entry-type19nw-008-summary.json
7d69408b76fb262e9d03295a9632c7fb2cac90e6ed95474b90ba60489bbd9094
out/experiments/sharc-runtime-probe/strict-entry-type19nw-008-trace.json
3195e941880eaa32005db6a1c2186060e3a943a7be34afcdb7f4334522591cd6
```

This advances the qualifying continuous path but still does not reach PCG-C,
SPORT4A or DMA10 initialization, derive `I4=-0x13c`, identify the natural
`0x007fffff` producer, or establish numeric SSI cadence. A2 remains open and
A3 remains parked. **[V][O]**

**[C]** `0xb893fb` is not an undefined ALU opcode. It is a 32-bit compute,
`R2 = LEFTZ R8`, read with the wrong width. See the next section.

## Tracer decode and call-model corrections move strict startup to `0x1c1460` **[C][V][O]**

Six defects in the decoder and in `tools/sharc_trace.py` caused the startup,
callback and `0x1ca7e4` stops recorded above. With them fixed, a strict run
from `0x1c1338` runs to step 7,545 and stops at a different, earlier-unseen
boundary. The run uses the same image
(`out/sections/dt2-1.16/section_7_BLOB.bin`, SHA-256
`0f514a12a2255f5c081e292c47f1f29462003177658da4bbae0a22fd737fffa2`) and
flags as before, with at most 1,024 states. **[C][V][O]**

### `0x01` words with bit 39 set are 32-bit computes **[C][V]**

The decoder read every VISA word with first byte `0x01` as 48-bit Type2a.
When frame bit 39 (bit 7 of the first parcel) is 1, the instruction is 32
bits wide: an unconditional compute whose 23-bit field is
`((parcel1 & 0x7f) << 16) | parcel2`, frame bits 38:16. The public Selache
decoder applies this width rule to every `0x01` word (`selinstr/src/visa.rs`,
`visa_width`: "Type 2: sub5=00001, bit7=1→32b, bit7=0→48b";
`decode_32_type2b`). The PRM has no 32-bit form for this prefix. Its Type2b
(prefix `0xc0`) is a different encoding.

In the aligned Digitakt II 1.16 sweep (`tools/sharcflow.aligned`, minimum
depth 8), 1,880 of 2,396 Type2a words have bit 39 set. For them, the next
word decodes confidently 4 bytes later in 99.1% of cases and 6 bytes later
in 49.0%. For the 516 words with bit 39 clear, the 48-bit reading gives
92.4%. A second agent recomputed these numbers from the image and quoted the
Selache source. **[V]**

`tools/sharcspec/build_table.py` now emits the form as confident
`Type2a_short` (mask `0xff8000000000`, value `0x018000000000`, VISA only).
Classic ISA decoding keeps Type2a. In the aligned 1.16 sweep,
`Type21p_undoc16` words drop from 513 to 11 and `Type22p_undoc48` words from
75 to 2. `tools/sharcpcode.py compare` reports no regressions on the three
images. The earlier boundaries read as follows: **[C][V]**

| PC | earlier reading | 32-bit reading |
| ---: | --- | --- |
| `0xb893fb` | Type2a, undefined ALUOP `0x00` | `R2 = LEFTZ R8`, then a relative jump to `0xb8941e` |
| `0xb8783f` (lock routine `0xb87838`) | Type2a, then undoc16 `0x0000`, `0x001d` | `R2 = LEFTZ R2`, then a relative jump to `0xb8785e` |
| `0xb8c615` | Type2a, then undoc16 `0x0000`, `0x001a` | `R1 = BTGL R2 BY R1`, then a relative jump to `0xb8c631` |
| `0xb8b0eb`, `0xb8b141` | Type2a, then undoc16 pairs | `COMPU(R4, R2)`, then relative jumps to `0xb8b114`, `0xb8b164` |

The callback stop at `0x1c0efa` also goes away: the tracer reached it through
the misaligned stream. The `0x0000` words there remain undocumented where
they really occur. **[V]**

A whole-image width comparison against Selache on the three images finds
364, 395 and 434 other disagreements. By the same successor test every one
favours the existing table. For example, reading Type4a words with first
parcel bit 0 clear as 32-bit (Selache's rule) makes `Type21p_undoc16` words
four times more common. **[V]**

### A delayed call returns after its second delay slot **[C][V]**

The tracer returned from every delayed call to call + 7 short words. The
firmware's CJUMP returns in software: the second delay slot stores
`own address + 2`, the callee loads that value into I12, and
`JUMP (M14, I12) (DB)` with M14 = 1 continues after the store. After a 16-bit
`3c` push that is call + 7; after a 48-bit `3a` push it is call + 9. The old
rule returned two short words into the store and decoded half of its literal
as `Type22p_undoc48`. The stops at `0xb89376` (strict) and `0xb868c0`
(callback) were this artifact. In the strict run the epilogue now loads
I12 = `0xb89409` for the call whose return is `0xb8940a`. **[C][V]**

### Indirect branches use DAG2 registers **[C][V]**

`JUMP/CALL (Md, Ic)` uses DAG2: Ic is I8-I15 and Md is M8-M15 (SHARC+ PRM,
DAG chapter: "DAG2 supports indirect branch addressing"; ADSP-2136x PGR:
"Ic indicates a DAG2 index register (I15–8)"). The return idiom
`0x083f343f` (`pmi=4`, `pmm=6`) is therefore `JUMP (M14, I12) (DB)`. The
tracer now checks that I12 + M14 equals the recorded return when both are
known; in the strict run four returns are checked and all match. Other
Type 9 jumps now go to I(8+pmi) + M(8+pmm) when both are known. **[C][V]**

### Type3a and Type6a scale the modifier under 32-bit normal words **[C][V]**

The Type3a and Type6a memory handlers added M to I unscaled, while every
other normal-word access scales it by 4. In the strict run, a Type3a push
`DM(I7,M7)=R2` in a nested call moved I7 by one byte, and the next store
overwrote the saved I6. With scaling, RFRAME restores I6 = `0x26f7e0` and the
return check above passes. **[C][V]**

### Status flags and conditions **[V]**

The tracer now updates ASTATX per the PRM instruction pages: add, subtract,
increment, decrement and negate set AC/AV/AN/AZ and clear AS/AI/AF; pass,
not, and, or and xor clear AC/AV/AS/AI/AF and set AN/AZ; comp and compu also
shift CACC; the shifter operations set SZ, SV and SS as each page states
(for example, LSHIFT sets SV for any left shift, and LEFTZ sets SV when the
result is 32); multiplier operations make MN/MV/MU/MI unknown, and the MR
data move clears them. ASTATX is tracked per bit, so an operation that
defines some flags does not need the others to be known. The conditions
LT, GE, LE and GT follow PGR p.4-93 / PRM p.4-53, with
X = (¬AF ∧ (AN ⊕ (AV ∧ ¬ALUSAT))) ∨ (AF ∧ AN) ∨ AZ (LE is X, GT is ¬X) and
Y = (¬AF ∧ (AN ⊕ (AV ∧ ¬ALUSAT))) ∨ (AF ∧ AN ∧ ¬AZ) (LT is Y, GE is ¬Y).
AC, MV, MS, SV and SZ conditions read their bits. Identical states are
merged. Before these changes the strict run split into 1,072 states; it now
follows one path to step 7,058 and 66 paths after that. **[V]**

### Current boundaries **[V][O]**

All 66 strict states stop at `0x1c1460`, raw `0x04bfc0800000`, which the
table decodes as uncertain Type7d. The PRM ACONV table (p.355) suggests
`I7 = B2W(I7)`; this is not yet checked. No state reaches PCG-C, SPORT4A,
DMA10, `0x1ca6d6`, `0x1ca7e4` or the marker store. The only access in
`0x31000000..0x310fffff` is the RCU0+0x2c store at `0x1c1414`. **[V][O]**

The callback replay from `0x1c7749` is calibration, not qualification. It
reaches the marker store `0x1c7586` in 118 of 1,041 states. The store writes
`0x7fffffff` to `I4 + M5*4`, but M5 is not set on that entry path, so the
buffer is not resolved. Its other stops are `0x1c7521`
(`JUMP (M13, I12)` with unknown registers) and, before the negate above was
added, `0x1c7599` (ALU `0x22`). **[O]**

### Static results for the DMA/SPORT slice **[D][O]**

- `0x1ca7e4` has four direct callers, each with an immediate R8:
  `0x1c7971` (`0x2620c8`), `0x1c79f8` (`0x262100`), `0x1c7af9`
  (`0x264138`), `0x1c7b9e` (`0x264170`). The image holds no literal
  `0x1ca7e4`, so no pointer table calls it. **[D]**
- For the two large lists, R4 is the slot that the setup call `0x1ca58a`
  has just filled: `R4 = DM(0x261a00)` at `0x1c7a6a` for `0x264138`, and
  `R4 = DM(0x261a04)` at `0x1c7b1d` for `0x264170`. The SPORT/DMA object P
  arrives in I2 and is not written in `0x1c7749..0x1c7b9e`. Whether P and the
  slot object are the same is open. **[D][O]**
- `0x1ca7e4` reads the byte at R4+0x30 (`0x1ca802`). One branch at
  `0x1ca807` returns at once; the other calls the lock routine `0xb87838`
  before any descriptor work. **[D]**
- No instruction or data word in the image holds an absolute SPORT4A/B or
  DMA10/11 register address. The one data word `0x31023000` is the SPORT
  record field at DM `0x2696a0` (`0x26968c + 0x14`). The driver can reach
  these registers only through the object chain. Two agents found this with
  different methods. **[V]**
- No writer of the selector DM `0x25f780` was found; it is read at
  `0x1c7524`, `0x1c7578` and `0x1c75b6`. **[D][O]**
- The template CFG word `0x00100000` sets only INT. EN, WNR, FLOW, NDSIZE,
  MSIZE and PSIZE are 0, and MSIZE/PSIZE 0 are not listed values
  (ADSP-2156x HWR, DMA_CFG fields, pp.1286-1293). The word cannot be the
  final DMA10_CFG value; `0x1ca7e4` or its callees must add fields. **[D][O]**
- `0x007fffff` is `0x7fffffff >> 8`, the top 24 bits. The HWR says SPORT
  words shorter than 32 bits are right-justified in the transmit buffer
  (p.1050), which would send the low 24 bits. The link must use another word
  length, packing or framing. The SPORT4A control value is still unknown.
  **[D][O]**

## Type 7 corrections carry strict startup into the DAI setup **[C][V][O]**

### Type7a keeps its modifier register **[C][V]**

`tools/sharcspec/build_table.py` dropped frame bit 29 of Type7a. Its merge
rule handles a bit the PRM prints and the classic grid leaves blank, and a bit
both fix, but not a bit the classic grid declares a field and the PRM figure
does not bracket. Bits 29-27 are the M register selector, the field Type7b's
own PRM figure carries at the same place, and the PRM figure brackets only 28
and 27 because it prints the same bits its Type7d ACONV figure names `breg`
and `toby`. With the field restored, the tracer applies the modifier with the
documented normal-word scaling. Before this, every Type 7a modify lost its
index register: `MODIFY(I7, M7)` at `0xb8946a` made the stack pointer unknown,
and the frame stayed unknown for the rest of the run. **[C][V]**

### Type7d is ACONV, and the strict run executes it **[C][V]**

PRM Table 14-22 gives Type 7d as the Type 7a word whose condition is 11111 and
whose compute field is empty, so those bits select the form. They are now
pinned into its mask and the form is confident. PRM Table 6-4 (p.6-16) gives
`Id = B2W(Is)` as "Likely semantics Id <- Is >> 2", with `W2B` the mirror,
hedged by "Exact semantics depend on address map" and an illegal-address trap
for addresses with no equivalent. The handler applies the shift, marks the
event as the manual's likely semantics, and stops rather than guess when the
source is unknown.

The firmware corroborates the pair. At `0x1c1460..0x1c1478` eight ACONV words
convert I7, B7, I6 and B6 to word addresses and back again:

```text
0x1c1460  b2w  I7  0x26f7f0 -> 0x9be7c
0x1c1463  b2w  B7  0x26f000 -> 0x9bc00
0x1c1469  b2w  I6  0x26f7f0 -> 0x9be7c
0x1c146c  b2w  B6  0x26f000 -> 0x9bc00
0x1c146f  w2b  I7  0x9be7c  -> 0x26f7f0
0x1c1472  w2b  B7  0x9bc00  -> 0x26f000
0x1c1475  w2b  I6  0x9be7c  -> 0x26f7f0
0x1c1478  w2b  B6  0x9bc00  -> 0x26f000
```

The round trip is exact, and the tracer's own return check agrees: every
`JUMP (M14, I12)` return in the run matches the call it came from. **[V]**

### Type7d keeps symbolic conversion provenance bounded **[D][O]**

The native tracer now continues a Type7d whose source is an `Affine` value,
but does not claim an architectural address-map result.  W2B multiplies an
affine expression by four; B2W divides only when every known affine
coefficient and constant is four-aligned.  For B2W with unknown low bits it
uses a stable, source-derived opaque `aconv_b2w_...` symbol instead of treating
right shift as affine.  An `Unknown` source still stops the state.  The
existing `aconv` event and its `semantics="prm-likely"` tag remain the boundary
between this tracer approximation and hardware behavior. **[D][O]**

This follows the public SHARC+ PRM Rev. 1.5 Figure 14-21 / Table 14-22
(PDF pp.352–355, Type7d form and register-bank/class rows) and Table 6-4
(PDF p.201 / printed p.6-16): B2W/W2B are documented as likely `>> 2`/`<< 2`,
but a missing equivalent address retains the input and raises ILAD.  The
tracer does not model that map or trap.  The PRM also lists Type7d rows with
an optional condition and parallel compute; this phase intentionally admits
only the pure ACONV selector (`cond=11111`, both compute fields zero) pinned
by `tools/sharcspec/build_table.py`.  Compute-bearing/conditional rows do not
enter this handler and remain outside this phase. **[D][O]**

### Type7d pure-selector SLEIGH p-code is a bounded approximation **[V][O]**

The generated SHARC VISA language enumerates only the table's pure Type7d
selector (`cond=11111`, empty compute): `g` chooses I0–I7/I8–I15 or
B0–B7/B8–B15 with `breg`, and `idis` maps the destination as source-selector
XOR `idis`.  Its p-code writes the selected destination from the selected
source with the PRM-likely B2W `>> 2` or W2B `<< 2`; this makes the previously
empty lift for `bf0480c00000` nonempty.  Independent manual/byte and p-code
reviews checked the selector, mapping, byte, generated constructors, and
focused tests, which cover both register classes, both DAG banks, XOR
destinations, both directions, and reject a compute-bearing byte from this
pure constructor. **[V]**

The generated p-code deliberately omits the PRM's address-map lookup,
retain-input-on-no-equivalent behavior, and ILAD. Conditional and
compute-bearing Type7d forms remain outside this constructor family, and
Type11a is untouched.  Consequently, the concrete backend's likely-shift
result for `0x26f7f0` is `0x09bdfc`, not the observed mapped hardware result
`0x09be7c` recorded above.  Hardware equivalence beyond the represented shifts
is not claimed; reproducing the address-map conversion remains open. **[V][O]**

The hand-built byte fixtures exercise that pure selector only.  A reproducible
local byte check is available from the ignored DT2 loader stream:
`shasum -a 256 out/sections/dt2-1.16/section_7_BLOB.bin` gives
`0f514a12a2255f5c081e292c47f1f29462003177658da4bbae0a22fd737fffa2`,
and `LoadedMemory.from_stream(...).read(sw_to_byte(0x1c1460), 6)` gives
`bf0480c00000` (raw `0x04bfc0800000`).  An independent check reproduced the
hash, bytes, decode, and cited public-manual form.  This exact occurrence is
**[V]**; no firmware behavior or ILAD-free execution is claimed. **[O]**

### Type14d and Type15a **[V][O]**

Both gain handlers from their PRM pages. Type15a is already confident, with
270 aligned instances in 1.16. Type14d stays uncertain, and a second agent
checked why: its PRM figure is transcribed correctly, but nothing independent
pins its seven fixed bits. There is no classic form to compare (its fixed bits
differ from Type14a at bit 42), no cross-reference table like the one that
settles Type7d, and the public Selache decoder models the form wrongly: it
requires bit 40 to be 1, which is the direction field, so it misses every load,
and it decodes the register as a universal register with a blanket width
suffix where the PRM restricts it to the R register file and gives six width
and extension rows. Promoting it on firmware counts alone would set a
precedent for every SHARC+-only form, so it waits for a decision. **[O]**

Instead, `tools/sharc_trace.py` takes `--allow-provisional-form NAME`, which
executes a named unconfirmed form and records it on every state that used one.
Any run that names a form is calibration, never qualification. **[V]**

### Startup stops at a boot-source probe **[V][O]**

The strict run is boot bring-up code. It configures the caches and MODE1,
clears a block of DM, then probes two candidate boot sources in a loop: it
reads `DM(0x80000010)`, which the image provides as 0, and then
`DM(0x10000000)`, which no section of the firmware populates. The branch on
that second read forks, and the path that falls through jumps through
`JUMP (M13, I13)` at `0x1c144c` with I13 holding the value just read. The
target therefore depends on what the hardware has at that address, not on
anything in the image. This is a real boundary, not a tracer gap. **[V][O]**

The qualifying strict run ends in two states: `0x1c144c` above, after 7,545
instructions, and `0xb8cdaf`, an unconfirmed Type14d, after 7,566. **[V]**

### The calibrated continuation reaches the DAI setup **[D][O]**

With `14d` named as a provisional form, and therefore as calibration, the run
reaches 11,597 instructions and executes the DAI and PADS setup at
`0x1cb28e..0x1cb323`. It writes exactly the 34 DAI stores and the two PADS0
stores already recorded from a direct entry, including `0x3def7b9c`,
`0x3ef83fbe`, `0x0fdf9d38` and `0x000fffff`, and it also resolves the three
stores that run left open: `0x310c91e4` takes 1, `0x310c91e8` and `0x310c91ec`
take 0. Every one of the 3,730 returns checked in that run matches its call
site. **[D]**

The run still reaches no SPORT4A, DMA10 or PCG register, so the cadence and
framing questions stay open. It ends on the state budget, on a floating-point
compare (ALU opcode `0x8a`, PRM Table 18-5) that the tracer does not model,
and on the boot probe above. A block of about 295 accesses in
`0x3108b000..0x3108bc20` is not named in these notes and looks like a table
being cleared. **[O]**

## The SHARC machine-type consumer: received and cached, no dispatch found **[D][O]**

Investigated 2026-09-20 to decide whether a new machine needs SHARC synthesis
code (roadmap A5). Static reads only (loader-backed `decode_at`), so INFERENCE,
not qualified. Three passes over `out/sections/dt2-1.16/section_7_BLOB.bin`
(sha `0f514a12...`):

- **Receive site (reproduces existing [V]).** The `0x94 + 2i` machine word is
  read and change-tested at `FUN_001c2b24` (call `0x1c771e -> 0x1c2b24`; load
  `0x1c33d2`, `R0 = DM(I0,M0)`; compare `0x1c33d7`; equal/not-equal paths join
  at `0x1c33e9` and store a derived scalar to `DM(I5+0xc4)`). It is a
  change-detector, not a dispatch, and carries **no bound/range check** against
  the ColdFire's 0..6 machine range. `FUN_001c2b24`'s sole caller is `0x1c768c`
  (Ghidra: 1 caller); `I5` there derives from an argument of `0x1c768c`'s own
  caller, so the **absolute DM address of the per-track machine cache is not yet
  pinned** (`DM(I5+0xc4)` is frame-relative). **[D][O]**
- **No per-machine dispatch table in the scanned regions.** A whole-image scan
  for runs of >=5 consecutive code-address words (main program `0x1c0000` region
  and the L2 driver overlay `0xb8xxxx` -- everything the loader stream populates)
  found only: the already-documented RPC command table (`DM 0x2577c4`, 11
  entries), the documented PCG 4-pointer table (`DM 0x2d7158`), and ADI SSL
  driver-service bookkeeping in the `0xb87xxx-0xb8dxxx` overlay (one run adjoins
  the ASCII string `"ASSERT [ADI_GPIO_CALLBAC..."`). No table of distinct
  synthesis-routine targets exists in that data. The 12 indirect calls
  (`COMPUTED_CALL`, raw `0x3f083f2c`, the documented idiom) are spread across 12
  unrelated functions and load their targets from locals, not an indexed table;
  none was shown machine-keyed. **[D][O]**
- **Not ruled out:** a compiler-emitted compare-chain dispatch (up to seven
  `type==N` branches, no table -- invisible to a data-table scan); the readers of
  the (unpinned) machine-cache DM address; and code in the external-memory blocks
  the loader places at `0x8045a6c8` (DT2 1.16 loads only 3,316 bytes there).

**The real gap: the SHARC audio synthesis engine is unlocated.** These passes,
like the prior interface work, stayed in the control/bring-up code (frame
receive, SPORT/PCG/DMA, boot). No per-frame/per-voice audio-render routine -- the
code that reads the sample buffers and produces output for SPORT/SSI -- is named
anywhere in FINDINGS. A whole-image scan did turn up float ramp/interpolation
tables in external memory (near `0x8055c840`), a plausible synthesis-table lead.
All DT2 machines are sample-based variants (SAMPLE/WERP/STRETCH/REPITCH/SLICED/
MANUAL SLICE), which makes a single parameter-driven sample engine (machine type
selecting a mode/params) at least as plausible as separate per-machine kernels.
Deciding A5 -- and building a machine that makes a new sound -- needs that engine
located first. **[O]**

## The SHARC audio engine: ingredients located, control flow runtime-assembled **[D][O]**

Three parallel static passes (2026-09-20, loader-backed `decode_at` on
`section_7_BLOB.bin` sha `0f514a12...`) hunting the per-voice synthesis engine.
They located the engine's *ingredients* but not its running control flow; the
edges are runtime-established, so static analysis stalls here and the emulator
(gated by A2) is the natural next tool. OBSERVATION unless marked INFERENCE.

**Audio output buffers (OBSERVATION).** Four DMA descriptor rings, all built by
the same `0x1ca58a` setup + `0x1ca7e4` submit, all with config `0x00100000`
(decoded against ADSP-2156x HWR DMA_CFG: EN=0, WNR=0 = **transmit**, INT=1 =
interrupt on X-count) -- so all four are output/transmit, none receive:

- Ring A: head `0x2620c8`, buffers `0x261cc8`/`0x261dc8`, 256 B (setup/submit
  `0x1c792f`/`0x1c7971`); Ring B: head `0x262100`, `0x261ec8`/`0x261fc8`, 256 B
  (`0x1c79c4`/`0x1c79f8`).
- Ring C: head `0x264138`, buffers `0x262138`/`0x262938`, 2048 B
  (`0x1c7ab7`/`0x1c7af9`); Ring D: head `0x264170`, buffers `0x263138`/`0x263938`,
  2048 B (`0x1c7b6a`/`0x1c7b9e`) -- a second full ping-pong ring, new to the crib
  sheet. The two 2048 B rings are the audio-output ping-pong pairs (INFERENCE:
  candidates for SPORT4A-TX / SPORT4B-TX). No literal reference to any ring
  buffer exists outside descriptor construction -- the render loop writes them
  through a runtime pointer, so the writer is not findable by literal scan.

**Synthesis tables + reader code (OBSERVATION).** Only two real external float
payloads load (rest of `0x80xxxxxx` is FILL/zero scratch), LE float32:

- Block A `0x8045a6c8`, 829 floats: exponential curve, denormal -> exactly 1.0
  (INFERENCE: pitch/note-to-freq or dB/exponential envelope map). Loaded at boot
  by `0x1c1686` (`R8 = 0x8045a6c8`) then passed to a `25a_direct` call at
  `0x1c168c -> 0x1c7442` with a second (internal) address -- the shape of a
  boot-time copy/expand into internal memory.
- Block B `0x8055c440`, 2324 floats. Table1 (idx 0-255, `0x8055c440..0x8055c83c`)
  is a **folded quarter-wave cosine**, `value(k) ~= cos(min(k,256-k)*pi/256)` --
  the classic single-table sin/cos generator. Read at `FUN_1c71ec` via two DAG
  pointers 128 words apart: `I5 = 0x8055c440` (`0x1c724f`, value 1.0) and
  `I5 = 0x8055c640` (`0x1c7247`/`0x1c7263`, the fold-point, value 0.0). Table2
  (idx 256+) is another exponential-shaped curve with a discontinuity past the
  DT2 payload boundary (DN2 1.11 loads far more here); read at `0x1c6c16`
  (`I4 = 0x8055c874`) inside a large routine `~0x1c6156..0x1c71e7`.
- **Boot-time relocation hypothesis (INFERENCE):** if the tables are copied into
  internal SHARC memory at boot (`0x1c1686 -> 0x1c7442`), the real per-frame
  synthesis reads *internal* addresses and would never appear in an `0x80xxxxxx`
  literal scan -- which explains why no per-frame render loop was found touching
  these addresses. Verifying this needs decoding `0x1c7442`.

**Per-track processing structure (OBSERVATION).** `FUN_001c2b24` (the frame-RX
consumer) contains a **uniform 16-track counted loop** (`0x1c2c97`, `TRACKS=16`)
that calls **one** routine `0x1c24e9` per track with a `track*0x60` (96-byte)
stride -- no per-machine branch at this level. `0x1c24e9` computes the per-track
stride, reads `DM(0x255934)`, and hits an ALU `MAX` (opcode `0x62`, PRM Table
18-5) the tracer does not model -- a clean decode boundary, a candidate
clamp/limit step; decoding past it is the top per-track lead. Separately,
`FUN_001c2b24` reads per-track TX-mirror fields `{0x54, 0x73c, 0x75c, 0x94}` off
base `I4`/`I1` and caches a derived word to `DM(I5+0xc4)`. Unit note: `Type19a`'s
16-bit offset is a byte literal (`0x94`, `0x73c`...), while `Type15b`'s 7-bit
field is a normal-word index (`49*4 = 0xc4`) -- reconciles the `+0xc4` cache
offset. A whole-image `Type19a` scan found 8 other routines
(`~0x1c8900..0x1cd600`) forming pointers to the same `0x34`/`0x54` per-track
fields; none is reached by a direct call, none decoded past pointer formation --
the most promising concrete lead for a follow-up decode pass.

**Why static stalls (INFERENCE).** The engine's control flow is runtime-built:
its routines (`FUN_001c2b24`, `FUN_1c71ec`, the table readers, `0x1c24e9`) have
no static direct callers -- they are entered via tasks/callbacks or the image's
12 unresolved indirect calls (`COMPUTED_CALL`, raw `0x3f083f2c`); the tables are
relocated to internal memory; the output buffers are addressed by runtime
pointers; and per-track state (`I5`) is stack-relative off a runtime `I6`. So the
ingredients are now mapped but assembling them into the running per-frame engine,
and proving whether any per-machine branch hides deep in `0x1c24e9` or the render
kernel, needs dynamic execution.

**Bearing on "does a new machine need SHARC code" (INFERENCE, strengthened but
not proven).** Every static level examined -- receive/cache, the 16-track loop,
the synthesis tables (general DSP primitives, not per-machine), and the absence
of any dispatch table -- points to a **uniform, parameter-driven engine** where
the machine type is one per-track parameter, not a selector of separate kernels.
If that holds, a new machine is largely a new parameter/mode configuration
(ColdFire-side, where `tools/machinepatch.py` already clones a machine slot),
not new DSP code. Unproven: the undecoded tail of `0x1c24e9`, the 8 field-reader
candidates, and any branch inside the (runtime-only) render kernel could still
hide per-machine behavior. **[D][O]**

**Firming pass (2026-09-20): the per-track routine and field readers are uniform
(OBSERVATION).** Two decode passes closed the leads the paragraph above left open:

- `0x1c24e9` (called once per track from the 16-track loop) fully decoded, entry
  to return: 396 instructions, a leaf (zero CALLs), no loop, no computed/indirect
  jump but its own return. No `comp`/`compu` ALU op anywhere and no
  AZ/AN/LT/LE/GT/GE-conditioned branch -- none of the shape a `switch(type)` or
  `if(type==N)` chain needs. Its four conditional branches all test the
  shifter-zero flag right after a bit-toggle/shift (per-track boolean flags), and
  the FINDINGS "MAX at 0x1c2530" is now resolved as a two-sided clamp
  `R1 = min(max(R1,R3),R4)` (`0x1c2530` MAX, `0x1c2532` MIN). The body is a float
  convert/multiply/clamp/bit-test parameter pipeline addressed through I6 (word
  indices 5-126); it never reads the machine-type cache word (index 49 / `0xc4`
  absent). Full-body OBSERVATION, not inference: `0x1c24e9` is uniform, no
  per-machine dispatch.
- Of the 8 other per-track field (`0x34`/`0x54`) readers, six decode as uniform
  (generic field marshalling; a shared compiler check idiom -- byte-identical
  `2a_short` computes recurring across unrelated routines; and `0x1c9fd5`'s
  count-bounded callback-registration loop). No compare-chain against 0..6 and no
  indexed jump in any of them.
- **Two residual sites, not closed:** `0x1cc225` (`comp(R9,R14)` -> EQ; `R9-1==0`
  -> EQ) and the twins `0x1cc44e`/`0x1cc4cb` (`compu` vs literal `3` -> GE) have
  genuine two-way compares, but their non-constant operand was not traced to the
  machine-type field. A two-way test against a register or the literal 3 cannot
  by itself select among 7 machines, so these are unlikely to be a machine
  dispatch (more plausibly a stereo/mode/bounds flag); provenance untraced. **[O]**

Net: the "uniform parameter-driven engine" reading is now OBSERVATION at the
per-track processing routine and 6/8 field readers, with two two-way compares and
the runtime-only render kernel the only residual uncertainty. A second-agent
byte-check is still owed before any of this is marked **[V]**.

Tool gap noted: `tools/sharc_trace.py` `_execute()` has no case for Type
`8a_rel`/`8a_abs`, so symbolic runs stop at the first Type8a branch -- worth
adding for future SHARC symbolic tracing (and the A2 work). **[O]**

## The frame displacements are raw ColdFire bytes, unscaled **[V]**

Settled, and it matters because every offset claim about the DSP's view of the
frame depends on it. `FUN_001c2b24` forms its frame pointers with SHARC+ form
**`19a`** (plain, not `19a_scaled`) -- a 48-bit `Ireg += imm32`. Decoding the
`data` fields directly:

| addr | form | I-reg | data | hex |
| --- | --- | --- | --- | --- |
| `0x1c2cc1` | 19a | I4 | 1884 | `0x75c` |
| `0x1c2ccb` | 19a | I1 | 148 | `0x94` |
| `0x1c2cd7` | 19a | I4 | 1852 | `0x73c` |
| `0x1c33da` | 19a | I4 | 84 | `0x54` |

All four match the ColdFire byte literals in value *and* register.

The proof does not even need the ColdFire side. The TX frame is `0x802` = 2050
bytes (`tools/framelink.py` `TABLES`). Word-scaled (x4), `0x73c`/`0x75c` would be
bytes 7408/7536 -- more than three times the whole frame. Short-word scaled (x2)
gives 3704/3768, still past the end. Only the raw-byte reading fits.

The contrast is instructive: the same function's prologue at `0x1c2b24` uses
`19a_scaled` (`I7 += -48`, normal-word, = -192 bytes, a stack allocation). Two
visually similar forms, two different units. The frame-pointer code consistently
uses the unscaled one.

## `FUN_001c2b24` makes 32 calls with an integer, not 16 with a pointer **[C][V]**

Corrects the "Per-track processing structure" note above, which recorded
`FUN_001c2b24` as calling `0x1c24e9` **once per track**, 16 times.

The hardware `LCNTR` loop is set up at `0x1c2c94` (form `12a_imm`, count 16) with
body `0x1c2c97`-`0x1c2cb2`. Each iteration makes **two unconditional calls** to
`0x1c24e9`, passing a plain incrementing integer in `R12`: `R12=R13`, call,
`R13++`, `R12=R13`, call. Across the loop the argument runs 0..31.

So it is 32 calls, and the argument is a small integer -- not a track-relative
frame pointer. Sixteen tracks x two is the obvious reading (stereo, or two voices
per track) but that is inference, not shown. The consequence for the frame work
is that `FUN_1c24e9`'s own `I6` addressing (word range 5-126, previously
documented) is **not** shown to be the ColdFire frame; it is more likely that
routine's own stack frame, with the real parameter access derived from `R12` by
code not yet traced.

## No read of the frame's per-track parameter block found yet **[D][O]**

The ColdFire places the four parameter pages in a per-track `0x60`-byte block at
frame byte `0xda + track*0x60` (see `04-coldfire-dsp-link.md`, "The mirror index
to TX frame map"). Two independent searches over the whole main program block
(blk93, file offsets `0x31c3c`-`0x4b470`, 104,500 bytes, decoded sequentially
with resync-on-desync: 22,646 instructions, 99.3% confident, 8 desync points
losing ~16 bytes) found **no read site**:

- **Literal displacements.** Zero `19a`/`19a_scaled` instructions anywhere carry
  `0xda`, `0xfa`, `0x116`, `0x130`, `0xde` or `0xe6`. The already-known scalar
  fields do recur as expected (`0x54` x4, `0x94` x2, `0x73c`, `0x75c`), matching
  the "8 other per-track field readers" already recorded -- so the method does
  find real frame reads when they exist.
- **Stride `0x60`.** The literal 96 appears 19 times in blk93 and every one is
  explained: consecutive stack-slot indices in prologues (`I6+90,91,...,98`,
  callee-saved spill areas) or unrelated small constants. No `I += 0x60` and no
  M-register modifier load of `0x60`.
- `0xec` (LEV) produced three in-region `19a` hits, all investigated and rejected
  as value collisions: `0x1c5b57` sits in a run of consecutive-by-one offsets
  (52,53,54,55,56 -- a byte/flags struct), and `0x1c6a90`/`0x1c6ebd` sit inside
  the synthesis-table reader alongside sibling adds of 72, 212, 20 and 300 off
  the same base, none of which match any frame field.

**What this negative does not cover**, stated precisely per the repo rule:

1. Pointer arithmetic synthesised by shift/add rather than a literal -- a
   compiler can build `track*0x60` as `(t<<6)-(t<<5)` and never materialise 96.
   No `0x60`-into-register load was found to seed such a multiply either, but
   shift/add combinations were not exhaustively enumerated.
2. De-interleaving done in the SPORT/DMA descriptor rather than in program code,
   in which case no program literal would exist. The frame's receive-side DMA
   descriptor construction was not inspected.
3. The interior of `FUN_1c24e9` past its prologue. Only the caller side was
   traced.

Point 3 is the strongest remaining lead: `FUN_1c24e9` is the 396-instruction
uniform per-track routine with no per-machine branch, it is called 32 times with
an integer 0..31, and an index-to-address computation inside it is exactly the
shape that point 1 says a literal search cannot see.

## The ColdFire frame is mapped into SHARC DM at `0x2558dc` **[V]**

The link's other end, found by decoding `FUN_1c24e9` and reading its literal
bases back against the ColdFire frame map. This closes the transport question
that the per-track-block audit above left open: the SHARC does receive the
parameter pages, at byte-identical offsets.

`FUN_1c24e9` computes a per-track pointer with a literal multiply -- which is
why the earlier scan for a `0x60` stride, and the scan for shift/add synthesis
of 96, both missed it: it is neither an immediate displacement nor a shift
sequence, it is an integer multiply by a register loaded with `0x60`.

```
0x1c250e  m4=r12                        ; M4 := track index
0x1c2512  r2=0x60
0x1c2514  r2=r12*r2 (ssi) , m3=r8       ; R2 := track * 0x60
0x1c251a  i4=r2
0x1c2537  i2=modify (i4,0x2559b6)       ; I2 := track*0x60 + 0x2559b6   (19a, unscaled)
```

Subtract a common base of `0x2558dc` from every literal this function and its
caller use, and all eleven known per-track scalar frame offsets land exactly:

| SHARC literal | - `0x2558dc` | ColdFire frame field |
| --- | --- | --- |
| `0x2558de` | `0x02` | per-track scalar |
| `0x255910` | `0x34` | per-track scalar |
| `0x255930` | `0x54` | read by `FUN_001c2b24` |
| `0x255950` | `0x74` | per-track scalar |
| `0x255970` | `0x94` | machine type |
| `0x255990` | `0xb4` | per-track scalar |
| `0x256018` | `0x73c` | read by `FUN_001c2b24` |
| `0x256038` | `0x75c` | read by `FUN_001c2b24` |
| `0x256058` | `0x77c` | per-track scalar |
| `0x256078` | `0x79c` | per-track scalar |
| `0x256098` | `0x7bc` | per-track scalar |

Eleven independent hits with no exceptions is not coincidence. And
`0x2559b6 - 0x2558dc = 0xda` -- exactly the frame offset where the per-track
`0x60`-byte parameter block begins. So `I2 = 0x2559b6 + track*0x60` **is** track
`t`'s parameter page, in the frame's own byte numbering.

**Addressing units, resolved** (this has caused repeated errors, so it is worth
stating precisely): the unit is per *instruction form*, not global.

| form | scaling |
| --- | --- |
| `19a` (`modify`, classic) | raw bytes, always x1 |
| `19a_scaled` (`modify (nw)`) | x2/x4 by width |
| `15b`/`4a`/`4b` (`dm(K,I)`) | x4 under 32-bit normal words |
| `3b`/`3c` (M-register indexed) | x2 if short-word tagged, else x4 |

32-bit normal words is already established for this firmware. So base pointers
built with `modify` stay byte-exact, while loads through them advance four bytes
per immediate unit -- each mirrored 16-bit ColdFire field occupying one 32-bit
SHARC slot. That also explains the descriptor-shaped structure at `0x268220`
carrying `XCNT = 1025`, `XMOD = 4`: `0x802 / 2 = 1025` words, one per 32-bit
slot. The two facts are the same mechanism seen from two sides, not a
contradiction.

## `FUN_1c24e9` is the filter/amp/FX converter, and it does not read the SRC page **[V]**

> Corrected below: `FUN_1c24e9` does read five SRC-page words; see "`FUN_1c24e9` reads the SRC page" at the end of this file. **[C]**

Mapping every `I2`-relative read through the scaling rules above:

| site | form | block byte | mirror | page |
| --- | --- | --- | --- | --- |
| `0x1c26a5` `r11=dm(0x5,i2)` | 15b, x4 | `0x14` | -- | slice sub-block |
| `0x1c2699` `r14=dm(0x6,i2)` | 15b, x4 | `0x18` | -- | slice sub-block |
| `0x1c268b` `r4=dm(0x7,i2)` | 15b, x4 | `0x1c` | -- | slice sub-block |
| `0x1c26b7` `r10=dm(0xa,i2)` | 15b, x4 | `0x28` | 39 | filter |
| `0x1c2652` `r4=dm(0xb,i2)` | 15b, x4 | `0x2c` | 41 | filter |
| `0x1c268e` `r14=dm(0xc,i2)` | 15b, x4 | `0x30` | 43 | filter |
| `0x1c26d1` `r2=dm(0xd,i2)` | 4a, x4 | `0x34` | 45 | filter |
| `0x1c2659` `r8=dm(0x17,i2)` | 15b, x4 | `0x5c` | 67 | FX |
| `0x1c2622` `i0=modify(i2,0x22)` | 19a, x1 | `0x22` | 36 | filter |
| `0x1c267f` `i4=modify(i2,0x3e)` | 19a, x1 | `0x3e` | 50 | amp |
| `0x1c262d` `i0=modify(i2,0x42)` | 19a, x1 | `0x42` | 52 | amp |
| `0x1c2615` `i3=modify(i2,0x46)` | 19a, x1 | `0x46` | 54 | amp |
| `0x1c2633` `i1=modify(i2,0x4e)` | 19a, x1 | `0x4e` | 58 | amp |
| `0x1c263d` `i3=modify(i2,0x52)` | 19a, x1 | `0x52` | 60 | amp |
| `0x1c266d` `i4=modify(i2,0x56)` | 19a, x1 | `0x56` | 64 | FX |
| `0x1c2630` `i3=modify(i2,0x5a)` | 19a, x1 | `0x5a` | 66 | FX |

Note the loads are 32-bit while parameters are 16-bit, so each `dm(K,i2)` read
covers a **pair** of adjacent parameters, which the code then unpacks -- visible
at `0x1c26a5`, where `r11=dm(0x5,i2)` is followed by `lshift` by +-`0x10`. The
mirror index given in the table above is the first of each pair.

All sixteen feed a dense `leftz`/`float`/multiply/spill chain running to about
`0x1c2870` -- an ordinary audio-parameter conversion pipeline, values turned into
floats and scaled.

**Every one of them is filter, amp, FX or the slice sub-block. Not one touches
block bytes `0x00`-`0x12`, which is the entire SRC page** -- TUNE, PLAY, CFADE,
SAMP, STRT/SLICE, LEN, BARS/GRID/LOOP, LEV. This routine is the continuous
per-track parameter converter for the pages *after* the machine's own; the SRC
page must be consumed somewhere else, plausibly at note-on. **[O]**

**CFADE (block byte `0x04`, mirror 27) is not read here [V].** Three
register-indexed reads (`dm(m6,i2)`, `dm(m5,i2)`, `dm(m4,i2)`) have unresolved
offsets because M5/M6/M7 are never assigned in this function or its caller and
must come from further up the chain **[O]** -- but none looks like a CFADE
consumer: M6's feeds bit tests, M5's and M4's feed straight float/multiply
chains. And since nothing on the ColdFire side writes mirror 27 today, any such
read would see a constant zero.

**The function is uniform, confirmed [V].** Its only branches are four `8a_rel`
conditionals at `0x1c25c4`-`0x1c2604`, all testing bits of a flags word via
`btgl`. The bit positions come from a **track-independent global** (`R14`, from
`r1=dm(0x255902)` = frame offset `0x26`, no track index) and from a hardcoded
zero (`R3`, `r3=r3-r3`). The machine type is not loaded into any register until
`0x1c26d4`, *after* all four branches. So these are not a per-machine dispatch.

## `FUN_001c2b24` passes the track in R12 and a 0..31 counter in R8 **[C][V]**

Refines the correction above. The loop is:

```
0x1c2c91  r13=r13-r13 , r15=m5
0x1c2c94  lcntr=0x10, do (pc,0x1e) until lce
0x1c2c97  r8=pass r15 , r12=r13
0x1c2c9a  cjump 0x1c24e9 (db)
0x1c2ca3  r8=r15+1 , r12=r13          ; same R13 -- not yet incremented
0x1c2ca6  r15=r15+r14 , r11=m7        ; R14=2, so R15 += 2 per iteration
0x1c2ca9  cjump 0x1c24e9 (db)
0x1c2cac  r13=r13+1 , dm(i7,m7)=r2    ; R13 increments in the delay slot
```

So **R12 is the plain track index 0..15**, identical for both calls in an
iteration, and it is what drives the `*0x60` block addressing. **R8** is the
0..31 counter (`R15`, `R15+1`), and inside the callee it indexes a second
structure with stride `0xdc`: `0x1c2560 r0=r8*r0(ssi)` with `r0 = 0xdc`. Two
sub-slots per track; which two is **[O]**.

## Two search gaps from the earlier audit, closed **[D]**

**Shift/add synthesis: none.** Over the same 22,646-instruction decode of blk93,
all 600 shift-immediate instructions (`Type6a(mem)`, `Type6a(nomem)`,
`Type6b_shiftimm`) were decoded; 27 have an immediate of +-2, +-4, +-5 or +-6,
and none sits near a second shift on the same source register. Cross-referencing
against all 78 fixed-point ADD/SUB (`Type2a`, `Type2a_short`) within a
12-instruction window gave 6 candidate pairs, all rejected on inspection (the
add precedes the shift, so it uses the pre-shift value). Not enumerated:
`Type2c`/ShortCompute add/sub, and the "M register loaded with 96 from memory"
sub-case. The negative is moot anyway -- the stride turned out to be a literal
multiply.

**The receive descriptor.** A descriptor-shaped structure is built at `0x268220`
around `0x1c7e30`-`0x1c7f10`: ADDRSTART `0x268240`, CFG `0x00100000`, XCNT
`1025`, XMOD `4`, YCNT/YMOD from M5 (~0). `ADDRSTART + XCNT*XMOD = 0x269244`,
which is independently loaded into R4 two instructions earlier -- a self-checking
coincidence that supports the field reading. It is submitted via `0x1c834a` and
`0x1c83ff`, neither decoded. Note this destination is `0x268240`, **not**
`0x2558dc`, so it is not obviously the same buffer; whether there are two
descriptors, a TX/RX pair, or a generic builder is **[O]**.

## Whole-program sweep: the frame is referenced in exactly one region **[V]**

> **The scope claim in this section's first paragraph was wrong.** blk93 is not
> the entire executable program -- it is roughly a third of the SHARC code. See
> "The SHARC code is not one block" below. The sweep was re-run over every block
> and the *result* held: still zero frame references outside blk93.

A decode over **every word** of the extracted SHARC main program
(`out/sharc/dt2-1.16-main.bin`, blk93, 104,500 bytes, ~52,000 instructions --
wrongly believed at the time to be the entire executable program, on the basis
that loader blocks 94-103 are zero-FILL; the blocks *before* 93 were never
checked) scanning for any literal
in the frame's whole mapped range `[0x2558dc, 0x2560de]`:

**71 hits, every one between short-word `0x1c2517` and `0x1c33c1`** -- entirely
inside `FUN_1c24e9` and its caller `FUN_1c2b24`. `tools/sharcflow.py` confirms no
call targets anything between `0x1c2b24` and `0x1c33fe`, so `0x1c33c1` is still
inside the caller's body, not a third function.

The scanned range covers every per-track variant (`0x2559b6 + n*0x60` for all
n = 0..15) and CFADE's own per-track absolute addresses (`0x2559ba + n*0x60`).
**No other routine anywhere in the program loads a literal pointing into the
frame.** So there is no statically literal-addressed reader of the SRC page, and
**CFADE is not read anywhere reachable by this method [V]**.

A constant-propagation pass over `FUN_1c2b24`'s own body (modelling `17a`/`17b`
literal loads, `5a_move`/`5b_move` copies and `19a` modify) found only one
resolvable frame pointer, `0x1c3112 i2=modify(i5,0x7fc)` -- frame byte `0x7fc`,
the tail, nowhere near any track's SRC page.

### Two threads that could still overturn this

**1. A second `*0x60` pointer idiom with a stack-loaded base [O].** At
`0x1c33bc`-`0x1c33d0` in `FUN_1c2b24`, after the per-track call loop:

```
0x1c33bc  r2=0x60                            (17b)
0x1c33be  r12=dm(i6,-2)  ||  r2=r5*r2        (4a: parallel load + R2 := R5*R2)
0x1c33c1  i4=0x255970                        (17a -- frame 0x94, machine type)
0x1c33c4  r2=r2+r12     ||  i0=i10           (R2 := R2 + R12)
0x1c33c7  r1=dm(i4+m0*2) (sw, pre-modify)
0x1c33cc  i12=r2
0x1c33d0  i4=i12                             ; I4 := R5*0x60 + DM[I6-2]
```

Structurally identical to `FUN_1c24e9`'s idiom, but the additive term is loaded
from the stack rather than being an inline literal. **If `DM[I6-2]` holds
`0x2559b6` or `0x2558dc`, this is an SRC-page reader.** It could not be resolved:
`tools/sharc_trace.py` stops at `0x1c2c01` on an unhandled `14d source: prm`
form, before reaching the loop.

**2. `FUN_1c24e9`'s M-register-indexed reads [O].** M4-indexed reads occur
against six different base registers at `0x1c252b`, `0x1c2546`, `0x1c2554`,
`0x1c260a`, `0x1c260c`, `0x1c2613`, `0x1c261e`, `0x1c262b`, `0x1c26c5`,
`0x1c26d4`, `0x1c26dd`, `0x1c26e2`, `0x1c27d2` -- more than previously recorded.
M4 is **not** a constant track index throughout: it is reassigned at `0x1c27a9`
(`f5=float r0, m4=r2`). M5, M6 and M7 are never assigned in either function and
must be set further up the call chain. **If any of M5/M6/M7 holds a small value,
`dm(m5,i2)` and friends read the SRC page** -- and one of them,
`0x1c2594 r8=dm(m6,i2)`, is the flags word feeding the function's only branches.
Resolving M5/M6/M7 is the cheapest remaining way to settle the SRC question.

An incidental observation worth checking rather than trusting: `0x255970`
(frame `0x94`) and `0x255990` (frame `0xb4`) are exactly `0x20` apart, which
matches a 16-entry 2-byte packed array if `dm(m4,i4)(sw)` at `0x1c26d4` means
`0x255970 + track*2`. That would make the machine type a separate packed 16-track
array rather than a field inside the `0x60` block **[O]**.

## What the render loop investigation adds **[D][O]**

Pushed from the audio side rather than the parameter side; nothing overturns the
standing "ingredients located, control flow runtime-assembled" conclusion.

- **No voice structure or voice table found.** No interpolation loop with the
  canonical two-adjacent-samples-plus-fractional-weight shape was located, though
  an exhaustive whole-image scan for that shape was not run -- this is open, not
  a negative.
- **No slot-to-sample-pointer directory on the SHARC side.** A scan of all 26
  distinct external-memory literals (`0x80000000`-`0x8fffffff`) in the program
  found no N-entry address array. The best candidate for slot metadata remains
  the ColdFire's FlexBus window `0x8C000000`-`0x8C00000F`, read only by
  `FUN_400cf4a8`, `FUN_400cf534` and `FUN_400cf67c`.
- **The transmit rings still have no literal writer.** The one confirmed store
  (`0x1c7586`) writes only the marker word `0x7fffffff` through a runtime pointer
  no static path resolves.
- **`FUN_1c71ec` looks like a time-stretch kernel, not an LFO.** It loads two DAG
  register pairs to the cosine table's ends (`0x8055c440` value 1.0, `0x8055c640`
  value 0.0) and runs float multiply-accumulate chains against them
  (`0x1c7258 f12=mrf+f0*f7, f8=f8+f12`) -- interpolated table lookup from a
  fractional phase. Nearby: a buffer pair `0x8055c890`/`0x8055d890` exactly 4096
  bytes (1024 floats) apart with a `1023` loop bound set immediately before at
  `0x1cca02`, and one instruction at `0x1c72a0` with the shape of a radix-2
  butterfly (sum and difference of a register pair in one slot). Together that
  suggests an FFT/phase-vocoder or windowed-grain kernel -- plausibly STRETCH or
  WERP. **[O]**; a single butterfly-shaped instruction is not proof of an FFT.
  LFOs are separately established as ColdFire-side only.
- **Rejected hypothesis, recorded so it is not retried:** `FUN_1c71ec`'s tail
  contains six blocks of an identical push/`cjump`/pop pattern calling six
  well-separated targets (`0x1ccbd8`, `0x1cdecb`, `0x1cb3d8`, `0x1cd286`,
  `0x1cc79e`, `0x1cbf07`). Six calls and six sample machine types invites a
  dispatch reading, but the six execute **unconditionally in sequence** with no
  compare or branch between them -- a fixed six-stage pipeline, not a selector.

## The SHARC code is not one block **[C][V]**

Every SHARC search in this project ran over **blk93** alone, on the belief that
it was the whole program. It is not. The boot stream has 104 blocks, 58 carrying
payload, **319,352 bytes** in total; blk93 is 104,500 of them.

Classifying blocks by decode quality needs care: per-instruction confidence is
**not** a reliable code/data signal, because short encodings coincidentally match
random data (blk40 is data yet scores 98.5%). The reliable signal is **desync
frequency** -- real code resyncs once or twice per 8 KB, data every 25-45 bytes.

| blk | target | bytes | segs/KB | verdict |
| --- | --- | --- | --- | --- |
| **69** | `0x20000000` | **109,436** | 0.12 | **CODE -- larger than blk93** |
| 93 | `0x283827cc` | 104,500 | 0.12 | CODE (the one everyone searched) |
| 1 | `0x282403f0` | 10,312 | 0.25 | CODE |
| 88 | `0x28380548` | 8,484 | 0.12 | CODE |
| 56, 76, 78, 80, 91 | `0x282d7158`, `0x28380000`+ | 192-388 | 2.6-5.3 | small CODE fragments |
| 17,19,21,27,35,37,39,40,99,101 | various | 236-41,576 | 15-105 | data and tables |

**blk69 is not a driver overlay**, the inference that kept it unexamined. Two
lines of evidence. It does carry peripheral-register-shaped literals -- clusters
at `0x310c93xx`/`0x310ca3xx`/`0x31089xxx` on 4-byte register strides, plus float
constants (`0x3f800000` = 1.0f, `0x3f000000` = 0.5f). But it also shares
**byte-identical code** with the application blocks: 3,492 bytes with blk88
including an **823-byte contiguous identical run** (blk69 `0x20011256` <->
blk88 `0x28380608`), 10,221 bytes with blk93, 4,694 with blk1 including a
336-byte run. A pure driver overlay does not contain 800 contiguous bytes
identical to application code. It reads as a second, largely self-contained
program image -- driver/IOP setup plus shared runtime library -- in its own
`0x20000000` address space. Whether that is a second core's build or a
separately linked overlay is **[O]**.

Also corrected: blk93's payload starts at file offset **`0x31bcc`**, not
`0x31c3c` as earlier recorded -- a 112-byte discrepancy. Cross-validated against
`FUN_1c24e9`'s known address.

## The frame sweep, re-run over every block: still one region **[V]**

The literal sweep was re-run across all 58 payload-carrying blocks, scanning
only genuine literal-carrying fields (`data`, `addr` on types 14a/14d/15a/16a/
17a/18a/19a/19a_scaled/12a_imm) and explicitly excluding register selectors
(`ureg`), opcode bits (`compute`, `cond`) and branch displacements (`reladdr`)
-- an unfiltered pass produces false positives from all three.

**Zero hits for `[0x2558dc, 0x2560de]` outside blk93.** All 76 real hits are the
already-known ones in `FUN_1c24e9` and `FUN_1c2b24`. The machine-type cache read
at `0x255970` appears exactly twice, both in blk93. The `*0x60` stride idiom
exists nowhere else: literal `0x60` occurs in blk1, blk69 and blk88, but always
inside runs of consecutive small immediates loaded into successive registers,
never feeding a multiply.

So the negative now covers the whole image, not a third of it. Since no literal
anywhere points into the SRC page, the only remaining ways it could be read are
the two runtime-value threads already recorded above -- the stack-loaded base at
`0x1c33bc` and the unresolved `M5`/`M6`/`M7` in `FUN_1c24e9`. Those are no
longer side notes; they are the only candidates left.

## A confirmed writer to the transmit ring heads **[V][O]**

The strongest render-loop lead this project has had. A region at byte addresses
`0x2838eb06`-`0x2838f736` (function starting about `0x2838ec80`, i.e. `sw`
`~0x1c7640`, immediately adjacent to the `FUN_1c71ec` time-stretch kernel)
references **all six** ring addresses. Two of them are type `14a` with `d=1`:

```
0x2838f5cc   ureg=20, d=1, addr=0x264138
0x2838f710   ureg=20, d=1, addr=0x264170
```

`d=1` is a memory **write** (ADI SHARC Programming Reference Table 10-1, acronym
`D`: "0 = Memory read, 1 = Memory write";
`out/refs/adsp-2136x_2137x_214xx_pgr_rev2.4/all.txt`). `0x264138` and `0x264170`
are the two ring *head* addresses, `+0x2000` and `+0x1038` from the audio buffer
bases -- so these are control-block writes (write pointer or flag) rather than
audio data. The containing function was not traced. **[O]** whether it is the
render loop, but it is the first confirmed non-marker write to the ring
structures and it sits next to the one kernel we have tentatively identified.

Machine dispatch remains unfound. A stride-2 little-endian scan of the six
data/table blocks for values landing in the three code ranges produced three
isolated hits and no run of three consecutive same-region words -- no jump
table. The scan assumed absolute pointers, so a PC-relative table or one needing
a base add would not appear.

## M5, M6 and M7 are unresolvable statically **[V][O]**

The last route by which `FUN_1c24e9` could read the SRC page. It closes as a
strong negative rather than an answer.

A full writer-form scan (`17a`/`17b` immediates, `5a_move`/`5b_move`, and
`14a`/`15a`/`15b`/`3a`/`3b`/`3d` with `d=0`, which is the complete set of forms
that can write a ureg; M0-M15 are ureg codes 32-47, re-derived from
`tools/sharcspec/decode_table.json` rather than trusted) over all 58 payload
blocks, 65,595 confident instructions:

- **blk93: M4 written 93 times, M5/M6/M7 never.** That covers the block holding
  the documented entry point `sw 0x1c1338` and the whole graph reachable from it
  by resolvable calls. `FUN_1c2b24` has exactly one direct caller, `0x1c771e`,
  also in blk93; scanning its body and a 600-instruction window before the call
  finds no M5/M6/M7 writer either.
- **In genuine code across the whole image: one writer site**, in blk69, and it
  is a generic DAG-bank restore rather than a convention setter:

```
0x20010bce  m7 = pm(i11, +74)      ; 15a, d=0
0x20010bd4  m6 = pm(i11, +73)
0x20010bda  m5 = pm(i11, +72)
0x20010be0  m4 = pm(i11, +71)
```

surrounded by identically shaped restores of B4-B7 (`addr` 87-90), L4-L7
(107-110), then M0-M3, B0-B3, L0-L3. These are **loads, not immediates**, and the
pointer chain is runtime state: `i0 = dm(0x2ca020)` -> `i3 = dm(i0,+0)` ->
`i11 = i3`, where `0x2ca020` is referenced 76 times in blk69 across many register
classes (including a *store* of M5 at `0x2000b8ec`) -- a reused spill slot, not a
dedicated context pointer.

Three reasons this cannot be pushed further statically: blk69 is in its own
`0x20000000` space with **no established call edge** to or from blk93 (blk93 has
12 unresolved indirect calls, so a link cannot be excluded either); the table
base is runtime-computed; and SHARC+ DAG modifier registers have **no documented
reset value** (checked `out/refs/sharc-plus-prm/all.txt`; only status, loop and
interrupt registers have documented resets, and `tools/sharc_trace.py`'s
`CORE_UREG_RESET_VALUES` deliberately excludes M/I/L/B).

`M4`'s second value at `0x1c27a9` is likewise not a constant: `R2` there comes
from `r2 = r2 or fext r2 by 0x0:0x10` then `r2 = lshift r2 by -0x8` two
instructions earlier -- a bitfield extract, not the track index.

**CFADE: not provably read; treat as no.** Nothing puts M6 at the value 2 that
would be required, and even then the read would return the constant zero the
ColdFire always writes. The three branches gated on that word are not shown to
depend on it either way.

## The ring function is descriptor construction, not the render loop **[C][V]**

The `0x264138`/`0x264170` writes were a real lead and it resolves cleanly in the
wrong direction.

**[C]** The function's true bounds are `sw 0x1c75d8`-`0x1c7bd3`, not `0x1c7640`.
The preceding return is at `0x1c75d3` (`9b_abs`, `JUMP(M14,I12)(DB)` with `15b` +
`25c_rframe` delay slots) and the next at `0x1c7bcf`; `0x1c7640` is about 104
short-words inside the body.

It builds **all four** transmit descriptor rings in order -- A (`0x2620c8`, 256 B),
B (`0x262100`, 256 B), C (`0x264138`, 2048 B, audio), D (`0x264170`, 2048 B,
audio) -- each by the same setup, field-fill, submit pattern ending in a call to
`0x1ca7e4` with the head address in R8.

`ureg=20` is **I4** (read from `UREG_NAMES`' construction, not a comment). The two
flagged writes are the **NEXT_DESC_ADDR chain pointers** of two-descriptor
circular lists, sourced from compile-time immediates one instruction earlier:

```
0x1c7add  I4 = 0x262138
0x1c7ae0  DM(0x26413c) = I4      ; descriptor1.START_ADDR
0x1c7ae3  I4 = 0x264154
0x1c7ae6  DM(0x264138) = I4      ; descriptor1.NEXT_DESC_ADDR
0x1c7ae9  I4 = 0x262938
0x1c7aec  DM(0x264158) = I4      ; descriptor2.START_ADDR
0x1c7aef  I4 = 0x264138
0x1c7af2  DM(0x264154) = I4      ; descriptor2.NEXT -> closes the loop
0x1c7af6  R8 = 0x264138
0x1c7af9  call 0x1ca7e4          ; submit
```

Ring D is byte-identical in shape at `+0x38`. The already-recorded CFG word
`0x00100000` is written to `+0x08` in the same span.

**The sample-filling code is not here.** The body has exactly one hardware loop
(`0x1c7656`, `LCNTR=32`, four fixed-point instructions), no float
multiply-accumulate, no loop with a 512/1024/2048 trip count, and no access to
any audio buffer body -- only writes of their *addresses* into descriptor
headers. About twenty small callees were not opened, so that is bounded.

**[C]** The call to `FUN_1c2b24` is at **`0x1c771e`** (`25a_direct`, target
`0x1c2b24`), not `0x1c768c` as previously recorded -- that address holds an
unrelated `19a_scaled` push.

Two oddities worth recording. The function has **no static caller** anywhere in
blk93's 664 resolved calls, like `FUN_1c71ec` and `FUN_1c2b24` before it -- all
three are reached only through runtime paths, and blk93 has 12 unresolved
indirect calls (`sw 0x1c8530, 0x1c86b0, 0x1c8847, 0x1c9ae7, 0x1c9df7, 0x1c9f29,
0x1c9f8b, 0x1ca216, 0x1caf50, 0x1caf67, 0x1cb095, 0x1cb1b8`). And a **conditional
RTI** sits mid-body at `0x1c7641` (`Type11a`, `x=1`, `cond=NE`) while the
function returns by the ordinary call convention at `0x1c7bcf` -- unusual for
compiler-generated C, and circumstantial evidence this code is interrupt-adjacent
**[O]**.

No per-machine dispatch here: none of the 12 indirect-call sites falls inside the
body, and every conditional branch tests a shifter or ALU flag around lock and
retry sequences, never a small-integer compare or table load.

## Reading the DSP: the `FUN_1c71ec` pipeline is a wavetable engine, not an FFT **[C][D]**

First systematic *read* of SHARC code rather than a search over it. Seven
functions decoded in parallel, one agent each. Context for why this took so
long: blk93 holds about **410 functions** and until this pass we had read
**five**, roughly 1% of the SHARC code. Every earlier pass was a literal scan.

**[C] The FFT / phase-vocoder reading was wrong.** It rested on a single
butterfly-shaped instruction at `0x1c72a0` in the orchestrator. Six independent
reads of the stages now argue against it: **no dual add/subtract butterfly, no
bit-reversed addressing, no twiddle table and no log2(N) nested loop appears in
any of the six.** Stage 3 checked the dual-add/subtract encodings explicitly
(`mf=0,cu=00`, opcode top nibble `0x7`/`0xF`; `mf=1`, opcode6 `0x20`-`0x3F`) --
zero hits in 98 instructions. What the stages actually contain is
**phase-accumulator-driven, two-tap interpolated table lookup** -- a wavetable
or granular resampling engine.

Every stage's trip count is a **runtime register value**, never a literal. The
`1023` bound that supported the "1024-point FFT" reading is inside stage 5's
body, not the orchestrator.

| stage | addr | instrs | what it does |
| --- | --- | --- | --- |
| orchestrator | `0x1c71ec` | 251 | interpolated cosine lookup, inline butterfly, then the call chain |
| 1 | `0x1ccbd8` | 59 (leaf) | streaming **two-state linear recurrence**: `f1 = f11 + f0*f4` with `f11`,`f8` updated per sample, two state words persisted to the caller struct at `+6`/`+7`. Either a two-pole IIR or a coupled-form quadrature oscillator |
| 2 | `0x1cdecb` | 51 | **gated block copy**: tests a float against 0.0, then either a SIMD (PEYEN) stride-2 copy or, on the other path, `out = a*(1-frac) + b*frac` linear interpolation |
| 3 | `0x1cb3d8` | 98 | **on-the-fly polynomial envelope**: iterates `x <- x*(2- | x | )` three times to synthesise a saturating S-curve, then applies it multiplicatively along a ramp over the sample stream |
| 4 | `0x1cd286` | 148 | gathers **9 field-pairs** from the argument struct into two parallel arrays, then a MAC loop with a data-dependent count. Guarded by "if struct word 12 == 0, return". Float compute opcodes undecoded **[O]** |
| 5 | `0x1cc79e` | 173 | **table-interpolated resampler**: unsigned 32-bit phase to float (with the `+2^32` correction), a reciprocal helper for step size, then per sample `trunc` -> index, `index+1`, two `DM(I2,M)` taps, linear blend, `clip`. Table base `0x26bb68` |
| 6 | `0x1cbf07` | 133 | **two-tap interpolated lookup** with `2^32` and `8192.0` constants and an `0x1fff` mask -- a 13-bit wavetable index from a fixed-point phase accumulator. Persists index and position back to the caller struct |

Stages 5 and 6 are independently the same shape: fixed-point phase accumulator,
integer index plus fraction, two adjacent taps, linear blend. That is a
**wavetable oscillator**, twice. With stage 1's recurrence, stage 3's envelope
and stage 4's per-item accumulation over a runtime count, the whole reads as a
**bank of interpolating oscillators with per-partial state** -- additive or
granular resynthesis -- rather than a spectral transform.

**These are shared utilities, not machine-specific code.** Stage 3 is called
from **four** sites: `0x1c7387` (the orchestrator) plus `0x1c2307`, `0x1c231f`,
`0x1c6c59`, all unrelated. Stage 2 has a second caller at `0x1c6c44`. So the
engine is assembled from a common DSP library, which is why no per-machine
dispatch shows up as a branch -- the differences are likely in *which* routines
run and with what arguments, not in a switch.

**[C] The orchestrator makes nine calls, not six.** After the six there is a 7th
to `0x1ccd96` (argument `r12 = 0x8045c3c0`, the 32-float table), an 8th that
**re-calls stage 6** `0x1cbf07` with a different argument shape, and a 9th to
`0x1c4e70`. Its true bounds are `0x1c71ec`-`0x1c7461`, 251 instructions. A
conditional `rts` at `0x1c7292` means the chain is not even unconditionally
reached from entry.

**Dataflow.** Calls 1-3 pass a single value in R8/R12, with 2 and 3 re-reading
through an `(i3,m5)` cursor. Calls 4-6 pass **two**: a shared base pointer in
I3, constant across all three, plus a per-call offset built from a local and one
of **I14, I11, I10**. So stages 4, 5 and 6 work on one shared context at three
different offsets. The asymmetry between {1,2,3} and {4,5,6} suggests two
phases. Each call returns through a shared landing-pad trampoline at
`0x1c6eb7`/`0x1c6ef2`/`0x1c6f0a` -- a compiler code-size optimisation, not
pipeline structure.

**None of the seven reads the parameter frame.** All take pointers as arguments.
The orchestrator's own first four reads are off **I13**, a caller-supplied
context pointer, at offsets `-0x1a`/`+0x1d`/`-0x1b`/`+0x1c`. I13's provenance is
unresolved **[O]** -- it is the remaining link between the frame and the engine.

The orchestrator's address `0x1c71ec` does **not** appear as data anywhere: all
six firmware sections were scanned for the raw short-word `0x001c71ec`, the byte
form `0x2838e3d8`, and a bare 3-byte `1c 71 ec`, both endiannesses. Zero hits.
So it is not reached through a static function-pointer table **[V]**.

Tooling note worth acting on: `tools/sharc_trace.py`'s `_compute()` models only
the integer ALU, multiplier and shifter forms. It stops on the first float op in
every one of these functions. One agent extended a scratchpad copy using
`tools/sharcspec/compute_table.json`'s `aluop_32_40bit`/`mulop_32_40bit` tables
and got a full symbolic walk of stage 5. **Folding float compute into the real
tool would unblock every further read** -- stage 4's loop body is undecoded for
exactly this reason.

## A function inventory: 1228 functions, and we have read ten **[V]**

`tools/sharcinv.py` classifies every function in the SHARC code from a feature
vector -- float ALU / multiply / MAC counts, dual add/subtract, loop constructs
split by literal vs register trip count, memory accesses by space, decoded
IEEE-754 float immediates, and touches of the known named tables. It runs over
the whole image in about 1.4 s and reuses `sharcldr.py` for blocks,
`sharcflow.py` for calls and returns, `sharc_disasm.py` for decode and
`sharcspec/compute_table.json` for opcode semantics (PRM Tables 18-2, 18-5,
18-7, 18-9, 18-10). 19 synthetic tests in `tests/test_sharcinv.py`.

```
uv run python tools/sharcinv.py out/sections/dt2-1.16/section_7_BLOB.bin --top 25 --json OUT
```

**1228 functions** across the nine code blocks: **blk69 660**, **blk93 454**,
blk1 82, blk88 30, and single functions in blk76/blk78. blk56/80/91 are code
fragments too short to contain a return.

Labels: unclassified 674, block copy 229, glue/trampoline 128, envelope or gain
42, orchestrator 40, driver 37, DMA construction 34, IIR/recurrence 21,
wavetable lookup 9, FFT-like 8, interpolating oscillator 5, parameter converter

1. The 55% abstention rate is deliberate -- the heuristic only fires on shapes
confirmed by a hand-read, and the vector is emitted for every function
regardless, so a wrong label is visible rather than load-bearing.

Validated against the ten functions hand-read this session: **8 of 10 boundaries
exact, 9 of 10 labels in clean semantic agreement, 1 honest abstention
(`0x1cd286`), 0 contradictions.** It independently rediscovered `0x26bb68` as
stage 5's index table and labelled `0x1c75d8` "DMA/descriptor construction",
matching the hand-read wording. The one boundary disagreement (`0x1c71ec`, 235
vs 251 instructions) is explained: `0x1c7442`-`0x1c7462` is an independently
callable shared tail with two other call sites, so splitting it is defensible.

A method note the tool had to solve: a callee can sit physically inside another
routine's return-delimited span with no return before it -- `0x1c24e9` is inside
a larger span -- so every direct-call target landing strictly inside a span also
opens a function there.

### FFT code does exist, and it is not in this pipeline **[D]**

**25 dual add/subtract instructions across 13 functions.** Not the clean zero
that would have settled it. The densest:

| function | instrs | dual add/sub | label |
| --- | --- | --- | --- |
| blk69 `0xb8063e` | 602 | 4 | FFT-like |
| blk69 `0xb80c6c` | 52 | 4 | FFT-like |
| blk93 `0x1c5615` | 385 | 3 | FFT-like |
| blk93 `0x1c5ed4` | 128 | 3 | FFT-like |
| blk93 `0x1cb647` | 168 | 3 | FFT-like |

None is in the `FUN_1c71ec` chain, which stands as a wavetable engine. No
bit-reversed addressing (`19a_bitrev`) anywhere in the image, which is a second
FFT tell and argues these are something else -- possibly just the butterfly
*instruction* used for a cheap paired sum/difference. `blk69@0xb8063e` is the
one to read first. The classifier's dual-add/subtract rule has no
ground-truth-confirmed example yet, so treat the list as a strong lead.

### The reading shortlist

Ranked by compute density and table touches. The two standouts are
**`blk93@0x1c18a6`** (654 instructions, 162 float multiplies, 33 MACs, labelled
interpolating oscillator) and **`blk93@0x1c642a`** (1458 instructions -- the
largest in the image -- 33 callees, and it touches all three of the cosine
tables and the 32-float table). Also notable: `blk88@0x1c0d68` has **15 callers**,
unusually many, suggesting a core shared primitive.

## The 12 "indirect calls" are returns **[C][V]**

Recorded above as unresolved indirect calls that might reach the callerless
engine functions. They are not calls. All twelve decode identically to the
established return idiom -- `9b_abs`, `cond=31`, `j=1` (delayed; the PGR acronym
table at `adsp-2136x_2137x_214xx_pgr_rev2.4/all.txt:19101` gives J as "Jump
type, 0=Non delayed, 1=Delayed", **not** a call/jump selector -- Type9b has no
such bit), `pmi=4` -> I12, `pmm=5` -> M13 -- and for every one the nearest I12
write 6-17 short-words earlier is a memory load or register move restoring a
return address, never a literal:

```
0x1c8530: I12 = DM(I3,M5)    0x1c9ae7: I12 = DM(I2+4)     0x1caf67: I12 = DM(I6,M7)
0x1c86b0: I12 = DM(I2,M5)    0x1ca216: I12 = DM(I5+13)    0x1cb095: I12 = DM(I4,M5)
0x1caf50: I12 = DM(I5+13)    0x1cb1b8: I12 = DM(I3+13)
```

So this was never a dispatch table, and the callerless engine functions remain
callerless. Likewise **[C]** the `I13 = I5` at `sw 0x1c717c`, 224 bytes before
`FUN_1c71ec`, is not its call setup: `0x1c71ad`-`0x1c71eb` is the *epilogue* of
the preceding function -- a bank restore of ~28 registers followed by the same
return idiom at `0x1c71e7`. `FUN_1c71ec` merely sits next in memory.

## I13 is never loaded from a literal **[V][O]**

All 58 payload blocks scanned, 65,595 confident instructions: **44 writers of
I13**, in 7 blocks (blk93 17, blk69 11, blk88 7, blk40 4, blk1 2, blk37 2,
blk27 1). **Not one is a `17a`/`17b` immediate.** Every write is a
register-to-register move or a small-offset DM load off I3/I4/I5/I6/I7/I12/I15.

That is itself the finding: I13's value is always runtime data, consistent with
a genuine per-call context pointer, and static tracing bottoms out in a chain of
register moves rather than a literal. Four of blk88's writes copy **MODE1** (ureg
114) through I13 as scratch, so not every write is even a pointer.

The task-creation sequence at `sw 0x1c7749`-`0x1c7793` was read: it calls
`0xb8615d` with `R12=1000`, `R8=0x25f7c0`, `R4=0x1c7749` -- registering its own
entry address. **It does not set I13**, and no I13 write occurs anywhere in that
function's body. If the RTOS deposits I13 as part of a per-task register-bank
restore before dispatch, that is the same kernel-side mechanism already recorded
as statically unreachable for M5/M6/M7.

Next step for this thread, if pursued: a scripted fixed-point **backward slice**
over the I3/I4/I5/I6/I7/I12 chains feeding the 44 writes. By hand it explodes --
I6 alone has 474 writes in blk93.

## `tools/sharc_trace.py` now models float compute **[V]**

The gap that forced seven functions to be hand-decoded this session. +544 lines:
IEEE-754 single-precision helpers with the NaN "all ones" quirk and overflow
detection; 13 float ALU branches (add, subtract, negate, abs, pass, compare,
min, max, clip, float-convert, trunc, and the recips/rsqrts seeds); float
multiply; both **MULALU** multifunction forms; **dual add/subtract** for fixed
and float; and the five previously missing ShortCompute opcodes, which completes
that opcode space. Dual-result ops required a new tuple path in
`_apply_compute()` so both registers are written from one combined ASTATX
update -- with tests proving the OR-ing of flags across the two halves, which a
naive implementation gets wrong.

Citations are in the code: PRM Tables 18-2/18-5/18-7/18-10/18-13/18-16/18-17 and
the PGR functional definitions at 11-24 through 11-57.

Tests: **472 -> 521 passed**, 5 skipped, 172 subtests, no regressions, ~40 new
test methods.

Effect, measured:

| function | before | after |
| --- | --- | --- |
| stage 4 loop `0x1cd33c` | 1 step, `unsupported cu=0x0 opcode=0x81` | **49 steps, clean return** -- the full 32-instruction body |
| stage 5 `0x1cc79e` | 22 steps | 85-101 steps |
| stage 3 `0x1cb3d8` | 14 steps | 43-51 steps |
| stage 6 `0x1cbf07` | 28 steps | 57-78 steps |
| stage 1 `0x1ccbd8` | 20 steps | 29-32 steps |
| orchestrator `0x1c71ec` | 5 steps | 16 steps |

Remaining stops are honest ones: `nonconcrete Type12a UREG loop count` and
`return without followed call` are correct end-of-static-walk conditions. Two
unrelated gaps remain -- `unsupported Type3a predicate`, and fixed-point
`RN=min(RX,RY)` (`cu=0x0 opcode=0x61`).

**This resolves stage 4's `[O]`**: its loop body contains a dual add/subtract
and two MULALU forms. Not a contradiction of stage 3's zero-butterfly result --
different functions.

Not modelled, and reported rather than guessed: 40-bit extended float (the
register file holds 32 bits per ureg), denormal flush-to-zero, the AI flag where
the manual gives no formula, RECIPS/RSQRTS values (the seed comes from an
undocumented ROM table, so they decode but return Unknown), and the 3-result MUL
Dual Add/Subtract form, which still raises a clear error.

## The per-frame render chain, and a RAM-backed dispatch candidate **[V][O]**

Reading the top of the inventory shortlist located the structure the whole
project has been circling.

**`FUN_1c2b24` is the per-frame render orchestrator, not just the frame reader.**
After its documented 32 per-track calls to `0x1c24e9` (parameter conversion), it
calls the image's heaviest compute functions in sequence:

```
0x1c307a  R12 = 0x00252d3c          ; shared context pointer
0x1c3083  call 0x1c642a             ; 1458 instrs -- the largest function in the image
0x1c308d  R4 = caller frame[-17], R8 = frame[-18]
0x1c3090  call 0x1c18a6             ; 654 instrs
0x1c3097  call 0x1c207b
          call 0x1c14e7
```

`R12 = 0x252d3c` is passed through as a shared context pointer across several of
these calls, and `0x1c18a6` reuses the same constant for its own three tail
calls into the L2 overlay. So there is one per-frame context object threaded
through the render chain.

### `0x1c642a` -- the largest function, one caller, 33 callees **[V]**

Confirmed a single function: exactly one return in its span, and no call in the
image targets an address strictly inside it. It ends at `0x1c71e7`, with its
delay slots finishing at `0x1c71ec` -- which is `FUN_1c71ec`'s entry. **That is
code adjacency, not a relationship**: `FUN_1c71ec` is not among its callees and
its return goes to `0x1c308a` in its caller. Easy to misread; recorded so nobody
does.

51 call sites over 33 distinct targets, 30 in blk93, three in the L2 overlay
(`0xb80105`, `0xb88f06`, `0xb88f70`), one in blk88 (`0x1c0d68`). Its tail --
after the last call -- is five literal-count loops (32, 32, 32, 16, 16) each
immediately followed by a register-counted loop, and that tail is where it
touches **both halves of the cosine pair** (`0x8055c440` at `0x1c70a5`/`0x1c70b3`,
`0x8055c640` at `0x1c70b6`/`0x1c70b9`), the 32-float table (`0x8045c3c0` at
`0x1c6c22`), and three point-reads into the exponential tail at `0x8055c840`,
`0x8055c858`, `0x8055c874`.

It does **not** touch the 1024-float pair, the 829-float table, stage 5's table,
or any audio ring -- so the biggest function is not the ring writer either.

### The dispatch candidate **[O]**

Three sites in `0x1c642a` use an indirect jump that is **structurally different
from the return idiom**: `pmi=4` -> I12 as usual, but `pmm=5` -> **M13**, and
`j=0` (non-delayed) -- `JUMP(M13,I12)`, at `0x1c6579`, `0x1c66ec`, `0x1c6c25`.
Every confirmed return in this image uses **M14** and is delayed. `sharc_trace.py`
independently stops at the first of these with "unknown `9b_abs` indirect target
through I12/M13", treating it as a genuinely unresolved indirect jump.

Crucially, I12 is **not restored from a frame slot** at these sites -- it is
freshly loaded from memory:

```
I4 <- I14                  ; I14 set once in the caller FUN_1c2b24 at 0x1c2ce0
                           ; to the literal 0x254d98
I12 = DM(I4,M4)            ; 3b load -- a function pointer out of memory
JUMP(M13,I12)              ; non-delayed indirect jump
```

`tools/sharcldr.py --addr 0x254d98 --addr-space byte` reports that address as
**not covered by any loaded block** -- it is RAM, not loader-initialised ROM.
So this reads a **runtime-written function pointer**. The other two sites take
their base from I9, reloaded from a stack slot, consistent with the same object
having three dispatched slots.

**This is the first genuine computed-call shape found in the image**, and it is
the natural explanation for why no per-machine branch has ever turned up: the
selection would be a pointer written into RAM, not a compare. It is **[O]** --
nobody has found the writer of `0x254d98`, and nothing yet ties its contents to
the machine type at frame `0x94`. Finding that writer is the next step, and it
is a bounded search.

Note the contrast with the 12 sites debunked above: those were `M14`, delayed,
with I12 restored from a frame slot. These three are `M13`, non-delayed, with
I12 loaded from RAM. The distinction is what makes them credible.

### `0x1c18a6` -- a 128-entry interpolated coefficient table **[V]**

654 instructions, exact match to the inventory. The same interpolation idiom as
the wavetable stages but against a **previously uncatalogued table pair**:

```
R7 = 0x43000000 (128.0)   ; index scale
F8 = F2 * F7
R11 = 0x42fe0000 (127.0)  ; clamp
F9 = min(F0, 127.0)
R0 = trunc F9 ; R12 = R0+1
M2 = R8 ; M4 = R2
R8  = DM(I2 post-mod M2)   ; I2 = 0x256588
R14 = DM(I1 post-mod M2)   ; I1 = 0x256388
R13 = DM(I2 post-mod M4)
R6  = DM(I1 post-mod M4)
```

Two contiguous 128-float tables (`0x256588 - 0x256388 = 0x200`), two adjacent
taps from each, blended by the fraction. A 128-entry table indexed by a scaled
parameter is the shape of a **note/pitch to coefficient conversion** (128 = the
MIDI note range) **[D]**.

Then six more loops (31, 31, 16, 31, 30, 31) of float MAC traffic, one of them
bracketed by `SET MODE1,0x200000` / `CLEAR MODE1,0x200000` -- **SIMD mode**, so
16 iterations is 32 scalar. Inside it, six words from `DM(0x254d80..0x254da0)`
are loaded into `I3,I12,M1-M4`, and the loop then gathers one element from each
of **four separately-based arrays** per index -- the shape of a polyphase or
windowed-sinc kernel gathering coefficient and sample streams **[D]**.

It touches **no** known named table and no audio ring, and does not read the
parameter frame. Its three tail calls hand off to the L2 overlay with external
pointers `0x80459388`, `0x8005912c`, `0x80000018`.

## The FFT question is settled: there is no FFT **[V]**

`blk69@0xb8063e`, the strongest candidate (602 instructions, 4 dual
add/subtracts), was read in full. **blk69's address convention differs** -- its
target is `0x20000000` = the L2 byte base, so `base_sw = 0xb80000` and the
`byte = 0x28000000 | 2*sw` alias used for blk93/1/88 does **not** apply.

The checklist, item by item:

| test | result |
| --- | --- |
| nested log2(N) x N/2 loops | **absent** -- 17 loops, all flat, **zero literal-count**; 15 contain no butterfly at all, and the 2 that do are both counted by the same external register M7 |
| twiddle table or on-the-fly twiddles | **absent** -- zero `0x80xxxxxx` literals anywhere in the function |
| bit-reversed addressing | **absent** -- and none in the image |
| power-of-two transform size | **absent** -- no 64/128/256/512/1024 literal |
| in-place vs ping-pong | single indexed traversal, no alternating buffers |

The four butterflies sit in two near-identical blocks, each **five float
multiplies feeding a chain of two dual add/subtracts** plus single adds and
subtracts -- a 5-product linear-combination network, not a 2-input radix-2
butterfly repeated across stages. Its caller `blk69@0xb80fd0` calls seven
distinct routines once each in sequence (an audio pipeline), not one butterfly
repeatedly (an FFT driver), and one of its own callees is independently labelled
IIR/recurrence.

The shape check on the other four high-count functions kills the idea entirely:

- `blk93@0x1c5ed4` (128 instrs, **0 loops**) carries the float immediates
  **pi (3.14159274)**, **pi/8**, and **96000.0** -- a filter-coefficient
  calculator of the classic `2*pi*f/fs` form. **The sample rate is 96 kHz.**
- `blk93@0x1cb647` has one loop of 32 with the immediate `1/32 = 0.03125` -- a
  fixed 32-sample block average.
- `blk93@0x1c5615` has literal loop counts `[32, 15, 32]` -- 15 breaks any
  power-of-two staging.
- `blk69@0xb80c6c` has **zero loops** -- it cannot be a transform stage.

So the classifier's dual-add/subtract rule catches a real and recurring idiom --
paired sum and difference for coefficient combines, block averaging and
one-shot rotations -- that **never co-occurs with any other FFT tell**. The
label should be renamed; "FFT-like" is misleading.

Tracer gap found while doing this: `_compute()` still lacks the ALU opcodes
`mant` (`0x0`) and `scalb` (`0xbd`), both documented in PRM Table 18-5. They
stall a trace of `0xb8063e` at step ~45.

## The call graph was incomplete: Type 8a CALL was never decoded **[C][V]**

The most consequential finding of this pass, and it invalidates a recurring
claim. `tools/sharcflow.py` (and `tools/sharcinv.py`, which builds on it)
recognise only **Type 25a** `CJUMP` and **Type 9b** indirect calls. They do not
decode **Type 8a with `b=1`**, which is an ordinary PC-relative `CALL` (SHARC+
PRM p.357).

Decoding all Type 8a calls in blk93 finds **51 real call sites that every
previous pass missed**. So **every "no static caller" claim in this document is
unsafe** -- including the ones about `FUN_1c71ec`, `FUN_1c2b24`, the
ring-construction function and `0x1cb4b2`. They may well have had callers all
along. The 12-indirect-calls-are-returns correction still stands (that was about
Type 9b), but "callerless, therefore reached at runtime only" does not.

50 of those 51 target one address: **`0x1c06ba`**, a heavily shared primitive
reached from all over the engine, including from stage 3's envelope routine
`0x1cb3d8`. It is **RECIPS followed by three Newton-Raphson iterations** -- the
reciprocal idiom the SHARC+ PRM documents verbatim. The inventory's guess of
"IIR or recurrence" for it is wrong.

Two further tracer gaps surfaced, neither previously known: multifunction
categories **`0x1e`/`0x1f`** (MUL+MAX and MUL+MIN, PGR Table 12-12 p.588) are
not modelled -- only `0x18`/`0x19` (MUL+ADD/SUB) are -- and they stop every
symbolic path through `0x1c207b`.

## [C] The dispatch lead: right shape, wrong table

The `JUMP(M13,I12)` sites in `0x1c642a` are real and the form is genuinely an
indirect jump. But the instruction **order** recorded above is backwards, and
that changes the conclusion. Re-read from the bytes:

```
0x1c6569  17a      I4  = 0x8055c840        ; a literal -- NOT I14
0x1c656c  3b       I12 = DM(I4 + M4*4)     ; the jump target is fetched HERE
0x1c656e  5b_move  I4  = I14 (= 0x254d98)  ; only now, AFTER the fetch
0x1c6579  9b_abs   JUMP(M13,I12)
```

All three sites follow this shape, with base literals `0x8055c840`,
`0x8055c858`, `0x8055c874` -- **exactly** the three "point-reads into the
exponential tail" already recorded independently for this function, which is a
strong cross-check that this is the real fetch.

So `0x254d98` is **an argument passed to the callee**, not the source of the
jump target. The actual fetch base sits in the `0x8055c8xx` float-table region.
`M4` is copied from `R6`, an incoming parameter of `0x1c642a` itself, which is
why the target cannot be resolved -- not because I14 was unresolved (it is
concrete).

**Machine-type dependence is not established, and two traced paths argue against
it.** The four writes to `0x254d78`/`80`/`88`/`90` happen in `FUN_1c2b24` at
`~0x1c2c5e`, **1800+ instructions before** the machine-type read at
`0x1c33c1`, with no dataflow between them. The earlier trace reported three accesses to `0x254d98`/`9c`/`a0` in
`0x1c18a6`'s prologue on an unconditional single path (0 branches in 400
traced steps); the bounded direction for `0x254d9c`/`98` is corrected below.

The one untraced hop that could still connect it: `R4` at `0x1c1928` is an
incoming argument, and `FUN_1c2b24` passes `R4 = caller frame[-17]`. Where that
frame slot comes from is **[O]**.

The region resolves as **one 11-word object**, `0x254d78`-`0x254da0`, whose
middle six words `0x1c18a6` loads into `I3,I12,M1-M4`. Writers found only in
blk93. The scan saw only direct-literal address operands, so a store through a
computed base would be invisible.

### [C] Bounded cross-image forwarding at `0x1c18a6` **[V][D][O]**

The earlier incoming-argument wording for both stores is superseded within the
bounded straight-line interval. In both DT2 1.15C and DT2 1.16, the identical
raw instructions are:

- `0x1c18ed`, `100200252658`: Type14a `R2 = DM(0x252658)`.
- `0x1c18f3`, `100400254d9c`: Type14a `R4 = DM(0x254d9c)`.
- `0x1c191d`, `110200254d9c`: Type14a `DM(0x254d9c) = R2`.
- `0x1c1928`, `110400254d98`: Type14a `DM(0x254d98) = R4`.

Exhaustive canonical section-replay decoding and independent raw review find no
intervening `R2`/`R4` destination or control transfer. The exact byte-proven
assignments are `DM(0x254d9c) <- DM(0x252658)` and
`DM(0x254d98) <- old DM(0x254d9c)`. **[V]** The Ghidra proof remains scoped to
the R4 forwarding path, where address-space-aware exact matching and raw p-code
agree on `ram 0x4a9b38 -> value -> ram 0x4a9b30`; the R2 store slice returned no
seeds. **[V]** This is consistent with a two-word state/history shift, but
semantic direction, ownership, natural execution, and relation to machine type
are not proven. **[D][O]**

The mapping audit SHA-256 is
`24441e37f2933e1a8c561d5182b98f59812549e70df0b1251057f4b09e9f10d9`; the DT2
1.15C `0x252658` census artifact SHA-256 is
`1bbb5eaed7ec762d4a3692e851ae8b319e212b9f37ad8134c90c76f07b259b91`; and the
DT2 1.16 summary SHA-256 is
`ba1e830b04c8b79e283ff4305990d6bbf20d24b049297a2b52f97167400305ca`.
Each exhaustive even-offset canonical Type14a-g=0 census has exactly one direct
literal access to `0x252658`, the R2 reader at `0x1c18ed`, and no direct
Type14a writer. Computed/indexed/dual-memory stores, loader data, and
ColdFire/host writes remain possible. **[V]**

DN2 1.11 canonical scan proves exact encodings `0x1c19d7: R4=DM(0x25d010)` and
`0x1c1aaf: DM(0x25d00c)=R4`, the same -4 source/destination address delta as
DT2; scan SHA-256 is
`fe05c06f323b41595e155d7238b92444c784e22e56ce04ed7a36420584cb5cb7`. **[V]**
The containing DN2 block is `blk83@0x1c1738`, bounds `0x1c1738..0x1c1e1f`, 659
instructions; its vector is close to DT2 (3 calls, 204 loads/204 stores, 33
MACs, one nested loop; DT2 654 instructions). This is structural analogy only:
HighFunction at the correct PCs has a semantic-location mismatch and cannot
prove that the same runtime value flows between them. The earlier zero
`0x254dxx` DN result is an exhaustive direct-Type14a result under that
form/range, not architectural absence; DN uses shifted RAM addresses in the
candidate. **[D][O]**

One documented static call is at `0x1c3090` from `blk93@0x1c2b24`, immediately
after R4/R8 frame setup; it is unconditional only within an already reached
caller. It neither establishes natural execution nor excludes indirect callers.
**[D][O]**

**[C]** The direct local writer of `0x254d9c` is now known; the unresolved
source/owner is `0x252658`, especially computed/indexed or host writers and its
relation to machine type. I6+124 and external-table readers remain open. The
next automated seed is computed-writer/ownership analysis for `0x252658`, then
table-reader control flow. **[O]**

### [C] `0x252658` is zero-filled at boot and has no SHARC writer **[V][C][O]**

**[C]** The "R2 store slice returned no seeds" limitation recorded above was a
defect in `tools/ghidraq.py`, not a decompiler limit. `memory_access_operands`
classified a HighFunction `COPY` as a store only when the destination was
direct memory and the source was not, and as a load only the mirror way, so a
`COPY` with direct memory on *both* ends matched neither branch and was
dropped. Ghidra folds `R2 = DM(0x252658); DM(0x254d9c) = R2` into exactly that
form. The decompiled C of `FUN_001c18a6` in `sharc-batch-dt2-116` always
contained both `_DAT_00254d98 = _DAT_00254d9c;` and
`_DAT_00254d9c = _DAT_00252658;`; only the query layer could not see the
second. With the classifier fixed (`COPY_MEM_TO_MEM_WRITE` and
`COPY_MEM_TO_MEM_READ`), an unbounded whole-image `stores sw:0x254d9c` reports
the store at `sw 0x1c191d` (p-code `0x38323a` = 2 x `0x1c191d`) as
`exact-constant-target`, with `address_slice` root `ram 0x4a9b38` and
`value_slice` root `ram 0x4a4cb0`. Ghidra dataflow now proves the R2 path
`DM(0x254d9c) <- DM(0x252658)` independently of the byte-level replay, which it
previously could not. **[V]**

All three chain addresses lie inside one zero-FILL loader block, in every
image. DT2 1.15C and 1.16 share a byte-identical header at file offset
`0x2bc0`, `01 01 ea ad c0 12 24 28 c8 50 01 00 00 00 00 00`: block 18,
`block_code 0xadea0101` with BFLAG_FILL set, `target_address 0x282412c0`,
`byte_count 86216`, `argument 0x00000000`, covering `0x282412c0..0x28256388`.
DN2 1.11 block 16, at file offset `0x2b70`, has the same shape:
`target_address 0x28241290`, `byte_count 114728`, `argument 0`, covering
`0x28241290..0x2825d2b8`. A second agent re-parsed every block of all three
files with an independent struct walk and confirmed that no later block
re-covers any of these addresses, so last-write-wins leaves the fill standing.
The loaded value at `0x252658`, `0x254d98` and `0x254d9c`, and at DN2
`0x25d00c` and `0x25d010`, is 32-bit zero at boot. **[V]**

For this program's data blocks the loader byte address is the Ghidra displayed
address plus `0x28000000`: Ghidra places loader block 1, `target_address
0x282403f0`, at displayed `0x2403f0`. Probing `sharcldr` with the bare
displayed address, or with `sw_to_byte` applied to it, both miss, and two
separate reviews in this batch made exactly those two mistakes. In the same
program `read` and `range` take the displayed word address while `xrefs` and
the symbol table take the doubled p-code byte offset, so a plain-address
`xrefs` silently returns nothing. **[C]**

**[C]** `tools/sharcldr.py --addr` previously printed "not covered by any
loaded block" for an address inside a FILL block, because `offset_for_address`
reports *file* offsets and a FILL block contributes no file bytes. That message
produced a wrong "the loader never touches this address" conclusion in this
batch. `fill_block_for_address` now backs a separate report naming the fill
block, its constant and its range. **[V]**

**[O]** With the fixed classifier, an unbounded whole-image `stores
sw:0x252658` over DT2 1.16 returns no genuine writer, in either
`sharc-batch-dt2-116` (564 bounded functions) or `elektron-sharc` (1999). The
single match in each is a self-copy `DM(0x252658) = DM(0x252658)` attached to
an `INDIRECT` call-effect node, with no raw instruction at its p-code address
(`pcode 0x383640` returns `no-instruction`); it is a decompiler
value-preservation placeholder, not a store. Ghidra's reference table likewise
holds exactly one reference to the address, the read at `0x3831da`. The value
is therefore zero at boot and no *direct* SHARC store changes it.
`elektron-sharc`'s decompilation of this function is truncated by
`halt_baddata()`, so `sharc-batch-dt2-116` is the better witness here despite
its smaller function count. **[O]**

**[C]** That sweep could not see indexed stores at all, so it does not bound
the owner to non-SHARC writers. The generated language emits an empty
semantic body for every form without hand-written semantics, and a pypcode
lift of all 22,668 aligned DT2 1.16 main-program instructions finds 0 p-code
ops for, among others, `15b` (4528), `3c` (1488), `4a` (1024), `3a` (983) and
all but 5 of 1349 `3b` -- the indexed DM load/store forms. An instruction with
no p-code contributes no `STORE` to HighFunction, so no query built on it can
report such a store. The sweep agrees: across five whole-image queries it
returned only `exact-constant-target` matches and not one
`computed-or-unresolved` candidate, in an image with thousands of indexed
memory instructions. The owner therefore remains an indexed SHARC store, a
ColdFire/host write, a DMA path, or code outside Ghidra's function
boundaries. **[O]**

### A zero-initialiser also writes `0x254d9c`, and CBUFEN is set at startup **[V][D][O]**

**[C]** `0x254d9c` has a second writer, an indexed store that the Ghidra
sweep could not see. `blk93@0x1c15e3`, called only from the direct call at
`sw 0x1c8092` in `blk93@0x1c7ff9`, loads `I0 = 0x255118`, `I1 = 0x254d98`
and `I2 = 0x255498` by literal at `sw 0x1c1653..0x1c1659`, then runs nine
Type16b stores `DM(Ix, M6) = 0` at `sw 0x1c16a7..0x1c16b7`, rotating over
I1, I0 and I2. Type16b is post-modify only (PRM p.16-20). With M6 = 1 and
normal words scaled by 4, it zeroes `0x254d98`, `0x254d9c`, `0x254da0`,
`0x255118..0x255120` and `0x255498..0x2554a0`; the store at `sw 0x1c16ad`
writes 0 to `0x254d9c`. It does not touch `0x252658`. A second agent decoded
the range from the image bytes and confirmed every address. The x4 scaling
of Type16b is inferred from its siblings 3a, 4a and 15b, which were checked
live; Type16b itself has no language semantics yet. **[V][D]**

M6 is a runtime constant. The startup routine `blk88@0x1c0f24` sets
`M5 = 0`, `M6 = 1`, `M7 = -1`, `M13 = 0`, `M14 = 1` and `M15 = -1`
(`M6 = 1` at `sw 0x1c0f40`, bytes `a6 0f 01 00`). Across all nine code
blocks the only other writer of each is a PM load in blk69 paired with a
PM store of the same register at the same offset, an interrupt save and
restore. The same routine sets `B7 = 0x26f000`, `I7 = 0x26f7f0` and
`L7 = 0x1fd` at `sw 0x1c0f64..0x1c0f6a`, mirrors them into B6, I6 and L6,
and sets MODE1.CBUFEN (bit 24) at `sw 0x1c0f82`, bytes
`14 02 01 01 18 00`, `MODE1 = set(MODE1, 0x1011800)`. No BIT CLR anywhere
in the image touches bit 24, and MODE1 itself is never loaded from an
immediate or from memory. So the stack is a circular buffer
`[0x26f000, 0x26f7f4)`: post-modify accesses through I7 and I6 wrap inside
it (PRM p.6-25), and MODIFY wraps whenever L != 0 regardless of CBUFEN
(PRM p.6-7). This routine is the one the earlier startup trace reaches
through `0x1c0f26`. 22 register-to-register moves into MODE1 or MODE1STK
from runtime values are not statically resolved. `tools/sharc_trace.py`
wraps only MODIFY; its ordinary post-modify accesses are always linear.
**[V][O]**

**[O]** `tools/sharcwriters.py` censuses every DM store in the image, 12,634
across 15 forms, and resolves each from its function entry with
`tools/sharc_trace.py`, seeding the six runtime-constant M registers and L6
and L7. For `0x252658` none resolves to the target: 496 resolve elsewhere,
3,441 are stack-relative, 224 depend on the caller's I or M registers, 35
go through a loaded pointer, and 8,438 stay unresolved. The largest
unresolved causes are the stack pointer's circular MODIFY while B7 is not
concrete (1,045 with its cascade), undecoded or unconfirmed forms (627),
Type11a and compute opcode `0xa5` (391), and budgets (595). For `0x254d9c`
the same run finds both writers, at `sw 0x1c191d` and `sw 0x1c16ad`.
**[D][O]**

### A bounded set contains I6/I7/B6/B7, but only conditionally -- circular-MODIFY fixed with fresh symbols, not reuse **[D][C][O]**

None of this section has been checked by a second agent against the image
bytes yet, so it is marked **[D]** throughout, not **[V]**, except the one
line explicitly reused from an earlier, already-**[V]** finding.

**[D]** A whole-image, function-owned census (7764 instructions across
`CODE_BLOCKS`, same recovery scope as `tools/sharcwriters.py`'s own) of
every instruction that can write I6, I7, B6 or B7. Full writer table and
proof: `out/sharcwriters/stack-invariant.md`/`.json` (not committed --
built from the firmware, see CLAUDE.md). Two decode bugs in the
(throwaway, uncommitted) census scanner were caught mid-pass by
cross-checking against `tools/sharcfn.py`'s own renderer: `merge_fields`
already combines a form's split sub-fields (`srcureglow[1:1]`+
`srcureglow[0:0]`, `is[2:2]`+`is[1:0]`) into one key, so re-splitting them
from the now-absent raw sub-keys always read 0 -- caught on the
already-**[V]** boot mirror `B6=B7` decoding as `src=68` (B4) instead of
71 (B7).

**[C][D]** The task brief's working hypothesis that "blk93 recomputes B7
from I7 at runtime" (sw `0x1c1463`, form 7d) is corrected by this census:
every one of the 28 ACONV instructions in the image is `same_reg` (PRM
Table 14-22: ACONV's source and destination are the same register, B2W
then W2B or the reverse), confined to 3 functions (`blk88@0x1c0b1d`, four
`blk69` functions, and `blk93@0x1c13e6`). None of them recomputes B7 from
I7; they transiently reinterpret I6/I7/B6/B7 as word addresses (dividing
by 4) and convert back. This does not threaten S: `tools/sharc_trace.py`'s
Type7d handler already stops the trace outright when the source register
is not concrete, and I6/I7/B6/B7 are never concretely seeded when tracing
from a function's own entry, so every path through one of these 28
instructions halts rather than continuing with a wrong value -- consistent
with the Task 3 rerun below, where "Type7d B2W(B7) source is not concrete"
is the 5th-largest `UNRESOLVED` reason (366 stores).

**[D]** SHARC+ Core Programming Reference Table 16-1 (`out/refs/sharc-plus-
prm/all.txt` ~line 21951): `CJUMP label (DB); JUMP label (DB), R2=I6,
I6=I7;` -- every call via Type25a implicitly sets I6 to the caller's
current I7 (1718 sites). Lines 22099/22161/22176: `RFRAME; I7=I6,
I6=DM(0,I6);` -- already implemented by `tools/sharc_trace.py` (1109
sites). Neither needed a code change; CJUMP's side effect is not even
implemented by the tracer (a real gap, out of scope here). The remaining
4008 post-modify pushes/pops through I6/I7 all use the already-proven-
constant M7 (`-1`); the 821 circular MODIFYs are the one category this
pass changed (below).

**[O]** **Item 2, closing the invariant -- not fully closed.** Every
`EXCLUDED-STACK` classification assumes I6/I7 are within S (or, after this
pass, within S widened by one circular-MODIFY buffer length) at the entry
of the function that owns the store -- since `tools/sharcwriters.py` seeds
each function fresh from its own entry, this is a claim about *every*
control-flow path that can reach a function, not just the "normal" one.
Walked the three ureg moves and the interrupt-entry/exit reload chain the
earlier pass in this section had left open, rather than just re-flagging
them:

- **`blk88@0x1c075e`, `I6 = I4` (sw `0x1c0784`, `0x1c07e1`) -- closed,
  safe.** `I12 = DM(I6, M7) u=0` at sw `0x1c0774` reads this function's own
  return address off I6 (still the real frame pointer at that point)
  *before* either repurposing; I4 is untouched up to sw `0x1c0784`, so I6
  becomes `I4e`, a symbol `tools/sharcwriters.py` does not recognise as
  stack-bounded -- any later store through it lands `ENTRY-RELATIVE`
  (dependent on I4), never wrongly `EXCLUDED-STACK`. The function returns
  through a plain indirect `RETURN` (sw `0x1c0865`, I12/M14, not RFRAME)
  and makes no further calls visible in its listing, so a clobbered I6/I7
  cannot propagate to a callee or an RFRAME. This one writer does not
  threaten S.
- **`blk69@0xb8853a`, `I7 = I11` (sw `0xb885f5`) -- open, and this is the
  real finding.** I11 is a copy of I3 (this function's own sw `0xb885bd`,
  or the caller's save `I11 = I3` at `blk69@0xb88200` sw `0xb88228`), and
  I3 = `DM(I0+0)` where I0 = `DM(0x2ca3e0)` (`blk69@0xb88200` sw
  `0xb88200`/`0xb88203`) -- a global "current context" pointer,
  dereferenced. So `I7 = I11` sets the *real* stack-pointer register to a
  context-struct address, and the very next instruction (sw `0xb885f7`,
  `I7 = modify(I7, 0x81)`, a plain +0x204 linear step, L7 not yet
  reloaded at this point so not circular) computes a fixed offset off it.
  This is the exact shape of a genuine multi-context stack switch: I7
  repointed to a *different* per-context stack area computed from a
  runtime-populated global, not the boot-constant `[0x26f000, 0x2c0000)`
  region. The image gives no static value for `0x2ca3e0` (runtime-
  populated), so this cannot be resolved statically to either "still
  within S" or "a specific other region" -- **not closed**, and **not**
  safe to assume away.
- **The 24 fixed-offset reloads.** B6/B7's own `PM(0x59)`/`PM(0x5a)`
  save/restore *is* a clean, verified round-trip (`blk69@0xb88200` sw
  `0xb88353`/`0xb88356` stores the just-loaded B6/B7 there; `blk69@0xb8853a`
  sw `0xb885cf`/`0xb885d2` restores the identical bytes) -- that specific
  hop is closed. But the values it round-trips were themselves loaded from
  `DM(I7+2)`/`DM(I7+4)` (`blk69@0xb88200` sw `0xb88239`/`0xb88253`, and
  `I6 = DM(I7+3)` at sw `0xb8823b`) -- offsets of the *same* I7 the item
  above shows may already be a per-context pointer, not the boot-constant
  stack. So this reload's soundness is conditional on the same open
  question, not independent of it. The other ~18 reloads across `blk1`/
  `blk88`/other `blk69` functions were not individually walked in this
  pass.

**Verdict: conditional, not closed.** S = `[0x26f000, 0x2c0000)` was
**not** widened -- there is no static evidence for a specific numeric
range to widen it to (the context-struct address is a runtime value) --
and it was **not** proven closed either. `tools/sharcwriters.py` now
records this explicitly rather than hiding it: every `EXCLUDED-STACK`
result carries an `'assumption'` string
(`ENTRY_SEED_ASSUMPTION`) naming exactly this gap, and the JSON output
adds `excluded_stack_depends_on_unproven_entry_assumption` (a count) at
the top level. Because the assumption underlies `STACK_SYMBOLS` itself,
that count is every `EXCLUDED-STACK` row -- 4312 of the 12634 census rows
for `0x252658` in this rerun (below) -- not a small flagged subset. Both
targets under test (`0x252658`, `0x254d9c`) sit well below S's current
lower bound (`0x26f000 - 0x252658` = `0x1c9a8`, ~117 KB), so a second stack
region would have to be implausibly large or specifically placed to reach
either one; that is contextual reassurance for *this* run, not a proof,
and does not apply to an arbitrary future target passed to this tool.

**[D]** Fixed in `tools/sharc_trace.py`'s Type19a_scaled handler, but with
a **fresh, uniquely-named symbol per modify site**, not by reusing the
input symbol. PRM p.6-7 ("MODIFY wraps whenever L != 0") guarantees a
circular MODIFY's result lands in `[B,B+L*scale)` regardless of B/L's
concrete values -- a bound the tracer was discarding to
`Unknown('scaled circular modify I%d')` whenever the pre-modify value/B/L
were not *all* concrete, true almost always since I6/I7 are seeded as
named symbols at each function's own entry and B7 is open per Item 2
above. An earlier version of this fix re-used the bare input symbol
unchanged, which asserted a false equality (two different circular
MODIFYs of I7 -- or the same site visited twice with genuinely different
runtime state -- would both collapse to the identical `Affine` term,
letting the algebra cancel "site A's result minus site B's result" to a
spurious 0 and alias two different stack frames); replaced before this was
used anywhere else, per review. The fix now: `_stack_bounded_symbol()`
recognises when the pre-modify value is a single named symbol (any
existing constant offset, not just a bare one) whose name matches the
entry-seed convention (`I6e`, `B7e`, ...) or an earlier
`tools/sharc_trace.py`-minted `circ_`-prefixed symbol; when that symbol's
offset is smaller than one buffer length (PRM p.6-23's own single
`+-byte_length` wrap-correction bound, computed from L7's proven-constant
value), the handler mints a brand-new `circ_<reg>_<pc>` symbol for the
result (never the input's own name) instead of `Unknown`.
`tools/sharcwriters.py`'s classifier was extended, not left to the
tracer's Affine machinery alone: `is_circ_symbol()`/`combined_affine_range()`
treat a `circ_` term as ranging over S widened by `CIRC_WRAP_SLACK`
(`L7*4 = 0x7f4`) on both sides, not S itself, and every `EXCLUDED-STACK`
result states this explicitly (`via_circular_modify` when a `circ_` term
was involved, plus the `assumption` string always). Deliberately still
conservative: an input offset at or beyond one buffer length is not
provably within the same window, so it still falls back to `Unknown`.
Tests: `tests/test_sharc_trace.py` (8: fresh-symbol minting, two different
sites proven *not* equal, chaining through a prior `circ_` symbol, a small
offset still accepted, a large offset and an unrecognised symbol name and
a non-concrete L7 all still falling back to `Unknown`, and the
`_stack_bounded_symbol` helper itself), `tools/sharcwriters.py`
(`combined_affine_range`, `is_circ_symbol`, the classifier's `circ_`
handling including a mixed plain+`circ_` expression, and the
`ENTRY_SEED_ASSUMPTION` string being present on every `EXCLUDED-STACK`
result), plus 2 end-to-end tests through `tools/sharc_trace.py`'s own
single-instruction executor with no firmware (including one proving two
different modify-site PCs yield different, non-equal addresses).

**[D]** Re-run, `--jobs 16`, same image (`image_sha256`
`0f514a12a2255f5c081e292c47f1f29462003177658da4bbae0a22fd737fffa2`; prior
outputs kept as `out/sharcwriters/{252658,254d9c}.json` and `*-v2.json`,
this run as `*-v3.json`). Control `0x254d9c`: still exactly two `HIT`s, at
`sw 0x1c191d` (Type14a) and `sw 0x1c16ad` (Type16b) -- unchanged, and
`EXCLUDED-CONST`/`ENTRY-RELATIVE`/`LOADED-POINTER` also unchanged from the
prior pass's `*-v2.json` for both targets, the same soundness canary as
before. Target `0x252658`: `HIT` 0 (unchanged), `EXCLUDED-STACK`
4,312 -> 4,389 (+77 from relaxing the bare-symbol guard to a bounded-offset
one), `UNRESOLVED` 7,569 -> 7,492 (-77).
`excluded_stack_depends_on_unproven_entry_assumption` = 4389 (every
`EXCLUDED-STACK` row -- see Item 2's verdict above), of which
`excluded_stack_via_circular_modify` = 948 (this pass's entire cumulative
gain over the pre-fix baseline: `3441 -> 4389`). Top `UNRESOLVED` reasons
unchanged in shape from the prior pass: the M7-cascade-through-an-already-
adjusted-register bucket `"I<n> + M<n> * 4"` drops 1,072 -> 1,005 (the 77
newly-resolved stores' own former bucket), undecoded/unconfirmed forms
(627), unsupported full computes (437), Type11a/unsupported multifunction
(391+114), Type7d non-concrete source (366, the ACONV finding above), and
RFRAME through a nonconcrete I6 (101, Item 2's open question, still
untouched by any classifier change).

## Four more functions read

Full notes in `docs/findings/functions/`.

**`blk88@0x1c0d68`** -- 127 instructions, leaf, **15 callers**, and the most
useful of the four. Convergent evidence makes it a shared **`base^x` evaluator**:
`R4` across all 15 callers takes almost exclusively one of **2.0, 10.0 or pi**,
selecting a base (pitch doubling, decade/dB, angle). Callers set up
`R12 = 64.0` with `R8 ~= 1/12` (the semitone fraction) and one uses
`R13 = 220.0` (A3). So this is the engine's **pitch and dB conversion
primitive**. It reads three hard-coded RAM cells (`0x2411c8`, `0x241210`,
`0x241234`) that no caller passes -- shared global state. Its single dual
add/subtract is a plain register-pair sum/difference, not an indexed FFT walk --
another data point for the image-wide conclusion. Two float ALU opcodes
(`0xd9`, `0xda`) are absent from the public table, so no exact formula **[O]**.

**`blk93@0x1c207b`** (303 instructions) -- per-track gain smoothing and soft
limiting. Float constants **0.8465 and 0.1534** (summing to ~1.0, a one-pole
blend pair), **3.1623** (sqrt(10), the +10 dB amplitude ratio), 1.0, 8.0, 1/32.
Real `clip Fx by F1` ops, a MUL+ADD and a MUL+MAX multifunction followed by a
single `min` -- a clamp built from two ALU ops. 14 loop setups, **all literal**:
ten of count 16 span 24, one 16/59, one 15/24, and one **count 3 span 94** that
holds essentially all the heavy compute. Writes state back into the shared
context struct `0x252d3c` at `+4`/`+12`.

**`blk93@0x1c14e7`** (98 instructions) -- the last call in the render chain, and
a candidate for the missing ring writer. Five literal loops (256, 16, 16, 32,
16). The `R12`/`R8`/`R4` context arguments are **never dereferenced**; every
address it uses is a hard-coded literal in `0x252d3c`-`0x254800`, all RAM. Its
32-count loop walks a **32-entry pointer table at `0x252d78`** (= `R12+0x3c`),
one slot per track, dereferences each and writes **16 floats through it**
(8x2, stereo-shaped) scaled by 1.0965000391. `MODE1.PEYEN` (SIMD) is on for
every compute loop. Where those floats land is unknowable statically because the
table is runtime-written **[O]** -- but a per-track pointer table written through
is exactly the shape the ring writer would have.

**`blk93@0x1cb4b2`** (183 instructions) -- normalises a state-struct integer via
the `+2^32` unsigned-to-float idiom, calls the shared reciprocal `0x1c06ba`,
then either emits one saturated value through the `x <- x*(2-|x|)` polynomial
(**four** iterations here, against stage 3's three) or refreshes a run of values
through a register-counted loop blending a second array via six dual float-MACs.

## Tool changes **[V]**

`tools/sharc_trace.py`: added `mant` (`0xad`) and `scalb` (`0xbd`) with their
bespoke flag behaviour (MANT overrides infinity to the all-ones sentinel, unlike
ordinary float ops; SCALB overrides IEEE subnormal rounding on underflow), plus
fixed-point `min`/`max` (`0x61`/`0x62`), plus **Type3a predicates** -- which
turned out to be contained: PRM Table 13-1 shows `IF cond` gates the whole
instruction, compute and transfer, so it uses the same fork pattern as the other
predicated forms.

`tools/sharcinv.py`: the **"FFT-like" label is renamed "paired sum/difference
(coefficient combine)"**, and the rule now requires corroboration -- bit-reversed
addressing, uniform power-of-two loop counts, a table touch, or nested loops --
before claiming anything spectral. Two new vector features, `loop_pow2_uniform`
and `nested_loops`. Of the 13 functions carrying dual add/subtract, **11 have
zero corroborators and none reaches the threshold of two**: a clean zero,
confirming the FFT result from a second direction. Also filters a 0-instruction
boundary artifact (1228 -> 1227 functions) and annotates interior-call splits so
the `0x1c71ec` 235-vs-251 mismatch is visible rather than confusing.

Tests: **521 -> 545 passed**, 5 skipped, 174 subtests.

## Type 8a fixed, and what survived the re-check **[V]**

`tools/sharcflow.py` now decodes Type 8a calls. Bit 39 `b` is the CALL/JUMP
selector (`b=1` call, `b=0` branch); bit 26 `j` is delayed vs non-delayed, not a
call selector; bits 37:33 carry the condition, which is unconditional on every
call in this image. **The offset formula is identical to 25a's** -- the earlier
report that "9 targets didn't resolve sanely" was a **missing 24-bit wraparound
mask**, not a different convention. Adding that mask also fixed a **pre-existing
25a bug**: blk88's call at `0x1c0fa5` had been silently resolving to a negative
garbage target, and is now correctly `0xb893e2` in blk69.

Call sites **1720 -> 1780** (+51 blk93, +7 blk69, +2 blk88); distinct targets
546 -> 551. Tests **545 -> 556**.

The re-check matters more than the counts:

- **`0x1c06ba` goes from 0 to 30 caller functions** across 55 call sites. It is
  the shared reciprocal (RECIPS + three Newton-Raphson iterations).
- **`FUN_1c71ec`, the ring-construction function `0x1c75d8`, and `0x1cb4b2` are
  still genuinely callerless** in the corrected graph -- none of the five new
  targets is any of them. So that part of the runtime-dispatch story holds.
- `FUN_1c2b24`'s caller was already known (`0x1c75d8` at `0x1c771e`) and is
  unaffected.
- Callerless total 719 -> 718.

The inventory still mislabels `0x1c06ba` as "IIR or recurrence" -- its leaf-ish
vector cannot distinguish RECIPS+Newton-Raphson from a real recurrence **[O]**.

## blk69 and blk93 are one library **[V]**

The strongest evidence yet, and it settles a question open since blk69 was
found. `blk69@0xb819bd`'s caller (span `0xb820b1`-`0xb821ee`) fires eight calls
in sequence: six into blk69, and **two directly into blk93 at `sw 0x1ccbd8`** --
which is stage 1 of the `FUN_1c71ec` wavetable pipeline. That is a live
cross-block call, not merely shared bytes. One call graph, one DSP library,
spanning both address spaces.

`blk69@0xb819bd` itself (212 instructions) is **the polynomial envelope
saturator** `x <- x*(2-|x|)`, applied to three fields (`+8`/`+112`/`+120`) of
nodes in a short pointer chain, first a fixed set then a register-counted loop.
Same idiom as blk93's stage 3. The inventory's "wavetable lookup" label is
wrong; the dominant instruction mix is the envelope. No peripheral literals, so
**not driver code**.

## A 6-tap polyphase resampler, and a candidate SRC-page consumer **[D][O]**

`blk93@0x1c4f81` (375 instructions) is the most interesting function read so
far. It is **not** the simple two-tap blend its label predicted -- the label
fired on three `0x80000000` literals that are `-0.0` compare sentinels, not
table addresses. What it actually contains:

- a **64-bit fixed-point phase accumulator** (`R4:R13`, add-with-carry), with
  the step pair built from `I4+98`/`I4+99`
- two Type 8a calls to the shared reciprocal `0x1c06ba` with the documented
  `2^32` correction
- **two structurally identical 64-count hardware loops, each doing a
  phase-indexed 6-tap MAC** -- windowed-sinc or polyphase interpolation, not a
  2-tap blend

A 64-bit phase accumulator feeding a 6-tap polyphase kernel is **proper
sample-rate conversion** -- the shape of a good pitch-shifter or resampler, and
the obvious engine for REPITCH.

It reads a **new table at `0x25d940`**, not in the catalogue, RAM-only
(runtime-populated), indexed by a phase-derived M-register offset, three
consecutive 8-byte long-word entries per access -- a plausible polyphase kernel
bank **[O]**.

**And the parameter-frame question.** `I4` is its sole meaningful incoming
register, read at word offsets **98-109** and, via dynamically indexed
M-register reads, **380-444**. **If `I4` is the frame base `0x2558dc`, all of
that lands inside the parameter frame** -- which would make this the
long-missing SRC-page consumer. Not provable without a caller trace, and the
Type 8a gap had been hiding callers **[O]**. **This is the single most promising
open thread.**

Bounds caveat worth knowing generally: this function's live exit is an
unconditional jump **backward** into the shared register-restore epilogue of the
*preceding* return-delimited span (`0x1c4f56`-`0x1c4f7c`), not through its own
nominal tail. So "375 instructions" undercounts what executes, and the
return-delimited boundary model has a blind spot here.

## Three more functions, and a not-a-function **[D]**

Full notes in `docs/findings/functions/`.

**`blk93@0x1cddff` is not a function.** It is the cold `else` branch of
`blk93@0x1cdd8b` (55 instructions, called once from `0x1c642a`), reached by a
**Type 8a conditional JUMP** (`b=0`, `cond=GT`) at `0x1cddb5`, and its own
unconditional jump at `0x1cdec8` rejoins the parent's shared 13-register restore
epilogue. Nothing calls it, anywhere, and its address appears nowhere as data --
verified across the whole blob in both endiannesses. This is why every
"no callers" scan missed it, and it is a caution for the whole inventory: a
return-delimited span can contain a branch target that looks like a function and
is not. Its body is 8 MAC pairs interleaved with min/max/trunc clamps against
127.0 and one literal 14-iteration loop -- a new shape, not the envelope, not
the one-pole blend **[O]**.

**`blk93@0x1ccfa4`** (180 instructions, straight-line, no loop, no branch) is
**multi-table two-tap interpolated coefficient synthesis**: two textbook
interpolated lookups (`idx = trunc(x*N)`, `idx2 = min(idx+1, N-1)`, lerp)
against a **128-entry table at `0x2c2cc0`** and a **1024-entry table at
`0x2c2018`** -- both RAM, neither in the catalogue. Not the envelope (no float
abs anywhere). It makes **six** calls, not the two the tools reported: four
Type 8a to the reciprocal, plus two to **`0x1c1284`, a previously undocumented
~30-instruction helper whose constant is `ln(10) = 2.302585`** -- a log or dB
primitive, distinct from the `base^x` evaluator. One caller builds a 6-slot
struct with `0.995` per slot before calling, a plausible one-pole coefficient.

**`blk88@0x1c1199`** (90 instructions, genuine leaf, no loop) is **not** the
"IIR or recurrence" the label claims -- the same mislabel already corrected for
`0x1c06ba`. It **inlines** RECIPS + three Newton-Raphson iterations **twice**
rather than calling the shared routine, and evaluates a 3-coefficient Horner
polynomial with constants `-0.19033`, `-7.13793`, `-42.8277`. `R8` is guarded
against `< 2^-12` immediately before the first reciprocal, so `R8` is the
operand being inverted. Three exit points, two of them conditional returns --
which `sharcflow.py` also misses, since it matches only the unconditional return
word **[O]**.

## Tracer and table gaps found this round **[O]**

Recorded so they are not rediscovered: `Type10a_rel` ("with PC-relative jump",
classic PGR p.458) is **absent from `tools/sharc_visa_tables.py` entirely**;
Type 2b shifter opcode `0xb0` is missing from `compute_table.json`'s shiftop
table; ALU opcode `0xe0` is unsupported; multifunction categories `0x1a` and
`0x1e` and opcode `0xda` were identified by hand as
`FM=Fx*Fy, FA=float RXA by RYA / max(...)` and `FN=float RX by RY`. Also:
`--blob` and `--base-sw` are mutually exclusive in `sharc_trace.py`, by design.

## A named musical computation: MIDI note to frequency **[D]**

`blk93@0x1cbe19` (100 instructions) is **not** the "envelope or gain" its label
claimed. Its constant set is decisive: **220.0, 69.0, 12.0, 1/12, 2.0** -- that is
the 12-tone equal temperament formula

```
f = 220 * 2^((note - 69) / 12)
```

and it calls `blk88@0x1c0d68` with `R4 = 2.0`, which is exactly that shared
`base^x` primitive's pitch branch. Its final output, after the `base^x` call and
a reciprocal call, is **clamped between 20.0 and 22000.0** -- the audio frequency
range. It writes that to `*(I5+60)`, and it sits immediately before
`0x1cbf07`, stage 6 of the wavetable pipeline (the `0x1fff`-masked 8192-entry
lookup). So this computes the frequency that drives a wavetable oscillator.

This is the first end-to-end *musical* computation identified in the DSP, and it
confirms the library reading: a note number goes in, a clamped frequency comes
out, via a shared exponential primitive.

It also uses a **new ROM table at DM `0x26b338`** (byte `0x2826b338`, file offset
`0x66c8` in blk37 -- real payload, not RAM): **128 entries, monotonic and
concave, 0.0 at index 0 rising to 1.0 at index 127**. A 0..127 response curve,
read with the same scale-by-127 / clamp / two-adjacent-taps idiom as `0x1c18a6`
and the `0x2c2cc0` table. Closed form not determined **[O]**.

A third callee was found that the tools had missed -- a Type 8a call to the
shared reciprocal -- plus a thin forwarder `0x1cc6c4` to `0x1c12b4`, next to the
`ln(10)` log primitive, plausibly another shared-math sibling **[O]**.

Of the two call sites in the orchestrator, only `R4` is reliably set by both;
the extra `R12` traffic at `0x1c6e88` is caller-side scheduling the callee never
reads.

## [C] The polyphase resampler is in the render chain, and `I4` is not the frame

Two corrections to the `0x1c4f81` entry above, both from re-running the caller
search with Type 8a decoding fixed.

**It is not an independently called function.** Nothing calls it -- no 25a, no
Type 8a, and its address appears nowhere as data in any block, both
endiannesses. What reaches it is an `8a_rel` **conditional JUMP** (`b=0`,
`cond=23`) at `sw 0x1c4f25`, from *inside* the preceding span `0x1c4ecf`, well
before that span's own return. Its exit jumps backward into `0x1c4ecf`'s restore
epilogue. **The two inventory "functions" are architecturally one routine**,
split only because a return belonging to `0x1c4ecf`'s non-resampling path
happens to sit just before `0x1c4f81` begins.

Within the stated aligned/depth>=8 decoded-block coverage, `0x1c4ecf` has one
known direct `25a_direct` call at `sw 0x1c6b00`, inside `0x1c642a`. This
bounded coverage does not claim global exclusivity. So the chain is:

```
FUN_1c2b24 -> 0x1c642a -> call 0x1c4ecf  (sw 0x1c6b00)
                       -> cond=23 branch to 0x1c4f81  [the resampler]
                       -> jump back into 0x1c4ecf's epilogue
```

**The static call structure places the conditional continuation in this render
call chain**, two calls deep behind a conditional gate. It does not establish
natural per-frame or universal execution. **[D][O]**

**`I4` is not the parameter frame.** Tested numerically against the confirmed
SRC-page layout: word offsets 98-101 would land on **track 1's amp/FX fields**,
not the SRC page; 104-107 on track 2's SAMP/LEN/LEV, skipping TUNE, PLAY, CFADE
and STRT entirely; 108-109 fall past the page. The two reads that seed the phase
step, `DM(I4+98)`/`DM(I4+99)`, would read amp parameters, and **no track's TUNE
is read anywhere under this hypothesis**.

The clinching argument is alignment: every per-track block starts at frame byte
`0xda`, which is **2 mod 4**, so a 4-byte-granularity read from a 4-aligned base
can never land on a track's TUNE field at all. The apparent SRC-page hits were
modulo coincidences.

What `I4` is remains **[O]**, but a per-voice DSP-side struct now has direct
precedent in the same call chain: `0x1c207b` uses its own per-track buffer
family with the same `0x60` stride at an unrelated base (~`0x252c58`/`0x252cb8`).
Closing this needs the tracer to reach `sw 0x1c6b00` and read `I4` there.

`blk93@0x1cdbb2` (115 instructions) is the third of three back-to-back sibling
calls in the orchestrator (`0x1cbe19`, `0x1cbdea`, `0x1cdbb2`). It uses the
`R4 -> I4`, `R8 -> I5`, `R12 -> I3` convention already confirmed for `0x1cb4b2`
-- evidence of a shared function family -- with a genuine read-modify-write at
`I4+12`/`+16`, so state persists across calls. Its shape matches none of the
confirmed six; proposed as a conditional per-track state accumulator **[O]**.

## Tooling: a function dossier, and six decode gaps closed **[V]**

**`tools/sharcfn.py`** produces a **function dossier** in one command, replacing
the setup work six agents had each been doing by hand (each wrote its own
annotated disassembler in the scratchpad):

```
uv run python tools/sharcfn.py BLOB.bin ADDR [--json OUT] [--listing]
uv run python tools/sharcfn.py BLOB.bin --batch A,B,... --out-dir DIR
```

It gives identification (block, the `base_sw` convention stated explicitly, file
offset, sha check), bounds with **both known hazards flagged** -- an internal
branch target that is not a function, and a backward jump into a preceding
span's epilogue -- the call graph including a local conditional-return scan that
`sharcflow.py` does not do, an annotated listing with resolved operands, named
regions, RAM-vs-ROM, decoded IEEE-754 constants and loop trip counts, and the
inventory feature vector.

Validated against all fourteen hand-read functions: instruction counts match
everywhere except `0x1c71ec`'s already-documented tail split. It found two real
bugs in itself during validation (`RETURN` not recognised; multiply opcode
`0x30` and the whole MUL+ALU space decoded against the wrong table) and
reproduced `0x1c4f81`'s backward-epilogue hazard exactly. Dossiers for the top
25 shortlist are pre-generated.

**Decode gaps closed** in `sharc_trace.py` and `sharcspec/`:

| gap | resolution |
| --- | --- |
| `Type10a_rel` | was already split out by `build_table.py` but flagged `visa=False` because the PRM heading says "ISA". Firmware evidence overrides it: `sw 0x1c5030` decodes cleanly as this form inside VISA code and desyncs without it. Narrow `VISA_OVERRIDE`, same precedent as `Type2a_short`/`Type6b_shiftimm` |
| ALU `0xe0` | `FN = FX copysign FY` (PRM p.19-19, PGR Table 12-4 p.574) |
| multifunction `0x1a`/`0x1e`/`0x1f` | MUL+float-by-scale, MUL+MAX, MUL+MIN (PGR Table 12-12 pp.587-588), reusing the dual-result path |
| `0xd9`/`0xda`/`0xdd`/`0xc9` | the `fix`/`float`/`trunc ... by RY` scaled-convert family, plus the unscaled `fix` that was also missing |
| ALU `0x05`/`0x06` | add/subtract with carry (PRM p.438) |
| Type 2b | the whole form was unhandled |
| shifter `0xb0` | **absent from both PRM Table 17-9 and PGR Table 12-11.** Decodes now, but value and flags return `Unknown` -- no public source, nothing invented |

The hand-derived guesses from the previous round all verified correct against the
manuals: `0x1a` = `FA=float RXA by RYA`, `0x1e` = max, `0xda` = `FN=float RX by RY`.

Tests **556 -> 593**. **One honest gap: the opcode work added no opcode-specific
tests** -- the agent ran out of time. That should be filled in the style of
`FloatComputeTest` before the next opcode pass.

Newly surfaced and still open: `cu=2` opcode `0x10`, `cu=1` opcode `0x48`,
form `1a`, and Type 9b indirect targets.

## [C] Machine-path boundary for the conditional resampler **[V][D][O]**

The practical goal is to identify which SHARC code each machine uses, so
existing machines can be modified or augmented and new ones may eventually be
added. The bounded result below advances selection/dispatch mapping, not merely
resampler classification.

Within the stated aligned/depth>=8 decoded-block coverage, one known direct
call is the unconditional `0x1c6b00 -> 0x1c4ecf` call owned by
`blk93@0x1c642a`; this bounded coverage does not claim global exclusivity.
The exact local chain is `0x1c6ad3: I4=DM(I6+124)`, `0x1c6ad7: R2=4`,
`0x1c6ad9: R1=DM(I6+124)`, `0x1c6adb: R13=R1+R2`, and
`0x1c6afd: R4=R13`. The exact conditional gate targets are
`0x1c6acc -> 0x1c6530`, `0x1c6aeb -> 0x1c6afd`, and the back-edge
`0x1c6b09 -> 0x1c6ae8`; the `R14=0x20` setup and its decrements are also
byte facts. The incoming `I6` root/object identity, natural branch outcomes,
and effective loop trip count remain unresolved. Thus the earlier render-chain
wording does **not** show `0x1c4ecf` is a machine selector or that it runs
universally/per track. **[C][V][D][O]**

Within the conditional continuation, `0x25d940` is materialized exactly twice:
into `I5` at `0x1c50aa` in the first literal-64 loop and `I1` at `0x1c52bf`
in the second; `DM(I1,M5)=R2` at `0x1c52de` is an indexed store in that second
loop. The concrete same-base read/access shapes are also verified. No initializer
was found in the covered decoded blocks, but this bounded coverage is not a
whole-firmware absence result: aliases, register-built addresses, omitted or
uncertain code, and runtime initialization remain unexamined. Table,
coefficient, working-bank, per-machine-storage, and ownership interpretations
remain open. **[V][D][O]**

**Next static experiment.** Prioritize the runtime function-pointer dispatch
RAM at `0x254d98` and its writers, especially any chain from the transported
machine type at frame offset `0x94`; separately resolve incoming `I6` and
`DM(I6+124)`. For augmentation, observe or patch writer `0x1c52de` first as an
evidenced bank seam; changing either loop body or the shared table may affect
all users, while a machine-specific dispatch remains unproven before the
`0x254d98` mapping is known. This is a provisional ranking, not a demonstrated
machine mapping or patch prescription. **[D][O]**

Reproducible ignored evidence is
`out/sharc-engine/machine-path/summary.txt` (SHA-256
`fb7a6a080015e4ce173f745dde17160aa5970931b85ad2a2919a3a22cb5301e4`); its two
identical extraction records hash to
`73c5ce0ad83f7bb0e92156841f27b06aa7a74fe808d2f29848756ba1bf98b218`.
**[D]**

## Where new DSP code could live, and what engine pieces cost **[D][O]**

The public references give L2 SRAM as 1 MB at byte `0x20000000..0x20100000`
(eight 128 KB banks), and allow code and data in external DDR, more slowly.
The sizes of the four L1 blocks are in the datasheet, which is not under
`out/refs/`, so L1 space past each loaded window is of unknown extent, not
free. The L1 blocks are not contiguous, so the gaps between them, such as
`0x26f000..0x2c0000`, are probably not memory at all. **[D]**

L2 past the loader, `0x2001ab7c..0x20100000` (939,140 bytes), is the only
large candidate. `0x200fa000..0x200fe000` (16 KB) is occupied: two called
functions load those addresses as literals. The remaining ~922,756 bytes have
no literal reference, but `tools/sharcwriters.py` leaves 59% of stores
unresolved for any probe address and the SHARC has no runtime leg, so this is
weaker evidence than the ColdFire cave map. The external-DDR FILL spans
(about 4.35 MB, 1.05 MB and 33.5 MB) showed live references wherever
sampled, one of them read by the render orchestrator `FUN_001c642a`; treat
them as occupied. The L2 code block's short-word base is `0xb80000`,
confirmed by byte comparison. **[D][O]**

The context-switch pointer `DM(0x2ca3e0)` is zero in the loader image, so
its structures are built at run time. Its two writers, `FUN_00b85af7` and
`FUN_00b85f6c`, are located but not yet read; they decide whether a second
stack sits inside the L2 candidate. **[O]**

Engine pieces, as a yardstick. The large code blocks average 4.6 bytes per
instruction (4.5-4.9); the main program is 104,848 bytes. **[D]**

| function | sw range | instructions | bytes | role |
|---|---|---:|---:|---|
| wavetable stage 1 | `0x1ccbd8`-`0x1ccc58` | 59 | 256 | 2-pole IIR / quadrature-oscillator recurrence |
| wavetable stage 2 | `0x1cdecb`-`0x1cdf38` | 51 | 218 | gated block copy / linear interpolation |
| wavetable stage 3 | `0x1cb3d8`-`0x1cb4b2` | 98 | 436 | saturating envelope, shared, 4 call sites |
| wavetable stage 4 | `0x1cd286`-`0x1cd3b5` | 148 | 606 | 9-field-pair MAC gather |
| wavetable stage 5 | `0x1cc79e`-`0x1cc935` | 173 | 814 | table-interpolated 2-tap resampler, table `0x26bb68` |
| wavetable stage 6 | `0x1cbf07`-`0x1cc040` | 133 | 626 | 2-tap wavetable lookup, 8192-entry table |
| per-track gain smoothing/limiter | `0x1c207b`-`0x1c238a` | 303 | 1,566 | 16-track loop, writes `0x252d3c` |
| large interpolated-table/RAM forwarder | `0x1c18a6`-`0x1c1f7d` | 654 | 3,502 | role open |
| indirect scale/update, 32-slot table | `0x1c14e7`-`0x1c15e3` | 98 | 504 | writes 32-entry pointer table `0x252d78` |
| envelope/coefficient-table refresh | `0x1cb4b2`-`0x1cb647` | 183 | 810 | |
| shared scalar mapping (1/x + Horner) | `0x1c1199`-`0x1c127c` | 90 | 454 | |

The whole six-stage wavetable pipeline is 662 instructions and 2,956 bytes,
so the L2 candidate could hold a new DSP feature of that size several hundred
times over. Space is not the constraint for new DSP code; the hook into the
render path is. **[D]**

## `FUN_1c24e9` reads the SRC page, and the machine type is only a change detector **[C][V]**

Corrects "`FUN_1c24e9` ... does not read the SRC page" above. That table
assumed 32-bit loads at x4 scale and so missed five Type4b loads whose
access width is short-word sign-extended (x2). With `I2 = 0x2559b6 +
track*0x60` (Type19a at `0x1c2537`, destination I2 from `is XOR idis`), per
track `t` the function reads frame offsets `t*0x60 +`:

| PC | form | frame offset | mirror |
|---|---|---|---|
| `0x1c265f` | 4b `DM(I2+2)` | `+0xde` | 27 (CFADE, slot 2) |
| `0x1c265b` | 4b `DM(I2+6)` | `+0xe6` | 31 |
| `0x1c25e5` | 4b `DM(I2+7)` | `+0xe8` | 32 |
| `0x1c264a` | 4b `DM(I2+8)` | `+0xea` | 33 (slot 6) |
| `0x1c264e` | 4b `DM(I2+9)` | `+0xec` | 34 (LEV) |

All five are unconditional and run before the type byte is loaded
(`0x1c26d4`), so they are the same for every machine. Two agents decoded
them independently from `section_7_BLOB.bin`, and a concrete
`tools/sharc_trace.py` run of `FUN_1c24e9` on a captured frame read the
poked values back. The M-indexed reads `DM(I2,M5)` at `0x1c2648` and
`DM(I2,M6)` at `0x1c2594` land on `+0xda`/`+0xdc` only if M5=0 and M6=1 at
that point, which is still unproven. **[O]**

A slot-6 value poked into the frame reaches a float-converted store at
`0x1c2737` (`DM(I5-96)=R1`, `0x38888889` for raw 2), the same unpack and
scale path the filter/amp/FX fields take; `I5` there did not resolve to a
concrete buffer, so its reader is not yet named. CFADE's value past its
first shift is not established: the tracer logs compute events without
values. **[O]**

The machine type word (frame `0x94+2t`) is used only as a change detector
in `FUN_001c2b24`: `R1 = DM(I4,M0)` (cache at `0x255970`), `R0 =
DM(I0,M0)` (live), `R1 = comp(R1,R0)` at `0x1c33d7`, then `IF EQ JUMP
0x1c33e9 (DB)` at `0x1c33df` (`0007 0004 0a00`, j=1). The two delay-slot
instructions `0x1c33e2` (ASHIFT R2 by -8) and `0x1c33e5`
(`DM(I5+0xc4)=R2`) always run (two instructions after a (DB) branch, SHARC+
PRM p.4-18); on a change the fallthrough `0x1c33e7` then stores M14, which
nothing in the function writes, so the reset seed 1 survives. Changes also
write M14/M13 into per-track arrays at `0x24f0ac`/`0x24f0ec` and call
`FUN_001c60a2`, which sets a per-track flag at `0x2412c8+4+t*0x1d8+0x1b9`.
Nothing traced indexes a table by the type value. Checked by a second agent
against the bytes, the manual and a seeded trace run.

`tools/sharcfn.py`'s `render_modify()` prints the MODIFY destination as the
`is` register; the real destination is `is XOR idis` (plus bank), as
`tools/sharc_trace.py` executes it. Do not trust `sharcfn.py --listing`
destinations for 19a/16a/16b forms. **[V]**

`tools/sharc_trace.py` now implements FEXT (se) (shift-immediate opcode
0x12, PRM Table 17-9) and register-operand ASHIFT (cu=2 opcode 0x04).

## Where the SRC-page words go in `FUN_1c24e9` **[D][O]**

With `R8` held symbolic and `R12=0`, `tools/sharc_trace.py` resolves the
destination of `FUN_1c24e9`'s stores: `I5 = R8*0xdc + 0x2506ec` (`0x1c2560`,
`0x1c256e`, `0x1c25ca`), a table of 32 slots of 0xdc bytes indexed by
`FUN_1c2b24`'s slot argument. `FUN_1c2b24` passes the same base to
`FUN_1c642a` (`R8=0x2506ec; R4=0x2412c8; CALL 0x1c642a` at `0x1c307d`), and a
concrete run of `FUN_1c642a` walks it with stride 220 until an indirect jump
through I12/M13 at `0x1c6579`.

| frame word | mirror | fate in `FUN_1c24e9` |
|---|---|---|
| `+0xde` CFADE | 27 | read, shifted, spilled, then overwritten at `0x1c26f2`; never stored |
| `+0xe6` | 31 | float-converted and scaled, then dropped |
| `+0xe8` | 32 | gates the branch at `0x1c2727`; not stored as data |
| `+0xea` | 33 | stored at table `+12` as `raw/30720.0` |
| `+0xec` LEV | 34 | stored at table `+72` as `raw/32768.0` |

So the CFADE control on XSLICE reaches the frame (`+0xde = 0x00c0` after a
turn, HUD "Crossfade=50") but the SHARC discards it. Mirror 33 is the live
unused slot on SLICE; SAMPLE's LOOP (`0xd0`, max `0x7802`) uses it. From a
single agent's trace; needs a second check. **[D][O]**
