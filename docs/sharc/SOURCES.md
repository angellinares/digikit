# Public sources for the SHARC+ work

Every document used for the SHARC+ instruction tables in `tools/sharcspec/`
was published by Analog Devices on analog.com. No files from any vendor
toolchain installation (bundled manuals, library sources, linker files,
headers) were used, and no vendor tools were run. The PDFs are not
committed: keep local copies in `docs/refs/` (git-ignored). `tools/refstext.py`
extracts them to per-page text with each PDF's bookmarks in `out/refs/`. An earlier,
independent list of SHARC+ manual sections is in `docs/refs/sharc-plus-isa.md`.

| File | Document | Pages | Used for |
|---|---|---|---|
| `sc58x-2158x-prm.pdf` | SHARC+ Core Programming Reference, Rev 1.5 (June 2023), ADSP-SC5xx and ADSP-215xx | 798 | Instruction bit-layout figures (primary source) and computation opcode tables |
| `adsp-2136x_2137x_214xx_pgr_rev2.4.pdf` | SHARC Processor Programming Reference, Rev 2.4 (classic core) | 694 | Independent cross-check of opcode bits and computation tables |
| `adsp-21562-21563-21565-21566-21567-21569.pdf` | ADSP-2156x datasheet, Rev D | 102 | Memory map and peripheral address ranges |
| `2156x_EZKIT_Manual.pdf` | ADZS-21569-EZKIT manual | 36 | Board context only |
| `adsp-2156x-hwr.pdf` | ADSP-2156x SHARC+ Processor Hardware Reference, Rev 1.0 (December 2020) | 2331 | Boot stream block flags in `tools/sharcldr.py`; peripheral register addresses (Appendix A), DMA channel assignment (Table 27-2) and SEC ids (Table 6-5) in `tools/sharcimm.py` |
| `sharc-plus-prm.pdf` | SHARC+ Core Programming Reference, Rev 1.4 | | Page citations in `tools/sharc_visa_tables.py` |
| `ADSP-21160_isr_rev2.1.pdf` | ADSP-21160 SHARC DSP Instruction Set Reference, Rev 2.1 (April 2013) | 262 | Third opcode source; Table 1-21 records the Type 23/24/25 renumbering |
| `50836807228561adsp2106xsharcprocessorusersmanual_revision2_1.pdf` | ADSP-2106x SHARC Processor User's Manual, Rev 2.1 (March 2004) | 698 | Third opcode source; documents Type 23 (`IDLE16`) and Type 24 (`CJUMP`/`RFRAME`) |
| `3789835185494138226006565l_book_tr.pdf` | ADSP-21065L SHARC DSP Technical Reference, Rev 2.0 (July 2003) | 508 | Third opcode source; per-type opcode bit maps in its instruction-set appendix |

`dt2/elz.py`, the section decompressor, follows the format of `aplib.c` in
[mischa85/elektron-firmware-tool](https://github.com/mischa85/elektron-firmware-tool)
(MIT). It is a separate implementation; no code was copied.

## How the opcodes were taken from the manual

The SHARC+ Core Programming Reference presents each instruction format as a
bit-layout figure. The figures are vector drawings, not images: each bit is a
cell with a column number.

- Gray fill (0.82, 0.82, 0.82) marks fixed opcode bits; one rectangle covers
  each contiguous run of fixed bits.
- Yellow fill (0.95, 0.80, 0.19) marks unused bits, or bits outside the field
  being shown.
- Digits in white cells are placeholders, so text extraction alone gives wrong
  opcodes. Printed digits in gray cells are not always right either: several
  figures keep a template's default digits or copy another figure.
- Field names are attached by bracket lines drawn under bit ranges.

`tools/sharcspec/extract_figures.py` reads the fill rectangles, bit-column
positions and bracket endpoints with PyMuPDF. Every figure was then checked by
rendering it, and every opcode bit was compared against the classic core's
tables (`compare_sources.py`); the conflicts and their resolution are in
`tools/sharcspec/README.md`. Field value tables (`BH`, `BHSE`, `ACONV`,
`ALUOP`, register class codes) are ordinary text in the manual.

Example (Type 2b, 32-bit VISA): bits 47–39 = `110000000`,
bits 38–32 = `compute[22:16]`, bits 31–16 = `compute[15:0]`.

## Where things are in the SHARC+ Core Programming Reference (Rev 1.5)

PDF page numbers.

| PDF p. | Chapter | Use |
|---|---|---|
| 39 | Introduction ("Differences from Previous SHARC Processors") | SHARC+ vs classic |
| 52 | Register File Registers and Core MMRs | register model |
| 64 | Processing Elements (ALU, multiplier, shifter, 64-bit FP) | semantics |
| 105 | Program Sequencer (pipeline, VISA, loops, delayed branches, conditions) | control flow |
| 186 | Data Address Generators | addressing |
| 219 | L1 Memory Interface (byte address space) | address aliases |
| 262 | L1 Cache Controller | cache instructions |
| 301 | Instruction Set Reference: groups and notation | encodings |
| 304 | Group I: Types 1a/1b, 2a/2b/2c, 3a–3d, 4a/4b/4d, 5a/5b, 6a, 7a/7b/7d | encodings |
| 356 | Group II: Types 8a, 9a/9b, 10a, 11a/11c, 12a, 13a | encodings |
| 384 | Group III: Types 14a/14d, 15a/15b, 16a/16b, 17a/17b | encodings |
| 402 | Group IV: Types 18a, 19a, 20a, 21a/21c, 22a/22c, 25a/25c, 26a | encodings |
| 422 | Computation Opcode Reference | encodings |
| 439–530 | ALU / MR / multiplier / shifter / multifunction computations | per-op semantics and flags |
| 531 | Immediate and constant opcodes | encodings |
| 536 | Register (reg) opcodes | encodings |
| 564 | Numeric formats | semantics |
| 569–775 | Core register descriptions | core registers |
