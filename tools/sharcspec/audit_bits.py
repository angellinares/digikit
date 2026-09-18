#!/usr/bin/env python3
"""List, per decode form, the bits the PRM figure prints a value for that
decode_table.json does not fix.

build_table.py's merge rule drops a PRM value wherever the classic PGR grid
leaves that bit blank ('.'), on the theory that the PRM's own digit there is
a stale template value. That call is right for the compute forms -- restoring
the dropped bits there destroys real matches (see below) -- and wrong for the
forms whose figure prints the whole word: Type21a, Type21c, Type22a, Type26a
(docs/sharc/SPEC-FINDINGS.md 3.7 and 3.8).

This script copies a few tables (PAIRS, SPLIT_FORMS, FULL_WORD, rebase())
from build_table.py rather than importing it: build_table.py is a top-level
script with no `if __name__` guard, and importing it re-runs the merge and
overwrites decode_table.json as a side effect.

Usage:
    python3 audit_bits.py [--json OUT.json] [--top N]
"""
import argparse
import json
import os

HERE = os.path.dirname(os.path.abspath(__file__))

# ---------------------------------------------------------------------------
# Copied verbatim from build_table.py (see module docstring for why this is
# a copy, not an import).
# ---------------------------------------------------------------------------

PAIRS = {
    "Type1a": ["Type 1a"], "Type1b": ["Type 1b"], "Type2a": ["Type 2a"], "Type2b": ["Type 2b"], "Type2c": ["Type 2c"],
    "Type3a": ["Type 3a"], "Type3b": ["Type 3b"], "Type3c": ["Type 3c"], "Type4a": ["Type 4a"], "Type4b": ["Type 4b"],
    "Type5a_move": ["Type 5a"], "Type5a (swap)": ["Type 5a #2"], "Type5b (move)": ["Type 5b"], "Type5b (swap)": ["Type 5b #2"],
    "Type6a (mem)": ["Type 6a"], "Type7a": ["Type 7a"], "Type7b": ["Type 7b"],
    "Type11a": ["Type 11a", "Type 11a #2"], "Type11c": ["Type 11c", "Type 11c #2"],
    "Type12a_imm": ["Type 12a"], "Type13a": ["Type 13a"], "Type14a": ["Type 14a"], "Type15a": ["Type 15a"], "Type15b": ["Type 15b"],
    "Type16a": ["Type 16a"], "Type16b": ["Type 16b"], "Type17a": ["Type 17a"], "Type17b": ["Type 17b"], "Type18a": ["Type 18a"],
    "Type19a": ["Type 19a"], "Type19a_bitrev": ["Type 19a #2"], "Type20a": ["Type 20a"], "Type21a": ["Type 21a"], "Type21c": ["Type 21c"],
    "Type22c": ["Type 22c"], "Type25a_direct": ["Type 25a"], "Type25a_pcrel": ["Type 25a #2"], "Type25c_rframe": ["Type 25c"],
}
PRM_VALUE_WINS = {"Type2b"}
FULL_WORD = {"Type21a": (48, 0x000000000000), "Type21c": (16, 0x000100000000)}
SPLIT_FORMS = {
    "Type8a":  dict(abs_key="Type 8a", rel_key="Type 8a #2"),
    "Type9a":  dict(abs_key="Type 9a", rel_key="Type 9a #2"),
    "Type9b":  dict(abs_key="Type 9b", rel_key="Type 9b #2"),
    "Type10a": dict(abs_key="Type 10a", rel_key="Type 10a #2"),
}
# Errata forms whose decode_table.json entry is built from the CLASSIC
# pattern directly (build_table.py resets `keys = []` and rebinds `pb` to
# classic's own pattern), so the merged PRM figure is never consulted for
# fixed-bit values at all. The real PRM figure (a documented copy/erratum)
# is still shown below for completeness, flagged as informational.
ERRATA_FROM_CLASSIC = {"Type3a": "Type 3a", "Type25c_rframe": "Type 25c"}
# Forms with no PAIRS/errata/split entry at all, i.e. the "prm"-only source:
# same code shape as PRM_VALUE_WINS (only pb.get(b) in ('0','1') is used --
# a "-" shaded cell is never consulted even though the figure prints a value
# there).
PRM_ONLY_SOURCE = "prm"


def rebase(pattern, msb):
    """Copied verbatim from build_table.py: pattern index -> frame bit."""
    off = 47 - msb
    return {msb - i + off: ch for i, ch in enumerate(pattern)}


# ---------------------------------------------------------------------------
# Figure helpers (new code).
# ---------------------------------------------------------------------------

def prm_name(f):
    import re
    return re.sub(r"^Figure \d+-\d+:\s*", "", f["caption"]).replace(" Instruction", "").replace(" Opcode", "").replace(" Syntax", "").strip()


def figure_printed_bits(fig):
    """Frame-bit -> printed digit (0/1) for every position the PRM figure
    prints a concrete value: plain fixed cells ('0'/'1') AND shaded/"unused"
    cells ('-', whose actual digit is tallied left-to-right in
    unused_printed). Field ('x') and white/unassigned ('?') positions are
    excluded -- see figure_field_and_white_bits()."""
    pattern, msb = fig["pattern"], fig["msb"]
    shift = 47 - msb
    printed = {}
    for i, ch in enumerate(pattern):
        if ch in ("0", "1"):
            printed[msb - i + shift] = int(ch)
    dash_bits = [msb - i + shift for i, ch in enumerate(pattern) if ch == "-"]
    up = fig.get("unused_printed", "")
    assert len(dash_bits) == len(up), (fig["caption"], len(dash_bits), len(up))
    for b, ch in zip(dash_bits, up):
        printed[b] = int(ch)
    return printed


def figure_field_and_white_bits(fig):
    """(field_bits, white_bits): frame-bit sets for 'x' field positions and
    '?' unassigned/white positions."""
    shift = 47 - fig["msb"]
    field_bits = set()
    for fl in fig["fields"]:
        hi = fl["bits"][0] + shift
        lo = (fl["bits"][-1] if len(fl["bits"]) > 1 else fl["bits"][0]) + shift
        field_bits.update(range(lo, hi + 1))
    white_bits = {b + shift for b in fig.get("unspecified_bits", [])}
    return field_bits, white_bits


def classic_variant_bits(classic, keys):
    """Frame-bit -> set of chars seen across the named classic tables (one
    dict per bit; '.' means that table leaves it blank)."""
    variants = [rebase(classic[k]["pattern"], classic[k]["msb"]) for k in keys]
    bits = set()
    for v in variants:
        bits.update(v)
    out = {}
    for b in bits:
        out[b] = {v.get(b) for v in variants}
    return out


def leading_run(mask):
    lead = 0
    for b in range(47, -1, -1):
        if not (mask >> b) & 1:
            break
        lead += 1
    return lead


def figure_lookup(figures):
    return {prm_name(f): f for f in figures if prm_name(f).startswith("Type")}


def audit_form(dform, figs_by_name, classic):
    name, width = dform["name"], dform["width"]
    mask = int(dform["mask"], 16)
    lo_bound = 48 - width

    base_name = name
    is_split = False
    if name.endswith("_abs") or name.endswith("_rel"):
        cand = name[: -len("_abs")] if name.endswith("_abs") else name[: -len("_rel")]
        if cand in SPLIT_FORMS:
            base_name, is_split = cand, True

    result = {
        "name": name, "width": width, "mask": dform["mask"], "value": dform["value"],
        "fixed_bits": dform["fixed_bits"], "source": dform["source"],
        "unconfirmed_bits": dform["unconfirmed_bits"],
        "figure": None, "applicable": False, "every_bit_printed": None,
        "has_field_x": None, "has_white_q": None,
        "dropped_bits": [], "dropped_count": 0,
        "lead_now": leading_run(mask), "lead_restored": leading_run(mask),
        "note": "",
    }

    if name in ("Type23p_undoc16", "Type21p_undoc16"):
        result["note"] = "no PRM figure at all (undocumented firmware-only form); N/A"
        return result

    fig = figs_by_name.get(base_name)
    if fig is None:
        result["note"] = "no matching PRM figure found by name; check figure_lookup mapping"
        return result

    result["figure"] = fig["caption"]
    result["applicable"] = True

    printed = figure_printed_bits(fig)
    field_bits, white_bits = figure_field_and_white_bits(fig)
    result["has_field_x"] = bool(field_bits)
    result["has_white_q"] = bool(white_bits)
    result["every_bit_printed"] = not field_bits and not white_bits

    # classic per-bit variant sets, for tagging *why* a bit isn't fixed
    # (blank vs a genuine disagreement between paired classic tables --
    # only Type11a/Type11c have >1 key; disagreement is not the audited bug).
    keys = None
    if is_split:
        keys = [SPLIT_FORMS[base_name]["abs_key" if name.endswith("_abs") else "rel_key"]]
    elif base_name in ERRATA_FROM_CLASSIC:
        keys = [ERRATA_FROM_CLASSIC[base_name]]
    elif base_name in PAIRS:
        keys = PAIRS[base_name]
    variant_bits = classic_variant_bits(classic, keys) if keys else {}

    dropped = []
    restored_mask = mask
    for b, v in sorted(printed.items(), reverse=True):
        if b < lo_bound or b > 47:
            continue  # outside this decode form's own bit envelope
        if (mask >> b) & 1:
            continue  # already fixed
        vals = variant_bits.get(b)
        if vals is None:
            reason = "no classic key for this form" if not keys else "bit outside classic table's own width"
        elif vals <= {".", None}:
            reason = "classic blank (the audited rule)"
        elif len(vals) == 1 and vals <= {"0", "1"}:
            # Only possible if the generic loop's own field_bits check would
            # have skipped it as a field bit somewhere else, or a source path
            # (PRM_ONLY_SOURCE / PRM_VALUE_WINS / split / errata) never
            # consulted classic for this bit at all.
            reason = f"classic agrees on {vals.pop()} but this source path never used it"
        else:
            reason = f"classic tables disagree ({sorted(vals)}) -- legitimate abstention, not the blank-bit rule"
        dropped.append({"bit": b, "value": v, "reason": reason})
        if "legitimate" not in reason:
            restored_mask |= (1 << b)

    result["dropped_bits"] = dropped
    result["dropped_count"] = len(dropped)
    result["lead_restored"] = leading_run(restored_mask)

    if is_split:
        result["note"] = ("split abs/rel form: fixed bits come ONLY from the one named classic "
                           "table (fixed_bits_for in build_table.py never looks at the PRM pattern "
                           "at all, so any classic-blank bit is dropped unconditionally)")
    elif base_name in ERRATA_FROM_CLASSIC:
        result["note"] = ("errata override: fixed bits come from classic's OWN pattern, not this "
                           "PRM figure at all (build_table.py treats this PRM figure as a stale "
                           "copy/erratum); comparison here is informational only")
    elif base_name in FULL_WORD:
        result["note"] = "FULL_WORD override (bd153a3): every printed bit, including shaded cells, is already fixed"
    elif base_name in PRM_VALUE_WINS:
        result["note"] = "PRM_VALUE_WINS: fixed bits taken from PRM pattern's 0/1 chars only -- shaded ('-') cells are still never consulted"
    elif base_name not in PAIRS:
        result["note"] = "PRM-only form ('prm' source, no classic table exists): 0/1 chars only, shaded cells still dropped"
    else:
        result["note"] = "generic multi/single-key merge: the audited rule (classic blank -> PRM value discarded)"

    return result


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--json", default=None, help="write full JSON report here")
    ap.add_argument("--top", type=int, default=15, help="rows to print in the table")
    args = ap.parse_args()

    decode_table = json.load(open(os.path.join(HERE, "decode_table.json")))
    figures = json.load(open(os.path.join(HERE, "figures.json")))["figures"]
    classic = json.load(open(os.path.join(HERE, "classic.json")))
    figs_by_name = figure_lookup(figures)

    rows = [audit_form(f, figs_by_name, classic) for f in decode_table["forms"]]

    # Rank: applicable forms first, loosest (most dropped bits) first, then
    # by how much that would lengthen the decoder's leading-run tie-break.
    def sort_key(r):
        return (not r["applicable"], -r["dropped_count"], -(r["lead_restored"] - r["lead_now"]))

    ranked = sorted(rows, key=sort_key)

    print(f"{'name':16s} {'w':>3s} {'fixed':>5s} {'dropped':>7s} {'lead now->restored':>18s}  {'x/?':>4s}  source")
    for r in ranked[: args.top]:
        lead = f"{r['lead_now']:>2}->{r['lead_restored']:<2}" if r["applicable"] else "n/a"
        xq = ("x" if r["has_field_x"] else ".") + ("?" if r["has_white_q"] else ".") if r["applicable"] else "  "
        print(f"{r['name']:16s} {r['width']:>3d} {r['fixed_bits']:>5d} {r['dropped_count']:>7d} {lead:>18s}  {xq:>4s}  {r['source']}")

    if args.json:
        with open(args.json, "w") as fh:
            json.dump({"forms": ranked}, fh, indent=1)
        print(f"\nwrote {args.json}")


if __name__ == "__main__":
    main()
