# fmt: off
"""Elektron OS .syx transport and ELE3 container.

Layer cake, outermost first:
  1. MIDI SysEx  -- 128-byte messages, F0 00 20 3C <dev> 00 <cmd> ... F7
  2. 8-in-7      -- MIDI carries 7-bit bytes; each group of 7 data bytes is
                    preceded by a byte holding their high bits, MSB first.
  3. preamble    -- 8 bytes; bytes 4..8 are the 32-bit content checksum.
  4. ELE3        -- magic, then a section table of 16-byte entries.

Nothing here is encrypted. See docs/findings/01-container-and-patching.md.
"""
import struct

MFR = b'\x00\x20\x3c'
COUNT_OFF, TABLE_OFF, ENTRY_SZ = 0x1C, 0x20, 16


def decode_syx(path):
    """.syx file -> decoded byte stream (preamble + container)."""
    try:
        with open(path, 'rb') as fh:
            d = fh.read()
    except OSError as exc:
        raise ValueError('cannot read SysEx file: %s' % path) from exc
    out = bytearray()
    i = 0
    while i < len(d):
        if d[i] != 0xF0:
            raise ValueError('expected F0 at offset %d' % i)
        j = d.index(0xF7, i)
        body = d[i+1:j]
        i = j + 1
        if len(body) != 126:          # 14-byte start/end markers
            continue
        payload = body[9:125]         # 9-byte header, 116 payload, 1 checksum
        k = 0
        while k < len(payload):
            ms = payload[k]; k += 1
            for n in range(7):
                if k >= len(payload):
                    break
                out.append(payload[k] | (0x80 if (ms >> (6 - n)) & 1 else 0))
                k += 1
    return bytes(out)


def container(path):
    """Decoded stream -> ELE3 container bytes (base = the magic)."""
    dec = decode_syx(path)
    off = dec.find(b'ELE3')
    if off < 0:
        raise ValueError('no ELE3 magic; unsupported container')
    return dec[off:]


def sections(path):
    """-> (container_bytes, [(id, offset, comp_len, dest), ...])

    `dest` is a load address for code sections, but for the bootstrap
    (section 2) it is a *version* word -- see
    docs/findings/01-container-and-patching.md, "Scope, and the 2.01
    firmwares".
    """
    c = container(path)
    n = struct.unpack_from('>I', c, COUNT_OFF)[0]
    out = []
    for k in range(n):
        out.append(struct.unpack_from('>IIII', c, TABLE_OFF + ENTRY_SZ * k))
    return c, out


def compressed_stream(c, off, clen):
    """Slice one section's aPLib stream, including its 8-byte header
    ([u32 stream_len][u32 byte_sum]) which the depacker expects."""
    return bytes(c[off:off + clen])


if __name__ == '__main__':
    import sys
    c, secs = sections(sys.argv[1])
    print('container %d bytes, %d sections' % (len(c), len(secs)))
    for sid, off, clen, dest in secs:
        ln, sm = struct.unpack_from('>II', c, off)
        print('  id=%d off=0x%06x clen=0x%06x dest=0x%08x  aplib[len=%d sum=0x%08x]'
              % (sid, off, clen, dest, ln, sm))
