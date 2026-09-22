#!/usr/bin/env python3
"""Which MMIO addresses does the firmware touch *periodically* once the OS is up?

`tools/addrtrace.py` counts hits at guest *code* addresses. This is its
memory-side counterpart, and it exists to answer one question: where is the
front-panel scan? There is no key, encoder or button support in this emulator,
and the bus the panel hangs off has never been identified.

Totals cannot answer it. A one-shot init burst and a scan loop both show up as
"this address was touched". What separates them is *recurrence*: a panel scan
runs forever at a fixed rate, so it appears in **every** observation window at
a steady count. So the run is chopped into fixed instruction-count chunks and
every address is reported per chunk. The columns that matter are `chunks`
(how many windows touched it at all) and `min/med/max` (how evenly), not
`hits`. `steady` marks an address present in every window.

Only the chunks *after the intro hands over* are counted; the intro's own
traffic is discarded, so the measured window is the main OS in both builds.

Hooks are range-scoped `UC_HOOK_MEM_READ`/`UC_HOOK_MEM_WRITE` installed through
`Machine.install_mmio_trace`, the same mechanism `SdGate`, `Esdhc` and the UART8
model already use. The documented perturbation hazard in this project is for
*global* `UC_HOOK_CODE`/`UC_HOOK_BLOCK` hooks (see `bootcheck.py --profile`);
the default ranges here are the two ColdFire peripheral windows, so the
callbacks fire on peripheral traffic only and never on RAM.

The `first_pc`/`pcs` columns are the point of the whole exercise: they are the
code addresses to hand to Ghidra.

Usage:
    uv run python tools/mmiotrace.py --syx Digitone_II_OS1.10E.syx \
        --snapshot snapshots/Digitone_II_OS1.10E/sdboot280M.snap \
        --json out/mmio-dn2.json

    uv run python tools/mmiotrace.py --syx Digitakt_II_OS1.15C.syx \
        --snapshot snapshots/boot280M.snap --chunks 12 --chunk 5000000
"""
import argparse
import collections
import json
import os
import statistics
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from addrtrace import load_main_image

from emu import symbols
from emu.dtim import Dtims, Timers
from emu.longrun import build, spin
from emu.pit import Pits, intro_running

# The two ColdFire peripheral windows. Wide enough that nothing on the panel
# bus can hide, narrow enough that RAM traffic never enters a callback.
DEFAULT_RANGES = ((0xEC000000, 0xEC0FFFFF), (0xFC000000, 0xFC0FFFFF))

# Bases taken from the models that already own them, not from a datasheet
# reading: emu/console.py, emu/gpio.py, emu/edma.py, emu/pit.py, emu/dtim.py,
# emu/esdhc.py, plus docs/findings/07-emulator.md's peripheral map. An address with no
# name here is unclaimed by any existing model, which is exactly what a panel
# controller should look like.
PERIPHERALS = (
    (0xEC070000, 0x040, 'UART8'),      # service console
    (0xEC074000, 0x040, 'UART9'),      # MIDI DIN, 31250 baud
    (0xEC094000, 0x100, 'GPIO'),
    (0xFC044000, 0x1000, 'eDMA'),
    (0xFC045000, 0x1000, 'eDMA_TCD'),
    (0xFC048000, 0x1000, 'INTC0'),
    (0xFC04C000, 0x1000, 'INTC1'),
    (0xFC050000, 0x1000, 'INTC2'),
    (0xFC05C000, 0x100, 'DSPI0'),      # NOR flash
    (0xFC070000, 0x100, 'DTIM0'),
    (0xFC074000, 0x100, 'DTIM1'),
    (0xFC078000, 0x100, 'DTIM2'),
    (0xFC07C000, 0x100, 'DTIM3'),
    (0xFC080000, 0x100, 'PIT0'),
    (0xFC084000, 0x100, 'PIT1'),
    (0xFC088000, 0x100, 'PIT2'),
    (0xFC08C000, 0x100, 'PIT3'),
    (0xFC090000, 0x100, 'EPORT'),
    (0xFC0CC000, 0x100, 'eSDHC'),
)


def name_for(addr):
    """-> 'PERIPHERAL+0xNN', or None if no existing model claims this address."""
    for base, size, name in PERIPHERALS:
        if base <= addr < base + size:
            return '%s+0x%02x' % (name, addr - base)
    return None


class CountingSink:
    """An `install_mmio_trace` sink that counts instead of writing JSONL.

    `JsonlMmioTrace` flushes a line per event, which is far too slow for a
    whole peripheral window over tens of millions of instructions. This keeps
    counters in memory and snapshots them on `cut()`, once per spin chunk.
    """

    def __init__(self, max_pcs=8):
        self.cur = collections.Counter()
        self.buckets = []
        self.pcs = collections.defaultdict(set)
        self.first_pc = {}
        self.max_pcs = max_pcs
        self.events = 0

    def event(self, *, pc, address, width, direction, value,
              register=None, instruction_count=None, read_phase=None):
        key = (address, direction)
        self.cur[key] += 1
        self.events += 1
        if key not in self.first_pc:
            self.first_pc[key] = pc
        seen = self.pcs[key]
        if len(seen) < self.max_pcs:
            seen.add(pc)

    def cut(self):
        """End the current observation window and start a new one."""
        self.buckets.append(self.cur)
        self.cur = collections.Counter()

    def drop(self):
        """Discard the current window (used for the pre-handover intro)."""
        self.cur = collections.Counter()

    def close(self):
        pass


def rows_from(sink):
    """-> one report row per (address, direction), most recurrent first."""
    n = len(sink.buckets)
    keys = set()
    for bucket in sink.buckets:
        keys.update(bucket)
    rows = []
    for key in keys:
        addr, direction = key
        counts = [bucket.get(key, 0) for bucket in sink.buckets]
        present = sum(1 for c in counts if c)
        rows.append({
            'address': '0x%08x' % addr,
            'direction': direction,
            'peripheral': name_for(addr),
            'hits': sum(counts),
            'chunks_present': present,
            'chunks': n,
            'steady': present == n and n > 1,
            'per_chunk_min': min(counts),
            'per_chunk_median': int(statistics.median(counts)),
            'per_chunk_max': max(counts),
            'first_pc': '0x%08x' % sink.first_pc[key],
            'pcs': sorted('0x%08x' % p for p in sink.pcs[key]),
        })
    rows.sort(key=lambda r: (r['chunks_present'], r['hits']), reverse=True)
    return rows


def run(args):
    main_img, _ = load_main_image(args.syx)
    profile = symbols.resolve(main_img)
    sink = CountingSink()

    m, ev, st, pc, inq, at = build(
        args.snapshot, syx=args.syx, unblock=True, softfloat=True,
        bitmap=True, dsp=True, slc=args.slc,
        sdgate=args.sdgate, esdhc=args.esdhc,
        trace=sink, trace_ranges=tuple(args.range))

    intro = intro_running(m, profile.intro_pit3_isr)
    pits = Timers(Pits(m, hold=intro), Dtims(m, channels=(3,), hold=intro))
    phase = {'post_intro': not intro}

    if intro and profile.intro_done is not None:
        def handover(uc, a, size, data):
            pits.release()
            phase['post_intro'] = True
        at(profile.intro_done, handover)

    done, collected, pc_, stop = 0, 0, pc, 'limit'
    t0 = time.time()
    while collected < args.chunks and done < args.max_instrs:
        pc_, executed, stop = spin(m, pc_, args.chunk, pits=pits)
        done += executed
        if phase['post_intro']:
            sink.cut()
            collected += 1
        else:
            sink.drop()
        if stop != 'limit':
            break

    return {
        'syx': args.syx,
        'snapshot': args.snapshot,
        'ranges': ['0x%08x-0x%08x' % r for r in args.range],
        'instrs': done,
        'chunk': args.chunk,
        'chunks_collected': collected,
        'intro_live_at_restore': bool(intro),
        'reached_post_intro': phase['post_intro'],
        'stop': stop,
        'events': sink.events,
        'elapsed_s': round(time.time() - t0, 1),
        'rows': rows_from(sink),
    }


def print_report(report, top):
    print(json.dumps({k: v for k, v in report.items() if k != 'rows'}, indent=2))
    if not report['reached_post_intro']:
        print('\n*** the intro never handed over -- no main-OS window was '
              'observed; raise --max-instrs ***')
    rows = report['rows']
    steady = [r for r in rows if r['steady']]
    unclaimed = [r for r in steady if r['peripheral'] is None]

    def table(title, subset):
        print('\n--- %s (%d) ---' % (title, len(subset)))
        print('%-12s %-6s %-16s %9s %7s %7s %7s %7s  %s'
              % ('address', 'dir', 'peripheral', 'hits', 'chunks',
                 'min', 'med', 'max', 'first_pc'))
        for r in subset[:top]:
            print('%-12s %-6s %-16s %9d %3d/%-3d %7d %7d %7d  %s'
                  % (r['address'], r['direction'], r['peripheral'] or '-',
                     r['hits'], r['chunks_present'], r['chunks'],
                     r['per_chunk_min'], r['per_chunk_median'],
                     r['per_chunk_max'], r['first_pc']))

    table('UNCLAIMED and steady -- panel candidates', unclaimed)
    table('all steady addresses', steady)
    table('everything touched', rows)


def parse_range(spec):
    lo, _, hi = spec.partition('-')
    if not hi:
        raise argparse.ArgumentTypeError(
            'range must be LO-HI, e.g. 0xEC000000-0xEC0FFFFF')
    return (int(lo, 0), int(hi, 0))


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument('--syx', required=True)
    ap.add_argument('--snapshot', required=True)
    ap.add_argument('--range', type=parse_range, action='append',
                    help='LO-HI peripheral window to observe; repeatable. '
                         'Defaults to the two ColdFire windows.')
    ap.add_argument('--chunk', type=int, default=5_000_000,
                    help='instructions per observation window')
    ap.add_argument('--chunks', type=int, default=10,
                    help='how many post-intro windows to collect')
    ap.add_argument('--max-instrs', type=int, default=400_000_000)
    ap.add_argument('--top', type=int, default=40)
    ap.add_argument('--slc', action='store_true', default=True)
    ap.add_argument('--sdgate', dest='sdgate', action='store_true', default=True)
    ap.add_argument('--no-sdgate', dest='sdgate', action='store_false')
    ap.add_argument('--esdhc', dest='esdhc', action='store_true', default=True)
    ap.add_argument('--no-esdhc', dest='esdhc', action='store_false')
    ap.add_argument('--json', help='write the full report here')
    args = ap.parse_args(argv)
    if not args.range:
        args.range = list(DEFAULT_RANGES)

    report = run(args)
    if args.json:
        os.makedirs(os.path.dirname(os.path.abspath(args.json)), exist_ok=True)
        json.dump(report, open(args.json, 'w'), indent=2)
    print_report(report, args.top)
    return 0 if report['reached_post_intro'] else 1


if __name__ == '__main__':
    sys.exit(main())
