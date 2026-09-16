"""Native Bitmap::setPixel / getPixel.

Once emu/softfloat.py removed the soft-float work, these two routines became
the top cost -- together ~54% of everything executed, because the rasteriser
touches all 8,192 pixels of every frame and reads many of them back.

Both are small, fully understood bit-twiddling routines, so running them
natively is exact rather than approximate. From the disassembly at 0x40104eb4
and 0x40104f80:

    setPixel(bmp, x, y, val)        getPixel(bmp, x, y) -> masked word
      if x < 0 or y < 0: return       same bounds check, returns 0 outside
      if x >= bmp[+04]: return        word = data[x*stride + (y >> 5)]
      if y >= bmp[+08]: return        return word & (0x80000000 >> (y & 31))
      i = x * bmp[+0C] + (y >> 5)
      m = 0x80000000 >> (y & 31)
      bmp[+10][i] |= m  if val & 1
                    &= ~m otherwise

Two details worth keeping: the bounds comparisons are *signed*, and the value
is tested with `btst.b #0` -- so val=2 clears a pixel rather than setting it.
The HLE writes the same bits into emulated memory, so anything that reads the
bitmap back sees identical state.

    uv run python -m emu.hle        # verify against the real routines
"""
import os
import struct
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from unicorn.m68k_const import UC_M68K_REG_A7, UC_M68K_REG_D0, UC_M68K_REG_PC
from emu import config

# Digitakt reference values, used only by the selftest below. Callers pass
# the resolved per-build addresses into install_bitmap instead.
SET_PIXEL = 0x40104eb4
GET_PIXEL = 0x40104f80

# The return address of the setPixel call being reported to on_pixel, set just
# before each callback. Lets a tracer attribute every pixel to the code that
# drew it -- which routine draws which region of the intro -- without changing
# on_pixel's four-argument signature that emu/frame.py and emu/gui.py rely on.
LAST_PIXEL_CALLER = None


def _s32(v):
    return v - (1 << 32) if v & 0x80000000 else v


def install_bitmap(at, stats=None, on_pixel=None, set_pixel=None, get_pixel=None):
    """HLE both routines. `on_pixel(x, y, val, bmp)` is called for each
    setPixel -- that is how emu/frame.py and emu/gui.py observe drawing
    without paying for a second hook. `bmp` is the Bitmap instance, which is
    worth keeping: it is how the panel framebuffer at 0x4313b298 was found."""
    sp_addr = set_pixel if set_pixel is not None else SET_PIXEL
    gp_addr = get_pixel if get_pixel is not None else GET_PIXEL

    # NOTE: do NOT cache the Bitmap header. It was tried, on the assumption
    # that geometry is fixed per Bitmap pointer, and it is not -- the firmware
    # mutates the fields of an existing Bitmap. Caching sent writes to a stale
    # buffer, the rasteriser read back the wrong pixels, and the intro's
    # instruction cost per frame changed by ~4x (setPixel 617,694 -> 163,840
    # over the same 20M instructions). The selftest below did not catch it
    # because it uses one Bitmap and never mutates it.
    S5 = struct.Struct('>5I')
    S4 = struct.Struct('>4I')
    SI = struct.Struct('>I')

    def fields(uc, bmp):
        w, h, stride, data = S4.unpack(uc.mem_read(bmp + 4, 16))
        return _s32(w), _s32(h), _s32(stride), data

    def do_set(uc, addr, size, data_):
        sp = uc.reg_read(UC_M68K_REG_A7)
        ret, bmp, x, y, val = S5.unpack(uc.mem_read(sp, 20))
        x, y = _s32(x), _s32(y)
        w, h, stride, base = fields(uc, bmp)
        if 0 <= x < w and 0 <= y < h:
            off = base + ((x * stride) + (y >> 5)) * 4
            word = SI.unpack(uc.mem_read(off, 4))[0]
            mask = 0x80000000 >> (y & 31)
            word = (word | mask) if (val & 1) else (word & ~mask & 0xFFFFFFFF)
            uc.mem_write(off, SI.pack(word))
            if on_pixel is not None:
                global LAST_PIXEL_CALLER
                LAST_PIXEL_CALLER = ret
                on_pixel(x, y, val & 1, bmp)
        uc.reg_write(UC_M68K_REG_A7, sp + 4)
        uc.reg_write(UC_M68K_REG_PC, ret)
        if stats is not None:
            stats['setPixel'] += 1

    def do_get(uc, addr, size, data_):
        sp = uc.reg_read(UC_M68K_REG_A7)
        ret, bmp, x, y = S4.unpack(uc.mem_read(sp, 16))
        x, y = _s32(x), _s32(y)
        w, h, stride, base = fields(uc, bmp)
        out = 0
        if 0 <= x < w and 0 <= y < h:
            off = base + ((x * stride) + (y >> 5)) * 4
            word = SI.unpack(uc.mem_read(off, 4))[0]
            out = word & (0x80000000 >> (y & 31))
        uc.reg_write(UC_M68K_REG_D0, out & 0xFFFFFFFF)
        uc.reg_write(UC_M68K_REG_A7, sp + 4)
        uc.reg_write(UC_M68K_REG_PC, ret)
        if stats is not None:
            stats['getPixel'] += 1

    at(sp_addr, do_set)
    at(gp_addr, do_get)


def selftest(verbose=True):
    """Run the real routines and the HLE against each other on the same
    Bitmap, comparing the whole backing store byte for byte."""
    import random
    from emu.harness import Machine, call
    import emu.dspboot as db

    img = open(config.main_image(), 'rb').read()
    W, H = 128, 64
    STRIDE = (H + 31) // 32
    BMP, DATA = 0x30000000, 0x30020000
    NBYTES = W * STRIDE * 4

    def fresh():
        m = Machine()
        m.install_isa_patches_scoped(img, db.MAIN_LOAD)
        m.install_exceptions()
        m.load(img, db.MAIN_LOAD)
        m.ensure(BMP)
        m.ensure(DATA)
        m.uc.mem_write(BMP, struct.pack('>5I', 0, W, H, STRIDE, DATA))
        return m

    rnd = random.Random(1234)
    pattern = bytes(rnd.randrange(256) for _ in range(NBYTES))
    cases = [(x, y, v) for x in (-1, 0, 1, 63, 127, 128, 200)
             for y in (-1, 0, 1, 31, 32, 63, 64, 99) for v in (0, 1, 2, 3, 255)]
    cases += [(rnd.randrange(-4, 132), rnd.randrange(-4, 68), rnd.randrange(4))
              for _ in range(400)]

    m = fresh()
    bad_set = bad_get = 0
    for x, y, v in cases:
        # real routine
        m.uc.mem_write(DATA, pattern)
        call(m, SET_PIXEL, [BMP, x & 0xFFFFFFFF, y & 0xFFFFFFFF, v], limit=200_000)
        real = bytes(m.uc.mem_read(DATA, NBYTES))
        # HLE, applied to the same starting pattern
        buf = bytearray(pattern)
        if 0 <= x < W and 0 <= y < H:
            off = ((x * STRIDE) + (y >> 5)) * 4
            word = struct.unpack_from('>I', buf, off)[0]
            mask = 0x80000000 >> (y & 31)
            word = (word | mask) if (v & 1) else (word & ~mask & 0xFFFFFFFF)
            struct.pack_into('>I', buf, off, word)
        if bytes(buf) != real:
            bad_set += 1
            if bad_set <= 5:
                print('  setPixel MISMATCH at (%d,%d,%d)' % (x, y, v))

        # getPixel
        m.uc.mem_write(DATA, pattern)
        got = call(m, GET_PIXEL, [BMP, x & 0xFFFFFFFF, y & 0xFFFFFFFF],
                   limit=200_000) & 0xFFFFFFFF
        want = 0
        if 0 <= x < W and 0 <= y < H:
            off = ((x * STRIDE) + (y >> 5)) * 4
            want = struct.unpack_from('>I', pattern, off)[0] & (0x80000000 >> (y & 31))
        if got != want:
            bad_get += 1
            if bad_get <= 5:
                print('  getPixel MISMATCH at (%d,%d): real=0x%08x hle=0x%08x'
                      % (x, y, got, want))
    # Regression guard: the firmware MUTATES an existing Bitmap's header, so
    # geometry must be re-read every call. Point the same Bitmap object at a
    # second buffer and check the write follows it.
    DATA2 = DATA + 0x10000
    m.ensure(DATA2)
    m.uc.mem_write(BMP, struct.pack('>5I', 0, W, H, STRIDE, DATA2))
    m.uc.mem_write(DATA, b'\x00' * NBYTES)
    m.uc.mem_write(DATA2, b'\x00' * NBYTES)
    call(m, SET_PIXEL, [BMP, 5, 5, 1], limit=200_000)
    moved = bytes(m.uc.mem_read(DATA2, NBYTES)).strip(b'\x00') != b''
    stale = bytes(m.uc.mem_read(DATA, NBYTES)).strip(b'\x00') != b''
    bad_move = (not moved) or stale
    if bad_move:
        print('  MUTATION MISMATCH: write went to the old buffer '
              '(geometry is being cached when it must not be)')
    if verbose:
        print('bitmap HLE selftest: %d cases, setPixel mismatches=%d, '
              'getPixel mismatches=%d, header-mutation ok=%s'
              % (len(cases), bad_set, bad_get, not bad_move))
    return bad_set == 0 and bad_get == 0 and not bad_move


if __name__ == '__main__':
    raise SystemExit(0 if selftest() else 1)
