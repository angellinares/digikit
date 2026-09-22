"""Capture the frame the ColdFire sends to the SHARC, from a snapshot.

    uv run python tools/sharcframe.py SNAPSHOT [--syx SYX] [--passes N]
        [--limit N] [--out-dir out/sharcframe] [--name NAME]
        [--compare FRAME.bin] [--json OUT] [--open-gate] [--poke ADDR=LONG]...

The vector-191 handler builds the frame and calls the DSPI2 driver with
(tx_len, tx, rx_len, rx), but the emulator never raises vector 191
(docs/findings/04-coldfire-dsp-link.md, "The ColdFire tells the SHARC through
a periodic DSPI2 frame"). This tool restores SNAPSHOT, clears the pacing counter, enters the
handler with Machine.raise_vector(191), and runs until the handler's rte
returns to the snapshot PC. A code hook at the driver copies tx_len bytes
from tx and returns to the caller, so DSPI2 and eDMA are never touched. No
timer is created, so no other interrupt enters.

The addresses come from the tools/framelink.py profile of the MAIN OS image
the emulator loads (emu/config.py: DT2_MAIN_IMG, else the one image in
DT2_SECTIONS or sections/), selected by SHA-256; an image without a profile
stops the tool. Digitakt II 1.15C: handler 0x4002d652, driver FUN_400cf9c4,
pacing counter 0x4028ac90. Digitakt II 1.16: handler 0x4002dd0c, driver
FUN_400cd2bc, pacing counter 0x402a1488.

Each pass writes NAME-passN.bin with the TX bytes of the first driver call
(NAME-passN-K.bin for later calls in the same pass). --compare prints the
byte ranges where the pass 0 frame differs from FRAME.bin. Exit status is 0
when every pass returned and reached the driver.

--poke writes a big-endian long into guest memory before the first pass.
--open-gate first writes 0 to the profile's frame-build gate, which opens it
(docs/findings/04-coldfire-dsp-link.md, "The frame capture runs; the frame
build is switched off"): on 1.15C that is --poke 0x4094e4f4=0, on 1.16 --poke 0x409664f4=0.
"""

import argparse
import hashlib
import json
import os
import struct
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from unicorn import UcError  # noqa: E402
from unicorn.m68k_const import UC_M68K_REG_A7, UC_M68K_REG_D0, UC_M68K_REG_PC  # noqa: E402

import framelink  # noqa: E402


def restore(snapshot, syx):
    """-> (m, at): the Machine restored from snapshot, with no timer running."""
    from emu.longrun import build

    # Same feature flags as tools/guirun.py's build() call, minus unblock (this
    # tool drives the run itself and never waits on a semaphore), on_pixel and
    # weakptr (GUI/panel concerns), and slc (no corresponding option here). No
    # Pits/Dtims/Timers are built afterwards: this tool never spins, so
    # nothing would ever tick them, matching the module docstring's "no other
    # interrupt enters".
    m, ev, st, pc, inq, at = build(snapshot, syx=syx, softfloat=True, bitmap=True, dsp=True)
    return m, at


def capture(m, at, prof, passes, limit):
    uc = m.uc
    calls = []

    def driver(uc, addr, size, user):
        sp = uc.reg_read(UC_M68K_REG_A7)
        ret, tx_len, tx, rx_len, rx = struct.unpack('>IIIII', uc.mem_read(sp, 20))
        data = bytes(uc.mem_read(tx, tx_len)) if tx and tx_len else b''
        calls.append({'caller': ret, 'tx_len': tx_len, 'tx': tx,
                      'rx_len': rx_len, 'rx': rx, 'data': data})
        uc.reg_write(UC_M68K_REG_D0, 0)
        uc.reg_write(UC_M68K_REG_A7, sp + 4)
        uc.reg_write(UC_M68K_REG_PC, ret)

    at(prof['driver'], driver)
    resume = uc.reg_read(UC_M68K_REG_PC)
    results = []
    for n in range(passes):
        del calls[:]
        uc.mem_write(prof['counter'], bytes(4))
        uc.reg_write(UC_M68K_REG_PC, resume)
        m.halt_vec = None
        if not m.raise_vector(prof['vector']):
            raise SystemExit('vector %d holds no handler in this snapshot' % prof['vector'])
        handler = uc.reg_read(UC_M68K_REG_PC)
        stop = None
        try:
            uc.emu_start(handler, resume, count=limit)
        except UcError as e:
            stop = 'UcError: %s' % e
        pc = uc.reg_read(UC_M68K_REG_PC)
        if stop is None:
            if pc == resume:
                stop = 'returned'
            elif m.halt_vec is not None:
                stop = 'unhandled vector %d' % m.halt_vec
            else:
                stop = 'limit'
        results.append({'pass': n, 'handler': handler, 'stop': stop, 'pc': pc,
                        'calls': list(calls)})
    return results


def ranges(a, b):
    """Yield (start, end) offsets where a and b differ; extra length is one range."""
    n = min(len(a), len(b))
    start = None
    for i in range(n):
        if a[i] != b[i]:
            if start is None:
                start = i
        elif start is not None:
            yield start, i
            start = None
    if start is not None:
        yield start, n
    if len(a) != len(b):
        yield n, max(len(a), len(b))


def parse_poke(text):
    """'0x4094e4f4=0' -> (0x4094e4f4, b'\\x00\\x00\\x00\\x00')."""
    addr, sep, value = text.partition('=')
    if not sep:
        raise argparse.ArgumentTypeError('want ADDR=LONG, got %r' % text)
    return int(addr, 0), struct.pack('>I', int(value, 0) & 0xFFFFFFFF)


def parse_args(argv=None):
    p = argparse.ArgumentParser(description='Capture the ColdFire-to-SHARC frame from a snapshot.')
    p.add_argument('snapshot')
    p.add_argument('--syx')
    p.add_argument('--passes', type=int, default=1)
    p.add_argument('--limit', type=lambda s: int(s, 0), default=5_000_000,
                   help='instruction limit per pass')
    p.add_argument('--out-dir', default='out/sharcframe')
    p.add_argument('--name')
    p.add_argument('--compare')
    p.add_argument('--json')
    p.add_argument('--open-gate', action='store_true',
                   help="write 0 to the profile's frame-build gate before the first pass")
    p.add_argument('--poke', action='append', default=[], type=parse_poke, metavar='ADDR=LONG',
                   help='write a big-endian long before the first pass; repeatable')
    return p.parse_args(argv)


def main(argv=None):
    args = parse_args(argv)
    name = args.name or os.path.splitext(os.path.basename(args.snapshot))[0]
    from emu import config
    image = config.main_image()
    image_sha256, prof = framelink.profile_for(image)
    print('%s: %s' % (image, prof['name']))
    pokes = ([(prof['gate'], bytes(4))] if args.open_gate else []) + args.poke
    m, at = restore(args.snapshot, args.syx)
    try:
        for addr, data in pokes:
            m.uc.mem_write(addr, data)
            print('poke %#010x = %s' % (addr, data.hex()))
        results = capture(m, at, prof, args.passes, args.limit)
    finally:
        m.close()
    os.makedirs(args.out_dir, exist_ok=True)
    for r in results:
        print('pass %d: handler %#010x, stop %s at %#010x, %d driver call(s)'
              % (r['pass'], r['handler'], r['stop'], r['pc'], len(r['calls'])))
        if r['handler'] != prof['handler']:
            print('  note: vector %d holds %#010x, not the frame handler %#010x'
                  % (prof['vector'], r['handler'], prof['handler']))
        for k, c in enumerate(r['calls']):
            path = os.path.join(args.out_dir, '%s-pass%d%s.bin'
                                % (name, r['pass'], '-%d' % k if k else ''))
            with open(path, 'wb') as f:
                f.write(c['data'])
            c['file'] = path
            c['sha256'] = hashlib.sha256(c.pop('data')).hexdigest()
            print('  caller %#010x: tx %#x bytes at %#010x, rx %#x bytes at %#010x -> %s'
                  % (c['caller'], c['tx_len'], c['tx'], c['rx_len'], c['rx'], path))
    if args.compare and results and results[0]['calls']:
        with open(results[0]['calls'][0]['file'], 'rb') as f:
            a = f.read()
        with open(args.compare, 'rb') as f:
            b = f.read()
        diffs = list(ranges(a, b))
        print('compare with %s: %d ranges differ' % (args.compare, len(diffs)))
        for s, e in diffs:
            print('  +%#06x-+%#06x  %s | %s' % (s, e, a[s:e][:32].hex(), b[s:e][:32].hex()))
    if args.json:
        with open(args.json, 'w') as f:
            json.dump({'snapshot': args.snapshot, 'image': image, 'image_sha256': image_sha256,
                       'profile': prof['name'],
                       'poke': [['%#010x' % a, d.hex()] for a, d in pokes],
                       'results': results}, f, indent=1)
    ok = results and all(r['stop'] == 'returned' and r['calls'] for r in results)
    return 0 if ok else 1


if __name__ == '__main__':
    sys.exit(main())
