"""ADSP-2156x 32-bit normal-word aliases from the product datasheet.

Rev. D Tables 2--6 (printed pp. 9--10), cited in
docs/refs/adsp-2156x-data-addressing.md. Callers select the architectural
access space; an access's byte width alone never selects these aliases.
These translations do not initialize memory or supply missing values.
"""

from __future__ import annotations


def normal_word_to_byte(address: int) -> int | None:
    # Every range below lies in [0x00090000, 0x18000000), none in
    # [0x000E8000, 0x04000000): most data addresses fail here at once.
    if address < 0x00090000 or address >= 0x18000000:
        return None
    if 0x000E8000 <= address < 0x04000000:
        return None
    if 0x00090000 <= address < 0x0009C000:
        return 0x28240000 + (address - 0x00090000) * 4
    if 0x000B0000 <= address < 0x000BC000:
        return 0x282C0000 + (address - 0x000B0000) * 4
    if 0x000C0000 <= address < 0x000C8000:
        return 0x28300000 + (address - 0x000C0000) * 4
    if 0x000E0000 <= address < 0x000E8000:
        return 0x28380000 + (address - 0x000E0000) * 4
    if 0x04000000 <= address < 0x08000000:
        return 0x60000000 + (address - 0x04000000) * 4
    if 0x08000000 <= address < 0x08046000:
        return address * 4
    if 0x0A090000 <= address < 0x0A09C000:
        return address * 4
    if 0x0A0B0000 <= address < 0x0A0BC000:
        return address * 4
    if 0x0A0C0000 <= address < 0x0A0C8000:
        return address * 4
    if 0x0A0E0000 <= address < 0x0A0E8000:
        return address * 4
    if 0x10000000 <= address < 0x18000000:
        return 0x80000000 + (address - 0x10000000) * 4
    return None


def byte_to_normal_word(address: int) -> int | None:
    if 0x00240000 <= address < 0x00270000:
        return address // 4
    if 0x002C0000 <= address < 0x002F0000:
        return address // 4
    if 0x00300000 <= address < 0x00320000:
        return address // 4
    if 0x00380000 <= address < 0x003A0000:
        return address // 4
    if 0x20000000 <= address < 0x20118000:
        return address // 4
    if 0x28240000 <= address < 0x28270000:
        return address // 4
    if 0x282C0000 <= address < 0x282F0000:
        return address // 4
    if 0x28300000 <= address < 0x28320000:
        return address // 4
    if 0x28380000 <= address < 0x283A0000:
        return address // 4
    if 0x60000000 <= address < 0x70000000:
        return 0x04000000 + (address - 0x60000000) // 4
    if 0x80000000 <= address < 0xA0000000:
        return 0x10000000 + (address - 0x80000000) // 4
    return None


def normal_word_to_architectural_byte(address: int) -> int | None:
    mapped = normal_word_to_byte(address)
    if mapped is not None and address < 0x00100000:
        return mapped - 0x28000000
    return mapped
