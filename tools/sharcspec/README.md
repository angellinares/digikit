# SHARC+ instruction encodings from public ADI documents

Clean-room groundwork for an open ADSP-21569 (SHARC+) toolchain, imported from
Em's sharc-spec work on 2026-09-15. Inputs are limited to documents Analog
Devices publishes on analog.com (see `docs/sharc/SOURCES.md`). No files from
any vendor toolchain installation are used.

The manuals are not committed. Put `sc58x-2158x-prm.pdf` and
`adsp-2136x_2137x_214xx_pgr_rev2.4.pdf` in the repo's `docs/refs/`
(git-ignored) and run the scripts from this directory.

| Source | Role |
|---|---|
| **PRM:** SHARC+ Core Programming Reference Rev 1.5, `sc58x-2158x-prm.pdf` | Primary. The only source for SHARC+ additions. 54 vector bit-layout figures. |
| **PGR:** SHARC Processor Programming Reference Rev 2.4 (classic core), `adsp-2136x_2137x_214xx_pgr_rev2.4.pdf` | Cross-check. Chapter 10 grid tables with binary constants. SHARC+ is documented as instruction-set compatible. |
| **Selache:** public SHARC+ encoder/decoder at `js216/selache` | Independent cross-check for selected SHARC+-only field layouts; never replaces the primary manuals. |

`tools/sharc_isa.py` is the repository's typed interface over the generated
`decode_table.json`. Both Python decoder entry points delegate form selection
to it; the generated table remains the encoding input, and SLEIGH remains a
generated Ghidra adapter rather than the source of truth.

## Scripts

```sh
uv run --with pymupdf python extract_figures.py      # PRM figures  -> figures.json
uv run --with pymupdf python classic_tables.py       # PGR tables   -> classic.json
uv run python compare_sources.py                     # bit-by-bit PRM vs PGR report
uv run python check_overlaps.py                      # patterns no fixed bit distinguishes
uv run python check_template_digits.py
uv run --with pymupdf python verify_overlay.py       # renders/sheet_NN.png visual check
```

Pattern strings run MSB first: `0`/`1` fixed, `x` field, `-` unused (yellow),
`?` white with no label (PRM), `.` blank cell (PGR).

## How the PRM figures are read

The figures are vector drawings, so text extraction alone is wrong: digits in
white cells are placeholders. `extract_figures.py` works from geometry instead:

- **Rows:** stroked rectangles of ~9 pt cells, with bit numbers printed above.
  Cell pitch is measured, since some rows are drawn a little over 9 pt × n.
- **Fixed bits:** gray fill (0.82). A fill can be a rectangle or a closed
  polygon of lines, and its digits are printed inside.
- **Unused bits:** yellow fill (0.95, 0.80, 0.19). In ch. 14–17 these are bits
  the instruction doesn't use. In ch. 18 they are bits outside the field being
  shown (e.g. the Type 2c prefix around ShortCompute).
- **Fields:** stroked segments are grouped into connected components (shared
  endpoints or T-junctions). A component that ends beside a label and touches
  cells under a row assigns the label to the cells between its outermost
  contacts. Labels can sit on either side.
- **Figures:** grouped by the following caption; figures split across pages are
  joined. Each figure's inner tag is checked against its caption.

Automated checks: every bit is accounted for exactly once, label widths match
bracket widths, and nothing is left unattached. Result: 54 figures, 0
attachment anomalies. All 54 were then checked by eye against
`renders/sheet_00..13.png`.

## Findings: the PRM figures are right about layout, not always about values

`compare_sources.py` found 12 of 49 PRM/PGR pairs identical. Most other
differences are compatible:

- the PRM shades bits the PGR leaves blank (must-be-zero / don't-care);
- the PRM draws one figure with a selector field where the PGR has two tables
  (`r` in 8a, `rel` in 9a/10a, `x` in 11a/11c);
- SHARC+ added fields in formerly fixed bits (`l`/`w`/`x` in 3b/4b marker bits,
  `sc`/`w` in 19a).

### PRM errata (conflicts with the PGR or internal contradictions)

| Figure (PRM p.) | Problem | Resolution |
|---|---|---|
| Type 2a (312) | Fixed digits `0010000000` are the Type 1a template; PGR: `000` `00001` | Use PGR values |
| Type 2b (314) | Digits `110000000` are template; PGR: `000000011` | Use PGR |
| Type 3a (318) | Figure is a copy of Type 1a (inner tag "Type1a") | Use PGR Type 3a layout |
| Type 4b (334) | Label `dreg[6:0]` over a 4-bit bracket; label typo `data[5:5` | 4 bits (matches 4a/4d, PGR) |
| Type 5b move (343), 9b (367) | VISA marker drawn `0000000`; PGR: `0111111` | Use PGR |
| Type 11c (377) | Bits 47–41 `1100000` are template; PGR: `0000101` | Use PGR |
| Type 17a (400) | Label `i[2:0]` over a 7-bit bracket at 38–32; PGR: `UREG` | `ureg[6:0]` |
| Type 17b (401) | Bits 47–39 `100100000`; PGR: `000011111` | Use PGR |
| Type 19a bitrev (408) | Bits 41–39 `000`; PGR bitrev `101` | Unresolved: 19a gained `sc`/`w` fields in SHARC+ |
| Type 25c rframe (420) | Figure is a copy of 25a rframe (inner tag), drawn 32-bit; PGR: 16-bit `0001100100000001` | Use PGR |

### SHARC+-only encodings with no second source

Types 3d, 4d, 7d, 12a (ureg), 14d, 22a, 25a (rframe), 26a (sync), and the new
fields in 3b/4b. Their digits don't match the template defaults (good
sign), but that isn't proof. **Types 7a and 7d print identical fixed bits**
(`000001001`) although they are different instructions, so at least one is
wrong or the difference lies in field values.

The former provisional `Type19p_undoc48` is not in this list. The PRM's
Type19a BH table documents `sc=01` as enhanced address scaling, and Selache
independently confirms the `0x15` form's `w/g/idis/is/data32` layout. It is now
the confident `Type19a_scaled` form; `w=1` selects `(NW)` and `w=0` selects
`(SW)`.

### Open questions

- Types 25a/25c rframe: bits 23–4 (25a) / 31–20 (25c) are white with no label,
  and the PRM text doesn't say what they hold.
- The complete VISA length rule: which remaining prefixes select 16/32/48-bit
  decoding. Short forms reuse long-form prefixes with a marker field (e.g. 1b
  = 1a with bits 22–16 = `0111111`). A public independent decoder establishes
  that first byte `0x02` selects a 48-bit immediate-shift form; its field layout
  matches the PRM Type6a no-memory ShiftImm figure. The same decoder selects
  32 bits for first byte `0x01` with bit 39 set; the table emits that as
  `Type2a_short`. A whole-image comparison with that decoder found no other
  width it gets right and this table gets wrong.
- Classic 5a/6a pattern details differ slightly between the PRM and PGR in
  blank-vs-gray bits: must-be-zero or don't-care?

## Firmware test corpus (`fw/`)

The Elektron updates are the real-code reference. In this repo, from `.syx`
to DSP memory:

```sh
uv run python -m emu.extract Digitakt_II_OS1.16.syx -o DIR   # sections, via dt2/elz.py
uv run python tools/sharcldr.py DIR/section_7_BLOB.bin       # boot stream blocks
```

- **`dt2/elz.py`** (was `fw/elz.py`): the section codec (LZ77 + interlaced
  Elias-gamma, offset bias 767, reuse code, far bonus above 3328). Written
  from the format in mischa85/elektron-firmware-tool `aplib.c` (MIT). Every
  compressed section of DT2 1.16 and DN2 1.11 ends exactly on its end marker,
  and stream lengths and byte sums match the section headers.
- **Boot stream** (`fw/bootstream.py` in sharc-spec; `tools/sharcldr.py`
  here): section 7 is an ADI-style boot stream of 16-byte LE headers {code,
  target, count, arg}, with sign 0xAD and a zero XOR over each header.
  Walking the chain fixes flag 0x0100 as "fill, no payload": it is the
  only rule that consumes the section exactly (DT2: 104 blocks / 321,016
  bytes; DN2: 95 / 836,956). 0x5000 opens a program, 0x0800 is an init call,
  and 0x8000 is final (its target is the entry point).
- **Two programs per stream:** a small init program (init call at `0x120230`,
  the same in DT2 and DN2), then the main one. Entry is SW `0x1c1338` (DT2) /
  `0x1c12e2` (DN2). `2 × SW + 0x28000000` lands exactly on a block target, which
  confirms VISA code addresses count 16-bit words in the L1 alias.

## Next: a third source for SHARC+-only encodings

1. **Real compiled code:** the Elektron DSP image (ADI boot stream of
   FreeRTOS code). Candidate encodings can be tested statistically. Wrong
   tables desynchronize VISA decoding quickly; right ones produce plausible
   control flow (calls landing on `rframe`/return sequences, `b2w`/`w2b` in
   byte-addressed code).
2. **ADI errata or EngineerZone answers** for the PRM figures.
