"""Rendering of one decoded instruction by form: conditions, memory, loop, call and jump forms."""

from __future__ import annotations

from collections import Counter

from tools.sharc_dev import sharcflow, sharcinv
from tools.sharc_dev.sharc_core.encoding import ACCESS_WIDTHS, TYPE4B_ACCESS_WIDTHS
from tools.sharc_dev.sharc_trace import UREG_CODES

from .decode import render_compute, render_shortcompute, ureg_name
from .regions import _annotate_literal, name_literal_region

# --- per-form rendering --------------------------------------------------


def sign_extend(value: int, bits: int) -> int:
    return sharcinv.sign_extend(value, bits)


# PGR Table 10-4 (p.10-33, "Condition and Termination Opcodes"): the 5-bit
# COND field's 32 codes, shared by every IF-conditional compute/branch/
# memory form (Type 2a, 3a/3b/3d, 4a/4b/4d, 5a/5b move+swap, 6a_mem/
# 6a_nomem/6b_shiftimm, 7a/7b, 8a, 9a/9b, 10a_rel, 11a/11c). This is a
# display-only name table -- tools/sharc_dev/sharc_trace.py's SIMPLE_COND_BITS and
# _predicate() separately implement the ASTATX-bit semantics for the
# subset of codes it can resolve statically; the two are not meant to be
# merged; this one only has to spell the code the manual gives it. Code
# 0x1F is "TRUE/FOREVER": the PGR footnote says an unspecified COND is
# this value and the compute/branch always executes, so callers never
# print an "IF" for it. 0x0F ("LCE/NOT LCE") is documented only as a
# DO-UNTIL termination test, not a valid IF condition; it is listed here
# only so an occurrence still gets a manual-traceable name instead of a
# bare number.
COND_NAMES = {
    0x00: "EQ",
    0x10: "NE",
    0x01: "LT",
    0x11: "GE",
    0x02: "LE",
    0x12: "GT",
    0x03: "AC",
    0x13: "NOT AC",
    0x04: "AV",
    0x14: "NOT AV",
    0x05: "MV",
    0x15: "NOT MV",
    0x06: "MS",
    0x16: "NOT MS",
    0x07: "SV",
    0x17: "NOT SV",
    0x08: "SZ",
    0x18: "NOT SZ",
    0x09: "FLAG0",
    0x19: "NOT FLAG0",
    0x0A: "FLAG1",
    0x1A: "NOT FLAG1",
    0x0B: "FLAG2",
    0x1B: "NOT FLAG2",
    0x0C: "FLAG3",
    0x1C: "NOT FLAG3",
    0x0D: "TF",
    0x1D: "NOT TF",
    0x0E: "BM/SF1",
    0x1E: "NOT BM/SF1",
    0x0F: "LCE",  # DO-UNTIL termination code only, not a real IF condition
    0x1F: "TRUE",
}
ALWAYS_TRUE_COND = 0x1F


def cond_name(cond: int) -> str:
    return COND_NAMES.get(cond, "cond%d" % cond)


def cond_prefix(f: dict) -> str:
    """'IF <name> ' for a form whose merged fields carry a real, non-
    always-true condition; '' when the form has no "cond" field at all
    (e.g. Type2a_short/2c/2b: PRM/decode_table.json give these no COND
    bits whatsoever -- they are a structurally different, unconditional
    encoding, not just an always-true instance of Type2a) or when the
    condition is 0x1F (already always true, so printing it would only add
    noise to every unconditional listing line)."""
    cond = f.get("cond")
    if cond is None or cond == ALWAYS_TRUE_COND:
        return ""
    return "IF %s " % cond_name(cond)


def _space_dir(f: dict) -> str:
    space = "PM" if f.get("g") else "DM"
    direction = "store" if f.get("d") else "load"
    return space, direction


def _type14d_suffix(f, direction):
    """Type14d width/sign suffix (PRM out/refs/sharc-plus-prm pp.384-387,
    Figure 15-2: the w, ex, d, l, x fields; BH/BHEX/BHSE/BHSEEX/EX/LWEX
    encode tables on pp.386-387). Mirrors tools/sharc_dev/sharc_trace.py's "14d"
    _execute branch field-to-width mapping bit for bit for the w=0,ex=0
    rows it actually runs (BH for a store, BHSE for a load: l picks
    byte/short, and for a load x additionally picks zero- vs
    sign-extend). sharc_trace refuses to *execute* ex=1 (exclusive access)
    and stops there without computing a width, but the PRM still gives
    those rows an unambiguous syntax (BHEX/BHSEEX for w=0, EX/LWEX for
    w=1), so this disassembler -- which only has to name the encoding, not
    run it -- renders those too. w=1,ex=0 has no row in the PRM opcode
    table at all (the same combination sharc_trace calls out as
    undocumented), and a store (d=1) with x=1 is off the BH/BHEX tables
    (they have no x column): both are flagged inline rather than guessed
    at, since the manual does not name them.
    """
    store = direction == "store"
    w, ex, l, x = f.get("w"), f.get("ex"), f.get("l"), f.get("x")  # noqa: E741
    if w:
        if not ex:
            return " (undocumented: w=1,ex=0)"
        return " (lw,ex)" if l else " (ex)"
    base = "sw" if l else "bw"
    if store:
        tag = "%s,ex" % base if ex else base
        if x:
            tag += ", undocumented x=1"
        return " (%s)" % tag
    if x:
        base += "se"
    return " (%s,ex)" % base if ex else " (%s)" % base


def render_mem_direct(sw, f, kind):
    """14a/14d: direct (pure-absolute) DM/PM addressing. 14a's "l" is the
    PRM's (LW) "long word" modifier (a Ureg-pair access, PRM pp.382-383);
    14d's "l" is instead one of the BW/SW/BWSE/SWSE sub-word-width
    selectors (see _type14d_suffix), so 14d needs its own rendering, not
    the plain ", long" suffix.

    Type15a is NOT another absolute form despite sharing this function's
    DIRECT_MEM_FORMS grouping: SHARC+ Core Programming Reference
    (out/refs/sharc-plus-prm) pp.387-390, Figure 15-3 "Type15a Instruction
    Opcode" and its Syntax Summary (p.387) give "DM(<data32>,Ia) = Ureg" /
    "PM(<data32>,Ic) = Ureg" (and the load-direction reverse) -- an
    I-register-relative access, not an absolute one; the Description
    (p.388) is explicit: "The I register is pre-modified with an immediate
    value specified in the instruction. The I register is not updated."
    The worked example (p.389), "DM(24,I5)=TCOUNT;", shows that <data32>
    raw and unscaled, the same convention this file's 4a/15b renderer
    already uses for its own (much narrower) immediate field -- see
    _fmt_index_offset(); tools/sharc_dev/sharc_trace.py's own "15a" handler applies a
    x4 byte-space scale to this same field only for its internal 32-bit-
    normal-word address *arithmetic*, not for display, and its module
    comment gives the identical frame slot rendered by a Type4a as
    "DM(I6-4)" (raw, unscaled) alongside the Type15a re-read of the same
    slot as data32 0xfffffffc (also -4, raw, unscaled) -- so a matching
    disassembler rendering keeps the raw signed value too. Bank selection
    (g=1 -> DAG2/PM, I8-I15) is computed exactly as that same tracer
    handler does: index = i[2:0] + (8 if g else 0) (PRM opcode table
    p.387: g=0 -> dm/I1REG i.e. DAG1, g=1 -> pm/I2REG i.e. DAG2)."""
    if kind == "15a":
        bank = 8 if f.get("g") else 0
        index = (f.get("i", 0) & 0x7) + bank
        off = sign_extend(f.get("addr", 0) & 0xFFFFFFFF, 32)
        ureg = f.get("ureg")
        dreg = f.get("dreg")
        reg = (
            ureg_name(ureg)
            if ureg is not None
            else ("R%d" % dreg if dreg is not None else "?")
        )
        space, direction = _space_dir(f)
        suffix = ", long" if f.get("l") else ""
        addr = _fmt_index_offset_hex(index, off)
        if direction == "store":
            return "%s(%s) = %s%s" % (space, addr, reg, suffix), None, False
        return "%s = %s(%s)%s" % (reg, space, addr, suffix), None, False

    addr = f.get("addr", 0)
    ureg = f.get("ureg")
    dreg = f.get("dreg")
    reg = (
        ureg_name(ureg)
        if ureg is not None
        else ("R%d" % dreg if dreg is not None else "?")
    )
    space, direction = _space_dir(f)
    if kind == "14d":
        suffix = _type14d_suffix(f, direction)
    else:
        suffix = ", long" if f.get("l") else ""
    if direction == "store":
        return "%s(0x%x) = %s%s" % (space, addr, reg, suffix), addr, False
    return "%s = %s(0x%x)%s" % (reg, space, addr, suffix), addr, False


def render_mem_indexed(sw, f):
    """3a/3b/3d/6a_mem/1b-style i,m-mod addressing: DM/PM(Ii, Mj), post-
    modified by Mj; u distinguishes the addressing variant (not resolved
    further here -- printed as-is).

    3a/3b/3d name their register with the wide "ureg" field; 6a_mem instead
    has a plain 4-bit "dreg" (R0-R15) -- same fallback tools/sharc_dev/sharcfn/'s
    render_mem_immoff() already uses for 4a/4b/4d/15b."""
    bank = 8 if f.get("g") else 0
    i = f.get("i", 0) + bank
    m = f.get("m", 0) + bank
    dreg = f.get("dreg")
    ureg = f.get("ureg")
    reg = (
        "R%d" % dreg
        if dreg is not None
        else (ureg_name(ureg) if ureg is not None else "?")
    )
    space, direction = _space_dir(f)
    u = f.get("u")
    u_note = " u=%d" % u if u is not None else ""
    if direction == "store":
        return "%s(I%d, M%d)%s = %s" % (space, i, m, u_note, reg)
    return "%s = %s(I%d, M%d)%s" % (reg, space, i, m, u_note)


# Bit width of the merged "data" immediate-offset field, by form (PRM Type
# 4a/4b ACCESS opcode tables p.13-29/13-32 and decode_table.json: data[5:5]
# + data[4:0], 6 bits; PGR Figure 15-3/PRM Type15b p.15-11: data[6:0], 7
# bits). merge_fields() only concatenates the split data[hi:lo] pieces raw
# -- it does not sign-extend -- so the renderer has to know each form's
# width itself. tools/sharc_dev/sharc_trace.py's "4a"/"4b"/"15b" _execute branches
# already treat this field as a signed twos-complement displacement (an
# index modifier, like the Mj registers, not an unsigned byte count);
# printing it unsigned here disagreed with what actually executes.
_IMMOFF_DATA_BITS = {"4a": 6, "4b": 6, "4d": 6, "15b": 7}


def _fmt_index_offset(i: int, off: int) -> str:
    """'I%d + %d' / 'I%d - %d' for a signed I-register displacement."""
    return "I%d %s %d" % (i, "-" if off < 0 else "+", abs(off))


def _fmt_index_offset_hex(i: int, off: int) -> str:
    """'I%d + 0x%x' / 'I%d - 0x%x': same shape as _fmt_index_offset() for a
    wide (32-bit) signed I-register displacement (Type15a's <data32>), where
    hex reads better than decimal."""
    return "I%d %s 0x%x" % (i, "-" if off < 0 else "+", abs(off))


_LXW_SUFFIX = {
    "normal-word": "",
    "byte": " (bw)",
    "byte-sign-extended": " (bwse)",
    "short-word": " (sw)",
    "short-word-sign-extended": " (swse)",
}


def render_mem_immoff(sw, f, insn_type):
    """4a/4b/4d/15b: I-register + immediate-offset addressing. PRM
    Type4a/4b/4d pp.13-26/13-30/13-34 ("u"): u=0 pre-modifies I for the
    address (I keeps its old value afterwards); u=1 accesses the CURRENT
    (unmodified) I and only writes I+offset back afterwards (post-modify)
    -- so the address this instruction actually touches is I alone when
    u=1, not I+offset (matching tools/sharc_dev/sharc_trace.py's "4a"/"4b" handlers
    and tools/sharc_dev/sharcdb/'s `ptr` table, both of which already draw this
    distinction). Type15b has no u bit and is always pre-modify (PRM
    p.15-11)."""
    # g selects DAG2 (I8-I15) in 4a/4b/4d/15b, as in sharc_core forms_move (index = i + 8 if g).
    i = f.get("i", 0) + (8 if f.get("g") else 0)
    bits = _IMMOFF_DATA_BITS.get(insn_type, 6)
    off = sign_extend(f.get("data", 0), bits)
    dreg = f.get("dreg")
    ureg = f.get("ureg")
    reg = (
        "R%d" % dreg
        if dreg is not None
        else (ureg_name(ureg) if ureg is not None else "?")
    )
    space, direction = _space_dir(f)
    if insn_type in ("4b", "4d"):
        # l/x/w select a width, not (lw): Type4b's (1, 1, 1) is the plain
        # normal word (PRM p.13-32), Type4d's rows are bw/sw/bwse/swse.
        table = TYPE4B_ACCESS_WIDTHS if insn_type == "4b" else ACCESS_WIDTHS
        width = table.get((f.get("l", 0), f.get("x", 0), f.get("w", 0)))
        long_ = _LXW_SUFFIX.get(width, " (undocumented l/x/w)")
    else:
        long_ = ", long" if f.get("l") else ""
    post_modify = insn_type != "15b" and bool(f.get("u"))
    if post_modify:
        addr = "I%d" % i
        note = (
            (", post-modify %s %d" % ("-" if off < 0 else "+", abs(off))) if off else ""
        )
    else:
        addr = _fmt_index_offset(i, off)
        note = ""
    if direction == "store":
        return "%s(%s) = %s%s%s" % (space, addr, reg, long_, note)
    return "%s = %s(%s)%s%s" % (reg, space, addr, long_, note)


def render_dual_mem(f):
    """1a/1b: simultaneous DM(dmi,dmm) and PM(pmi,pmm) reference. PGR Table
    10-1 ("Opcode Acronyms (ISA/VISA)", p.443-444): DMD "DAG1 access
    direction" and PMD "DAG2 access direction" are each 0 = Read, 1 = Write
    -- the same direction the Type 3/4/6/14/15 "D" field encodes (see
    _space_dir()). The manual's own Type 1a/1b Syntax (p.375) prints
    "DM(Ia,Mb) = dreg" for a write and "dreg = DM(Ia,Mb)" for a read: the
    register moves to whichever side receives the value, same as every
    other memory-form renderer here. A previous version instead always put
    the memory term first and only flipped "=" to "<-" for a read, which
    reads backwards ("DM(...) <- Rd" naturally parses as memory receiving
    from the register, i.e. a write, even when dmd/pmd said read)."""
    dmi, dmm = f.get("dmi", 0), f.get("dmm", 0)
    pmi, pmm = f.get("pmi", 0), f.get("pmm", 0)
    dmdreg, pmdreg = f.get("dmdreg", 0), f.get("pmdreg", 0)
    dm = (
        "DM(I%d, M%d) = R%d" % (dmi, dmm, dmdreg)
        if f.get("dmd")
        else "R%d = DM(I%d, M%d)" % (dmdreg, dmi, dmm)
    )
    pm = (
        "PM(I%d, M%d) = R%d" % (8 + pmi, 8 + pmm, pmdreg)
        if f.get("pmd")
        else "R%d = PM(I%d, M%d)" % (pmdreg, 8 + pmi, 8 + pmm)
    )
    return dm + " ; " + pm


def render_modify(sw, insn_type, f):
    """19a/19a_scaled/19a_bitrev/16a/16b: I-register modify, by raw bytes,
    scaled by normal-word, bit-reversed, or storing a literal while
    modifying (16a/16b)."""
    if insn_type.startswith("19a"):
        # PGR Type 19 encodes the destination as Is XOR Idis, not as a
        # direct register number (see tools/sharc_dev/sharc_trace.py's _compute,
        # "19a"/"19a_scaled", citing PGR Table/Figure for Type 19); g
        # selects DAG1 (I0-I7) vs DAG2 (I8-I15) for both registers.
        bank = 8 if f.get("g") else 0
        src_low, dis_low = f.get("is", 0), f.get("idis", 0)
        src, dst = bank + src_low, bank + (src_low ^ dis_low)
        space = "PM" if f.get("g") else "DM"
        val = f.get("data", 0)
        note = {
            "19a": "raw-byte offset",
            "19a_scaled": "scaled by normal-word",
            "19a_bitrev": "bit-reversed addressing",
        }[insn_type]
        return "I%d = modify(I%d, %s)  [%s space=%s]" % (
            dst,
            src,
            hex(val),
            note,
            space,
        )
    i, m = f.get("i", 0), f.get("m", 0)
    space = "PM" if f.get("g") else "DM"
    val = f.get("data", 0)
    return "%s(I%d, M%d) = 0x%x  [also I%d += M%d]" % (
        space,
        i,
        m,
        val & 0xFFFFFFFF,
        i,
        m,
    )


def render_literal_load(f, form):
    ureg = f.get("ureg")
    val = f.get("data", 0)
    reg = ureg_name(ureg) if ureg is not None else "?"
    return "%s = 0x%x" % (reg, val & 0xFFFFFFFF)


def render_18a(f):
    bop = f.get("bop", 0)
    sreg = f.get("sreg", 0)
    code = UREG_CODES.get("USTAT1", 0) + sreg
    reg = ureg_name(code)
    mask = f.get("data", 0)
    names = {
        0: "set",
        1: "clear",
        2: "toggle",
        3: "?bop3",
        4: "bit-test",
        5: "xor-test",
    }
    op = names.get(bop, "bop?%d" % bop)
    if bop in (4, 5):
        return "%s(%s, mask=0x%x) -> BTF" % (op, reg, mask)
    return "%s = %s(%s, 0x%x)" % (reg, op, reg, mask)


# Opcode -> mnemonic for render_shiftimm(), restricted to the ShiftImm
# opcodes tools/sharc_dev/sharc_trace.py's _shift_immediate() actually implements and
# cites (PRM Table 17-9 pp.17-10/17-11; PRM p.3-17 for the (SE) note). The
# other rows in tools/sharc_dev/sharcspec/compute_table.json's shiftop_shiftimm table
# (rot, fdep, fdep (se), bitext, bffwrp, exp, leftz, lefto, fpack, funpack)
# are left out: that table's own cross_check note says the PRM's OCR'd
# DATA8/BIT6:LEN6/BITLEN12/DATA7 immediate-column split is ambiguous for
# them, and _shift_immediate() itself does not model them (it raises on
# any opcode not listed here), so a generic per-row template would risk
# printing a wrong immediate field width.
_SHIFTIMM_MNEMONICS = {
    0x00: "lshift",
    0x01: "ashift",
    0x08: "or-lshift",
    0x09: "or-ashift",
    0x10: "fext",
    0x12: "fext-se",
    0x30: "bset",
    0x31: "bclr",
    0x32: "btgl",
    0x33: "btst",
}


def render_shiftimm(f):
    """6a_mem/6b_shiftimm's parallel ShiftImm sub-instruction: a 23-bit
    field (PRM Table 18-9 pp.431-433, cross-checked against PGR Table
    12-11; tools/sharc_dev/sharcspec/compute_table.json's shiftop_shiftimm) packing a
    6-bit opcode, an 8-bit immediate, and RN/RX register numbers -- the
    same layout tools/sharc_dev/sharc_trace.py's _shift_immediate() executes. Opcodes
    outside _SHIFTIMM_MNEMONICS fall back to the raw field dump (see that
    dict's comment for why)."""
    field = f.get("shiftimm", 0) & 0x7FFFFF
    opcode = (field >> 16) & 0x3F
    data8 = (field >> 8) & 0xFF
    rn, rx = (field >> 4) & 0xF, field & 0xF
    dataex = f.get("dataex", 0)
    name = _SHIFTIMM_MNEMONICS.get(opcode)
    if name is None:
        return "shiftimm(dataex=0x%x, shiftimm=0x%x)" % (dataex, field)
    if opcode in (0x00, 0x01, 0x08, 0x09):
        amount = sign_extend(data8, 8)
        if opcode in (0x08, 0x09):
            return "R%d = R%d or %s(R%d, %d)" % (
                rn,
                rn,
                name.split("-")[1],
                rx,
                amount,
            )
        return "R%d = %s(R%d, %d)" % (rn, name, rx, amount)
    if opcode in (0x10, 0x12):
        position = data8 & 0x3F
        length = (dataex << 2) | (data8 >> 6)
        return "R%d = %s(R%d, pos=%d, len=%d)" % (rn, name, rx, position, length)
    if opcode in (0x30, 0x31, 0x32):
        return "R%d = %s(R%d, bit=%d)" % (rn, name, rx, data8)
    return "%s(R%d, bit=%d)  [status only]" % (name, rx, data8)


def render_loop(sw, f, insn_length_bytes, insn_type):
    reladdr = f.get("reladdr")
    start_sw = sw + (insn_length_bytes or 6) // 2
    end_sw = sw + sign_extend(reladdr, 23) if reladdr is not None else None
    if insn_type == "12a_imm":
        count = f.get("data", 0)
        head = "DO 0x%x UNTIL LCE  (trip count %d, literal)" % (end_sw or 0, count)
    else:
        ureg = f.get("ureg")
        head = "DO 0x%x UNTIL LCE  (trip count %s, register)" % (
            end_sw or 0,
            ureg_name(ureg) if ureg is not None else "?",
        )
    body = " body [0x%x, 0x%x)" % (start_sw, end_sw) if end_sw is not None else ""
    return head + body, end_sw


def render_call_or_jump(sw, insn_type, f):
    b = f.get("b")
    cond = f.get("cond")
    j = f.get("j")
    conditional = cond is not None and cond != ALWAYS_TRUE_COND
    delayed_note = "" if j is None else (" delayed" if j else " non-delayed")
    cond_note = "" if not conditional else (" IF %s" % cond_name(cond))
    if insn_type.startswith("25a"):
        return "CALL" + cond_note + " (linked, delayed)"
    kind = "CALL" if b else "JUMP"
    return kind + cond_note + delayed_note


# forms whose merged fields carry a 23-bit (or 12-bit, 2c) compute field
# alongside their own addressing/control fields; kept in sync with
# tools/sharc_dev/sharcinv.py's COMPUTE_FORMS list (imported, not duplicated).
COMPUTE_FORMS = sharcinv.COMPUTE_FORMS

DIRECT_MEM_FORMS = {"15a", "14a", "14d"}
INDEXED_MEM_FORMS = {"3a", "3b", "3d", "6a_mem"}
IMMOFF_MEM_FORMS = {"4a", "4b", "4d", "15b"}
DUAL_MEM_FORMS = {"1a", "1b"}
MODIFY_FORMS = {"19a", "19a_scaled", "19a_bitrev", "16a", "16b"}
LITERAL_LOAD_FORMS = {"17a", "17b"}
LOOP_FORMS = {"12a_imm", "12a_ureg"}
CALL_JUMP_FORMS = {
    "25a_direct",
    "25a_pcrel",
    "8a_abs",
    "8a_rel",
    "9a_abs",
    "9a_rel",
    "9b_abs",
    "9b_rel",
}


def render_instruction(
    sw: int,
    insn,
    mem,
    named_tables_touched: set,
    literal_regions: Counter,
    float_immediates: list,
):
    """-> (mnemonic, notes: [str], gap: bool) for one decoded instruction.
    Never returns None: an unhandled form falls back to a field dump."""
    t = insn.type_name
    f = sharcinv.merge_fields(insn.fields)
    notes = []
    gap = False

    if insn.kind == "unknown":
        return (
            "!!UNDECODED!! %s (word0=0x%04x)" % (insn.note, insn.raw or 0),
            notes,
            True,
        )
    if insn.kind == "uncertain":
        notes.append("uncertain form (%s)" % insn.note)

    def literal_note(value, float_capable):
        text = _annotate_literal(value, float_capable, mem)
        region = name_literal_region(value)
        if region:
            named_tables_touched.add(region)
            literal_regions[region] += 1
        if float_capable:
            fl = sharcinv.float32(value)
            if sharcinv._plausible_float(fl):
                float_immediates.append(fl)
        if text:
            notes.append(text)

    compute_text = None
    if t in COMPUTE_FORMS:
        compute_field = f.get("compute")
        if compute_field:
            compute_text, gap = render_compute(compute_field)

    if t == "9b_abs" and insn.raw == sharcflow.RETURN_JUMP:
        return "RETURN", notes, gap

    if t in CALL_JUMP_FORMS:
        head = render_call_or_jump(sw, t, f)
        if t in ("25a_direct", "8a_abs"):
            target = f.get("addr")
        elif t in ("25a_pcrel", "9a_rel", "8a_rel", "9b_rel"):
            target = sharcflow.pcrel_target(sw, f.get("reladdr", 0))
        elif t in ("9a_abs", "9b_abs"):
            # 9a_abs has no addr field: like 9b_abs it jumps through PM(I, M).
            target = None
            head += "  target=indirect PM(I%d, M%d)" % (
                8 + f.get("pmi", 0),
                8 + f.get("pmm", 0),
            )
        else:
            target = None
        if target is not None:
            head += "  target=0x%x" % target
        if compute_text:
            head = compute_text + "; " + head
        return head, notes, gap

    if t in LOOP_FORMS:
        text, end_sw = render_loop(sw, f, insn.length_bytes, t)
        return text, notes, gap

    if t in DUAL_MEM_FORMS:
        text = render_dual_mem(f)
        if compute_text:
            text = compute_text + "; " + text
        return text, notes, gap

    if t in DIRECT_MEM_FORMS:
        text, addr, _ = render_mem_direct(sw, f, t)
        # Type15a's "addr" is an I-register-relative displacement (see
        # render_mem_direct's docstring), not an absolute address -- unlike
        # 14a/14d, it has nothing for literal_note's absolute-address
        # annotation (named-region lookup, float-literal check) to key on.
        if t != "15a":
            literal_note(addr, False)
        if compute_text:
            text = compute_text + "; " + text
        return text, notes, gap

    if t in INDEXED_MEM_FORMS:
        text = render_mem_indexed(sw, f)
        if t == "6a_mem":
            # PRM Type 6a performs the ShiftImm sub-op in parallel with the
            # memory transfer (tools/sharc_dev/sharc_trace.py's "6a_mem" _execute
            # branch); 6a_mem has no "compute" field, so this is the only
            # place its ShiftImm half gets rendered.
            text += "  [parallel %s]" % render_shiftimm(f)
        if compute_text:
            text = compute_text + "; " + text
        # 3a/3b/3d/6a_mem all carry a "cond" field (decode_table.json) that
        # gates the whole instruction, not just a compute half (PRM p.7924,
        # cited in tools/sharc_dev/sharc_trace.py's Type3a handling).
        return cond_prefix(f) + text, notes, gap

    if t in IMMOFF_MEM_FORMS:
        text = render_mem_immoff(sw, f, t)
        if compute_text:
            text = compute_text + "; " + text
        # 4a/4b/4d carry "cond"; 15b does not (cond_prefix(f) is "" then).
        return cond_prefix(f) + text, notes, gap

    if t in MODIFY_FORMS:
        text = render_modify(sw, t, f)
        if "data" in f:
            literal_note(f["data"], t in ("16a",))
        return text, notes, gap

    if t in LITERAL_LOAD_FORMS:
        text = render_literal_load(f, t)
        literal_note(f.get("data", 0), True)
        return text, notes, gap

    if t == "18a":
        text = render_18a(f)
        literal_note(f.get("data", 0), False)
        return text, notes, gap

    if t == "2c":
        text = render_shortcompute(f.get("compute", 0))
        return text, notes, gap

    if t in ("2a", "2a_short", "2b") and compute_text:
        # Only Type2a (48-bit) actually carries a "cond" field; Type2a_short
        # and Type2b are a structurally different 32-bit encoding with no
        # COND bits at all (decode_table.json), not merely an always-true
        # instance of Type2a, so cond_prefix(f) is always "" for them.
        return cond_prefix(f) + compute_text, notes, gap

    if t in ("5a_move", "5b_move"):
        src_hi = f.get("srcureghigh", 0)
        src_lo = (f.get("srcureglow", 0)) & 3
        src = (src_hi << 2) | src_lo
        dst = f.get("dstureg")
        text = "%s = %s" % (ureg_name(dst) if dst is not None else "?", ureg_name(src))
        if compute_text:
            text = compute_text + "; " + text
        return cond_prefix(f) + text, notes, gap

    if t in ("5a_swap", "5b_swap"):
        c, d = f.get("cdreg"), f.get("dreg")
        text = "R%s <-> R%s" % (c, d)
        if compute_text:
            text = compute_text + "; " + text
        return cond_prefix(f) + text, notes, gap

    if t == "3c":
        # decode_table.json's Type3c has its own "d" field (bit 37, PGR
        # p.449's opcode figure), the same Data-direction bit Table 10-1
        # (p.443) defines for every other Type 3/4/6/14/15 memory form (0 =
        # read, 1 = write) and that tools/sharc_dev/sharc_trace.py's "3c" _execute
        # branch checks the same way (store when d, else load). A previous
        # version of this branch never read "d" at all and always rendered
        # a store, which disagreed with the tracer for every d=0 (load)
        # encoding -- e.g. DT2 1.16 sw 0x1c653b (dmi=4, dmm=5, d=0, dreg=6)
        # is a load, "R6 = DM(I4, M5)", not "DM(I4, M5) = R6".
        dmi, dmm, dreg = f.get("dmi", 7), f.get("dmm", 7), f.get("dreg", 2)
        _, direction = _space_dir(f)
        if direction == "store":
            text = "DM(I%d, M%d) = R%d" % (dmi, dmm, dreg)
        else:
            text = "R%d = DM(I%d, M%d)" % (dreg, dmi, dmm)
        return text, notes, gap

    if t == "18a":  # pragma: no cover -- handled above; kept for clarity
        return render_18a(f), notes, gap

    if t == "20a":
        ops = [
            name
            for name in ("lpu", "lpo", "spu", "spo", "ppu", "ppo", "fc")
            if f.get(name)
        ]
        text = "status-stack " + (",".join(ops) if ops else "(no-op)")
        return text, notes, gap

    if t in ("21a", "21c"):
        return "NOP/reserved (%s)" % t, notes, gap

    if t in ("22a", "22c"):
        return "EMU %s" % hex(f.get("emu", 0)), notes, gap

    if t in ("25a_rframe", "25c_rframe"):
        return "(return-frame delay slot: %s)" % t, notes, gap

    if t == "26a":
        return "(reserved/26a)", notes, gap

    if t == "7d":
        # docs/findings/06-sharc-engine-and-startup.md: "Type7d is ACONV".
        return (
            "ACONV  breg=%s toby=%s idis=%s"
            % (f.get("breg"), f.get("toby"), f.get("idis")),
            notes,
            gap,
        )

    if t in ("6b_shiftimm",):
        text = render_shiftimm(f)
        return cond_prefix(f) + text, notes, gap

    if t == "7a":
        # PRM Type7a MODIFY (SHARC+ Core Programming Reference pp.13-46/
        # 13-48): "Ia = MODIFY(Ia,Mb)" / "Ic = MODIFY(Ic,Md)" -- an I
        # register update by the M register, done in parallel with an
        # optional compute (COMPUTE_FORMS already renders that half into
        # compute_text above). tools/sharc_dev/sharc_trace.py's own "7a" handler
        # already computes dest = source XOR idis (an idis=0 modify leaves
        # source and dest the same register, matching the PRM's own worked
        # example "I3 = MODIFY(I3,M5); /* Semantically same as
        # MODIFY(I3,M5) */") -- this renderer had no branch for Type7a at
        # all, so a compute=0 (pure-MODIFY) instance fell straight through
        # to the raw field dump below, and a compute!=0 instance rendered
        # only its compute half, silently dropping the MODIFY.
        bank = 8 if f.get("g") else 0
        source = f.get("is", 0) + bank
        dest = (f.get("is", 0) ^ f.get("idis", 0)) + bank
        modifier = f.get("m", 0) + bank
        modify_text = (
            "modify(I%d, M%d)" % (source, modifier)
            if dest == source
            else "I%d = modify(I%d, M%d)" % (dest, source, modifier)
        )
        text = compute_text + "; " + modify_text if compute_text else modify_text
        return cond_prefix(f) + text, notes, gap

    if compute_text:
        # Reaches here for 7a and 11a (both carry "cond" but have no more
        # specific branch above); 1a/9a_abs/9a_rel/3a/4a/2a/2a_short/2b/
        # 5a_move/5a_swap already returned earlier, each with its own
        # cond_prefix() call above.
        return cond_prefix(f) + compute_text, notes, gap

    # Generic fallback -- never skip an instruction.
    fields_text = " ".join("%s=0x%x" % (k, v) for k, v in sorted(f.items()))
    return "[%s] %s" % (t, fields_text), notes, gap
