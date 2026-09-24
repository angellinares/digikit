# fmt: off
"""Query an already-imported/analyzed Ghidra program over PyGhidra.

    GHIDRA_INSTALL_DIR=/opt/homebrew/Cellar/ghidra/12.1.3/libexec \
    uv run python tools/ghidraq.py PROGRAM strings 'Digisharc.*'
    uv run python tools/ghidraq.py PROGRAM xrefs 0x4022b20e
    uv run python tools/ghidraq.py PROGRAM func 0x40001622
    uv run python tools/ghidraq.py PROGRAM callers 0x40001622
    uv run python tools/ghidraq.py PROGRAM symbols 'rpc.*'
    uv run python tools/ghidraq.py PROGRAM range 0x402f9c14 0x40307f60

PROGRAM is the program's path inside the project ("section_3_MAIN_OS.bin"
for Digitakt II, "dn2_MAIN_OS.bin" for Digitone II; leading slash optional).

Read-only throughout: nothing opens a transaction, nothing is saved, and
the program on disk is untouched.

The JVM and project load cost ten-odd seconds, which dwarfs any single
query, so every subcommand takes MULTIPLE arguments and `--then` chains a
further subcommand into the same invocation:

    uv run python tools/ghidraq.py PROGRAM strings 'rpc' \
        --then xrefs 0x40123456 0x40123480 \
        --then decompile 0x40008000

Use --json for machine-readable output (one object per query, on stdout).

This exists because tools/ghidra/*.java need a full ghidra.sh launch per
run (minutes).
"""
import argparse
import json
import os
import re
import sys

# tools/ghidra/ is a plain directory of Java scripts that shadows the real
# `ghidra` Java-bridge namespace PyGhidra needs, once tools/ lands on
# sys.path -- which it does when this is run as `python tools/ghidraq.py`.
_here = os.path.dirname(os.path.abspath(__file__))
sys.path[:] = [p for p in sys.path if os.path.abspath(p or '.') != _here]

DEFAULT_PROJECT = os.path.expanduser('~/ghidra-projects/dt2')
DEFAULT_PROJECT_NAME = 'dt2'
DEFAULT_GHIDRA = '/opt/homebrew/Cellar/ghidra/12.1.3/libexec'
DECOMPILE_TIMEOUT = 60
SUBCOMMANDS = ('strings', 'symbols', 'xrefs', 'func', 'callers', 'decompile', 'read', 'range', 'pcode', 'slice', 'stores', 'loads')
SLICE_SCHEMA_VERSION = 1
SLICE_NODE_CAP = 2000


def _addr(program, value):
    return program.getAddressFactory().getDefaultAddressSpace().getAddress(value)


def q_strings(program, args, out):
    """Defined strings whose value matches any given regex (case-insensitive)."""
    from ghidra.program.model.data import (  # pyright: ignore[reportMissingImports]
        StringDataType,
    )
    pats = [re.compile(a, re.I) for a in args]
    listing = program.getListing()
    hits = []
    it = listing.getDefinedData(True)
    while it.hasNext():
        d = it.next()
        dt = d.getDataType()
        if not isinstance(dt, StringDataType) and 'string' not in dt.getName().lower():
            continue
        val = d.getValue()
        if val is None:
            continue
        val = str(val)
        if any(p.search(val) for p in pats):
            hits.append({'addr': '0x%x' % d.getAddress().getOffset(),
                         'len': d.getLength(), 'value': val})
    out('strings', {'patterns': args, 'count': len(hits), 'hits': hits})


def q_symbols(program, args, out):
    """Symbols (functions, labels, data) whose name matches any given regex."""
    pats = [re.compile(a, re.I) for a in args]
    hits = []
    for sym in program.getSymbolTable().getAllSymbols(True):
        name = sym.getName()
        if any(p.search(name) for p in pats):
            hits.append({'addr': '0x%x' % sym.getAddress().getOffset(),
                         'name': name, 'type': str(sym.getSymbolType()),
                         'namespace': str(sym.getParentNamespace().getName(True))})
    out('symbols', {'patterns': args, 'count': len(hits), 'hits': hits})


def q_xrefs(program, args, out):
    """Everything that references each given address, with the containing function."""
    fm = program.getFunctionManager()
    results = []
    for a in args:
        target = _addr(program, int(a, 16))
        refs = []
        for r in program.getReferenceManager().getReferencesTo(target):
            frm = r.getFromAddress()
            fn = fm.getFunctionContaining(frm)
            refs.append({'from': '0x%x' % frm.getOffset(),
                         'type': str(r.getReferenceType()),
                         'in_function': fn.getName() if fn else None,
                         'function_entry': '0x%x' % fn.getEntryPoint().getOffset() if fn else None})
        results.append({'addr': a, 'count': len(refs), 'refs': refs})
    out('xrefs', results)


def q_func(program, args, out):
    """The function containing each address: name, bounds, callers, callees."""
    fm = program.getFunctionManager()
    results = []
    for a in args:
        fn = fm.getFunctionContaining(_addr(program, int(a, 16)))
        if fn is None:
            results.append({'addr': a, 'function': None})
            continue
        body = fn.getBody()
        results.append({
            'addr': a,
            'function': fn.getName(),
            'entry': '0x%x' % fn.getEntryPoint().getOffset(),
            'size': body.getNumAddresses(),
            'signature': str(fn.getSignature()),
            'callers': sorted({f.getName() for f in fn.getCallingFunctions(None)}),
            'callees': sorted({f.getName() for f in fn.getCalledFunctions(None)}),
        })
    out('func', results)


def q_callers(program, args, out):
    """Just the callers of each address's function, with their entry points."""
    fm = program.getFunctionManager()
    results = []
    for a in args:
        fn = fm.getFunctionContaining(_addr(program, int(a, 16)))
        if fn is None:
            results.append({'addr': a, 'function': None, 'callers': []})
            continue
        callers = [{'name': f.getName(), 'entry': '0x%x' % f.getEntryPoint().getOffset()}
                   for f in fn.getCallingFunctions(None)]
        results.append({'addr': a, 'function': fn.getName(),
                        'count': len(callers),
                        'callers': sorted(callers, key=lambda c: c['entry'])})
    out('callers', results)


def q_decompile(program, args, out):
    """Decompiled C for the function containing each address."""
    from ghidra.app.decompiler import (  # pyright: ignore[reportMissingImports]
        DecompileOptions,
        DecompInterface,
    )
    fm = program.getFunctionManager()
    ifc = DecompInterface()
    ifc.setOptions(DecompileOptions())
    ifc.openProgram(program)
    results = []
    try:
        for a in args:
            fn = fm.getFunctionContaining(_addr(program, int(a, 16)))
            if fn is None:
                results.append({'addr': a, 'function': None, 'c': None})
                continue
            res = ifc.decompileFunction(fn, DECOMPILE_TIMEOUT, None)
            results.append({'addr': a, 'function': fn.getName(),
                            'entry': '0x%x' % fn.getEntryPoint().getOffset(),
                            'c': res.getDecompiledFunction().getC() if res.decompileCompleted() else None,
                            'error': None if res.decompileCompleted() else str(res.getErrorMessage())})
    finally:
        ifc.dispose()
    out('decompile', results)


def q_read(program, args, out):
    """Hex + ASCII of bytes at ADDR:LEN (length defaults to 64)."""
    from jpype import JArray, JByte  # pyright: ignore[reportMissingImports]
    mem = program.getMemory()
    results = []
    for a in args:
        spec, _, n = a.partition(':')
        n = int(n, 0) if n else 64
        base = int(spec, 16)
        # Must be a real Java byte[]: JPype copies a Python bytearray into a
        # throwaway array, so getBytes() would fill that and we would read
        # back the zeros we started with.
        buf = JArray(JByte)(n)
        try:
            got = mem.getBytes(_addr(program, base), buf)
        except Exception as exc:
            results.append({'addr': spec, 'error': str(exc)})
            continue
        raw = bytes(bytearray(x & 0xFF for x in buf[:got]))
        results.append({'addr': spec, 'len': got, 'hex': raw.hex(),
                        'ascii': ''.join(chr(c) if 32 <= c < 127 else '.' for c in raw)})
    out('read', results)


def q_range(program, args, out):
    """Everything Ghidra knows about one or more address ranges: LO HI [LO HI ...].

    For each [LO, HI) range: memory block membership (and whether it is an
    uninitialised/bit block), defined data (address, type, label), defined
    instruction byte-count, symbols, and every address inside the range that
    is the *destination* of at least one reference, each with its referrers
    (from-address, type, containing function). This is the Ghidra-side
    counterpart to a static immediate scan (tools/refscan.py): it reports
    what Ghidra's own analysis -- which can follow some things a no-semantics
    disassembly sweep cannot, like relocations -- resolved as touching the
    range, not just what a fresh linear sweep can see.
    """
    if len(args) % 2 != 0:
        raise ValueError('range wants pairs of LO HI, got an odd number of args: %r' % (args,))
    listing = program.getListing()
    fm = program.getFunctionManager()
    af = program.getAddressFactory()
    space = af.getDefaultAddressSpace()
    refmgr = program.getReferenceManager()
    memory = program.getMemory()
    results = []
    for i in range(0, len(args), 2):
        lo, hi = int(args[i], 16), int(args[i + 1], 16)
        lo_addr, hi_addr = space.getAddress(lo), space.getAddress(hi - 1)
        addr_set = program.getAddressFactory().getAddressSet(lo_addr, hi_addr)

        blocks = []
        for blk in memory.getBlocks():
            if blk.getStart().getOffset() <= hi - 1 and blk.getEnd().getOffset() >= lo:
                blocks.append({'name': blk.getName(),
                               'start': '0x%x' % blk.getStart().getOffset(),
                               'end': '0x%x' % blk.getEnd().getOffset(),
                               'initialized': blk.isInitialized(),
                               'type': str(blk.getType())})

        data = []
        it = listing.getDefinedData(addr_set, True)
        while it.hasNext():
            d = it.next()
            data.append({'addr': '0x%x' % d.getAddress().getOffset(),
                        'type': d.getDataType().getName(),
                        'len': d.getLength(),
                        'label': d.getLabel()})

        insn_bytes = 0
        insn_count = 0
        it = listing.getInstructions(addr_set, True)
        while it.hasNext():
            ins = it.next()
            insn_bytes += ins.getLength()
            insn_count += 1

        symbols = []
        it = program.getSymbolTable().getSymbolIterator(lo_addr, True)
        while it.hasNext():
            sym = it.next()
            if sym.getAddress().getOffset() >= hi:
                break
            symbols.append({'addr': '0x%x' % sym.getAddress().getOffset(),
                            'name': sym.getName(), 'type': str(sym.getSymbolType())})

        referenced = []
        dit = refmgr.getReferenceDestinationIterator(addr_set, True)
        while dit.hasNext():
            dest = dit.next()
            refs = []
            for r in refmgr.getReferencesTo(dest):
                frm = r.getFromAddress()
                fn = fm.getFunctionContaining(frm)
                refs.append({'from': '0x%x' % frm.getOffset(),
                            'type': str(r.getReferenceType()),
                            'in_function': fn.getName() if fn else None})
            referenced.append({'addr': '0x%x' % dest.getOffset(), 'refs': refs})

        results.append({
            'lo': '0x%x' % lo, 'hi': '0x%x' % hi, 'size': hi - lo,
            'blocks': blocks, 'defined_data': data,
            'instruction_bytes': insn_bytes, 'instruction_count': insn_count,
            'symbols': symbols, 'referenced_addresses': referenced,
        })
    out('range', results)


def _offset(value):
    return value.getOffset()


def _space_name(space):
    return str(space.getName())


def address_space_metadata(space, language=None):
    """Stable address-space facts; SHARC conversion requires language and word size."""
    wordsize = space.getAddressableUnitSize()
    name = _space_name(space)
    language = None if language is None else str(language)
    short_words = bool(language and language.startswith('SHARC_VISA') and wordsize == 2)
    result = {'name': name, 'word_size': wordsize, 'language': language,
              'short_word_addresses': short_words,
              'conversion_rule': 'displayed_offset = logical_short_word * 2' if short_words else None}
    if short_words:
        result['delay_slot_caveat'] = ('HighFunction p-code does not model SHARC_VISA delay-slot execution.')
    return result


def program_address_metadata(program):
    return address_space_metadata(program.getAddressFactory().getDefaultAddressSpace(),
                                  program.getLanguage().getLanguageID())


def resolve_coordinate(program, token):
    """Parse a displayed offset, or explicit SHARC ``sw:`` logical word address."""
    metadata = program_address_metadata(program)
    short_word = token.startswith('sw:')
    number = token[3:] if short_word else token
    try:
        value = int(number, 0)
    except ValueError:
        raise ValueError('invalid coordinate %r' % token)
    if value < 0:
        raise ValueError('coordinate must be non-negative: %r' % token)
    if short_word and not metadata['short_word_addresses']:
        raise ValueError('sw: coordinate %r requires SHARC_VISA language with addressable unit size 2' % token)
    displayed = value * 2 if short_word else value
    logical = value if short_word else (displayed // 2 if metadata['short_word_addresses'] and displayed % 2 == 0 else None)
    return displayed, {'requested': token, 'logical': None if logical is None else '0x%x' % logical,
                       'displayed': '0x%x' % displayed,
                       'conversion_rule': metadata['conversion_rule']}


def q_pcode(program, args, out):
    """Raw instruction p-code at PC; no decompilation, SSA, or reference changes."""
    payload = {'schema_version': 1, 'kind': 'raw-instruction-pcode',
               'requested': {'pc': args[0] if args else None}, 'status': 'error',
               'errors': [], 'operations': [], 'instruction': None,
               'program': _program_identity(program),
               'address_space': program_address_metadata(program)}
    if len(args) != 1:
        payload['errors'].append('pcode wants PC')
        out('pcode', payload)
        return
    try:
        pc, payload['pc'] = resolve_coordinate(program, args[0])
    except ValueError as exc:
        payload['errors'].append(str(exc))
        out('pcode', payload)
        return
    try:
        instruction = program.getListing().getInstructionAt(_addr(program, pc))
        if instruction is None:
            payload['status'] = 'no-instruction'
            out('pcode', payload)
            return
        payload['instruction'] = {'mnemonic': str(instruction.getMnemonicString()),
                                  'length': instruction.getLength()}
        raw_ops = instruction.getPcode()
        if raw_ops is None:
            payload['status'] = 'empty-pcode'
            out('pcode', payload)
            return
        for order, op in enumerate(raw_ops):
            try:
                item = serialize_raw_pcode_op(op, order)
                payload['operations'].append(item)
            except Exception as exc:
                payload['errors'].append('operation %d: %s: %s' %
                                         (order, type(exc).__name__, exc))
        if payload['errors']:
            payload['status'] = 'malformed-pcode'
        elif not payload['operations']:
            payload['status'] = 'empty-pcode'
        else:
            payload['status'] = 'ok'
    except Exception as exc:
        payload['errors'].append('%s: %s' % (type(exc).__name__, exc))
    out('pcode', payload)


def serialize_varnode(vn):
    """A Java-independent, JSON-safe varnode identity (never str(vn))."""
    result = {'kind': 'constant' if vn.isConstant() else
              'register' if vn.isRegister() else 'unique' if vn.isUnique() else 'address',
              'size': vn.getSize()}
    offset = vn.getOffset()
    if vn.isConstant():
        result['value'] = '0x%x' % offset
        return result
    address = vn.getAddress()
    if address is not None:
        result['space'] = _space_name(address.getAddressSpace())
    result['offset'] = '0x%x' % offset
    return result


def op_sort_key(op):
    seq = op.getSeqnum()
    return (_offset(seq.getTarget()), seq.getTime(), str(op.getMnemonic()),
            0 if op.getOutput() is None else op.getOutput().getOffset())


def sorted_ops(ops):
    return sorted(ops, key=op_sort_key)


def serialize_op(op):
    seq = op.getSeqnum()
    return {'seq': {'pc': '0x%x' % _offset(seq.getTarget()), 'time': seq.getTime()},
            'mnemonic': str(op.getMnemonic()),
            'inputs': [serialize_varnode(v) for v in op.getInputs()],
            'output': serialize_varnode(op.getOutput()) if op.getOutput() is not None else None}


def serialize_raw_pcode_op(op, order):
    """Instruction p-code serialization; a sequence number is optional metadata."""
    result = {'order': order, 'mnemonic': str(op.getMnemonic()),
              'inputs': [serialize_varnode(v) for v in op.getInputs()],
              'output': serialize_varnode(op.getOutput()) if op.getOutput() is not None else None}
    get_seqnum = getattr(op, 'getSeqnum', None)
    if get_seqnum is not None:
        seq = get_seqnum()
        result['seq'] = {'pc': '0x%x' % _offset(seq.getTarget()), 'time': seq.getTime()}
    return result


def serialize_memory_op(op, direction, program=None):
    """Serialize an access with its normalized representation when applicable."""
    result = serialize_op(op)
    access = memory_access_operands(op, direction, program)
    if access is not None:
        result['representation'] = access['representation']
        memory_space = access['memory_space']
        result['memory_space'] = ({'status': 'resolved', 'name': _space_name(memory_space)}
                                  if memory_space is not None else
                                  {'status': 'unresolved', 'space_id':
                                   None if access['space'] is None else '0x%x' % access['space'].getOffset()})
    return result


def _vn_key(vn):
    data = serialize_varnode(vn)
    return tuple(sorted(data.items()))


def backward_slice_fallback(seeds, node_cap=SLICE_NODE_CAP):
    """Bounded def-use fallback; it intentionally does not evaluate expressions."""
    pending, seen, ops, roots = list(seeds), set(), [], []
    partial = False
    while pending:
        vn = pending.pop()
        key = _vn_key(vn)
        if key in seen:
            continue
        seen.add(key)
        if vn.isConstant():
            continue
        definition = vn.getDef()
        if definition is None:
            roots.append(vn)
            continue
        if len(ops) >= node_cap:
            partial = True
            break
        ops.append(definition)
        pending.extend(definition.getInputs())
    return {'ops': sorted_ops({op_sort_key(op): op for op in ops}.values()),
            'roots': sorted({ _vn_key(v): v for v in roots }.values(), key=_vn_key),
            'partial': partial}


def parse_slice_selector(selector):
    if selector in ('store:value', 'store:address', 'load:value', 'load:address', 'output'):
        return (selector, None)
    if selector.startswith('input:'):
        index = int(selector[6:], 10)
        if index < 0:
            raise ValueError('input selector must be non-negative')
        return ('input', index)
    if selector.startswith('reg:') and selector[4:]:
        return ('reg', selector[4:])
    raise ValueError('unsupported selector %r' % selector)


def _is_memory_space(space):
    """True only for a real Ghidra memory space (or an explicit test double)."""
    try:
        return bool(space.isMemorySpace())
    except AttributeError:
        return bool(getattr(space, '_test_is_memory_space', False))


def _is_direct_memory(vn):
    if vn is None or vn.isConstant() or vn.isRegister() or vn.isUnique():
        return False
    address = vn.getAddress()
    return address is not None and _is_memory_space(address.getAddressSpace())


def _space_identity(space):
    """An address-space identity that does not conflate equal offsets/names."""
    try:
        return ('id', space.getSpaceID())
    except AttributeError:
        return ('object', id(space))


def _resolve_raw_memory_space(program, space_vn):
    """Resolve a LOAD/STORE space-id varnode through the program factory."""
    if program is None or not space_vn.isConstant():
        return None
    space_id = space_vn.getOffset()
    try:
        space = program.getAddressFactory().getAddressSpace(space_id)
    except Exception:
        space = None
    if space is None or not _is_memory_space(space):
        return None
    return space


def memory_access_operands(op, direction, program=None):
    """Normalize raw p-code and HighFunction direct-memory COPY accesses."""
    inputs, output = list(op.getInputs()), op.getOutput()
    mnemonic = str(op.getMnemonic())
    if direction == 'store' and mnemonic == 'STORE' and len(inputs) == 3:
        return {'space': inputs[0], 'memory_space': _resolve_raw_memory_space(program, inputs[0]),
                'address': inputs[1], 'value': inputs[2], 'representation': 'STORE',
                'direct_address': False}
    if direction == 'load' and mnemonic == 'LOAD' and len(inputs) == 2 and output is not None:
        return {'space': inputs[0], 'memory_space': _resolve_raw_memory_space(program, inputs[0]),
                'address': inputs[1], 'value': output, 'representation': 'LOAD',
                'direct_address': False}
    if mnemonic != 'COPY' or len(inputs) != 1 or output is None:
        return None
    source = inputs[0]
    source_memory, output_memory = _is_direct_memory(source), _is_direct_memory(output)
    # A memory-to-memory COPY is how the decompiler folds "load a literal address,
    # then store that value to another literal address". It is a real store at the
    # output and a real load at the source, so having direct memory on both ends
    # must not drop the access.
    if direction == 'store' and output_memory:
        return {'space': None, 'memory_space': output.getAddress().getAddressSpace(),
                'address': output, 'value': source,
                'representation': 'COPY_MEM_TO_MEM_WRITE' if source_memory else 'COPY_DIRECT_WRITE',
                'direct_address': True}
    if direction == 'load' and source_memory:
        return {'space': None, 'memory_space': source.getAddress().getAddressSpace(),
                'address': source, 'value': output,
                'representation': 'COPY_MEM_TO_MEM_READ' if output_memory else 'COPY_DIRECT_READ',
                'direct_address': True}
    return None


def store_operands(op):
    access = memory_access_operands(op, 'store')
    return None if access is None else (access['space'], access['address'], access['value'])


def load_operands(op):
    access = memory_access_operands(op, 'load')
    return None if access is None else (access['space'], access['address'], access['value'])


def slice_seed_specs(ops, selector, register=None, program=None):
    """Return (varnode, follow-definition) pairs for a slice selector."""
    kind, value = parse_slice_selector(selector)
    result = []
    for op in ops:
        inputs = list(op.getInputs())
        direction = 'store' if kind.startswith('store:') else 'load' if kind.startswith('load:') else None
        access = memory_access_operands(op, direction, program) if direction else None
        if access is not None and kind.endswith(':value'):
            result.append((access['value'], True))
        elif access is not None and kind.endswith(':address'):
            # A COPY's memory varnode is its own definition output/input; following it
            # would incorrectly slice through the transferred value.
            result.append((access['address'], not access['direct_address']))
        elif kind == 'input' and value is not None and value < len(inputs):
            result.append((inputs[value], True))
        elif kind == 'output' and op.getOutput() is not None:
            result.append((op.getOutput(), True))
        elif kind == 'reg' and register is not None:
            for vn in inputs + ([op.getOutput()] if op.getOutput() is not None else []):
                if (vn.isRegister() and vn.getOffset() == register.getAddress().getOffset()
                        and vn.getSize() == register.getMinimumByteSize()):
                    result.append((vn, True))
    unique = {( _vn_key(vn), follow): (vn, follow) for vn, follow in result}
    return sorted(unique.values(), key=lambda item: (_vn_key(item[0]), item[1]))


def select_slice_seeds(ops, selector, register=None, program=None):
    """Backward-compatible varnode-only selector helper."""
    return [vn for vn, _follow in slice_seed_specs(ops, selector, register, program)]


def _high_ops(high):
    ops = []
    it = high.getPcodeOps()
    while it.hasNext():
        ops.append(it.next())
    return sorted_ops(ops)


def _decompiler(program):
    from ghidra.app.decompiler import (  # pyright: ignore[reportMissingImports]
        DecompileOptions,
        DecompInterface,
    )
    ifc = DecompInterface()
    ifc.setOptions(DecompileOptions())
    ifc.openProgram(program)
    return ifc


def _slice(seeds, root_seeds=()):
    """Use Ghidra's slice first; retain fallback for bridge overload differences."""
    fallback = backward_slice_fallback(seeds)
    fallback['roots'] = sorted({_vn_key(v): v for v in (*fallback['roots'], *root_seeds)}.values(), key=_vn_key)
    try:
        from ghidra.app.decompiler.component import (  # pyright: ignore[reportMissingImports]
            DecompilerUtils,
        )
        found = []
        for seed in seeds:
            found.extend(DecompilerUtils.getBackwardSliceToPCodeOps(seed))
        found = sorted_ops({op_sort_key(op): op for op in found}.values())
        return {'ops': found[:SLICE_NODE_CAP], 'roots': fallback['roots'],
                'partial': fallback['partial'] or len(found) > SLICE_NODE_CAP,
                'engine': 'DecompilerUtils'}
    except Exception as exc:
        fallback['engine'] = 'def-use-fallback'
        fallback['error'] = '%s: %s' % (type(exc).__name__, exc)
        return fallback


def _program_identity(program):
    return {'name': str(program.getName()), 'language': str(program.getLanguage().getLanguageID())}


def _function_identity(fn):
    return {'name': str(fn.getName()), 'entry': '0x%x' % _offset(fn.getEntryPoint())}


def q_slice(program, args, out):
    """Backward p-code slice at PC SELECTOR, without modifying the project."""
    payload = {'schema_version': SLICE_SCHEMA_VERSION, 'requested': {'pc': args[0] if args else None,
               'selector': args[1] if len(args) > 1 else None}, 'status': 'error', 'partial': False,
               'errors': [], 'matching_ops': [], 'selected_seeds': [], 'slice_ops': [], 'roots': []}
    payload['program'] = _program_identity(program)
    payload['address_space'] = program_address_metadata(program)
    if len(args) != 2:
        payload['errors'].append('slice wants PC SELECTOR')
        out('slice', payload); return
    try:
        pc, payload['pc'] = resolve_coordinate(program, args[0])
        parse_slice_selector(args[1])
    except ValueError as exc:
        payload['errors'].append(str(exc)); out('slice', payload); return
    fn = program.getFunctionManager().getFunctionContaining(_addr(program, pc))
    if fn is None:
        payload['errors'].append('no containing function'); out('slice', payload); return
    payload['function'] = _function_identity(fn)
    ifc = _decompiler(program)
    try:
        from ghidra.util.task import TaskMonitor  # type: ignore[import-not-found]
        res = ifc.decompileFunction(fn, DECOMPILE_TIMEOUT, TaskMonitor.DUMMY)
        if not res.decompileCompleted():
            payload['errors'].append(str(res.getErrorMessage())); out('slice', payload); return
        matches = [op for op in _high_ops(res.getHighFunction()) if _offset(op.getSeqnum().getTarget()) == pc]
        parsed_kind, parsed_value = parse_slice_selector(args[1])
        register = program.getLanguage().getRegister(parsed_value) if parsed_kind == 'reg' else None
        if parsed_kind == 'reg' and register is None:
            payload['errors'].append('unknown register %s' % parsed_value)
        seed_specs = slice_seed_specs(matches, args[1], register, program)
        seeds = [vn for vn, follow in seed_specs if follow]
        roots = [vn for vn, follow in seed_specs if not follow]
        direction = 'store' if parsed_kind.startswith('store:') else 'load' if parsed_kind.startswith('load:') else None
        payload['matching_ops'] = [serialize_memory_op(op, direction, program) if direction else serialize_op(op)
                                   for op in matches]
        payload['selected_seeds'] = [serialize_varnode(vn) for vn, _follow in seed_specs]
        sliced = _slice(seeds, roots)
        payload['slice_ops'] = [serialize_op(op) for op in sliced['ops']]
        payload['roots'] = [serialize_varnode(v) for v in sliced['roots']]
        payload['partial'], payload['engine'] = sliced['partial'], sliced['engine']
        if 'error' in sliced: payload['errors'].append(sliced['error'])
        payload['status'] = 'ok' if seed_specs else 'no-seeds'
    except Exception as exc:
        payload['errors'].append('%s: %s' % (type(exc).__name__, exc))
    finally:
        ifc.dispose()
    out('slice', payload)


def _serialize_slice(result):
    serialized = {'ops': [serialize_op(x) for x in result['ops']],
                  'roots': [serialize_varnode(x) for x in result['roots']],
                  'partial': result['partial'], 'engine': result['engine']}
    if 'error' in result:
        serialized['error'] = result['error']
    return serialized


def _q_memory_accesses(program, args, out, direction, command):
    """Find normalized memory accesses, slicing address and value separately."""
    payload = {'schema_version': SLICE_SCHEMA_VERSION, 'status': 'ok', 'partial': False,
               'errors': [], 'target': args[0] if args else None, 'functions': []}
    payload['program'] = _program_identity(program)
    payload['address_space'] = program_address_metadata(program)
    if len(args) not in (1, 3):
        payload['status'] = 'error'; payload['errors'].append('%s wants TARGET [LO HI]' % command); out(command, payload); return
    try:
        target, payload['target_coordinate'] = resolve_coordinate(program, args[0])
        if len(args) == 3:
            lo, payload_lo = resolve_coordinate(program, args[1])
            hi, payload_hi = resolve_coordinate(program, args[2])
            payload['bounds'] = {'lo': payload_lo, 'hi': payload_hi}
            bounds = (lo, hi)
        else:
            bounds = None
        if bounds is not None and bounds[0] > bounds[1]:
            raise ValueError('%s range requires LO <= HI' % command)
    except ValueError as exc:
        payload['status'] = 'error'; payload['errors'].append(str(exc)); out(command, payload); return
    functions = sorted(list(program.getFunctionManager().getFunctions(True)), key=lambda f: _offset(f.getEntryPoint()))
    if not functions:
        out(command, payload); return
    ifc = _decompiler(program)
    try:
        from ghidra.util.task import TaskMonitor  # type: ignore[import-not-found]
        for fn in functions:
            entry = _offset(fn.getEntryPoint())
            if bounds and not (bounds[0] <= entry < bounds[1]): continue
            item = _function_identity(fn); item['matches'] = []
            try:
                result = ifc.decompileFunction(fn, DECOMPILE_TIMEOUT, TaskMonitor.DUMMY)
                if not result.decompileCompleted():
                    item['error'] = str(result.getErrorMessage()); payload['partial'] = True; payload['functions'].append(item); continue
                for op in _high_ops(result.getHighFunction()):
                    access = memory_access_operands(op, direction, program)
                    if access is None: continue
                    address, value = access['address'], access['value']
                    static_address = access['direct_address'] or address.isConstant()
                    memory_space = access['memory_space']
                    exact = (static_address and memory_space is not None
                             and address.getOffset() == target
                             and _space_identity(memory_space) == _space_identity(
                                 program.getAddressFactory().getDefaultAddressSpace()))
                    # A resolved static access to another offset or address space cannot
                    # be the requested default-space target.  An unresolved raw p-code
                    # space-id is retained as partial evidence rather than guessed.
                    if static_address and memory_space is not None and not exact:
                        continue
                    address_slice = _slice([] if access['direct_address'] else [address],
                                           [address] if access['direct_address'] else [])
                    value_slice = _slice([value])
                    space_metadata = ({'status': 'resolved', 'name': _space_name(memory_space)}
                                      if memory_space is not None else
                                      {'status': 'unresolved', 'space_id':
                                       None if access['space'] is None else '0x%x' % access['space'].getOffset()})
                    partial = (address_slice['partial'] or value_slice['partial']
                               or memory_space is None)
                    item['matches'].append({'op': serialize_op(op),
                        'representation': access['representation'],
                        'memory_space': space_metadata,
                        'classification': 'exact-constant-target' if exact else 'computed-or-unresolved',
                        'address_slice': _serialize_slice(address_slice),
                        'value_slice': _serialize_slice(value_slice),
                        'partial': partial})
                    payload['partial'] = payload['partial'] or partial
                item['matches'].sort(key=lambda match: op_sort_key_from_serialized(match['op']))
                if item['matches']: payload['functions'].append(item)
            except Exception as exc:
                item['error'] = '%s: %s' % (type(exc).__name__, exc); payload['partial'] = True; payload['functions'].append(item)
    finally:
        ifc.dispose()
    out(command, payload)


def op_sort_key_from_serialized(op):
    return (int(op['seq']['pc'], 16), op['seq']['time'], op['mnemonic'],
            0 if op['output'] is None else int(op['output'].get('offset', op['output'].get('value', '0')), 16))


def q_stores(program, args, out):
    """Find memory writes for TARGET, slicing address and value separately."""
    _q_memory_accesses(program, args, out, 'store', 'stores')


def q_loads(program, args, out):
    """Find memory reads for TARGET, slicing address and value separately."""
    _q_memory_accesses(program, args, out, 'load', 'loads')


HANDLERS = {'strings': q_strings, 'symbols': q_symbols, 'xrefs': q_xrefs,
            'func': q_func, 'callers': q_callers, 'decompile': q_decompile,
            'read': q_read, 'range': q_range, 'pcode': q_pcode, 'slice': q_slice,
            'stores': q_stores, 'loads': q_loads}


def _print_text(kind, payload):
    print('=' * 72)
    print('## %s' % kind)
    if kind in ('slice', 'stores', 'loads'):
        print('%s (partial=%s)' % (payload['status'], payload['partial']))
        for error in payload['errors']:
            print('  ERROR: %s' % error)
        return
    if kind == 'pcode':
        print('%s (raw instruction p-code)' % payload['status'])
        for error in payload['errors']:
            print('  ERROR: %s' % error)
        if payload['instruction'] is not None:
            print('  %s (%d bytes)' % (payload['instruction']['mnemonic'],
                                       payload['instruction']['length']))
        for op in payload['operations']:
            print('  %d  %s' % (op['order'], op['mnemonic']))
        return
    if kind in ('strings', 'symbols'):
        print('%d hit(s) for %s' % (payload['count'], payload['patterns']))
        for h in payload['hits']:
            if kind == 'strings':
                print('  %s  (%d)  %r' % (h['addr'], h['len'], h['value']))
            else:
                ns = '' if h['namespace'] in ('Global', '') else h['namespace'] + '::'
                print('  %s  %-10s %s%s' % (h['addr'], h['type'], ns, h['name']))
        return
    for item in payload:
        if kind == 'xrefs':
            print('%s -- %d reference(s)' % (item['addr'], item['count']))
            for r in item['refs']:
                print('  from %s  %-14s %s' % (r['from'], r['type'], r['in_function'] or '(no function)'))
        elif kind in ('func', 'callers'):
            if item['function'] is None:
                print('%s -- no function' % item['addr']); continue
            if kind == 'func':
                print('%s -- %s @ %s  (%d bytes)' % (item['addr'], item['function'], item['entry'], item['size']))
                print('  sig:     %s' % item['signature'])
                print('  callers: %s' % (', '.join(item['callers']) or '(none)'))
                print('  callees: %s' % (', '.join(item['callees']) or '(none)'))
            else:
                print('%s -- %s, %d caller(s)' % (item['addr'], item['function'], item['count']))
                for c in item['callers']:
                    print('  %s  %s' % (c['entry'], c['name']))
        elif kind == 'decompile':
            if item['function'] is None:
                print('%s -- no function' % item['addr']); continue
            print('%s -- %s @ %s' % (item['addr'], item['function'], item['entry']))
            print(item['c'] or ('DECOMPILE FAILED: %s' % item['error']))
        elif kind == 'read':
            if 'error' in item:
                print('%s -- %s' % (item['addr'], item['error'])); continue
            print('%s (%d bytes)' % (item['addr'], item['len']))
            h = item['hex']
            for off in range(0, item['len'], 16):
                row = h[off * 2:off * 2 + 32]
                print('  %08x  %-32s  %s' % (int(item['addr'], 16) + off, row,
                                             item['ascii'][off:off + 16]))
        elif kind == 'range':
            print('%s-%s (%d bytes)' % (item['lo'], item['hi'], item['size']))
            for b in item['blocks']:
                print('  block %-16s %s-%s  initialized=%s  type=%s'
                     % (b['name'], b['start'], b['end'], b['initialized'], b['type']))
            print('  %d defined data item(s), %d instruction(s) (%d bytes), %d symbol(s)'
                 % (len(item['defined_data']), item['instruction_count'],
                    item['instruction_bytes'], len(item['symbols'])))
            for d in item['defined_data']:
                print('    data   %s  %-16s len=%-4d %s' % (d['addr'], d['type'], d['len'], d['label'] or ''))
            for s in item['symbols']:
                print('    symbol %s  %-10s %s' % (s['addr'], s['type'], s['name']))
            print('  %d referenced address(es) inside the range:' % len(item['referenced_addresses']))
            for r in item['referenced_addresses']:
                print('    %s  <- %d ref(s)' % (r['addr'], len(r['refs'])))
                for ref in r['refs']:
                    print('        from %s  %-14s %s' % (ref['from'], ref['type'], ref['in_function'] or '(no function)'))


def main(argv):
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument('program', help='program path inside the project')
    ap.add_argument('subcommand', choices=SUBCOMMANDS)
    ap.add_argument('args', nargs='+', help='arguments for the subcommand')
    ap.add_argument('--then', nargs='+', action='append', default=[],
                    metavar='SUBCOMMAND ARG',
                    help='another subcommand to run in the same JVM; repeatable')
    ap.add_argument('--project', default=DEFAULT_PROJECT)
    ap.add_argument('--project-name', default=DEFAULT_PROJECT_NAME)
    ap.add_argument('--json', action='store_true', help='emit JSON instead of text')
    args = ap.parse_args(argv)

    queries = [(args.subcommand, args.args)]
    for chain in args.then:
        if chain[0] not in SUBCOMMANDS:
            ap.error('--then wants a subcommand first, got %r (choose from %s)'
                     % (chain[0], ', '.join(SUBCOMMANDS)))
        if len(chain) < 2:
            ap.error('--then %s needs at least one argument' % chain[0])
        queries.append((chain[0], chain[1:]))

    os.environ.setdefault('GHIDRA_INSTALL_DIR', DEFAULT_GHIDRA)
    import pyghidra  # pyright: ignore[reportMissingImports]
    pyghidra.start(verbose=False)

    program_path = args.program if args.program.startswith('/') else '/' + args.program
    collected = []

    def out(kind, payload):
        if args.json:
            collected.append({'query': kind, 'result': payload})
        else:
            _print_text(kind, payload)

    project = pyghidra.open_project(args.project, args.project_name, create=False)
    try:
        with pyghidra.program_context(project, program_path) as program:
            for name, qargs in queries:
                HANDLERS[name](program, qargs, out)
    finally:
        project.close()

    if args.json:
        json.dump(collected, sys.stdout, indent=2)
        print()
    return 0


if __name__ == '__main__':
    sys.exit(main(sys.argv[1:]))
