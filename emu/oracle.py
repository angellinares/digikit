"""Acceptance oracle: run the device's OWN validators against a candidate image.

The bootstrap decides whether to accept a MIDI-delivered OS image using a small
number of self-contained routines. Emulating them turns "will the device take
this?" from a hardware experiment into a unit test.

Implemented (both verified byte-exact against known-good references):
    crc32   0x80001bd0  CRC-32, poly 0xEDB88320, residue 0xDEBB20E3
    depack  0x80000432  aPLib decompressor -- accepts the tool's repack

Not yet implemented (see docs/findings/01-container-and-patching.md,
"Recovery"):
    content checksum  0x40003ca6  word-sum with each word XORed by its index
    HMAC-SHA256       0x80005e2a  verifies the 32-byte container trailer

Addresses are for OS 1.15C only. Check the input hash before trusting them.
"""
import struct
import sys, os
sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from emu.harness import Machine, call
from emu import config

BOOT_LOAD = 0x80000400      # bootstrap payload load address
CRC32     = 0x80001bd0
DEPACK    = 0x80000432
SRC       = 0x50000000
DST       = 0x60000000


def _machine(bootstrap):
    """bootstrap = section 2 raw bytes (payload starts 4 bytes in)."""
    m = Machine()
    m.install_isa_patches()
    m.load(bootstrap[4:], BOOT_LOAD)
    return m


def crc32(bootstrap, data, init=0xFFFFFFFF):
    """The device's CRC-32. Returns the raw register (no final XOR)."""
    m = _machine(bootstrap)
    m.ensure(SRC)
    for off in range(0, len(data), 0x100000):
        m.ensure(SRC + off)
    m.uc.mem_write(SRC, data)
    return call(m, CRC32, [init, SRC, len(data)])


def crc_accepts(bootstrap, blob):
    """True if `blob` (payload||crc) satisfies the bootstrap's residue check."""
    return crc32(bootstrap, blob) == 0xDEBB20E3


def depack(bootstrap, stream, out_cap=0x800000):
    """Run the device's aPLib depacker. `stream` includes its 8-byte header."""
    m = _machine(bootstrap)
    for off in range(0, len(stream) + 0x100000, 0x100000):
        m.ensure(SRC + off)
    for off in range(0, out_cap, 0x100000):
        m.ensure(DST + off)
    m.uc.mem_write(SRC, stream)
    n = call(m, DEPACK, [SRC, DST])
    if n > out_cap:
        raise ValueError('implausible output length %d' % n)
    return bytes(m.uc.mem_read(DST, n))


def verify_container(bootstrap, syx_path, expected):
    """Decompress every section of `syx_path` with the DEVICE's depacker and
    compare against `expected` = {section_id: bytes}. -> list of (id, ok, n)."""
    from dt2.container import sections, compressed_stream
    c, secs = sections(syx_path)
    out = []
    for sid, off, clen, _dest in secs:
        if sid not in expected:
            continue
        got = depack(bootstrap, compressed_stream(c, off, clen))
        out.append((sid, got == expected[sid], len(got)))
    return out


if __name__ == '__main__':
    import zlib
    boot = open(config.bootstrap(sys.argv[1] if len(sys.argv) > 1 else None),
                'rb').read()
    for name, msg in [("'123456789'", b'123456789'), ('1KB 0xA5', b'\xa5' * 1024)]:
        got = crc32(boot, msg)
        exp = zlib.crc32(msg) ^ 0xFFFFFFFF
        print('  crc %-12s emulated=0x%08x zlib=0x%08x %s'
              % (name, got, exp, 'ok' if got == exp else 'MISMATCH'))
    blob = b'x' * 32
    blob += struct.pack('<I', zlib.crc32(blob))
    print('  residue check: %s' % ('accepts' if crc_accepts(boot, blob) else 'REJECTS'))
