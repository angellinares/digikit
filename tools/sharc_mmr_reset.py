"""Read concrete reset facts from the public ADSP-2156x HWR register tables.

Input is tools/refstext.py's extraction, never a DSP capture. This supplies
reset values only; it does not emulate register side effects or ROM handoff.
"""

from __future__ import annotations

import re

_ROW = re.compile(r"^(0x[0-9a-fA-F]{8})\s+(\S+)\s+.+?\s+(0x[0-9a-fA-F]{8})\s*$")


def parse_reset_tables(text: str) -> dict[int, tuple[int, str]]:
    registers: dict[int, tuple[int, str]] = {}
    for line in text.splitlines():
        match = _ROW.match(line)
        if match is None:
            continue
        address, name, reset = int(match[1], 16), match[2], int(match[3], 16)
        if not 0x30000000 <= address < 0x32000000:
            continue
        previous = registers.get(address)
        if previous is not None and previous[0] != reset:
            raise ValueError(
                f"conflicting reset values at {address:#x}: {previous[0]:#x} and {reset:#x}"
            )
        registers[address] = reset, name
    if not registers:
        raise ValueError("no ADSP-2156x peripheral reset table rows found")
    return registers
