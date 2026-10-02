"""Per-form instruction execution: _execute dispatches on the decoded form
through FORMS, merged from the family modules (forms_compute, forms_move,
forms_dag, forms_flow, forms_system).
"""

from __future__ import annotations

from collections.abc import Callable, Mapping

from sharc_disasm import Instruction

from .forms_compute import FORMS as _FORMS_COMPUTE
from .forms_dag import FORMS as _FORMS_DAG
from .forms_flow import FORMS as _FORMS_FLOW
from .forms_move import FORMS as _FORMS_MOVE
from .forms_system import FORMS as _FORMS_SYSTEM
from .state import (
    State,
    _note_provisional,
    _stop,
)


def _build_forms(
    tables: tuple[Mapping[str, Callable[..., list[State]]], ...],
) -> dict[str, Callable[..., list[State]]]:
    merged: dict[str, Callable[..., list[State]]] = {}
    for table in tables:
        for form, handler in table.items():
            if form in merged:
                raise ValueError("form %s has two handlers" % form)
            merged[form] = handler
    return merged


# Form name -> handler(state, insn, fields, name).
FORMS = _build_forms(
    (_FORMS_COMPUTE, _FORMS_MOVE, _FORMS_DAG, _FORMS_FLOW, _FORMS_SYSTEM)
)


def _execute(state: State, insn: Instruction) -> list[State]:
    if insn.kind != "confident" or insn.length_bytes is None:
        if insn.length_bytes is None or insn.type_name not in state.provisional_forms:
            return [_stop(state, insn, "uncertain or undecodable form: " + insn.note)]
        _note_provisional(state, insn.type_name)
    state.at_loaded_entry = False
    f, name = insn.fields, insn.type_name
    # sharc_disasm.disassemble() only ever sets type_name to a decode-table
    # entry's name or the literal string "unknown" -- never None -- despite
    # the field's wider declared type (Instruction's own docstring: "the
    # TYPES name ..., or 'unknown'").
    assert name is not None, "instruction with no type_name"
    handler = FORMS.get(name)
    if handler is not None:
        try:
            return handler(state, insn, f, name)
        except ValueError as error:
            return [_stop(state, insn, str(error))]
    return [_stop(state, insn, "unsupported form " + str(name))]
