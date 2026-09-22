#!/usr/bin/env python3
"""Trustworthy A/B for emulator speed -- replaces the untrusted
out/speed-ab/report.md (HANDOVER-2026-09-22-emulator-and-machines.md, Task A;
docs/findings/07-emulator.md, "What actually limits speed"). That A/B's
"floor" configuration dispatched exceptions in Python, used `count=` (a
~7.6-1.84x tax by itself, measured two different ways in the finding above),
and spent its window idling at one address -- so its number described that
one bug, not the emulator.

Rules this tool follows, to keep the next measurement trustworthy:

  * SAME WORK on both sides of any comparison. Every repeat calls
    `tools.emucheck.setup()` for a fresh Machine from the same snapshot, and
    in `--mode exact` every repeat must execute the same instruction count
    and land on the same PC -- if they don't, this tool says so instead of
    reporting a number (see `check_determinism`).
  * NO `count=` in a number meant to represent throughput. `--mode exact`
    still uses `emu.longrun.spin`'s default counted stepping (`count=` per
    chunk) because pass/fail and determinism work needs exact accounting --
    that IS "exact" mode's cost, and it is reported, not hidden. `--mode
    fast` uses `spin(..., fast=True)` (`_FastStepper`, a block-hook bound
    with no `count=`) for a ceiling that costs exactness instead: the
    instruction count becomes an estimate and the run is not the same
    instruction stream twice, so `--mode fast` never claims determinism.
  * THREE REPEATS, always, because a single run cannot show its own spread.
  * A QUIET MACHINE for numbers that matter. This tool does not enforce
    that -- it can't -- so treat any run made next to another job (an
    emulator run, a build, a Ghidra JVM) as a functional check, not a
    result. Look at `--json`'s `wall_s.spread` before trusting a median.

What each mode measures:

  (default) timing   `--repeats` fresh builds, each run for `--instrs`
                      guest instructions (a floor in `pits` mode -- see
                      `emu.longrun.spin`'s docstring -- so `instrs executed`
                      can slightly exceed `--instrs`). Records wall seconds,
                      instructions executed, final PC, and instr/s per
                      repeat, then the median and spread. No hooks beyond
                      `setup()`'s own -- this is the number the other modes
                      exist to explain, not to instrument further.

  --crossings         One untimed run that counts every Python crossing by
                      kind: it wraps `Uc.hook_add` before `setup()` calls
                      `build()`, so every hook this build ever installs is
                      counted by (hook type, installing function, address)
                      each time its callback actually FIRES -- not how many
                      hooks were registered, but how many times the guest
                      crossed into Python through one. It also wraps
                      `Machine.raise_vector` (interrupt/exception delivery
                      doesn't go through a Unicorn hook at all, and the
                      HANDOVER explicitly calls this out as something the
                      original A/B's hook inventory may have missed) and
                      reads `ev['idle_spins']['n']` for idle-yield passes.
                      See `run_crossings` for the grouping rule.

  --profile           One cProfile run over the same call, split into four
                      buckets by (filename, funcname) -- see `run_profile`'s
                      docstring for the exact rule. This is the same method
                      docs/findings/07-emulator.md's own cProfile finding
                      used (34% Unicorn / ~54% ctypes binding / 12% handlers
                      on a 10M-instruction run); this tool automates the
                      split instead of reading a `pstats` dump by hand.

Examples:
    # Functional check: does the tool run at all? (small budget, on purpose)
    uv run python tools/speedab.py --snapshot snapshots/dt2-1.16/boot400M.snap \\
        --syx Digitakt_II_OS1.16.syx --sections out/sections/dt2-1.16 \\
        --instrs 2000000 --repeats 3 --mode exact

    # The real measurement (quiet machine, default 50M-instruction window):
    uv run python tools/speedab.py --snapshot snapshots/dt2-1.16/boot400M.snap \\
        --syx Digitakt_II_OS1.16.syx --sections out/sections/dt2-1.16 \\
        --json out/speedab/exact.json
    uv run python tools/speedab.py ... --mode fast --json out/speedab/fast.json
    uv run python tools/speedab.py ... --crossings --instrs 50000000
    uv run python tools/speedab.py ... --profile --instrs 50000000
"""
import argparse
import collections
import cProfile
import json
import os
import pstats
import statistics
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from emu.longrun import spin, setpixel_count  # noqa: F401  (setpixel_count: see note below)
from emu.pit import INSTR_PER_SEC
from tools.emucheck import setup

# setpixel_count is imported but not used directly here: `setup()` always
# builds with bitmap=True (see tools/emucheck.py), so any caller of this
# module that wants a setPixel count from `ev` must use it rather than
# `ev['setpixel']` -- see emu/longrun.py's docstring for why.


# ---------------------------------------------------------------- timing ---

def time_one(snapshot, syx, instrs, chunk, fast):
    m, ev, st, pc0, inq, at, pits, profile = setup(snapshot, syx)
    try:
        t0 = time.perf_counter()
        pc, done, stop = spin(m, pc0, instrs, chunk=chunk, pits=pits, fast=fast)
        dt = time.perf_counter() - t0
    finally:
        m.close()
    return {
        'wall_s': dt,
        'instrs': done,
        'final_pc': pc,
        'stop': stop,
        'instr_per_s': (done / dt) if dt > 0 else float('nan'),
        'guest_s': done / INSTR_PER_SEC,
        'real_time_ratio': (done / INSTR_PER_SEC) / dt if dt > 0 else float('nan'),
    }


def check_determinism(repeats):
    """-> (ok, detail). Only meaningful for --mode exact; callers should not
    call this for --mode fast (whose instruction stream is an estimate by
    design -- see this module's docstring)."""
    instrs = {r['instrs'] for r in repeats}
    pcs = {r['final_pc'] for r in repeats}
    stops = {r['stop'] for r in repeats}
    ok = len(instrs) == 1 and len(pcs) == 1 and len(stops) == 1
    detail = ('all %d repeats executed %d instructions and stopped at pc=%#x (%s)'
             % (len(repeats), next(iter(instrs)), next(iter(pcs)), next(iter(stops)))
             if ok else
             'NOT THE SAME WORK: instrs=%r final_pc=%r stop=%r'
             % (sorted(instrs), sorted('%#x' % p for p in pcs), sorted(stops)))
    return ok, detail


def _spread(values):
    return {
        'median': statistics.median(values),
        'min': min(values),
        'max': max(values),
        'stdev': statistics.stdev(values) if len(values) > 1 else 0.0,
    }


def run_timing(snapshot, syx, instrs, repeats, mode, chunk):
    fast = (mode == 'fast')
    reps = [time_one(snapshot, syx, instrs, chunk, fast) for _ in range(repeats)]

    result = {
        'mode': mode,
        'instrs_requested': instrs,
        'repeats': reps,
        'wall_s': _spread([r['wall_s'] for r in reps]),
        'instr_per_s': _spread([r['instr_per_s'] for r in reps]),
        'real_time_ratio': _spread([r['real_time_ratio'] for r in reps]),
    }
    if mode == 'exact':
        ok, detail = check_determinism(reps)
        result['deterministic'] = ok
        result['determinism_detail'] = detail
    else:
        result['deterministic'] = None
        result['determinism_detail'] = ('--mode fast never claims determinism; '
                                        'see this module\'s docstring')
    return result


def print_timing(result):
    print('mode=%s  instrs_requested=%s' % (result['mode'], format(result['instrs_requested'], ',')))
    for i, r in enumerate(result['repeats']):
        print('  repeat %d: %13s instrs in %8.3fs  ->  %.4g instr/s  '
              '(guest %.3fs, real-time ratio %.3g)  final_pc=%#010x  stop=%s'
              % (i, format(r['instrs'], ','), r['wall_s'], r['instr_per_s'],
                 r['guest_s'], r['real_time_ratio'], r['final_pc'], r['stop']))
    w, i, rt = result['wall_s'], result['instr_per_s'], result['real_time_ratio']
    print('  wall_s        median=%.4f  min=%.4f  max=%.4f  stdev=%.4f' % (w['median'], w['min'], w['max'], w['stdev']))
    print('  instr/s       median=%.4g  min=%.4g  max=%.4g  stdev=%.4g' % (i['median'], i['min'], i['max'], i['stdev']))
    print('  real-time x   median=%.4g  min=%.4g  max=%.4g' % (rt['median'], rt['min'], rt['max']))
    if result['deterministic'] is None:
        verdict = 'N/A'
    elif result['deterministic']:
        verdict = 'PASS'
    else:
        verdict = 'FAIL'
    print('  determinism: %s -- %s' % (verdict, result['determinism_detail']))


# ------------------------------------------------------------ crossings ---

def _hook_const_map():
    import unicorn
    return {n: getattr(unicorn, n) for n in dir(unicorn) if n.startswith('UC_HOOK_')}


def _hook_type_name(mask, consts):
    for name, val in consts.items():
        if mask == val:
            return name
    parts = [name for name, val in consts.items() if val and (mask & val) == val]
    return '|'.join(sorted(parts)) if parts else hex(mask)


def _crossing_group(kind, begin, end):
    """The grouping rule for --crossings' summary table.

    Code and INTR/block hooks group by Unicorn hook type directly, with code
    hooks further split by whether the registration was a single address
    (begin == end, the `at()`/`maybe_at()` pattern this project's hooks all
    use) or a wider range/global hook (e.g. `isa='global'`, off by default).

    Memory hooks (read/write/read-after) are grouped by WHO installed them,
    read from the callback's `__qualname__`: a closure defined inside
    `Machine.install_mmio` (but not `install_mmio_trace`, a separate opt-in
    path) is "MMIO"; everything else -- the USR8/UDR8 UART range hook, and
    the dsp/esdhc/edma/gpio narrow device hooks -- is "other". This is a
    name-based rule, not an address-range guess, because narrow device hooks
    are just as address-narrow as MMIO hooks and cannot be told apart by
    begin/end alone.
    """
    if kind == 'UC_HOOK_CODE':
        return 'code hooks (single-address)' if begin == end else 'code hooks (wide/global)'
    if kind in ('UC_HOOK_MEM_READ', 'UC_HOOK_MEM_WRITE', 'UC_HOOK_MEM_READ_AFTER'):
        return 'memory hooks (MMIO)'  # qualname branch applied by the caller
    if kind == 'UC_HOOK_INTR':
        return 'INTR'
    if kind == 'UC_HOOK_BLOCK':
        return 'block hooks (fast mode only)'
    return 'other (%s)' % kind


def run_crossings(snapshot, syx, instrs, chunk, mode):
    """Untimed: counts Python crossings by kind over a fixed guest-instrction
    window. -> dict with 'calls' (Counter keyed by (kind, qualname, begin,
    end)), 'vectors' (Counter keyed by interrupt vector), 'idle_spin_passes',
    'instrs', 'stop'.
    """
    from unicorn.unicorn_py3.unicorn import Uc
    import emu.harness as harness

    consts = _hook_const_map()
    call_counts = collections.Counter()
    orig_hook_add = Uc.hook_add

    def counting_hook_add(self, htype, callback, user_data=None, begin=1, end=0, aux1=0, aux2=0):
        kind = _hook_type_name(htype, consts)
        qualname = getattr(callback, '__qualname__', repr(callback))
        key = (kind, qualname, begin, end)

        def counted(*a, **kw):
            call_counts[key] += 1
            return callback(*a, **kw)
        return orig_hook_add(self, htype, counted, user_data, begin, end, aux1, aux2)

    vector_counts = collections.Counter()
    orig_raise_vector = harness.Machine.raise_vector

    def counting_raise_vector(self, vector, *a, **kw):
        vector_counts[vector] += 1
        return orig_raise_vector(self, vector, *a, **kw)

    Uc.hook_add = counting_hook_add
    harness.Machine.raise_vector = counting_raise_vector
    try:
        m, ev, st, pc, inq, at, pits, profile = setup(snapshot, syx)
        try:
            pc, done, stop = spin(m, pc, instrs, chunk=chunk, pits=pits,
                                  fast=(mode == 'fast'))
            idle_passes = ev['idle_spins']['n']
        finally:
            m.close()
    finally:
        Uc.hook_add = orig_hook_add
        harness.Machine.raise_vector = orig_raise_vector

    return {'instrs': done, 'stop': stop, 'calls': call_counts,
            'vectors': vector_counts, 'idle_spin_passes': idle_passes}


def print_crossings(result):
    print('crossings over %s instructions (stop=%s)'
          % (format(result['instrs'], ','), result['stop']))
    grouped = collections.Counter()
    for (kind, qualname, begin, end), n in result['calls'].items():
        group = _crossing_group(kind, begin, end)
        if group == 'memory hooks (MMIO)' and not (
                'install_mmio' in qualname and 'trace' not in qualname.lower()):
            group = 'memory hooks (other)'
        grouped[group] += n

    print('\nby group:')
    for group, n in grouped.most_common():
        print('  %10s  %s' % (format(n, ','), group))

    print('\nby (hook type, handler, begin address) -- top 20 by call count:')
    for (kind, qualname, begin, end), n in result['calls'].most_common(20):
        addr = ('%#010x-%#010x' % (begin, end)) if end != begin else '%#010x' % begin
        print('  %10s  %-16s %-55s %s' % (format(n, ','), kind, qualname, addr))

    print('\nraise_vector by vector:')
    for vec, n in result['vectors'].most_common():
        print('  %10s  vector %d' % (format(n, ','), vec))

    print('\nidle-spin passes: %s' % format(result['idle_spin_passes'], ','))


# -------------------------------------------------------------- profile ---

def run_profile(snapshot, syx, instrs, chunk, mode):
    """One cProfile run, split into four buckets by (filename, funcname).

    Method: cProfile's `tottime` per function is already exclusive of that
    function's own callees -- a call into Python from a ctypes-invoked hook
    shows up as its own stats entry, so the caller's tottime does not
    include it "for free". That is what makes filename-based bucketing of
    `tottime` meaningful here, matching how the docs/findings/07-emulator.md
    34%/54%/12% split was read from a `pstats` dump by hand:

      unicorn_native            the ONE entry whose function is `emu_start`
                                (Unicorn's own dispatch loop: mostly the
                                ctypes call into the C engine and whatever
                                thin Python wraps it). This is "time inside
                                emu_start excluding Python callbacks" because
                                every callback it invokes is a nested,
                                separately-profiled call.
      unicorn_python_binding    every other function whose file is under
                                site-packages/unicorn (reg_read/reg_write/
                                mem_read and friends -- the per-call
                                allocation and FFI marshalling the finding
                                names), PLUS stdlib `ctypes` frames and `~`
                                built-ins with `ctypes`/`_ctypes` in the
                                name, since those exist only to serve that
                                binding layer and the finding folds them in.
      our_handlers               files under this repo's emu/ or tools/.
      other                     everything left over (struct, collections,
                                cProfile's own bookkeeping, etc).
    """
    import unicorn as _unicorn_mod

    unicorn_dir = os.path.dirname(os.path.abspath(_unicorn_mod.__file__))
    repo_root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    emu_dir = os.path.join(repo_root, 'emu')
    tools_dir = os.path.join(repo_root, 'tools')

    m, ev, st, pc0, inq, at, pits, profile = setup(snapshot, syx)
    prof = cProfile.Profile()
    try:
        prof.enable()
        pc, done, stop = spin(m, pc0, instrs, chunk=chunk, pits=pits,
                              fast=(mode == 'fast'))
        prof.disable()
    finally:
        m.close()

    stats = pstats.Stats(prof)
    buckets = collections.Counter()
    top = collections.defaultdict(list)
    for func, (cc, nc, tt, ct, callers) in stats.stats.items():
        filename, lineno, funcname = func
        if filename.startswith(unicorn_dir) and funcname == 'emu_start':
            bucket = 'unicorn_native'
        elif filename.startswith(unicorn_dir):
            bucket = 'unicorn_python_binding'
        elif 'ctypes' in filename or (filename == '~' and 'ctypes' in funcname):
            bucket = 'unicorn_python_binding'
        elif filename.startswith(emu_dir) or filename.startswith(tools_dir):
            bucket = 'our_handlers'
        else:
            bucket = 'other'
        buckets[bucket] += tt
        top[bucket].append((tt, nc, '%s:%s(%s)' % (filename, lineno, funcname)))

    for b in top:
        top[b].sort(reverse=True)
        top[b] = top[b][:8]

    total = sum(buckets.values()) or 1.0
    return {'instrs': done, 'stop': stop, 'total_tt': total,
            'buckets': dict(buckets), 'fractions': {k: v / total for k, v in buckets.items()},
            'top': {k: v for k, v in top.items()}}


def print_profile(result):
    print('profile over %s instructions (stop=%s), total tottime=%.3fs'
          % (format(result['instrs'], ','), result['stop'], result['total_tt']))
    for bucket, frac in sorted(result['fractions'].items(), key=lambda kv: -kv[1]):
        print('  %6.1f%%  %-24s  %.3fs' % (frac * 100, bucket, result['buckets'][bucket]))
    for bucket, rows in result['top'].items():
        print('\n  top in %s:' % bucket)
        for tt, nc, name in rows:
            print('    %8.4fs  (%8s calls)  %s' % (tt, format(nc, ','), name))


# ------------------------------------------------------------------ CLI ---

def main():
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument('--snapshot', required=True)
    ap.add_argument('--syx', required=True)
    ap.add_argument('--sections', help='sets DT2_SECTIONS for this process')
    ap.add_argument('--instrs', type=int, default=50_000_000,
                    help='guest instruction window (a floor; see spin()); default 50M')
    ap.add_argument('--repeats', type=int, default=3)
    ap.add_argument('--mode', choices=('exact', 'fast'), default='exact')
    ap.add_argument('--chunk', type=int, default=10_000_000)
    ap.add_argument('--crossings', action='store_true',
                    help='run the untimed Python-crossing census instead of timing')
    ap.add_argument('--profile', action='store_true',
                    help='run the untimed cProfile split instead of timing')
    ap.add_argument('--json', help='write the result dict here')
    args = ap.parse_args()
    if args.sections:
        os.environ['DT2_SECTIONS'] = args.sections

    out = {'args': vars(args)}

    if args.crossings:
        r = run_crossings(args.snapshot, args.syx, args.instrs, args.chunk, args.mode)
        print_crossings(r)
        out['crossings'] = {
            'instrs': r['instrs'], 'stop': r['stop'],
            'idle_spin_passes': r['idle_spin_passes'],
            'calls': {'|'.join(map(str, k)): v for k, v in r['calls'].items()},
            'vectors': dict(r['vectors']),
        }
    elif args.profile:
        r = run_profile(args.snapshot, args.syx, args.instrs, args.chunk, args.mode)
        print_profile(r)
        out['profile'] = {k: v for k, v in r.items() if k != 'top'}
    else:
        r = run_timing(args.snapshot, args.syx, args.instrs, args.repeats,
                       args.mode, args.chunk)
        print_timing(r)
        out['timing'] = r

    if args.json:
        os.makedirs(os.path.dirname(args.json) or '.', exist_ok=True)
        with open(args.json, 'w') as fh:
            json.dump(out, fh, indent=2, default=str)
        print('\nwrote %s' % args.json)


if __name__ == '__main__':
    main()
