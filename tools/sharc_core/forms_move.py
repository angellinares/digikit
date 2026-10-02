"""Data move forms: register/memory transfers and immediate loads and stores.

Each handler executes one decoded instruction and returns the successor
states. FORMS maps form names to handlers; sharc_core.forms merges the
family tables.
"""

from __future__ import annotations

from collections.abc import Mapping

from sharc_disasm import Instruction

from .compute import (
    Result,
    _apply_compute_simd,
    _compute_simd,
)
from .encoding import (
    ACCESS_WIDTHS,
    TYPE4B_ACCESS_WIDTHS,
    UREG_NAMES,
    _field,
    _split_compute_fields,
    _wide,
)
from .memory import (
    _access_modifier_scale,
    _dm_read,
    _dm_write,
    _load_normal_ureg,
    _normal_word_stride,
    _simd_ureg_mem_companion,
)
from .sequencer import (
    _advance,
    _predicate,
    _predicate_pe,
)
from .state import (
    UREG_CODES,
    State,
    _copy,
    _cureg_code,
    _event,
    _json_value,
    _lw_pair_loads_mate,
    _lw_pair_mate,
    _render,
    _simd_active,
    _snapshot_uregs,
    _stop,
    _ureg,
    _write_ureg,
)
from .values import (
    Const,
    Operand,
    Unknown,
    Value,
    _add,
    _multiply,
    _signed,
)

# Bytes per access width (ACCESS_WIDTHS / TYPE4B_ACCESS_WIDTHS values).
_ACCESS_WIDTH_BYTES: dict[str, int] = {
    "normal-word": 4,
    "byte": 1,
    "byte-sign-extended": 1,
    "short-word": 2,
    "short-word-sign-extended": 2,
    "long-word": 8,
}

# Type14d load (l, x) -> (access width, bytes, sign-extended) (PRM p.386).
_TYPE14D_LOAD_WIDTHS: dict[tuple[int, int], tuple[str, int, bool]] = {
    (0, 0): ("byte", 1, False),
    (1, 0): ("short-word", 2, False),
    (0, 1): ("byte-sign-extended", 1, True),
    (1, 1): ("short-word-sign-extended", 2, True),
}


def _lw_store_pair(
    uregs: Mapping[int, Value], code: int
) -> tuple[list[int], list[Value]]:
    """UREG codes (explicit first) and values a (LW) store from CODE writes
    low-then-high into memory. A real pair-mate's own value goes in the
    high half (_lw_pair_mate); an unpaired UREG (_lw_pair_mate returns
    None) replicates CODE's own value into both halves instead (PRM
    p.2-9/2-10, see state._LW_COMPLEMENTARY_CODES)."""
    mate = _lw_pair_mate(code)
    explicit_value = _ureg(uregs, code)
    if mate is None:
        return [code, code], [explicit_value, explicit_value]
    return [code, mate], [explicit_value, _ureg(uregs, mate)]


def _lw_load_codes(code: int) -> list[int]:
    """UREG codes a (LW) load into CODE fills, explicit (low half) first:
    both members of a register-file neighbor pair, or just CODE itself for
    a complementary system-register pair or an unpaired UREG (see
    state._lw_pair_loads_mate)."""
    if _lw_pair_loads_mate(code):
        mate = _lw_pair_mate(code)
        assert mate is not None
        return [code, mate]
    return [code]


def _type_17a(
    state: State, insn: Instruction, f: Mapping[str, int], name: str
) -> list[State]:
    """17a, 17b."""
    value = _wide(f, "data") if name == "17a" else _signed(_field(f, "data[15:0]"), 16)
    code = _field(f, "ureg")
    _write_ureg(state, code, Const(value))
    _event(state, insn, "ureg-write", ureg=UREG_NAMES[code], value=value & 0xFFFFFFFF)
    return _advance(state, insn)


def _type_3a_transfer(
    target: State,
    insn: Instruction,
    f: Mapping[str, int],
    old: Mapping[int, Value],
    long_word: bool,
    ureg: int,
    compute: Result | None,
    compute_y: Result | None,
) -> None:
    """Type 3a's transfer and compute on TARGET (the state itself, or the
    executed fork), reading operands from OLD, the pre-instruction file."""
    bank = 8 if _field(f, "g") else 0
    index, modifier = _field(f, "i") + bank, _field(f, "m") + bank
    post_modify = bool(_field(f, "u"))
    space = "PM" if bank else "DM"
    access_width = "long-word" if long_word else "normal-word"
    iv, mv = _ureg(old, 16 + index), _ureg(old, 32 + modifier)
    scale = _access_modifier_scale(access_width, target.assume_nw32, iv)
    scaled_mv = _multiply(mv, Const(scale), "M%d * %d" % (modifier, scale))
    modified = _add(iv, scaled_mv, "I%d + M%d * %d" % (index, modifier, scale))
    address = iv if post_modify else modified
    rendered = _render(address)
    mode = "post-modify" if post_modify else "pre-modify"
    # SIMD: PRM p.13-15 ("In SIMD mode, the Type 3a and 3b instruction
    # provides the same access ... for the X and Y processing
    # elements"), Table 6-10: PEy moves the Cureg at the next normal
    # word. (LW) overrides SIMD (no companion, handled above). The
    # block handler's ring A pass (sw 0x1c7593-0x1c759f, 16 SIMD passes
    # over 64 words) loads its even words with "R2 = DM(I4, M5)" (3a):
    # without the companion S2 kept a stale float and half of ring A
    # was written with its negation.
    companion = (
        None
        if long_word or space != "DM"
        else _simd_ureg_mem_companion(target, ureg, address, store=bool(_field(f, "d")))
    )
    if long_word:
        pair_addresses: list[Operand] = []
        for offset in range(2):
            pair_addresses.append(
                _add(
                    address,
                    Const(_normal_word_stride(address) * offset),
                    "%s + %d"
                    % (
                        rendered,
                        _normal_word_stride(address) * offset,
                    ),
                )
            )
        if _field(f, "d"):
            codes, values = _lw_store_pair(old, ureg)
            all_written = True
            for item_address, value in zip(pair_addresses, values, strict=True):
                if not _dm_write(target, item_address, 4, value, normal_word=True):
                    all_written = False
            _event(
                target,
                insn,
                "store",
                space=space,
                ureg_pair=[UREG_NAMES[item] for item in codes],
                values=[_json_value(value) for value in values],
                address=address,
                expression=rendered,
                concrete_write=all_written,
                addressing_mode="post-modify" if post_modify else "pre-modify",
                access_width="long-word",
            )
        else:
            codes = _lw_load_codes(ureg)
            loaded_values: list[Const | None] = []
            for item_address in pair_addresses[: len(codes)]:
                loaded_values.append(
                    _dm_read(target, item_address, 4, normal_word=True)
                )
            for item, mem_value in zip(codes, loaded_values, strict=True):
                target.uregs[item] = mem_value or Unknown("memory-address " + rendered)
            _event(
                target,
                insn,
                "load",
                space=space,
                ureg_pair=[UREG_NAMES[item] for item in codes],
                address=address,
                expression=rendered,
                concrete_values=[
                    _json_value(value)
                    if value is not None
                    else {"unknown": "unavailable memory"}
                    for value in loaded_values
                ],
                addressing_mode="post-modify" if post_modify else "pre-modify",
                access_width="long-word",
            )
    elif _field(f, "d"):
        value = _ureg(old, ureg)
        wrote = _dm_write(target, address, 4, value, normal_word=True)
        _event(
            target,
            insn,
            "store",
            space=space,
            ureg=UREG_NAMES[ureg],
            value=value,
            address=address,
            expression=rendered,
            concrete_write=wrote,
            addressing_mode="post-modify" if post_modify else "pre-modify",
            access_width="normal-word",
        )
        if companion is not None:
            _companion_transfer(target, insn, companion, True, "normal-word", old, mode)
    else:
        loaded = _load_normal_ureg(target, space, address, ureg)
        _event(
            target,
            insn,
            "load",
            space=space,
            ureg=UREG_NAMES[ureg],
            address=address,
            expression=rendered,
            concrete_value=loaded,
            addressing_mode="post-modify" if post_modify else "pre-modify",
            access_width="normal-word",
        )
        if companion is not None:
            _companion_transfer(
                target, insn, companion, False, "normal-word", old, mode
            )
    if post_modify:
        target.uregs[16 + index] = modified
    if compute is not None:
        _apply_compute_simd(target, insn, compute, compute_y)


def _type_3a(
    state: State, insn: Instruction, f: Mapping[str, int], name: str
) -> list[State]:
    """3a."""
    # PRM Type 3a is a conditional compute plus one normal-word DM/PM
    # transfer. Table 13-1's syntax row is "IF cond compute, DM(Ia,Mb)
    # = Ureg" -- cond gates the *whole* instruction, not just the
    # compute half (PRM p.7924: a false condition "generate[s] NOPs
    # on the processing element"), so a resolved-false predicate skips
    # both the transfer and the compute, and an unresolved predicate
    # forks into executed/skipped states exactly like every other
    # conditional form here (2a, 5a_move, 9a_abs).
    #
    # l=1 is the (LW) long-word override (PRM p.13-15's ACCESS Encode
    # Table: every u/g/d row repeats with "(lw)" appended, same cond/
    # compute/i/m fields). p.13-13: "(LW) ... lets programs specify
    # long word addressing, overriding default addressing from the
    # memory map" -- the named ureg's 32-bit value and its *neighbor*
    # register (PRM p.2-4 "Data Register Neighbor Pairing": "Every even
    # data register has an associated odd register representing a
    # register pair. For example, R1:0 are a neighbor data register
    # pair") together fill one 64-bit long-word slot, ureg at the low
    # word and ureg+1 at the high word (Figure 7-19 "Long Word
    # Addressing of Single-Data": "RX = DM(LONG WORD X0 ADDRESS)"
    # loads RX from the low 32 bits and its neighbor RY from the high
    # 32 bits of the same 64-bit bus access). p.2-5's NOTE: "The
    # instruction modifier (LW) overrides SIMD Mode" -- matching
    # Type14a's PM/DM long-word branch and memory._simd_ureg_mem_
    # companion's own long-word case, this is SISD-only, no
    # complementary-register companion transfer. An odd-coded ureg pairs
    # with ureg-1 instead of ureg+1 (state._lw_pair_mate, PRM p.6-5's
    # odd-DAG-register case), and a handful of non-register-file ureg
    # codes pair up (or, for a load, do not) differently again --
    # state._lw_pair_mate's docstring has the full rule and citations.
    cond = _field(f, "cond")
    old = _snapshot_uregs(state.uregs)
    compute_fields = _split_compute_fields(f)
    # SIMD: the compute runs on PEy too (PRM p.101, "Dispatches a single
    # instruction to both processing element's computational units"),
    # as Type 2 already does. Only for an unconditional instruction: a
    # conditional one's per-PE condition is not modelled here, so PEy's
    # half is left out rather than guessed.
    try:
        compute, compute_y = _compute_simd(
            state,
            compute_fields,
            False,
            old,
            state.special,
            approx_recips=state.approx_recips,
        )
    except ValueError as error:
        return [_stop(state, insn, str(error))]
    if cond != 0x1F:
        compute_y = None

    long_word = bool(_field(f, "l"))
    ureg = _field(f, "ureg")

    predicate = _predicate(state, cond)
    if predicate is True:
        _type_3a_transfer(state, insn, f, old, long_word, ureg, compute, compute_y)
        state.trace[-1].update(condition=cond, predicate_assumption=True)
        return _advance(state, insn)
    if predicate is False:
        _event(
            state, insn, "type3a-skipped", condition=cond, predicate_assumption=False
        )
        return _advance(state, insn)
    executed, skipped = _copy(state), _copy(state)
    _type_3a_transfer(executed, insn, f, old, long_word, ureg, compute, compute_y)
    executed.trace[-1].update(condition=cond, predicate_assumption=True)
    _event(skipped, insn, "type3a-skipped", condition=cond, predicate_assumption=False)
    return _advance(executed, insn) + _advance(skipped, insn)


def _type_14a(
    state: State, insn: Instruction, f: Mapping[str, int], name: str
) -> list[State]:
    """14a."""
    if _field(f, "l"):
        code = _field(f, "ureg")
        if _field(f, "g"):
            return [_stop(state, insn, "unsupported Type14a PM long-word access")]
        address = _wide(f, "addr")
        rendered = _render(Const(address))
        if _field(f, "d"):
            codes, values = _lw_store_pair(state.uregs, code)
            concrete_write = True
            for offset, value in enumerate(values):
                if not _dm_write(
                    state,
                    address + _normal_word_stride(address) * offset,
                    4,
                    value,
                    normal_word=True,
                ):
                    concrete_write = False
            _event(
                state,
                insn,
                "store",
                space="DM",
                ureg_pair=[UREG_NAMES[item] for item in codes],
                values=[_json_value(value) for value in values],
                address=address,
                expression=rendered,
                access_width="long-word",
                concrete_write=concrete_write,
                simd_companion_possible=False,
            )
        else:
            codes = _lw_load_codes(code)
            loaded_values: list[Const | None] = []
            for offset in range(len(codes)):
                loaded_values.append(
                    _dm_read(
                        state,
                        address + _normal_word_stride(address) * offset,
                        4,
                        normal_word=True,
                    )
                )
            for item, mem_value, offset in zip(
                codes, loaded_values, range(len(codes)), strict=True
            ):
                _write_ureg(
                    state,
                    item,
                    mem_value
                    or Unknown(
                        "memory-address "
                        + _render(
                            Const(address + _normal_word_stride(address) * offset)
                        )
                    ),
                )
            _event(
                state,
                insn,
                "load",
                space="DM",
                ureg_pair=[UREG_NAMES[item] for item in codes],
                address=address,
                expression=rendered,
                concrete_values=[
                    _json_value(value)
                    if value is not None
                    else {"unknown": "unavailable memory"}
                    for value in loaded_values
                ],
                access_width="long-word",
                simd_companion_possible=False,
            )
        return _advance(state, insn)
    address = _wide(f, "addr")
    rendered = _render(Const(address))
    code = _field(f, "ureg")
    space = "PM" if _field(f, "g") else "DM"
    try:
        companion = _simd_ureg_mem_companion(
            state, code, Const(address), store=bool(_field(f, "d"))
        )
    except ValueError as error:
        return [_stop(state, insn, str(error))]
    if _field(f, "d"):
        value = _ureg(state.uregs, code)
        store_now = state.concrete is not None
        wrote = (
            _dm_write(state, address, 4, value, normal_word=True)
            if store_now
            else False
        )
        _event(
            state,
            insn,
            "store",
            space=space,
            ureg=UREG_NAMES[code],
            value=value,
            address=address,
            expression=rendered,
            simd_companion_possible=True,
            **({"concrete_write": wrote} if store_now else {}),
        )
        if companion is not None and space == "DM":
            companion_value = _ureg(state.uregs, companion[0])
            _dm_write(state, companion[1], 4, companion_value, normal_word=True)
            _event(
                state,
                insn,
                "store-pey",
                space=space,
                ureg=UREG_NAMES[companion[0]],
                value=companion_value,
                address=companion[1],
                expression=_render(companion[1]),
            )
    else:
        loaded = _load_normal_ureg(state, space, address, code)
        _event(
            state,
            insn,
            "load",
            space=space,
            ureg=UREG_NAMES[code],
            address=address,
            expression=rendered,
            concrete_value=loaded,
            simd_companion_possible=True,
        )
        if companion is not None and space == "DM":
            companion_loaded = _load_normal_ureg(
                state, space, companion[1], companion[0]
            )
            _event(
                state,
                insn,
                "load-pey",
                space=space,
                ureg=UREG_NAMES[companion[0]],
                address=companion[1],
                expression=_render(companion[1]),
                concrete_value=companion_loaded,
            )
    return _advance(state, insn)


def _type_14d(
    state: State, insn: Instruction, f: Mapping[str, int], name: str
) -> list[State]:
    """14d."""
    # SHARC+ Core Programming Reference (out/refs/sharc-plus-prm)
    # pp.384-387, Figure 15-2 ("Type14d Instruction Opcode"): a direct-
    # address DM <-> R-register-file move, an "extension (exclusive
    # access) to 14a instruction". The w/ex/d/l opcode table (p.384-385)
    # lists only EX/LWEX rows (Dreg = dm(addr32) EX/LWEX and the mirror
    # store) at w=1,ex=1; every w=0 row is BH/BHEX (store, l selects
    # byte/short) or BHSE/BHSEEX (load, l selects byte/short and x
    # selects zero- vs sign-extend, p.386 BHSE/BHSEEX Encode Tables).
    # BWSE/SWSE are load-only per the Description on p.386. This
    # decoder does not model exclusive-access monitors, so it stops on
    # ex=1 (EX/BHEX/BHSEEX/LWEX) and on the undocumented w=1,ex=0
    # combination the opcode table has no row for.
    if _field(f, "ex"):
        return [_stop(state, insn, "unsupported Type14d exclusive access")]
    if _field(f, "w"):
        return [_stop(state, insn, "undocumented Type14d encoding (w=1, ex=0)")]
    store = bool(_field(f, "d"))
    l_bit, x_bit = _field(f, "l"), _field(f, "x")
    if store:
        if x_bit:
            return [
                _stop(
                    state,
                    insn,
                    "undocumented Type14d store encoding (x=1)",
                )
            ]
        access_width, width, signed = (
            ("byte", 1, False),
            ("short-word", 2, False),
        )[l_bit]
    else:
        access_width, width, signed = _TYPE14D_LOAD_WIDTHS[(l_bit, x_bit)]
    address = _wide(f, "addr")
    rendered = _render(Const(address))
    code = _field(f, "dreg")
    if store:
        value = _ureg(state.uregs, code)
        wrote = _dm_write(
            state,
            address,
            width,
            value,
            normal_word=access_width == "normal-word" or access_width == "long-word",
        )
        _event(
            state,
            insn,
            "store",
            space="DM",
            dreg="R%d" % code,
            value=value,
            address=address,
            expression=rendered,
            access_width=access_width,
            concrete_write=wrote,
        )
    else:
        loaded = _dm_read(
            state,
            address,
            width,
            signed,
            normal_word=access_width == "normal-word" or access_width == "long-word",
        )
        _write_ureg(state, code, loaded or Unknown("memory-address " + rendered))
        _event(
            state,
            insn,
            "load",
            space="DM",
            dreg="R%d" % code,
            address=address,
            expression=rendered,
            concrete_value=loaded,
            access_width=access_width,
        )
    return _advance(state, insn)


def _type_5a_move(
    state: State, insn: Instruction, f: Mapping[str, int], name: str
) -> list[State]:
    """5a_move, 5b_move."""
    cond = _field(f, "cond")
    old = _snapshot_uregs(state.uregs)
    compute = compute_y = None
    if name == "5a_move":
        try:
            compute, compute_y = _compute_simd(
                state, f, False, old, state.special, approx_recips=state.approx_recips
            )
        except ValueError as error:
            return [_stop(state, insn, str(error))]
    src = (
        _field(f, "srcureghigh") << 2
        | _field(f, "srcureglow[1:1]") << 1
        | _field(f, "srcureglow[0:0]")
    )
    dst = _field(f, "dstureg")
    copied = _ureg(old, src)
    if src == UREG_CODES["PX"]:
        # PRM p.2-8, Figure 2-2: Dreg receives PX bits 63:24.
        # The core's 32-bit Dreg model retains the upper 32 bits,
        # supplied exactly by PX2; other UREG transfers take PX1.
        copied = _ureg(
            old, UREG_CODES["PX2"] if dst < 16 or 80 <= dst < 96 else UREG_CODES["PX1"]
        )
    if name == "5b_move" and cond != 0x1F and _simd_active(state) is True:
        # PRM p.13-39 and Table 4-22 (p.4-55): register moves use
        # each PE's own predicate; an uncomplementary destination uses
        # PEx only. A shared source broadcasts to both destinations.
        predicate_x = _predicate_pe(state, cond, "x")
        companion_dst = _cureg_code(dst)
        predicate_y = (
            _predicate_pe(state, cond, "y") if companion_dst is not None else False
        )
        if predicate_x is None or predicate_y is None:
            return [_stop(state, insn, "unknown conditional SIMD Type5b predicate")]
        if predicate_x:
            _write_ureg(state, dst, copied)
        if predicate_y:
            assert companion_dst is not None
            companion_src = _cureg_code(src)
            _write_ureg(
                state,
                companion_dst,
                copied if companion_src is None else _ureg(old, companion_src),
            )
        _event(
            state,
            insn,
            "conditional-simd-ureg-copy",
            source=UREG_NAMES[src],
            destination=UREG_NAMES[dst],
            predicate_x=predicate_x,
            predicate_y=predicate_y,
        )
        return _advance(state, insn)
    # The unconditional ("always") case is PE-independent by
    # construction, so its SIMD companion can be resolved without
    # forking on the predicate at all (SHARC+ PRM p.28, "Data and
    # Complementary Data Register Transfers": a complementary
    # destination with an uncomplementary source loads both PEs from
    # that one source -- "R5 = I8; loads R5 and S5 with I8"; a
    # complementary source and destination move each PE from its own
    # register; an uncomplementary destination gets no implicit move
    # at all, p.4-55 Table 4-22). Conditional (cond != 0x1F) ureg
    # copies are not SIMD-duplicated here: PEx's and PEy's predicates
    # can differ, which would need this form's existing predicate-fork
    # logic doubled for the PEy side too. An unresolved MODE1.PEYEN is
    # treated like SISD (no companion), exactly like
    # _simd_ureg_mem_companion: the explicit copy is correct either
    # way, so unknown MODE1 only leaves a companion unmodelled.
    cureg_dst = _cureg_code(dst) if cond == 0x1F else None
    simd_companion_source = None
    if cureg_dst is not None and _simd_active(state) is True:
        cureg_src = _cureg_code(src)
        simd_companion_source = cureg_src if cureg_src is not None else src
    predicate = _predicate(state, cond)
    if predicate is False:
        _event(
            state,
            insn,
            "ureg-copy-skipped",
            source=UREG_NAMES[src],
            destination=UREG_NAMES[dst],
            condition=cond,
            predicate_assumption=False,
        )
        return _advance(state, insn)
    executed = state if predicate is True else _copy(state)
    # The Type 5a data move and compute both consume the pre-instruction file.
    _write_ureg(executed, dst, copied)
    if simd_companion_source is not None:
        # simd_companion_source is only ever set inside the "cureg_dst is
        # not None" branch above, so this implies cureg_dst is not None too.
        assert cureg_dst is not None
        _write_ureg(
            executed,
            cureg_dst,
            copied
            if simd_companion_source == src
            else _ureg(old, simd_companion_source),
        )
    if compute is not None:
        # PEy's compute only when unconditional (see _type_3a).
        _apply_compute_simd(
            executed, insn, compute, compute_y if cond == 0x1F else None
        )
    _event(
        executed,
        insn,
        "ureg-copy",
        source=UREG_NAMES[src],
        destination=UREG_NAMES[dst],
        condition=cond,
        predicate_assumption=True,
        simd_companion=(
            {
                "source": UREG_NAMES[simd_companion_source],
                "destination": UREG_NAMES[cureg_dst],
            }
            if simd_companion_source is not None and cureg_dst is not None
            else None
        ),
    )
    if predicate is True:
        return _advance(executed, insn)
    skipped = _copy(state)
    _event(
        skipped,
        insn,
        "ureg-copy-skipped",
        source=UREG_NAMES[src],
        destination=UREG_NAMES[dst],
        condition=cond,
        predicate_assumption=False,
    )
    return _advance(executed, insn) + _advance(skipped, insn)


def _companion_transfer(
    state: State,
    insn: Instruction,
    companion: tuple[int, Value],
    store: bool,
    access_width: str,
    source: Mapping[int, Value],
    addressing_mode: str,
) -> None:
    """The implicit (PEy) half of a SIMD DM transfer: Cdreg <-> the
    companion address _simd_ureg_mem_companion() returned (PRM p.212 Table
    6-10; p.7-5 for byte and short word). SOURCE holds the register
    values from before the instruction, for a store."""
    code, address = companion
    width = _COMPANION_WIDTH_BYTES[access_width]
    if store:
        value = _ureg(source, code)
        _dm_write(
            state,
            address,
            width,
            value,
            normal_word=access_width == "normal-word" or access_width == "long-word",
        )
        _event(
            state,
            insn,
            "store-pey",
            space="DM",
            ureg=UREG_NAMES[code],
            value=value,
            address=address,
            expression=_render(address),
            addressing_mode=addressing_mode,
            access_width=access_width,
        )
        return
    loaded: Const | dict[str, int] | None
    if access_width == "normal-word":
        loaded = _load_normal_ureg(state, "DM", address, code)
    else:
        scalar = _dm_read(
            state,
            address,
            width,
            access_width.endswith("sign-extended"),
            normal_word=access_width == "normal-word" or access_width == "long-word",
        )
        _write_ureg(
            state, code, scalar or Unknown("memory-address " + _render(address))
        )
        loaded = scalar
    _event(
        state,
        insn,
        "load-pey",
        space="DM",
        ureg=UREG_NAMES[code],
        address=address,
        expression=_render(address),
        concrete_value=loaded,
        addressing_mode=addressing_mode,
        access_width=access_width,
    )


def _pm_companion_transfer(
    state: State,
    insn: Instruction,
    companion: tuple[int, Operand],
    store: bool,
    source: Mapping[int, Value],
    addressing_mode: str,
) -> None:
    """Record a PEy PM transfer without treating PM as concrete DM backing."""
    code, address = companion
    if store:
        _event(
            state,
            insn,
            "store-pey",
            space="PM",
            ureg=UREG_NAMES[code],
            value=_ureg(source, code),
            address=address,
            expression=_render(address),
            addressing_mode=addressing_mode,
            access_width="normal-word",
            concrete_write=False,
        )
        return
    _write_ureg(state, code, Unknown("program-memory " + _render(address)))
    _event(
        state,
        insn,
        "load-pey",
        space="PM",
        ureg=UREG_NAMES[code],
        address=address,
        expression=_render(address),
        concrete_value=None,
        addressing_mode=addressing_mode,
        access_width="normal-word",
    )


def _type_nw_companion(
    old: Mapping[int, Value],
    index: int,
    code: int,
    address: Operand,
    store: bool,
    scale: int,
) -> tuple[int, Operand] | None:
    """Type 1a/15 normal-word PEy transfer, including BDCST I1/I9 loads."""
    cureg = _cureg_code(code)
    if cureg is None:
        return None
    mode = old.get(UREG_CODES["MODE1"])
    mode_value = mode.value if isinstance(mode, Const) else 0
    broadcast = not store and (
        (index == 1 and mode_value & (1 << 23))
        or (index == 9 and mode_value & (1 << 22))
    )
    if not broadcast and not (mode_value & (1 << 21)):
        return None
    offset = 0 if broadcast else scale
    return cureg, _add(address, Const(offset), "%s + %d" % (_render(address), offset))


_COMPANION_WIDTH_BYTES = {
    "normal-word": 4,
    "byte": 1,
    "byte-sign-extended": 1,
    "short-word": 2,
    "short-word-sign-extended": 2,
}


def _type_4a(
    state: State, insn: Instruction, f: Mapping[str, int], name: str
) -> list[State]:
    """4a; PRM pp.13-26--13-28: condition gates transfer and compute."""
    cond = _field(f, "cond")
    if cond == 0x1F:
        return _type_4a_access(state, insn, f, name)
    # Conditional SIMD needs independent per-PE gating, including DAG
    # updates. Keep that boundary explicit rather than applying PEx to PEy.
    if _simd_active(state) is not False:
        return [_stop(state, insn, "conditional Type4a SIMD is not modeled")]
    predicate = _predicate_pe(state, cond, "x")
    if predicate is True:
        return _type_4a_access(state, insn, f, name)
    if predicate is False:
        _event(
            state, insn, "type4a-skipped", condition=cond, predicate_assumption=False
        )
        return _advance(state, insn)
    executed, skipped = _copy(state), _copy(state)
    result = _type_4a_access(executed, insn, f, name)
    _event(skipped, insn, "type4a-skipped", condition=cond, predicate_assumption=False)
    return result + _advance(skipped, insn)


def _type_4a_access(
    state: State, insn: Instruction, f: Mapping[str, int], name: str
) -> list[State]:
    """Execute the transfer and optional compute after predicate selection."""
    old = _snapshot_uregs(state.uregs)
    try:
        compute, compute_y = _compute_simd(
            state, f, False, old, state.special, approx_recips=state.approx_recips
        )
    except ValueError as error:
        return [_stop(state, insn, str(error))]
    index = _field(f, "i") + (8 if _field(f, "g") else 0)
    offset = _signed((_field(f, "data[5:5]") << 5) | _field(f, "data[4:0]"), 6)
    # The immediate modifier is in normal-word address units.  Only turn
    # it into a byte displacement when the caller has explicitly fixed
    # internal normal words at 32 bits.
    iv = _ureg(old, 16 + index)
    offset *= _access_modifier_scale("normal-word", state.assume_nw32, iv)
    space = "PM" if _field(f, "g") else "DM"
    if _field(f, "u"):
        address, next_i = iv, _add(iv, Const(offset), "I%d + %d" % (index, offset))
    else:
        address, next_i = _add(iv, Const(offset), "I%d + %d" % (index, offset)), iv
    code = _field(f, "dreg")
    # SIMD: the explicit transfer moves Rn at Ia; the implicit one moves
    # Sn at Ia+k (SHARC+ PRM p.212, Table 6-10), as for Type3b/14a/15a.
    # FUN_1c403c's page copy (sw 0x1c405d/0x1c4060) needs it.
    companion = None
    if space == "DM":
        try:
            companion = _simd_ureg_mem_companion(
                state, code, address, store=bool(_field(f, "d"))
            )
        except ValueError as error:
            return [_stop(state, insn, str(error))]
    if _field(f, "d"):
        value = _ureg(old, code)
        wrote = _dm_write(state, address, 4, value, normal_word=True)
        _event(
            state,
            insn,
            "store",
            space=space,
            dreg="R%d" % code,
            value=value,
            address=address,
            expression=_render(address),
            concrete_write=wrote,
        )
        if companion is not None:
            companion_value = _ureg(old, companion[0])
            _dm_write(state, companion[1], 4, companion_value, normal_word=True)
            _event(
                state,
                insn,
                "store-pey",
                space=space,
                ureg=UREG_NAMES[companion[0]],
                value=companion_value,
                address=companion[1],
                expression=_render(companion[1]),
            )
    else:
        loaded = _dm_read(state, address, 4, normal_word=True)
        _write_ureg(
            state, code, loaded or Unknown("memory-address " + _render(address))
        )
        _event(
            state,
            insn,
            "load",
            space=space,
            dreg="R%d" % code,
            address=address,
            expression=_render(address),
            concrete_value=loaded,
        )
        if companion is not None:
            companion_loaded = _dm_read(state, companion[1], 4, normal_word=True)
            _write_ureg(
                state,
                companion[0],
                companion_loaded or Unknown("memory-address " + _render(companion[1])),
            )
            _event(
                state,
                insn,
                "load-pey",
                space=space,
                ureg=UREG_NAMES[companion[0]],
                address=companion[1],
                expression=_render(companion[1]),
                concrete_value=companion_loaded,
            )
    state.uregs[16 + index] = next_i
    if compute is not None:
        _apply_compute_simd(state, insn, compute, compute_y)
    return _advance(state, insn)


def _type_4b_access(
    executed: State,
    insn: Instruction,
    index: int,
    offset: int,
    post_modify: bool,
    store: bool,
    space: str,
    code: int,
    width: int,
    signed: bool,
    access_width: str,
    cond: int,
    companion: tuple[int, Value] | None,
    mode: str,
) -> None:
    """Type 4b's access on EXECUTED (the state itself, or the executed fork)."""
    old = _snapshot_uregs(executed.uregs)
    iv = _ureg(old, 16 + index)
    address = (
        iv if post_modify else _add(iv, Const(offset), "I%d + %d" % (index, offset))
    )
    if store:
        value = _ureg(old, code)
        wrote = _dm_write(
            executed,
            address,
            width,
            value,
            normal_word=access_width == "normal-word" or access_width == "long-word",
        )
        _event(
            executed,
            insn,
            "store",
            space=space,
            dreg="R%d" % code,
            value=value,
            address=address,
            expression=_render(address),
            concrete_write=wrote,
            addressing_mode="post-modify" if post_modify else "pre-modify",
            access_width=access_width,
            condition=cond,
            predicate_assumption=True,
        )
    else:
        loaded = _dm_read(
            executed,
            address,
            width,
            signed,
            normal_word=access_width == "normal-word" or access_width == "long-word",
        )
        _write_ureg(
            executed, code, loaded or Unknown("memory-address " + _render(address))
        )
        _event(
            executed,
            insn,
            "load",
            space=space,
            dreg="R%d" % code,
            address=address,
            expression=_render(address),
            concrete_value=loaded,
            addressing_mode="post-modify" if post_modify else "pre-modify",
            access_width=access_width,
            condition=cond,
            predicate_assumption=True,
        )
    if companion is not None:
        _companion_transfer(executed, insn, companion, store, access_width, old, mode)
    if post_modify:
        executed.uregs[16 + index] = _add(
            iv, Const(offset), "I%d + %d" % (index, offset)
        )


def _type_4b(
    state: State, insn: Instruction, f: Mapping[str, int], name: str
) -> list[State]:
    """4b."""
    # SHARC+ Core Programming Reference rev. 1.4, pp. 13-29--13-32:
    # conditional DM/PM transfer with a signed six-bit immediate modifier.
    # Its (l, x, w) BH/BHSE tables (p.13-32) are TYPE4B_ACCESS_WIDTHS, not
    # Type3b's ACCESS_WIDTHS: (1, 1, 1) is the plain normal-word access and
    # Type4b has no (lw) option (encoding.py has the evidence). Reading
    # (1, 1, 1) as a long word made every such load Unknown and dropped
    # every such store, e.g. sw 0x1c336b "IF SZ R8 = DM(I6 - 12)", which
    # picks a voice's left-channel sample address.
    width_fields = (_field(f, "l"), _field(f, "x"), _field(f, "w"))
    access_width = TYPE4B_ACCESS_WIDTHS.get(width_fields)
    if access_width is None:
        return [_stop(state, insn, "unsupported Type4b access width")]
    width = _ACCESS_WIDTH_BYTES[access_width]
    signed = access_width.endswith("sign-extended")
    store = bool(_field(f, "d"))
    if store and signed:
        return [_stop(state, insn, "unsupported Type4b sign-extended store")]
    bank = 8 if _field(f, "g") else 0
    index = _field(f, "i") + bank
    offset = _signed((_field(f, "data[5:5]") << 5) | _field(f, "data[4:0]"), 6)
    offset *= _access_modifier_scale(
        access_width, state.assume_nw32, _ureg(state.uregs, 16 + index)
    )
    post_modify = bool(_field(f, "u"))
    space = "PM" if bank else "DM"
    code = _field(f, "dreg")
    cond = _field(f, "cond")

    # SIMD (PRM p.13-30): the Y element uses Ia + k and Cdreg, k = one
    # normal word or the adjacent short word/byte (p.7-5). As for
    # Type3b, the transfer follows the PEx condition.
    companion = None
    if space == "DM":
        iv0 = _ureg(state.uregs, 16 + index)
        address0 = (
            iv0
            if post_modify
            else _add(iv0, Const(offset), "I%d + %d" % (index, offset))
        )
        try:
            companion = _simd_ureg_mem_companion(
                state, code, address0, access_width, store=bool(_field(f, "d"))
            )
        except ValueError as error:
            return [_stop(state, insn, str(error))]
    mode = "post-modify" if post_modify else "pre-modify"

    predicate = _predicate(state, cond)
    if predicate is True:
        _type_4b_access(
            state,
            insn,
            index,
            offset,
            post_modify,
            store,
            space,
            code,
            width,
            signed,
            access_width,
            cond,
            companion,
            mode,
        )
        return _advance(state, insn)
    if predicate is False:
        _event(
            state,
            insn,
            "memory-access-skipped",
            condition=cond,
            predicate_assumption=False,
        )
        return _advance(state, insn)
    executed, skipped = _copy(state), _copy(state)
    _type_4b_access(
        executed,
        insn,
        index,
        offset,
        post_modify,
        store,
        space,
        code,
        width,
        signed,
        access_width,
        cond,
        companion,
        mode,
    )
    _event(
        skipped,
        insn,
        "memory-access-skipped",
        condition=cond,
        predicate_assumption=False,
    )
    return _advance(executed, insn) + _advance(skipped, insn)


def _type_3b_access(
    executed: State,
    insn: Instruction,
    f: Mapping[str, int],
    index: int,
    modifier: int,
    post_modify: bool,
    addressing_mode: str,
    store: bool,
    space: str,
    ureg: int,
    access_width: str,
    cond: int,
) -> str | None:
    """Type 3b's access on EXECUTED; returns a stop reason, or None."""
    old = _snapshot_uregs(executed.uregs)
    iv, mv = _ureg(old, 16 + index), _ureg(old, 32 + modifier)
    width = _ACCESS_WIDTH_BYTES[access_width]
    scale = _access_modifier_scale(access_width, executed.assume_nw32, iv)
    scaled_mv = _multiply(mv, Const(scale), f"M{modifier} * {scale}")
    address = (
        iv if post_modify else _add(iv, scaled_mv, f"I{index} + M{modifier} * {scale}")
    )
    try:
        companion = _simd_ureg_mem_companion(
            executed, ureg, address, access_width, store=bool(_field(f, "d"))
        )
    except ValueError as error:
        return str(error)
    if store:
        value = _ureg(old, ureg)
        wrote = _dm_write(
            executed,
            address,
            width,
            value,
            normal_word=access_width == "normal-word" or access_width == "long-word",
        )
        _event(
            executed,
            insn,
            "store",
            space=space,
            ureg=UREG_NAMES[ureg],
            value=value,
            address=address,
            expression=_render(address),
            concrete_write=wrote,
            addressing_mode=addressing_mode,
            access_width=access_width,
            condition=cond,
            predicate_assumption=True,
        )
        if companion is not None and space == "DM":
            companion_value = _ureg(old, companion[0])
            _dm_write(
                executed,
                companion[1],
                width,
                companion_value,
                normal_word=access_width == "normal-word"
                or access_width == "long-word",
            )
            _event(
                executed,
                insn,
                "store-pey",
                space=space,
                ureg=UREG_NAMES[companion[0]],
                value=companion_value,
                address=companion[1],
                expression=_render(companion[1]),
                addressing_mode=addressing_mode,
                access_width=access_width,
            )
    else:
        if access_width == "normal-word":
            loaded: Const | dict[str, int] | None = _load_normal_ureg(
                executed, space, address, ureg
            )
        else:
            scalar_loaded = _dm_read(
                executed,
                address,
                width,
                access_width.endswith("sign-extended"),
                normal_word=access_width == "normal-word"
                or access_width == "long-word",
            )
            _write_ureg(
                executed,
                ureg,
                scalar_loaded or Unknown("memory-address " + _render(address)),
            )
            loaded = scalar_loaded
        _event(
            executed,
            insn,
            "load",
            space=space,
            ureg=UREG_NAMES[ureg],
            address=address,
            expression=_render(address),
            concrete_value=loaded,
            addressing_mode=addressing_mode,
            access_width=access_width,
            condition=cond,
            predicate_assumption=True,
        )
        if companion is not None and space == "DM":
            _companion_transfer(
                executed, insn, companion, False, access_width, old, addressing_mode
            )
    if post_modify:
        executed.uregs[16 + index] = _add(
            iv, scaled_mv, "I%d + M%d * %d" % (index, modifier, scale)
        )
    return None


def _type_3b(
    state: State, insn: Instruction, f: Mapping[str, int], name: str
) -> list[State]:
    """3b."""
    # SHARC+ Core Programming Reference rev. 1.4, pp. 13-16--13-19.
    # Validate and decode the complete access before making a predicate
    # assumption, so unsupported forms stop rather than creating paths.
    width_fields = (_field(f, "l"), _field(f, "x"), _field(f, "w"))
    access_width = ACCESS_WIDTHS.get(width_fields)
    if access_width is None:
        return [_stop(state, insn, "unsupported Type3b access width")]
    store = bool(_field(f, "d"))
    if store and access_width.endswith("sign-extended"):
        return [_stop(state, insn, "unsupported Type3b sign-extended store")]
    bank = 8 if _field(f, "g") else 0
    index, modifier = _field(f, "i") + bank, _field(f, "m") + bank
    post_modify = bool(_field(f, "u"))
    addressing_mode = "post-modify" if post_modify else "pre-modify"
    space = "PM" if bank else "DM"
    ureg = _field(f, "ureg")
    cond = _field(f, "cond")

    predicate = _predicate(state, cond)
    if predicate is True:
        error = _type_3b_access(
            state,
            insn,
            f,
            index,
            modifier,
            post_modify,
            addressing_mode,
            store,
            space,
            ureg,
            access_width,
            cond,
        )
        if error:
            return [_stop(state, insn, error)]
        return _advance(state, insn)
    if predicate is False:
        # Matching _type_3a's identical fast path above: a concretely
        # False predicate must not fork (the missing case here was
        # forking on every False predicate, even a fully known one,
        # which a single-path concrete runner cannot resolve).
        _event(
            state,
            insn,
            "memory-access-skipped",
            space=space,
            ureg=UREG_NAMES[ureg],
            addressing_mode=addressing_mode,
            access_width=access_width,
            condition=cond,
            predicate_assumption=False,
        )
        return _advance(state, insn)
    executed, skipped = _copy(state), _copy(state)
    error = _type_3b_access(
        executed,
        insn,
        f,
        index,
        modifier,
        post_modify,
        addressing_mode,
        store,
        space,
        ureg,
        access_width,
        cond,
    )
    if error:
        return [_stop(executed, insn, error)]
    _event(
        skipped,
        insn,
        "memory-access-skipped",
        space=space,
        ureg=UREG_NAMES[ureg],
        addressing_mode=addressing_mode,
        access_width=access_width,
        condition=cond,
        predicate_assumption=False,
    )
    return _advance(executed, insn) + _advance(skipped, insn)


def _type_3c(
    state: State, insn: Instruction, f: Mapping[str, int], name: str
) -> list[State]:
    """3c."""
    index, modifier = _field(f, "dmi"), _field(f, "dmm")
    old = _snapshot_uregs(state.uregs)
    iv, mv = _ureg(old, 16 + index), _ureg(old, 32 + modifier)
    scale = _access_modifier_scale("normal-word", state.assume_nw32, iv)
    scaled_mv = _multiply(mv, Const(scale), "M%d * %d" % (modifier, scale))
    address = iv
    companion = _type_nw_companion(
        old, index, _field(f, "dreg"), address, bool(_field(f, "d")), scale
    )
    state.uregs[16 + index] = _add(
        iv, scaled_mv, "I%d + M%d * %d" % (index, modifier, scale)
    )
    code = _field(f, "dreg")
    if _field(f, "d"):
        value = _ureg(old, code)
        wrote = _dm_write(state, address, 4, value, normal_word=True)
        _event(
            state,
            insn,
            "store",
            space="DM",
            dreg="R%d" % code,
            value=value,
            address=address,
            expression=_render(address),
            concrete_write=wrote,
        )
        if companion is not None:
            _companion_transfer(
                state, insn, companion, True, "normal-word", old, "post-modify"
            )
    else:
        loaded = _dm_read(state, address, 4, normal_word=True)
        _write_ureg(
            state, code, loaded or Unknown("memory-address " + _render(address))
        )
        _event(
            state,
            insn,
            "load",
            space="DM",
            dreg="R%d" % code,
            address=address,
            expression=_render(address),
            concrete_value=loaded,
        )
        if companion is not None:
            _companion_transfer(
                state, insn, companion, False, "normal-word", old, "post-modify"
            )
    return _advance(state, insn)


def _type_16a(
    state: State, insn: Instruction, f: Mapping[str, int], name: str
) -> list[State]:
    """16a, 16b."""
    if name == "16a" and (_field(f, "by") or _field(f, "sl")):
        return [_stop(state, insn, "unsupported Type16a by/sl")]
    index, modifier = (
        _field(f, "i") + (8 if _field(f, "g") else 0),
        _field(f, "m") + (8 if _field(f, "g") else 0),
    )
    old = _snapshot_uregs(state.uregs)
    iv, mv = _ureg(old, 16 + index), _ureg(old, 32 + modifier)
    scale = _access_modifier_scale(
        "normal-word", state.assume_nw32 and not bool(_field(f, "g")), iv
    )
    scaled_mv = _multiply(mv, Const(scale), "M%d * %d" % (modifier, scale))
    address = iv
    value = Const(
        _wide(f, "data") if name == "16a" else _signed(_field(f, "data[15:0]"), 16)
    )
    wrote = _dm_write(state, address, 4, value, normal_word=True)
    _event(
        state,
        insn,
        "store",
        space="PM" if _field(f, "g") else "DM",
        address=address,
        expression=_render(address),
        value=value,
        by=_field(f, "by") if name == "16a" else 0,
        sl=_field(f, "sl") if name == "16a" else 0,
        concrete_write=wrote,
    )
    state.uregs[16 + index] = _add(
        iv, scaled_mv, "I%d + M%d * %d" % (index, modifier, scale)
    )
    return _advance(state, insn)


def _type_15b(
    state: State, insn: Instruction, f: Mapping[str, int], name: str
) -> list[State]:
    """15b.

    SHARC+ Core Programming Reference (out/refs/sharc-plus-prm) pp.389-392,
    Figure 15-4 p.392: DM(<data7>,Ia) = Ureg / Ureg = DM(<data7>,Ia).
    p.391 Description: "The optional (LW) in this syntax lets programs
    specify long word addressing, overriding default addressing from the
    memory map... if the instruction type uses optional forced long word
    modifier (LW)... register pair access is done" -- the same
    register-pair access (state._lw_pair_mate) at (address, address+4)
    Type15a's own (lw) already implements (this form's docstring, citing
    p.389), not a single wider (8-byte) transfer into one register. That was the
    previous model here; sharc_core.memory._dm_read intentionally refuses
    any width > 4 ("a register pair, which this tracer does not model"),
    so every (LW) Type15b site read Unknown rather than a wrong value --
    found tracing docs/findings/06's voice render polyphase interpolator
    (FUN_1c4f81's coefficient-table reads at sw 0x1c50b9 etc., all (LW)
    Type15b), which needs six int16 taps per phase from three (LW) pair
    reads, not six separate ones.
    p.6-10's (lw) row "scales the same as an unqualified/(nw) access, not
    by 8" (Type15a's docstring cites the same table): the <data7>
    displacement is the normal-word (x4 under assume_nw32) scale
    unconditionally, independent of l -- only the pair's own second word
    is a fixed +4, exactly as Type15a's l=1 branch already does.
    """
    index = _field(f, "i") + (8 if _field(f, "g") else 0)
    space = "PM" if _field(f, "g") else "DM"
    offset = _signed(_field(f, "data[6:0]"), 7)
    iv = _ureg(state.uregs, 16 + index)
    offset *= _access_modifier_scale("normal-word", state.assume_nw32, iv)
    address = _add(iv, Const(offset), "I%d + %d" % (index, offset))
    rendered = _render(address)
    code = _field(f, "ureg")
    if _field(f, "l"):
        if _field(f, "d"):
            codes, values = _lw_store_pair(state.uregs, code)
            offsets: list[Operand] = []
            for off in range(len(codes)):
                offsets.append(
                    _add(
                        address,
                        Const(_normal_word_stride(address) * off),
                        "%s + %d"
                        % (
                            rendered,
                            _normal_word_stride(address) * off,
                        ),
                    )
                )
            all_written = True
            for offset_address, value in zip(offsets, values, strict=True):
                if not _dm_write(state, offset_address, 4, value, normal_word=True):
                    all_written = False
            _event(
                state,
                insn,
                "store",
                space=space,
                ureg_pair=[UREG_NAMES[item] for item in codes],
                values=[_json_value(value) for value in values],
                address=address,
                expression=rendered,
                access_width="long-word",
                long_word=True,
                concrete_write=all_written,
            )
        else:
            codes = _lw_load_codes(code)
            loaded_values: list[Const | None] = []
            for off in range(len(codes)):
                offset_address = _add(
                    address,
                    Const(_normal_word_stride(address) * off),
                    "%s + %d"
                    % (
                        rendered,
                        _normal_word_stride(address) * off,
                    ),
                )
                loaded_values.append(
                    _dm_read(state, offset_address, 4, normal_word=True)
                )
            for item, mem_value in zip(codes, loaded_values, strict=True):
                _write_ureg(
                    state, item, mem_value or Unknown("memory-address " + rendered)
                )
            _event(
                state,
                insn,
                "load",
                space=space,
                ureg_pair=[UREG_NAMES[item] for item in codes],
                address=address,
                expression=rendered,
                access_width="long-word",
                long_word=True,
                concrete_values=[
                    _json_value(value)
                    if value is not None
                    else {"unknown": "unavailable memory"}
                    for value in loaded_values
                ],
            )
        return _advance(state, insn)
    # A non-(LW) Type15b is a normal-word DAG transfer.  In SIMD mode it
    # therefore has the same implicit PEy half as Type14a/15a/3a: the
    # complementary data register accesses the next normal word (PRM
    # Table 6-10).  (LW) returned above and explicitly overrides SIMD.
    old = _snapshot_uregs(state.uregs)
    companion = None
    # Type15a/b's form-specific SIMD rule affects only the Cureg subset.
    # Uncomplementary UREG transfers (for example TCOUNT) have the same
    # behavior in SIMD and SISD mode, unlike forms that use the generic
    # Table 4-22 replication rule.
    if _cureg_code(code) is not None:
        companion = _type_nw_companion(
            old,
            index,
            code,
            address,
            bool(_field(f, "d")),
            _normal_word_stride(address),
        )
    if _field(f, "d"):
        value = state.uregs.get(code, Unknown("uninitialized " + UREG_NAMES[code]))
        wrote = _dm_write(state, address, 4, value, normal_word=True)
        _event(
            state,
            insn,
            "store",
            space=space,
            ureg=UREG_NAMES[code],
            address=address,
            expression=rendered,
            long_word=False,
            concrete_write=wrote,
        )
        if companion is not None:
            if space == "DM":
                _companion_transfer(
                    state, insn, companion, True, "normal-word", old, "pre-modify"
                )
            else:
                _pm_companion_transfer(state, insn, companion, True, old, "pre-modify")
    else:
        # The PM data bus shares the physical address space (PRM p.7-2).
        # This includes the ordinary 32-bit PM stack reads in RTOS restore.
        loaded = _load_normal_ureg(state, space, address, code)
        _event(
            state,
            insn,
            "load",
            space=space,
            ureg=UREG_NAMES[code],
            address=address,
            expression=rendered,
            long_word=False,
            concrete_value=loaded,
        )
        if companion is not None:
            if space == "DM":
                _companion_transfer(
                    state, insn, companion, False, "normal-word", old, "pre-modify"
                )
            else:
                _pm_companion_transfer(state, insn, companion, False, old, "pre-modify")
    return _advance(state, insn)


def _type_15a(
    state: State, insn: Instruction, f: Mapping[str, int], name: str
) -> list[State]:
    """15a."""
    # SHARC+ Core Programming Reference (out/refs/sharc-plus-prm)
    # pp.387-390, Figure 15-3 p.390 ("Type15a Instruction Opcode"):
    # DM(<data32>,Ia) = Ureg / Ureg = DM(<data32>,Ia), and the PM/Ic
    # form when g=1 (opcode table p.387: g=0 -> dm/I1REG(DAG1), g=1 ->
    # pm/I2REG(DAG2)). p.388 Description: "The I register is pre-
    # modified with an immediate value specified in the instruction.
    # The I register is not updated" -- pre-modify without writeback,
    # unlike Type19a's post-modify MODIFY. The optional (lw) "forces
    # register pair access" (p.389), modelled the same way as Type14a's
    # own (lw) register-pair form, with no SIMD companion.
    # SHARC+ Core Programming Reference (out/refs/sharc-plus-prm) pp.6-9
    # -6-10 ("Enhanced Modify Instruction for Address Scaling") and
    # Table 6-2 p.6-10/6-11: in byte-addressed space, an immediate
    # displacement on a load/store is scaled by the access size (the
    # (lw) row scales the same as an unqualified/(nw) access, not by 8);
    # in word-addressed space it is not scaled at all. This tracer's
    # opt-in 32-bit-normal-word model represents such pointers in byte
    # space (matching Type4a/4b/15b's own modifier scaling above), so
    # <data32> needs the same four-byte scaling those forms already
    # apply -- this 32-bit displacement was previously added unscaled,
    # which put a Type15a access at a different address than a Type4a/
    # 4b/15b access using the same architectural word offset from the
    # same I register (e.g. a stage-6 wavetable local stored via Type4a
    # at DM(I6-4) and re-read via Type15a at DM(0xfffffffc,I6)).
    bank = 8 if _field(f, "g") else 0
    index = _field(f, "i[2:0]") + bank
    iv = _ureg(state.uregs, 16 + index)
    addr = _wide(f, "addr") * _access_modifier_scale(
        "normal-word", state.assume_nw32, iv
    )
    address = _add(iv, Const(addr), "I%d + %d" % (index, addr))
    rendered = _render(address)
    space = "PM" if bank else "DM"
    if _field(f, "l"):
        code = _field(f, "ureg")
        if _field(f, "d"):
            codes, values = _lw_store_pair(state.uregs, code)
            offsets: list[Operand] = []
            for offset in range(len(codes)):
                offsets.append(
                    _add(
                        address,
                        Const(_normal_word_stride(address) * offset),
                        "%s + %d"
                        % (
                            rendered,
                            _normal_word_stride(address) * offset,
                        ),
                    )
                )
            concrete_write = True
            for offset_address, value in zip(offsets, values, strict=True):
                wrote = _dm_write(state, offset_address, 4, value, normal_word=True)
                if not wrote:
                    concrete_write = False
            _event(
                state,
                insn,
                "store",
                space=space,
                ureg_pair=[UREG_NAMES[item] for item in codes],
                values=[_json_value(value) for value in values],
                address=address,
                expression=rendered,
                access_width="long-word",
                concrete_write=concrete_write,
                simd_companion_possible=False,
            )
        else:
            codes = _lw_load_codes(code)
            loaded_values: list[Const | None] = []
            for offset in range(len(codes)):
                offset_address = _add(
                    address,
                    Const(_normal_word_stride(address) * offset),
                    "%s + %d"
                    % (
                        rendered,
                        _normal_word_stride(address) * offset,
                    ),
                )
                loaded_values.append(
                    _dm_read(state, offset_address, 4, normal_word=True)
                )
            for item, mem_value in zip(codes, loaded_values, strict=True):
                _write_ureg(
                    state, item, mem_value or Unknown("memory-address " + rendered)
                )
            _event(
                state,
                insn,
                "load",
                space=space,
                ureg_pair=[UREG_NAMES[item] for item in codes],
                address=address,
                expression=rendered,
                concrete_values=[
                    _json_value(value)
                    if value is not None
                    else {"unknown": "unavailable memory"}
                    for value in loaded_values
                ],
                access_width="long-word",
                simd_companion_possible=False,
            )
        return _advance(state, insn)
    code = _field(f, "ureg")
    old = _snapshot_uregs(state.uregs)
    companion = None
    # Type15a's form-specific SIMD rule affects only the Cureg subset.
    # Uncomplementary UREG transfers have the same behavior in SIMD and
    # SISD mode (PRM p.16-8), unlike forms that use Table 4-22 replication.
    if _cureg_code(code) is not None:
        companion = _type_nw_companion(
            old,
            index,
            code,
            address,
            bool(_field(f, "d")),
            _normal_word_stride(address),
        )
    if _field(f, "d"):
        value = _ureg(state.uregs, code)
        wrote = _dm_write(state, address, 4, value, normal_word=True)
        _event(
            state,
            insn,
            "store",
            space=space,
            ureg=UREG_NAMES[code],
            value=value,
            address=address,
            expression=rendered,
            simd_companion_possible=True,
            concrete_write=wrote,
        )
        if companion is not None:
            if space == "DM":
                _companion_transfer(
                    state, insn, companion, True, "normal-word", old, "pre-modify"
                )
            else:
                _pm_companion_transfer(state, insn, companion, True, old, "pre-modify")
    else:
        loaded = _load_normal_ureg(state, space, address, code)
        _event(
            state,
            insn,
            "load",
            space=space,
            ureg=UREG_NAMES[code],
            address=address,
            expression=rendered,
            concrete_value=loaded,
            simd_companion_possible=True,
        )
        if companion is not None:
            if space == "DM":
                _companion_transfer(
                    state, insn, companion, False, "normal-word", old, "pre-modify"
                )
            else:
                _pm_companion_transfer(state, insn, companion, False, old, "pre-modify")
    return _advance(state, insn)


def _type_1a_access(
    space: str,
    index: int,
    modifier: int,
    dreg: int,
    store: bool,
    state: State,
    insn: Instruction,
    old: Mapping[int, Value],
    scale: int,
) -> None:
    """One of Type 1a's two post-modify transfers (DM or PM), reading
    operands from OLD, the pre-instruction file."""
    iv, mv = _ureg(old, 16 + index), _ureg(old, 32 + modifier)
    scale = _access_modifier_scale("normal-word", state.assume_nw32, iv)
    scaled_mv = _multiply(mv, Const(scale), "M%d * %d" % (modifier, scale))
    address = iv
    companion = _type_nw_companion(old, index, dreg, address, store, scale)
    if store:
        value = _ureg(old, dreg)
        wrote = _dm_write(state, address, 4, value, normal_word=True)
        _event(
            state,
            insn,
            "store",
            space=space,
            dreg="R%d" % dreg,
            value=value,
            address=address,
            expression=_render(address),
            concrete_write=wrote,
            addressing_mode="post-modify",
            access_width="normal-word",
        )
        if companion is not None:
            code, companion_address = companion
            if space == "DM":
                _companion_transfer(
                    state, insn, companion, True, "normal-word", old, "post-modify"
                )
            else:
                _event(
                    state,
                    insn,
                    "store-pey",
                    space="PM",
                    ureg=UREG_NAMES[code],
                    value=_ureg(old, code),
                    address=companion_address,
                    expression=_render(companion_address),
                    addressing_mode="post-modify",
                    access_width="normal-word",
                    concrete_write=False,
                )
    else:
        loaded = _load_normal_ureg(state, space, address, dreg)
        _event(
            state,
            insn,
            "load",
            space=space,
            dreg="R%d" % dreg,
            address=address,
            expression=_render(address),
            concrete_value=loaded,
            addressing_mode="post-modify",
            access_width="normal-word",
        )
        if companion is not None:
            code, companion_address = companion
            if space == "DM":
                _companion_transfer(
                    state, insn, companion, False, "normal-word", old, "post-modify"
                )
            else:
                _write_ureg(
                    state, code, Unknown("program-memory " + _render(companion_address))
                )
                _event(
                    state,
                    insn,
                    "load-pey",
                    space="PM",
                    ureg=UREG_NAMES[code],
                    address=companion_address,
                    expression=_render(companion_address),
                    concrete_value=None,
                    addressing_mode="post-modify",
                    access_width="normal-word",
                )
    state.uregs[16 + index] = _add(
        iv, scaled_mv, "I%d + M%d * %d" % (index, modifier, scale)
    )


def _type_1a(
    state: State, insn: Instruction, f: Mapping[str, int], name: str
) -> list[State]:
    """1a."""
    # SHARC+ Core Programming Reference (out/refs/sharc-plus-prm)
    # pp.13-3--13-6, Figure 13-1: an unconditional compute in parallel
    # with a DM transfer (fixed to DAG1, I0-7/M0-7) and a PM transfer
    # (fixed to DAG2, I8-15/M8-15). "The I values are post-modified and
    # updated by the specified M registers. Pre-modify offset
    # addressing is not supported" (p.13-4) -- unlike Type3a/3b/4a/4b,
    # there is no pre/post "u" bit; both accesses are always
    # post-modify. This tracer does not model the SIMD Y-element
    # (PEy/Sn) companion access the same page describes.
    old = _snapshot_uregs(state.uregs)
    try:
        compute, compute_y = _compute_simd(
            state, f, False, old, state.special, approx_recips=state.approx_recips
        )
    except ValueError as error:
        return [_stop(state, insn, str(error))]
    scale = _access_modifier_scale("normal-word", state.assume_nw32)

    dm_index, dm_modifier = _field(f, "dmi[2:0]"), _field(f, "dmm[2:0]")
    pm_index = ((_field(f, "pmi[2:2]") << 2) | _field(f, "pmi[1:0]")) + 8
    pm_modifier = _field(f, "pmm[2:0]") + 8
    _type_1a_access(
        "DM",
        dm_index,
        dm_modifier,
        _field(f, "dmdreg[3:0]"),
        bool(_field(f, "dmd")),
        state,
        insn,
        old,
        scale,
    )
    _type_1a_access(
        "PM",
        pm_index,
        pm_modifier,
        _field(f, "pmdreg[3:0]"),
        bool(_field(f, "pmd")),
        state,
        insn,
        old,
        scale,
    )
    if compute is not None:
        _apply_compute_simd(state, insn, compute, compute_y)
    return _advance(state, insn)


def _type_5a_swap(
    state: State, insn: Instruction, f: Mapping[str, int], name: str
) -> list[State]:
    """5a_swap."""
    # SHARC+ Core Programming Reference pp.13-37/13-38, Figure 13-14:
    # Dreg <-> CDreg (the PEx Rn register swaps with its PEy
    # complementary Sn register), with an optional condition and a
    # parallel compute. This tracer's register file models only PEx
    # (Rn); the PEy companion (Sn) is not tracked at all (see the
    # "18a" ASTATY handling above for the same limitation on flags).
    # Since Rn's new value after the swap is whatever untracked value
    # was in Sn, that new value is Unknown -- there is nothing to
    # concretely compute -- but the parallel compute and the
    # condition/predicate-fork machinery are still modeled exactly
    # like Type5a (move) above.
    cond = _field(f, "cond")
    old = _snapshot_uregs(state.uregs)
    try:
        compute, compute_y = _compute_simd(
            state, f, False, old, state.special, approx_recips=state.approx_recips
        )
    except ValueError as error:
        return [_stop(state, insn, str(error))]
    dreg = _field(f, "dreg")
    cdreg = _field(f, "cdreg")
    predicate = _predicate(state, cond)
    if predicate is False:
        _event(
            state,
            insn,
            "dreg-swap-skipped",
            dreg="R%d" % dreg,
            condition=cond,
            predicate_assumption=False,
        )
        return _advance(state, insn)
    executed = state if predicate is True else _copy(state)
    _write_ureg(
        executed, dreg, Unknown("Type5a swap from untracked complementary S%d" % cdreg)
    )
    if compute is not None:
        # PEy's compute only when unconditional (see _type_3a).
        _apply_compute_simd(
            executed, insn, compute, compute_y if cond == 0x1F else None
        )
    _event(
        executed,
        insn,
        "dreg-swap",
        dreg="R%d" % dreg,
        cdreg="S%d" % cdreg,
        condition=cond,
        predicate_assumption=True,
    )
    if predicate is True:
        return _advance(executed, insn)
    skipped = _copy(state)
    _event(
        skipped,
        insn,
        "dreg-swap-skipped",
        dreg="R%d" % dreg,
        condition=cond,
        predicate_assumption=False,
    )
    return _advance(executed, insn) + _advance(skipped, insn)


def _type_4d(
    state: State, insn: Instruction, f: Mapping[str, int], name: str
) -> list[State]:
    """4d."""
    # SHARC+ Core Programming Reference pp.13-32--13-35, Figure 13-12:
    # a 48-bit re-encoding of Type4a's index+6-bit-immediate transfer
    # that adds the byte/short access-width options Type4a itself does
    # not support ("does not support compute option", p.13-32 NOTE),
    # so unlike Type4a there is no compute field here. Same u/g/d/i
    # layout and index arithmetic as Type4a and Type4b; the l/w/x
    # width table is Type3b/4b's shared ACCESS_WIDTHS (l, x, w).
    width_fields = (_field(f, "l"), _field(f, "x"), _field(f, "w"))
    access_width = ACCESS_WIDTHS.get(width_fields)
    if access_width is None:
        return [_stop(state, insn, "unsupported Type4d access width")]
    store = bool(_field(f, "d"))
    if store and access_width.endswith("sign-extended"):
        return [_stop(state, insn, "unsupported Type4d sign-extended store")]
    if _field(f, "cond") != 0x1F:
        return [_stop(state, insn, "unsupported Type4d predicate")]
    width = _ACCESS_WIDTH_BYTES[access_width]
    bank = 8 if _field(f, "g") else 0
    index = _field(f, "i") + bank
    offset = _signed((_field(f, "data[5:5]") << 5) | _field(f, "data[4:0]"), 6)
    offset *= _access_modifier_scale(
        access_width, state.assume_nw32, _ureg(state.uregs, 16 + index)
    )
    post_modify = bool(_field(f, "u"))
    space = "PM" if bank else "DM"
    code = _field(f, "dreg")
    iv = _ureg(state.uregs, 16 + index)
    address = (
        iv if post_modify else _add(iv, Const(offset), "I%d + %d" % (index, offset))
    )
    # SIMD: PEy moves Cdreg at the adjacent byte/short word (PRM p.13-33,
    # "The load on PEy reads 16-bits from the 2-bytes addressed by I0+2";
    # p.7-5). FUN_1c2b24's 1024-short copy (sw 0x1c2c7c/0x1c2c7f) of
    # the per-voice flag masks runs in SIMD mode and needs it.
    companion = None
    if space == "DM":
        try:
            companion = _simd_ureg_mem_companion(
                state, code, address, access_width, store=bool(_field(f, "d"))
            )
        except ValueError as error:
            return [_stop(state, insn, str(error))]
    old = _snapshot_uregs(state.uregs)
    mode = "post-modify" if post_modify else "pre-modify"
    if store:
        value = _ureg(state.uregs, code)
        wrote = _dm_write(
            state,
            address,
            width,
            value,
            normal_word=access_width == "normal-word" or access_width == "long-word",
        )
        _event(
            state,
            insn,
            "store",
            space=space,
            dreg="R%d" % code,
            value=value,
            address=address,
            expression=_render(address),
            concrete_write=wrote,
            addressing_mode="post-modify" if post_modify else "pre-modify",
            access_width=access_width,
        )
        if companion is not None:
            _companion_transfer(state, insn, companion, True, access_width, old, mode)
    else:
        loaded = (
            _load_normal_ureg(state, space, address, code)
            if access_width == "normal-word"
            else _dm_read(
                state,
                address,
                width,
                access_width.endswith("sign-extended"),
                normal_word=access_width == "normal-word"
                or access_width == "long-word",
            )
        )
        if access_width != "normal-word":
            # The dict[str, int] PX1/PX2 summary only comes back from the
            # access_width == "normal-word" branch above (_load_normal_ureg's
            # combined-PX case), which this guard excludes.
            assert not isinstance(loaded, dict)
            _write_ureg(
                state, code, loaded or Unknown("memory-address " + _render(address))
            )
        _event(
            state,
            insn,
            "load",
            space=space,
            dreg="R%d" % code,
            address=address,
            expression=_render(address),
            concrete_value=loaded,
            addressing_mode="post-modify" if post_modify else "pre-modify",
            access_width=access_width,
        )
        if companion is not None:
            _companion_transfer(state, insn, companion, False, access_width, old, mode)
    if post_modify:
        state.uregs[16 + index] = _add(iv, Const(offset), "I%d + %d" % (index, offset))
    return _advance(state, insn)


def _type_3d_access(
    executed: State,
    insn: Instruction,
    index: int,
    modifier: int,
    scale: int,
    scaled_mv: Operand,
    address: Value,
    post_modify: bool,
    store: bool,
    space: str,
    ureg: int,
    width: int,
    access_width: str,
    companion: tuple[int, Value] | None,
    mode: str,
) -> None:
    """Type 3d's access on EXECUTED (the state itself, or the executed fork)."""
    old = _snapshot_uregs(executed.uregs)
    iv = _ureg(old, 16 + index)
    if store:
        value = _ureg(old, ureg)
        wrote = _dm_write(
            executed,
            address,
            width,
            value,
            normal_word=access_width == "normal-word" or access_width == "long-word",
        )
        _event(
            executed,
            insn,
            "store",
            space=space,
            ureg=UREG_NAMES[ureg],
            value=value,
            address=address,
            expression=_render(address),
            concrete_write=wrote,
            addressing_mode="post-modify" if post_modify else "pre-modify",
            access_width=access_width,
        )
        if companion is not None:
            _companion_transfer(
                executed, insn, companion, True, access_width, old, mode
            )
    else:
        loaded = (
            _load_normal_ureg(executed, space, address, ureg)
            if access_width == "normal-word"
            else _dm_read(
                executed,
                address,
                width,
                access_width.endswith("sign-extended"),
                normal_word=access_width == "normal-word"
                or access_width == "long-word",
            )
        )
        if access_width != "normal-word":
            # The dict[str, int] PX1/PX2 summary only comes back from
            # the access_width == "normal-word" branch above (see
            # Type4d's identical guard); this branch never sees it.
            assert not isinstance(loaded, dict)
            _write_ureg(
                executed, ureg, loaded or Unknown("memory-address " + _render(address))
            )
        _event(
            executed,
            insn,
            "load",
            space=space,
            ureg=UREG_NAMES[ureg],
            address=address,
            expression=_render(address),
            concrete_value=loaded,
            addressing_mode="post-modify" if post_modify else "pre-modify",
            access_width=access_width,
        )
        if companion is not None:
            _companion_transfer(
                executed, insn, companion, False, access_width, old, mode
            )
    if post_modify:
        executed.uregs[16 + index] = _add(
            iv, scaled_mv, "I%d + M%d * %d" % (index, modifier, scale)
        )


def _type_3d(
    state: State, insn: Instruction, f: Mapping[str, int], name: str
) -> list[State]:
    """3d."""
    # SHARC+ Core Programming Reference pp.13-22--13-25, Figure 13-9: a
    # 48-bit re-encoding of Type3a's index+M-register transfer that
    # adds byte/short and exclusive-access options Type3a does not
    # support ("extension to 3a instruction (exclusive access without
    # compute option)", p.13-22 NOTE), so there is no compute field.
    # w selects the ACCESS (0) vs WACCESS (1) group; ex marks an
    # exclusive-access monitor this tracer does not model, matching
    # the existing "14d" handler's ex=1 stop below -- left unsupported
    # rather than guessed.
    #
    # docs/findings/06 ("Wrap copies +0x1bc"): the w=0 ACCESS group's own
    # encode table (p.324, "ACCESS (Type 3d)") names every u/g/d row's
    # modifier from the BH ("... BH (Type 3d)", a store) or BHSE
    # ("... BHSE (Type 3d)", a load) sub-table that immediately follows
    # it -- both indexed by ex, l and (BHSE only) x -- not from a
    # separate "no modifier when ex=0" case. Those sub-tables' own (l, x)
    # columns are exactly Type3b/Type4d's byte/short(-sign-extended)
    # ACCESS_WIDTHS (l, x, w=0) slice (PRM pp.13-16--13-19/13-31--13-35).
    # This handler previously hardcoded w=0 (any ex) as normal-word,
    # l/x unused ("the plain 48-bit re-encoding of Type3a"): a concrete
    # run of dt2-1.16's voice render refuted that (sw 0x1c50fb/0x1c5322,
    # decoded fields l=0, x=0, w=0, ex=0 -- confirmed via tools/sharc.py's
    # insn table). With the hardcoded normal-word read, a looping voice's
    # ACTIVE flag was cleared on every wrap instead of tracking
    # FIELD_LOOP the way docs/findings/06's "Flags and seed" documents;
    # overriding the load to the ACCESS_WIDTHS-correct 1-byte value
    # (matching the sibling Type4d byte store at the very same site)
    # reproduced the documented wrap behaviour. So w=0 uses
    # ACCESS_WIDTHS[(l, x, 0)] regardless of ex -- only WACCESS (w=1)
    # and the exclusive-monitor semantics of ex=1 remain unsupported.
    if _field(f, "w"):
        return [_stop(state, insn, "unsupported Type3d WACCESS")]
    if _field(f, "ex"):
        return [_stop(state, insn, "unsupported Type3d exclusive access")]
    access_width = ACCESS_WIDTHS.get((_field(f, "l"), _field(f, "x"), _field(f, "w")))
    if access_width is None:
        return [_stop(state, insn, "unsupported Type3d access width")]
    store = bool(_field(f, "d"))
    if store and access_width.endswith("sign-extended"):
        return [_stop(state, insn, "unsupported Type3d sign-extended store")]
    width = _ACCESS_WIDTH_BYTES[access_width]
    bank = 8 if _field(f, "g") else 0
    index, modifier = _field(f, "i") + bank, _field(f, "m") + bank
    post_modify = bool(_field(f, "u"))
    space = "PM" if bank else "DM"
    ureg = _field(f, "ureg")
    cond = _field(f, "cond")
    # SIMD: PRM p.13-20, "Type 3d ... SIMD Mode": the Y element moves the
    # Cureg at the explicit address + one/two (normal/short word) -- in byte
    # space the adjacent byte/short word (p.7-5). FUN_1c642a's end-of-frame
    # loop (sw 0x1c7191-0x1c71a6, 16 passes, I4 += 2 bytes) copies the
    # per-voice trig-pending bytes to the latched bytes this way, R2 for
    # the even voice and S2 for the odd one; without the companion the odd
    # (R-channel) voice of a track was never armed.
    iv0, mv0 = _ureg(state.uregs, 16 + index), _ureg(state.uregs, 32 + modifier)
    scale = _access_modifier_scale(access_width, state.assume_nw32, iv0)
    scaled_mv = _multiply(mv0, Const(scale), "M%d * %d" % (modifier, scale))
    address = (
        iv0
        if post_modify
        else _add(iv0, scaled_mv, "I%d + M%d * %d" % (index, modifier, scale))
    )
    companion = None
    if space == "DM":
        try:
            companion = _simd_ureg_mem_companion(
                state, ureg, address, access_width, store=bool(_field(f, "d"))
            )
        except ValueError as error:
            return [_stop(state, insn, str(error))]
    mode = "post-modify" if post_modify else "pre-modify"

    predicate = _predicate(state, cond)
    if predicate is True:
        _type_3d_access(
            state,
            insn,
            index,
            modifier,
            scale,
            scaled_mv,
            address,
            post_modify,
            store,
            space,
            ureg,
            width,
            access_width,
            companion,
            mode,
        )
        return _advance(state, insn)
    if predicate is False:
        _event(
            state,
            insn,
            "type3d-skipped",
            condition=cond,
            predicate_assumption=False,
        )
        return _advance(state, insn)
    executed, skipped = _copy(state), _copy(state)
    _type_3d_access(
        executed,
        insn,
        index,
        modifier,
        scale,
        scaled_mv,
        address,
        post_modify,
        store,
        space,
        ureg,
        width,
        access_width,
        companion,
        mode,
    )
    _event(skipped, insn, "type3d-skipped", condition=cond, predicate_assumption=False)
    return _advance(executed, insn) + _advance(skipped, insn)


FORMS = {
    "17a": _type_17a,
    "17b": _type_17a,
    "3a": _type_3a,
    "14a": _type_14a,
    "14d": _type_14d,
    "5a_move": _type_5a_move,
    "5b_move": _type_5a_move,
    "4a": _type_4a,
    "4b": _type_4b,
    "3b": _type_3b,
    "3c": _type_3c,
    "16a": _type_16a,
    "16b": _type_16a,
    "15b": _type_15b,
    "15a": _type_15a,
    "1a": _type_1a,
    "5a_swap": _type_5a_swap,
    "4d": _type_4d,
    "3d": _type_3d,
}
