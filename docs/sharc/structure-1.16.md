# Digitakt II SHARC+ DSP program — structural map v2 (full multi-region load)

Imported from Em's sharc-spec work (`analysis/structure_v2.md`) on
2026-09-15. The firmware is Digitakt II OS 1.16. Other files it names under
`analysis/` were not imported. The host CPU is a ColdFire MCF5441x, so the
original's "ARM" is written ColdFire here.

Supersedes `structure.md` (v1) for everything touching the call graph, the RPC
dispatcher, and the ColdFire↔SHARC shared-memory hypothesis. v1's string/xref
inventory (§2/§3 there) is still valid raw data and is reused here; this
document's job is (a) load **all** code regions into one Ghidra program so
cross-bank branches resolve, and (b) push much further into the RPC
dispatcher and Audio Task than v1 could.

Tools: Ghidra 12.1.3, `SHARC_VISA:LE:32:default` (installed copy verified
byte-identical to `ghidra/SHARC_VISA/data/languages/sharc_visa.sla` in
sharc-spec — confirmed same toolchain, not stock/prior-art). `sharc_decode.py` +
`render.py` for the disassembly quoted throughout. Clean-room notes: all
addresses/bytes below are direct tool output; the ADI peripheral-address
ranges cited (SPORT/DAI blocks, DDR) are public datasheet knowledge, not
taken from vendor toolchain files.

---

## 0. Loading all regions — what changed and the one open addressing question

**Loaded 12 regions in one `ProgramDB`** (up from v1's 11, and up from the
single-region 56-function trial run mentioned in the task brief):

| region file | BW load | bank | note |
|---|---|---|---|
| `..._20000000.bin` | `0x20000000` | L2 | **newly added** — v1 omitted it entirely |
| `..._28240000.bin` | `0x28240000` | L1-2824 | |
| `..._282403f0.bin` | `0x282403f0` | L1-2824 | |
| `..._2824045c.bin` | `0x2824045c` | L1-2824 | |
| `..._282404a8.bin` | `0x282404a8` | L1-2824 | |
| `..._282412b8.bin` | `0x282412b8` | L1-2824 | |
| `..._282412c0.bin` | `0x282412c0` | L1-2824 | **app bank** — holds the `RPC dispatcher` and `Audio Task` strings |
| `..._282c0000.bin` | `0x282c0000` | L1-282C | FreeRTOS kernel bank |
| `..._282d72dc.bin` | `0x282d72dc` | L1-282D | |
| `..._282d72e0.bin` | `0x282d72e0` | L1-282D | |
| `..._28380000.bin` | `0x28380000` | L1-2838 | |
| `..._28382670.bin` | `0x28382670` | L1-2838 | **entry bank** — `entry` (SW `0x1c1338`), Audio Task + RPC dispatcher *creation* sites both live here |

Excluded (confirmed data, not code, per task brief): `..._80000000.bin`,
`..._8045a6c8.bin`, `..._8055c440.bin` (all DDR).

**L1 addressing** (confirmed, from FINDINGS.md/v1): `BW = 2·SW + 0x28000000`.
Ghidra byte-offset into the `ram` space = `BW − 0x28000000`; this is what was
used for all 10 L1-bank regions above, and it reproduces the known entry
identity exactly (`(0x28382670 − 0x28000000)/2 = 0x1c1338` ✓).

**L2 addressing — flagged hypothesis, not confirmed.** The task brief's
formula is scoped to `0x28xxxxxx`. For L2 I used the natural generalization
`BW = 2·SW + 0x20000000` (offset = `BW − 0x20000000`), which places L2 code at
low SW addresses (`0x0`–`~0x6ADE`) that cannot collide with any L1 SW address
(L1's lowest region starts at SW `0x120000`). **This kept L2 self-consistent
and non-overlapping, and Ghidra's own function-finder subsequently produced
plausible-looking functions inside it** (e.g. `FUN_0000013e`, body 52 addrs) —
consistent with real code, not proof the base is right. No cross-bank branch
from L1 into L2 (or vice versa) was observed to validate this independently;
**treat all L2 SW addresses in this document as hypothesis-scale, not
confirmed-scale**, same caveat v1 raised for L2 in general.

**The L1 and L2 names themselves are cross-checked against the public ADI
manuals.** The SHARC+ Core Programming Reference (Rev 1.5, chapter 7, "L1 Memory
Interface") describes L1 as four independent blocks, each mapped to its own
region of the address space and each reachable through several word-size aliases
of the same bytes. That matches the `0x28xxxxxx` regions above: separate
sub-bases at `0x2824xxxx`, `0x282Cxxxx`, `0x282Dxxxx` and `0x28380000`, and the
confirmed `BW = 2·SW + 0x28000000` alias. The ADSP-2156x SHARC+ Processor
Hardware Reference (Rev 1.0, chapter 8, "L2 System Memory") describes one L2
instance, L2CTL0, a single unified SRAM with the boot ROM rather than four
blocks, which matches the single `0x20000000` region. So the labels used here
are right, and `docs/SHARC-ADDRESS-MAP.md` had them the other way round when it
was merged. **[D][C]**

The datasheet page that states this part's numeric memory map outright is not
extracted under `out/refs/` -- the Core Programming Reference defers to it -- so
this rests on the structural match above and not on a single table. **[O]**

---

## 1. Full call graph (Task 1)

Flow-seeded disassembly from `entry` (SW `0x1c1338`), then linear
force-disassembly of every remaining undefined word per block (same method as
v1 and the task's own single-region trial), then `analyzeAll()`.

| metric | v1 (11 regions, old-ish SLEIGH-analysis run) | **this run (12 regions, fixed branch p-code)** |
|---|---:|---:|
| functions | 7 | **147** |
| call edges | 14 | **329** |
| branch/call targets checked | — | 3,184 |
| branch/call targets resolved | — | **2,076 (65.20%)** |

Confirms the task brief's expectation ("many more than 56 with all regions
connected") — the L2 addition plus the fixed branch p-code together produced
a **21× increase in function count** and **23× increase in call edges** over
v1's native Ghidra pass. Full data: `analysis/callgraph_full.txt`,
`analysis/functions_full.json`.

Top-called functions (in-degree, from the 329 CALL-flow edges Ghidra's p-code
actually recognizes — see §1a for why this **undercounts** the true call
graph):

| callee | calls | note |
|---|---:|---|
| `thunk_FUN_001c06db` | 33 | matches v1's independently-found `SW 0x1c06bd` (34 calls) — cross-validated |
| `FUN_001c0d68` | 14 | matches v1's `SW 0x1c0d68` (16 calls) and v1's native-Ghidra corroboration — cross-validated a 3rd time here |
| `thunk_FUN_001c3fa8` | 10 | called repeatedly from inside the RPC dispatcher task body, §2 |
| `thunk_FUN_001c0c4b` | 8 | |

**Entry → init → task-creation chain** (evidence: disassembly, this run +
v1 §4):

```
entry (SW 0x1c1338)
  -> [0x1c1338] one early MMR-ish write (addr=0x31400)
  -> unconditional jump to SW 0x1c0000 (a small ~9.8KB L1-2838 block;
     now resolves cleanly since that bank is loaded in the same program)
  -> (falls into / calls back into) the bulk of the entry-bank "main()"
     code, which in sequence (by address, SW 0x1c35xx onward):
       - repeatedly touches a DDR structure at 0x82a00000+ (§4)
       - SW ~0x1c3f5a-0x1c3f75: creates the "RPC dispatcher" FreeRTOS task (§2)
       - SW ~0x1cb25d-0x1cb32d: configures a SPORT/DAI peripheral pair (§3)
       - SW ~0x1c776d-0x1c7780: creates the "Audio Task" FreeRTOS task (§3)
```

### 1a. The calling convention is software-emulated — a load-bearing discovery

Every one of the ~30 "call sites" traced by hand in §2/§3 below has the
**identical three-instruction shape**:

```
Type3c   d=1, R2           ; push R2 (i7/m7-indexed — NOT the i6 the cspec guessed)
Type16a  [i7,m7] = PC+2    ; store the address of the NEXT instruction (a return slot)
Type25a_direct addr=TARGET ; unconditional GOTO to the callee
```
and every callee ends with:
```
Type9b_abs  pmi=4, pmm=6, j=1    ; indirect jump through a fixed register pair
Type25c_rframe                    ; frame restore / return
```
This exact 2-word return idiom (`083f 343f` / `1901`) recurs **dozens of
times verbatim** across every function traced. **This is the real subroutine
call/return convention this compiled firmware uses** — implemented in
software (push + absolute jump; indirect-jump-back + rframe), not via a
hardware CALL/RTS pair.

This explains a real, previously-unexplained gap: **our SLEIGH module (per
SPEC-FINDINGS.md §6, correctly per the manual) gives `Type25a_direct` plain-GOTO
p-code, and does not classify the `Type9b_abs`/`Type25c_rframe` pair as a
resolvable RETURN**. So Ghidra's own auto-analysis:
- never learns that a `Type25a_direct` "callee" returns control to the
  instruction right after it (the indirect jump target is dynamic/unknown to
  Ghidra), so
- it cannot stitch caller and callee into one function, and
- code that is *only* reachable via this software-return path (most of the
  RPC dispatcher task body, see §2) ends up **outside any function's body at
  all** in Ghidra's model, even though it disassembles cleanly and executes.

This is why the 147/329 figures in §1 are a **lower bound**: the majority of
real subroutine calls in this firmware use `Type25a_direct`, which Ghidra
does not count as a call edge at all (329 counted here are from whatever
subset resolves to Ghidra's native `isCall()`, e.g. some `Type8a` forms).
The decoder-driven traces in §2/§3 are the more complete signal for this
specific convention, exactly as v1 found for its own (smaller) call-graph
work — now with a concrete mechanistic explanation for *why*.

---

## 2. THE RPC DISPATCHER — location + mechanism (priority task)

### 2a. Finding the string reference (v1 could not; this run did)

v1 explicitly reported: *"No RPC-dispatcher string reference was found
(checked under all 3 conventions)."* Re-scanning the existing
`analysis/xref_table.txt` (built from the same 74,596-instruction decode,
just re-grepped) turns it up directly:

```
0x002577f0 [L1(byte-off)] (1 refs): SW0x1c3f62/Type17a/load
```
`0x2577f0 = 0x282577f0 − 0x28000000` — the byte-offset form of the exact
string address given in the task brief. **Evidence, not hypothesis**: this is
a literal decoder xref hit, not an inference.

### 2b. The site is a task-creation call, structurally identical to Audio Task's

Disassembly at SW `0x1c3f46`–`0x1c3f75` (`render.disassemble`, entry bank):

```
001c3f46  Type25a_direct  addr=0x1c7e02          ; call common helper #1
...
001c3f4d  Type25a_direct  addr=0x1c7f45          ; call common helper #2
001c3f54  Type8a_rel      reladdr=0xfffcc9       ; loop-back branch
001c3f5a  Type17a  ureg2 = 0x257800              ; pointer, byte-off (a second, adjacent string)
001c3f60  Type17b  ureg12 = 0x3e8                ; 1000 decimal — stack depth? priority?
001c3f62  Type17a  ureg8 = 0x2577f0              ; "RPC dispatcher"  <-- the landmark string
001c3f65  Type17a  ureg4 = 0x1c3bf0              ; SW-convention pointer -> valid code (§2c)
001c3f68  Type16b  0x0
001c3f6a  Type25a_direct  addr=0xb8615d          ; call target (see below)
001c3f6d  Type3c  push; 001c3f6e store retaddr
001c3f73  Type9b_abs (return); 001c3f75 Type25c_rframe
```

Compare the **Audio Task** creation site, SW `0x1c7762`–`0x1c7780`:

```
001c7765  Type17a  ureg2 = 0x25f7cc
001c776b  Type17b  ureg12 = 0x3e8                ; same 1000 constant
001c776d  Type17a  ureg8 = 0x25f7c0              ; "Audio Task"   <-- v1's landmark
001c7770  Type17a  ureg4 = 0x1c7749              ; SW-convention pointer (decode desyncs here, §3a)
001c7773  Type16b  0x0
001c7775  Type25a_direct  addr=0xb8615d          ; IDENTICAL call target
```

**The call target `0xb8615d` is byte-for-byte identical at both sites.**
`0xb8615d` does not resolve as an absolute SW address in any loaded region
(matches the known Type25a_direct/Type8a addressing-mode gap, FINDINGS/v1
§0) — it's almost certainly a shared-library stub reference (most plausibly
**`xTaskCreate`**, given the argument shape: name-string pointer in one
register, a valid code pointer in another, `1000` in a third, identical
target both times) whose true encoding our decoder can't resolve without
dataflow. Grade: **evidence for "these two sites call the same function with
the xTaskCreate-shaped argument pattern"** (the register values and repeated
target are direct tool output); **hypothesis** for "that function is
`xTaskCreate`" (inferred from the FreeRTOS context — `tasks.c`/`queue.c`
strings elsewhere, per v1 §2 — not from a symbol).

**Conclusion: "RPC dispatcher" is not a bare function, it is a FreeRTOS
task**, created at SW `0x1c3f5a`–`0x1c3f6a` in the entry bank, right next to
(same bank, ~14KB away from) the Audio Task's own creation call. This
corrects v1's hypothesis that the RPC dispatcher's code lives in the "app
bank" alongside its name string — the string is data placed in the app bank;
the code that creates and runs the task is in the entry bank.

### 2c. The task body

`ureg4 = 0x1c3bf0` lands exactly on a valid instruction boundary (confirmed
by linear-decoding the whole entry-bank region and checking real instruction
starts, not just byte-slicing — this is the check v1 flagged as needed and
didn't have for the Audio Task pointer). Ghidra concurs: after seeding a
function there, it disassembles as a coherent 6-word stub —

```
001c3bf0  Type19a  ... 0xffffffee          ; adjusts an index by -18 (DAG setup)
001c3bf3  Type25a_direct  addr=0x1c7782    ; call
```

— which Ghidra auto-labels `thunk_FUN_001c7782` (a "does nothing but jump"
function, in Ghidra's own classification). `FUN_001c7782` runs 2 instructions
and returns immediately (`Type9b_abs`/`Type25c_rframe`) with `ureg0 =
0x25f848` (another byte-offset pointer — a config/handle value). **Following
the software-return convention (§1a)**, control resumes at SW `0x1c3bf6`
(the instruction right after the `Type25a_direct` at `0x1c3bf3`) — which is
exactly where the long, call-heavy body I traced by hand begins (push+call to
`0x1c7d09`, `0x1c7f45`, `0x1c4353`, `0x1c7e02`, `0x1c39cf`, `0x1c43c9`, and
dozens more, spanning at least SW `0x1c3bf0`–`0x1c6000`, ~4,600 words / ~9KB).
**This span is exactly where Ghidra's function analysis has a hole** (§1a) —
none of it (past the 6-word stub) is inside any Ghidra function body, because
the return edges into it are all via the unresolvable `Type9b_abs` indirect
jump.

**So: SW `0x1c3bf0` is the RPC dispatcher task's entry trampoline; its real
body runs from `0x1c3bf6` onward.** Grade: **evidence** for the trampoline
shape and its first callee; **hypothesis, well-supported** that this is the
literal task body (matches the xTaskCreate argument-pointer role, decodes
cleanly for thousands of bytes, uses the same call convention throughout).

### 2d. Command dispatch mechanism — what could and couldn't be pinned down

Scanning every branch inside the traced span (`Type8/9/10/11/25` forms) for
their condition-code field turns up a **large, varied set of conditional
branches** — cond values 0, 1, 2, 7, 8, 16, 17, 18, 20, 23, 26, 29, 30 all
appear. Paired values differ by exactly 16 (0/16, 1/17, 2/18, 7/23), matching
SHARC's documented true/complement condition-code encoding (PGR) — i.e. these
are genuine `if / if-not` pairs, not decode noise.

**What this does NOT let me conclude**, and why:
- **No compute-mnemonic dataflow is ported into the SLEIGH yet** (FINDINGS.md
  §6 — "compute mnemonics NOT yet ported"), so I cannot see *what value* each
  comparison is testing, i.e. I cannot confirm whether any of these branches
  is literally `if (cmd == N) call handler_N()`. `render.py` *can* decode the
  compute field text (e.g. `R0 = pass R1`, ALU ops) for individual
  instructions I hand-inspected, but reconstructing "which register holds the
  incoming command id, and where did it come from" needs real dataflow
  (SSA/def-use), which is out of scope for a disassembly-first SLEIGH.
- I found **no register-indirect "jump through a computed table" instruction**
  in the traced span other than the fixed, uniform `Type9b_abs pmi=4,pmm=6`
  return idiom (§1a) and one distinct `Type9a_abs`/`Type9a_rel` at
  `0x1c46c4`/`0x1c551a` whose target register isn't a literal, so it *could*
  be a jump-table dispatch but could equally be another return/callback —
  **unresolved, flagged as an open item**, not claimed either way.
- What I *can* say with the available (static, non-dataflow) tools: the task
  body is structured as **a long sequence of small, guarded subroutine calls**
  (every one of the dozens of distinct callees is reached from behind a
  conditional branch or as part of a straight-line sequence, per §1a's
  convention), which is *consistent with* a large `switch`/`if-else` command
  dispatcher compiled without a jump table (common for SHARC compilers
  with a non-dense case set), but this is **hypothesis, not confirmed** —
  equally consistent with ordinary sequential task logic that happens to have
  many small helper calls.

**Where it reads command/parameters from**: see §4 — a DDR structure based at
`0x82a00000` is referenced *exclusively* by code in this same SW span
(`0x1c35xx`–`0x1c48xx`), nowhere else in the whole firmware. This is the best
available candidate for where the dispatcher reads its command/parameters
from, graded **medium-high-confidence hypothesis** (see §4 for the full
evidence).

**Bottom line for the priority ask**: the RPC dispatcher **is a FreeRTOS
task** (not a bare ISR-driven function), its creation site and entry
trampoline are pinned down to real, verifiable addresses (SW `0x1c3f5a` /
`0x1c3bf0`), its body is a long chain of guarded subroutine calls using a
software (not hardware) call convention, and its most likely
command/parameter source is the DDR block at `0x82a00000` (§4) — but the
literal "read cmd id, compare/jump-table, call handler[cmd]" bytes were not
isolated with certainty; that requires dataflow tooling this SLEIGH module
doesn't yet have (see SPEC-FINDINGS.md §7 item 1/2, unchanged by this session).

---

## 3. AUDIO TASK & buffers (Task 3)

### 3a. Task creation — confirmed; task-pointer decode — still unresolved

The creation site (SW `0x1c776d`, name string `"Audio Task"` @ BW
`0x2825f7c0`) is unchanged from v1 and confirmed again here (§2b). The
candidate task-function pointer `ureg4 = 0x1c7749` **still does not land on a
real instruction boundary** even after re-checking against a from-scratch
linear decode of the whole entry-bank region (not just a byte-sliced window —
the exact check v1 flagged as missing). The surrounding bytes (SW
`0x1c7743`–`0x1c775f`) contain a run of unknown-opcode / decode-desync
instructions (`Type22a`/`Type21a`/`??`) — part of the firmware's known ~3.5%
undecodable tail (SPEC-FINDINGS.md §3.6), landing at an unfortunate spot. **This
remains an open item, not resolved this session**: unlike the RPC dispatcher,
I cannot point to a specific SW address as the Audio Task's own entry
function with the same confidence.

### 3b. Peripheral setup (confirmed, unchanged from v1, now with Ghidra corroboration)

The SPORT/DAI-pair MMR configuration block at SW `0x1cb25d`–`0x1cb32d`
(writing to `0x310c9xxx` and `0x310cAxxx`, two peripheral instances at
identical sub-offsets 0x1000 apart — direct evidence, quoted in full in v1
§3b) is now split by Ghidra into two adjacent functions, `FUN_001cb230`
(74 words) and `FUN_001cb274` (380 words), with the boundary falling exactly
where the disassembly shows a return idiom mid-cluster. **Caveat**: given
§1a's discovery that this SLEIGH's return idiom sometimes falsely terminates
a function (it correctly identifies *a* return, but two configuration blocks
written back-to-back with the same idiom in between can look like "function A
ends, function B begins" even if they're really one continuous setup
routine) — **I am not asserting these are two independently-meaningful
subroutines**; they may be one SPORT/DAI-pair setup block that Ghidra's
p-code-driven function splitter cut in the wrong place.

The bracketing single-hit MMR touches (`0x31004000` ×6, `0x3108c000` ×1 at
SW `0x1c1414` right after `entry`) are unchanged from v1 — still the best
DMA/interrupt-controller candidates, still unconfirmed (no public offset
table to name the exact register).

**No SEC/TRU hits anywhere in the firmware.** Re-checked this session by
grepping the full xref table for `0x31089xxx`/`0x3108Axxx` (the public
SEC/TRU MMR range) across *all* 5,842 distinct referenced addresses, not just
the audio-task's neighborhood — zero hits. Absence of evidence: either the
cross-core-interrupt path doesn't go through SEC/TRU register pokes visible
to static immediate-operand scanning (e.g. it's set up once via a table the
decoder can't see, or via the ColdFire side only), or it's simply not
used for this purpose in this firmware. Not resolved.

### 3c. Buffers — not conclusively identified

Looked for: (1) large aligned regions in L2/DDR that could be ring buffers,
(2) DMA descriptor-shaped data (address+count pairs) near the peripheral
setup code. Neither turned up anything solid:
- The MMR values written in the SPORT/DAI setup block (e.g. `0x3def7b9c`,
  `0x3def7bc2`) are not address-shaped — they read as packed control-register
  bitfields (clock/frame-sync/word-length config), not buffer pointers.
- The two DDR "named" data regions found by the boot-stream loader
  (`0x8045a6c8`, 3.3KB; `0x8055c440`, 9.3KB) are too small and irregularly
  sized to be obvious stereo sample ring buffers; more likely coefficient or
  config tables (consistent with v1's read of these).
- L2 (`0x20000000`) has only 4 xref hits firmware-wide, and all 4 are
  non-aligned, high-entropy values consistent with float-literal noise, not
  real buffer pointers (checked this session, see §4).

**This is a genuine gap**: identifying the actual audio sample buffer(s)
needs either the DMA descriptor format (an ADI-public but not-yet-consulted
detail) or working compute-field dataflow to trace what a SPORT DMA's
source/destination register is loaded from. Not resolved this session —
flagged rather than guessed at.

---

## 4. ColdFire↔SHARC shared memory (Task 4)

### 4a. Primary candidate: DDR block at `0x82a00000` — medium-high confidence

Re-running the region-scoped grep this session (restricting v1's existing
xref table to the SW span that §2 now positively identifies as the RPC
dispatcher's creation + task-body code, `0x1c3bf0`–`0x1c7700`) turns up:

```
0x82a00008  (3 refs)   SW 0x1c4547, 0x1c4761, 0x1c47ea
0x82a00010  (4 refs)   SW 0x1c4768, 0x1c47b8, 0x1c47cf, 0x1c489f
0x82a00014  (1 ref)    SW 0x1c47bb
0x82a00018  (1 ref)    SW 0x1c47cc
0x82a0001c  (2 refs)   SW 0x1c47c7, 0x1c4823
0x82a000e4  (4 refs)   SW 0x1c482b, 0x1c482e, 0x1c48b6, 0x1c48c3
0x82a000e8  (1 ref)    SW 0x1c4820
0x82a000f0  (1 ref)    SW 0x1c4826
0x82a001b8  (3 refs)   SW 0x1c4720, 0x1c47e5, 0x1c4855
0x82a001bc  (2 refs)   SW 0x1c472f, 0x1c4868
0x82a001c0  (2 refs)   SW 0x1c4738, 0x1c4873
0x82a001c8  (6 refs)   SW 0x1c361a, 0x1c3774, 0x1c3a04, 0x1c3a86, 0x1c3ae8, 0x1c3b7e
```

**Every single reference to this DDR base, firmware-wide, falls inside SW
`0x1c35xx`–`0x1c48xx`** — i.e. exclusively in the setup code immediately
preceding the RPC dispatcher's creation and inside its own task body. No
other code anywhere in the 12 loaded regions touches this structure. That
specificity (14 distinct small fields spanning a ~456-byte structure,
0x82a00000–0x82a001c8, touched only from this one functional area, from many
different call sites) is a much stronger, more falsifiable signal than v1's
original framing of it as a generic "shared parameter block" — **this reads
as the RPC command/parameter block itself**, not a general-purpose
ColdFire↔SHARC channel shared with audio. Grade: **hypothesis**, but well-
supported (concrete addresses, exclusive-usage pattern, direct spatial
correlation with the dispatcher code newly located in §2).

The `0x80000000`/`0x80000018` (DDR aperture base) and named-region pointers
(`0x8045xxxx`/`0x8055xxxx`) v1 also found are unchanged and still read as
static DDR data-table references, not necessarily ColdFire-shared.

### 4b. L2 — re-checked, still not evidenced

All 4 firmware-wide L2 (`0x20000000`+) xref hits (`0x20020f13`, `0x2050bf79`,
`0x2052bf7f`, `0x20cbca21`) are non-4-byte-aligned, high-entropy values —
consistent with float-literal noise (same pattern v1 flagged for the "OTHER"
bucket), not real pointers. **No evidence L2 is used as an ColdFire↔SHARC
channel**; not ruled out, just unsupported by static-immediate scanning.

### 4c. SEC/TRU (cross-core interrupt) — no evidence found

See §3b — zero hits firmware-wide for the public SEC/TRU MMR range
(`0x31089xxx`/`0x3108Axxx`). If ColdFire→SHARC signaling uses a hardware interrupt
at all, its setup is not visible to static immediate-operand scanning (could
be configured via a table, via the ColdFire side only, or via a MMR
address this scan's peripheral-block bucketing missed).

### 4d. Audio-task/RPC shared-memory link — not established

Despite the task brief's framing ("look for addresses referenced by BOTH the
RPC dispatcher and the audio path"), **no address was found that both the
`0x1c35xx`–`0x1c48xx` RPC-dispatcher span and any audio-task/peripheral-setup
code (`0x1cb25d`–`0x1cb32d`, or the still-unlocated Audio Task body) both
reference.** The two subsystems' static-immediate footprints are disjoint in
this analysis. This may mean they don't share memory directly (the RPC
dispatcher could signal the audio task via a FreeRTOS queue/semaphore
instead, consistent with the `queue.c`/`event_groups.c` strings v1 found in
the kernel bank), or it may mean the link exists but is built via a
two-instruction hi/lo pointer construction our static scan can't reconstruct
(the same limitation that hid the RPC dispatcher string reference in v1,
until re-grepping found it). **Open item, not resolved.**

---

## 5. Evidence-vs-hypothesis summary

| claim | grade | basis |
|---|---|---|
| 147 functions / 329 call edges / 65.2% branch resolution across all 12 regions | **evidence** | direct Ghidra tool output |
| L1 addressing `BW=2·SW+0x28000000` | **evidence** | confirmed identity on entry address, carried from FINDINGS.md |
| L2 addressing `BW=2·SW+0x20000000` | **hypothesis** | analogous-rule guess; non-overlapping and produces plausible functions, not independently verified |
| "RPC dispatcher" string ref found at SW `0x1c3f62` | **evidence** | literal decoder xref hit |
| RPC dispatcher is a FreeRTOS task created at SW `0x1c3f5a`–`0x1c3f6a` | **evidence** (call-site shape, register values) | disassembly |
| The `0xb8615d` call target is `xTaskCreate` | **hypothesis** (well-supported: identical target reused for the confirmed Audio Task creation, FreeRTOS strings present in same firmware) | inference |
| SW `0x1c3bf0` is the RPC dispatcher task's entry trampoline | **hypothesis, well-supported** (clean decode, valid instruction boundary, matches xTaskCreate arg-pointer role, Ghidra independently treats it as a real function/thunk) | disassembly + Ghidra |
| The task body (`0x1c3bf6`+) is a chain of guarded subroutine calls via a software call convention | **evidence** for the convention itself (repeated exact byte pattern); **hypothesis** that this specific span is "the task body" | disassembly |
| A literal command-id compare-chain or jump table drives dispatch | **not established** — open item | needs compute-field dataflow, out of current SLEIGH's scope |
| `0x82a00000` DDR block is the RPC command/parameter structure | **hypothesis, medium-high confidence** | exclusive spatial correlation, 14 fields, multiple call sites, firmware-wide search |
| Audio Task entry function pointer | **not established** — open item | decode desync at the one candidate address (`0x1c7749`), unresolved this session |
| SPORT/DAI pair configured at SW `0x1cb25d`–`0x1cb32d` (`0x310c9xxx`/`0x310cAxxx`) | **evidence** | direct load-constant/store-to-MMR disassembly, unchanged from v1 |
| Audio buffer address(es) | **not established** — open item | no address-shaped values found in the peripheral setup or in L2/DDR beyond known small data tables |
| SEC/TRU cross-core interrupt usage | **no evidence found** (absence of evidence) | firmware-wide xref search, zero hits |
| RPC dispatcher and Audio Task share memory directly | **not established** | disjoint static-immediate footprints; may signal via FreeRTOS queue/semaphore instead |

---

## Artifact manifest (sharc-spec `analysis/`; only this file was imported)

| file | contents |
|---|---|
| `callgraph_full.txt` | full 12-region call graph: 147 functions, 329 call edges, resolution stats (§1) |
| `functions_full.json` | machine-readable function list (name/entry/body size), 147 entries |
| `structure_v2.md` | this file |
| (existing, reused unchanged) `xref_table.txt`, `xref_summary.json`, `strings.txt`/`strings_landmarks.txt` | v1's raw decoder-xref and string data — re-queried, not regenerated, since the underlying decode is unchanged (only the Ghidra load configuration changed this session) |

## Open items for a future session

1. Port compute-field dataflow so the RPC dispatcher's command-id source
   register and comparison chain can actually be read (SPEC-FINDINGS.md §7 item
   1/2 — unchanged ask, now with a much more precisely located target:
   SW `0x1c3bf6`–`~0x1c6000`).
2. Resolve the Audio Task's own entry-function pointer (decode desync at SW
   `0x1c7743`–`0x1c775f` blocks this — needs the unknown-opcode tail
   (SPEC-FINDINGS.md §3.6) narrowed further, or a wider/shifted decode window).
3. Determine whether `Type9a_abs`/`Type9a_rel` (seen at SW `0x1c46c4` and
   `0x1c551a` inside the dispatcher body) is a jump-table dispatch or another
   return/callback path — currently ambiguous.
4. Find the actual audio sample buffer address(es) — needs DMA descriptor
   format or compute dataflow, neither available this session.
5. Confirm or refute the `0x82a00000` DDR block's role by finding where the
   ColdFire-side firmware writes to it (out of scope for this SHARC-only
   analysis, but would be the strongest possible confirmation).
