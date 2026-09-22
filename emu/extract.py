"""Extract a firmware .syx into decompressed sections, with no outside tool.

    uv run python -m emu.extract [firmware.syx] [-o DIR] [--oracle]

Packed sections are decompressed with dt2/elz.py, a byte-level decoder for
the device's aPLib-style codec. It reads every firmware in the repo root,
Digitakt II 1.16 and Digitone II 1.11 included, in about a second.

--oracle uses the device's own routine instead, as a cross-check. The aPLib
depacker sits at 0x80000432 inside section 2 -- which is itself compressed,
so it cannot bootstrap itself. Section 4 is the *updater*, it is stored raw,
and an updater has to unpack the image it installs, so it carries its own
copy of the same routine; --oracle runs that copy under Unicorn. The address
was found by scanning section 4 for function prologues and keeping the one
that reproduced section 2's known first output bytes; `find_depacker` does
that search again if the constant ever stops matching. Its output is
byte-identical to `emu.oracle.depack` -- the device's own section-2 routine --
for every compressed section of both Digitakt II 1.15C and Digitone II 1.10E,
and so is dt2/elz.py's. It fails on the 1.16 and 1.11 updaters
(docs/findings/01-container-and-patching.md, "Scope, and the 2.01
firmwares"). Section 3 takes about a minute this way.

A section's 8-byte header is [u32 compressed_len][u32 sum of those bytes], big
endian, and the sum is checked here. Two sections opt out of it:

    4  updater  header present, sum 0, payload stored raw
    5  meta     15 ASCII bytes of build stamp, no header at all
"""
import glob
import hashlib
import os
import struct
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from emu.harness import Machine, call
from dt2.container import sections
from dt2.elz import depack_section

BOOT_LOAD = 0x80000400      # where the updater image loads
SRC       = 0x50000000      # scratch: the compressed stream
DST       = 0x60000000      # scratch: the decompressed output
DEPACK    = 0x80005710      # the updater's copy of the 0x80000432 routine
OUT_CAP   = 0x800000
SCAN_LIMIT = 20_000_000     # per candidate in find_depacker; a real one needs ~2M

# Prologues worth trying as a depacker entry: link a6, lea -N(a7),a7, and
# movem.l regs,-(a7).
PROLOGUES = (0x4E56, 0x4FEF, 0x48E7)

# The names elektron-firmware-tool uses, so a directory extracted either way
# looks the same to emu/config.py's globs. Section 2 is the bootstrap, not the
# DSP -- that name is the tool's mistake, kept here for compatibility.
NAMES = {2: 'DSP', 3: 'MAIN_OS', 4: 'UPDATER', 5: 'META', 7: 'BLOB'}

# The same file emu/run.py reads; see MARKER there.
SOURCE_MARKER = '.source-sha256'


def classify(stream):
    """-> ('packed'|'raw', bytes)

    Packed only when the 8-byte header is self-consistent: the declared length
    fits, and the bytes after it sum to the declared sum. Anything else is
    stored plainly, with a header to strip (section 4) or without one
    (section 5). Guessing from the section id instead would be a guess; this
    is a check.
    """
    if len(stream) >= 8:
        ln, sm = struct.unpack_from('>II', stream, 0)
        if ln + 8 <= len(stream) and (sum(stream[8:8 + ln]) & 0xFFFFFFFF) == sm:
            return 'packed', stream
        if sm == 0:
            return 'raw', stream[8:]
    return 'raw', stream


def updater_image(c, secs):
    """-> section 4's raw payload, the image that holds the depacker."""
    for sid, off, clen, dest in secs:
        if sid == 4:
            kind, payload = classify(bytes(c[off:off + clen]))
            if kind == 'raw':
                return payload
    raise SystemExit(
        'No raw section 4 in this container, so there is no depacker to\n'
        'borrow from it. Extract with elektron-firmware-tool instead:\n\n'
        '    https://github.com/mischa85/elektron-firmware-tool')


def _machine(img):
    m = Machine()
    m.install_isa_patches()
    m.load(img, BOOT_LOAD)
    return m


def depack(img, stream, at=DEPACK, out_cap=OUT_CAP, limit=2_000_000_000):
    """Run the updater's depacker. `stream` includes its 8-byte header."""
    m = _machine(img)
    for off in range(0, len(stream) + 0x100000, 0x100000):
        m.ensure(SRC + off)
    for off in range(0, out_cap, 0x100000):
        m.ensure(DST + off)
    m.uc.mem_write(SRC, stream)
    n = call(m, at, [SRC, DST], limit=limit)
    if not 0 < n <= out_cap:
        raise ValueError('implausible output length %d' % n)
    return bytes(m.uc.mem_read(DST, n))


def find_depacker(img, stream2):
    """Re-derive the depacker's address by scanning the updater image.

    Only needed if DEPACK stops matching on some other build. A candidate is
    accepted when it decodes section 2 into something longer than the stream
    it came from, whose first word is its own payload length -- the invariant
    that identified the routine originally. Both halves are load-bearing: a
    routine that writes nothing and returns 8 satisfies the first word test on
    its own, because the output is zeros and 0 == 8 - 8. `stream2` is section
    2's compressed stream, header included.

    Each candidate gets SCAN_LIMIT instructions and no more. Most of them are
    not depackers and will happily spin until the full budget is gone, which
    turns a one-minute scan into an hour.
    """
    for off in range(0, len(img) - 2, 2):
        if struct.unpack_from('>H', img, off)[0] not in PROLOGUES:
            continue
        try:
            out = depack(img, stream2, at=BOOT_LOAD + off, limit=SCAN_LIMIT)
        except Exception:
            continue
        if (len(out) > len(stream2)
                and struct.unpack_from('>I', out, 0)[0] == len(out) - 8):
            return BOOT_LOAD + off
    raise SystemExit('No depacker found in the updater image.')


def resolve_depacker(img, stream):
    """DEPACK, or the address a scan finds when this build moved the routine.

    DEPACK was derived on Digitakt II 1.15C and holds on Digitone II 1.10E. On
    Digitone II 1.11 the routine sits at 0x80005720, and the constant fails
    with

        ValueError: implausible output length 0

    which reads as a corrupt firmware rather than a moved symbol, and stops
    extraction on a build the rest of the project handles.

    `find_depacker` was written for exactly this and only needed wiring in. The
    scan runs only when the constant fails, and gets `stream` -- the first
    packed section, normally section 2 and a few kilobytes -- so the probe
    decode that decides it is cheap.
    """
    try:
        depack(img, stream)
    except Exception:
        return find_depacker(img, stream)
    return DEPACK


def extract(syx, outdir, progress=None, oracle=False):
    """Decompress every section of `syx` into `outdir`.

    -> [(id, kind, path, nbytes, dest)], one entry per section. Also records
    which .syx these came from, which emu/run.py checks before pairing them
    with a firmware. `progress(sid, kind, filename)` is called before each
    section is decompressed, because with `oracle` section 3 takes about a
    minute and silence for that long reads as a hang. `oracle` decompresses
    with the updater's own routine under Unicorn instead of dt2/elz.py.
    """
    c, secs = sections(syx)
    img = updater_image(c, secs) if oracle else None
    os.makedirs(outdir, exist_ok=True)
    written = []
    at = None
    for sid, off, clen, dest in secs:
        kind, payload = classify(bytes(c[off:off + clen]))
        name = 'section_%d_%s.bin' % (sid, NAMES.get(sid, 'SECTION'))
        if progress:
            progress(sid, kind, name)
        if kind == 'packed' and not oracle:
            payload = depack_section(payload)
        elif kind == 'packed':
            # Resolved once, from the first packed section, so a build that
            # moved the routine extracts instead of erroring.
            if at is None:
                at = resolve_depacker(img, payload)
            payload = depack(img, payload, at=at)
        # Drop any earlier extraction of this section whatever it was named,
        # so a tool-made directory cannot leave a duplicate behind.
        for stale in glob.glob(os.path.join(outdir, 'section_%d_*.bin' % sid)):
            os.remove(stale)
        path = os.path.join(outdir, name)
        with open(path, 'wb') as fh:
            fh.write(payload)
        written.append((sid, kind, path, len(payload), dest))
    record_source(syx, outdir)
    return written


def record_source(syx, outdir):
    """Stamp `outdir` with the hash of the .syx it came from."""
    h = hashlib.sha256()
    with open(syx, 'rb') as fh:
        for chunk in iter(lambda: fh.read(1 << 20), b''):
            h.update(chunk)
    with open(os.path.join(outdir, SOURCE_MARKER), 'w') as fh:
        fh.write(h.hexdigest() + '\n')


USAGE = """usage: python -m emu.extract [firmware.syx] [-o DIR] [--oracle]

Decompress a firmware .syx into its sections.

  firmware.syx   the firmware to extract; resolved like every other path in
                 this project -- this argument, then DT2_SYX, then the only
                 .syx present (see emu/config.py)
  -o DIR         where to write them; overrides DT2_SECTIONS
  --oracle       decompress with the updater's own routine under Unicorn
                 (slow; 1.15C and 1.10E only) instead of dt2/elz.py
  --help         this"""


def main(argv):
    from emu import config
    if '--help' in argv or '-h' in argv:
        print(USAGE)
        return 0
    outdir = None
    if '-o' in argv:
        i = argv.index('-o')
        if i + 1 >= len(argv):
            raise SystemExit('-o needs a directory')
        outdir = argv[i + 1]
        argv = argv[:i] + argv[i + 2:]
    oracle = '--oracle' in argv
    rest = [a for a in argv if not a.startswith('-')]
    syx = config.firmware(rest[0] if rest else None)
    # -o is the "explicit argument" tier of config.py's resolution order, so
    # without it this must fall through to DT2_SECTIONS rather than a literal.
    outdir = outdir or config.sections_dir()
    print('Extracting %s -> %s/\n' % (syx, outdir.rstrip('/')), flush=True)

    def started(sid, kind, name):
        note = '  (a few megabytes, about a minute)' if oracle and kind == 'packed' and sid == 3 else ''
        print('  %-26s %-14s%s' % (name, kind + '...', note), end='', flush=True)

    for sid, kind, path, n, dest in extract(syx, outdir, progress=started, oracle=oracle):
        print('\r  %-26s %-14s%9d bytes  dest=%#010x'
              % (os.path.basename(path), kind, n, dest), flush=True)
    return 0


if __name__ == '__main__':
    sys.exit(main(sys.argv[1:]))
