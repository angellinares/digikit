"""Data memory reads and writes, DAG address arithmetic and SIMD companions.

Moved verbatim from tools/sharc_trace.py.
"""

from __future__ import annotations

from sharcimm import name_address
from sharcldr import SW_ALIAS_BASE, LoadedMemory, sw_to_byte

from .encoding import (
    CORE_MMR_RANGE,
    CORE_MMR_RESET_VALUES,
    L1_BLOCK3_NW_BASE,
    L1_BLOCK3_NW_LIMIT,
    L1_BLOCK3_SW_BASE,
    SYSTEM_MMR_RANGE,
    UREG_CODES,
    UREG_NAMES,
)
from .state import (
    State,
    _cureg_code,
    _json_value,
    _render,
    _simd_active,
)
from .values import (
    Const,
    Operand,
    Unknown,
    Value,
    _add,
)


class UnmodeledMMR(Exception):
    """A core or system MMR (State.explicit_memory_model, opt-in) has no
    known reset value (State.mmrs, pre-populated from
    encoding.CORE_MMR_RESET_VALUES) and no harness-set value (a --poke, or
    any other write through _dm_write()) at the point a form reads it.

    tools/sharc_run.py's Runner catches this and turns it into a named
    Halt; a caller that has not opted into ``explicit_memory_model`` never
    triggers it, and sharc_trace.py's symbolic driver does not catch it, so
    only sharc_run.py's concrete runner is expected to see it today.
    """

    def __init__(self, address: int, name: str | None) -> None:
        self.address = address
        self.name = name
        super().__init__("unmodeled MMR %#x (%s)" % (address, name or "unnamed"))


def _in_core_mmr_range(address: int) -> bool:
    lo, hi = CORE_MMR_RANGE
    return lo <= address < hi


def _in_system_mmr_range(address: int) -> bool:
    lo, hi = SYSTEM_MMR_RANGE
    return lo <= address <= hi


def _concrete_address(value: Value | int) -> int | None:
    return (
        value.value
        if isinstance(value, Const)
        else (value if isinstance(value, int) else None)
    )


def _canonical_dm_address(
    state: State, address: int, width: int, *, for_write: bool = False
) -> int | None:
    """Resolve a DSP DM address to the loader's byte-address alias.

    Application code uses unaliased DM pointers such as ``0x26968c`` whereas
    the boot stream is keyed at ``SW_ALIAS_BASE + 0x26968c``.  Keep an already
    mapped direct address (notably external memory and MMRs) unchanged; only
    retry an unmapped low address through the alias.

    SHARC+ byte address space is the chip's one universal address map: "[a]ll
    physical memory can be addressed using the byte addressable memory space"
    and "[d]ata access of all sizes can be done using byte address space"
    (SHARC+ Core Programming Reference, out/refs/sharc-plus-prm, "Byte
    Address Space Overview of Data Accesses", p.7-4, extraction page 221);
    for a plain direct address (no DAG modify/index arithmetic -- Type14a/
    14d's own ``addr`` field, as opposed to ``Ia+mod``) the accessed byte
    range is always ``[address, address+width)`` unscaled, and only the
    opcode's own size suffix -- (bw)/(sw)/unqualified/(lw) -- picks WIDTH:
    "[t]he address space does not select the memory word size for byte
    addresses. Accesses to byte addresses obey the size of the opcode ...
    not the address space" (p.6-3, extraction page 187); Table 6-2 "Legal
    and Illegal Accesses to Byte Space With or Without Address Scaling"
    (p.6-10/6-11, extraction pages 194-195) and Table 6-3 "Operand Addressed
    in Non-Byte Space or Byte Space for Extended Precision Accesses" (p.6-13,
    extraction page 197) scale a MODIFY's own offset/index by the access
    size when the I-register is in byte-addressed space, never the base
    address of a direct/absolute access like this one. So which of the two
    physical locations a DM pointer resolves to -- raw, or the loader's
    ``SW_ALIAS_BASE``-relative mirror -- is a property of the address alone,
    not of which width happens to be asking: WIDTH governs only how many of
    those bytes this call needs, checked separately below. Deciding the
    family from a WIDTH-wide presence probe (as this function used to) lets
    two different-width accesses to the identical address disagree about
    which physical location it names -- reproduced directly (a 1-byte write
    then a 2-byte read at the same fresh low address;
    tests/test_sharc_memory.py's ``test_family_choice_is_width_independent``)
    -- which is exactly backwards for a universal byte-addressed space where
    every access size names the same underlying bytes.
    """
    concrete = state.concrete
    if concrete is None:
        return None
    # Single-byte probe: which family ADDRESS belongs to, not whether this
    # particular WIDTH-wide access is fully backed there yet (that is
    # checked once the family is settled, below).
    if not _byte_present(state, concrete, address) and 0 <= address < SW_ALIAS_BASE:
        alias = SW_ALIAS_BASE + address
        if for_write or _byte_present(state, concrete, alias):
            address = alias
    # Runtime RAM and MMR destinations need not have loader initializer bytes.
    # A concrete write creates those bytes in this path's overlay.
    if for_write:
        return address
    for here in range(address, address + width):
        if not _byte_present(state, concrete, here):
            return None
    return address


def _byte_present(state: State, concrete: LoadedMemory, here: int) -> bool:
    """Byte HERE has a value on this path: written, or loader-backed."""
    return here in state.overlay or concrete.read(here, 1) is not None


def dm_write_range(state: State, address: int, width: int) -> tuple[int, int] | None:
    """The canonical ``[start, end)`` byte range a WIDTH-byte write to
    architectural DM ADDRESS actually lands in -- exactly what
    ``_dm_write()``/``_canonical_dm_address(..., for_write=True)`` resolve
    to, without performing a write.

    A watchpoint or a harness poke that wants to observe a real store to
    ADDRESS, rather than to whatever ``_dm_write`` happens to print as its
    own address, must watch/poke this range, not ``[address,
    address+width)``: an unmapped low DM pointer -- most application data
    below ``SW_ALIAS_BASE`` -- is silently redirected to ``SW_ALIAS_BASE +
    address`` (see ``_canonical_dm_address``'s docstring), so a
    :class:`~tools.sharc_run.Watchpoint` built from the raw address never
    fires and a poke at the raw address, while it does take effect, lands
    at a different overlay key than the one a caller inspecting ``address``
    directly would look at.

    Returns ``None`` only when STATE has no backing image at all
    (``state.concrete is None``); unlike a read, a write does not need its
    destination bytes to already exist, so this never fails on that account.
    """
    start = _canonical_dm_address(state, address, width, for_write=True)
    return None if start is None else (start, start + width)


def _dm_read(
    state: State, address: Value | int, width: int, signed: bool = False
) -> Const | None:
    """Read little-endian loader-backed DM bytes plus this path's overlay."""
    concrete = _concrete_address(address)
    if state.concrete is None or concrete is None or width not in (1, 2, 4, 8):
        return None
    fixed_width_mmr = (
        concrete in CORE_MMR_RESET_VALUES or name_address(concrete) is not None
    )
    if width == 4 and fixed_width_mmr and concrete in state.mmrs:
        mmr_value = state.mmrs[concrete]
        return mmr_value if isinstance(mmr_value, Const) else None
    if width == 4 and fixed_width_mmr and state.data_memory_tainted:
        return None
    if state.explicit_memory_model and width == 4:
        # Broader than fixed_width_mmr: an address inside the core/system
        # MMR envelope (encoding.CORE_MMR_RANGE/SYSTEM_MMR_RANGE) that
        # tools/sharcimm.py does not individually name still goes through
        # this explicit model rather than falling into ordinary DM/RAM
        # handling below.
        in_mmr_range = (
            fixed_width_mmr
            or _in_core_mmr_range(concrete)
            or _in_system_mmr_range(concrete)
        )
        if in_mmr_range:
            if concrete in state.mmrs:
                mmr_value = state.mmrs[concrete]
                return mmr_value if isinstance(mmr_value, Const) else None
            raise UnmodeledMMR(concrete, name_address(concrete))
    if (
        width == 4
        and not state.assume_nw32
        and not fixed_width_mmr
        and not 0x30000000 <= concrete < 0x40000000
    ):
        # Internal normal-word width depends on runtime IMDWx state.  Reading
        # four loader bytes as one word is opt-in until that state is known.
        return None
    canonical = _canonical_dm_address(state, concrete, width)
    if canonical is None:
        if (
            state.explicit_memory_model
            and width in (1, 2, 4)
            and not (
                fixed_width_mmr
                or _in_core_mmr_range(concrete)
                or _in_system_mmr_range(concrete)
            )
        ):
            # Internal RAM (an unaliased low DM pointer, or an address the
            # loader alias already covers but never wrote) a real boot never
            # wrote reads as 0, matching SHARC+ SRAM after reset -- opt-in,
            # since the default run must still fork/stop on it (see
            # State.explicit_memory_model's docstring).
            return Const(0)
        return None
    concrete = canonical
    if state.data_memory_tainted:
        for here in range(concrete, concrete + width):
            if here not in state.overlay:
                return None
    backing = state.concrete
    assert backing is not None
    raw = bytearray()
    for here in range(concrete, concrete + width):
        if here in state.overlay:
            raw.append(state.overlay[here])
        else:
            byte = backing.read(here, 1)
            assert byte is not None
            raw.append(byte[0])
    value = int.from_bytes(raw, "little", signed=signed)
    # A long word needs a register pair, which this tracer intentionally does
    # not model.  Do not truncate it into a false 32-bit value.
    return Const(value) if width <= 4 else None


def _read_px48(state: State, address: Value | int) -> tuple[Const, Const] | None:
    """Read a loader-backed 48-bit normal word into the PX1/PX2 halves.

    A combined-PX DM or PM transfer without ``LW`` is 48 bits.  L1 block 3's
    normal-word alias packs those words in three 16-bit columns, while loader
    records use the short-word/system-byte view.  Each 48-bit word therefore
    consumes six loader bytes.  The three parcels are individually little-
    endian, but retain their architectural high-to-low order.
    """
    concrete = _concrete_address(address)
    if (
        state.concrete is None
        or concrete is None
        or not L1_BLOCK3_NW_BASE <= concrete < L1_BLOCK3_NW_LIMIT
    ):
        return None
    offset = concrete - L1_BLOCK3_NW_BASE
    byte_address = sw_to_byte(L1_BLOCK3_SW_BASE) + 6 * offset
    raw = state.concrete.read(byte_address, 6)
    if raw is None:
        return None
    high = int.from_bytes(raw[0:2], "little")
    middle = int.from_bytes(raw[2:4], "little")
    low = int.from_bytes(raw[4:6], "little")
    px2 = Const((high << 16) | middle)
    px1 = Const(low << 16)
    return px1, px2


def _load_normal_ureg(
    state: State, space: str, address: Value | int, code: int
) -> Const | dict[str, int] | None:
    """Load one normal-word UREG value, including combined-PX DM/PM reads."""
    if code == UREG_CODES["PX"]:
        halves = _read_px48(state, address)
        if halves is not None:
            px1, px2 = halves
            state.uregs[UREG_CODES["PX"]] = Unknown(
                "combined PX represented by PX1/PX2"
            )
            state.uregs[UREG_CODES["PX1"]] = px1
            state.uregs[UREG_CODES["PX2"]] = px2
            return {"PX1": px1.value, "PX2": px2.value}
        state.uregs[UREG_CODES["PX1"]] = Unknown("memory-address " + _render(address))
        state.uregs[UREG_CODES["PX2"]] = Unknown("memory-address " + _render(address))
    elif space == "DM":
        loaded = _dm_read(state, address, 4)
        state.uregs[code] = loaded or Unknown("memory-address " + _render(address))
        return loaded
    state.uregs[code] = Unknown("memory-address " + _render(address))
    return None


def _dm_write(state: State, address: Value | int, width: int, value: Value) -> bool:
    concrete = _concrete_address(address)
    if (
        state.concrete is None
        or concrete is None
        or not isinstance(value, Const)
        or width not in (1, 2, 4)
    ):
        return False
    fixed_width_mmr = (
        concrete in CORE_MMR_RESET_VALUES or name_address(concrete) is not None
    )
    if width == 4 and fixed_width_mmr:
        state.mmrs[concrete] = value
        return True
    if (
        width == 4
        and not state.assume_nw32
        and not fixed_width_mmr
        and not 0x30000000 <= concrete < 0x40000000
    ):
        return False
    concrete = _canonical_dm_address(state, concrete, width, for_write=True)
    if concrete is None:
        return False
    raw = (value.value & 0xFFFFFFFF).to_bytes(4, "little")[:width]
    state.overlay.update(zip(range(concrete, concrete + width), raw, strict=False))
    return True


def _dossier(state: State, target: int, return_sw: int) -> dict:
    registers = {
        UREG_NAMES[k]: _json_value(v)
        for k, v in state.uregs.items()
        if isinstance(v, Const)
    }
    objects = []
    if state.concrete is not None and state.dossier_bytes:
        seen = set()
        for name, value in registers.items():
            if not isinstance(value, int) or value in seen:
                continue
            raw = bytearray()
            for offset in range(state.dossier_bytes):
                b = _dm_read(state, value + offset, 1)
                if b is None:
                    break
                raw.append(b.value)
            if raw:
                seen.add(value)
                objects.append(
                    {
                        "register": name,
                        "address": value,
                        "bytes": list(raw),
                        "words_le": [
                            int.from_bytes(raw[i : i + 4], "little")
                            for i in range(0, len(raw) - 3, 4)
                        ],
                    }
                )
    return {
        "target_sw": target,
        "return_sw": return_sw,
        "registers": registers,
        "objects": objects,
    }


# Widths this tracer resolves a SIMD companion memory transfer for (SHARC+
# PRM p.212, Table 6-10 "DAG Address vs. Access Modes": explicit address
# Ia, implicit address Ia+k, k=1 for normal-word). Byte and short-word
# access in byte space have their own rule (PRM p.7-5, extraction page
# 222: the companion is the adjacent byte or short word), in
# _SIMD_SUBWORD_COMPANION_BYTES; outside byte space (assume_nw32 off) they
# still raise rather than transfer only the explicit half.
_SIMD_COMPANION_WIDTHS = frozenset({"normal-word"})
_SIMD_SUBWORD_COMPANION_BYTES = {
    "byte": 1,
    "byte-sign-extended": 1,
    "short-word": 2,
    "short-word-sign-extended": 2,
}


def _simd_ureg_mem_companion(
    state: State,
    code: int,
    address: Operand,
    access_width: str = "normal-word",
    *,
    store: bool = False,
) -> tuple[int, Operand] | None:
    """The SIMD companion (Cureg code, companion address) for a single
    UREG<->memory transfer, or None when no companion transfer applies
    (SISD mode, or a UREG with no SIMD complement -- SHARC+ PRM p.15-12's
    TCOUNT/USTAT1 worked example: only "Cureg subset registers" gain a
    companion in SIMD mode, every other UREG "operates the same in SIMD
    and SISD mode").

    An unresolved MODE1.PEYEN is treated like SISD (no companion): the
    explicit transfer this helper's caller performs regardless is correct
    either way (SIMD only ever *adds* an implicit transfer beside it, PRM
    p.28), so an unknown MODE1 cannot make the explicit half wrong -- it
    can only leave a companion transfer unmodelled, exactly as this
    tracer already left it before this helper existed.  ACCESS_WIDTH not
    in _SIMD_COMPANION_WIDTHS raises ValueError instead, since a
    concretely SIMD-active companion this tracer cannot model would
    otherwise be silently dropped; callers should stop the state on that.
    """
    cureg = _cureg_code(code)
    if cureg is None:
        # A STORE of a UREG without a SIMD complement (a DAG register such
        # as M13) still writes both locations in SIMD mode, with the same
        # UREG as the source of each: PRM Table 4-22 (p.4-57), "Ureg to
        # Ureg/CUreg (from uncomplementary register to complementary
        # pair): Executes move in each PE (and/or memory) ... Ureg is
        # source for each move" (the LW case, p.2-10, likewise replicates
        # the value). The firmware relies on it: FUN_1c642a's end-of-frame
        # loop (sw 0x1c7191-0x1c71a6) clears 32 per-voice bytes in 16
        # passes of `DM(I4, M3) = M13` with I4 += 2, and FUN_1c2b24's entry
        # zeroes 32 words in 16 passes of `DM(I5, M4) = M11` with M4 = 2.
        # A load into such a UREG has no implicit move (Table 4-22, "to
        # uncomplementary register ... no implicit move occurs").
        if not store or code == UREG_CODES["PX"]:
            return None
        cureg = code
    if access_width == "long-word":
        # PRM p.13-17: the (LW) modifier "override[s] SIMD mode, so these
        # loads always operate in SISD mode" -- unlike byte/short-word,
        # this is a documented no-companion case, not an unmodelled one.
        return None
    if _simd_active(state) is not True:
        return None
    if access_width in _SIMD_SUBWORD_COMPANION_BYTES and state.assume_nw32:
        # Byte space: PRM p.7-5, "Byte Access in SIMD Mode" / "Short-
        # Word Access in SIMD Mode": the SIMD pair "is updated with the
        # content of the explicit address + 1-byte" (+ 2-byte for a short
        # word) "memory location".
        k = _SIMD_SUBWORD_COMPANION_BYTES[access_width]
        return cureg, _add(address, Const(k), "%s + %d" % (_render(address), k))
    if access_width not in _SIMD_COMPANION_WIDTHS:
        raise ValueError(
            "unsupported SIMD companion access width %r for %s"
            % (access_width, UREG_NAMES[code])
        )
    k = _access_modifier_scale("normal-word", state.assume_nw32)
    companion_address = _add(address, Const(k), "%s + %d" % (_render(address), k))
    return cureg, companion_address


def _access_modifier_scale(access_width: str, assume_nw32: bool) -> int:
    """Return SHARC+ byte-space scaled-address arithmetic width.

    A load/store modifier is scaled by the size of the access in byte
    space and not at all in word space, "except in the case of (lw)" (PRM
    p.6-9, "Enhanced Modify Instruction for Address Scaling"): Table 6-2
    (pp.6-10/6-11) gives "Rm = dm(mod, In) (lw)" and "Rm = dm(In, mod)
    (lw)" the same rows as the unqualified access, scaled_mod = mod << 2
    in byte space. So a long word steps in normal-word units, like
    Type15b's (lw) displacement already does here."""
    if access_width.startswith("short-word"):
        return 2
    if access_width in ("normal-word", "long-word") and assume_nw32:
        return 4
    return 1


def _modify_scale(option: str | None, assume_nw32: bool) -> int:
    """Scale of an M register in MODIFY (Type7a/7b). OPTION is None for the
    plain form, "short-word" for (sw), "normal-word" for (nw).

    PRM p.6-10: "Ia = MODIFY(Ib,Mc); /* Add Mc bytes, Ia=Ib+Mc */ Does not
    scale the modifier, whatever the address space"; only the (sw)/(nw)
    forms scale, and only in byte space (Table 6-2, pp.6-10/6-11). The
    DT2 1.16 frame path depends on it: sw 0x1c661c "I4 = modify(I4, M0)"
    adds a byte offset (0x0, 0x14, 0x28, ...) to a table address in M0
    (0x245c4c)."""
    if option is None:
        return 1
    return _access_modifier_scale(option, assume_nw32)


def _circular_wrap_const(index: int, base: int, length: int, delta: int) -> int | None:
    """Concrete DAG circular-buffer wrap: the true I in [B, B+L) advances by
    DELTA and is corrected by one +-length step when it leaves the buffer
    (SHARC+ Core Programming Reference, out/refs/sharc-plus-prm, Sec. 6
    "Circular Buffering": "If the index pointer falls outside the buffer,
    the DAG subtracts or adds the buffer length to the index value,
    wrapping the index pointer back within the start and end boundaries of
    the buffer"). Valid for a modifier magnitude up to and including LENGTH
    (an exact-L step lands back on INDEX, which is correct: a full lap of a
    circular buffer is the identity). Returns None when |delta| exceeds
    LENGTH, since a single +-length correction is no longer guaranteed to
    land back in range and this module does not model multi-lap modifies.
    """
    if length <= 0 or abs(delta) > length:
        return None
    candidate = (index + delta) & 0xFFFFFFFF
    lower, upper = base, base + length
    if candidate < lower:
        candidate += length
    elif candidate >= upper:
        candidate -= length
    return candidate
