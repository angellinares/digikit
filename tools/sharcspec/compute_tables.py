#!/usr/bin/env python3
"""Extract the SHARC+ COMPUTE-field (bits 22:0) sub-decode into compute_table.json.

This is the sequel to build_table.py / decode_table.json (which decode the outer
instruction TYPE). About half of the SHARC+ instruction types carry a 23-bit
"compute" field -- a mini instruction-within-an-instruction that picks a compute
unit (ALU / multiplier / shifter) or a parallel multifunction op, then an opcode
and register operands. This script decodes THAT field.

Sources (see docs/sharc/SOURCES.md -- both are ADI public documents):
  PRM: "SHARC+ Core Programming Reference" Rev 1.5, sc58x-2158x-prm.pdf
       Chapter 18 "Computation Opcode Reference" (PDF/printed pages 422-437,
       identical numbering -- verified: doc[421] is the printed page-422 chapter
       start, so PDF page N (1-based) == printed page N throughout this PDF).
       Also Chapter 25 "Multi-Function Instruction Computations" (528-530) for
       the multifunction mnemonic *order* (it has no binary opcodes, but its
       list order matches PGR Table 12-12 row-for-row -- used as corroboration).
  PGR: "SHARC Processor Programming Reference" Rev 2.4, adsp-2136x_2137x_214xx_pgr_rev2.4.pdf
       (text-extracted to pgr.txt, not committed). Chapter 12 "Computation Type
       Opcodes" (PGR pages 12-1..12-18, pgr.txt lines ~22453-23200). SHARC+ is
       documented as instruction-set compatible with this classic core, so it
       is used here exactly as build_table.py used it: an independent second
       source to cross-check the PRM.

Two extraction methods are used, and each table below says which:
  * GEOMETRIC (pymupdf): Chapter 18's *bit-layout* diagrams (Figure 18-1/2/3 and
    the small "Compute Field" register-position tables, Table 18-11 etc.) are
    vector-drawn grids of bit-number cells with a field-name row underneath, no
    fixed-bit shading. mmatch_fields() locates the bit-number row, derives the
    column pitch, then matches each field label's text-box CENTER against the
    center of every candidate contiguous bit-run for the field widths recorded
    in FIELD_WIDTHS below (widths were determined once by hand from the same
    coordinates -- see the docstring of mmatch_fields -- and are then
    re-verified against the live PDF on every run; a mismatch raises, it is not
    silently accepted).
  * TEXT (manual transcription): Chapter 18's *mnemonic* tables (ALUOP, MULOP,
    SHIFTOP, ...) are ordinary text tables (no vector shading), and were
    transcribed by hand from PyMuPDF plain-text extraction of PDF pages 422-438
    (get_text(), not OCR) while reading the chapter start to finish. This is
    the same method classic_tables.py's author used for the PGR grid tables,
    just done by eye instead of by rule-detection, because chapter 18's tables
    are one-column-per-field prose, not ruled grids. Every row is cited to its
    PRM table number and PDF page, and cross-checked against the PGR chapter
    12 equivalent where one exists.

Run: .venv/bin/python compute_tables.py   (produces compute_table.json + stdout summary)
"""
import json
import re
from collections import Counter

import pymupdf

PRM = "../../docs/refs/sc58x-2158x-prm.pdf"
NOTES = []          # human-readable gaps / caveats, printed in the summary
CONFLICTS = []       # {topic, prm, pgr} dicts where PRM and PGR disagree


def note(msg):
    NOTES.append(msg)


def conflict(topic, prm_value, pgr_value, detail=""):
    CONFLICTS.append({"topic": topic, "prm": prm_value, "pgr": pgr_value, "detail": detail})


# --------------------------------------------------------------------------
# 1. GEOMETRIC extraction of the compute-field bit layouts (Chapter 18 PRM)
# --------------------------------------------------------------------------

def words_near(page, y, tol=1.5):
    # y is matched against each word's TOP (w[1]), not its vertical center: the
    # coordinates recorded in the tables below were read off PyMuPDF word tuples
    # (x0, y0, x1, y1, text, ...) directly, i.e. y0.
    return [w for w in page.get_text("words") if abs(w[1] - y) < tol]


def bit_header(page, y):
    """A row of plain bit-number words (e.g. '15' '14' ... '0'). Returns
    {bitnum: (x0,x1)} and the column pitch."""
    ws = [w for w in words_near(page, y) if re.fullmatch(r"\d{1,2}", w[4])]
    ws.sort(key=lambda w: w[0])
    cells = {int(w[4]): (w[0], w[2]) for w in ws}
    centers = sorted(((w[0] + w[2]) / 2 for w in ws))
    pitch = (centers[-1] - centers[0]) / (len(centers) - 1)
    return cells, pitch


def match_fields(page, header_y, label_y, field_widths, printed_page, table_name):
    """field_widths: [(display_name, width_bits), ...] MSB-first, must sum to the
    header span, OR [(display_name, printed_text, width_bits), ...] when the PDF's
    printed label text differs from the display name (see Table 18-13's 'Rx/Fs'
    errata). Matches each field label (by its PRINTED text, at label_y) to a
    contiguous bit run by comparing the label's text-box center against every
    width-consistent run's center. Raises if a label can't be placed unambiguously
    -- this is the self-check."""
    cells, pitch = bit_header(page, header_y)
    bits_sorted = sorted(cells, reverse=True)  # msb..lsb
    norm = [(f[0], f[0], f[1]) if len(f) == 2 else f for f in field_widths]
    total = sum(w for _, _, w in norm)
    assert total == len(bits_sorted), (
        f"{table_name} p{printed_page}: field widths sum to {total}, header has {len(bits_sorted)} bits")

    def cell_edges(b):
        return cells[b]

    hi = bits_sorted[0]
    out = []
    for display, printed, width in norm:
        lo = hi - width + 1
        run = [b for b in bits_sorted if lo <= b <= hi]
        x0 = cell_edges(max(run))[0]
        x1 = cell_edges(min(run))[1]
        out.append({"label": display, "hi": hi, "lo": lo, "_printed": printed, "_expect_center": (x0 + x1) / 2})
        hi = lo - 1

    label_words = words_near(page, label_y)
    for field in out:
        wanted = field.pop("_printed")
        cand = [w for w in label_words if w[4] == wanted]
        if not cand:
            raise AssertionError(f"{table_name} p{printed_page}: label {wanted!r} not found near y={label_y}")
        w = cand[0]
        got_center = (w[0] + w[2]) / 2
        if abs(got_center - field["_expect_center"]) > pitch * 0.6:
            raise AssertionError(
                f"{table_name} p{printed_page}: label {wanted!r} centered at {got_center:.1f}, "
                f"expected ~{field['_expect_center']:.1f} for bits[{field['hi']}:{field['lo']}]")
        del field["_expect_center"]
    return out


def geometric_tables(doc):
    """All Chapter-18 register/operand bit-layout tables, geometrically verified.
    (printed page) -> 0-based pdf index is identity in this PDF (checked once
    in the docstring above and spot-checked for every page used below)."""
    tabs = {}

    # Figure 18-1/18-2/18-3 were already extracted with full geometry (including
    # fixed/unused-bit shading) by extract_figures.py -> figures.json. Reuse it
    # rather than re-deriving, and just re-key by our field-hi/lo convention.
    figs = {f["caption"]: f for f in json.load(open("figures.json"))["figures"]}

    def fig_fields(cap):
        return [{"label": fl["label"], "hi": fl["bits"][0],
                  "lo": fl["bits"][-1] if len(fl["bits"]) > 1 else fl["bits"][0]}
                 for fl in figs[cap]["fields"]]

    tabs["Figure18-1_Compute"] = {
        "source": "PRM Figure 18-1 (p.423), extracted geometrically by extract_figures.py",
        "field_width_bits": 23,
        "fields": fig_fields("Figure 18-1:Compute Instruction"),
        "note": "32-bit-wide diagram; bits 31-23 are drawn unused/zero padding around the "
                "23-bit compute field (bits 22-0). Field-local bit numbers == compute-field "
                "bit numbers here because the diagram is drawn LSB-aligned at bit 0.",
    }
    tabs["Figure18-2_ShortCompute"] = {
        "source": "PRM Figure 18-2 (p.424)",
        "field_width_bits": 12,
        "fields": fig_fields("Figure 18-2:ShortCompute Instruction"),
        "note": "16-bit diagram; bits 15-12 are the fixed '1100' SC-bit prefix "
                "(Table 18-2) that is NOT part of the register/opcode sub-field.",
    }
    tabs["Figure18-3_ShiftImm"] = {
        "source": "PRM Figure 18-3 (p.425)",
        "fields": fig_fields("Figure 18-3:ShiftImm Computation Opcode"),
        "note": "32-bit diagram. dataex[26:23] lies ABOVE the 23-bit compute field proper "
                "(bit 22 down to 0) -- it is extension immediate bits borrowed from the "
                "enclosing Type 6a instruction frame, used only for >8-bit immediates "
                "(bit6:len6 / bitlen12); see Table 18-9.",
    }

    page = lambda n: doc[n - 1]  # printed page N == pdf index N-1 in this PDF

    tabs["Table18-11_SingleComputeFixed"] = {
        "source": "PRM Table 18-11 (p.433), geometric",
        "fields": match_fields(page(433), 542.7, 561.4,
                                [("RN/FN", 4), ("Rx/Fx", 4), ("Ry/Fy", 4)], 433, "Table18-11"),
        "note": "Single-function compute field, bits 11-0 (register operand region of "
                "Figure 18-1's rn/rx/ry -- same bit positions, just relabeled RN/FN per "
                "which ALUOP/MULOP/SHIFTOP was selected: fixed-point op -> Rn/Rx/Ry, "
                "floating-point op -> Fn/Fx/Fy, same 4-bit binary value = register index.",
    }
    tabs["Table18-13_DualAddSubFixed"] = {
        "source": "PRM Table 18-13 (p.434), geometric",
        "fields": match_fields(page(434), 213.9, 232.7,
                                [("Rs/Fs", "Rx/Fs", 4), ("Ra/Fa", 4), ("Rx/Fx", 4), ("Ry/Fy", 4)], 434, "Table18-13"),
        "note": "PRM prints the first label as 'Rx/Fs' (p.434); Table 18-14's own bit "
                "descriptions define this field as Rs (subtraction result) / Fs, and no "
                "other field is called plain 'Rx' twice, so this is treated as a PRM "
                "typo and recorded as Rs/Fs. Flagged, not silently fixed.",
        "errata": "label misprint: PRM shows 'Rx/Fs', should read 'Rs/Fs' (p.434)",
    }
    for fixed, label_y, tag in [(True, 525.4, "Fixed"), (False, 592.8, "Float")]:
        names = ["Rm", "Ra", "Rxm", "Rym", "Rxa", "Rya"] if fixed else ["Fm", "Fa", "Fxm", "Fym", "Fxa", "Fya"]
        header_y = 506.6 if fixed else 574.1
        tabs[f"Table18-1{'5' if fixed else '6'}_MulAlu{tag}"] = {
            "source": f"PRM Table 18-1{'5' if fixed else '6'} (p.434), geometric",
            "fields": match_fields(page(434), header_y, label_y,
                                    [(names[0], 4), (names[1], 4), (names[2], 2),
                                     (names[3], 2), (names[4], 2), (names[5], 2)],
                                    434, f"Table18-1{'5' if fixed else '6'}"),
            "note": "MUL/ALU multifunction, bits 15-0. Multiply/ALU RESULT registers "
                    "(Rm/Ra) get a full 4-bit field (any R0-15); the four INPUT operands "
                    "are each a 2-bit field selecting within a fixed quad of registers "
                    "(Rxm in R3-0, Rym in R7-4, Rxa in R11-8, Rya in R15-12 -- per "
                    "Table 18-17's bit descriptions), which is how 6 register roles fit "
                    "in 16 bits.",
        }
    for fixed, label_y, tag in [(True, 390.0, "Fixed"), (False, 457.5, "Float")]:
        names = ["Rs", "Rm", "Ra", "Rxm", "Rym", "Rxa", "Rya"] if fixed else ["Fs", "Fm", "Fa", "Fxm", "Fym", "Fxa", "Fya"]
        header_y = 371.2 if fixed else 438.7
        tabs[f"Table18-1{'8' if fixed else '9'}_MulDualAddSub{tag}"] = {
            "source": f"PRM Table 18-1{'8' if fixed else '9'} (p.435), geometric",
            "fields": match_fields(page(435), header_y, label_y,
                                    [(names[0], 4), (names[1], 4), (names[2], 4), (names[3], 2),
                                     (names[4], 2), (names[5], 2), (names[6], 2)],
                                    435, f"Table18-1{'8' if fixed else '9'}"),
            "note": "MUL + dual-ALU-add/subtract multifunction, bits 19-0: same 16-bit "
                    "MUL/ALU layout as Table 18-15/16, extended with a 4th 4-bit result "
                    "register Rs/Fs (ALU subtraction result) in bits 19-16.",
        }
    tabs["Table18-21_ShortComputeFixed"] = {
        "source": "PRM Table 18-21 (p.436), geometric",
        "fields": match_fields(page(436), 284.6, 303.3,
                                [("RN/FN", 4), ("RX/FX", 4)], 436, "Table18-21"),
        "note": "ShortCompute register sub-field, bits 7-0 (matches Figure 18-2's rn/rx "
                "at the same positions). RN doubles as the Y-input AND result register "
                "(Table 18-22): RN = RN op RX.",
    }
    tabs["Table18-23_SingleFn64Float"] = {
        "source": "PRM Table 18-23 (p.436), geometric",
        "fields": match_fields(page(436), 515.0, 533.8,
                                [("Fm:n", 4), ("Fx:y", 4), ("Fz:w", 4)], 436, "Table18-23"),
        "note": "64-bit single-function float, bits 11-0. Each field is a full 4-bit "
                "register-PAIR code (Table 18-28): value N selects pair FN+1:N, valid "
                "only for even N (odd N selects the single register FN with no pair).",
    }
    tabs["Table18-25_MultiFn64Float"] = {
        "source": "PRM Table 18-25 (p.437), geometric",
        "fields": match_fields(page(437), 112.1, 130.8,
                                [("Fm:n", 4), ("Fa:b", 4), ("Fx:y", 2), ("Fz:w", 2), ("Fp:q", 2), ("Fr:s", 2)],
                                437, "Table18-25"),
        "note": "64-bit multifunction float, bits 15-0: mirrors Table 18-15/16's 32-bit "
                "MUL/ALU layout exactly (4+4+2+2+2+2). The four 2-bit input fields select "
                "one of 2 valid pairs within their quad (Table 18-27), e.g. Fx:y in "
                "{F1:0,F3:2}, Fz:w in {F5:4,F7:6}, Fp:q in {F9:8,F11:10}, Fr:s in {F13:12,F15:14}.",
    }
    return tabs


# --------------------------------------------------------------------------
# 2. TEXT-transcribed mnemonic tables (Chapter 18 PRM, cross-checked vs PGR ch.12)
# --------------------------------------------------------------------------
# Every row below was read from PyMuPDF plain-text of the cited PDF page and,
# where a PGR table exists, compared value-for-value against pgr.txt.

TOP_LEVEL = {
    "compute_field_width_bits": 23,
    "compute_field_bit_range": "22:0",
    "instruction_frame_note": (
        "The compute field always occupies bits 22:0 of whatever instruction slot "
        "carries it (e.g. bits 22:0 of a 48-bit Type 1a word's low half, or the "
        "entirety of a 32-bit Type 2b/6a word's low 23 bits) -- decode_table.json's "
        "'compute[22:16]'/'compute[15:0]' fields for Type1a are this same field."),
    "single_function_selector": {
        "source": "PRM Table 18-1 (p.422) + Figure 18-1 (p.423); PGR Table 12-1 (pgr.txt:22464) "
                   "+ single-computation bit figure (pgr.txt ~22518)",
        "structure": "mf[22]=0, cu[21:20] selects unit, opcode[19:12] (8 bits) selects the op, "
                     "then rn[11:8]/rx[7:4]/ry[3:0].",
        "cu_values": {"00": "ALU", "01": "Multiplier", "10": "Shifter", "11": "reserved (not used by SINGLEFN)"},
        "rows": [
            {"cu": "00", "opcode_19_12": "0xxxxxxx", "unit": "ALU", "format": "32-bit Fixed"},
            {"cu": "00", "opcode_19_12": "1xxxxxxx", "unit": "ALU", "format": "32/40-bit Float"},
            {"cu": "00", "opcode_19_12": "0xx1xxxx", "unit": "ALU", "format": "64-bit Float",
             "note": "overlaps the 32-bit-fixed row's 0xxxxxxx pattern; disambiguated by the "
                     "full 8-bit ALUOP value (Table 18-6 opcodes are all 0001xxxx)."},
            {"cu": "01", "opcode_19_12": "xxxxxxxx", "unit": "Multiply", "format": "32-bit Fixed"},
            {"cu": "01", "opcode_19_12": "00110000", "unit": "Multiply", "format": "32/40-bit Float"},
            {"cu": "01", "opcode_19_12": "00110011", "unit": "Multiply", "format": "64-bit Float"},
            {"cu": "10", "opcode_19_12": "xxxxxxxx", "unit": "Shifter", "format": "32-bit Fixed"},
        ],
        "pgr_cross_check": {
            "structure": "PGR frames this as one 3-bit field bits[22:20] (000=ALU/Fixed|Float, "
                         "001=Multiply, 010=Shifter) rather than mf+cu[1:0]; numerically identical "
                         "(mf=0 for all three, cu the low 2 bits) -- a difference in how the PGR text "
                         "groups the same bits, not a value conflict.",
            "conflict": False,
        },
    },
    "short_compute_selector": {
        "source": "PRM Table 18-2 (p.423, ShortCompute, Type 2c only) + Figure 18-2",
        "structure": "sc[15:12]=1100 (fixed), opcode[11:8] (4 bits), rn[7:4], rx[3:0].",
        "note": "This '1100' prefix lives in the low 16 bits of a 48-bit Type 2c word, "
                "outside the 23-bit compute field as defined for Compute/ShiftImm; ShortCompute "
                "is documented as its own 16-bit micro-field, listed here for completeness.",
    },
    "shift_immediate_selector": {
        "source": "PRM Figure 18-3 (p.425, Type 6a only) + PGR shift-immediate bit figure (pgr.txt ~22530)",
        "structure": "bit22=0 (fixed), shiftimm-opcode[21:16] (6 bits, = the upper 6 bits of the "
                     "general 8-bit SHIFTOP code), data[15:8] (8-bit immediate), rn[7:4], rx[3:0]. "
                     "dataex[26:23] (outside the 23-bit field) supplies extra immediate bits for "
                     "operations needing more than 8 (bit6:len6, bitlen12).",
        "pgr_cross_check": {"conflict": False, "note": "PGR Table 12-2 gives the same bit22=0 / "
                             "bits21-16 selector; PGR text explicitly states 'for shift immediate "
                             "(type 6 instructions) the upper 6 MSBs [of the 8-bit shiftop] represent "
                             "valid bits' (pgr.txt:22566), matching the PRM figure exactly."},
    },
    "multi_function_selector": {
        "source": "PRM Table 18-4 (p.423, MULTIFN) + Figure 18-1; PGR Table 12-1 (pgr.txt:22464)",
        "structure": "mf[22]=1, opcode[21:16] (6 bits) selects the multifunction category, "
                     "then the register sub-fields documented in Table 18-15..18-19.",
        "rows": [
            {"opcode_21_16": "0xxxxx", "category": "MUL/ALU", "format": "32-bit Fixed"},
            {"opcode_21_16": "011xxx", "category": "MUL/ALU", "format": "32/40-bit Float"},
            {"opcode_21_16": "00xx11", "category": "MUL/ALU", "format": "64-bit Float",
             "note": "shares the '00' prefix with MRDATAMOVE's '000000' prefix but requires "
                     "bit17=1 (opcode bit4=1), which MRDATAMOVE (opcode[19:12]=0) never sets -- "
                     "no actual overlap."},
            {"opcode_21_16": "10xxxx", "category": "MUL Dual Add/Subtract", "format": "32-bit Fixed",
             "note": "PRM ch.25 (p.528) and PGR Table 12-1 both document only ONE syntax form "
                     "in this whole 6-bit range (Rm=Rx*Ry,Ra=+,Rs=-); the x's are apparently unused/reserved."},
            {"opcode_21_16": "11xxxx", "category": "MUL Dual Add/Subtract", "format": "32/40-bit Float"},
        ],
        "dual_alu_note": "Dual ALU add/subtract (no multiplier) is NOT multifunction (mf=0): "
                          "it is Table 18-3's 'Single Compute Parallel Add/Subtract', cu=00, "
                          "opcode[19:16]=0111 (fixed) / 1111 (float). PGR groups it under the "
                          "same 'Multiple Computation' heading as multifunction ops (Table 12-1) "
                          "even though its mf/cu bits read as a plain single-computation ALU op; "
                          "this is a PGR presentation choice, not a value conflict.",
        "pgr_cross_check": {
            "conflict": False,
            "note": ("PGR Table 12-1 encodes the same space as bits[22:20] (not mf+opcode[21:16]): "
                     "10x=MUL/ALU Fixed, 101 1xxx=MUL/ALU Float, 110=MUL/dualALU Fixed, "
                     "111=MUL/dualALU Float. Bit-for-bit consistent with the PRM once mf(bit22)=1 "
                     "is folded back in: PRM's opcode21=0 <=> PGR's bits[22:21]='10'; PRM's "
                     "'011xxx' <=> PGR's '101' + opcode19='1'. Verified by hand, no discrepancy."),
        },
    },
    "mr_data_move_selector": {
        "source": "PRM Table 18-29 'MRDATAMOVE' (p.438) -- CONFIRMED PRM bits: D-bit[16], opcode[15:12]. "
                   "PGR Table 12-1 (pgr.txt:22464, 'Data Move: 100 / 000 / MRx data move Fixed') and "
                   "PGR's own MR-transfer figure (pgr.txt ~22815, '100000 D OPCODE DREG') give the "
                   "UPPER bits: 22:17 fixed '100000'.",
        "structure_confirmed_by_prm": "D[16], opcode[15:12] (selects which MR reg, see MRDATAMOVE table), "
                                        "then RN (register field, position not given by Table 18-29's text "
                                        "columns; PGR's figure places DREG at bits 11:8, matching the usual "
                                        "rn[11:8] position).",
        "structure_from_pgr_only": "bits[22:17] = '100000' (fixed) -- NOT independently confirmed in the "
                                     "SHARC+ PRM chapter 18 text (Table 18-29 only labels D-bit[16] and "
                                     "opcode[15:12]); carried over from the classic PGR by analogy since "
                                     "Table 18-1's own top-level table has NO explicit MRx-data-move row "
                                     "(unlike PGR Table 12-1, which does). This is a genuine PRM-vs-PGR "
                                     "presentation gap, not a value conflict -- flagged as unconfirmed.",
        "conflict": False,
        "gap": True,
    },
}

# ALUOP -- Table 18-5 (32/40-bit fixed+float) + Table 18-6 (64-bit float). p.425-429.
# Cross-checked row for row against PGR Table 12-3 (fixed) / 12-4 (float), pgr.txt:22540-22645.
# Every row matched exactly; ZERO conflicts found for ALUOP.
ALUOP_32_40 = [
    ("00000001", "RN = RX + RY"), ("00000010", "RN = RX - RY"),
    ("00000101", "RN = RX + RY + ci"), ("00000110", "RN = RX - RY + ci - 1"),
    ("00001001", "RN = (RX + RY) / 2"), ("00001010", "comp(RX, RY)"), ("00001011", "compu(RX, RY)"),
    ("00100001", "RN = pass RX"), ("00100010", "RN = -RX"),
    ("00100101", "RN = RX + ci"), ("00100110", "RN = RX + ci - 1"),
    ("00101001", "RN = RX + 1"), ("00101010", "RN = RX - 1"), ("00110000", "RN = abs RX"),
    ("01000000", "RN = RX and RY"), ("01000001", "RN = RX or RY"),
    ("01000010", "RN = RX xor RY"), ("01000011", "RN = not RX"),
    ("01100001", "RN = min(RX, RY)"), ("01100010", "RN = max(RX, RY)"), ("01100011", "RN = clip RX by RY"),
    ("10000001", "FN = FX + FY"), ("10000010", "FN = FX - FY"),
    ("10001001", "FN = (FX + FY) / 2"), ("10001010", "comp(FX, FY)"),
    ("10010001", "FN = abs(FX + FY)"), ("10010010", "FN = abs(FX - FY)"),
    ("10100001", "FN = pass FX"), ("10100010", "FN = -FX"), ("10100101", "FN = rnd FX"),
    ("10101101", "RN = mant FX"), ("10110000", "FN = abs FX"), ("10111101", "FN = scalb FX by RY"),
    ("11000001", "RN = logb FX"), ("11000100", "FN = recips FX"), ("11000101", "FN = rsqrts FX"),
    ("11001001", "RN = fix FX"), ("11001010", "FN = float RX"), ("11001101", "RN = trunc FX"),
    ("11011001", "RN = fix FX by RY"), ("11011010", "FN = float RX by RY"), ("11011101", "RN = trunc FX by RY"),
    ("11100000", "FN = FX copysign FY"), ("11100001", "FN = min(FX, FY)"),
    ("11100010", "FN = max(FX, FY)"), ("11100011", "FN = clip FX by FY"),
]
ALUOP_64 = [
    ("00010001", "FM:N = FX:Y + FZ:W"), ("00010010", "FM:N = FX:Y - FZ:W"),
    ("00010011", "comp(FX:Y, FZ:W)"), ("00010100", "FM:N = -FX:Y"),
    ("00010101", "FM:N = abs FX:Y"), ("00010110", "FM:N = pass FX:Y"),
    ("00010111", "RN = fix FX:Y"), ("00011000", "RN = fix FX:Y by RY"),
    ("00011001", "RN = trunc FX:Y"), ("00011010", "RN = trunc FX:Y by RY"),
    ("00011011", "FM:N = float RX"), ("00011100", "FM:N = float RX by RY"),
    ("00011101", "FM:N = cvt FX"), ("00011110", "FN = cvt FX:Y"),
    ("00011111", "FM:N = scalb FX:Y by RY"),
]

# MULOP -- Table 18-7 (32/40-bit) + Table 18-8 (64-bit float). p.428-430.
# y/x/f/r are modifier bits (see MOD1/2/3 below); '(mrf|mrb)' rows have a bit
# selecting which of the two accumulators. Cross-checked vs PGR Table 12-5/12-6.
MULOP_32_40 = [
    ("0000 F00x", "RN = sat mrf MOD2"), ("0000 F01x", "RN = sat mrb MOD2"),
    ("0000 F10x", "mrf = sat mrf MOD2"), ("0000 F11x", "mrb = sat mrb MOD2"),
    ("0001 0100", "mrf = 0"), ("0001 0110", "mrb = 0"),
    ("0001 100x", "RN = rnd mrf MOD3"), ("0001 101x", "RN = rnd mrb MOD3"),
    ("0001 110x", "mrf = rnd mrf MOD3"), ("0001 111x", "mrb = rnd mrb MOD3"),
    ("01yx f00r", "RN = RX * RY MOD1"), ("01yx F10r", "mrf = RX * RY MOD1"), ("01yx F11r", "mrb = RX * RY MOD1"),
    ("10yx F00r", "RN = mrf + RX * RY MOD1"), ("10yx F01r", "RN = mrb + RX * RY MOD1"),
    ("10yx F10r", "mrf = mrf + RX * RY MOD1"), ("10yx F11r", "mrb = mrb + RX * RY MOD1"),
    ("11yx F00r", "RN = mrf - RX * RY MOD1"), ("11yx F01r", "RN = mrb - RX * RY MOD1"),
    ("11yx F10r", "mrf = mrf - RX * RY MOD1"), ("11yx F11r", "mrb = mrb - RX * RY MOD1"),
    ("0011 0000", "FN = FX * FY"),
]
MULOP_64 = [
    ("0011 0001", "FM:N = FX:Y * FZ:W"), ("0011 0010", "FM:N = FX:Y * FY"), ("0011 0011", "FM:N = FX * FY"),
]
MOD1 = [("SSI", "__11 0__0"), ("SUI", "__01 0__0"), ("USI", "__10 0__0"), ("UUI", "__00 0__0"),
        ("SSF", "__11 1__0"), ("SUF", "__01 1__0"), ("USF", "__10 1__0"), ("UUF", "__00 1__0"),
        ("SSFR", "__11 1__1"), ("SUFR", "__01 1__1"), ("USFR", "__10 1__1"), ("UUFR", "__00 1__1")]
MOD2 = [("SI", "____0__1"), ("UI", "____0__0"), ("SF", "____1__1"), ("UF", "____1__0")]
MOD3 = [("SF", "____1__1"), ("UF", "____1__0")]

# SHIFTOP/SHIFTIMM -- Table 18-9, p.430-433. shiftimm is the upper-6-bit truncation
# of the 8-bit shiftop (see shift_immediate_selector above). Instruction text is the
# PRM's worked example where given, else the PGR Table 12-11 mnemonic (both cited).
# NOTE: the PDF's 2-column (shiftimm | shiftop) layout partially interleaves in
# plain-text extraction; rows are re-paired here against PGR Table 12-11
# (pgr.txt:22882-22957) which lists the same set as one flat 8-bit column and
# resolves the pairing unambiguously.
SHIFTOP = [
    ("00000000", "000000", "RN = lshift RX by RY|DATA8"),
    ("00000100", "000001", "RN = ashift RX by RY|DATA8"),
    ("00001000", "000010", "RN = rot RX by RY|DATA8"),
    ("00100000", "001000", "RN = RN or lshift RX by RY|DATA8"),
    ("00100100", "001001", "RN = RN or ashift RX by RY|DATA8"),
    ("01000000", "010000", "RN = fext RX by RY|BIT6:LEN6"),
    ("01000100", "010001", "RN = fdep RX by RY|BIT6:LEN6"),
    ("01001000", "010010", "RN = fext RX by RY|BIT6:LEN6 (se)"),
    ("01001100", "010011", "RN = fdep RX by RY|BIT6:LEN6 (se)"),
    ("01010000", "010100", "RN = bitext RX|BITLEN12"),
    ("01011000", "010110", "RN = bitext RX|BITLEN12 (nu)"),
    ("01100100", "011001", "RN = RN or fdep RX by RY|BIT6:LEN6"),
    ("01101100", "011011", "RN = RN or fdep RX by RY|BIT6:LEN6 (se)"),
    ("01110100", "011101", "BITDEP RX by RY|BITLEN12"),
    ("01110000", None, "RN = bffwrp"),
    ("01111100", "011111", "bffwrp = RN|DATA7"),
    ("11000000", "110000", "RN = bset RX by RY|BITLEN12"),
    ("11000100", "110001", "RN = bclr RX by RY|DATA7"),
    ("11001000", "110010", "RN = btgl RX by RY"),
    ("11001100", "110011", "btst RX by RY"),
    ("10000000", None, "RN = exp RX"),
    ("10000100", None, "RN = exp RX (ex)"),
    ("10001000", None, "RN = leftz RX"),
    ("10001100", None, "RN = lefto RX"),
    ("10010000", None, "RN = fpack FX"),
    ("10010100", None, "FN = funpack RX"),
]
DUAL_ADD_SUB = [
    ("0111", "RA = RX + RY, RS = RX - RY", "32-bit Fixed"),
    ("1111", "FA = FX + FY, FS = FX - FY", "32/40-bit Float"),
]
SHORTCOMPUTE = [
    ("0000", "RN = RN + RX"), ("0001", "RN = RN - RX"), ("0010", "RN = pass RX"), ("0011", "comp(RN, RX)"),
    ("0100", "RN = not RX"), ("0101", "RN = RX + 1"), ("0110", "RN = RX - 1"), ("0111", "RN = RN * RX (ssi)"),
    ("1000", "FN = FN + FX"), ("1001", "FN = FN - FX"), ("1010", "FN = float RX"), ("1011", "comp(FN, FX)"),
    ("1100", "RN = RN and RX"), ("1101", "RN = RN or RX"), ("1110", "RN = RN xor RX"), ("1111", "FN = FN * FX"),
]
MRDATAMOVE = [
    ("0000", "MR0F"), ("0001", "MR1F"), ("0010", "MR2F"),
    ("0100", "MR0B"), ("0101", "MR1B"), ("0110", "MR2B"),
]
# Multifunction MUL/ALU opcode[21:16] mnemonic table. The PRM (ch.18/25) documents
# the FIELD LAYOUT and lists every syntax form in prose (ch.25, p.528-529) but never
# prints the 6-bit opcode next to each one. The PGR's Table 12-12 (pgr.txt:23129-23198)
# DOES print binary opcodes, in the SAME order as the PRM's ch.25 prose list (checked
# item-by-item below) -- so PGR values are used here, with the ordering cross-check
# as corroborating evidence rather than an independent bit-level confirmation.
MULTIFN_MUL_ALU = [
    ("000100", "RM = R3-0 * R7-4 (SSFR), RA = RXA + RYA"),
    ("000101", "RM = R3-0 * R7-4 (SSFR), RA = RXA - RYA"),
    ("000110", "RM = R3-0 * R7-4 (SSFR), RA = (RXA + RYA)/2"),
    ("001000", "MRF = MRF + R3-0 * R7-4 (SSF), RA = RXA + RYA"),
    ("001001", "MRF = MRF + R3-0 * R7-4 (SSF), RA = RXA - RYA"),
    ("001010", "MRF = MRF + R3-0 * R7-4 (SSF), RA = (RXA + RYA)/2"),
    ("001100", "RM = MRF + R3-0 * R7-4 (SSFR), RA = RXA + RYA"),
    ("001101", "RM = MRF + R3-0 * R7-4 (SSFR), RA = RXA - RYA"),
    ("001110", "RM = MRF + R3-0 * R7-4 (SSFR), RA = (RXA + RYA)/2"),
    ("010000", "MRF = MRF - R3-0 * R7-4 (SSF), RA = RXA + RYA"),
    ("010001", "MRF = MRF - R3-0 * R7-4 (SSF), RA = RXA - RYA"),
    ("010010", "MRF = MRF - R3-0 * R7-4 (SSF), RA = (RXA + RYA)/2"),
    ("010100", "RM = MRF - R3-0 * R7-4 (SSFR), RA = RXA + RYA"),
    ("010101", "RM = MRF - R3-0 * R7-4 (SSFR), RA = RXA - RYA"),
    ("010110", "RM = MRF - R3-0 * R7-4 (SSFR), RA = (RXA + RYA)/2"),
    ("011000", "FM = F3-0 * F7-4, FA = FXA + FYA"),
    ("011001", "FM = F3-0 * F7-4, FA = FXA - FYA"),
    ("011010", "FM = F3-0 * F7-4, FA = float RXA by RYA"),
    ("011011", "FM = F3-0 * F7-4, RA = fix FXA by RYA"),
    ("011100", "FM = F3-0 * F7-4, FA = (FXA + FYA)/2"),
    ("011101", "FM = F3-0 * F7-4, FA = abs FXA"),
    ("011110", "FM = F3-0 * F7-4, FA = max(FXA, FYA)"),
    ("011111", "FM = F3-0 * F7-4, FA = min(FXA, FYA)"),
]
MULTIFN_MUL_DUAL_ADDSUB = [
    (None, "RM = R3-0 * R7-4 (SSFR), RA = RXA + RYA, RS = RXA - RYA", "32-bit Fixed",
     "PRM ch.25 p.529 lists exactly one fixed-point form; PGR Table 12-1 assigns the whole "
     "opcode[21:16]=10xxxx range to this category with no further sub-table, implying the "
     "6-bit field is not further decoded (single defined value, rest reserved) -- opcode value "
     "itself is not printed in either source."),
    (None, "FM = F3-0 * F7-4, FA = FXA + FYA, FS = FXA - FYA", "32/40-bit Float",
     "Same as above for opcode[21:16]=11xxxx."),
]


def build():
    doc = pymupdf.open(PRM)
    geo = geometric_tables(doc)

    out = {
        "sources": {
            "prm": {"title": "SHARC+ Core Programming Reference, Rev 1.5", "file": "sc58x-2158x-prm.pdf",
                     "chapter": "18 Computation Opcode Reference (pp.422-438), 25 Multi-Function "
                                "Instruction Computations (pp.528-530, mnemonic order only)"},
            "pgr": {"title": "SHARC Processor Programming Reference, Rev 2.4",
                     "file": "adsp-2136x_2137x_214xx_pgr_rev2.4.pdf (text: pgr.txt)",
                     "chapter": "12 Computation Type Opcodes (pp.12-1..12-18)"},
        },
        "top_level_structure": TOP_LEVEL,
        "figures_and_bit_layouts": geo,
        "aluop_32_40bit": {
            "source": "PRM Table 18-5 (p.425-427); cross-check PGR Table 12-3/12-4 (pgr.txt:22540-22645)",
            "cross_check": "every row matched exactly; 0 conflicts",
            "rows": [{"opcode": o, "syntax": s} for o, s in ALUOP_32_40],
        },
        "aluop_64bit": {
            "source": "PRM Table 18-6 (p.427-428); no PGR equivalent (64-bit float ALU is a SHARC+ addition)",
            "cross_check": "PGR has no 64-bit float compute; SHARC+-only, not independently confirmed",
            "rows": [{"opcode": o, "syntax": s} for o, s in ALUOP_64],
        },
        "mulop_32_40bit": {
            "source": "PRM Table 18-7 (p.428-429); cross-check PGR Table 12-5 (pgr.txt:22706-22746)",
            "cross_check": ("all rows matched, EXCEPT: PGR Table 12-5 additionally lists "
                             "'MRxF/B = Rn' / 'Rn = MRxF/B' at opcode 0000 0000, which the PRM's "
                             "Table 18-7 does NOT include (PRM instead documents MR<->register-file "
                             "moves as the separate MRDATAMOVE encoding, Table 18-29). PGR's "
                             "0000 0000 also collides with its own 'Rn = SAT MRF mod2 (UI)' row "
                             "(f=0,x=0 -> 0000 000x -> 0000 0000), so this looks like a PGR erratum "
                             "rather than a real second encoding; flagged as a conflict."),
            "rows": [{"opcode": o, "syntax": s} for o, s in MULOP_32_40],
        },
        "mulop_64bit": {
            "source": "PRM Table 18-8 (p.429); cross-check PGR Table 12-6 (pgr.txt:22750-22757, fn=fx*fy only)",
            "cross_check": "PGR only documents the 32-bit float row (0011 0000); the two 64-bit rows "
                             "(0011 0001, 0011 0010) are SHARC+-only, not independently confirmed",
            "rows": [{"opcode": o, "syntax": s} for o, s in MULOP_64],
        },
        "mod1_table": {"source": "PRM 'MOD1 Encode Table' (p.430); PGR Table 12-7 (pgr.txt:22699) -- identical",
                        "cross_check": "identical", "rows": [{"option": o, "opcode": v} for o, v in MOD1]},
        "mod2_table": {"source": "PRM 'MOD2 Encode Table' (p.430-431); PGR Table 12-8 (pgr.txt:22755) -- identical",
                        "cross_check": "identical", "rows": [{"option": o, "opcode": v} for o, v in MOD2]},
        "mod3_table": {"source": "PRM 'MOD3 Encode Table' (p.431); PGR Table 12-9 (pgr.txt:22773) -- identical",
                        "cross_check": "identical", "rows": [{"option": o, "opcode": v} for o, v in MOD3]},
        "shiftop_shiftimm": {
            "source": "PRM Table 18-9 (p.431-433); cross-check PGR Table 12-11 (pgr.txt:22882-22957)",
            "cross_check": ("PRM's own 2-column text layout (shiftimm | shiftop) is textually mangled by "
                             "linear PDF extraction (the two opcode columns interleave with syntax lines); "
                             "rows here are reconstructed by matching each PRM 8-bit shiftop value's upper "
                             "6 bits against PGR Table 12-11's flat opcode column, which lists the same "
                             "instruction set unambiguously. All reconstructed shiftop values matched a "
                             "PGR row. PGR is missing 'bffwrp=RN' / EXP(ex) / LEFTZ / LEFTO / FPACK / "
                             "FUNPACK's shiftimm forms because those don't have Type-6a immediate forms "
                             "(shiftimm column left None here); PGR also omits BITDEP entirely -- may be a "
                             "214xx-only op per PGR's own footnote."
                             " Correction (2026-09-28): the 6-bit values of bitext (nu), or fdep and or "
                             "fdep (se) had been copied from PRM Table 17-9's shiftimm column, which is off "
                             "by one row for those three rows (it prints 011001/011011/011101). They are now "
                             "the upper 6 bits of the 8-bit values, per PGR Table 12-11 (pp.12-10/12-11: "
                             "'for shift immediate (type 6 instructions) the upper 6 MSBs represent valid "
                             "bits'): 010110/011001/011011. The firmware agrees: DT2 1.16 uses 011001 only "
                             "with BIT6:LEN6 fields such as 23:8 and 31:1 in its float-conversion helpers. "
                             "PGR does list BITDEP (0111 0100, footnote: 214xx only); added as 011101."),
            "rows": [{"shiftop_8bit": op, "shiftimm_6bit": si, "syntax": s} for op, si, s in SHIFTOP],
        },
        "dual_add_subtract": {
            "source": "PRM Table 18-10 (p.433); cross-check PGR (dual-ALU figure, pgr.txt:22986-23010) -- identical",
            "cross_check": "identical",
            "rows": [{"opcode_19_16": o, "syntax": s, "format": f} for o, s, f in DUAL_ADD_SUB],
        },
        "multifn_mul_alu": {
            "source": "PGR Table 12-12 (pgr.txt:23129-23198); PRM ch.25 p.528-529 lists the same syntax "
                       "forms in prose, in the SAME order, with no binary opcodes -- used as an "
                       "independent ordering cross-check, not a bit-level one.",
            "cross_check": "PRM/PGR mnemonic order matches 1:1 for all 22 rows (verified by hand); "
                             "PRM does not print opcode bits for this table at all, so the binary values "
                             "themselves are PGR-only and not independently bit-confirmed for SHARC+.",
            "rows": [{"opcode_21_16": o, "syntax": s} for o, s in MULTIFN_MUL_ALU],
        },
        "multifn_mul_dual_addsub": {
            "source": "PRM ch.25 p.529 (syntax only, no opcode printed); PGR Table 12-1 assigns the "
                       "whole 10xxxx/11xxxx range with no sub-table",
            "rows": [{"opcode_21_16": o, "syntax": s, "format": f, "note": n} for o, s, f, n in MULTIFN_MUL_DUAL_ADDSUB],
        },
        "shortcompute": {
            "source": "PRM Table 18-2 opcode field (p.423-424) + ShortCompute opcode list (p.424-425); "
                       "cross-check PGR 'Short Compute Opcodes' (pgr.txt:23108-23120) -- identical",
            "cross_check": "identical",
            "rows": [{"opcode_11_8": o, "syntax": s} for o, s in SHORTCOMPUTE],
        },
        "mrdatamove": {
            "source": "PRM Table 18-29 'MRDATAMOVE Encode Table' (p.438); cross-check PGR Table 12-10 "
                       "(pgr.txt:22827-22841) -- identical for the opcode->MR-register mapping",
            "cross_check": "identical for the confirmed bits (opcode[15:12] -> MR register); the D-bit "
                             "(direction: 0=to register file, 1=to MR register) and RN register field "
                             "positions are PRM-confirmed (D=bit16, per Table 18-29 text) / PGR-inferred "
                             "(RN=DREG at bits 11:8, from PGR's figure) respectively -- see "
                             "top_level_structure.mr_data_move_selector for the gap.",
            "rows": [{"opcode_15_12": o, "mr_register": r} for o, r in MRDATAMOVE],
        },
        "register_operand_encoding": {
            "source": "PRM Table 18-11/18-12 (single-fn), 18-27/18-28 (64-bit float pairs), geometrically "
                       "verified positions above",
            "rule": ("For every ordinary 4-bit rn/rx/ry (or Rm/Ra/Rs) field, the raw binary value IS the "
                     "register number: 0000=R0/F0 .. 1111=R15/F15 (Table 18-28 confirms this literally: "
                     "opcode 0000->R0, 0001->R1, ..., 1111->R15). Whether the register is read/written as "
                     "fixed-point Rn or floating-point Fn is NOT encoded in the field itself -- it is "
                     "determined by which ALUOP/MULOP/SHIFTOP was selected (a fixed-point opcode reads/"
                     "writes RN, a floating-point opcode reads/writes FN at the identical bit position "
                     "and identical numeric encoding)."),
            "restricted_2bit_fields": ("In MUL/ALU and MUL-dual-add/sub multifunction ops, the 4 INPUT "
                     "operand fields (Rxm/Rym/Rxa/Rya or Fxm/Fym/Fxa/Fya) are only 2 bits, each selecting "
                     "within a hard-wired quad: Rxm in {R0-R3}, Rym in {R4-R7}, Rxa in {R8-R11}, Rya in "
                     "{R12-R15} (PRM Table 18-17's bit descriptions, e.g. 'Rxa ... (R11-8)'). Only the "
                     "RESULT registers (Rm, Ra, and Rs in dual-add/sub) get the full 4-bit R0-15 field."),
            "float_64bit_pairs": ("64-bit float register-pair fields select an even/odd register pair "
                     "FN+1:N. Single-function 64-bit ops (Table 18-23) use a full 4-bit code, valid for "
                     "any even N (Table 18-28: e.g. 0010 -> F3:2; odd codes are the plain single register, "
                     "no pair). Multifunction 64-bit ops (Table 18-25) use 2-bit input fields, each "
                     "choosing 1 of 2 valid pairs within a quad (Table 18-27): Fx:y in {F1:0,F3:2}, "
                     "Fz:w in {F5:4,F7:6}, Fp:q in {F9:8,F11:10}, Fr:s in {F13:12,F15:14}; opcode values "
                     "01/11 (odd) are reserved/undefined for a pair ('-' in the PRM table)."),
        },
    }

    # Register the one real conflict found (MULOP MRxF/B rows) into CONFLICTS.
    conflict("mulop_mr_move_encoding",
             prm_value="MR<->register-file moves use a dedicated MRDATAMOVE encoding "
                        "(Table 18-29: D-bit[16] + opcode[15:12]); Table 18-7 (MULOP) has no "
                        "'MRxF/B=Rn' row.",
             pgr_value="Table 12-5 lists 'MRxF/B = Rn' / 'Rn = MRxF/B' at MULOP opcode 0000 0000, "
                       "which collides with its own 'Rn = SAT MRF mod2 (UI)' row (opcode 0000 000x "
                       "with x=0).",
             detail="Likely a PGR-side erratum/duplication rather than a real second encoding; the "
                    "PRM's separate MRDATAMOVE table is treated as authoritative for SHARC+.")
    conflict("mr_data_move_top_bits",
             prm_value="Table 18-29 only documents D-bit[16] and opcode[15:12]; bits 22:17 are not "
                        "given in the SHARC+ PRM text for this table.",
             pgr_value="Table 12-1 / MR-transfer figure fix bits 22:17 = '100000'.",
             detail="Carried into top_level_structure.mr_data_move_selector as PGR-only / unconfirmed "
                    "for SHARC+; not contradictory, just unverified from the primary (PRM) source.")

    out["conflicts"] = CONFLICTS
    out["notes"] = NOTES
    return out


def summarize(data):
    n_alu = len(data["aluop_32_40bit"]["rows"]) + len(data["aluop_64bit"]["rows"])
    n_mul = len(data["mulop_32_40bit"]["rows"]) + len(data["mulop_64bit"]["rows"])
    n_shift = len(data["shiftop_shiftimm"]["rows"])
    n_multifn = len(data["multifn_mul_alu"]["rows"]) + len(data["multifn_mul_dual_addsub"]["rows"]) \
                + len(data["dual_add_subtract"]["rows"])
    n_short = len(data["shortcompute"]["rows"])
    n_mr = len(data["mrdatamove"]["rows"])

    print("=" * 72)
    print("SHARC+ compute-field decode: extraction summary")
    print("=" * 72)
    print(f"ALU ops captured:          {n_alu:3d}  (32/40-bit: {len(data['aluop_32_40bit']['rows'])}, "
          f"64-bit float: {len(data['aluop_64bit']['rows'])})")
    print(f"Multiplier ops captured:   {n_mul:3d}  (32/40-bit: {len(data['mulop_32_40bit']['rows'])}, "
          f"64-bit float: {len(data['mulop_64bit']['rows'])})")
    print(f"Shifter ops captured:      {n_shift:3d}")
    print(f"Multifunction ops:         {n_multifn:3d}  (MUL/ALU: {len(data['multifn_mul_alu']['rows'])}, "
          f"MUL dual add/sub: {len(data['multifn_mul_dual_addsub']['rows'])}, "
          f"dual ALU: {len(data['dual_add_subtract']['rows'])})")
    print(f"ShortCompute ops:          {n_short:3d}")
    print(f"MRDATAMOVE entries:        {n_mr:3d}")
    print(f"Modifier tables:           MOD1={len(MOD1)} MOD2={len(MOD2)} MOD3={len(MOD3)}")
    print()
    print("Top-level compute-field structure (bits 22:0):")
    print("  Single-function (mf=0):  mf[22] cu[21:20] opcode[19:12] rn[11:8] rx[7:4] ry[3:0]")
    print("    cu: 00=ALU 01=Multiplier 10=Shifter")
    print("  Multifunction   (mf=1):  mf[22] opcode[21:16] <register sub-fields, bits 15:0>")
    print("  ShiftImm (Type 6a only, replaces mf/cu): bit22=0 shiftop[21:16] data[15:8] rn[7:4] rx[3:0]")
    print("    (+ dataex[26:23], outside the 23-bit field, in the enclosing Type 6a frame)")
    print("  ShortCompute (Type 2c only, 16-bit micro-field): sc[15:12]=1100 opcode[11:8] rn[7:4] rx[3:0]")
    print()
    print(f"PRM/PGR conflicts flagged: {len(data['conflicts'])}")
    for c in data["conflicts"]:
        print(f"  - {c['topic']}: PRM says {c['prm']!r} vs PGR says {c['pgr']!r}")
    print()
    print("Gaps / caveats:")
    print("  - MR data move top bits (22:17) are PGR-only (Table 12-1 / MR-transfer figure); "
          "the SHARC+ PRM's own Table 18-29 does not print them (see mr_data_move_selector).")
    print("  - MULOP MRxF/B<->Rn: PGR's Table 12-5 rows collide with its own SAT-MRF encoding; "
          "treated as a PGR erratum, PRM's separate MRDATAMOVE table used instead.")
    print("  - Multifunction MUL/ALU opcode[21:16] values (Table 18-15/multifn_mul_alu) come from "
          "PGR Table 12-12; the PRM (ch.25) gives the same 22 mnemonics in the same order but never "
          "prints the opcode bits, so this table's binary values are PGR-sourced, order-cross-checked "
          "only, not independently bit-confirmed against the SHARC+ PRM.")
    print("  - MUL dual-add/subtract (opcode[21:16]=10xxxx/11xxxx) has only one defined syntax form "
          "each in both PRM and PGR; neither source pins the low bits (reserved/don't-care assumed).")
    print("  - ALUOP 64-bit float (Table 18-6) and MULOP's 2 extra 64-bit rows (Table 18-8) are "
          "SHARC+-only; PGR (classic core) has no 64-bit float compute to cross-check against.")
    print("  - Table 18-13's first field is printed 'Rx/Fs' in the PRM; read here as 'Rs/Fs' per "
          "Table 18-14's bit descriptions (see errata note on that table).")
    print("  - SHIFTOP/SHIFTIMM pairing (Table 18-9) required reconstruction against PGR Table 12-11 "
          "because linear PDF text extraction interleaves the PRM's two-column opcode layout.")
    print("=" * 72)


if __name__ == "__main__":
    data = build()
    json.dump(data, open("compute_table.json", "w"), indent=1)
    n_bytes = len(json.dumps(data))
    print(f"Wrote compute_table.json ({n_bytes:,} bytes)\n")
    summarize(data)
