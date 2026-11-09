"""DAG forms: index register modify and I-register moves.

Each handler executes one decoded instruction and returns the successor
states. FORMS maps form names to handlers; sharc_core.forms merges the
family tables.
"""

from __future__ import annotations

from collections.abc import Mapping

from tools.sharc_dev.sharc_disasm import Instruction

from .compute import (
    _apply_compute_simd,
    _compute_simd,
)
from .encoding import (
    UREG_CODES,
    _field,
    _wide,
)
from .memory import (
    _circular_wrap_const,
    _modify_scale,
)
from .sequencer import (
    _advance,
    _predicate,
    _predicate_pe,
)
from .state import (
    State,
    _copy,
    _event,
    _json_value,
    _simd_active,
    _snapshot_uregs,
    _stop,
    _ureg,
)
from .values import (
    CIRC_SYMBOL_PREFIX,
    Affine,
    Const,
    Unknown,
    _aconv,
    _add,
    _is_unknown,
    _multiply,
    _signed,
    _stack_bounded_symbol,
)


def _type_7a(
    state: State, insn: Instruction, f: Mapping[str, int], name: str
) -> list[State]:
    """7a."""
    # Type 7a is MODIFY: the manual guarantees an index-register update in
    # parallel with its optional compute.  The table now carries the M
    # register selector at bits 29-27, the same field Type7b uses.
    cond = _field(f, "cond")
    bank = 8 if _field(f, "g") else 0
    source_low = _field(f, "is[2:2]") << 2 | _field(f, "is[1:0]")
    destination_low = source_low ^ _field(f, "idis")
    source, destination = source_low + bank, destination_low + bank
    modifier = _field(f, "m") + bank
    # PRM p.348's "BH (Type 7a)" encode table (bits 39/23, labels "w"/"l"
    # here): (w, l) = (0, 0) blank, (0, 1) "(sw)", (1, 0) "(nw)" -- MODIFY's
    # own short-word/normal-word address-scale selector, previously
    # undecoded (decode_table.json had no field at either bit) so this form
    # always scaled M as normal-word. dt2-1.16's voice-render inner loop
    # (0x1c50af/0x1c50ca/0x1c50cf/0x1c50d4, all indexing a 16-bit PCM
    # sample buffer through I4) decodes w=0, l=1 -- "(sw)" -- at every one
    # of its MODIFY-with-compute and bare MODIFY instructions. (1, 1) is
    # not in the manual's table; treat it like l alone (short-word) since
    # that is the only bit this tracer has evidence for. The blank row is
    # the plain MODIFY, which never scales (_modify_scale).
    if f.get("l"):
        access_width: str | None = "short-word"
    elif f.get("w"):
        access_width = "normal-word"
    else:
        access_width = None
    conditional = cond != 0x1F
    circular_wrap: Const | None = None
    run_x: bool | None = True
    run_y: bool | None = _simd_active(state) is True
    if conditional:
        # PRM p.14-47: "If a condition is specified, it affects the entire
        # instruction", compute and index modify alike.  SISD tests PEx's
        # condition.  SIMD tests it in each PE for the compute; the modify
        # happens when either PE's test is true (logical OR).
        simd = _simd_active(state)
        modify_runs: bool | None
        if simd is True:
            run_x = _predicate_pe(state, cond, "x")
            run_y = _predicate_pe(state, cond, "y")
            if run_x is True or run_y is True:
                modify_runs = True
            elif run_x is False and run_y is False:
                modify_runs = False
            else:
                modify_runs = None
        elif simd is False:
            run_x = _predicate(state, cond)
            run_y = False
            modify_runs = run_x
        else:
            # SIMD unresolved: PEx true makes the modify happen either way
            # (OR of both PEs); PEy's compute outcome is then unknown.
            run_x = _predicate(state, cond)
            run_y = False
            modify_runs = True if run_x else None
            if run_x and (_field(f, "compute[22:16]") or _field(f, "compute[15:0]")):
                return [
                    _stop(state, insn, "unresolved SIMD conditional Type7a compute")
                ]
        if modify_runs is False:
            _event(
                state,
                insn,
                "i-modify-skipped",
                source="I%d" % source,
                destination="I%d" % destination,
                predicate="false in every active PE",
            )
            return _advance(state, insn)
        if modify_runs is None:
            state.uregs[16 + destination] = Unknown("conditional Type7a modify outcome")
            _event(
                state,
                insn,
                "i-modify-uncertain",
                source="I%d" % source,
                destination="I%d" % destination,
                predicate="unresolved",
                scale_assumption="assume_nw32"
                if state.assume_nw32
                else "unscaled normal-word",
            )
            return _advance(state, insn)
        length = _ureg(state.uregs, UREG_CODES["L%d" % source])
        if isinstance(length, Const) and length.value != 0:
            # The MODIFY instruction wraps whenever L is nonzero,
            # independent of MODE1.CBUFEN (out/refs/sharc-plus-prm
            # lines 9344-9346, 10410-10412): unlike an ordinary
            # load/store post-modify, this is not gated by CBUFEN.
            base = _ureg(state.uregs, UREG_CODES["B%d" % source])
            index_now = _ureg(state.uregs, 16 + source)
            modifier_now = _ureg(state.uregs, 32 + modifier)
            scale_now = _modify_scale(access_width, state.assume_nw32, index_now)
            if (
                isinstance(base, Const)
                and isinstance(index_now, Const)
                and isinstance(modifier_now, Const)
            ):
                wrapped_value = _circular_wrap_const(
                    index_now.value,
                    base.value,
                    length.value,
                    modifier_now.value * scale_now,
                )
                if wrapped_value is None:
                    return [
                        _stop(
                            state,
                            insn,
                            "circular modifier is not smaller than L%d" % source,
                        )
                    ]
                circular_wrap = Const(wrapped_value)
            else:
                return [_stop(state, insn, "unsupported Type7a circular modify")]
        elif not isinstance(length, Const):
            return [_stop(state, insn, "unsupported Type7a circular modify")]
    # SHARC+ PRM, SIMD mode: the compute of a Type 7a executes in both
    # processing elements (the index modify is shared).
    try:
        compute, compute_y = _compute_simd(
            state,
            f,
            False,
            _snapshot_uregs(state.uregs),
            state.special,
        )
    except ValueError as error:
        return [_stop(state, insn, str(error))]
    if circular_wrap is not None:
        state.uregs[16 + destination] = circular_wrap
        _event(
            state,
            insn,
            "i-modify",
            source="I%d" % source,
            destination="I%d" % destination,
            modifier="M%d" % modifier,
            circular=True,
        )
    else:
        index_value = _ureg(state.uregs, 16 + source)
        modifier_value = _ureg(state.uregs, 32 + modifier)
        scale = _modify_scale(access_width, state.assume_nw32, index_value)
        scaled_modifier = _multiply(
            modifier_value, Const(scale), "M%d * %d" % (modifier, scale)
        )
        state.uregs[16 + destination] = _add(
            index_value,
            scaled_modifier,
            "I%d + M%d * %d" % (source, modifier, scale),
        )
        _event(
            state,
            insn,
            "i-modify",
            source="I%d" % source,
            destination="I%d" % destination,
            modifier="M%d" % modifier,
            **(
                {"scale_assumption": "assume_nw32"}
                if conditional and state.assume_nw32
                else {}
            ),
        )
    if not run_x:
        compute = None
    if not run_y:
        compute_y = None
    _apply_compute_simd(state, insn, compute, compute_y)
    return _advance(state, insn)


def _type_7b_modify(
    executed: State, insn: Instruction, source: int, destination: int, modifier: int
) -> None:
    """Type 7b's circular or plain MODIFY on EXECUTED."""
    length = _ureg(executed.uregs, UREG_CODES["L%d" % source])
    index_value = _ureg(executed.uregs, 16 + source)
    modifier_value = _ureg(executed.uregs, 32 + modifier)
    scale = _modify_scale(None, executed.assume_nw32)
    if (
        isinstance(length, Const)
        and length.value != 0
        and isinstance(index_value, Const)
        and isinstance(modifier_value, Const)
    ):
        base = _ureg(executed.uregs, UREG_CODES["B%d" % source])
        if isinstance(base, Const):
            wrapped = _circular_wrap_const(
                index_value.value,
                base.value,
                length.value,
                modifier_value.value * scale,
            )
            executed.uregs[16 + destination] = (
                Const(wrapped)
                if wrapped is not None
                else Unknown("circular modifier is not smaller than L%d" % source)
            )
            _event(
                executed,
                insn,
                "i-modify",
                source="I%d" % source,
                destination="I%d" % destination,
                modifier="M%d" % modifier,
                circular=True,
            )
            return
    scaled_modifier = _multiply(
        modifier_value, Const(scale), "M%d * %d" % (modifier, scale)
    )
    executed.uregs[16 + destination] = _add(
        index_value,
        scaled_modifier,
        "I%d + M%d * %d" % (source, modifier, scale),
    )
    _event(
        executed,
        insn,
        "i-modify",
        source="I%d" % source,
        destination="I%d" % destination,
        modifier="M%d" % modifier,
    )


def _type_7b(
    state: State, insn: Instruction, f: Mapping[str, int], name: str
) -> list[State]:
    """7b."""
    # Type 7b VISA MODIFY (out/refs/sharc-plus-prm pp.13-49/13-50): the
    # same index-register update as Type7a's MODIFY, minus the parallel
    # compute option, over the full condition-code range (not just
    # Type7a's 0x1F/0x17). "If the DAG's Lx and Bx registers that
    # correspond to Ia or Ic are set up for circular buffering, the
    # modify operation always executes circular buffer wraparound,
    # independent of the state of the CBUFEN bit" (same page), so --
    # unlike an ordinary load/store post-modify -- this form is not
    # gated by MODE1.CBUFEN. Its MODIFY Encode Table (p.13-50) has no
    # (sw)/(nw) bits, so it is the plain MODIFY and never scales M
    # (_modify_scale).
    cond = _field(f, "cond")
    bank = 8 if _field(f, "g") else 0
    source_low = _field(f, "is[2:2]") << 2 | _field(f, "is[1:0]")
    destination_low = source_low ^ _field(f, "idis")
    source, destination = source_low + bank, destination_low + bank
    modifier = _field(f, "m") + bank

    predicate = _predicate(state, cond)
    if predicate is True:
        _type_7b_modify(state, insn, source, destination, modifier)
        return _advance(state, insn)
    if predicate is False:
        _event(
            state,
            insn,
            "i-modify-skipped",
            source="I%d" % source,
            destination="I%d" % destination,
            condition=cond,
        )
        return _advance(state, insn)
    executed, skipped = _copy(state), _copy(state)
    _type_7b_modify(executed, insn, source, destination, modifier)
    _event(
        skipped,
        insn,
        "i-modify-skipped",
        source="I%d" % source,
        destination="I%d" % destination,
        condition=cond,
    )
    return _advance(executed, insn) + _advance(skipped, insn)


def _type_7d(
    state: State, insn: Instruction, f: Mapping[str, int], name: str
) -> list[State]:
    """7d."""
    # SHARC+ Core Programming Reference (out/refs/sharc-plus-prm), ACONV
    # (Type 7d), Figure 13-21 p.352 and its Encode Table (same page):
    # this handler is deliberately only the pure ACONV row: cond=11111
    # and an empty compute (Table 13-22, p.350). decode_table.json pins
    # those fields into the mask/value, so cond/compute are not free here.
    # The PRM also describes conditional/compute-parallel Type7d rows;
    # they are outside this bounded decoder contract and must not silently
    # enter this handler as pure ACONV. g selects
    # DAG1/DAG2 (add 8, as for Type7a/Type19a); breg selects the I or B
    # register class; toby selects W2B (1) vs B2W (0); the destination
    # register is the source XOR idis, the same trick as Type7a/Type19a.
    #
    # Table 6-4 "Switch Address Instruction Semantics" (same PRM p.200,
    # printed 6-16; identical table in out/refs/sc58x-2158x-prm) hedges
    # the shift:
    #   "Id = B2W(Is) ... Base addr in byte-addressed space: Convert
    #   byte pointer to word pointer. Likely semantics Id <- Is >> 2.
    #   Exact semantics depend on address map and must work correctly
    #   for all addresses in both internal and external memory. In case
    #   of byte addresses not having word space equivalent Is will be
    #   retained as is i.e. Id = Is and illegal address space (ILAD)
    #   interrupt is generated."
    #   "Id = W2B(Is) ... Likely semantics Id <- Is << 2 ... [same ILAD
    #   hedge]." (Bd/Bs rows mirror Id/Is.)
    # This decoder does not model the address map or the ILAD trap, so
    # it only applies the documented "likely" shift, tags the event
    # semantics="prm-likely", and stops when the source is not concrete
    # rather than guess whether the trap fires.
    bank = 8 if _field(f, "g") else 0
    source_low = _field(f, "is[2:2]") << 2 | _field(f, "is[1:0]")
    destination_low = source_low ^ _field(f, "idis")
    source, destination = source_low + bank, destination_low + bank
    breg = bool(_field(f, "breg"))
    reg_class = "B" if breg else "I"
    base_code = UREG_CODES["B0"] if breg else UREG_CODES["I0"]
    src_code, dst_code = base_code + source, base_code + destination
    value = _ureg(state.uregs, src_code)
    w2b = bool(_field(f, "toby"))
    direction = "w2b" if w2b else "b2w"
    if _is_unknown(value):
        return [
            _stop(
                state,
                insn,
                "Type7d %s(%s%d) source is not concrete"
                % (direction.upper(), reg_class, source),
            )
        ]
    result = _aconv(value, w2b, src_code, state.pc_sw)
    state.uregs[dst_code] = result
    _event(
        state,
        insn,
        "aconv",
        direction=direction,
        source="%s%d" % (reg_class, source),
        destination="%s%d" % (reg_class, destination),
        value=_json_value(result),
        semantics="prm-likely",
    )
    return _advance(state, insn)


def _type_19a(
    state: State, insn: Instruction, f: Mapping[str, int], name: str
) -> list[State]:
    """19a, 19a_scaled."""
    bank = 8 if _field(f, "g") else 0
    src_low = _field(f, "is")
    # PGR Type 19 encodes the destination as Id XOR Is, not as a direct
    # register number (Table 17-2 and Figure 17-2).
    dst_low = src_low ^ _field(f, "idis")
    src, dst = src_low + bank, dst_low + bank
    v = _ureg(state.uregs, 16 + src)
    delta = _signed(_wide(f, "data"), 32)
    scale = 1
    scaled_width = None
    if name == "19a_scaled":
        scaled_width = "normal-word" if _field(f, "w") else "short-word"
        # The opt-in normal-word model represents the loaded program's
        # internal pointers in byte space. Enhanced MODIFY scales NW/SW
        # immediates by four/two bytes there, and not at all when the
        # source already holds a normal-word address (PRM p.6-9, Table
        # 6-2; `_modify_scale`, as for Type7a (nw)/(sw) MODIFY). Example:
        # a C stack pointer I7 = 0x903fe with modify(i7,-4) (nw) gives
        # 0x903fa. The circular length below takes the same scale, so a
        # normal-word buffer wraps in words.
        if state.assume_nw32:
            scale = _modify_scale(scaled_width, state.assume_nw32, v)
            delta *= scale

    result = _add(v, Const(delta), "I%d + %d" % (src, delta))
    circular = False
    wrapped = False
    if name == "19a_scaled":
        base = _ureg(state.uregs, UREG_CODES["B%d" % src])
        length = _ureg(state.uregs, UREG_CODES["L%d" % src])
        if isinstance(length, Const) and length.value == 0:
            pass
        elif (
            isinstance(v, Const)
            and isinstance(base, Const)
            and isinstance(length, Const)
        ):
            circular = True
            byte_length = length.value * scale
            # PRM Sec. 6 "Circular Buffering": the modifier's magnitude
            # may be up to and including the buffer length L -- a step
            # of exactly L is a full lap and lands back on the start
            # value. Only a magnitude greater than L is out of the
            # single-correction case this tracer models.
            if byte_length < abs(delta):
                result = Unknown("circular modifier is not smaller than L%d" % src)
            else:
                candidate = _circular_wrap_const(
                    v.value, base.value, byte_length, delta
                )
                # byte_length >= abs(delta) and byte_length > 0 (the
                # length==0 case already returned above), exactly
                # _circular_wrap_const's own precondition for a non-None
                # result -- but state that explicitly, matching how the
                # other two call sites in this file (Type7a/7b MODIFY)
                # handle its documented None case, rather than assuming it.
                if candidate is None:
                    result = Unknown("circular modifier is not smaller than L%d" % src)
                else:
                    wrapped = candidate != (v.value + delta) & 0xFFFFFFFF
                    result = Const(candidate)
        else:
            bounded = _stack_bounded_symbol(v)
            known_byte_length = (
                length.value * scale if isinstance(length, Const) else None
            )
            if (
                bounded is not None
                and known_byte_length is not None
                and abs(bounded[1]) < known_byte_length
            ):
                # v has no proof yet of its own concrete value, but it
                # is a symbol some caller has already bounded (an
                # entry-time seed, or an earlier circular-MODIFY-
                # derived symbol -- recursively, ultimately grounded in
                # an entry-time seed), offset by less than one buffer
                # length. PRM p.6-23: "If the index pointer falls
                # outside the buffer, the DAG subtracts or adds the
                # buffer length to the index value, wrapping the index
                # pointer back within the start and end boundaries of
                # the buffer" -- one +-byte_length correction. So the
                # true (concrete) result is v + delta, corrected by at
                # most one +-byte_length: within one buffer length of
                # wherever v's own bound places it -- never a claim
                # that the result equals v, another modify site's
                # result, or the same site's own value on a different
                # visit. A FRESH symbol (never v's own name) is minted
                # so two circular-MODIFY results are never treated as
                # equal or made to cancel by the Affine algebra; this
                # module does not itself know or state the numeric
                # bound -- that is the caller's fact (tools/
                # sharcwriters.py's STACK_SYMBOLS / CIRC_WRAP_SLACK).
                fresh = "%s%d_%x" % (CIRC_SYMBOL_PREFIX, src, state.pc_sw)
                result = Affine(constant=0, terms=((fresh, 1),))
                circular = True
            else:
                result = Unknown("scaled circular modify I%d" % src)
    state.uregs[16 + dst] = result
    _event(
        state,
        insn,
        "i-add",
        source="I%d" % src,
        destination="I%d" % dst,
        offset=delta,
        scaled_width=scaled_width,
        circular=circular,
        wrapped=wrapped,
    )
    return _advance(state, insn)


def _type_19a_bitrev(
    state: State, insn: Instruction, f: Mapping[str, int], name: str
) -> list[State]:
    """19a_bitrev: BITREV(Ia, data32) and Ib = BITREV(Ia, data32).

    SHARC+ Core Programming Reference, "Bit-Reverse Instruction" and
    "Enhanced Bit-Reverse Instruction" (pp.6-9): the immediate is added
    to the source I register, the 32-bit sum is bit-reversed and written
    to the destination (Id XOR Is, as for 19a), and no address is output.
    The PRM's own worked example for bit-reverse mode reverses all 32 bits
    (0x83000 <-> 0xC1000). Like 19a here, a B/L-configured circular
    wraparound is not modelled."""
    bank = 8 if _field(f, "g") else 0
    src_low = _field(f, "is")
    dst_low = src_low ^ _field(f, "idis")
    src, dst = src_low + bank, dst_low + bank
    v = _ureg(state.uregs, 16 + src)
    delta = _wide(f, "data") & 0xFFFFFFFF
    result: Const | Unknown
    if isinstance(v, Const):
        total = (v.value + delta) & 0xFFFFFFFF
        flipped = 0
        for k in range(32):
            flipped |= ((total >> k) & 1) << (31 - k)
        result = Const(flipped)
    else:
        result = Unknown("bitrev(I%d + %d)" % (src, delta))
    state.uregs[16 + dst] = result
    _event(
        state,
        insn,
        "i-bitrev",
        source="I%d" % src,
        destination="I%d" % dst,
        offset=delta,
    )
    return _advance(state, insn)


FORMS = {
    "7a": _type_7a,
    "7b": _type_7b,
    "7d": _type_7d,
    "19a": _type_19a,
    "19a_bitrev": _type_19a_bitrev,
    "19a_scaled": _type_19a,
}
