#!/usr/bin/env python3
"""Instrumentation counterpart to `tools/bootcheck.py`.

bootcheck.py emits a single verdict for a run. This tool instead installs
counting hooks at caller-named guest addresses and reports hit counts,
first-hit ordering, and registers at first hit for each one, plus
end-of-run memory watches. It can drive either of two boot paths:

- default (cold boot): boots from reset via `emu.dspboot.run`
  (`resume_from=None`). This is the only mode that can answer "did this
  code EVER run" -- a snapshot already contains whatever happened before
  it, so a resume cannot see anything prior to the checkpoint. `--sdgate
  --esdhc` are available here, and the cold path is in fact the ONLY place
  they can affect the SD bring-up decision: that decision happens at
  roughly 25-30M instructions, before the first snapshot is even rung
  (60M), so installing these models on a resume is a no-op.
- `--resume SNAPSHOT`: the production/GUI hook set, via `emu.longrun.build`
  + `emu.longrun.spin`, resuming from the given `.snap`. This is the only
  path on which the display module actually initialises, so it is the
  mode to use when the question is about display handover, not "did it
  ever run".

Hooks are scoped `UC_HOOK_CODE` (`begin=end=addr`), one per distinct
address, NOT `UC_HOOK_BLOCK` -- block hooks are known in this project to
change outcomes, not just timing (see bootcheck.py's `--profile` warning),
so a single global block hook here would make "did it run" itself
unreliable.

Works with either firmware image without clobbering the shared `sections/`
directory: extraction is cached per-syx under `.coldtrace/<syx name>/`, keyed
by the syx's sha256, entirely separate from `emu.config.sections_dir()`.

`--digest` hashes end-of-run registers plus instruction count (and, on the
resume path, the pit_fired dict, dtim3_fired and vec208, so the digest is
sensitive to the display handover), so you can A/B a hooked run against an
unhooked one and confirm the hooks themselves did not perturb the outcome.

Usage:
    uv run python tools/addrtrace.py --syx Digitone_II_OS1.10E.syx \
        --at 0x4011d67a=sd_bringup --at 0x4011dbdc=sd_gate_set \
        --sym display_start --sym mainloop \
        --watch 0x44459024 --limit 120000000 --json out.json

    uv run python tools/addrtrace.py --syx Digitone_II_OS1.10E.syx \
        --resume snapshots/Digitone_II_OS1.10E/boot280M.snap \
        --sym display_start --json out.json
"""
import argparse, glob, hashlib, json, os, struct, sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from unicorn import UC_HOOK_CODE

from emu import dspboot, extract, snapshot, symbols

REPO_ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

VEC208_SLOT = 0x40000340


def load_main_image(syx_path, sections_dir=None):
    """-> (main_img_bytes, sections_dir). Caches extraction per-syx under
    .coldtrace/<syx basename>/, never touching the shared sections/ dir."""
    if sections_dir is None:
        base = os.path.splitext(os.path.basename(syx_path))[0]
        sections_dir = os.path.join(REPO_ROOT, '.coldtrace', base)

    h = hashlib.sha256()
    with open(syx_path, 'rb') as fh:
        for chunk in iter(lambda: fh.read(1 << 20), b''):
            h.update(chunk)
    digest = h.hexdigest()

    marker = os.path.join(sections_dir, extract.SOURCE_MARKER)
    cached = None
    if os.path.exists(marker):
        with open(marker) as fh:
            if fh.read().strip() == digest:
                hits = glob.glob(os.path.join(sections_dir, 'section_3_*.bin'))
                if hits:
                    cached = hits[0]

    if cached:
        path = cached
    else:
        written = extract.extract(syx_path, sections_dir)
        path = next(p for sid, kind, p, n, dest in written if sid == 3)

    with open(path, 'rb') as fh:
        main_img = fh.read()
    return main_img, sections_dir


def make_target_hook_factory(targets, hits, first_seq, first_regs, seq,
                              first_instr, instr_count=None):
    """Shared hit-counting callback factory, used by both the cold and
    resume paths so they cannot drift from each other.

    instr_count: optional zero-arg callable read at first hit to record
    'when' (in instructions) the hit happened. Never allowed to raise into
    Unicorn -- a failure just leaves first_instr as None."""
    def make_hook(addr):
        def cb(uc, a, size, data):
            hits[addr] += 1
            if first_seq[addr] is None:
                seq[0] += 1
                first_seq[addr] = seq[0]
                first_regs[addr] = {name: uc.reg_read(rid)
                                     for name, rid in snapshot.REGS}
                if instr_count is not None:
                    try:
                        first_instr[addr] = instr_count()
                    except Exception:
                        first_instr[addr] = None
        return cb
    return make_hook


def cold_trace(syx_path, main_img, targets, limit=120_000_000, watches=(),
                sdgate=False, esdhc=False, want_faults=False, want_digest=False):
    """targets: dict {addr: [name, ...]}. Returns a plain JSON-serialisable
    result dict."""
    profile = symbols.resolve(main_img, load_addr=dspboot.MAIN_LOAD)

    seq = [0]
    hits = {addr: 0 for addr in targets}
    first_seq = {addr: None for addr in targets}
    first_regs = {addr: None for addr in targets}
    first_instr = {addr: None for addr in targets}
    box = {}

    def read_n():
        return box.get('st', {}).get('n')

    make_hook = make_target_hook_factory(targets, hits, first_seq, first_regs,
                                          seq, first_instr, instr_count=read_n)

    def pre_start(m):
        for addr in targets:
            m.uc.hook_add(UC_HOOK_CODE, make_hook(addr), begin=addr, end=addr)

    try:
        m, st, stop = dspboot.run(syx_path, main_img, limit=limit,
                                   machine_out=box, pre_start=pre_start,
                                   sdgate=sdgate, esdhc=esdhc)
    except Exception:
        print('dspboot.run raised; hits collected before the exception:',
              file=sys.stderr)
        for addr, names in targets.items():
            for name in names:
                print('  %-28s 0x%08x  hits=%d' % (name, addr, hits[addr]),
                      file=sys.stderr)
        raise

    target_list = []
    for addr, names in targets.items():
        for name in names:
            target_list.append({
                'name': name, 'addr': addr, 'hits': hits[addr],
                'first_seq': first_seq[addr], 'first_regs': first_regs[addr],
                'first_instr': first_instr[addr],
            })

    watch_results = []
    for addr, size in watches:
        try:
            raw = m.uc.mem_read(addr, size)
            value = int.from_bytes(bytes(raw), 'big')
            note = None
        except Exception:
            value = None
            note = 'unmapped'
        watch_results.append({'addr': addr, 'size': size, 'value': value,
                               'note': note})

    result = {
        'mode': 'cold',
        'syx': syx_path,
        'image_bytes': len(main_img),
        'limit': limit,
        'instructions': st['n'],
        'stop': stop,
        'targets': target_list,
        'unresolved': list(profile.unresolved),
        'watches': watch_results,
        'sdgate': sdgate,
        'esdhc': esdhc,
    }

    if want_faults:
        result['faults'] = m.fault_report()

    if want_digest:
        h = hashlib.sha256()
        for name, rid in snapshot.REGS:
            h.update(('%s=%d' % (name, m.uc.reg_read(rid))).encode())
        h.update(('n=%d' % st['n']).encode())
        result['digest'] = h.hexdigest()

    return result


def resume_trace(syx_path, snapshot_path, main_img, targets, watches=(),
                  slc=True, sdgate=False, esdhc=False,
                  post_intro=60_000_000, max_instrs=400_000_000,
                  step=10_000_000, want_faults=False, want_digest=False):
    """targets: dict {addr: [name, ...]}. Returns a plain JSON-serialisable
    result dict, same shape as cold_trace's."""
    from emu.longrun import build, spin, setpixel_count
    from emu.pit import Pits, intro_running, BASES as PIT_BASES
    from emu.dtim import Dtims, Timers, BASES as DTIM_BASES

    profile = symbols.resolve(main_img, load_addr=dspboot.MAIN_LOAD)

    m, ev, st, pc, inq, at = build(snapshot_path, syx=syx_path, unblock=True,
                                    softfloat=True, bitmap=True, dsp=True,
                                    slc=slc, sdgate=sdgate, esdhc=esdhc)
    intro = intro_running(m, profile.intro_pit3_isr)
    pits = Timers(Pits(m, hold=intro), Dtims(m, channels=(3,), hold=intro))

    phase = {"post_intro": not intro}

    if intro and profile.intro_done is not None:
        def handover(uc, a, s_, d):
            pits.release()
            phase["post_intro"] = True
        at(profile.intro_done, handover)

    seq = [0]
    hits = {addr: 0 for addr in targets}
    first_seq = {addr: None for addr in targets}
    first_regs = {addr: None for addr in targets}
    first_instr = {addr: None for addr in targets}
    done_box = [0]
    # On this path the counter is only the loop's own running `done` total,
    # advanced per `spin` chunk -- so first_instr here is accurate to the
    # `--step` size, not exact, unlike the cold path's exact per-instruction
    # count.
    make_hook = make_target_hook_factory(targets, hits, first_seq, first_regs,
                                          seq, first_instr,
                                          instr_count=lambda: done_box[0])

    for addr in targets:
        at(addr, make_hook(addr))

    total, post_at, done, pc_ = max_instrs, None, 0, pc
    stop = "limit"
    while done < total:
        pc_, executed, stop = spin(m, pc_, step, pits=pits)
        done += executed
        done_box[0] = done
        if phase["post_intro"] and post_at is None:
            post_at = done
        if post_at is not None and done - post_at >= post_intro:
            break
        if stop != "limit":
            break

    target_list = []
    for addr, names in targets.items():
        for name in names:
            target_list.append({
                'name': name, 'addr': addr, 'hits': hits[addr],
                'first_seq': first_seq[addr], 'first_regs': first_regs[addr],
                'first_instr': first_instr[addr],
            })

    watch_results = []
    for addr, size in watches:
        try:
            raw = m.uc.mem_read(addr, size)
            value = int.from_bytes(bytes(raw), 'big')
            note = None
        except Exception:
            value = None
            note = 'unmapped'
        watch_results.append({'addr': addr, 'size': size, 'value': value,
                               'note': note})

    fired = pits.fired
    vec208 = struct.unpack('>I', m.uc.mem_read(VEC208_SLOT, 4))[0]

    result = {
        'mode': 'resume',
        'syx': syx_path,
        'image_bytes': len(main_img),
        'limit': max_instrs,
        'instructions': done,
        'stop': stop,
        'targets': target_list,
        'unresolved': list(profile.unresolved),
        'watches': watch_results,
        'post_intro_instrs': None if post_at is None else done - post_at,
        'pit_fired': {("PIT%d" % c): fired.get("PIT%d" % c, 0)
                      for c in (0, 2, 3)},
        'dtim3_fired': fired.get("DTIM3", 0),
        'vec208': vec208,
        'vec208_is_intro_isr': vec208 == profile.intro_pit3_isr,
        'tasks_created': len(ev["tasks"]),
        'distinct_tasks_scheduled': len(ev["switch"]),
        'setpixel': setpixel_count(ev),
        'sdgate': sdgate,
        'esdhc': esdhc,
    }

    if want_faults:
        result['faults'] = m.fault_report()

    if want_digest:
        h = hashlib.sha256()
        for name, rid in snapshot.REGS:
            h.update(('%s=%d' % (name, m.uc.reg_read(rid))).encode())
        h.update(('n=%d' % done).encode())
        h.update(json.dumps(result['pit_fired'], sort_keys=True).encode())
        h.update(('dtim3=%d' % result['dtim3_fired']).encode())
        h.update(('vec208=%d' % vec208).encode())
        result['digest'] = h.hexdigest()

    return result


def parse_at(spec):
    if '=' in spec:
        addr_s, name = spec.split('=', 1)
    else:
        addr_s, name = spec, None
    addr = int(addr_s, 0)
    if name is None:
        name = 'addr_0x%08x' % addr
    return addr, name


def parse_watch(spec):
    if ':' in spec:
        addr_s, size_s = spec.split(':', 1)
        size = int(size_s, 0)
    else:
        addr_s, size = spec, 4
    addr = int(addr_s, 0)
    if size not in (1, 2, 4):
        raise SystemExit('--watch size must be 1, 2 or 4: %r' % spec)
    return addr, size


def print_report(result):
    print('=== %s (%d bytes)  limit=%d  instructions=%d  stop=%s ===' % (
        os.path.basename(result['syx']), result['image_bytes'],
        result['limit'], result['instructions'], result['stop']))

    if result['unresolved']:
        print('unresolved symbols: %s' % ', '.join(result['unresolved']))

    if result.get('sdgate') or result.get('esdhc'):
        print('models: sdgate=%s esdhc=%s' % (result.get('sdgate', False),
                                                result.get('esdhc', False)))

    ordered = sorted(result['targets'],
                      key=lambda t: (t['first_seq'] is None, t['first_seq'] or 0))
    print('\n--- targets ---')
    for t in ordered:
        seq = t['first_seq'] if t['first_seq'] is not None else '-'
        instr = t.get('first_instr')
        instr_s = '@{:,}'.format(instr) if instr is not None else '-'
        print('  %-28s 0x%08x  hits=%-8d first_seq=%-6s %s' %
              (t['name'], t['addr'], t['hits'], seq, instr_s))
        if t['first_regs']:
            r = t['first_regs']
            print('      pc=0x%08x sr=0x%04x' % (r['pc'], r['sr']))
            print('      ' + ' '.join('d%d=%08x' % (i, r['d%d' % i]) for i in range(8)))
            print('      ' + ' '.join('a%d=%08x' % (i, r['a%d' % i]) for i in range(8)))

    print('\n--- watch ---')
    for w in result['watches']:
        if w['note']:
            print('  0x%08x  %s' % (w['addr'], w['note']))
        else:
            print('  0x%08x  0x%08x' % (w['addr'], w['value']))

    if result.get('mode') == 'resume':
        print('\n--- display ---')
        fired = result['pit_fired']
        print('  PIT0=%d PIT2=%d PIT3=%d' %
              (fired.get('PIT0', 0), fired.get('PIT2', 0), fired.get('PIT3', 0)))
        print('  DTIM3=%d' % result['dtim3_fired'])
        marker = ' (intro ISR)' if result['vec208_is_intro_isr'] else ''
        print('  vec208=0x%08x%s' % (result['vec208'], marker))
        print('  tasks_created=%d distinct_tasks_scheduled=%d' %
              (result['tasks_created'], result['distinct_tasks_scheduled']))
        print('  post_intro_instrs=%s' % result['post_intro_instrs'])

    if 'faults' in result:
        faults = result['faults']
        print('\n--- faults (%d page(s)) ---' % len(faults))
        for rec in faults[:20]:
            print('  %s' % rec)

    if 'digest' in result:
        print('\ndigest: %s' % result['digest'])


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__,
                                  formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument('--syx', required=True)
    ap.add_argument('--sections-dir')
    ap.add_argument('--at', action='append', default=[])
    ap.add_argument('--sym', action='append', default=[])
    ap.add_argument('--watch', action='append', default=[])
    ap.add_argument('--limit', type=lambda s: int(s, 0), default=120_000_000)
    ap.add_argument('--digest', action='store_true')
    ap.add_argument('--json')
    ap.add_argument('--faults', action='store_true')
    ap.add_argument('--resume', help='resume from this snapshot instead of cold-booting')
    if hasattr(argparse, 'BooleanOptionalAction'):
        ap.add_argument('--slc', dest='slc', default=None,
                         action=argparse.BooleanOptionalAction,
                         help='(resume only) default True')
    else:
        ap.add_argument('--slc', dest='slc', action='store_true', default=None,
                         help='(resume only) default True')
        ap.add_argument('--no-slc', dest='slc', action='store_false',
                         help='(resume only)')
    ap.add_argument('--sdgate', action='store_true', default=False,
                     help='install emu.gpio.SdGate; only affects the SD bring-up '
                          'decision on the cold-boot path (see module docstring)')
    ap.add_argument('--esdhc', action='store_true', default=False,
                     help='install emu.esdhc.Esdhc; only affects the SD bring-up '
                          'decision on the cold-boot path (see module docstring)')
    ap.add_argument('--post-intro', type=lambda s: int(s, 0), default=60_000_000,
                     help='(resume only) instructions to observe after the intro hands over')
    ap.add_argument('--step', type=lambda s: int(s, 0), default=10_000_000,
                     help='(resume only)')
    args = ap.parse_args(argv)

    resume_only_given = (
        args.slc is not None
        or args.post_intro != 60_000_000 or args.step != 10_000_000)
    if resume_only_given and not args.resume:
        print('error: --slc/--post-intro/--step require --resume',
              file=sys.stderr)
        return 2

    main_img, sections_dir = load_main_image(args.syx, args.sections_dir)
    profile = symbols.resolve(main_img, load_addr=dspboot.MAIN_LOAD)

    targets = {}
    for spec in args.at:
        addr, name = parse_at(spec)
        targets.setdefault(addr, []).append(name)

    for name in args.sym:
        addr = getattr(profile, name, None)
        if addr is None:
            print('warning: symbol %r did not resolve' % name, file=sys.stderr)
            continue
        targets.setdefault(addr, []).append(name)

    watches = [parse_watch(spec) for spec in args.watch]

    try:
        if args.resume:
            slc = True if args.slc is None else args.slc
            result = resume_trace(args.syx, args.resume, main_img, targets,
                                   watches=watches, slc=slc, sdgate=args.sdgate,
                                   esdhc=args.esdhc, post_intro=args.post_intro,
                                   step=args.step, max_instrs=args.limit,
                                   want_faults=args.faults,
                                   want_digest=args.digest)
        else:
            result = cold_trace(args.syx, main_img, targets, limit=args.limit,
                                 watches=watches, sdgate=args.sdgate,
                                 esdhc=args.esdhc, want_faults=args.faults,
                                 want_digest=args.digest)
    except Exception:
        return 1

    if args.json:
        with open(args.json, 'w') as fh:
            json.dump(result, fh, indent=2)

    print_report(result)
    return 0


if __name__ == '__main__':
    sys.exit(main())
