#!/usr/bin/env python3
"""Merge PRM figures and classic PGR tables into one instruction decode table.

Rules (see README "Findings"):
  * bit positions and field names come from the PRM figure;
  * fixed-bit VALUES come from the classic PGR wherever it has a table for the form,
    because several PRM figures carry stale template digits;
  * PRM-only forms (SHARC+ additions) take PRM values and are marked unconfirmed;
  * where the PRM draws a selector field over bits the PGR splits into two tables
    (e.g. Type 8a "r"), the field wins;
  * errata overrides are applied explicitly and listed in the output.

Frames are 48 bits, MSB aligned: a 16-bit form occupies bits 47..32, a 32-bit form
bits 47..16. VISA availability comes from the PRM headings ("ISA", "VISA", "ISA/VISA").
"""
import json
import re

prm = json.load(open("figures.json"))["figures"]
classic = json.load(open("classic.json"))

# PRM figure name -> classic table keys (a merged figure lists every classic variant).
PAIRS = {
    "Type1a": ["Type 1a"], "Type1b": ["Type 1b"], "Type2a": ["Type 2a"], "Type2b": ["Type 2b"], "Type2c": ["Type 2c"],
    "Type3a": ["Type 3a"], "Type3b": ["Type 3b"], "Type3c": ["Type 3c"], "Type4a": ["Type 4a"], "Type4b": ["Type 4b"],
    "Type5a_move": ["Type 5a"], "Type5a (swap)": ["Type 5a #2"], "Type5b (move)": ["Type 5b"], "Type5b (swap)": ["Type 5b #2"],
    # The PGR's second Type 6a table parses identically to the first, so the no-memory
    # form keeps its PRM fixed bits (see the "else" branch below).
    "Type6a (mem)": ["Type 6a"], "Type7a": ["Type 7a"], "Type7b": ["Type 7b"],
    # Type8a/9a/9b/10a are NOT merged via PAIRS -- see SPLIT_FORMS below: each
    # is a genuine absolute-or-indirect vs. PC-relative pair that must stay
    # two separate decode forms (merging them loses the addressing mode).
    "Type11a": ["Type 11a", "Type 11a #2"], "Type11c": ["Type 11c", "Type 11c #2"],
    "Type12a_imm": ["Type 12a"], "Type13a": ["Type 13a"], "Type14a": ["Type 14a"], "Type15a": ["Type 15a"], "Type15b": ["Type 15b"],
    "Type16a": ["Type 16a"], "Type16b": ["Type 16b"], "Type17a": ["Type 17a"], "Type17b": ["Type 17b"], "Type18a": ["Type 18a"],
    "Type19a": ["Type 19a"], "Type19a_bitrev": ["Type 19a #2"], "Type20a": ["Type 20a"], "Type21a": ["Type 21a"], "Type21c": ["Type 21c"],
    "Type22c": ["Type 22c"], "Type25a_direct": ["Type 25a"], "Type25a_pcrel": ["Type 25a #2"], "Type25c_rframe": ["Type 25c"],
}
ISA_ONLY = {"Type10a"}  # PRM heading "Type 10a ISA (...)"; every other form is ISA/VISA or VISA

# The PRM heading marks the whole Type10a figure "ISA" (no "VISA" or
# "ISA/VISA", unlike its siblings 8a/9a "ISA/VISA" and 9b "VISA" -- see
# docs/sharc/SOURCES.md), so ISA_ONLY above keeps both Type10a_abs and
# Type10a_rel out of the VISA table by default. Firmware contradicts that
# for the rel (PC-relative jump) half: `sw 0x1c5030` in blk93@0x1c4f81
# decodes as Type10a_rel by the classic PGR p.458 "with PC-relative jump"
# bit layout (fixed 111 at bits 47-45, then D/DMI/DMM/COND/RELADDR/DREG/
# COMPUTE -- the same split this file already derives from SPLIT_FORMS
# below), inside code this tool otherwise decodes as VISA and that
# desyncs immediately without this form. The abs (register-indirect jump)
# half has no such firmware evidence yet, so it stays ISA-only pending
# that; see build_table.py's own Type6b_shiftimm/Type2a_short precedent
# for overriding a PRM ISA/VISA heading on firmware evidence.
VISA_OVERRIDE = {"Type10a_rel"}

# Forms where the PRM figure's fixed bits are correct and the PGR disagrees.
# Firmware decoding confirms the PRM value; the earlier "stale template digit"
# call against the PRM was wrong for these.
PRM_VALUE_WINS = {"Type2b"}

# Forms whose PRM figure prints a value for every bit: the instruction is the
# whole word, not a prefix. The classic PGR grid leaves the low bits blank, and
# the merge rule below treats a blank as "any value" -- which let Type21a match
# any first word 0x0000-0x007f and swallow the one or two short instructions
# after it (docs/findings/05-sharc-isa-and-decoding.md, "Type21a was
# swallowing the two words after it"). Figure 17-5
# draws Type21a as 48 zero bits and Figure 17-6 draws Type21c as 0x0001.
# The value is the whole printed word; `free` lists bits the figure draws as a
# field rather than a digit (Type22a's `emu` selects idle from emuidle).
# Field declarations to ignore, so the classic grid's fixed values at those bits
# are used instead. Type19a's PRM figure draws bits 41-40 as a selector
# `sc[1:0]` and bit 39 as `w`, while the classic PGR grid fixes the ordinary
# unscaled form at 10110. Keep that classic form narrow here; the SHARC+ scaled
# `sc=01` form is emitted separately below from the PRM BH table and an
# independent public decoder.
DROP_FIELDS = {"Type19a": {"sc[1:0]", "w"}}

# Type7a's PRM figure (Figure 14-19) brackets bits 28 and 27 as breg/toby, the
# names the Type7d ACONV figure uses for the same bits. Type7d is documented as
# Type7a's own cond=11111, compute=0 case, so the two figures print the same
# bits. For every other Type7a word the classic grid, and Type7b's own PRM
# figure at the same position, make bits 29-27 the M register selector. The
# merge loop cannot restore bit 29 on its own: the PRM has no bracket there and
# the classic side is a live field rather than a blank.
FIELD_OVERRIDE = {
    "Type7a": {
        "remove": ("breg", "toby"),
        "add": [{"label": "m[2:0]", "hi": 29, "lo": 27}],
    }
}

# PRM Table 14-22: Type 7d is the Type 7a word whose condition is 11111 and
# whose compute field is empty, so those bits select the form and are not
# free fields.
PIN_FIELDS = {
    "Type7d": {"cond[4:0]": 0x1F, "compute[22:16]": 0, "compute[15:0]": 0}
}

# Bits where a split branch form takes the PRM figure's digit although its own
# classic table leaves that bit blank, listed per emitted form name. The generic
# merge refuses this wholesale and is right to: filling every such gap on the
# branch forms costs 225 aligned instructions in Digitakt II 1.16 and 279 in
# Digitone II 1.11, and takes Type8a_abs from 29 matches to 6. Most of those
# PRM digits are the stale template values the merge rule exists to ignore,
# exactly as on the compute forms (SPEC-FINDINGS 3.8). Measured per bit over
# both images, only bit 25 on the Type8a pair is a real discriminator; bit 23,
# which the same wholesale fill would have fixed on Type9a, is set in 18 of
# Type9a_abs's 98 instructions and 14 of Type9a_rel's 34, so it is a field in
# use, not a zero. Measure before adding a bit here (SPEC-FINDINGS 3.10).
RESTORE_PRM_GAP = {
    # Of Type8a_rel's 1,382 instructions across the two images, 13 have bit 25
    # set and 12 of those 13 carry a target top byte outside {0x00, 0xff},
    # where every real relative branch stays. Of Type8a_abs's 54, 19 have it
    # set and 18 of those point at something that is not a code address. One of
    # them is the nonsense `jump 0x3e0030` that truncates FUN_001c136a -- the
    # same six bytes `00 07 3e 02 30 00` at 1.16 SW 0x1c13bc and 1.11 SW
    # 0x1c1366.
    "Type8a_abs": {25},
    "Type8a_rel": {25},
}

FULL_WORD = {
    "Type21a": (48, 0x000000000000, ()),
    "Type21c": (16, 0x000100000000, ()),
    "Type22a": (48, 0x008000000000, (38,)),   # Figure 17-7: idle/emuidle
    "Type26a": (48, 0x004000000000, ()),      # Figure 17-13
}

UNDOCUMENTED = [
    # The words Type21a used to swallow: with Type21a tightened to the all-zero
    # word, a first word whose top nine bits are zero and whose rest is not is
    # left over. 95% of the old Type21a matches are such words (859 of 904 in
    # Digitakt II 1.16), and treating them as one short instruction lands 86
    # more Type8a_rel branches on an instruction start. No public figure
    # documents them: prefix and length only, no name and no semantics.
    {
        "name": "Type21p_undoc16",
        "width": 16,
        "prefix_bits": "000000000",
        "note": "provisional, from firmware only (the old Type21a prefix; 95% "
                "of its matches are not the all-zero NOP word)",
    },
    # Type22a is idle/emuidle, and its figure prints every other bit, so the
    # 220 words that matched its nine-bit prefix across the two images are not
    # idle instructions. Same treatment as Type21a, but at the width the loose form read.
    {
        "name": "Type22p_undoc48",
        "width": 48,
        "prefix_bits": "000000001",
        "note": "provisional, from firmware only (the old Type22a prefix; its "
                "strict word occurs in neither image; 48 bits as the loose "
                "Type22a read them -- a 16-bit reading strands the other two "
                "words)",
    },
    {
        "name": "Type26p_undoc48",
        "width": 48,
        "prefix_bits": "0000000001000000",
        "note": "provisional, from firmware only (the old Type26a prefix; one "
                "instance per image, too few to judge)",
    },
    # The words Type8a used to take on its loose eight-bit prefix, now that
    # both halves fix bit 25 to zero (RESTORE_PRM_GAP). Excluding them without
    # giving them a home costs 50 aligned instructions in 1.16 and 71 in 1.11
    # and adds two truncated functions, because an undecoded word strands the
    # sweep -- so they keep their 48-bit length and lose only the claim to be a
    # branch. One prefix covers both halves: bit 40 is Type8a's own abs/rel
    # selector, kept here as the field `r`. 32 across the two images, 30 of
    # them pointing at something that is not an address.
    {
        "name": "Type8p_undoc48",
        "width": 48,
        "prefix_bits": "0000011",
        "extra_fixed": {25: 1},
        "fields": [
            {"label": "r", "hi": 40, "lo": 40},
            {"label": "b", "hi": 39, "lo": 39},
            {"label": "a", "hi": 38, "lo": 38},
            {"label": "cond[4:0]", "hi": 37, "lo": 33},
            {"label": "j", "hi": 26, "lo": 26},
            {"label": "ci", "hi": 24, "lo": 24},
            {"label": "imm[23:16]", "hi": 23, "lo": 16},
            {"label": "imm[15:0]", "hi": 15, "lo": 0},
        ],
        "source": "firmware (undocumented; the bit-25 words Type8a used to "
                  "take; unconfirmed)",
        "note": "provisional, from firmware only (bits 47-41 = 0000011 with "
                "bit 25 set, which no real Type 8a branch has; 32 across the "
                "two images)",
    },
]

# ----------------------------------------------------------------------
# Absolute/PC-relative (or register-indirect/PC-relative) branch pairs.
# ----------------------------------------------------------------------
# classic.json (the classic PGR) keeps each of these as two SEPARATE tables
# (e.g. "Type 8a" and "Type 8a #2") that differ in exactly one selector bit
# ("r" for Type8a, "rel" for Type9a/9b/10a) plus, in the r=1/rel=1 table, a
# different meaning for one field. The PRM draws only ONE figure per type
# and shows that selector as an ordinary field, which is why the old PAIRS
# merge (see the generic loop below) collapsed both classic tables into a
# single decode form and lost the addressing-mode distinction: the merged
# "addr"/pmi+pmm field cannot represent both an absolute address and a
# signed PC-relative displacement. See
# docs/findings/05-sharc-isa-and-decoding.md, "The Type 8a branch forms were
# taking words that are not branches" / task report for the
# firmware evidence (branch-target landing rate) that this split is right.
#
#   shared_target=<label>  : Type8a only -- the merged figure's own field at
#                             that label literally means ADDR when the
#                             selector is 0 and RELADDR when it's 1 (same
#                             bit range either way), so the rel form is just
#                             the abs form's fields with that label renamed.
#   shared_target=None      : Type9a/9b/10a -- the merged figure only ever
#                             shows the selector=0 (register-indirect: PMI
#                             pointer register + PMM modify register)
#                             interpretation. The selector=1 form instead
#                             gets classic's own 6-bit RELADDR field, which
#                             occupies exactly the same bits as PMI+PMM
#                             (1 + 2 + 3 = 6 bits, confirmed against
#                             classic.json) but is unrelated to those
#                             registers -- it's a plain signed displacement.
SPLIT_FORMS = {
    "Type8a":  dict(abs_key="Type 8a", rel_key="Type 8a #2", selector="r", shared_target="addr"),
    "Type9a":  dict(abs_key="Type 9a", rel_key="Type 9a #2", selector="rel", shared_target=None),
    "Type9b":  dict(abs_key="Type 9b", rel_key="Type 9b #2", selector="rel", shared_target=None),
    "Type10a": dict(abs_key="Type 10a", rel_key="Type 10a #2", selector="rel", shared_target=None),
}
# Bit range of the register-indirect PMI/PMM fields in Type9a/9b/10a's merged
# PRM figure, which the rel=1 RELADDR field replaces (see shared_target=None
# above). Fixed across all three forms -- PMI/PMM always sit in the same
# frame-bit slot regardless of instruction width/type.
INDIRECT_REGISTER_LABELS = {"pmi[2:2]", "pmi[1:0]", "pmm[2:0]"}
RELADDR6_FIELDS = [{"label": "reladdr[5:5]", "hi": 32, "lo": 32}, {"label": "reladdr[4:0]", "hi": 31, "lo": 27}]


def fixed_bits_for(width, fields, keys, prm=None, gap_bits=()):
    """Fixed-bit dict for a SINGLE classic.json table (`keys` has one entry):
    every bit position not claimed by a field, where that classic table
    marks a concrete 0/1 value. This is the same rule the generic multi-key
    merge below uses (a 1-element `variants` list trivially agrees with
    itself), factored out so split_branch_form can use it per variant.

    With `prm`, a bit the classic table leaves blank also takes the PRM
    figure's digit. The generic merge refuses to do that, and is right to: on
    the compute forms the PRM's digits in those gaps are stale template values,
    and restoring them destroys real matches (SPEC-FINDINGS 3.8). The branch
    figures are different -- they print honest zeros where their classic tables
    are blank, and without those zeros Type8a_rel matches words that are not
    branches at all (SPEC-FINDINGS 3.10).
    """
    variants = [rebase(classic[k]["pattern"], classic[k]["msb"]) for k in keys]
    field_bits = {b for fl in fields for b in range(fl["lo"], fl["hi"] + 1)}
    fixed = {}
    for b in range(47, 47 - width, -1):
        if b in field_bits:
            continue
        vals = {v.get(b) for v in variants}
        if len(vals) == 1 and vals <= {"0", "1"}:
            fixed[b] = int(vals.pop())
        elif (b in gap_bits and prm is not None and vals <= {".", None}
              and prm.get(b) in ("0", "1")):
            fixed[b] = int(prm[b])
    return fixed


def make_form(name, width, fields, fixed, source, keys=()):
    mask = sum(1 << b for b in fixed)
    value = sum(v << b for b, v in fixed.items())
    isa_only = name.split("_")[0] in ISA_ONLY and name not in VISA_OVERRIDE
    return {
        "name": name, "width": width, "visa": not isa_only, "isa": width == 48,
        "mask": f"0x{mask:012x}", "value": f"0x{value:012x}", "fixed_bits": len(fixed),
        "fields": fields, "source": source, "classic_keys": list(keys),
        "unconfirmed_bits": 0,
    }


def split_branch_form(name, width, fields, cfg, prm=None):
    """Split one PRM-merged branch figure into its classic-confirmed
    absolute-or-indirect (selector=0) and PC-relative (selector=1) forms.
    See SPLIT_FORMS above for the rationale."""
    common = [fl for fl in fields if fl["label"] != cfg["selector"]]

    if cfg["shared_target"]:
        base = cfg["shared_target"]
        abs_fields = common
        rel_fields = [
            dict(fl, label=fl["label"].replace(base, "reladdr", 1)) if fl["label"].startswith(base) else fl
            for fl in common
        ]
    else:
        abs_fields = common
        rel_fields = [fl for fl in common if fl["label"] not in INDIRECT_REGISTER_LABELS] + RELADDR6_FIELDS

    abs_fixed = fixed_bits_for(width, abs_fields, [cfg["abs_key"]], prm,
                               RESTORE_PRM_GAP.get(f"{name}_abs", ()))
    rel_fixed = fixed_bits_for(width, rel_fields, [cfg["rel_key"]], prm,
                               RESTORE_PRM_GAP.get(f"{name}_rel", ()))
    src = "pgr (split from merged PRM figure; see build_table.py SPLIT_FORMS)"
    return [
        make_form(f"{name}_abs", width, abs_fields, abs_fixed, src, [cfg["abs_key"]]),
        make_form(f"{name}_rel", width, rel_fields, rel_fixed, src, [cfg["rel_key"]]),
    ]


def prm_name(f):
    return re.sub(r"^Figure \d+-\d+:\s*", "", f["caption"]).replace(" Instruction", "").replace(" Opcode", "").replace(" Syntax", "").strip()


def rebase(pattern, msb):
    off = 47 - msb
    return {msb - i + off: ch for i, ch in enumerate(pattern)}


forms, notes = [], []
for f in prm:
    name = prm_name(f)
    if not name.startswith("Type"):
        continue
    width = f["width"]
    pb = rebase(f["pattern"], f["msb"])
    shift = 47 - f["msb"]
    fields = []
    for fl in f["fields"]:
        hi = fl["bits"][0] + shift
        lo = (fl["bits"][-1] if len(fl["bits"]) > 1 else fl["bits"][0]) + shift
        fields.append({"label": fl["label"], "hi": hi, "lo": lo})

    if name in SPLIT_FORMS:
        forms.extend(split_branch_form(name, width, fields, SPLIT_FORMS[name], pb))
        notes.append(f"{name}: split into {name}_abs/{name}_rel -- classic.json keeps these as "
                      "separate absolute-or-indirect/PC-relative tables; see SPLIT_FORMS")
        continue

    source = "prm"
    keys = PAIRS.get(name, [])

    # Errata overrides.
    if name == "Type3a":  # PRM figure is a copy of Type 1a
        c = classic["Type 3a"]
        pb = rebase(c["pattern"], c["msb"])
        fields = [{"label": cell["text"].lower().replace(" ", ""), "hi": cell["bits"][0], "lo": cell["bits"][1]}
                  for cell in c["cells"] if cell["kind"] == "field"]
        source, keys = "pgr (PRM figure is a copy of Type 1a)", []
        notes.append("Type3a: layout and values from PGR")
    if name == "Type25c_rframe":  # PRM figure is a copy of Type 25a rframe
        c = classic["Type 25c"]
        pb, width, fields = rebase(c["pattern"], c["msb"]), 16, []
        source, keys = "pgr (PRM figure is a copy of Type 25a rframe)", []
        notes.append("Type25c_rframe: 16-bit form from PGR")
    if name in PRM_VALUE_WINS:
        notes.append(f"{name}: fixed-bit values from PRM figure (overrides PGR; firmware-confirmed)")
    for fl in fields:
        if name == "Type4b" and fl["label"] == "dreg[6:0]":
            fl["label"] = "dreg[3:0]"
        if name == "Type17a" and fl["label"] == "i[2:0]":
            fl["label"] = "ureg[6:0]"
        fl["label"] = fl["label"].replace("data[5:5", "data[5:5]").replace("]]", "]")

    if name in DROP_FIELDS:
        dropped = sorted(DROP_FIELDS[name])
        fields = [fl for fl in fields if fl["label"] not in DROP_FIELDS[name]]
        notes.append(f"{name}: PRM field declarations {dropped} dropped so the "
                      "classic grid's fixed values at those bits are used")

    if name in FIELD_OVERRIDE:
        override = FIELD_OVERRIDE[name]
        fields = [fl for fl in fields if fl["label"] not in override["remove"]]
        fields += [dict(fl) for fl in override["add"]]
        fields.sort(key=lambda fl: -fl["hi"])
        notes.append(f"{name}: PRM fields {sorted(override['remove'])} replaced "
                      "with the classic grid's m[2:0], as in Type7b")

    fixed = {}
    unconfirmed = []
    if keys and name not in PRM_VALUE_WINS:
        variants = [rebase(classic[k]["pattern"], classic[k]["msb"]) for k in keys]
        field_bits = {b for fl in fields for b in range(fl["lo"], fl["hi"] + 1)}
        for b in range(47, 47 - width, -1):
            vals = {v.get(b) for v in variants}
            if b in field_bits:
                continue
            if len(vals) == 1 and vals & {"0", "1"}:
                fixed[b] = int(vals.pop())
            elif pb.get(b) in ("0", "1") and vals <= {".", None}:
                # PRM shades a bit the PGR leaves blank: expected value, not used to match.
                pass
        source = "pgr values + prm layout"
    elif keys:  # PRM_VALUE_WINS: fixed bits from the PRM figure, not the PGR.
        field_bits = {b for fl in fields for b in range(fl["lo"], fl["hi"] + 1)}
        for b in range(47, 47 - width, -1):
            if b in field_bits:
                continue
            if pb.get(b) in ("0", "1"):
                fixed[b] = int(pb[b])
        source = "prm figure (overrides PGR; firmware-confirmed)"
    else:
        for b in range(47, 47 - width, -1):
            if pb.get(b) in ("0", "1"):
                fixed[b] = int(pb[b])
                if source == "prm":
                    unconfirmed.append(b)
    if name in FULL_WORD:
        full_width, word, free = FULL_WORD[name]
        fixed = {b: (word >> b) & 1 for b in range(48 - full_width, 48)
                 if b not in free}
        source = "prm figure (every bit printed; the PGR grid leaves them blank)"
    if name in PIN_FIELDS:
        pinned = PIN_FIELDS[name]
        for fl in [fl for fl in fields if fl["label"] in pinned]:
            digits = pinned[fl["label"]]
            for offset in range(fl["hi"] - fl["lo"] + 1):
                fixed[fl["lo"] + offset] = (digits >> offset) & 1
        fields = [fl for fl in fields if fl["label"] not in pinned]
        unconfirmed = []
        source = (
            "prm figure and the Table 14-22 selector; the firmware cluster "
            "matches the documented ACONV rows"
        )
        notes.append(f"{name}: PRM Table 14-22 pins {sorted(pinned)}; the form "
                      "is Type 7a with condition 11111 and an empty compute")
    mask = sum(1 << b for b in fixed)
    value = sum(v << b for b, v in fixed.items())
    forms.append({
        "name": name, "width": width, "visa": name not in ISA_ONLY, "isa": width == 48,
        "mask": f"0x{mask:012x}", "value": f"0x{value:012x}", "fixed_bits": len(fixed),
        "fields": fields, "source": source, "classic_keys": list(keys),
        "unconfirmed_bits": len(unconfirmed),
    })

# The same public decoder independently agrees with the PRM-only Type12a UREG
# field layout on the exact loop words reached from reset, so that form no
# longer needs to carry the table builder's single-source marker.
type12a_ureg = next(f for f in forms if f["name"] == "Type12a_ureg")
type12a_ureg["source"] = "prm layout + public Selache decoder cross-check"
type12a_ureg["unconfirmed_bits"] = 0

# A public SHARC+ VISA decoder identifies the 0x02-prefixed 48-bit form as the
# no-memory immediate-shift instruction.  Its remaining fixed bits and field
# positions are identical to the PRM's Type6a no-memory figure, and its low
# 23-bit ShiftImm field renders through the PRM opcode table without any new
# semantic mapping.  This corrects the old firmware-only interpretation of the
# first 0x023e parcel as a standalone reserved 16-bit Type 23 instruction.
# See docs/sharc/SOURCES.md and docs/findings/06-sharc-engine-and-startup.md,
# "`0x023e` is the first parcel of a 48-bit immediate shift".
type6a_nomem = next(f for f in forms if f["name"] == "Type6a (nomem)")
forms.append({
    **type6a_nomem,
    "name": "Type6b_shiftimm",
    "visa": True,
    "isa": False,
    "value": "0x020000000000",
    "source": "public Selache VISA decoder, cross-checked against PRM Type6a shiftimm",
    "classic_keys": [],
    "unconfirmed_bits": 0,
})
notes.append(
    "Type6b_shiftimm: public decoder selects 48 bits for the 0x02 prefix; "
    "the PRM Type6a no-memory mask and ShiftImm fields then decode the operation"
)

# The SHARC+ Type19a BH table assigns sc=01 to enhanced immediate MODIFY and
# uses bit 39 to choose SW/NW scaling.  The public Selache decoder independently
# confirms the remaining PRM field layout.  Keep the ordinary sc=10 Type19a
# above for classic compatibility and emit the scaled form explicitly.
forms.append({
    "name": "Type19a_scaled",
    "width": 48,
    "visa": True,
    "isa": True,
    "mask": "0xff0000000000",
    "value": "0x150000000000",
    "fixed_bits": 8,
    "fields": [
        {"label": "w", "hi": 39, "lo": 39},
        {"label": "g", "hi": 38, "lo": 38},
        {"label": "idis[2:0]", "hi": 37, "lo": 35},
        {"label": "is[2:0]", "hi": 34, "lo": 32},
        {"label": "data[31:16]", "hi": 31, "lo": 16},
        {"label": "data[15:0]", "hi": 15, "lo": 0},
    ],
    "source": "prm Type19a layout/BH table + public Selache decoder cross-check",
    "classic_keys": [],
    "unconfirmed_bits": 0,
})
notes.append(
    "Type19a_scaled: PRM Type19a BH sc=01 form; bit 39 selects SW/NW scaling"
)

# VISA width for the 0x01 prefix: bit 39 (bit 7 of the first parcel) selects a
# 32-bit single-function compute; bit 39 = 0 keeps the 48-bit conditional
# Type2a.  The public Selache decoder implements this rule for every 0x01
# instruction, and its 32-bit compute field is ((parcel1 & 0x7f) << 16) |
# parcel2, the same bits 38:16 as the PRM Type2b figure.  In Digitakt II 1.16,
# 1,880 of 2,396 aligned Type2a words have bit 39 set; 99.1% of those are
# followed by a confident instruction 4 bytes later, against 49.0% 6 bytes
# later.  This is not the PRM's Type2b (prefix 0xc0), which keeps its name.
# The longer fixed lead wins over Type2a in VISA decode; classic ISA keeps
# Type2a.  See docs/sharc/SPEC-FINDINGS.md and docs/FINDINGS.md.
forms.append({
    "name": "Type2a_short",
    "width": 32,
    "visa": True,
    "isa": False,
    "mask": "0xff8000000000",
    "value": "0x018000000000",
    "fixed_bits": 9,
    "fields": [
        {"label": "compute[22:16]", "hi": 38, "lo": 32},
        {"label": "compute[15:0]", "hi": 31, "lo": 16},
    ],
    "source": "public Selache VISA width rule + PRM compute tables; firmware-checked",
    "classic_keys": [],
    "unconfirmed_bits": 0,
})
notes.append(
    "Type2a_short: 0x01 prefix with bit 39 set is a 32-bit unconditional "
    "compute (Selache VISA width rule); bit 39 clear keeps 48-bit Type2a"
)

def undocumented_form(entry):
    prefix_bits = entry["prefix_bits"]
    mask = 0
    value = 0
    for i, ch in enumerate(prefix_bits):
        b = 47 - i
        mask |= 1 << b
        if ch == "1":
            value |= 1 << b
    extra_fixed = entry.get("extra_fixed", {})
    for bit, val in extra_fixed.items():
        mask |= 1 << bit
        if val:
            value |= 1 << bit
    fixed_bits = len(prefix_bits) + len(extra_fixed)
    width = entry.get("width", 16)
    # A prefix filling the first word leaves no room for an operand; a wider
    # form leaves its later words unlabelled, as the wildcard forms do.
    fields = entry.get("fields")
    if fields is None:
        fields = ([] if fixed_bits >= 16 else
                  [{"label": f"operand[{15 - fixed_bits}:0]", "hi": 47 - fixed_bits, "lo": 32}])
    return {
        "name": entry["name"], "width": width, "visa": True, "isa": width == 48,
        "mask": f"0x{mask:012x}", "value": f"0x{value:012x}", "fixed_bits": fixed_bits,
        "fields": fields,
        "source": entry.get("source", "firmware (undocumented; unconfirmed)"),
        "classic_keys": [],
        "unconfirmed_bits": fixed_bits,
    }


for entry in UNDOCUMENTED:
    forms.append(undocumented_form(entry))
    notes.append(f"{entry['name']}: {entry['note']}")

json.dump({"forms": forms, "notes": notes}, open("decode_table.json", "w"), indent=1)
for fm in forms:
    flag = "  UNCONFIRMED" if fm["unconfirmed_bits"] else ""
    print(f"{fm['name']:18s} {fm['width']:2d}b visa={fm['visa']!s:5} mask {fm['mask']} value {fm['value']} ({fm['fixed_bits']} fixed) {fm['source']}{flag}")
