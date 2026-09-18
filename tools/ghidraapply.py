"""Apply tools/codeseeds.py or tools/rttiscan.py output to a Ghidra project.

    uv run python tools/ghidraapply.py seeds out/symbols/dt2-1.15C-seeds.json
        [--project ~/ghidra-projects/dt2-emac] [--project-name dt2-emac]
        [--program /section_3_MAIN_OS.bin] [--analyze] [--dry-run] [--report OUT]
    uv run python tools/ghidraapply.py rtti out/symbols/dt2-1.15C-rtti.json [same options]

seeds: creates a function at each vector handler, then at each call target.
A target is skipped when a function already starts there, when it is inside
defined data or inside another instruction, or (for calls) when every call
site is inside defined data. Each target gets its own transaction, rolled
back if Ghidra adds an Error bookmark or makes no function. A handler that
still has Ghidra's default name becomes vector_<n>_handler. --analyze runs
auto-analysis once at the end, so the callees of new code get functions too.

rtti: labels class typeinfo objects, their name strings and vtables inside
the class namespace, then names functions that still have Ghidra's default
name, in this order:
  Class::method     the only function that loads a "Class::method" string;
  Class::vfunc_N    a vtable slot function, under the most basic class that
                    has it in that slot (skipped when several classes own it);
  Class::ctor_dtor  a function that loads a vtable address: the class of the
                    last such load in the function; a comment lists the rest.
A missing slot function is created as in seeds. Names that Ghidra did not
generate are never changed.

--dry-run counts what would change and writes nothing. The project must not
be open in another Ghidra process.
"""

import argparse
import json
import os
import sys

# tools/ghidra/ is a plain directory of Java scripts that shadows the real
# `ghidra` Java-bridge namespace PyGhidra needs, once tools/ lands on
# sys.path -- which it does when this is run as `python tools/ghidraapply.py`.
_here = os.path.dirname(os.path.abspath(__file__))
sys.path[:] = [p for p in sys.path if os.path.abspath(p or '.') != _here]

DEFAULT_GHIDRA = '/opt/homebrew/Cellar/ghidra/12.1.3/libexec'
DEFAULT_PROJECT = os.path.expanduser('~/ghidra-projects/dt2-emac')
DEFAULT_PROJECT_NAME = 'dt2-emac'
DEFAULT_PROGRAM = '/section_3_MAIN_OS.bin'
CLASS_KINDS = ('class', 'si_class', 'vmi_class')


def split_name(qualified):
    """Split A::B<C::D>::f at each :: outside template brackets."""
    parts, cur, depth, i = [], '', 0, 0
    while i < len(qualified):
        if depth == 0 and qualified.startswith('::', i):
            parts.append(cur)
            cur, i = '', i + 2
            continue
        c = qualified[i]
        depth += (c == '<') - (c == '>')
        cur += c
        i += 1
    parts.append(cur)
    return [p.replace(' ', '_') for p in parts]


class Applier:
    def __init__(self, program, dry_run):
        from ghidra.util.task import TaskMonitor
        self.program = program
        self.dry_run = dry_run
        self.monitor = TaskMonitor.DUMMY
        self.fm = program.getFunctionManager()
        self.listing = program.getListing()
        self.symtab = program.getSymbolTable()
        self.space = program.getAddressFactory().getDefaultAddressSpace()
        self.bookmarks = program.getBookmarkManager()
        self.stats = {}
        self.errors = []

    def addr(self, value):
        return self.space.getAddress(value)

    def count(self, key):
        self.stats[key] = self.stats.get(key, 0) + 1

    def edit(self, label, fn):
        """Run fn in its own transaction and keep it only if fn returns True."""
        tx = self.program.startTransaction(label)
        ok = False
        try:
            ok = bool(fn())
        except Exception as e:  # a Java exception from Ghidra, recorded per edit
            self.errors.append('%s: %s' % (label, e))
        finally:
            self.program.endTransaction(tx, ok)
        return ok

    def namespace(self, parts):
        """Create or find the namespace path; the last part is a class."""
        from ghidra.program.model.symbol import SourceType
        ns = self.program.getGlobalNamespace()
        for i, part in enumerate(parts):
            found = self.symtab.getNamespace(part, ns)
            if found is None:
                if i == len(parts) - 1:
                    found = self.symtab.createClass(ns, part, SourceType.ANALYSIS)
                else:
                    found = self.symtab.createNameSpace(ns, part, SourceType.ANALYSIS)
            ns = found
        return ns

    def label(self, value, parts):
        from ghidra.program.model.symbol import SourceType
        self.symtab.createLabel(self.addr(value), parts[-1], self.namespace(parts[:-1]),
                                SourceType.ANALYSIS)

    def ensure_function(self, value, sites=None):
        """-> the Function at value, creating it when the checks pass, else None."""
        from ghidra.app.cmd.disassemble import DisassembleCommand
        from ghidra.app.cmd.function import CreateFunctionCmd
        a = self.addr(value)
        func = self.fm.getFunctionAt(a)
        if func is not None:
            self.count('function exists')
            return func
        if self.listing.getDefinedDataContaining(a) is not None:
            self.count('skip: target in data')
            return None
        ins = self.listing.getInstructionContaining(a)
        if ins is not None and ins.getAddress() != a:
            self.count('skip: target inside an instruction')
            return None
        if sites is not None and all(
                self.listing.getDefinedDataContaining(self.addr(s)) is not None for s in sites):
            self.count('skip: every site in data')
            return None
        if self.dry_run:
            self.count('would create function')
            return None

        def create():
            before = self.bookmarks.getBookmarkCount('Error')
            if ins is None:
                DisassembleCommand(a, None, True).applyTo(self.program, self.monitor)
            CreateFunctionCmd(a).applyTo(self.program, self.monitor)
            return (self.fm.getFunctionAt(a) is not None
                    and self.bookmarks.getBookmarkCount('Error') == before)

        if self.edit('create function %#x' % value, create):
            self.count('created function')
            return self.fm.getFunctionAt(a)
        self.count('rolled back: error bookmark or no function')
        return None

    def rename(self, func, parts, comment=None):
        from ghidra.program.model.symbol import SourceType
        from ghidra.util.exception import DuplicateNameException
        sym = func.getSymbol()
        if sym.getSource() != SourceType.DEFAULT:
            self.count('keep: name not default')
            return False
        if self.dry_run:
            self.count('would rename')
            return False

        def do():
            ns = self.namespace(parts[:-1])
            try:
                sym.setNameAndNamespace(parts[-1], ns, SourceType.ANALYSIS)
            except DuplicateNameException:
                sym.setNameAndNamespace('%s_%x' % (parts[-1], func.getEntryPoint().getOffset()),
                                        ns, SourceType.ANALYSIS)
            if comment:
                func.setComment(comment)
            return True

        ok = self.edit('rename %s' % '::'.join(parts), do)
        self.count('renamed' if ok else 'rename failed')
        return ok


def apply_seeds(ap, data):
    installs = {}
    for v in data['vectors']:
        installs.setdefault(v['handler'], []).append(v)
    for handler, vs in sorted(installs.items()):
        func = ap.ensure_function(handler)
        if func is not None:
            vectors = sorted({v['vector'] for v in vs})
            comment = 'written to the vector table: %s' % ', '.join(
                'vector %d at %#x' % (v['vector'], v['site']) for v in vs)
            ap.rename(func, ['vector_%d_handler' % vectors[0]], comment)
    sites = {}
    for c in data['calls']:
        sites.setdefault(c['target'], []).append(c['site'])
    for target, ss in sorted(sites.items()):
        ap.ensure_function(target, ss)
    # Addresses only ever taken as a pointer. Ghidra records the data
    # reference and stops there, so nothing creates a function unless we do.
    # Seed files written before this existed have no 'pointers' key.
    psites = {}
    for p in data.get('pointers', []):
        psites.setdefault(p['target'], []).append(p['site'])
    for target, ss in sorted(psites.items()):
        ap.ensure_function(target, ss)


def apply_rtti(ap, data):
    if not ap.dry_run:
        def labels():
            for ti in data['typeinfos']:
                if ti['kind'] in CLASS_KINDS:
                    parts = split_name(ti['name'])
                    ap.label(ti['addr'], parts + ['typeinfo'])
                    ap.label(ti['name_addr'], parts + ['typeinfo_name'])
            for vt in data['vtables']:
                suffix = 'vtable' if vt['offset_to_top'] == 0 else 'vtable_%d' % -vt['offset_to_top']
                ap.label(vt['addr'], split_name(vt['class']) + [suffix])
            return True
        ap.edit('rtti labels', labels)

    for s in data['qualified_strings']:
        funcs = {}
        for site in s['sites']:
            f = ap.fm.getFunctionContaining(ap.addr(site))
            if f is not None:
                funcs[f.getEntryPoint().getOffset()] = f
        if len(funcs) != 1:
            ap.count('method string: %s functions' % ('no' if not funcs else 'several'))
            continue
        ap.rename(next(iter(funcs.values())), split_name(s['text']))

    owners = {}
    for vt in data['vtables']:
        for i, (slot, owner) in enumerate(zip(vt['slots'], vt['owners'])):
            owners.setdefault(slot, {}).setdefault(owner, i)
    for slot, by_owner in sorted(owners.items()):
        if len(by_owner) > 1:
            ap.count('vfunc: owned by several classes')
            continue
        owner, index = next(iter(by_owner.items()))
        func = ap.ensure_function(slot)
        if func is not None:
            ap.rename(func, split_name(owner) + ['vfunc_%d' % index])

    loads = {}
    for ref in data['vtable_refs']:
        f = ap.fm.getFunctionContaining(ap.addr(ref['site']))
        if f is None:
            ap.count('vtable load outside a function')
            continue
        loads.setdefault(f.getEntryPoint().getOffset(), (f, []))[1].append(
            (ref['site'], ref['class']))
    for entry, (f, refs) in sorted(loads.items()):
        classes = [c for _, c in sorted(refs)]
        comment = None
        if len(set(classes)) > 1:
            comment = 'loads the vtables of %s' % ', '.join(dict.fromkeys(classes))
        ap.rename(f, split_name(classes[-1]) + ['ctor_dtor'], comment)


def analyze(ap):
    from ghidra.app.plugin.core.analysis import AutoAnalysisManager

    def run():
        mgr = AutoAnalysisManager.getAnalysisManager(ap.program)
        mgr.initializeOptions()
        mgr.reAnalyzeAll(None)
        mgr.startAnalysis(ap.monitor)
        return True

    ap.edit('auto-analysis', run)


def totals(ap):
    return {'functions': ap.fm.getFunctionCount(),
            'error_bookmarks': ap.bookmarks.getBookmarkCount('Error')}


def parse_args(argv=None):
    p = argparse.ArgumentParser(description='Apply codeseeds or rttiscan output to Ghidra.')
    p.add_argument('kind', choices=('seeds', 'rtti'))
    p.add_argument('path')
    p.add_argument('--project', default=DEFAULT_PROJECT)
    p.add_argument('--project-name', default=DEFAULT_PROJECT_NAME)
    p.add_argument('--program', default=DEFAULT_PROGRAM)
    p.add_argument('--analyze', action='store_true')
    p.add_argument('--dry-run', action='store_true')
    p.add_argument('--report')
    return p.parse_args(argv)


def main(argv=None):
    args = parse_args(argv)
    with open(args.path) as f:
        data = json.load(f)
    os.environ.setdefault('GHIDRA_INSTALL_DIR', DEFAULT_GHIDRA)
    import pyghidra
    pyghidra.start(verbose=False)
    project = pyghidra.open_project(args.project, args.project_name, create=False)
    try:
        with pyghidra.program_context(project, args.program) as program:
            ap = Applier(program, args.dry_run)
            before = totals(ap)
            (apply_seeds if args.kind == 'seeds' else apply_rtti)(ap, data)
            if args.analyze and not args.dry_run:
                analyze(ap)
            after = totals(ap)
            if not args.dry_run:
                program.getDomainFile().save(pyghidra.task_monitor())
    finally:
        project.close()
    report = {'kind': args.kind, 'input': args.path, 'dry_run': args.dry_run,
              'before': before, 'after': after, 'stats': ap.stats, 'errors': ap.errors}
    print('before %s, after %s' % (before, after))
    for key, n in sorted(ap.stats.items()):
        print('  %6d  %s' % (n, key))
    if ap.errors:
        print('%d errors, first: %s' % (len(ap.errors), ap.errors[0]))
    if args.report:
        os.makedirs(os.path.dirname(os.path.abspath(args.report)), exist_ok=True)
        with open(args.report, 'w') as f:
            json.dump(report, f, indent=1)
    return 0


if __name__ == '__main__':
    sys.exit(main())
