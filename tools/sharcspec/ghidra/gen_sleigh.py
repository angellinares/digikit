#!/usr/bin/env python3
"""Generate a Ghidra SLEIGH processor module for SHARC+ VISA from decode_table.json.

CLEAN-ROOM: this generator reads decode_table.json (our validated form/field
table) and embeds the UREG names transcribed from the public SHARC+ PRM UREG and
SYSREG tables (pp. 26-12--26-16). It emits SLEIGH source using constructs
documented in Ghidra's own SLEIGH reference (docs/languages/html/sleigh_*.html).
It does not read or copy anything from the prior-art SHARC processor module
shipped with Ghidra.

======================================================================
BIT-MAPPING (the crux of this generator)
======================================================================
decode_table.json gives every form's mask/value/fields as bit positions in a
48-bit MSB-aligned "frame" built by our oracle decoder as:

    frame = (w0 << 32) | (w1 << 16) | w2

where w0,w1,w2 are successive 16-bit LITTLE-ENDIAN words from the instruction
stream (w0 = first word in memory = most-significant 16 bits of the frame).
So frame bit 47 = MSB of w0 ... frame bit 32 = LSB of w0 ; bit 31 = MSB of w1
... bit 16 = LSB of w1 ; bit 15 = MSB of w2 ... bit 0 = LSB of w2.

We define three 16-bit SLEIGH tokens word0, word1, word2 (endian little, so
each token's integer value already has bit15=MSB..bit0=LSB of that word,
matching frame bits 47..32 / 31..16 / 15..0 respectively with NO extra
flip needed). For a frame bit b:

    word_index      = (47 - b) // 16          # 0,1,2
    word_base_lo(w)  = 32 - 16*w               # 32,16,0
    in_word_bit(b)   = b - word_base_lo(word_index)   # 0(lsb)..15(msb)

This is a direct, order-preserving affine map -- confirmed against Ghidra's own
TriCore module's documented idiom of chaining same-size tokens with the ';'
pattern-concatenation operator (see sleigh_constructors.html sec 7.4.4.1): when
a constructor's pattern is "word0terms ; word1terms ; word2terms", SLEIGH
consumes word0's 2 bytes, then word1's 2 bytes, then word2's 2 bytes, in that
order -- exactly the MSB-first word order our oracle uses.

======================================================================
WIDTH / FORM SELECTION
======================================================================
Our oracle picks the matching form with the longest leading run of fixed mask
bits from bit 47 down, tie-broken by total fixed-bit count (popcount(mask)).
We verified computationally that fixed_bits == popcount(mask) always, and that
mask bits and field bits never overlap for any form. Because every form's
*full* mask (not just the leading run) must match (frame & mask == value) for
it to even be a candidate, encoding every form as one SLEIGH constructor whose
pattern pins down exactly its mask bits (as constant-valued fields) and leaves
its operand fields free reproduces the oracle's full-match test. SLEIGH's own
decision-tree constructor-matcher (built for exactly this kind of variable-
length, prefix-structured ISA -- see sec 7.4.4) resolves any remaining overlap
between two forms in favor of whichever constructor has more bits pinned down,
which is exactly our tie-break rule. We do not need context variables or
manual priorities for this.

======================================================================
ADDRESSING CHOICE
======================================================================
We give the "ram" code space wordsize=2 (address unit = 1 16-bit word) and
alignment=1 (in that space's own units -- every word is a valid instruction
start, since 16-bit forms exist). A form's absolute `addr` field (confirmed
against firmware to be a plain SW word address -- see report) is then used
directly as a branch target with NO scaling. inst_next / inst_start are
automatically expressed in the same word-address units by SLEIGH, so
PC-relative `reladdr` forms add directly too. The firmware loader must load
bytes at the SW word address = (BW_load - 0x28000000) // 2.

======================================================================
SCOPE (disassembly-first, per task spec)
======================================================================
 - One constructor per VISA form (Type10a is visa:false and is excluded).
 - Mnemonic: form name by default; real mnemonics (jump/call/rts/rframe/nop)
   only where the form's fields make the meaning unambiguous.
 - Real p-code (goto/call/return) for every control-flow form that carries a
   statically resolvable address: Type8a_abs/Type8a_rel/Type9a_rel/Type9b_rel
   (split call/jump on their `b` bit; see decode_table.json/build_table.py
   SPLIT_FORMS for why these are separate abs/rel forms rather than one
   merged form with a live selector field), Type25a_direct (absolute),
   Type25a_pcrel (pc-relative). Type25a is CJUMP, which the manual defines
   as a delayed call only, so both forms get `call`; the two delay-slot
   instructions that follow it (the compiler's push of R2 and store of the
   return address) are not modelled and appear after the call.
   PC-relative targets are computed as
   inst_start + signed(reladdr) -- relative to the branch instruction's OWN
   address, not the next instruction -- confirmed against firmware (see task
   report: this base landed 79.4% vs. 42.1% for "relative to next
   instruction" on Type8a_rel alone, and matches fw/validate.py's
   independently-derived convention). Type9a_abs/Type9b_abs (register-
   indirect via PMI/PMM pointer+modify registers) have no static target
   (computing one needs the DAG register file, out of scope for this pass)
   but still get real flow-shape p-code: the call variant gets NO p-code (a
   call always falls through after it returns, which is exactly SLEIGH's
   default for an instruction with none, so this is already correct) and
   the jump variant gets `return [0:4];` so Ghidra doesn't treat whatever
   bytes follow as this instruction's fallthrough. Type11a, Type11c,
   Type25a_rframe and Type25c_rframe are register-indirect/implicit returns
   with no encoded target, so they also get `return [0:4];` (marks the
   control-flow edge for Ghidra's function/block analysis; target is NOT
   resolved). Every other form gets an empty (but present, i.e.
   "implemented") {} body.
 - A jump, call or return with a cond field (Type8a, Type9a, Type9b jumps
   and calls with p-code, Type11a, Type11c) gets two constructors. With cond
   TRUE (0x1f, PGR Table 10-4) it keeps the p-code above. With any other
   cond it acts only when `condition(cond)` holds and otherwise falls
   through: a goto becomes `if (holds) goto`, and a call or return is preceded
   by `if (!holds) goto inst_next;`. `condition` is a user-defined p-code op
   because the status flags are not modelled yet. The TRUE constructor
   constrains `condtrue`, a second field over the cond bits, because a field
   cannot be both displayed and constrained in one constructor; the extra
   constraint makes it the more specific match.
 - Compute/shiftimm/short-compute fields and every other non-address operand
   are rendered as raw hex annotations (their own SLEIGH field, printed in
   hex) -- decompiler-grade arithmetic p-code for the 23-bit compute field is
   explicitly out of scope for this pass (see task spec).
 - Registers: R0-R15 and F0-F15 (separate namespaces, same idea as
   fixed/float aliasing but not literally aliased -- simplification), I0-I15,
   M0-M15, L0-L15, B0-B15, S0-S15, and the UREG system registers are declared.
   The unambiguous 4-bit dreg/cdreg fields attach to R0-R15, and complete,
   unsplit 7-bit ureg fields attach to the public-manual UREG table. Other
   register selects (srcureg/dstureg/cureg, sreg, and index-register selects
   mixed with DAG-group bits) remain raw hex fields.
"""

import json
import os
import re
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
SPEC_DIR = os.path.dirname(HERE)
OUT_DIR = os.path.join(HERE, "SHARC_VISA", "data", "languages")


# ----------------------------------------------------------------------
# Frame-bit <-> (word, in-word bit) mapping
# ----------------------------------------------------------------------
def word_of(bit):
    return (47 - bit) // 16


def word_base_lo(word):
    return 32 - 16 * word


def in_word(bit, word):
    return bit - word_base_lo(word)


def split_by_word(hi, lo):
    """Split a frame-absolute [hi:lo] bit range into per-word (word, hi, lo)
    chunks (in-word bit numbers), in case it straddles a word boundary."""
    chunks = []
    w0, w1 = word_of(hi), word_of(lo)
    for w in range(w0, w1 + 1):
        whi_frame = word_base_lo(w) + 15
        wlo_frame = word_base_lo(w)
        chi = min(hi, whi_frame)
        clo = max(lo, wlo_frame)
        chunks.append((w, in_word(chi, w), in_word(clo, w)))
    return chunks


# ----------------------------------------------------------------------
# Field registry: dedup SLEIGH token-field declarations by
# (word, hi, lo, signed) so identical bit ranges used by different forms
# share one field definition (required -- SLEIGH field names must be unique
# per token, and re-declaring the same range under two names is wasteful).
# ----------------------------------------------------------------------
class FieldRegistry:
    def __init__(self):
        self.by_key = {}  # (word,hi,lo,signed) -> name
        self.order = {0: [], 1: [], 2: []}  # word -> [(name,hi,lo,signed)]
        self.used_names = set()

    def sanitize(self, label):
        base = re.sub(r"\[.*?\]", "", label)
        base = re.sub(r"[^A-Za-z0-9]+", "_", base).strip("_")
        return base or "f"

    def get(self, word, hi, lo, label, signed=False):
        base = self.sanitize(label)
        # Dedup key includes the label's base name (not just bit position):
        # two forms sometimes reuse the same bit position for genuinely
        # different fields (e.g. Type8a's `r` bit coincides with another
        # form's `g` bit), and giving them the same SLEIGH field name would
        # make the disassembly display misleading even though the extracted
        # bits are numerically identical either way.
        key = (word, hi, lo, signed, base)
        if key in self.by_key:
            return self.by_key[key]
        suffix = "s" if signed else ""
        name = f"{base}_w{word}_{hi}_{lo}{suffix}"
        n = 2
        while name in self.used_names:
            name = f"{base}_w{word}_{hi}_{lo}{suffix}_{n}"
            n += 1
        self.used_names.add(name)
        self.by_key[key] = name
        self.order[word].append((name, hi, lo, signed))
        return name

    def emit_token(self, word):
        lines = [f"define token word{word} (16)"]
        for name, hi, lo, signed in self.order[word]:
            attrs = " signed" if signed else ""
            lines.append(f"    {name} = ({lo},{hi}){attrs}")
        return "\n".join(lines) + "\n;\n"


FIELDS = FieldRegistry()


# ----------------------------------------------------------------------
# Load decode table
# ----------------------------------------------------------------------
def load_forms():
    d = json.load(open(os.path.join(SPEC_DIR, "decode_table.json")))
    return d["forms"]


def sanitize_ident(name):
    """SLEIGH identifier / mnemonic-safe name from a form name like
    'Type5a (swap)' -> 'Type5a_swap'."""
    name = name.replace("(", "").replace(")", "")
    name = re.sub(r"\s+", "_", name.strip())
    name = re.sub(r"[^A-Za-z0-9_]", "_", name)
    return name


_LABEL_RANGE_RE = re.compile(r"^(\w+)\[(\d+):(\d+)\]$")


def label_base_and_shift(label):
    """For a split-field label like 'addr[23:16]' return (base, shift=16).
    For an unsplit label like 'addr' or a flag like 'd', return (label, 0)."""
    m = _LABEL_RANGE_RE.match(label)
    if m:
        return m.group(1), int(m.group(3))
    return label, 0


# ----------------------------------------------------------------------
# Per-form constructor generation
# ----------------------------------------------------------------------
class Constructor:
    """Holds everything needed to emit one SLEIGH constructor line."""

    def __init__(
        self,
        mnemonic,
        display_ops,
        word_terms,
        nwords,
        disasm_actions=None,
        semantic_lines=None,
        extra_decl=None,
        active_words=None,
    ):
        self.mnemonic = mnemonic
        self.display_ops = display_ops  # list of operand identifiers (for display)
        self.word_terms = word_terms  # dict word-> [pattern terms]
        self.nwords = nwords
        self.disasm_actions = disasm_actions or []  # lines inside [ ... ]
        self.semantic_lines = semantic_lines or []  # lines inside { ... }
        self.extra_decl = (
            extra_decl or []
        )  # e.g. "local tmp:4;" style decls only needed inline
        # Which words get their own explicit ";"-separated pattern group.
        # Normally every word 0..nwords-1 (the default). A branch form whose
        # target subtable's OWN span swallows a MIDDLE word (e.g. Type9a_rel:
        # the subtable starts at word0 and reaches into word1, but word2
        # still follows) must omit that middle word here -- it's already
        # consumed as part of the subtable reference placed at its start
        # word, and emitting a second explicit group for it would ask SLEIGH
        # to consume those bits twice. See gen_constructor's `swallowed`
        # computation.
        self.active_words = (
            active_words if active_words is not None else list(range(nwords))
        )
        # Force token consumption even for a word that's entirely "don't
        # care" for this form: reference a full-word wildcard field bare
        # (unconstrained) so SLEIGH still advances past it. Must happen here
        # (construction time), not lazily in emit()/pattern_str(), because
        # all token blocks are written out before any constructor's text is
        # emitted -- registering a brand-new field that late would produce a
        # pattern that references a field never declared in its token.
        for w in self.active_words:
            if not self.word_terms.get(w):
                self.word_terms[w] = [FIELDS.get(w, 15, 0, "wildcard")]

    def pattern_str(self):
        return " ; ".join(" & ".join(self.word_terms[w]) for w in self.active_words)

    def emit(self):
        disp = self.mnemonic
        if self.display_ops:
            disp += " " + ",".join(self.display_ops)
        pat = self.pattern_str()
        out = f":{disp} is {pat}"
        if self.disasm_actions:
            out += " [ " + " ".join(self.disasm_actions) + " ]"
        body = "\n    ".join(self.semantic_lines)
        out += " {\n    " + body + "\n}\n" if self.semantic_lines else " {\n}\n"
        return out


def build_field_terms(form, width, exclude_labels=(), no_bare_labels=()):
    """Return dict word-> [pattern terms] for a form's MASK (fixed bits, as
    constant constraints) and FIELDS (as bare operand references), plus a
    dict label-> (display_name, per-word fragments) for building composite
    operands (addr/reladdr/compute/etc split across words).

    `no_bare_labels` fields are still registered/returned in field_info (the
    caller needs their (word,hi,lo)) but are NOT added to word_terms as a bare
    (unconstrained) pattern term -- used for a field the caller will instead
    pin to a specific value itself (e.g. Type8a's `r` bit, which selects
    jump vs call), since a field cannot be both bare and value-constrained in
    the same pattern."""
    mask = int(form["mask"], 16)
    value = int(form["value"], 16)
    lowest = 48 - width
    word_terms = {w: [] for w in range(3)}

    # --- fixed (mask) bits: contiguous runs, split at word boundaries ---
    b = 47
    while b >= lowest:
        if (mask >> b) & 1:
            hi = b
            while b - 1 >= lowest and ((mask >> (b - 1)) & 1):
                b -= 1
            lo = b
            for w, chi, clo in split_by_word(hi, lo):
                fname = FIELDS.get(w, chi, clo, "fx")
                frame_hi = word_base_lo(w) + chi
                frame_lo = word_base_lo(w) + clo
                const = (value >> frame_lo) & ((1 << (frame_hi - frame_lo + 1)) - 1)
                word_terms[w].append(f"{fname}=0x{const:x}")
        b -= 1

    # --- variable fields: bare operand references, split at word boundaries ---
    field_info = {}  # label -> list of (word, fname, shift, nbits, hi, lo)
    for fl in form["fields"]:
        label = fl["label"]
        if label in exclude_labels:
            continue
        hi, lo = fl["hi"], fl["lo"]
        base, shift = label_base_and_shift(label)
        chunks = split_by_word(hi, lo)
        frags = []
        for w, chi, clo in chunks:
            fname = FIELDS.get(w, chi, clo, label)
            if label not in no_bare_labels:
                word_terms[w].append(fname)
            frags.append((w, fname, clo, chi - clo + 1))
        field_info[label] = (base, shift, frags, hi, lo)

    return word_terms, field_info


def reassemble_expr(frags_sorted):
    """Build a '(a << n) | (b << m) | c' disassembly-action expression from
    high-to-low ordered fragment dicts (each with 'fname' and 'shift')."""
    terms = []
    for frag in frags_sorted:
        fname, shift = frag["fname"], frag["shift"]
        terms.append(f"({fname} << {shift})" if shift else fname)
    return " | ".join(terms)


# ----------------------------------------------------------------------
# Control-flow special-casing
# ----------------------------------------------------------------------
# Forms with a statically resolvable branch/call target. `mode` is
# "abs" (target = combined field value, word address) or
# "pcrel" (target = inst_start + signed combined field value -- relative to
# THIS instruction's own address; see module docstring for the firmware
# evidence). `split_field`/`split_map`, when present, pick the mnemonic
# (jump/call) from a live field's value (Type8a/9a/9b's `b` bit -- see
# their PRM JUMPCLAUSE encode tables: b=0 jump, b=1 call); forms without a
# call variant (Type25a) just give a fixed `mnemonic` instead.
BRANCH_FORMS = {
    "Type8a_abs": dict(
        target_label="addr",
        mode="abs",
        split_field="b",
        split_map={0: "jump", 1: "call"},
    ),
    "Type8a_rel": dict(
        target_label="reladdr",
        mode="pcrel",
        split_field="b",
        split_map={0: "jump", 1: "call"},
    ),
    "Type9a_rel": dict(
        target_label="reladdr",
        mode="pcrel",
        split_field="b",
        split_map={0: "jump", 1: "call"},
    ),
    "Type9b_rel": dict(
        target_label="reladdr",
        mode="pcrel",
        split_field="b",
        split_map={0: "jump", 1: "call"},
    ),
    # CJUMP is always a delayed call (SHARC+ Core Programming Reference, Type 25a).
    "Type25a_direct": dict(target_label="addr", mode="abs", mnemonic="call"),
    "Type25a_pcrel": dict(target_label="reladdr", mode="pcrel", mnemonic="call"),
}

# Register-indirect jump/call forms (Type9a_abs/Type9b_abs -- the r=0/rel=0
# variant of the pair above: PMI selects a pointer I-register, PMM a modify
# M-register, per their PRM ADDRCLAUSE tables). No static target -- fully
# resolving one needs the DAG register file, explicitly out of scope for
# this pass (see module docstring) -- but the mnemonic (and hence whether
# the instruction falls through) is still known from the same `b` bit.
INDIRECT_BRANCH_FORMS = {
    "Type9a_abs": dict(split_field="b", split_map={0: "jump", 1: "call"}),
    "Type9b_abs": dict(split_field="b", split_map={0: "jump", 1: "call"}),
}

# Forms that are register-indirect / implicit returns: no encoded target,
# but we still want Ghidra to see a RETURN so it terminates the block.
RETURN_FORMS = {
    "Type11a": "rts",
    "Type11c": "rts",
    "Type25a_rframe": "rframe",
    "Type25c_rframe": "rframe",
}

# High-confidence real mnemonics for otherwise-generic forms.
NOP_FORMS = {"Type21a": "nop"}

COND_TRUE = 0x1F  # PGR Table 10-4: TRUE (FOREVER)


def cond_chunk(field_info):
    """-> (word, field name, lo, nbits) of a form's cond field, or None."""
    for base, _shift, chunks, _hi, _lo in field_info.values():
        if base == "cond" and len(chunks) == 1:
            return chunks[0]
    return None


def conditional_semantics(cond_fname, semantic):
    """-> p-code lines running the single flow statement in `semantic` only
    when condition(cond) holds, falling through otherwise."""
    (stmt,) = semantic
    head = [f"local code:4 = {cond_fname};", "local holds:1 = condition(code);"]
    if stmt.startswith("goto "):
        return head + [f"if (holds) {stmt}"]
    return head + ["if (!holds) goto inst_next;", stmt]


def constrained_terms(word_terms, field_info, constraints, tag):
    """Clone `word_terms` and pin `constraints` using private pattern fields."""
    result = {word: list(terms) for word, terms in word_terms.items()}
    for label, value in constraints:
        _base, _shift, chunks, _hi, _lo = field_info[label]
        assert len(chunks) == 1
        word, _fname, clo, nbits = chunks[0]
        alias = FIELDS.get(word, clo + nbits - 1, clo, tag)
        result[word].append(f"{alias}=0x{value:x}")
    return result


def dm_byte_addr_to_ram_unit(addr_name="addr", isl2_name="isl2", unit_name="unit"):
    """SLEIGH lines translating a SHARC DM BYTE address (already in local
    `addr_name`) into a unit offset in the `ram` space (local `unit_name`),
    for use as `*[ram]:4 {unit_name}`.

    The generated `ram` space has wordsize=2 (so short-word CODE addresses
    land on their own bytes: unit n -> Ghidra byte offset 2n), but DM
    literals in the instruction stream are byte addresses, not short-word
    ones. Ghidra always scales a LOAD/STORE offset by the space's wordsize,
    so using the byte address directly as the offset would access byte
    2*addr instead of addr. Dividing by 2 here (`addr >> 1`) undoes that
    scaling for the ordinary case, so Ghidra byte offset = addr, matching
    the importer's placement of on-chip and external DM literals at their
    own byte address (tools/sharc_import.py `ghidra_addr`).

    The one exception is the loader's bounded L2 byte window
    (0x20000000..0x20020000 exclusive), which is NOT identity-mapped: it
    aliases short-word range 0x00B80000.. (tools/sharcldr.py L2_BYTE_BASE/
    L2_BYTE_LIMIT/L2_SW_BASE). For an address in that window the unit is
    L2_SW_BASE + (addr - L2_BYTE_BASE)/2, which equals (addr >> 1) +
    (L2_SW_BASE - L2_BYTE_BASE/2) = (addr >> 1) + 0xF0B80000 (32-bit
    two's-complement: 0x00B80000 - 0x10000000 mod 2**32). `isl2` is that
    window's indicator (1 inside, 0 outside), so the same expression covers
    both cases without a conditional branch in the semantics.

    DM only (g==0): PM data addresses are 48-bit-word block addresses, not
    byte addresses, and are not translated by this helper. Both call sites
    below (type14a_scalar_constructors, type3b_exact_constructors) already
    pin g=0.

    Odd DM literals lose their low bit here (`>> 1` truncates); every
    Type14a DM literal actually present in the DT2 1.16 main program is
    even (verified by tools/sharcinv.py; see docs/findings/
    05-sharc-isa-and-decoding.md), so this is a noted limitation rather
    than an observed bug.
    """
    return [
        f"local {isl2_name}:4 = zext(({addr_name} >= 0x20000000) "
        f"&& ({addr_name} < 0x20020000));",
        f"local {unit_name}:4 = ({addr_name} >> 1) + {isl2_name} * 0xF0B80000;",
    ]


# ----------------------------------------------------------------------
# Indexed DM memory forms (Type15b, Type4a, Type3a): shared p-code builders.
#
# All three address DM through a DAG1 (g==0) index register I[i]. Type4a
# and Type3a additionally support post-modify addressing (u==1), which can
# update I[i] by a signed offset -- but only when the paired L register is
# zero; when L[i] != 0 circular buffering is active for that index and the
# wrap is not modelled (see circular_guard). The public PRM confirms
# circular addressing applies only to post-modify updates, never pre-modify
# (out/refs/sc58x-2158x-prm/all.txt, "Circular Buffering Mode": "Circular
# buffering starting at any address may only use post-modify addressing.")
# ----------------------------------------------------------------------

# tag -> SLEIGH field name of a private, register-bank-attached alias over
# an `i`/`m` field's bits (see bank_field()). Populated while generating
# Type15b/4a/3a's constructors; emitted as `attach variables` lines in
# main() once every form has been processed.
BANK_ALIASES = {}
BANK_REGISTER_LISTS = {
    "ibank": [f"I{i}" for i in range(8)],
    "lbank": [f"L{i}" for i in range(8)],
    "mbank": [f"M{i}" for i in range(8)],
}


def field_fname(field_info, label):
    """-> the SLEIGH field name of a label already registered as exactly one
    (word, hi, lo) chunk (not split across words)."""
    _base, _shift, chunks, _hi, _lo = field_info[label]
    assert len(chunks) == 1, f"{label}: expected a single-word field"
    _word, fname, _clo, _nbits = chunks[0]
    return fname


def field_word(field_info, label):
    """-> the token word of a label already registered as a single chunk."""
    _base, _shift, chunks, _hi, _lo = field_info[label]
    assert len(chunks) == 1, f"{label}: expected a single-word field"
    return chunks[0][0]


def index_alias(field_info, label, tag):
    """-> a NEW, unconstrained SLEIGH field over the same bits as
    field_info[label], registered under `tag` so it is a distinct token
    field from the original (bare, displayed) one -- lets the same raw code
    be read once as a plain integer and again as a register-bank selector
    (see bank_field), the same trick constrained_terms uses for pinned
    values, just left bare here instead of constrained."""
    _base, _shift, chunks, _hi, _lo = field_info[label]
    assert len(chunks) == 1, f"{label}: expected a single-word field"
    word, _fname, clo, nbits = chunks[0]
    return FIELDS.get(word, clo + nbits - 1, clo, tag)


def bank_field(field_info, label, tag):
    """Register (once; FIELDS.get dedups by bit position) the `tag`-named
    alias of field_info[label] and remember it in BANK_ALIASES so main()
    attaches it to the right register bank. `i` sits at the same frame
    position in every indexed memory form, so repeated calls across
    Type15b/4a/3a resolve to the same physical field and a single attach."""
    name = index_alias(field_info, label, tag)
    BANK_ALIASES[tag] = name
    return name


def with_bare(word_terms, *additions):
    """Clone `word_terms` and add each (word, fname) as an extra
    unconstrained pattern term -- for a private alias field (e.g. a
    register-bank selector from bank_field) that isn't already present in
    the generic pattern built by build_field_terms."""
    result = {word: list(terms) for word, terms in word_terms.items()}
    for word, fname in additions:
        result.setdefault(word, []).append(fname)
    return result


def split_sign_magnitude(field_info, label, sign_tag, mag_tag):
    """-> (word, sign_fname, mag_fname, mag_bits) for a field whose top bit
    is a sign and the remaining low bits are an unsigned magnitude (SHARC+'s
    usual small-signed-immediate encoding, e.g. Type15b's 7-bit `data`).
    Registers two NEW fields over the SAME bits as field_info[label] (see
    index_alias) so both are available as bare operands alongside the
    original combined field."""
    _base, _shift, chunks, _hi, _lo = field_info[label]
    assert len(chunks) == 1, f"{label}: expected a single-word field"
    word, _fname, clo, nbits = chunks[0]
    mag_bits = nbits - 1
    sign_fname = FIELDS.get(word, clo + nbits - 1, clo + mag_bits, sign_tag)
    mag_fname = FIELDS.get(word, clo + mag_bits - 1, clo, mag_tag)
    return word, sign_fname, mag_fname, mag_bits


def sign_magnitude_offset(name, sign_fname, mag_fname, mag_bits, scale):
    """P-code lines computing `name`:4 = ((magnitude) - 2**mag_bits *
    (sign bit)) * scale -- the exact two's-complement value of a
    [1 sign bit, mag_bits-bit magnitude] split immediate, pre-multiplied by
    the DM access scale (normal words are 4 bytes here; see module
    docstring). This identity holds because an n-bit two's-complement value
    is -2**(n-1)*sign + (the low n-1 bits read as unsigned).

    A bare token field has no inherent byte size when it isn't a whole
    number of bytes wide, so `zext()` can't take one as its input directly
    (sleigh: "Could not resolve at least 1 variable size") -- first copy it
    into an explicitly-sized local (same idiom as Type17a/b's high16/low16),
    then zext that."""
    return [
        f"local {name}_signraw:1 = {sign_fname};",
        f"local {name}_magraw:1 = {mag_fname};",
        f"local {name}_sign:4 = zext({name}_signraw);",
        f"local {name}_mag:4 = zext({name}_magraw);",
        f"local {name}:4 = ({name}_mag - ({name}_sign << {mag_bits})) * {scale};",
    ]


def compute_marker_lines(hi_fname, lo_fname, local="cmp", skip_label="nocompute"):
    """Combine a 23-bit `compute` field's word1 (bits 22:16, 7 bits) and
    word2 (bits 15:0, 16 bits) chunks into one 4-byte local and call the
    opaque `compute` marker when it is nonzero, skipping it otherwise.
    SHARC runs compute in parallel with the surrounding transfer, reading
    the pre-transfer register values; since `compute` models no effect at
    all, running this check after the transfer in the emitted p-code changes
    nothing observable."""
    return [
        f"local {local}hi:1 = {hi_fname};",
        f"local {local}lo:2 = {lo_fname};",
        f"local {local}:4 = (zext({local}hi) << 16) | zext({local}lo);",
        f"if ({local} == 0) goto <{skip_label}>;",
        f"compute({local});",
        f"<{skip_label}>",
    ]


def circular_guard(ibank_fname, lbank_fname, i_index_local, off_local,
                    plain_label="pmplain", done_label="pmdone"):
    """P-code lines for a post-modify (u==1) index-register update. When the
    paired L register is nonzero, circular buffering is active for this
    index and the wrap is not modelled here, so the update is deferred
    entirely to the opaque `circular` marker (argument: `i_index_local`, an
    ALREADY-sized local holding the raw index register number, not its
    content -- see sized_bit) and the plain update is skipped. Otherwise
    this is an ordinary linear post-modify: I = I + offset."""
    return [
        f"if ({lbank_fname} == 0) goto <{plain_label}>;",
        f"circular({i_index_local});",
        f"goto <{done_label}>;",
        f"<{plain_label}>",
        f"{ibank_fname} = {ibank_fname} + {off_local};",
        f"<{done_label}>",
    ]


def sized_bit(local_name, fname, size=1):
    """P-code line copying a bare (possibly sub-byte) token field into an
    explicitly-sized local. A bare field has no inherent byte size when its
    width isn't a whole number of bytes, so it can't be handed directly to
    `zext()` or to a pcodeop as an argument (sleigh: "Could not resolve at
    least 1 variable size") -- this is the same "materialize, then use"
    step sign_magnitude_offset already needs for the same reason."""
    return [f"local {local_name}:{size} = {fname};"]


def direction_branch(d_fname, load_stmt, store_stmt,
                      load_label="isload", done_label="iodone"):
    """P-code lines selecting a LOAD or STORE at runtime from the bare `d`
    field's raw value (0 = load, 1 = store), instead of a second SLEIGH
    constructor per direction. Type3a/4a keep exactly two constructors each
    (cond TRUE / not) so cond, u and d together don't multiply into extra
    constructors that would each separately cross Type3b/3d or Type4b/4d
    (see the module note above type15b_indexed_constructors)."""
    return sized_bit("draw", d_fname) + [
        "local dmod:4 = zext(draw);",
        f"if (dmod == 0) goto <{load_label}>;",
        store_stmt,
        f"goto <{done_label}>;",
        f"<{load_label}>",
        load_stmt,
        f"<{done_label}>",
    ]


def premodify_or_post(ibank_fname, off_local, u_fname, u_local="umod"):
    """P-code lines computing `addr`:4 = I[i], pre-modified by `off_local`
    when u==0 and left unchanged when u==1 -- purely arithmetically
    (multiplying the offset by 0 or 1, read from the bare `u` field's raw
    value), so there is exactly one `local addr` declaration regardless of
    u. `u_local` (0 or 1) is left available for the caller's post-modify
    update gate (see postmodify_update) instead of re-deriving it."""
    return sized_bit("uraw", u_fname) + [
        f"local {u_local}:4 = zext(uraw);",
        f"local unot:4 = 1 - {u_local};",
        f"local addr:4 = {ibank_fname} + {off_local} * unot;",
    ]


def postmodify_update(ibank_fname, lbank_fname, i_raw_fname, off_local,
                       u_local="umod", skip_label="noupdate"):
    """P-code lines performing the post-modify (u==1) index-register update
    -- circular-guarded -- gated by `u_local` (already computed by
    premodify_or_post: 0 when u==0, 1 when u==1), so the update is skipped
    entirely, not just made a no-op, when u==0 (pre-modify forms never
    update I)."""
    return (
        sized_bit("pmidx", i_raw_fname)
        + [f"if ({u_local} == 0) goto <{skip_label}>;"]
        + circular_guard(ibank_fname, lbank_fname, "pmidx", off_local)
        + [f"<{skip_label}>"]
    )


def whole_instruction_cond_gate(cond_fname):
    """P-code lines gating an ENTIRE instruction body behind `condition()`.
    Unlike conditional_semantics (a single flow statement for a branch
    form), a memory form's condition covers address computation, the
    transfer, the compute marker and any post-modify update -- so the whole
    body is skipped by falling through to inst_next when it does not hold."""
    return [
        f"local code:4 = {cond_fname};",
        "local holds:1 = condition(code);",
        "if (!holds) goto inst_next;",
    ]


def type15b_indexed_constructors(
    mnem, disp_ops, word_terms, nwords, active_words, field_info
):
    """Type15b indexed DM load/store: addr = I[i] + sext7(data)*4. Pre-modify
    only -- I is never updated. g=0, l=0 only (PM and long-word variants keep
    the generic empty body from gen_constructor's own fallback Constructor)."""
    ibank = bank_field(field_info, "i[2:0]", "ibank")
    ibank_word = field_word(field_info, "i[2:0]")
    data_word, sign_fname, mag_fname, mag_bits = split_sign_magnitude(
        field_info, "data[6:0]", "data15b_sign", "data15b_mag"
    )
    ureg = field_fname(field_info, "ureg[6:0]")
    offset = sign_magnitude_offset("off", sign_fname, mag_fname, mag_bits, 4)
    address = [f"local addr:4 = {ibank} + off;"]

    ctors = []
    for direction, transfer in (
        (0, [f"{ureg} = *[ram]:4 unit;"]),
        (1, [f"*[ram]:4 unit = {ureg};"]),
    ):
        wt = constrained_terms(
            word_terms,
            field_info,
            (("g", 0), ("l", 0), ("d", direction)),
            "type15b_indexed",
        )
        wt = with_bare(
            wt, (ibank_word, ibank), (data_word, sign_fname), (data_word, mag_fname)
        )
        ctors.append(
            Constructor(
                mnem,
                disp_ops,
                wt,
                nwords,
                semantic_lines=offset
                + address
                + dm_byte_addr_to_ram_unit()
                + transfer,
                active_words=active_words,
            )
        )
    return ctors


# Type3a/Type4a share their top bits with a MORE SPECIFIC sibling form each
# (Type3b/Type3d pin part of Type3a's free `compute` field; Type4b/Type4d
# pin part of Type4a's) that leaves g/d/u/l/cond completely free. Pinning
# g (and l, for Type3a) the way type14a_scalar_constructors pins g/d/l would
# make our pattern genuinely CROSS each sibling's (sec 7.8.1: neither
# contains the other -- ours is free on compute, the sibling's is free on
# g/d/u/l/cond), which sleigh rejects outright without a third, more
# specific "intersection" constructor per crossing pair (see
# gen_memory_crossing_resolvers). Splitting further on d and u as separate
# SLEIGH constructors (as type14a_scalar does for d) would multiply that
# crossing count for no benefit, since d/u don't need pattern-level
# specialization -- their bare bit values are perfectly usable at p-code
# RUNTIME (direction_branch, premodify_or_post/postmodify_update). Only
# `cond` keeps its own two-constructor TRUE/non-TRUE split (condtrue), since
# that one has a real runtime cost: the common TRUE case must avoid the
# unimplemented `condition()` CALLOTHER entirely, or EmulatorHelper would
# fault on every Type3a/Type4a instruction instead of just the rare
# conditional ones.
# Type3b and Type4b are NOT 48-bit siblings sharing Type3a/4a's frame the
# way Type3d/Type4d are -- decode_table.json gives them width 32 (2 words):
# a real Type3b/4b instruction is only 4 bytes, immediately followed by an
# unrelated next instruction, not a 6-byte Type3a/4a with extra `compute`
# bits pinned. So popcount is not a meaningful tie-break against them: a
# genuine 4-byte Type3b/4b match must ALWAYS win over any 6-byte Type3a/4a
# reading (confirmed against the firmware -- tools/sharc_disasm.py
# independently gives sw 0x1c1494 length 4 as Type3b -- see task report), or
# our resolver would silently swallow the next instruction's first word as
# a bogus `compute[15:0]`. Type3d/Type4d genuinely are same-length (48-bit)
# siblings, so the oracle's real tie-break (longest leading run, then most
# fixed bits; see module docstring) applies to them as intended.
MEMORY_CROSSING_RESOLVERS = [
    # (base form, base ctor index in its own ctors_by_form list -- 0 is the
    #  non-TRUE/gated variant, 1 is the cond==TRUE variant; see
    #  type3a_indexed_constructors/type4a_indexed_constructors), sibling
    # form, which side's semantics the resolver keeps.
    #
    # Type3b/Type4b (32-bit): sibling always wins, for both our variants.
    # `extra` replicates ALL of that base variant's own extra fixed bits
    # (g/l, plus cond=TRUE for index 1) on top of the sibling's own pattern,
    # since those pins come from our constrained_terms() calls, not from a
    # mask diff (g/l/cond are plain FIELDS in decode_table.json, not part
    # of Type3a/4a's own mask).
    dict(base="Type3a", index=0, sibling="Type3b", prefer="sibling",
         extra=[(32, 32, 0), (30, 30, 0)]),
    dict(base="Type3a", index=1, sibling="Type3b", prefer="sibling",
         extra=[(32, 32, 0), (30, 30, 0), (37, 33, COND_TRUE)]),
    dict(base="Type4a", index=0, sibling="Type4b", prefer="sibling",
         extra=[(40, 40, 0)]),
    dict(base="Type4a", index=1, sibling="Type4b", prefer="sibling",
         extra=[(40, 40, 0), (37, 33, COND_TRUE)]),
    # Type3d/Type4d (48-bit, genuine siblings): the oracle's real tie-break
    # applies. Both sides tie the leading run at the base form's own run
    # length (the sibling's extra fixed bits sit well below it, inside
    # `compute`), so total popcount decides. Computed once from
    # decode_table.json's masks: Type3a-gated=5 pop < Type3d=7 (sibling
    # wins); Type3a-condtrue=10 pop > 7 (base wins). Type4a-gated=5 pop <
    # Type4d=8 (sibling wins); Type4a-condtrue=10 pop > 8 (base wins). A
    # "base"-preferred entry derives its extra bits automatically from the
    # sibling's mask diff (extra_fixed_runs), since those genuinely are the
    # sibling's distinguishing mask bits.
    dict(base="Type3a", index=0, sibling="Type3d", prefer="sibling",
         extra=[(32, 32, 0), (30, 30, 0)]),
    dict(base="Type3a", index=1, sibling="Type3d", prefer="base"),
    dict(base="Type4a", index=0, sibling="Type4d", prefer="sibling",
         extra=[(40, 40, 0)]),
    dict(base="Type4a", index=1, sibling="Type4d", prefer="base"),
]


def type4a_indexed_constructors(
    mnem, disp_ops, word_terms, nwords, active_words, field_info
):
    """Type4a indexed DM load/store: offset = sext6(data)*4 from an I
    register. u=0: addr = I + offset, no update. u=1: addr = I, then
    (circular-guarded) I += offset -- both via runtime branching on the bare
    `u`/`d` fields, not separate constructors (see MEMORY_CROSSING_RESOLVERS
    above). g=0 only. Two constructors: cond==TRUE (no runtime gate) and
    everything else (whole_instruction_cond_gate)."""
    ibank = bank_field(field_info, "i[2:0]", "ibank")
    ibank_word = field_word(field_info, "i[2:0]")
    lbank = bank_field(field_info, "i[2:0]", "lbank")
    i_raw = field_fname(field_info, "i[2:0]")
    u_fname = field_fname(field_info, "u")
    d_fname = field_fname(field_info, "d")
    sign_fname = field_fname(field_info, "data[5:5]")
    mag_fname = field_fname(field_info, "data[4:0]")
    dreg = field_fname(field_info, "dreg[3:0]")
    cond_fname = field_fname(field_info, "cond[4:0]")
    comp_hi = field_fname(field_info, "compute[22:16]")
    comp_lo = field_fname(field_info, "compute[15:0]")

    body = (
        sign_magnitude_offset("off", sign_fname, mag_fname, 5, 4)
        + premodify_or_post(ibank, "off", u_fname)
        + dm_byte_addr_to_ram_unit()
        + direction_branch(
            d_fname, f"{dreg} = *[ram]:4 unit;", f"*[ram]:4 unit = {dreg};"
        )
        + compute_marker_lines(comp_hi, comp_lo)
        + postmodify_update(ibank, lbank, i_raw, "off")
    )

    ctors = []
    for cond_true in (False, True):
        wt = constrained_terms(word_terms, field_info, (("g", 0),), "type4a_indexed")
        wt = with_bare(wt, (ibank_word, ibank), (ibank_word, lbank))
        if cond_true:
            wt = constrained_terms(
                wt, field_info, (("cond[4:0]", COND_TRUE),), "condtrue"
            )
            semantic = list(body)
        else:
            semantic = whole_instruction_cond_gate(cond_fname) + body
        ctors.append(
            Constructor(
                mnem, disp_ops, wt, nwords, semantic_lines=semantic,
                active_words=active_words,
            )
        )
    return ctors


def type3a_indexed_constructors(
    mnem, disp_ops, word_terms, nwords, active_words, field_info
):
    """Type3a indexed DM load/store: offset = M[m]*4 (a full 32-bit register
    value, already effectively signed -- no sign-extension needed). u=0:
    addr = I + offset, no update. u=1: addr = I, then (circular-guarded)
    I += offset -- both via runtime branching on the bare `u`/`d` fields,
    not separate constructors (see MEMORY_CROSSING_RESOLVERS above). g=0,
    l=0 only. Two constructors: cond==TRUE (no runtime gate) and everything
    else (whole_instruction_cond_gate)."""
    ibank = bank_field(field_info, "i", "ibank")
    ibank_word = field_word(field_info, "i")
    lbank = bank_field(field_info, "i", "lbank")
    i_raw = field_fname(field_info, "i")
    mbank = bank_field(field_info, "m", "mbank")
    mbank_word = field_word(field_info, "m")
    u_fname = field_fname(field_info, "u")
    d_fname = field_fname(field_info, "d")
    ureg = field_fname(field_info, "ureg")
    cond_fname = field_fname(field_info, "cond")
    (_w0, comp_hi, _c0, _n0), (_w1, comp_lo, _c1, _n1) = field_info["compute"][2]

    body = (
        [f"local sm:4 = {mbank} * 4;"]
        + premodify_or_post(ibank, "sm", u_fname)
        + dm_byte_addr_to_ram_unit()
        + direction_branch(
            d_fname, f"{ureg} = *[ram]:4 unit;", f"*[ram]:4 unit = {ureg};"
        )
        + compute_marker_lines(comp_hi, comp_lo)
        + postmodify_update(ibank, lbank, i_raw, "sm")
    )

    ctors = []
    for cond_true in (False, True):
        wt = constrained_terms(
            word_terms, field_info, (("g", 0), ("l", 0)), "type3a_indexed"
        )
        wt = with_bare(wt, (ibank_word, ibank), (mbank_word, mbank), (ibank_word, lbank))
        if cond_true:
            wt = constrained_terms(
                wt, field_info, (("cond", COND_TRUE),), "condtrue"
            )
            semantic = list(body)
        else:
            semantic = whole_instruction_cond_gate(cond_fname) + body
        ctors.append(
            Constructor(
                mnem, disp_ops, wt, nwords, semantic_lines=semantic,
                active_words=active_words,
            )
        )
    return ctors


def extra_fixed_runs(mask, value, exclude_mask):
    """-> [(frame_hi, frame_lo, value), ...] for the contiguous runs of
    MASK's fixed bits that EXCLUDE_MASK does not already fix -- the extra
    constraint a sibling form (e.g. Type3b) adds on top of a base form's own
    mask (e.g. Type3a's). Same shape as CROSSING_RESOLVERS' `extra` list, so
    it can be applied to a Constructor's word_terms the same way
    gen_crossing_resolvers does."""
    extra_mask = mask & ~exclude_mask
    runs = []
    b = 47
    while b >= 0:
        if (extra_mask >> b) & 1:
            hi = b
            while b - 1 >= 0 and (extra_mask >> (b - 1)) & 1:
                b -= 1
            lo = b
            runs.append((hi, lo, (value >> lo) & ((1 << (hi - lo + 1)) - 1)))
        b -= 1
    return runs


def gen_memory_crossing_resolvers(ctors_by_form, forms_by_name):
    """Build the intersection constructors MEMORY_CROSSING_RESOLVERS lists:
    one per (Type3a/4a variant, sibling) crossing pair, cloning whichever
    side's pattern+semantics the oracle's tie-break prefers and additionally
    pinning the OTHER side's extra fixed bits -- see sec 7.8.1's documented
    resolution technique (also used by gen_crossing_resolvers above, for an
    unrelated pair of forms)."""
    resolvers = []
    for spec in MEMORY_CROSSING_RESOLVERS:
        base_ctor = ctors_by_form[spec["base"]][spec["index"]]
        # The sibling's own generic constructor is always LAST in its list
        # (gen_constructor appends it after any of the sibling's own
        # specializations, e.g. Type3b's type3b_exact_constructors).
        sibling_ctor = ctors_by_form[spec["sibling"]][-1]
        winner = base_ctor if spec["prefer"] == "base" else sibling_ctor

        # The resolver's pattern must be built from the WINNER's own
        # word_terms: its mnemonic/display_ops/semantics all reference
        # field names that only exist in ITS pattern (e.g. Type3b's `w`/`x`
        # aren't in Type3a's word_terms at all, and vice versa for our
        # ibank/lbank/mbank aliases) -- cloning the other side's word_terms
        # would leave the winner's own operands "undefined" to sleigh.
        if spec["prefer"] == "base":
            # Sibling's own extra fixed bits, genuinely derivable from its
            # mask diff against the base form's mask.
            sibling_mask = int(forms_by_name[spec["sibling"]]["mask"], 16)
            sibling_value = int(forms_by_name[spec["sibling"]]["value"], 16)
            base_mask = int(forms_by_name[spec["base"]]["mask"], 16)
            extra = extra_fixed_runs(sibling_mask, sibling_value, base_mask)
        else:
            extra = spec["extra"]

        wt = {w: list(terms) for w, terms in winner.word_terms.items()}
        for hi, lo, val in extra:
            chunks = split_by_word(hi, lo)
            assert len(chunks) == 1, "extra bits spanning >1 word not supported"
            w, chi, clo = chunks[0]
            fname = FIELDS.get(w, chi, clo, "memresolve")
            wt.setdefault(w, []).append(f"{fname}=0x{val:x}")

        resolvers.append(
            Constructor(
                winner.mnemonic,
                list(winner.display_ops),
                wt,
                winner.nwords,
                semantic_lines=list(winner.semantic_lines),
                active_words=list(winner.active_words),
            )
        )
    return resolvers


def type14a_scalar_constructors(
    mnem, disp_ops, word_terms, nwords, active_words, field_info
):
    """Scalar direct-DM Type14a load/store constructors, separate from Type3b."""
    _base, _shift, high_chunks, _hi, _lo = field_info["addr[31:16]"]
    _base, _shift, low_chunks, _hi, _lo = field_info["addr[15:0]"]
    _base, _shift, ureg_chunks, _hi, _lo = field_info["ureg[6:0]"]
    assert len(high_chunks) == len(low_chunks) == len(ureg_chunks) == 1
    _word, high, _clo, _nbits = high_chunks[0]
    _word, low, _clo, _nbits = low_chunks[0]
    _word, ureg, _clo, _nbits = ureg_chunks[0]
    address = [
        f"local high16:2 = {high};",
        f"local low16:2 = {low};",
        "local addr:4 = (zext(high16) << 16) | zext(low16);",
    ] + dm_byte_addr_to_ram_unit()
    constructors = []
    for direction, transfer in (
        (0, [f"{ureg} = *[ram]:4 unit;"]),
        (1, [f"*[ram]:4 unit = {ureg};"]),
    ):
        constructors.append(
            Constructor(
                mnem,
                disp_ops,
                constrained_terms(
                    word_terms,
                    field_info,
                    (("g", 0), ("d", direction), ("l", 0)),
                    "type14a_scalar",
                ),
                nwords,
                semantic_lines=address + transfer,
                active_words=active_words,
            )
        )
    return constructors


def type3b_exact_constructors(
    mnem, disp_ops, word_terms, nwords, active_words, field_info
):
    """The traced scalar Type3b reader; its specialization is independent of Type14a."""
    terms = constrained_terms(
        word_terms,
        field_info,
        (
            ("u", 0),
            ("i[2:0]", 4),
            ("m[2:0]", 4),
            ("cond[4:0]", COND_TRUE),
            ("g", 0),
            ("d", 0),
            ("l", 0),
            ("ureg[6:0]", 28),
            ("w", 1),
            ("x", 1),
        ),
        "type3b_exact",
    )
    return [
        Constructor(
            mnem,
            disp_ops,
            terms,
            nwords,
            semantic_lines=["local addr:4 = I4 + M4;"]
            + dm_byte_addr_to_ram_unit()
            + ["I12 = *[ram]:4 unit;"],
            active_words=active_words,
        )
    ]


# Shared branch-target subtables, keyed by (mode, bit-shape) -- NOT by mode
# alone, because "pcrel" now covers two unrelated field shapes: Type25a_pcrel/
# Type8a_rel's 24-bit reladdr (words 1-2) and Type9a_rel/Type9b_rel's 6-bit
# reladdr (words 0-1, borrowed from the same frame slot as PMI+PMM -- see
# build_table.py SPLIT_FORMS). Populated by get_target_subtable() the first
# time each (mode, shape) is needed and emitted once in main(). A jump-target
# computed purely from immediate instruction fields must be `export`ed from
# its own subtable (per sleigh_constructors.html sec 7.7.2.5's
# `dest: rel is simm8 [ rel=... ] { export *[ram]:4 rel; }` idiom) so the
# resulting varnode has a known size wherever a *root* instruction
# constructor (Type8a_abs/Type25a_direct/Type25a_pcrel/etc below) then uses
# it directly in `goto target;` / `call target;` -- computing it inline in a
# disassembly action with no subtable/export left SLEIGH unable to size the
# operand ("Could not resolve at least 1 variable size").
SUBTABLES = {}


def get_target_subtable(mode, frag_list, extra_by_word=None):
    """Returns (subtable_name, first_word, span) where `span` is how many
    consecutive tokens (starting at `first_word`) the subtable reference
    itself consumes -- the caller (gen_constructor) needs this to know which
    of ITS OWN words are swallowed by the subtable and must not get a
    second, separate pattern group (see Constructor.active_words).

    `extra_by_word` (word -> [bare fixed-field terms]) lets a caller fold
    EXTRA fixed-bit constraints into one of the subtable's own internal
    (already token-anchored) pattern groups -- needed when a word the
    subtable spans past its first also carries a fixed bit some OTHER form
    sharing that same (mode, shape) does NOT have (e.g. Type9b_rel's
    word1 sentinel 0x3f that marks the ISA's narrower 32-bit Type9b family,
    which Type9a_rel's otherwise-identical word0-word1 reladdr shape
    doesn't carry). Folding it in here (same token, plain '&') sidesteps
    trying to combine differently-token-anchored terms with '...' from the
    OUTSIDE, which SLEIGH rejects ("Mismatched tokens when combining
    patterns"). Forms that need this get their own distinct subtable
    (see the cache key below); forms that don't share the plain one."""
    extra_by_word = extra_by_word or {}
    shape = tuple((f["w"], f["chi"], f["clo"]) for f in frag_list)
    extra_key = tuple(sorted((w, tuple(terms)) for w, terms in extra_by_word.items()))
    key = (mode, shape, extra_key)
    if key in SUBTABLES:
        e = SUBTABLES[key]
        return e["name"], e["word"], e["span"]

    frags = list(frag_list)
    if mode == "pcrel" and frags:
        top = frags[0]
        signed_name = FIELDS.get(
            top["w"], top["chi"], top["clo"], top["label"], signed=True
        )
        frags[0] = dict(top, fname=signed_name)

    by_word = {}
    for frag in frags:
        by_word.setdefault(frag["w"], []).append(frag["fname"])
    for w, terms in extra_by_word.items():
        by_word.setdefault(w, []).extend(terms)
    words_used = sorted(by_word)
    pattern = " ; ".join(" & ".join(by_word[w]) for w in words_used)

    expr = reassemble_expr(frags)
    # PC-relative displacement is relative to THIS instruction's own address
    # (inst_start), NOT the next instruction (inst_next) -- confirmed against
    # firmware; see module docstring for the evidence.
    rhs = f"inst_start + ({expr})" if mode == "pcrel" else expr

    total_bits = sum(f["chi"] - f["clo"] + 1 for f in frag_list)
    span = words_used[-1] - words_used[0] + 1
    base_name = f"target_{mode}_{total_bits}b_w{words_used[0]}"
    siblings_same_shape = sum(1 for k in SUBTABLES if k[0] == mode and k[1] == shape)
    name = (
        base_name
        if not siblings_same_shape
        else f"{base_name}_v{siblings_same_shape + 1}"
    )
    text = (
        f"{name}: target is {pattern} [ target = {rhs}; ] "
        "{\n    export *[ram]:4 target;\n}\n"
    )
    SUBTABLES[key] = dict(name=name, word=words_used[0], span=span, text=text)
    return name, words_used[0], span


def find_target_fragments(field_info, base_label):
    """Collect every field whose split-label base matches base_label (e.g.
    'addr' matches 'addr[23:16]' and 'addr[15:0]', or an unsplit 'addr'),
    sorted most-significant fragment first, and return (frag_list, exclude).
    frag_list entries are (fname, shift). exclude is the set of JSON field
    labels consumed (so the generic operand loop skips them)."""
    # NOTE: for every branch form we actually special-case (Type8a's `addr`,
    # Type25a_direct's `addr`, Type25a_pcrel's `reladdr`), the target field is
    # given in decode_table.json as exactly two whole, non-further-split
    # labels (e.g. "addr[23:16]" + "addr[15:0]"), each of which maps to a
    # single (word,hi,lo) chunk -- see the word-boundary check performed
    # during development (only Type3a's unrelated "compute" field straddles a
    # word boundary within one label). So each label contributes exactly one
    # fragment, positioned by its own label-encoded shift amount.
    frags = []
    exclude = set()
    for label, (base, shift, chunks, hi, lo) in field_info.items():
        if base != base_label:
            continue
        exclude.add(label)
        assert len(chunks) == 1, (
            f"{label}: target field unexpectedly split across words"
        )
        w, fname, clo_inword, nbits = chunks[0]
        chi_inword = clo_inword + nbits - 1
        frags.append(
            dict(
                fname=fname,
                shift=shift,
                hi=hi,
                w=w,
                chi=chi_inword,
                clo=clo_inword,
                label=label,
            )
        )
    # sort fragments most-significant-first using each label's own hi bit
    frags.sort(key=lambda d: -d["hi"])
    return frags, exclude


def gen_constructor(form):
    name = form["name"]
    ident = sanitize_ident(name)
    width = form["width"]
    nwords = width // 16

    branch_cfg = BRANCH_FORMS.get(name)
    # Type9a_abs/Type9b_abs (register-indirect, no static target -- see
    # INDIRECT_BRANCH_FORMS) share the same jump/call split-field mechanism
    # as the real branch forms, just without a target subtable.
    indirect_cfg = None if branch_cfg else INDIRECT_BRANCH_FORMS.get(name)
    split_cfg = branch_cfg or indirect_cfg
    split_field = split_cfg.get("split_field") if split_cfg else None
    target_base_label = branch_cfg["target_label"] if branch_cfg else None

    # The split field (e.g. Type8a_abs/Type9a_rel's `b`) will be pinned to a
    # specific value per variant below, so it must not also appear bare in
    # word_terms. The
    # target field's own fragments (addr/reladdr) are instead consumed by a
    # shared subtable (see get_target_subtable) invoked as a single bare
    # `target` operand, so they must not appear bare here either.
    no_bare = set()
    if split_field:
        for fl in form["fields"]:
            if label_base_and_shift(fl["label"])[0] == split_field:
                no_bare.add(fl["label"])
    if branch_cfg:
        for fl in form["fields"]:
            if label_base_and_shift(fl["label"])[0] == target_base_label:
                no_bare.add(fl["label"])

    word_terms, field_info = build_field_terms(form, width, no_bare_labels=no_bare)

    frag_list, target_exclude = ([], set())
    target_subtable_name = target_word = target_span = None
    swallowed = set()
    if branch_cfg:
        frag_list, target_exclude = find_target_fragments(field_info, target_base_label)
        target_subtable_name, target_word, target_span = get_target_subtable(
            branch_cfg["mode"], frag_list
        )
        # Any word strictly AFTER target_word but still within the
        # subtable's own span is swallowed whole by the bare subtable
        # reference placed at target_word (e.g. Type9a_rel/Type9b_rel's
        # `reladdr` subtable starts at word0 but also consumes word1 --
        # see build_table.py SPLIT_FORMS for why that 6-bit field sits
        # where Type9a's merged figure shows PMI+PMM instead of Type8a's
        # word1-word2 addr/reladdr). A second, separate pattern group for
        # such a word would ask SLEIGH to match those bits twice.
        swallowed = set(range(target_word + 1, target_word + target_span))

    # Any OTHER field (not the target itself, not the split bit) that has
    # so much as one fragment inside a swallowed word can no longer be
    # independently referenced from this constructor's own pattern -- drop
    # it entirely (all its fragments, even ones in non-swallowed words) so
    # disassembly doesn't show a field with only half its bits visible.
    # This only ever affects the rare fields that used to ride along in the
    # same word as PMI/PMM (Type9a_rel's `e`/`compute`, Type9b_rel's `j`/
    # `ci`) -- not the mnemonic or the resolved branch target, which are
    # exactly what Ghidra's call-graph analysis needs from these forms.
    dropped_bases = set()
    if swallowed:
        base_words = {}
        for label, (base, shift, chunks, hi, lo) in field_info.items():
            if label in target_exclude:
                continue
            for w, fname, clo, nbits in chunks:
                base_words.setdefault(base, set()).add(w)
        dropped_bases = {b for b, ws in base_words.items() if ws & swallowed}

    # Build display operand list + collect plain (non-target, non-split) fields
    display_ops = []
    plain_labels = []
    split_field_label = None
    for label in form["fields"]:
        lbl = label["label"]
        if lbl in target_exclude:
            continue
        base, _shift = label_base_and_shift(lbl)
        if split_field and base == split_field:
            split_field_label = lbl
            continue
        if base in dropped_bases:
            continue
        plain_labels.append(lbl)

    for lbl in plain_labels:
        base, shift, chunks, hi, lo = field_info[lbl]
        for w, fname, clo, nbits in chunks:
            display_ops.append(fname)

    if dropped_bases:
        drop_fnames = {
            fname
            for label, (base, shift, chunks, hi, lo) in field_info.items()
            if base in dropped_bases
            for w, fname, clo, nbits in chunks
        }
        for w in word_terms:
            word_terms[w] = [t for t in word_terms[w] if t not in drop_fnames]

    variants = []  # list of (mnemonic, extra_word_terms_override_for_split_field)

    if split_cfg and split_field:
        # e.g. Type8a_abs/Type9a_rel/Type9a_abs: two constructors, split on
        # the 'b' bit's value (jump vs call -- see their PRM JUMPCLAUSE
        # encode tables).
        base, shift, chunks, hi, lo = field_info[split_field_label]
        assert len(chunks) == 1
        w, fname, clo, nbits = chunks[0]
        for val, mnem in split_cfg["split_map"].items():  # pyright: ignore[reportAttributeAccessIssue]
            variants.append((mnem, (w, fname, val)))
    else:
        mnem = (
            branch_cfg["mnemonic"]
            if branch_cfg
            else RETURN_FORMS.get(name, NOP_FORMS.get(name, ident))
        )
        variants.append((mnem, None))

    # Words that get their OWN explicit pattern group: everything except the
    # words a target subtable swallows past its own start word (see above).
    active_words = [w for w in range(nwords) if w not in swallowed]

    cond = cond_chunk(field_info)
    ctors = []
    for mnem, split_override in variants:
        has_flow = (
            bool(branch_cfg)
            or (bool(indirect_cfg) and mnem == "jump")
            or name in RETURN_FORMS
        )
        cond_cases = (
            (True, False) if has_flow and cond and cond[1] in display_ops else (None,)
        )
        for cond_true in cond_cases:
            wt = {w: list(terms) for w, terms in word_terms.items()}
            disp_ops = list(display_ops)
            semantic = []
            if split_override:
                w, fname, val = split_override
                wt[w].append(f"{fname}=0x{val:x}")
            if cond_true:
                w, _fname, clo, nbits = cond  # pyright: ignore[reportGeneralTypeIssues]
                alias = FIELDS.get(w, clo + nbits - 1, clo, "condtrue")
                wt.setdefault(w, []).append(f"{alias}=0x{COND_TRUE:x}")

            # Type17 models only the explicit SISD/PEx UREG write.  The manual's
            # SIMD complementary CUREG write is deliberately deferred.
            if name == "Type17a":
                _base, _shift, high_chunks, _hi, _lo = field_info["data[31:16]"]
                _base, _shift, low_chunks, _hi, _lo = field_info["data[15:0]"]
                assert len(high_chunks) == len(low_chunks) == 1
                _w, high, _clo, _nbits = high_chunks[0]
                _w, low, _clo, _nbits = low_chunks[0]
                _base, _shift, ureg_chunks, _hi, _lo = field_info["ureg[6:0]"]
                assert len(ureg_chunks) == 1
                _w, ureg, _clo, _nbits = ureg_chunks[0]
                semantic.extend(
                    [
                        f"local high16:2 = {high};",
                        f"local low16:2 = {low};",
                        "local imm:4 = (zext(high16) << 16) | zext(low16);",
                        f"{ureg} = imm;",
                    ]
                )
            elif name == "Type17b":
                _base, _shift, data_chunks, _hi, _lo = field_info["data[15:0]"]
                assert len(data_chunks) == 1
                _w, data, _clo, _nbits = data_chunks[0]
                _base, _shift, ureg_chunks, _hi, _lo = field_info["ureg[6:0]"]
                assert len(ureg_chunks) == 1
                _w, ureg, _clo, _nbits = ureg_chunks[0]
                semantic.extend(
                    [
                        f"local imm16:2 = {data};",
                        "local imm:4 = sext(imm16);",
                        f"{ureg} = imm;",
                    ]
                )

            if branch_cfg:
                # A bare subtable reference in the pattern links the LOCAL symbol
                # to the GLOBAL table symbol of the same name (sec 7.4.3), so the
                # display/semantic operand identifier must be the subtable's own
                # name (target_abs / target_pcrel), not an arbitrary alias.
                #
                # The subtable spans target_span tokens (word_target..word_target
                # +target_span-1) while any sibling terms already in this word
                # (e.g. Type8a_abs's j/ci) are only 1 token wide, so '&' can't
                # combine them directly ("Error: Mismatched pattern sizes"). The
                # '...' operator (sec 7.4.4.2) extends the shorter (sibling) side
                # to match before ANDing; any word AFTER target_word that the
                # subtable also swallows gets no separate group of its own (see
                # active_words above), and any word still further out (e.g.
                # Type9a_rel's word2 `compute`) keeps its normal group.
                #
                # A swallowed word can still carry its own FIXED (mask) bits
                # that a sibling form needs to stay distinguishable -- e.g.
                # Type9b_rel's word1 fixes bits[6:0]=0x3f (the same sentinel
                # that marks the ISA's narrower 32-bit Type9b family) even
                # though the subtable placed at word0 already claims word1's
                # reladdr bits. Dropping that fixed constraint entirely would
                # make Type9b_rel's pattern identical to Type9a_rel's (both
                # start 0x09 at word0). SLEIGH won't let the OUTER pattern glue
                # a word1-only term onto the subtable reference via '...' from
                # here ("Mismatched tokens when combining patterns"), so instead
                # fold it into the subtable's OWN word1 group (get_target_
                # subtable's `extra_by_word`) -- a plain, same-token '&', which
                # gives Type9b_rel its own distinct subtable instance while
                # Type9a_rel keeps sharing the plain one.
                extra_by_word = {}
                for w in sorted(swallowed):
                    terms = [t for t in wt.pop(w, []) if t.startswith("fx_")]
                    if terms:
                        extra_by_word[w] = terms
                subtable_name = target_subtable_name
                if extra_by_word:
                    subtable_name, _, _ = get_target_subtable(
                        branch_cfg["mode"], frag_list, extra_by_word
                    )
                siblings = wt.setdefault(target_word, [])  # pyright: ignore[reportArgumentType]
                combined = (
                    f"({' & '.join(siblings)}) ... & {subtable_name}"
                    if siblings
                    else subtable_name
                )
                wt[target_word] = [combined]  # pyright: ignore[reportArgumentType]
                disp_ops = [subtable_name] + disp_ops
                semantic.append(
                    f"call {subtable_name};"
                    if mnem == "call"
                    else f"goto {subtable_name};"
                )
            elif indirect_cfg:
                # Register-indirect jump/call: no statically resolvable target
                # (needs the DAG pointer/modify register file -- out of scope
                # for this pass). A call always falls through after it returns,
                # which is exactly SLEIGH/Ghidra's default behavior for an
                # instruction with NO control-flow p-code at all, so the call
                # variant is correctly left empty. A jump never falls through,
                # so it gets the same "flow leaves via an unresolved target"
                # marker as the register-indirect RETURN_FORMS below, so Ghidra
                # doesn't treat whatever bytes follow as this instruction's
                # fallthrough.
                if mnem == "jump":
                    semantic.append("return [0:4];")
            elif name in RETURN_FORMS:
                semantic.append("return [0:4];")

            if cond_true is False:
                semantic = conditional_semantics(cond[1], semantic)  # pyright: ignore[reportOptionalSubscript]

            # Keep each form's specializations in its own helper: adding one
            # cannot replace the other's constructors or its generic fallback.
            if name == "Type14a":
                ctors.extend(
                    type14a_scalar_constructors(
                        mnem, disp_ops, wt, nwords, active_words, field_info
                    )
                )
            if name == "Type3b":
                ctors.extend(
                    type3b_exact_constructors(
                        mnem, disp_ops, wt, nwords, active_words, field_info
                    )
                )
            if name == "Type15b":
                ctors.extend(
                    type15b_indexed_constructors(
                        mnem, disp_ops, wt, nwords, active_words, field_info
                    )
                )
            if name == "Type4a":
                ctors.extend(
                    type4a_indexed_constructors(
                        mnem, disp_ops, wt, nwords, active_words, field_info
                    )
                )
            if name == "Type3a":
                ctors.extend(
                    type3a_indexed_constructors(
                        mnem, disp_ops, wt, nwords, active_words, field_info
                    )
                )
            ctors.append(
                Constructor(
                    mnem,
                    disp_ops,
                    wt,
                    nwords,
                    semantic_lines=semantic,
                    active_words=active_words,
                )
            )
    return ctors


# ----------------------------------------------------------------------
# Crossing-pattern conflicts
# ----------------------------------------------------------------------
# A systematic pairwise check of every VISA form's (mask,value) against every
# other's (see gen_sleigh.py dev notes / task report) finds the pairs whose bit
# patterns genuinely CROSS: neither form's matching set contains the other's,
# so some frames satisfy both, some satisfy only one, some the other. Per
# sleigh_constructors.html sec 7.8.1, SLEIGH accepts one pattern properly
# containing another (that's how e.g. Type1a/Type1b -- differing only by
# whether compute[22:16] is pinned to the "no-op compute" value -- resolve
# automatically) but rejects a genuine crossing outright ("Constructor
# patterns cannot be distinguished") unless a third, more specific
# constructor is added whose pattern is EXACTLY the intersection of the two
# -- sec 7.8.1's documented resolution technique. Our oracle decoder already
# has a well-defined winner for that intersection (its "longest leading
# prefix, then most fixed bits" rule -- see sharc_decode.py), so each
# resolver constructor below just clones the winner's own constructor and
# additionally pins the loser's extra bits to their required value.
CROSSING_RESOLVERS = [
    # (winner form, loser form, [(frame_hi, frame_lo, value), ...],
    #  base label of a winner field to drop from display/pattern because
    #  the resolver's extra constraint exactly subsumes it -- or None)
    # Type7d used to leave compute[22:16] free and so crossed Type7b. It now
    # pins its own empty compute, which makes the two forms exclusive, and the
    # old extra constraint contradicted those pinned bits.
    # Type21a is now the all-zero word, so it no longer crosses Type22c (bit 32
    # is 0 there and 1 here); the provisional Type21p_undoc16 inherits the
    # crossing, since it fixes bits 47-39 while Type22c fixes 47-40 and bit 32.
    dict(
        winner="Type21p_undoc16", loser="Type22c", extra=[(32, 32, 1)], drop_label=None
    ),
    # Type22a now fixes every bit but `emu`, so Type22c no longer crosses it;
    # the provisional Type22p_undoc48 inherits the crossing as Type21p did.
    dict(
        winner="Type22p_undoc48", loser="Type22c", extra=[(32, 32, 1)], drop_label=None
    ),
]


def gen_crossing_resolvers(ctors_by_form):
    resolvers = []
    for spec in CROSSING_RESOLVERS:
        winner_ctors = ctors_by_form[spec["winner"]]
        assert len(winner_ctors) == 1, spec["winner"]
        base = winner_ctors[0]
        wt = {w: list(terms) for w, terms in base.word_terms.items()}
        disp_ops = list(base.display_ops)

        if spec["drop_label"] is not None:
            # Remove the bare field reference this resolver's extra
            # constraint subsumes (can't be both bare and value-constrained).
            drop_names = {
                n
                for n in FIELDS.used_names
                if n.startswith(spec["drop_label"] + "_")  # pyright: ignore[reportOperatorIssue]
            }
            for w in wt:
                wt[w] = [t for t in wt[w] if t not in drop_names]
            disp_ops = [o for o in disp_ops if o not in drop_names]

        for hi, lo, val in spec["extra"]:  # pyright: ignore[reportOptionalIterable]
            chunks = split_by_word(hi, lo)
            # Both current cases (compute[22:16]; a single status bit) fit in
            # one word; `val` is taken as already being that whole field's
            # value, so a genuine multi-word split would need per-chunk
            # shifting this simple loop does not do.
            assert len(chunks) == 1, "extra bits spanning >1 word not supported"
            w, chi, clo = chunks[0]
            fname = FIELDS.get(w, chi, clo, "resolve")
            const = val & ((1 << (chi - clo + 1)) - 1)
            wt.setdefault(w, []).append(f"{fname}=0x{const:x}")

        resolvers.append(
            Constructor(
                base.mnemonic,
                disp_ops,
                wt,
                base.nwords,
                semantic_lines=list(base.semantic_lines),
            )
        )
    return resolvers


def _identifiers(text):
    """Every bare identifier in a chunk of generated SLEIGH.

    Used to tell which subtables the constructors actually reference. A plain
    substring test will not do: `target_pcrel_6b_w0` is a substring of
    `target_pcrel_6b_w0_v2`, so an orphan would look used.
    """
    out, cur = set(), []
    for ch in text:
        if ch.isalnum() or ch == "_":
            cur.append(ch)
        elif cur:
            out.add("".join(cur))
            cur = []
    if cur:
        out.add("".join(cur))
    return out


def main():
    forms = load_forms()
    visa_forms = [f for f in forms if f["visa"]]
    print(
        f"# {len(forms)} total forms, {len(visa_forms)} VISA forms "
        f"(excluding {len(forms) - len(visa_forms)} ISA-only)",
        file=sys.stderr,
    )

    all_ctors = []
    ctors_by_form = {}
    dreg_fields = []  # (word,hi,lo) ranges to attach to R0-R15
    ureg_fields = []  # (word,hi,lo) ranges to attach to the 7-bit UREG table

    for form in visa_forms:
        ctors = gen_constructor(form)
        all_ctors.extend(ctors)
        ctors_by_form[form["name"]] = ctors

    all_ctors.extend(gen_crossing_resolvers(ctors_by_form))
    forms_by_name = {form["name"]: form for form in visa_forms}
    all_ctors.extend(gen_memory_crossing_resolvers(ctors_by_form, forms_by_name))

    # find dreg/cdreg 4-bit fields for register attachment (after all forms
    # processed, so FIELDS registry is fully populated)
    for form in visa_forms:
        for fl in form["fields"]:
            base, _ = label_base_and_shift(fl["label"])
            if base in ("dreg", "cdreg") and (fl["hi"] - fl["lo"] + 1) == 4:
                for w, chi, clo in split_by_word(fl["hi"], fl["lo"]):
                    key = (w, chi, clo, False, FIELDS.sanitize(base))
                    if key in FIELDS.by_key:
                        dreg_fields.append(FIELDS.by_key[key])

    dreg_fields = sorted(set(dreg_fields))

    # Collect only registered, unsplit 7-bit fields whose label base is
    # exactly `ureg`. The registry key includes that base as its fifth part;
    # srcureg/dstureg/cureg therefore cannot enter this attachment.
    for form in visa_forms:
        for fl in form["fields"]:
            base, _ = label_base_and_shift(fl["label"])
            if base != "ureg" or fl["hi"] - fl["lo"] + 1 != 7:
                continue
            chunks = split_by_word(fl["hi"], fl["lo"])
            if len(chunks) != 1:
                continue
            w, chi, clo = chunks[0]
            key = (w, chi, clo, False, FIELDS.sanitize(base))
            if key in FIELDS.by_key:
                ureg_fields.append(FIELDS.by_key[key])

    ureg_fields = sorted(set(ureg_fields))
    ureg_registers = (
        [f"R{i}" for i in range(16)]
        + [f"I{i}" for i in range(16)]
        + [f"M{i}" for i in range(16)]
        + [f"L{i}" for i in range(16)]
        + [f"B{i}" for i in range(16)]
        + [f"S{i}" for i in range(16)]
        + [
            "FADDR",
            "DADDR",
            "UREG_RESERVED_62",
            "PC",
            "PCSTK",
            "PCSTKP",
            "LADDR",
            "CURLCNTR",
            "LCNTR",
            "EMUCLK",
            "EMUCLK2",
            "PX",
            "PX1",
            "PX2",
            "TPERIOD",
            "TCOUNT",
        ]
        + [
            "USTAT1",
            "USTAT2",
            "MODE1",
            "MMASK",
            "MODE2",
            "FLAGS",
            "ASTATX",
            "ASTATY",
            "STKYX",
            "STKYY",
            "IRPTL",
            "IMASK",
            "IMASKP",
            "MODE1STK",
            "USTAT3",
            "USTAT4",
        ]
    )
    assert len(ureg_registers) == 128

    # ------------------------------------------------------------------
    # Emit sharc_visa.slaspec
    # ------------------------------------------------------------------
    lines = []
    lines.append("# SHARC+ VISA SLEIGH module -- GENERATED by gen_sleigh.py")
    lines.append(
        "# Clean-room: derived from decode_table.json and public PRM UREG/SYSREG tables. Do not hand-edit;"
    )
    lines.append("# edit gen_sleigh.py and regenerate.")
    lines.append("")
    lines.append("define endian=little;")
    # NOTE: alignment is expressed in the same raw-byte units as
    # Address.getOffset() for this word-addressed (wordsize=2) space, NOT in
    # word-address units -- Ghidra's Disassembler rejects any alignment that
    # isn't a multiple of the space's addressable-unit size (== wordsize
    # here), so 2 (one whole word) is the correct "no extra restriction"
    # value, matching how CP1600 (another wordsize=2 Ghidra processor)
    # declares alignment=2. Using 1 here silently made every
    # Disassembler-based disassembly fail with an empty result (see report).
    lines.append(
        "define alignment=2;   # word-addressed space (see gen_sleigh.py docstring)"
    )
    lines.append("")
    lines.append("define space ram type=ram_space size=4 wordsize=2 default;")
    lines.append("define space register type=register_space size=4;")
    lines.append("")
    lines.append(
        "define register offset=0x000 size=4 [ "
        + " ".join(f"R{i}" for i in range(16))
        + " ];"
    )
    lines.append(
        "define register offset=0x040 size=4 [ "
        + " ".join(f"F{i}" for i in range(16))
        + " ];"
    )
    lines.append(
        "define register offset=0x080 size=4 [ "
        + " ".join(f"I{i}" for i in range(16))
        + " ];"
    )
    lines.append(
        "define register offset=0x0c0 size=4 [ "
        + " ".join(f"M{i}" for i in range(16))
        + " ];"
    )
    lines.append(
        "define register offset=0x100 size=4 [ "
        + " ".join(f"L{i}" for i in range(16))
        + " ];"
    )
    lines.append(
        "define register offset=0x140 size=4 [ "
        + " ".join(f"B{i}" for i in range(16))
        + " ];"
    )
    lines.append("define register offset=0x180 size=4 [ PC ];")
    lines.append(
        "define register offset=0x1c0 size=4 [ "
        + " ".join(f"S{i}" for i in range(16))
        + " ];"
    )
    lines.append(
        "define register offset=0x200 size=4 [ "
        + " ".join(ureg_registers[96:99] + ureg_registers[100:112])
        + " ];"
    )
    lines.append(
        "define register offset=0x240 size=4 [ "
        + " ".join(ureg_registers[112:])
        + " ];"
    )
    lines.append("")
    lines.append(
        "define pcodeop condition;   # cond code (PGR Table 10-4) holds; flags not modelled yet"
    )
    lines.append(
        "define pcodeop compute;   # 23-bit parallel compute field; not decoded"
    )
    lines.append(
        "define pcodeop circular;   # post-modify index update when L[i] != 0; wrap not modelled"
    )
    lines.append("")

    for w in range(3):
        lines.append(FIELDS.emit_token(w))

    if dreg_fields:
        lines.append(
            "attach variables [ "
            + " ".join(dreg_fields)
            + " ] [ "
            + " ".join(f"R{i}" for i in range(16))
            + " ];"
        )
        lines.append("")

    if ureg_fields:
        lines.append(
            "attach variables [ "
            + " ".join(ureg_fields)
            + " ] [ "
            + " ".join(ureg_registers)
            + " ];"
        )
        lines.append("")

    # DAG1 bank (g==0) index-register aliases for the indexed DM forms
    # (Type15b/4a/3a): the SAME 3-bit `i`/`m` code is read once bare (the
    # raw index, e.g. for the `circular` marker) and again through one or
    # two of these private, register-attached aliases (I[i] as the address
    # base, L[i] to guard post-modify circular buffering, M[m] as a modify
    # register) -- see bank_field()/BANK_ALIASES below. Each field is only
    # 3 bits (8 codes), matching I0-I7/L0-L7/M0-M7 exactly.
    for tag, regs in BANK_REGISTER_LISTS.items():
        if tag in BANK_ALIASES:
            lines.append(
                "attach variables [ "
                + BANK_ALIASES[tag]
                + " ] [ "
                + " ".join(regs)
                + " ];"
            )
            lines.append("")

    # get_target_subtable() registers the plain, no-extra-bits variant for a
    # branch form before it is known whether that form's constructors will end
    # up on an `extra_by_word` variant instead. When every sharer of a
    # (mode, shape) takes a variant, the plain one is left with no user and
    # sleighc warns "Unreferenced table". Emit only what is referenced.
    ctor_text = [ctor.emit() for ctor in all_ctors]
    used = set()
    for text in ctor_text:
        used |= _identifiers(text)
    live = [e for e in SUBTABLES.values() if e["name"] in used]
    orphans = sorted(e["name"] for e in SUBTABLES.values() if e["name"] not in used)
    if orphans:
        print(
            f"# {len(orphans)} unreferenced subtable(s) not emitted: "
            + ", ".join(orphans),
            file=sys.stderr,
        )

    if live:
        lines.append(
            "# ---------------------------------------------------------------------"
        )
        lines.append(
            "# Shared branch-target subtables (export a sized address varnode so the"
        )
        lines.append(
            "# control-flow forms below can `goto target;` / `call target;` directly)."
        )
        lines.append(
            "# ---------------------------------------------------------------------"
        )
        for entry in live:
            lines.append(entry["text"])

    lines.append(
        "# ---------------------------------------------------------------------"
    )
    lines.append(
        "# Instruction constructors (one per decode_table.json VISA form; the two"
    )
    lines.append(
        "# forms flagged as control-flow-with-static-target get real goto/call"
    )
    lines.append(
        "# p-code, the register-indirect return forms get a generic `return`, and"
    )
    lines.append("# everything else is disassembly-only (empty {} body) for this pass.")
    lines.append(
        "# ---------------------------------------------------------------------"
    )
    lines.extend(ctor_text)

    os.makedirs(OUT_DIR, exist_ok=True)
    slaspec_path = os.path.join(OUT_DIR, "sharc_visa.slaspec")
    with open(slaspec_path, "w") as f:
        f.write("\n".join(lines) + "\n")
    print(
        f"wrote {slaspec_path} ({len(all_ctors)} constructors, "
        f"{sum(len(v) for v in FIELDS.order.values())} fields)",
        file=sys.stderr,
    )

    # ------------------------------------------------------------------
    # Emit .ldefs
    # ------------------------------------------------------------------
    ldefs = """<?xml version="1.0" encoding="UTF-8"?>

<language_definitions>
   <language processor="SHARC_VISA"
            endian="little"
            size="32"
            variant="default"
            version="1.0"
            slafile="sharc_visa.sla"
            processorspec="sharc_visa.pspec"
            id="SHARC_VISA:LE:32:default">
    <description>SHARC+ VISA (generated, clean-room, from decode_table.json)</description>
    <compiler name="default" spec="sharc_visa.cspec" id="default"/>
  </language>
</language_definitions>
"""
    with open(os.path.join(OUT_DIR, "sharc_visa.ldefs"), "w") as f:
        f.write(ldefs)

    # ------------------------------------------------------------------
    # Emit .pspec
    # ------------------------------------------------------------------
    pspec = """<?xml version="1.0" encoding="UTF-8"?>

<processor_spec>
  <programcounter register="PC"/>
</processor_spec>
"""
    with open(os.path.join(OUT_DIR, "sharc_visa.pspec"), "w") as f:
        f.write(pspec)

    # ------------------------------------------------------------------
    # Emit .cspec (minimal -- decompilation is out of scope for this pass)
    # ------------------------------------------------------------------
    cspec = """<?xml version="1.0" encoding="UTF-8"?>

<compiler_spec>
  <data_organization>
    <pointer_size value="4" />
  </data_organization>
  <global>
    <range space="ram"/>
  </global>
  <stackpointer register="I7" space="ram"/>
  <default_proto>
    <prototype name="asm" extrapop="0" stackshift="0" strategy="register">
      <input>
        <pentry minsize="1" maxsize="4">
          <register name="R0"/>
        </pentry>
      </input>
      <output>
        <pentry minsize="1" maxsize="4">
          <register name="R0"/>
        </pentry>
      </output>
    </prototype>
  </default_proto>
</compiler_spec>
"""
    with open(os.path.join(OUT_DIR, "sharc_visa.cspec"), "w") as f:
        f.write(cspec)

    # ------------------------------------------------------------------
    # Module.manifest + .opinion (top-level, alongside data/)
    # ------------------------------------------------------------------
    module_dir = os.path.dirname(os.path.dirname(OUT_DIR))
    with open(os.path.join(module_dir, "Module.manifest"), "w") as f:
        f.write("")
    with open(os.path.join(OUT_DIR, "sharc_visa.opinion"), "w") as f:
        f.write("<opinions>\n</opinions>\n")

    print("done", file=sys.stderr)


if __name__ == "__main__":
    main()
