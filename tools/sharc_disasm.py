"""Linear disassembler for SHARC+ VISA code, built on tools/sharc_visa_tables.py.

    uv run python tools/sharc_disasm.py REGION.bin [START_OFFSET]

Walks a byte buffer forward instruction by instruction and stops at the first
word it cannot classify: no form matches, several forms tie, or the buffer
ends inside the instruction. It never advances past such a word, because a
wrong length silently desyncs every instruction after it.

Memory holds 16-bit little-endian words, instructions start at even offsets,
and a 32- or 48-bit instruction stores its most significant word first:
insn = (w0 << 32) | (w1 << 16) | w2 for 48 bits, (w0 << 16) | w1 for 32.
A form whose table entry marks some fixed bits unconfirmed yields kind
"uncertain", and the walk continues.
"""

from __future__ import annotations

import struct
from dataclasses import dataclass, field
from pathlib import Path
from typing import Iterator, Protocol

from sharc_isa import load_instruction_set
from sharc_visa_tables import TYPES, get_type


ISA = load_instruction_set()


class Desync(Exception):
    """disassemble(..., on_unknown="raise") met a word it cannot classify."""

    def __init__(self, offset: int, reason: str, word0: int, hypotheses: list[str]):
        self.offset = offset
        self.reason = reason
        self.word0 = word0
        self.hypotheses = hypotheses
        super().__init__(f"desync at offset {offset:#x}: {reason} (word0={word0:#06x})")


@dataclass
class Instruction:
    """One decoded or undecodable unit from disassemble().

    offset       : byte offset in the buffer
    length_bytes : 2, 4 or 6; None when kind == "unknown"
    type_name    : the TYPES name (e.g. "15b"), or "unknown"
    fields       : {label: value} from the form's fields; empty when unknown
    raw          : the instruction in its own width; for "unknown" the first
                   word, or None when not even one word was left
    kind         : "confident" (all fixed bits confirmed), "uncertain" (the
                   table marks some unconfirmed), or "unknown" (always the
                   last record)
    note         : why the walk stopped, or the source of an uncertain form
    """

    offset: int
    length_bytes: int | None
    type_name: str | None
    fields: dict[str, int] = field(default_factory=dict)
    raw: int | None = None
    kind: str = "confident"
    note: str = ""


def _read_u16(data: bytes, offset: int) -> int | None:
    if offset + 2 > len(data):
        return None
    return struct.unpack_from("<H", data, offset)[0]


def identify(insn: int, bits: int, include_uncertain: bool = False) -> list[str]:
    """Every form of that width whose mask matches insn, most fixed bits first;
    forms with unconfirmed bits only with include_uncertain."""
    names = [t["name"] for t in TYPES
             if t["bits"] == bits and (include_uncertain or not t["uncertain"])
             and insn & t["opcode_mask"] == t["opcode_value"]]
    def fixed_bits(name: str) -> int:
        entry = get_type(name)
        assert entry is not None
        return entry["fixed_bits"]

    return sorted(names, key=lambda n: -fixed_bits(n))


def disassemble(data: bytes, start_offset: int = 0, count: int | None = None,
                on_unknown: str = "yield") -> Iterator[Instruction]:
    """Walk data from start_offset, one Instruction per unit, up to count
    instructions (None: until an unknown word or the end of the buffer).

    on_unknown: "yield" yields a final kind="unknown" record and stops;
    "raise" raises Desync instead.
    """
    offset = start_offset
    yielded = 0
    while count is None or yielded < count:
        if offset == len(data):
            return
        words = []
        for i in range(3):
            word = _read_u16(data, offset + 2 * i)
            if word is None:
                break
            words.append(word)
        result = ISA.decode_words(words)
        decoded = result.instruction
        hypotheses = [form.id for form in result.candidates]
        entry = get_type(decoded.form.id) if decoded is not None else None
        if not words:
            reason = f"only {len(data) - offset} byte(s) remain, not enough for a 16-bit word"
        elif entry is None and hypotheses:
            if result.truncated and len(hypotheses) == 1:
                form = result.candidates[0]
                reason = (f"matches {form.id} ({form.extent_bits} bits) but only "
                          f"{len(data) - offset} bytes remain")
            else:
                reason = f"forms tie: {hypotheses}"
        elif entry is None:
            reason = "no form matches"
        else:
            reason = None
        if reason is not None:
            word0 = words[0] if words else 0
            if on_unknown == "raise":
                raise Desync(offset, reason, word0, hypotheses)
            yield Instruction(offset=offset, length_bytes=None, type_name="unknown", fields={},
                              raw=words[0] if words else None, kind="unknown", note=reason)
            return
        assert entry is not None
        assert decoded is not None
        yield Instruction(offset=offset, length_bytes=entry["bits"] // 8,
                          type_name=entry["name"], fields=decoded.field_dict(),
                          raw=decoded.raw, kind="uncertain" if entry["uncertain"] else "confident",
                          note=f"source: {entry['source']}" if entry["uncertain"] else "")
        offset += entry["bits"] // 8
        yielded += 1


@dataclass
class WalkReport:
    start_offset: int
    end_offset: int
    bytes_total: int
    bytes_decoded: int
    instructions: int
    confident: int
    uncertain: int
    stopped_reason: str
    last_instruction: Instruction | None

    @property
    def fraction_decoded(self) -> float:
        return self.bytes_decoded / self.bytes_total if self.bytes_total else 0.0


class ShortWordReader(Protocol):
    """Minimal loader-memory interface for exact-PC decoding."""

    def read_sw(self, pc_sw: int, size: int) -> bytes | None:
        return None


def decode_loaded_at(reader: ShortWordReader, pc_sw: int) -> Instruction:
    """Decode exactly at a loader-backed short-word PC.

    Decode from the largest contiguous mapped window (6, 4, then 2 bytes),
    rather than sweeping across loader blocks or gaps.
    """
    for size in (6, 4, 2):
        window = reader.read_sw(pc_sw, size)
        if window is not None:
            return next(disassemble(window, count=1))
    return Instruction(
        0, None, "unknown", kind="unknown", note="PC unmapped in loader memory"
    )


def walk_and_report(data: bytes, start_offset: int = 0) -> WalkReport:
    """Run disassemble() until it stops, and report how far it got and why."""
    offset = start_offset
    n_conf = n_unc = 0
    last: Instruction | None = None
    stopped_reason = "reached end of buffer cleanly"
    for rec in disassemble(data, start_offset=start_offset, on_unknown="yield"):
        last = rec
        if rec.kind == "unknown":
            stopped_reason = rec.note
            break
        if rec.kind == "confident":
            n_conf += 1
        else:
            n_unc += 1
        assert rec.length_bytes is not None
        offset = rec.offset + rec.length_bytes
    return WalkReport(
        start_offset=start_offset, end_offset=offset, bytes_total=len(data) - start_offset,
        bytes_decoded=offset - start_offset, instructions=n_conf + n_unc, confident=n_conf,
        uncertain=n_unc, stopped_reason=stopped_reason, last_instruction=last,
    )


if __name__ == "__main__":
    import sys

    if len(sys.argv) < 2:
        raise SystemExit(__doc__)
    path = sys.argv[1]
    start = int(sys.argv[2], 0) if len(sys.argv) > 2 else 0
    try:
        blob = Path(path).read_bytes()
    except OSError as exc:
        raise SystemExit(str(exc)) from exc
    report = walk_and_report(blob, start)
    print(f"Walked {path} from offset {start:#x}:")
    print(f"  buffer size:      {len(blob)} bytes")
    print(f"  decoded:          {report.bytes_decoded} / {report.bytes_total} bytes "
          f"({report.fraction_decoded:.1%})")
    print(f"  instructions:     {report.instructions} "
          f"(confident={report.confident}, uncertain={report.uncertain})")
    print(f"  stopped at:       offset {report.end_offset:#x}")
    print(f"  stopped because:  {report.stopped_reason}")
