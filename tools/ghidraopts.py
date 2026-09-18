"""List and set Ghidra's per-analyzer options, then re-analyze.

Nothing else in tools/ can turn a named analyzer on or off: ghidraapply.py's
--analyze calls reAnalyzeAll with whatever options the program already has.
This tool exposes the options themselves, so a question like "does Ghidra's own
Function Start Search find the functions that are only ever referenced as a
stored pointer?" can be answered by measurement instead of by writing a
seeding heuristic first.

It writes to the program, so point it at a COPY made with tools/ghidracopy.py,
never at the analysed original. It refuses to save unless --save is given.

    # what analyzers exist, and what they are set to
    uv run python tools/ghidraopts.py --project ~/ghidra-projects/elektron-emac \
      --project-name elektron-emac --program /dt2-1.16-seedtest/section_3_MAIN_OS.bin --list

    # turn some on, re-analyze, keep the result
    uv run python tools/ghidraopts.py --project ... --project-name ... --program ... \
      --set 'Aggressive Instruction Finder=true' \
      --set 'Function Start Search=true' --analyze --save

Function and Error-bookmark counts are printed before and after, so the effect
of a change is visible without a full tools/ghidradump.py re-run.
"""

import argparse
import json
import os
import sys

# tools/ghidra/ is a plain directory of Java scripts that shadows the real
# `ghidra` Java-bridge namespace PyGhidra needs, once tools/ lands on
# sys.path -- which it does when this is run as `python tools/ghidraopts.py`.
_here = os.path.dirname(os.path.abspath(__file__))
sys.path[:] = [p for p in sys.path if os.path.abspath(p or '.') != _here]

DEFAULT_GHIDRA = '/opt/homebrew/Cellar/ghidra/12.1.3/libexec'
DEFAULT_PROJECT = os.path.expanduser('~/ghidra-projects/elektron-emac')
DEFAULT_PROJECT_NAME = 'elektron-emac'
DEFAULT_PROGRAM = '/dt2-1.16/section_3_MAIN_OS.bin'

TRUE = ('true', 'yes', 'on', '1')
FALSE = ('false', 'no', 'off', '0')


def parse_set(text):
    """'Name=true' -> ('Name', True). Only booleans; other types are rejected."""
    if '=' not in text:
        raise SystemExit('--set wants NAME=VALUE, got %r' % text)
    name, _, value = text.partition('=')
    v = value.strip().lower()
    if v in TRUE:
        return name.strip(), True
    if v in FALSE:
        return name.strip(), False
    raise SystemExit('--set %r: value must be one of %s'
                     % (text, ', '.join(TRUE + FALSE)))


def totals(program):
    return {'functions': program.getFunctionManager().getFunctionCount(),
            'error_bookmarks': program.getBookmarkManager().getBookmarkCount('Error')}


def option_rows(options):
    """-> [(name, type, value)] for every analyzer option, sorted by name."""
    rows = []
    for name in options.getOptionNames():
        try:
            kind = str(options.getType(name))
        except Exception as e:  # an option with no registered type
            kind = 'UNKNOWN(%s)' % e
        try:
            value = options.getObject(name, None)
        except Exception as e:
            value = 'UNREADABLE(%s)' % e
        rows.append((str(name), kind, value))
    return sorted(rows)


def main(argv=None):
    p = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument('--project', default=DEFAULT_PROJECT)
    p.add_argument('--project-name', default=DEFAULT_PROJECT_NAME)
    p.add_argument('--program', default=DEFAULT_PROGRAM)
    p.add_argument('--list', action='store_true',
                   help='print every analyzer option and its value, then stop')
    p.add_argument('--set', action='append', default=[], metavar='NAME=BOOL',
                   help='set one boolean analyzer option; repeatable')
    p.add_argument('--analyze', action='store_true',
                   help='re-run auto-analysis after applying --set')
    p.add_argument('--save', action='store_true',
                   help='save the program; without it nothing is kept')
    p.add_argument('--json', help='write the before/after report here')
    args = p.parse_args(argv)

    changes = [parse_set(s) for s in args.set]
    if not args.list and not changes and not args.analyze:
        raise SystemExit('nothing to do: pass --list, --set or --analyze')

    os.environ.setdefault('GHIDRA_INSTALL_DIR', DEFAULT_GHIDRA)
    import pyghidra
    pyghidra.start(verbose=False)

    from ghidra.program.model.listing import Program

    report = {'project': args.project, 'program': args.program,
              'set': [{'name': n, 'value': v} for n, v in changes],
              'analyze': args.analyze, 'saved': False}
    project = pyghidra.open_project(args.project, args.project_name, create=False)
    try:
        with pyghidra.program_context(project, args.program) as program:
            # Analyzer options are registered into the "Analyzers" option
            # group lazily, by each Analyzer.registerOptions(), the first
            # time AutoAnalysisManager is instantiated for this program.
            # Without this, getOptions(ANALYSIS_PROPERTIES) sees only
            # options some other code path already touched (here, just the
            # Decompiler Parameter ID's own option), not the full analyzer
            # list.
            from ghidra.app.plugin.core.analysis import AutoAnalysisManager
            AutoAnalysisManager.getAnalysisManager(program)
            options = program.getOptions(Program.ANALYSIS_PROPERTIES)

            if args.list:
                rows = option_rows(options)
                print('%d analyzer options in %s\n' % (len(rows), args.program))
                for name, kind, value in rows:
                    print('%-58s %-14s %s' % (name, kind, value))
                report['options'] = [{'name': n, 'type': k, 'value': str(v)}
                                     for n, k, v in rows]
                if not changes and not args.analyze:
                    return _finish(report, args)

            before = totals(program)
            print('before %s' % before)

            known = {str(n) for n in options.getOptionNames()}
            missing = [n for n, _ in changes if n not in known]
            if missing:
                raise SystemExit(
                    'no such analyzer option: %s\nRun --list to see the real '
                    'names; they must match exactly.' % ', '.join(missing))

            applied = []
            if changes:
                tx = program.startTransaction('ghidraopts: set analyzer options')
                try:
                    for name, value in changes:
                        was = options.getBoolean(name, False)
                        options.setBoolean(name, value)
                        applied.append({'name': name, 'was': bool(was), 'now': value})
                        print('  %-56s %s -> %s' % (name, was, value))
                    program.endTransaction(tx, True)
                except Exception:
                    program.endTransaction(tx, False)
                    raise
            report['applied'] = applied

            if args.analyze:
                from ghidra.app.plugin.core.analysis import AutoAnalysisManager
                print('re-analyzing...')
                tx = program.startTransaction('ghidraopts: re-analyze')
                try:
                    mgr = AutoAnalysisManager.getAnalysisManager(program)
                    mgr.initializeOptions()
                    mgr.reAnalyzeAll(None)
                    mgr.startAnalysis(pyghidra.task_monitor())
                    program.endTransaction(tx, True)
                except Exception:
                    program.endTransaction(tx, False)
                    raise

            after = totals(program)
            print('after  %s' % after)
            print('delta  functions %+d, error_bookmarks %+d'
                  % (after['functions'] - before['functions'],
                     after['error_bookmarks'] - before['error_bookmarks']))
            report['before'], report['after'] = before, after

            if args.save:
                program.getDomainFile().save(pyghidra.task_monitor())
                report['saved'] = True
                print('saved %s' % args.program)
            else:
                print('NOT saved (pass --save to keep this)')
    finally:
        project.close()

    return _finish(report, args)


def _finish(report, args):
    if args.json:
        os.makedirs(os.path.dirname(os.path.abspath(args.json)), exist_ok=True)
        with open(args.json, 'w') as f:
            json.dump(report, f, indent=2, default=str)
        print('wrote %s' % args.json)
    return 0


if __name__ == '__main__':
    sys.exit(main())
