#!/usr/bin/env python3
"""Table-driven SHARC+ instruction decoder (VISA or ISA) over decode_table.json.

Memory layout (observed in the Elektron DSP image): code is a sequence of 16-bit
little-endian words; a 32- or 48-bit instruction stores its most significant word
first. The decoder MSB-aligns up to three words into a 48-bit frame, picks the
matching form with the longest leading (most-significant-bit-aligned) run of
fixed bits, and reports ties as ambiguous.
"""
import json
import os
import struct

HERE = os.path.dirname(os.path.abspath(__file__))


class Decoder:
    def __init__(self, table=os.path.join(HERE, "decode_table.json"), mode="visa"):
        forms = json.load(open(table))["forms"]
        self.forms = []
        for f in forms:
            if mode == "visa" and not f["visa"]:
                continue
            if mode == "isa" and f["width"] != 48:
                continue
            mask = int(f["mask"], 16)
            lead = 0
            for b in range(47, -1, -1):
                if not (mask >> b) & 1:
                    break
                lead += 1
            self.forms.append((mask, int(f["value"], 16), f["fixed_bits"], lead, f))

    def decode_frame(self, frame):
        # VISA/ISA length is determined by the instruction's leading bits (PRM IAB
        # section): the matching form with the longest run of fixed bits starting
        # at bit 47 and running down without a gap wins ("longest leading prefix").
        # Ties are broken by total fixed-bit count, then reported as ambiguous.
        hits = [(lead, fixed, f) for mask, value, fixed, lead, f in self.forms if frame & mask == value]
        if not hits:
            return None, []
        best_lead = max(h[0] for h in hits)
        hits = [h for h in hits if h[0] == best_lead]
        best_fixed = max(h[1] for h in hits)
        top = [f for lead, fixed, f in hits if fixed == best_fixed]
        return (top[0] if len(top) == 1 else None), top

    @staticmethod
    def fields(frame, form):
        out = {}
        for fl in form["fields"]:
            out[fl["label"]] = (frame >> fl["lo"]) & ((1 << (fl["hi"] - fl["lo"] + 1)) - 1)
        return out


def words_at(mem, addr):
    """16-bit LE words from a bytes-like region starting at a BW offset."""
    return [struct.unpack_from("<H", mem, addr + 2 * i)[0] for i in range(3) if addr + 2 * i + 2 <= len(mem)]


def linear(mem, start, count, dec):
    """Decode `count` instructions linearly from byte offset `start` of mem."""
    pos, out = start, []
    for _ in range(count):
        w = words_at(mem, pos)
        if not w:
            break
        frame = 0
        for i, v in enumerate(w + [0] * (3 - len(w))):
            frame |= v << (32 - 16 * i)
        form, cands = dec.decode_frame(frame)
        if form is not None and form["width"] // 16 > len(w):
            # The frame was zero-padded past the end of the buffer, so a match
            # that needs those padding words is not a real instruction.
            # sharc_disasm.py makes the same check; without it the last word of
            # an image ending on an all-zero word reads as a phantom 48-bit
            # Type21a, which is the whole of the two decoders' disagreement on
            # Digitone II 1.11.
            form = None
        if form is None:
            out.append((pos, 1, None, cands, frame))
            pos += 2
            continue
        n = form["width"] // 16
        out.append((pos, n, form, cands, frame))
        pos += 2 * n
    return out
