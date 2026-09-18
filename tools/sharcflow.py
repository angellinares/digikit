"""Find SHARC+ calls and returns, and cover the DSP program in Ghidra.

    uv run python tools/sharcflow.py REGION.bin [--base-sw 0x1c1338]
        [--min-depth N] [--json OUT]
        [--program /dt2-1.16_SHARC [--project DIR --project-name NAME]
         [--cover] [--analyze] [--save]]

A call is CJUMP (`25a_direct`, or `25a_pcrel`), which is always delayed:
the two instructions after it execute before the target, and the return
address is the instruction after them. The compiler puts a push of R2
through I7/M7 (`3c` raw 0x9ff2, or a 48-bit `3a`) and a `16a` that stores its
own short-word address + 2 (the return address - 1) in those two slots
(SHARC+ Core Programming Reference, Type 25a; SHARC Processor Programming
Reference Rev 2.4, "Compiler Related Stalls"). An indirect call moves I6 to
R2 and I7 to I6 itself and jumps through `9b_abs` with M5 (DB), followed by
the same push and store. A return is the delayed `9b_abs` jump through I4/M6
(raw 0x083f343f); its two delay slots hold `25c_rframe` and an epilogue
instruction, in either order.

Without --program the tool only reports, from REGION.bin (raw code, 16-bit
little-endian words; --base-sw is the short-word address of its first word),
over the aligned instructions (on the linear sweep of tools/sharcimm.py, with
a decoded run of at least --min-depth instructions):

- calls: each CJUMP with its target, the forms in its two delay slots,
  whether they are the push and store, and the return address;
- indirect_calls: each `9b_abs` jump through M5 (DB) whose delay slots are
  the push and store;
- returns: each return jump, its delay-slot forms, and the address after them.

With --program it opens that program and, per call site, disassembles the
call if needed, sets FlowOverride.CALL where the language still models it as
a jump (and on the indirect calls), disassembles the fall-through and the
target, and creates a function at the target. --cover then disassembles, one
instruction at a time, every aligned instruction Ghidra has not reached,
counts those that clash with an existing instruction or data, starts a
function after each return's delay slots, and then at each run of
main-program code that no function holds. It recomputes every function body,
optionally runs auto-analysis, and prints functions, instructions and call
references before and after. Nothing is saved without --save.

Ghidra gets no delay-slot semantics from this: the push and store after a
call appear after it in the listing.
"""

from __future__ import annotations

import argparse
import collections
import json
import os
import sys

_here = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, _here)

import sharcimm  # noqa: E402

DEFAULT_GHIDRA = '/opt/homebrew/Cellar/ghidra/12.1.3/libexec'
PUSH_R2 = 0x9FF2
RETURN_JUMP = 0x083F343F


def _field(fields, stem):
    """The field whose label is STEM or starts with STEM[."""
    for label, value in fields.items():
        if label == stem or label.startswith(stem + '['):
            return value
    return None


def _value(fields, stem):
    """The value split across fields STEM[31:16] or STEM[23:16], and STEM[15:0]."""
    high = fields.get(stem + '[31:16]', fields.get(stem + '[23:16]'))
    return (high << 16) | fields[stem + '[15:0]']


def is_push(insn) -> bool:
    """DM(I7, M7) = R2, as `3c` or as the 48-bit `3a`."""
    if insn.type_name == '3c':
        return insn.raw == PUSH_R2
    if insn.type_name == '3a':
        f = insn.fields
        return (_field(f, 'u'), _field(f, 'i'), _field(f, 'm'), _field(f, 'g'),
                _field(f, 'd'), _field(f, 'ureg')) == (1, 7, 7, 0, 1, 2)
    return False


def is_store(insn, sw) -> bool:
    """DM(I7, M7) = return address - 1, a `16a` storing its own address + 2."""
    return (insn.type_name == '16a' and _field(insn.fields, 'i') == 7
            and _field(insn.fields, 'm') == 7 and _value(insn.fields, 'data') == sw + 2)


def aligned(data: bytes, min_depth: int = 8):
    """Aligned instructions in address order: [(offset, Instruction)]."""
    table = sharcimm.decode_all(data)
    depth = sharcimm.depths(table, len(data))
    sweep = sharcimm.sweep_offsets(table, len(data))
    return [(off, table[off]) for off in sorted(sweep) if depth[off] >= min_depth]


def find_sites(data: bytes, base_sw: int, min_depth: int = 8) -> dict:
    """calls, indirect_calls and returns, plus 'aligned': [(sw, length in bytes)]
    of every aligned instruction."""
    insns = aligned(data, min_depth)

    def slots(n):
        """The two instructions after insns[n] if they follow it without a gap."""
        out = []
        for k in (1, 2):
            if n + k >= len(insns):
                return None
            prev_off, prev = insns[n + k - 1]
            if prev_off + prev.length_bytes != insns[n + k][0]:
                return None
            out.append(insns[n + k])
        return out

    def after(pair):
        off, insn = pair[1]
        return base_sw + (off + insn.length_bytes) // 2

    calls, indirect_calls, returns = [], [], []
    for n, (off, insn) in enumerate(insns):
        sw = base_sw + off // 2
        pair = slots(n)
        forms = [p[1].type_name for p in pair] if pair else None
        linked = bool(pair) and is_push(pair[0][1]) and \
            is_store(pair[1][1], base_sw + pair[1][0] // 2)
        if insn.type_name in ('25a_direct', '25a_pcrel'):
            if insn.type_name == '25a_direct':
                target = _value(insn.fields, 'addr')
            else:
                rel = _value(insn.fields, 'reladdr')
                target = sw + (rel - (1 << 24) if rel & (1 << 23) else rel)
            calls.append({'sw': sw, 'target': target, 'slots': forms, 'linked': linked,
                          'returns_to': after(pair) if pair else None})
        elif insn.type_name == '9b_abs':
            f = insn.fields
            if insn.raw == RETURN_JUMP:
                returns.append({'sw': sw, 'slots': forms,
                                'after': after(pair) if pair else None})
            elif linked and (_field(f, 'b'), _field(f, 'j'), _field(f, 'pmm')) == (0, 1, 5):
                indirect_calls.append({'sw': sw, 'slots': forms, 'returns_to': after(pair)})
    return {'calls': calls, 'indirect_calls': indirect_calls, 'returns': returns,
            'aligned': [(base_sw + off // 2, insn.length_bytes) for off, insn in insns]}


def summary(sites: dict) -> list:
    calls, returns = sites['calls'], sites['returns']

    def hist(items, key):
        return ', '.join(f'{k}:{v}' for k, v in collections.Counter(key(i) for i in items).most_common(8))

    return [
        f"calls {len(calls)} (push and store in the delay slots {sum(c['linked'] for c in calls)}), "
        f"distinct targets {len({c['target'] for c in calls})}",
        "call delay slots: " + hist(calls, lambda c: '+'.join(c['slots'] or ['?'])),
        f"indirect calls {len(sites['indirect_calls'])}, returns {len(returns)}",
        "return delay slots: " + hist(returns, lambda r: '+'.join(r['slots'] or ['?'])),
    ]


# --- Ghidra -----------------------------------------------------------------

def _measure(program, lo_sw, hi_sw):
    from ghidra.program.model.address import AddressSet

    space = program.getAddressFactory().getDefaultAddressSpace()
    listing = program.getListing()
    refs = program.getReferenceManager()
    main = AddressSet(space.getAddress(2 * lo_sw), space.getAddress(2 * hi_sw - 1))
    insn_all = listing.getNumInstructions()
    insn_main = calls_all = 0
    for insn in listing.getInstructions(True):
        in_main = main.contains(insn.getAddress())
        insn_main += in_main
        for ref in refs.getReferencesFrom(insn.getAddress()):
            if ref.getReferenceType().isCall():
                calls_all += 1
    fm = program.getFunctionManager()
    funcs_main = sum(1 for f in fm.getFunctions(True) if main.contains(f.getEntryPoint()))
    in_func_main = sum(1 for insn in listing.getInstructions(main, True)
                       if fm.getFunctionContaining(insn.getAddress()) is not None)
    return {'functions': fm.getFunctionCount(), 'functions_main': funcs_main,
            'instructions': insn_all, 'instructions_main': insn_main,
            'in_function_main': in_func_main, 'call_refs': calls_all}


def cover(program, sites, stats, lo_sw, hi_sw):
    """Disassemble every aligned instruction Ghidra has not reached, one
    instruction each, start a function after each return's delay slots, then
    at each run of main-program instructions no function holds."""
    from ghidra.app.cmd.disassemble import DisassembleCommand
    from ghidra.app.cmd.function import CreateFunctionCmd
    from ghidra.program.model.address import AddressSet
    from ghidra.util.task import TaskMonitor

    mon = TaskMonitor.DUMMY
    space = program.getAddressFactory().getDefaultAddressSpace()
    listing = program.getListing()
    mem = program.getMemory()
    fm = program.getFunctionManager()

    for sw, nbytes in sites['aligned']:
        addr = space.getAddress(2 * sw)
        end = addr.add(nbytes - 1)
        if not (mem.contains(addr) and mem.contains(end)):
            stats['cover_outside_memory'] += 1
            continue
        have = listing.getInstructionContaining(addr)
        if have is not None:
            same = have.getAddress() == addr and have.getLength() == nbytes
            stats['cover_present' if same else 'cover_conflict'] += 1
            continue
        if listing.getInstructionContaining(end) is not None or \
                listing.getDefinedDataContaining(addr) is not None or \
                listing.getDefinedDataContaining(end) is not None:
            stats['cover_conflict'] += 1
            continue
        DisassembleCommand(addr, AddressSet(addr, end), False).applyTo(program, mon)
        got = listing.getInstructionAt(addr)
        if got is not None and got.getLength() == nbytes:
            stats['cover_disassembled'] += 1
        else:
            stats['cover_failed'] += 1

    for ret in sites['returns']:
        if ret['after'] is None:
            continue
        addr = space.getAddress(2 * ret['after'])
        if listing.getInstructionAt(addr) is None or fm.getFunctionContaining(addr) is not None:
            continue
        if CreateFunctionCmd(addr).applyTo(program, mon):
            stats['function_after_return'] += 1
        else:
            stats['function_after_return_failed'] += 1

    # Such a run follows a tail jump, a jump, a return or a gap, so it is code
    # no known flow reaches. Each new body can end before the next run, so
    # repeat until no run is left or no function can be made.
    main = AddressSet(space.getAddress(2 * lo_sw), space.getAddress(2 * hi_sw - 1))
    for _ in range(16):
        starts, prev_free, prev_end = [], False, None
        for insn in listing.getInstructions(main, True):
            addr = insn.getAddress()
            free = fm.getFunctionContaining(addr) is None
            if free and not (prev_free and prev_end == addr):
                starts.append(addr)
            prev_free, prev_end = free, insn.getMaxAddress().next()
        made = 0
        for addr in starts:
            if fm.getFunctionContaining(addr) is not None:
                continue
            if CreateFunctionCmd(addr).applyTo(program, mon):
                made += 1
            else:
                stats['function_run_start_failed'] += 1
        stats['function_run_start'] += made
        stats['function_run_rounds'] += 1
        if made == 0:
            break


def apply(program, sites, lo_sw, hi_sw, analyze, do_cover=False):
    from ghidra.app.cmd.disassemble import DisassembleCommand
    from ghidra.app.cmd.function import CreateFunctionCmd
    from ghidra.app.plugin.core.analysis import AutoAnalysisManager
    from ghidra.program.model.listing import FlowOverride
    from ghidra.util.task import TaskMonitor

    mon = TaskMonitor.DUMMY
    space = program.getAddressFactory().getDefaultAddressSpace()
    listing = program.getListing()
    mem = program.getMemory()
    fm = program.getFunctionManager()
    stats = collections.Counter()

    def insn_at(sw):
        addr = space.getAddress(2 * sw)
        if not mem.contains(addr):
            return addr, None
        if listing.getInstructionAt(addr) is None:
            DisassembleCommand(addr, None, False).applyTo(program, mon)
        return addr, listing.getInstructionAt(addr)

    def mark_call(sw, kind):
        addr, insn = insn_at(sw)
        if insn is None:
            stats[f'{kind}_not_disassembled'] += 1
            return None
        flow = insn.getFlowType()
        if flow.isCall():
            stats[f'{kind}_already_call'] += 1
        elif insn.getFlowOverride() != FlowOverride.CALL:
            insn.setFlowOverride(FlowOverride.CALL)
            stats[f'{kind}_override'] += 1
        DisassembleCommand(insn.getMaxAddress().next(), None, True).applyTo(program, mon)
        return insn

    for call in sites['calls']:
        if mark_call(call['sw'], 'call') is None:
            continue
        taddr = space.getAddress(2 * call['target'])
        if not mem.contains(taddr):
            stats['target_outside_memory'] += 1
            continue
        DisassembleCommand(taddr, None, True).applyTo(program, mon)
        if fm.getFunctionAt(taddr) is None:
            if CreateFunctionCmd(taddr).applyTo(program, mon):
                stats['function_created'] += 1
            else:
                stats['function_failed'] += 1

    for call in sites['indirect_calls']:
        mark_call(call['sw'], 'indirect_call')

    for ret in sites['returns']:
        addr, insn = insn_at(ret['sw'])
        if insn is None:
            stats['return_not_disassembled'] += 1
        elif insn.getFlowType().isTerminal():
            stats['return_already_terminal'] += 1
        else:
            insn.setFlowOverride(FlowOverride.RETURN)
            stats['return_override'] += 1

    if do_cover:
        cover(program, sites, stats, lo_sw, hi_sw)

    for func in list(fm.getFunctions(True)):
        CreateFunctionCmd.fixupFunctionBody(program, func, mon)

    if analyze:
        mgr = AutoAnalysisManager.getAnalysisManager(program)
        mgr.initializeOptions()
        mgr.reAnalyzeAll(None)
        mgr.startAnalysis(mon)
    return stats


def run_ghidra(args, sites, lo_sw, hi_sw):
    sys.path[:] = [p for p in sys.path if os.path.abspath(p or '.') != _here]
    os.environ.setdefault('GHIDRA_INSTALL_DIR', DEFAULT_GHIDRA)
    import pyghidra
    pyghidra.start(verbose=False)
    from ghidra.base.project import GhidraProject

    project = GhidraProject.openProject(os.path.expanduser(args.project), args.project_name, False)
    folder, name = args.program.rsplit('/', 1)
    program = project.openProgram(folder or '/', name, False)
    try:
        before = _measure(program, lo_sw, hi_sw)
        tx = program.startTransaction('sharcflow: calls, returns and coverage')
        try:
            stats = apply(program, sites, lo_sw, hi_sw, args.analyze, args.cover)
        finally:
            program.endTransaction(tx, True)
        after = _measure(program, lo_sw, hi_sw)
        if args.save:
            project.save(program)
    finally:
        project.close(program)
        project.close()
    return {'before': before, 'after': after, 'stats': dict(stats), 'saved': args.save}


def _int(text):
    return int(text, 0)


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument('region')
    ap.add_argument('--base-sw', type=_int, default=0x1C1338)
    ap.add_argument('--min-depth', type=int, default=8)
    ap.add_argument('--json')
    ap.add_argument('--program', help='program path in the project, e.g. /dt2-1.16_SHARC')
    ap.add_argument('--project', default='~/ghidra-projects/elektron-sharc')
    ap.add_argument('--project-name', default='elektron-sharc')
    ap.add_argument('--cover', action='store_true',
                    help='also disassemble every aligned instruction and create functions for uncovered code')
    ap.add_argument('--analyze', action='store_true', help='run auto-analysis afterwards')
    ap.add_argument('--save', action='store_true', help='save the program (default: discard)')
    args = ap.parse_args(argv)

    with open(args.region, 'rb') as f:
        data = f.read()
    lo_sw, hi_sw = args.base_sw, args.base_sw + len(data) // 2
    sites = find_sites(data, args.base_sw, args.min_depth)
    for line in summary(sites):
        print(line)
    print(f"aligned instructions {len(sites['aligned'])}")
    result = {'region': args.region, 'base_sw': args.base_sw,
              **{k: v for k, v in sites.items() if k != 'aligned'}}
    if args.program:
        ghidra = run_ghidra(args, sites, lo_sw, hi_sw)
        for key in ('before', 'after'):
            print(key, ' '.join(f'{k}={v}' for k, v in ghidra[key].items()))
        print('actions', ' '.join(f'{k}={v}' for k, v in sorted(ghidra['stats'].items())))
        print('saved' if ghidra['saved'] else 'not saved')
        result['ghidra'] = ghidra
    if args.json:
        with open(args.json, 'w') as f:
            json.dump(result, f, indent=1)
    return 0


if __name__ == '__main__':
    sys.exit(main())
