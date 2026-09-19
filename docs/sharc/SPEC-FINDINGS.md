# SHARC+ (ADSP-21569) firmware & instruction-set findings

Consolidated results from a clean-room effort to disassemble the SHARC+ DSP
program in the Elektron Digitakt II and Digitone II firmware updates. Written to
be portable into another repository.

Imported from Em's sharc-spec work on 2026-09-15. Script paths below are
relative to `tools/sharcspec/`. The firmware extracts (`fw/out/`), analysis
dumps and Ghidra projects were not imported. The firmware analysed here is
Digitakt II OS 1.16 and Digitone II OS 1.11.

## Provenance / clean-room status

Every encoding fact here derives from **public Analog Devices documents** or from
the **firmware images themselves**. No files from any vendor toolchain
installation were used, and no vendor tools were run or disassembled.

Public sources used (all from analog.com; see `docs/sharc/SOURCES.md`):

- **PRM** — SHARC+ Core Programming Reference, Rev 1.5 (`sc58x-2158x-prm.pdf`).
  Covers ADSP-SC5xx / ADSP-215xx. Primary source; only source for SHARC+ additions.
- **PGR** — SHARC Processor Programming Reference, Rev 2.4
  (`adsp-2136x_2137x_214xx_pgr_rev2.4.pdf`). Classic core, includes 214xx (so it
  has the VISA b/c forms). Used as an independent cross-check of opcode values.
- **Datasheet** — ADSP-21562…21569, Rev D.

One **format** reference used for the section codec: `mischa85/elektron-firmware-tool`
(`aplib.c`), MIT-licensed, cited where relevant. Only the codec's format was used,
reimplemented independently.

One open-source **cross-check**, run outside this repository: `js216/selache`, an
independent SHARC+ toolchain (GPL-3.0). Its decoder was scored against the
firmware as a second opinion (§3.4, §3.5, `docs/sharc/selache-comparison.html`).
None of its code or tables are in this repository; every change it prompted
rests on the firmware measurements recorded here.

> Note: the SHARC processor module inside the local Ghidra install is NOT stock
> Ghidra (stock Ghidra has no SHARC). It is prior art generated from this
> repo's `tools/sharc_visa_tables.py` (`tools/ghidra/SHARC/`). It was deliberately not used as a source; a clean Ghidra
> module would be generated from `decode_table.json` instead.

---

## 1. Firmware transport & container

### 1.1 SysEx wrapper (in this repo: `dt2/container.py`)

- Each `.syx` is a stream of `F0 … F7` messages. Manufacturer id `00 20 3C`
  (Elektron); device id byte **0x14 = Digitakt II**, **0x15 = Digitone II**.
- Message kinds by byte 6: `0x7F` start/end markers (16 bytes), `0x7E` data.
- Data payload is **8-in-7 packed**: one leading byte holds the MSBs (MSB-first)
  of the following 7 bytes. 116 packed bytes → **101 data bytes** per message.
- A 14-bit sequence counter (`(byte8<<7)|byte9`) increments per data message.
- Each message ends with a check byte; its exact algorithm was not needed and not
  pinned down (only matters for re-emitting a `.syx`).
- The concatenated payload begins with a 4-byte big-endian total length; trailing
  bytes past that are padding.

Extracted images: `dt2_os1.16.bin` (1,484,064 B), `dn2_os1.11.bin` (1,885,184 B).

### 1.2 ELE3 container

- Magic `ELE3` near the start; build date and version strings follow.
- Section table: **count** as a big-endian u32 at file offset **0x24**, then
  **16-byte entries** at **0x28**, each `{u32 type, u32 offset, u32 size, u32 load}`
  big-endian.
- Both images have 6 sections. Observed types and roles:

  | type | load addr | contents | codec |
  |---|---|---|---|
  | 5 | — | small metadata (15 B) | raw |
  | 2 | 0x02010000 | ColdFire bootstrap | LZ |
  | 3 | 0x40000400 | ColdFire main OS | LZ |
  | 4 | 0x80000400 | updater | raw |
  | **7** | (boot stream) | **SHARC DSP program** | LZ |
  | 8 | — | shared data (identical in both machines) | LZ |

- **Codec detection:** a section is LZ-compressed iff its stream length ≤ table
  size and its byte-sum matches the stream header (below); otherwise raw. There is
  no explicit flag.

### 1.3 Section LZ codec (`fw/elz.py`)

Format per `mischa85/elektron-firmware-tool` `aplib.c` (MIT); reimplemented.

- A compressed section is `[u32 BE stream length][u32 BE byte sum]` then the
  stream, padded with zeros to a multiple of 4. **The stream starts 8 bytes after
  the section table offset** (an 8-byte gap precedes it).
- Bitstream: tag bytes read **MSB-first**; `1` = literal byte, `0` = match.
- Match: interlaced Elias-gamma `g` (data bit, then stop bit; stop = 1). `g == 2`
  reuses the last offset. Else raw = `(g<<8) + next_byte`; **raw == 767 ends the
  stream**; offset = raw − 767 (bias **767**). Two bits give a short length 1..3;
  `00` means gamma + 2 follows. Offset > **3328** adds 1 to length. Copy count =
  length + 1.
- Verification: every compressed section in both images decodes and lands exactly
  on its end marker, and stream length + byte sum match the header.

Decompressed sizes: **DT2 section 7 = 321,016 B**, **DN2 section 7 = 836,956 B**.

---

## 2. SHARC boot stream (`fw/bootstream.py`)

Section 7 is an ADI-style boot stream: a chain of 16-byte little-endian headers
`{u32 code, u32 target, u32 count, u32 arg}`.

- **Sign byte 0xAD** = top byte of `code`. **Header XOR = 0** (all 16 bytes XOR to
  zero) — used as the validity check while walking.
- Flags live in `code` bits 4..15. Assignments were derived by requiring the walk
  to consume the section exactly, then matched to ADI's published boot flags:
  - **0x0100 = FILL / no payload** (the only no-payload rule under which the walk
    ends exactly on the last byte: DT2 104 blocks / 321,016 B; DN2 95 / 836,956 B).
  - **0x5000** opens a program (ignore-style header at each program start).
  - **0x0800** = init-code call.
  - **0x8000 = FINAL**; its `target` is the entry point.
- Each stream contains **two programs**: a small **init program** (init call to
  `0x120230`, identical size in both machines) followed by the **main program**.

Entry points: **DT2 main = SW 0x1c1338**, **DN2 main = SW 0x1c12e2**.

### Address model (confirmed)

SHARC+ aliases the same physical memory at different addresses per access width.
For L1 **code**, a short-word (SW, 16-bit-unit) address maps to a byte (BW) address:

```
BW = 2 * SW + 0x28000000
```

Confirmed because `2*entry_SW + 0x28000000` lands **exactly on byte 0** of each
main program's load region (DT2 0x28382670, DN2 0x283825c4). Load regions seen:
L1 blocks at 0x2824xxxx / 0x282Cxxxx / 0x2838xxxx, L2 at 0x20000000, DDR at
0x80000000 (mostly a zero fill).

See also docs/REMAINING.md §B.4 for the same boot stream in Digitakt II
1.15C and Digitone II 1.10E.

---

## 3. Instruction encodings

### 3.1 How the table was built

- `extract_figures.py` reads the PRM's 54 vector bit-layout figures by geometry
  (gray cells = fixed opcode bits, yellow = unused, bracket lines → field labels).
  Output `figures.json`; all 54 verified against renders.
- `classic_tables.py` parses the PGR ch.10 grid tables → `classic.json`.
- `compare_sources.py` diffs them bit-by-bit.
- `build_table.py` merges into `decode_table.json`: **bit positions & field names
  from the PRM figure, fixed-bit VALUES from the PGR where it has a table** (the
  PRM figures carry errata), with explicit overrides.

### 3.2 PRM figure errata (found via the PGR cross-check)

Use the PGR value, not the PRM figure digits, for these:

| PRM figure | Problem | Fix |
|---|---|---|
| Type 2a | fixed digits are the Type 1a template | PGR values |
| Type 2b | figure `110000000` is a template after all — **see §3.4** | PGR `000000011` |
| Type 3a | figure is a copy of Type 1a (inner tag "Type1a") | PGR Type 3a |
| Type 4b | `dreg[6:0]` label over a 4-bit field; `data[5:5` typo | 4 bits |
| Type 5b move, 9b | VISA marker drawn `0000000` | PGR `0111111` |
| Type 11c, 17b | figures carry template digits | PGR |
| Type 17a | `i[2:0]` label over a 7-bit field | `ureg[6:0]` |
| Type 19a | figure draws bits 41–40 as a field `sc[1:0]` | PGR fixes them `10`; §3.9 |
| Type 19a bitrev | bits 41–39 disagree | PGR `101`; bit 39 is the bit-reverse flag, §3.9 |
| Type 25c rframe | figure is a copy of 25a rframe | PGR 16-bit form |

### 3.3 SHARC+-only forms (PRM only, no second source)

Types 3d, 4d, 7d (`b2w`/`w2b` address switch), 12a-ureg, 14d, 22a, 25a-rframe,
26a (`sync`), plus new fields in 3b/4b. Marked `unconfirmed` in the table.
Note: Types **7a and 7d print identical fixed bits** in the PRM — at least one
figure is imprecise; distinguish by field values in context.

Resolved on 2026-09-16 by a third source: the ADSP-2106x, ADSP-21065L and
ADSP-21160 manuals (`docs/sharc/SOURCES.md`). Type 7a's fixed bits are the eight
of `000 00100`, with bit 39 the `G` field, "Selects DAG1 or DAG2" — so the PRM's
ninth digit is a shaded field bit, and the table, which already declines to fix
it, is right. Type 19a's claimed new field `sc[1:0]` goes the same way; see §3.9.
Type 7d keeps bit 39 fixed at 1 and stays unconfirmed, since the classic manuals
have no Type 7d to check it against.

### 3.4 The Type 2b correction (firmware-arbitrated)

The two manuals disagree on Type 2b's prefix: PRM `110000000`, PGR `000000011`.
Initially the PGR value was used (PRM digits looked like a stale template). This
was **wrong**: `000000011` sits in the `00000…` space crowded with the 48-bit
branch forms and caused width collisions. The **PRM value `110000000` is correct**
(sits cleanly next to Type 2c). Final: Type2b mask `0xff8000000000`, value
`0xc00000000000`. Lesson: the "template digit" heuristic produced a false positive
here; the firmware is the arbiter.

**Corrected 2026-09-19: the PGR value `000000011` is right.** The collisions above
were measured under the old "most fixed bits" width rule (§3.5), which mis-sized
~28% of instructions. Under longest-leading-prefix, `000000011` (nine leading
bits) cleanly beats Type 2a's `00000001` (eight) and collides with no branch form.
With `110000000`, every Type 2 word with bit 39 set is read as 48 bits instead of
32, and every `0xC000`–`0xC07F` word as 32 bits instead of 16.

The firmware decides it, with a control that can come out either way. Every
absolute `cjump` site is a known instruction start, and so is every `cjump`
target. A linear walk between two consecutive known starts must land **exactly**
on the second. The walk can stop at a word no form matches, land exactly, or
overshoot, and only a correct length for every instruction in between lands
exactly:

| image | spans | PRM `110000000` | PGR `000000011` | + Type10a_rel (§3.5) | overshoots, any column |
|---|---|---|---|---|---|
| DT2 1.15C | 2,102 | 1,827 | 1,997 | 2,018 | 0 |
| DT2 1.16 | 2,102 | 1,826 | 1,996 | 2,017 | 0 |
| DN2 1.10E | 2,043 | 1,718 | 1,949 | 1,979 | 0 |
| DN2 1.11 | 2,030 | 1,712 | 1,935 | 1,966 | 0 |

The rest of each row stops at a word no form matches; none overshoots. So the
PGR value lands 170–231 more spans per image and breaks none that landed before.
`tools/sharcpcode.py compare` on the regenerated language: 0 regressions; aligned
instructions that decode 21,792 → 22,825 (DT2 1.16) and 21,361 → 22,678 (DN2 1.11).

Two independent checks agree with the firmware. An open-source SHARC+ assembler
(`js216/selache`) encodes `r1 = r0 + r1` as `0x0180 0x1101`, a 32-bit Type 2b
with bit 39 set (`tests/test_sharc_disasm.py`, `test_assembled_sequence`). Its
decoder sizes Type 2 by the same bit. Where our old table and that decoder
disagreed on Type 2a/2b and the walk could tell them apart, it sided with bit 39
in 119 of 119 cases. Final: Type2b mask `0xff8000000000`, value `0x018000000000`.

### 3.5 VISA length (width) selection

- There is **no explicit length-prefix table** in the PRM. The only statement is
  the IAB section: "a decode of the instruction indicates the length." So width is
  determined by matching the opcode as a **prefix code**.
- Correct rule (in `sharc_decode.py`): among forms whose (mask & frame) == value,
  pick the one with the **longest leading run of fixed bits from bit 47 down**
  (longest-leading-prefix wins), tie-break by total fixed bits. The earlier
  "most total fixed bits anywhere" heuristic was wrong and mis-sized ~28%.
- **Type 10a is ISA-only** ("not supported in VISA address space", PRM) — exclude
  it from VISA candidates. Marked `visa:false`.
  **Amended 2026-09-19:** `Type10a_rel` (bits 47–45 `111`) is `visa:true` on
  firmware evidence (`build_table.py` `VISA_BY_FIRMWARE`). No VISA form claims
  `111`, so these words used to stop the walk. Reading them as 48 bits lands
  21–31 more spans per image exactly (the §3.4 table), with no overshoot.
  `Type10a_abs` (`110`) stays excluded: there it competes with Type 2c
  (`1100`). Read as 48-bit 10a_abs or as 16-bit 2c, the `1101` words land the
  same number of extra spans on DN2 1.11 (six either way), so the walk cannot
  choose. selache's decoder reads all of `110x` as 16-bit Type 2c.
- Frames are 48-bit MSB-aligned; a 16-bit form occupies bits 47..32, a 32-bit form
  47..16. In memory, code is 16-bit little-endian words, most-significant word
  first for multi-word instructions.

### 3.6 The undocumented 16-bit instruction (`Type23p_undoc16`)

- ADI's public PRM **skips Type 23 and Type 24** entirely.
- The firmware contains a heavily-used **16-bit** instruction the manual never
  documents. The word **`0x023e` occurs 329× in DT2** and is 49% of all unknowns.
- It belongs to a family sharing **top-7 bits `0000001`** with a flat 9-bit
  operand field (62% of all unknowns fall in this family). Behavioural signature:
  followed ~50% by Type21a (nop) and ~23% by Type5a-swap — an ~11–34× enrichment
  over base rates. Sits at real instruction boundaries (both address parities).
- Added provisionally as **`Type23p_undoc16`**: 16-bit, mask `0xfe0000000000`,
  value `0x020000000000` (top-7 `0000001`), 9-bit operand, `unconfirmed`.
  Prefix width tuned against the firmware: top-6 gains nothing, top-8 drops real
  members (e.g. `0x0300`); **top-7 is the sweet spot**.
- Name and semantics are **unknown**. Sources to identify it: infer from
  behaviour, or ADI errata.
- One open boundary case: the Ghidra prior-art module sizes `0x0300` as 48-bit
  where our provisional form makes it 16-bit. Branch alignment did not regress, so
  the 16-bit call holds for now, but the exact extent of the family is not final.

### 3.7 Type 21a is the whole word, not a prefix

- The PRM prints a value for every bit of Type 21a (Figure 17-5, p.413: 48
  zero bits) and Type 21c (Figure 17-6: `0x0001`). The classic PGR grid leaves
  those bits blank, and `build_table.py` read a blank as "any value", so
  `Type21a` matched any first word `0x0000`-`0x007f` and swallowed the one or
  two short instructions after it: **904 matches in Digitakt II 1.16, of which
  only 44 (4.9%) are the all-zero word** (Digitone II 1.11: 31 of 898).
- `FULL_WORD` in `build_table.py` now takes every bit from the figure for these
  two forms: Type21a `0xffffffffffff`/`0`, Type21c `0xffff00000000`/
  `0x000100000000`.
- The left-over first words become **`Type21p_undoc16`**, provisional in the
  same sense as Type23p_undoc16: top nine bits zero, a 7-bit operand, 16 bits
  long, no name and no semantics. 883 in 1.16, 969 in 1.11. Its commonest
  first words are `0x0032`, `0x001c`, `0x0008`, `0x0030`, `0x0010`. It is
  followed by itself 18% of the time, then by `2a`, `23p_undoc16` and `3a` —
  not the signature 3.6 records for Type23p_undoc16, so the two are probably
  unrelated despite the shared construction.
- In the SLEIGH module the crossing pattern with Type22c moves from Type21a to
  Type21p_undoc16 (bit 32), and Type21c stops crossing anything.
- Open: what the instruction is. The first-word values cluster, which is a
  lead; the manuals document nothing in this range beyond `0x0001`.

### 3.8 The same rule over the rest of the table

- `tools/sharcspec/audit_bits.py` lists, per form, the bits the PRM figure
  prints that the table does not fix. Ranked by how many: Type22a 38,
  Type26a 32, Type20a 28, Type3d and Type4d 16, Type2a 12, Type13a 11,
  Type11a 9.
- Only the idle-class forms are wrong. Type22a (idle/emuidle, Figure 17-7,
  p.414) prints every bit but `emu`, and Type26a (`sync`) prints every bit.
  Neither strict word occurs in either image, while their nine- and
  sixteen-bit prefixes took 220 and 2 instructions. Both are now tightened,
  and the words they took become `Type22p_undoc48` and `Type26p_undoc48`:
  48 bits, the width the loose forms always read them at.
- Restoring the dropped bits on the compute forms destroys real matches:
  Type2a loses all 1,089 of its instances in Digitakt II 1.16 and the aligned
  instruction count falls 22%, Type13a goes to zero, Type11a to two. Those
  PRM digits are the stale template values the merge rule exists to ignore,
  so it stands for them. Type3d, Type4d and Type20a change nothing
  measurable either way.
- A 16-bit reading of Type22a's leftovers was tried first and rejected: it
  stranded the two words after each one, split the RPC dispatcher into two
  functions and added 33 truncated functions in 1.16.
- Open: what the Type22p and Type26p words are. Their length is what the
  decoder always read; nothing else about them is confirmed.

### 3.9 Type 19a took two bits too many, and `Type19p_undoc48` has them

- Both classic manuals fix Type 19's bits 44–40 at `10110` and make bit 39 the
  bit-reverse flag, `0` for `MODIFY` and `1` for `BITREV` (ADSP-21065L
  `all.txt` 3617-3670; ADSP-21160 ISR `all.txt` 5723-5745). The PRM figure
  shades only bits 47–42 and draws bits 41–40 as a SHARC+ field `sc[1:0]` and
  bit 39 as `w`, so the merge rule — a PRM field wins over a PGR value — left
  `Type19a` matching on six bits where `Type18a` and `Type20a` match on eight.
- On six bits it claimed the whole `000101` block. Its tighter neighbours took
  what they own and `Type19a` kept the rest: its own `10110` with bit 39 clear,
  **and all of `10101`, which no ADI manual documents**. Bits 44–40 run Type 18
  `10100`, the gap, Type 19 `10110`, Type 20 `10111` — in the PGR Rev 2.4 and in
  the ADSP-21160, ADSP-21065L and ADSP-2106x manuals alike.
- `tools/sharcfields.py`, new, is the mirror of `audit_bits.py`: it tallies the
  values the table's declared fields actually take across the aligned
  instructions of both images and flags a field sitting on bits the classic grid
  fixes. The joint value of `sc[1:0]` and `w` is `10 0` for 1,178 instructions,
  `01 1` for 748 and `01 0` for 9.
- `DROP_FIELDS` in `build_table.py` now drops Type19a's `sc[1:0]` and `w`
  declarations, so the classic values at bits 41–39 are used and `Type19a` fixes
  nine bits, `000101100`. The 757 words in the gap become
  **`Type19p_undoc48`**: 48 bits, prefix `00010101`, Type19a's remaining field
  layout with bit 39 an unknown `u`. 410 in 1.16, 347 in 1.11.
- The override lives in `build_table.py`, not in `figures.json`, which
  `extract_figures.py` regenerates from the PDF.
- Measured, `out/sharcpcode/t23` against `t24`: **0 regressions**. Aligned and
  decoded counts, function counts and size histograms, functions truncated at
  bad data, probe verdicts and every Error and warning bookmark count are
  identical in both images, and `19a` plus `19p_undoc48` sums exactly to the old
  `19a` — 969 to 559 + 410, and 966 to 619 + 347.
- The provisional names in 3.6, 3.7, 3.8 and here read `21p`, `22p`, `23p`,
  `26p`, `19p` after the form whose loose prefix used to take the words. They
  are not claims about ADI's numbering: Types 23 and 24 are now known to be
  `IDLE16` and `CJUMP`/`RFRAME` (`docs/FINDINGS.md`).
- Open: what `10101` is. Bit 39 is set in 748 of the 757, which is the one thing
  arguing it is a distinct instruction rather than a wider Type 19 — if `w` were
  still the bit-reverse flag then nearly all of them would be `BITREV`, and the
  documented bit-reverse form occurs 8 times in both images combined.

### 3.10 The branch figures' gap digits, and `Type8p_undoc48`

- `audit_bits.py` reports `Type8a_abs`/`Type8a_rel` dropping seven PRM bits
  (32–27 and 25), `Type9a_*` one (23) and `Type9b_*` two (25, 23). Split forms
  take their fixed bits from one classic table only — `fixed_bits_for` never
  looked at the PRM pattern at all — so every bit that table blanks was dropped
  unconditionally.
- Restoring them all is wrong, and measurably: **225 aligned instructions lost
  in 1.16 and 279 in 1.11**, and `Type8a_abs` falls from 29 matches to 6. The
  PRM's digits in those gaps are the same stale template values as on the
  compute forms (§3.8).
- Per bit over both images, only bit 25 on the Type8a pair discriminates: set in
  13 of `8a_rel`'s 1,382 and 19 of `8a_abs`'s 54, with 30 of those 32 carrying a
  target that is not a plausible address. Bit 23 on Type9a is a field in use —
  set in 18 of `Type9a_abs`'s 98 and 14 of `Type9a_rel`'s 34. `RESTORE_PRM_GAP`
  in `build_table.py` names the bits, per form; measure before adding one.
- Excluding the bit-25 words without rehoming them still regressed (decoded
  instructions −50 and −71, `halt_baddata` 296 → 298): an undecoded word strands
  the alignment sweep. **`Type8p_undoc48`** takes them instead — bits 47–41 =
  `0000011` with bit 25 set, one prefix covering both halves since bit 40 is
  Type8a's own abs/rel selector, kept as the field `r`. `undocumented_form`
  grew an `extra_fixed` entry to express a fixed bit outside the top prefix.
- Measured: 0 regressions against both `t23` and `t24`; aligned counts identical
  to baseline; `8a_rel` + `8a_abs` + `8p_undoc48` = the old `8a_rel` + `8a_abs`
  in both images.
- Open: what the Type8p words are. 32 across two images, several byte-identical
  in both, so they are shared code rather than misalignment.

---

## 4. Decoder validation

`sharc_decode.py` (table-driven) over the extracted main programs:

| Metric | Digitakt II | Digitone II |
|---|---|---|
| Instructions decoded | 22,399 | 22,149 |
| Unknown | **1.13%** (252) | **1.30%** (287) |
| Branch-target alignment | **99.70%** (327/328) | — |

- **Branch alignment is the key metric:** 99.70% of branch/call targets land
  exactly on decoded instruction starts, i.e. the decoder stays in sync with the
  real instruction stream. A mis-sizing decoder would scatter targets mid-instruction.
- The two independently-compiled images produce near-identical instruction
  distributions and unknown rates — strong cross-image confirmation.
- Remaining unknowns are a **diffuse tail** with no dominant member: `0xffff`
  (likely padding), and small families (e.g. DN2 `0xe1e8`/`0xe5e8`/`0xe6e8` share
  nibble `1110`). No single next win comparable to the `0x023e` family.

### Cross-check vs prior art

Against an independent SHARC disassembler (the user's prior art), before the
length fix: **96.1% instruction-type agreement where lengths agreed** — the type
tables are compatible; the divergence was width selection, since fixed by the
longest-leading-prefix rule and the Type 2b correction.

---

## 5. Compute-field decode & readable disassembly

`compute_table.json` (built by `compute_tables.py`) captures the 23-bit compute
field's internal structure and mnemonic tables, from PRM ch.18 cross-checked
against PGR ch.12:

- Top level: single-function `mf[22]=0` → `cu[21:20]` (00 ALU / 01 Multiplier /
  10 Shifter), `opcode[19:12]`, `rn[11:8]`, `rx[7:4]`, `ry[3:0]`; multifunction
  `mf[22]=1` → `opcode[21:16]` + register subfields in [15:0] (2-bit inputs select
  within fixed register quads); ShortCompute (Type 2c) and ShiftImm (Type 6a)
  variants.
- Captured: 61 ALU + 25 multiplier + 25 shifter + 27 multifunction + 16
  ShortCompute ops, MOD1/2/3 modifiers, MR data move. Conflicts vs PGR flagged in
  the JSON (`conflict: true`), not silently resolved.

`render.py` turns a decoded instruction into readable text (`render_compute`,
`render_operands`, `disassemble`). Validated on the firmware:

- Compute fields render to real mnemonics **~78% raw / ~85%** excluding zero-fill
  padding and the architecturally-reserved `cu=11` slot; the remainder are genuine
  table gaps (opcodes absent from the manual).
- Register operands always in range; plausible DSP mix (mul / add-sub / compare /
  shift / move / MAC), none implausibly dominant; every hand-checked opcode matched.
- The init program disassembles as coherent code (load literal → move to ureg →
  call → save/restore DAG regs → `R4 = pass R0` return-value move).

## 6. Ghidra SLEIGH module (`ghidra/`)

- Stock Ghidra ships **no** SHARC module; the one in the install is prior art
  (untouched here). We generate our own, clean-room, from the tables.
- `ghidra/gen_sleigh.py` emits `ghidra/SHARC_VISA/` (a full language module:
  `sharc_visa.slaspec/.ldefs/.pspec/.cspec`, `Module.manifest`). 55 constructors
  from `decode_table.json`, registers R0-15/F0-15/I0-15/M0-15/L0-15/B0-15/PC.
- **Frame↔word mapping** (the crux): frame bit `b` → token word `(47-b)//16`,
  in-word bit `b-(32-16*word)`; each of word0/word1/word2 is a little-endian
  16-bit token chained with `;` (MS word first).
- **Width** resolves via SLEIGH's most-specific-match, matching our
  longest-leading-prefix rule. Three genuine "crossing" pattern conflicts
  (7b/7d, 21a/22c, 22a/22c) were resolved with intersection constructors per the
  SLEIGH manual.
- **Addressing:** `ram` space, `wordsize=2`, `alignment=2` (a `wordsize=2` space
  requires alignment=2 or Ghidra silently disassembles nothing). Loaded at byte
  offset `BW_load − 0x28000000`; Ghidra displays the SW word address directly, so
  `addr`/`reladdr` fields need no scaling as branch targets.
- **Scope: disassembly-first.** Real p-code only for control flow (Type8 jump/call,
  Type25 direct/pcrel goto, Type11/25 return) so Ghidra builds functions + xrefs;
  other forms have empty p-code. Compute mnemonics are NOT yet ported into SLEIGH
  (operands print as raw hex); ureg/sreg names not yet resolved.
- **Verified:** 100.00% instruction-boundary agreement vs our decoder over the
  whole DT2 main program (22,399/22,399) and init region (2,317/2,317); branch
  targets land 327/328 = 99.70%.
- Load via pyghidra (`GHIDRA_INSTALL_DIR`, Python 3.13 venv with pyghidra+jpype1);
  module installed as a NEW `Processors/SHARC_VISA` dir, not overwriting the
  existing SHARC dir.

---

## 6b. File manifest (`tools/sharcspec/`)

Encoding pipeline:
- `extract_figures.py` → `figures.json` (PRM figures, by geometry)
- `classic_tables.py` → `classic.json` (PGR ch.10 tables)
- `compare_sources.py` (PRM vs PGR bit diff)
- `build_table.py` → `decode_table.json` (merged table + errata + provisional form)
- `check_overlaps.py`, `check_template_digits.py`, `verify_overlay.py` (audits)
- `sharc_decode.py` (table-driven VISA/ISA decoder)
- `README.md` (methodology + errata); the documents are listed in `docs/sharc/SOURCES.md`

Firmware pipeline, as imported into this repo:
- `.syx` → container: `dt2/container.py`
- section depack: `dt2/elz.py` (was `fw/elz.py`; used by `emu/extract.py`)
- boot stream walk: `tools/sharcldr.py` (in place of `fw/bootstream.py`)
- `fw/validate.py` (linear sweep and recursive trace over a program region)

## 7. Firmware structure (Digitakt II OS 1.16, from Ghidra analysis)

Loaded all L1/L2 code regions into one Ghidra program (SHARC_VISA module) and
ran function/call-graph analysis. Findings, graded evidence vs hypothesis:

- **It is a FreeRTOS application** for the SHARC+ (source paths `tasks.c`,
  `queue.c`, `port.c` under a `freertos-sharc` port path; ADI drivers
  gpio/pcg/spu; a `digitakt_rompler_update.c` sampler unit). *Evidence.*
- **147 functions / 329 call edges** from the entry (SW 0x1c1338). Branch
  resolution ~65% loaded-all-regions (92.7% single-region) — lowered by data
  regions in the denominator. *Evidence.*
- **Software-emulated call convention** (found this session): the firmware does
  NOT use native SHARC call — it pushes a return address via I7/M7, does an
  unconditional jump-as-call, and returns via indirect-jump + rframe. This
  fragments Ghidra's native call graph and is the main thing to model next.
  *Evidence.*
- **RPC dispatcher** = a FreeRTOS **task** (string @ BW 0x282577f0; created near
  SW 0x1c3f62; trampoline at SW 0x1c3bf0; ~4,600-word body). This is the
  **ColdFire→SHARC command interface**. The exact dispatch (jump table vs compare
  chain) is not yet pinned — needs compute-field dataflow. *Evidence (task) +
  hypothesis (dispatch shape).*
- **Command/parameter block** = a DDR structure at **0x82a00000** (~456 bytes, 14
  fields); every firmware-wide reference to it falls inside the RPC dispatcher's
  code span — the most likely ColdFire→SHARC command block. *Strong hypothesis.*
- **Audio Task** (string @ BW 0x2825f7c0; `xTaskCreate` at SW 0x1c776d). Its
  SPORT/DAI setup is at SW 0x1cb25d–0x1cb32d (MMR `0x310c9xxx`/`0x310cAxxx`, two
  instances 0x1000 apart). Its task-entry pointer (~0x1c7749) doesn't cleanly
  decode yet. Audio buffers not yet identified. *Evidence (task, peripherals) +
  open (entry, buffers).*

Artifacts: `docs/sharc/structure-1.16.md` (the call-graph and function lists were not imported).

## 8. Open items / next steps

1. **Port compute mnemonics into the SLEIGH** so Ghidra shows `R0=R1+R2` etc.
   instead of raw hex; reassemble split addr/data/compute fields; resolve
   ureg/sreg names. (`render.py` already does all this — it's the reference.)
2. **Add arithmetic p-code** to non-control-flow constructors for decompilation
   (currently disassembly-first: only branches carry p-code).
3. **Analyse the firmware in Ghidra** — auto-analysis for functions/call graph,
   then locate the audio task, ColdFire↔SHARC communication, and hook points.
4. Identify Type 23/24 semantics (`0x023e` family) — behaviour, errata.
5. Resolve the `0x023e` vs `0x0300` family-extent boundary; chase the ~1% tail.
6. Confirm the SHARC+-only encodings (§3.3) against firmware usage.
7. Fill compute-table gaps (~14% of compute opcodes absent from the manual tables).
