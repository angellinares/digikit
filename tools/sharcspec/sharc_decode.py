#!/usr/bin/env python3
"""Table-driven SHARC+ instruction decoder (VISA or ISA) over decode_table.json.

Memory layout (observed in the Elektron DSP image): code is a sequence of 16-bit
little-endian words; a 32- or 48-bit instruction stores its most significant word
first. The decoder MSB-aligns up to three words into a 48-bit frame, picks the
matching form with the longest leading (most-significant-bit-aligned) run of
fixed bits, and reports ties as ambiguous.
"""
import os
import struct
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
TOOLS = os.path.dirname(HERE)
if TOOLS not in sys.path:
    sys.path.insert(0, TOOLS)

from sharc_isa import load_instruction_set  # noqa: E402


class Decoder:
    def __init__(self, table=os.path.join(HERE, "decode_table.json"), mode="visa"):
        self.instruction_set = load_instruction_set(table, mode)
        self.forms = [
            (
                form.frame_mask,
                form.frame_value,
                form.fixed_bits,
                form.leading_fixed_bits,
                form.table_dict(),
            )
            for form in self.instruction_set.forms
        ]

    def decode_frame(self, frame):
        # VISA/ISA length is determined by the instruction's leading bits (PRM IAB
        # section): the matching form with the longest run of fixed bits starting
        # at bit 47 and running down without a gap wins ("longest leading prefix").
        # Ties are broken by total fixed-bit count, then reported as ambiguous.
        selection = self.instruction_set.select_frame(frame)
        top = [form.table_dict() for form in selection.candidates]
        return (selection.form.table_dict() if selection.form is not None else None), top

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
