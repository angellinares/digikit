"""Project and card-slot helpers: build, save, load and export a project; read and diff the project slot of a card.

Used by the memaudit, fxroute, upload, debug and safe-late runners. The project round trip across module sets is
tests/regression/runtime/roundtrip.test.ts (finding 39). The project slot of a card is the sectors
`[0x58000 + 0x8000 * slot, + 0x8000)`.
"""

from __future__ import annotations

import hashlib
import os
import shutil
from pathlib import Path
from typing import Any

from tools.emulator import card_overlay
from tools.patchkit import pk_dkrt_boot as B
from tools.patchkit import pk_dkrt_machine as M

SLOT0 = 0x58000
SLOT_SECTORS = 0x8000
SECTOR = 512
# The one word of the slot that the first save of a project sets differently from every later save, with or without any module change
# (finding 39): the 32-bit word at slot offset 0x18, after the project name. It is left out of the comparison with the first save; every
# later save is compared with the second save byte for byte, so the word is held to the same value there.
STAMP = ((0x18, 4),)


class Fail(RuntimeError):
    pass


def slot_sectors(overlay: Path) -> dict[int, bytes]:
    """The project-slot sectors of an overlay file, with the bytes the guest wrote laid over zeros (mask applied)."""
    out: dict[int, bytes] = {}
    for sector, mask, data in card_overlay.records(overlay.read_bytes()):
        if SLOT0 <= sector < SLOT0 + 128 * SLOT_SECTORS:
            buf = bytearray(SECTOR)
            for i in range(SECTOR):
                if mask[i >> 3] & (1 << (i & 7)):
                    buf[i] = data[i]
            out[sector] = bytes(buf)
    return out


def read_sectors(card: Path, sectors: list[int]) -> dict[int, bytes]:
    fd = os.open(card, os.O_RDONLY)
    try:
        return {
            s: os.pread(fd, SECTOR, s * SECTOR).ljust(SECTOR, b"\0") for s in sectors
        }
    finally:
        os.close(fd)


def slot_image(card: Path, touched: set[int]) -> dict[int, bytes]:
    return read_sectors(card, sorted(touched))


def region_of(offset: int, device: str) -> str:
    """Name the part of the project slot a byte offset (from the slot start) falls in (finding 02, storage records)."""
    if device == "dt2":
        pat, kit, pool = 0x200, 0x200 + 128 * 0x15C00, 0xDA5A04
    else:
        pat, kit, pool = 0x200, 0x200 + 128 * 0x15C00, 0xC4B004 - 130 * 0x167
    if offset < pat:
        return "header"
    if offset < kit:
        return "pattern"
    if offset < pool:
        return "kit"
    return "pool"


def diff_slot(
    a: dict[int, bytes],
    b: dict[int, bytes],
    device: str,
    ignore: tuple[tuple[int, int], ...] = (),
) -> dict[str, Any]:
    """Compare two slot images sector by sector: the sectors only one has and the bytes that differ, by region. IGNORE: byte ranges
    (offset, length) of the first slot's start that are left out (the header stamp, see `STAMP`)."""
    only_a, only_b = sorted(set(a) - set(b)), sorted(set(b) - set(a))
    regions: dict[str, int] = {}
    first = None
    skip = {SLOT0 * SECTOR + off + i for off, n in ignore for i in range(n)}
    for s in sorted(set(a) & set(b)):
        if a[s] != b[s]:
            n = 0
            for i, (x, y) in enumerate(zip(a[s], b[s], strict=True)):
                if x != y and s * SECTOR + i not in skip:
                    n += 1
                    if first is None:
                        first = (
                            (s - SLOT0) // SLOT_SECTORS,
                            (s - SLOT0) % SLOT_SECTORS * SECTOR + i,
                        )
            if n:
                off = (s - SLOT0) % SLOT_SECTORS * SECTOR
                r = region_of(off, device)
                regions[r] = regions.get(r, 0) + n
    return {
        "only_first": len(only_a),
        "only_second": len(only_b),
        "differ_bytes": regions,
        "first_difference": first,
        "equal": not only_a and not only_b and not regions,
    }


def pattern0_changes(
    a: dict[int, bytes], b: dict[int, bytes], n: int = 0x15C00
) -> list[tuple[int, int, int]]:
    """(offset in pattern 0, byte of A, byte of B) for the bytes that differ in the first pattern record of slot 0
    (patternStorage at slot + 0x200: 16 tracks of trig, p-lock and sound-lock data, finding 02)."""
    out = []
    for k in range(n // SECTOR + 1):
        sec = SLOT0 + (0x200 + k * SECTOR) // SECTOR
        x, y = a.get(sec, bytes(SECTOR)), b.get(sec, bytes(SECTOR))
        for i in range(SECTOR):
            if x[i] != y[i]:
                out.append((sec * SECTOR - SLOT0 * SECTOR + i - 0x200, x[i], y[i]))
    return [o for o in out if o[0] < n]


def card_after(c: Any, base: Path, overlay: Path) -> Path:
    """BASE with the guest's writes laid over it, as a cached card file named by content."""
    h = hashlib.sha256(base.name.encode() + overlay.read_bytes()).hexdigest()[:20]
    from tools.patchkit import pk_digikit_scen as S

    dst = S.CACHE / "cards" / f"{c.image}-after-{h}.img"
    written = not dst.exists()
    if written:
        dst.parent.mkdir(parents=True, exist_ok=True)
        tmp = c.work / f"after-{h}.img"
        card_overlay.apply(base, overlay, tmp)
        shutil.move(tmp, dst)
    S.card_used(dst, written)
    return dst


def track_type(rig: B.BootRig, track: int) -> int:
    """The machine type byte of a track's sound (Digitakt II: the kit tracks lie 0x450 apart, finding 02)."""
    return (rig.w(M.TRACK_OBJ_WORD + 0x450 * (track - 1)) >> 8) & 0xFF


def select_dt2(c: Any, rig: B.BootRig, track: int, want: int, hint: int) -> None:
    seen: list[tuple[int, int]] = []
    # The first SRC list after a project save may not take (the keys go to the save's leftovers; how many presses it eats depends on the engine), so the
    # hint is tried up to four times (the coupled engine ate two tries, the ColdFire-only one). Every try starts from the top of the list (UP presses), so a try does not depend on the machine the track holds.
    for downs in (hint, hint, hint, hint, hint + 1, hint - 1):
        rig.select(downs, ups=12)
        seen.append((downs, track_type(rig, track)))
        if seen[-1][1] == want:
            c.check(f"track {track} holds type {want}", True, f"{downs} downs")
            return
    raise Fail(
        f"no list position gives type {want} on track {track}: (downs, type) {seen}"
    )


def build_project(c: Any, rig: B.BootRig, t: dict[str, Any]) -> None:
    """The panel script that makes the project: the run-time machine on track 1 (and the second one on track 2), parameter
    values, a p-lock and a sound lock on step 1 of track 1."""
    s = rig.s
    machines = t["machines"]  # [{track, type, hint}]
    for m in machines:
        if m["track"] != 1:
            s.do_tap(["TRK", f"TRIG {m['track']}"])
            s.run_ms(300)
        if c.dt2:
            select_dt2(c, rig, m["track"], m["type"], m["hint"])
        else:
            rig.select(m["hint"])
        s.do_tap("SYN" if not c.dt2 else "SRC")
        s.run_ms(300)
        for enc, by in (("ENC A", 9), ("ENC B", -7)):  # parameter values
            s.do_turn(enc, by)
            s.run_ms(100)
    if any(m["track"] != 1 for m in machines):
        s.do_tap(["TRK", "TRIG 1"])
        s.run_ms(300)
    # a note trig, a p-lock and a preset (sound) lock on it: GRID RECORDING, tap the trig, hold it and turn knobs
    s.do_tap("RECORD")
    s.run_ms(200)
    s.do_tap("TRIG 1")
    s.run_ms(200)
    s.emu.button(M.H.button("TRIG 1"), True)
    s.run_ms(100)
    s.do_turn("ENC C", t.get("lock_turn", 11))
    s.run_ms(100)
    s.do_turn("LEVEL", t.get("sound_lock_turn", 2))
    s.run_ms(300)
    s.emu.button(M.H.button("TRIG 1"), False)
    s.run_ms(300)
    s.do_tap("RECORD")
    s.run_ms(200)


def save_project(rig: B.BootRig) -> None:
    s = rig.s
    s.do_tap(["FUNC", "SETTINGS"])
    s.run_ms(300)
    s.do_tap("YES")  # slot 001
    s.run_ms(500)
    if rig.device == "dn2":
        s.do_tap("YES")  # the name, save
    s.run_ms(3500)


def load_project(rig: B.BootRig) -> None:
    s = rig.s
    s.do_tap("SETTINGS")
    s.run_ms(300)
    for _ in range(3):
        s.do_tap("YES")
        s.run_ms(500)
    s.do_tap("YES")
    s.run_ms(6000)
    s.health("project loaded")


def export(rig: B.BootRig, name: str) -> Path:
    out = rig.work / f"{rig.tag}-{name}.overlay.bin"
    rig.emu.call("card.overlay", path=str(out))
    return out
