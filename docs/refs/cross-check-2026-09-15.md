# An independent cross-check of the SHARC+ VISA encodings — 2026-09-15

Notes from a day spent transcribing the same PRM independently, offered because
`FINDINGS` records losing 88–93% of instruction walks to a single misread
figure, and a second reader is the cheapest possible check on that class of
error.

Nothing here changes any code. It is ordered by what might change what you do
next, and the last section is the part most likely to save you time.

Measured against **SHARC+ Core Programming Reference, Part Number
82-100131-01, Revision 1.5**, 798 pages,
`sha256 a3edf83beb75b44a77f8366cf54c0353083ab49a1c69d8179d2b727ba7ba1470`.

---

## 1. We are probably not reading the same revision

`docs/refs/sharc-plus-isa.md` records the copy in use as **771 pages**, from
`docs.ampnuts.ru`, instruction set beginning at printed page **12-2**, and adds
that it has *"not been checksummed against Analog Devices' own copies"*.

Ours is **798 pages**, instruction set in **chapters 14–15**. Chapters were
inserted ahead of it, so this is a substantive revision rather than a reprint.

**A disagreement between tables drawn from different revisions is not evidence
that either is wrong.** Worth settling before comparing anything below. The
hash above identifies ours exactly.

## 2. The figures are vector drawings, so the shading is data

> *"Read the Type5b_move figure again, at 400 DPI this time."*

No DPI is needed. Across the whole instruction chapter — pages 300–420 in our
revision, 121 pages — the PDF contains:

```
pages carrying a raster image : 0
raster images in total        : 0
```

Every bit cell is a **vector rectangle with a fill colour** in the content
stream; every bit number and value is real text at a known position. They can
be read exactly and reproducibly, with no OCR and no vision model.

### Why that figure in particular invites the error

**A shaded run is ONE rectangle spanning several cells, not one per cell.** The
seven shaded cells at bits 22–16 of Type5b_move are a single 63-point box:

```
x0=316.50  w=63.00  h=9.00  fill=(0.8235, 0.8235, 0.8235)
```

Count rectangles and you get **2** where the answer is **8**. Converting each
rectangle's width back into a cell count (`width / 9`) makes it unreadable any
other way. This is the file's structure inviting the mistake, not carelessness.

### Our read of Type5b_move, for comparison

```
bit   47 46 45 44 43 42 41 40 39 38 37 36 35 34 33 32
value  0  1  1  1  0  0  0  0  0  0  0  0  0  0  0  0
fixed  #  #  #  #  #  .  .  .  .  .  .  .  .  .  .  .

bit   31 30 29 28 27 26 25 24 23 22 21 20 19 18 17 16
fixed  .  #  .  .  .  .  .  .  .  #  #  #  #  #  #  #
```

**The eight are bit 30 and bits 22–16.** Fields: `srcureghigh[4:0]` = 42..38,
`cond[4:0]` = 37..33, `srcureglow[1:1]` = 32, `srcureglow[0:0]` = 31,
`dstureg[6:0]` = 29..23.

*In fairness:* we knew the number **eight** from `FINDINGS` before counting, so
the count is not a blind confirmation. The positions are ours, and the other 52
figures were read without opening `sharc_visa_tables.py`.

## 3. Three things that bite a decoder

### Decode order — 9 subsumption pairs

A pattern with few fixed bits **subsumes** stricter patterns nested inside its
space. Tried first, the loose one wins and swallows the strict one's
instructions; because the forms differ in length, the walk desynchronises from
there. Same failure class as the Type5b bug, generalised.

Same-mode pairs, loose → strict:

| loose | fixed bits | swallows | fixed bits |
|---|---|---|---|
| `Type1a` | 3 | `Type2a` | 20 |
| `Type4a` | 4 | `Type4d` | 8 |
| `Type6a_mem` | 4 | `Type6a_nomem` | 16 |
| `Type19a` | 6 | `Type18a`, `Type20a` | 9, 9 |
| `Type14d` | 7 | `Type25a_rframe` | 24, 16 |
| `Type25a_rframe` 32-bit | 16 | `Type25a_rframe` 48-bit | 24 |

Every subsumed pattern has to be tried before the one that subsumes it.

### ISA and VISA have to be partitioned before any prefix analysis

Of five apparent top-7 length ambiguities, **four dissolve** once the modes are
separated: they are `a`/`b` pairs — the same instruction in both encodings —
which never compete for the same bytes (`7a = ISA/VISA`, `7b = VISA`).

### The PRM contradicts itself at page 419

Figure captioned **`Type25a_rframe`**; section heading reads **`Type 25c VISA
(rframe)`**. Anything keyed on one disagrees with anything keyed on the other.

## 4. Notation traps in the compute tables

All five families extract, but each is written differently:

| family | field | patterns | values admitted |
|---|---|---|---|
| ALUOP | bits 19–12 | 61 | 61 / 256 |
| MULOP | bits 19–12 | 25 | 206 / 256 |
| SHIFTOP | bits 19–12 | 25 | 25 / 256 |
| SHIFTIMM | bits 21–16 | 18 | 18 / 64 |
| DUALADDSUB | bits 19–16 | 2 | 2 / 16 |

- **The cells are masked patterns, not values**: `ALUOP 00011000`,
  `MULOP 0000 F00x`, `SHIFTOP 11 0 0 __ __`. Letters and underscores are
  variable bits. A `^[01]+$` filter takes ALUOP and silently drops the rest.
- **Table 18-9 holds two encodings side by side**, headed `shiftimm (bits
  21-16)` and `shiftop (bits 19-12)` — matching the literal word `opcode` finds
  neither.
- **The dash in "bits 19−12" is U+2212 MINUS SIGN**, not a hyphen or en dash.

## 5. The part most likely to save you time

We went looking for the constraint that would make a figures-only table decode,
and measured three candidates. **All three are dead ends**, and two of them are
dead ends in the document rather than in anyone's implementation.

**The register classes are not a constraint.** Table 2-1 (pages 53–55) gives
class membership by *name* (`RREG = r0 - r15`) and **nothing in the PRM maps a
register name to the code that selects it**. Worse, cardinality rejects
nothing: **15 of the 20 classes exactly fill their field width** — `RREG` is 16
registers in 4 bits, `RFREG` 32 in 5, `I1REG` 8 in 3. `UREG` is worse still,
since the PRM says it *"includes almost all processor core registers"*, so
7-bit `ureg` fields are close to fully populated. Only `SYSREG` (18 of 32) and
the `UREGXDAG` variants (6 of 8) reject anything at all.

**The condition codes are not a constraint either.** Table 4-20 (pages
156–157) defines 24 mnemonics for a 5-bit field — 75% populated — and again
gives **no encodings**. The per-type "opcode field values (cond)" tables carry
exactly two rows each across all 19 pages that have one: `11111` for the
unconditional form and `-----` for "any condition". They say which syntax a
value selects, never which values exist.

**The compute-field check does not help.** Validating against ALUOP and MULOP
leaves our median decode run at 1 and *lowers* the mean from 3.48 to 2.77.

The conclusion we draw — and would value being wrong about — is that **the PRM
alone is not sufficient to build a decoder**, because the register and
condition encodings are not published in it. They belong to the assembler.

## 6. Our negative measurements, which support your choice of metric

**Match rate is an entropy measurement, not coverage:**

| input | match rate |
|---|---|
| SHARC code `0x20000000` | 63.66% |
| SHARC code `0x283825c4` | 63.32% |
| **ColdFire MAIN OS — a different architecture entirely** | **53.07%** |
| **random bytes** | **46.65%** |
| all-zero bytes | 99.99% |

53% on an instruction set it has nothing to do with. This is why **branch
alignment is the right metric** — 92–99% against 0.4–3.6% is roughly a 30×
separation, where match rate gives 1.2×. We reached that by walking into the
trap you had already avoided.

**And our patterns carry no positional information at all:**

| region | stride 2 | stride 4 | stride 6 | stride 8 |
|---|---|---|---|---|
| `0x283825c4` | 54.1% | 55.4% | 54.1% | 54.4% |
| `0x20000000` | 50.1% | 53.3% | 53.7% | 53.7% |

At a 6-byte stride across all three phases: **54.1%, 54.0%, 54.5%**. A real
instruction stream has a preferred phase. Median decode run is **1**, against
the **632** `FINDINGS` reports after the Type5b fix.

## 7. Two things that agreed with you independently

- **The address rule.** `byte = 2 × short_word + 0x28000000` here matches
  `load = exec × 2 + 0x28000000` derived separately in our repo. The DN2 boot
  stream's declared entry `exec 0x001c12e2` maps to `load 0x283825c4`, exactly
  the base of a code region — predicted first, then confirmed.
- **Recovery needs a physical MIDI DIN interface, not USB.** We learned that as
  an ~80% flash stall we first blamed on our own image.

---

Scripts behind all of this are in
<https://github.com/angellinares/dn2_firmware_explore> (AGPL-3.0-or-later), and
the extracted tables are committed as JSON there. Happy to re-run any of it
against a different revision, or to hand the data over in whatever shape is
useful.

The two things we would most value a second opinion on are the Type5b bit
positions in §2 and the subsumption list in §3.
