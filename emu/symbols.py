# fmt: off
"""Resolve firmware addresses from the image itself, instead of hardcoding
them per build.

Every address dspboot/longrun/gui/panel needs used to be a literal constant,
good for exactly one firmware: Digitakt II OS 1.15C. A second firmware (same
ColdFire SoC, same bootloader, same RTOS, different application build --
Digitone II 1.10E) runs under those constants today and produces silent
garbage: no error, just five minutes of boot into a blank panel, because every
hook lands on the wrong instruction.

Proof the addresses are derivable rather than fixed: five RTOS entry points
(entry, task_create, task_start, sem_pend, pend_b) sit at byte-identical
offsets in both 1.15C and 1.10E -- the RTOS is linked at a stable base
regardless of what the application above it looks like. Everything else moves
between builds by anywhere from a few bytes to several hundred KB, because the
application code before it changed size. So the five RTOS entry points are
FIXED (checked in as literal addresses, verified against opening bytes at
resolve time so a firmware that does NOT share this RTOS build fails loudly
rather than silently), and everything else is found by what it looks like or
how it is reached from something already resolved:

    * a `jsr <fixed-address>` call site (Xrefs, XrefShape)
    * a big-endian abs32 literal embedded in the instruction stream at a
      known offset from something already resolved (Operand, Operands)
    * a byte sequence unique in the whole image (Opcode)
    * a masked signature: N reference bytes captured from the Digitakt image,
      with any embedded absolute address in [0x40000000, 0x40400000) --
      i.e. anything that looks like an inlined pointer INTO the loaded image,
      which relocates between builds -- wildcarded out, then searched for a
      unique match in the target image (Sig)

Verified against two extracted MAIN OS images (both load at 0x40000400):
Digitakt II 1.15C and Digitone II 1.10E. See docs/ for the resolution table;
this file is the implementation, and the reference bytes below are the only
firmware-specific literals it contains besides the five RTOS addresses.

Nothing else about either firmware is checked in. In particular the masked
signature bytes are captured from the DIGITAKT image and pasted here as
literals -- NOT read from disk at runtime -- so this module, and everything
that imports it, works with no firmware image present at all.

REQUIRED symbols (boot cannot progress without them): entry, flash_read,
pend_call, completion_sem, depack_copy, task_start, sem_pend. An unresolved or
ambiguous REQUIRED symbol raises SymbolResolutionError naming exactly which
symbol(s) and why, at `resolve()` time -- not five minutes into a run.

Everything else is OPTIONAL: diagnostic (task_create_sites, call_sites,
idle_spins), late-boot/GUI (panel_diff, fb_front, fb_back), or the UI-trace
hook points (queue_send, ui_queue, ui_key_dispatch, view_offer,
view_activate, view_close, view_closed_mark, view_request_pop, view_sweep,
ui_tick_inc, ui_tick_counter). An unresolved
OPTIONAL symbol is simply None on the Profile; every caller of one must
degrade gracefully (install no hook) rather than crash -- see dspboot.py,
longrun.py and panel.py for the pattern.

    python -m emu.symbols [image]      # print the resolution report
"""
import hashlib
import os
import re
import struct
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

LOAD_ADDR = 0x40000400   # both known builds load MAIN OS here (see emu.config)

# Sig masks out any inlined abs32 that relocates between builds. The default
# window is the loaded image itself; DATA_HI widens it to cover the RAM
# variables above the image -- semaphores, buffers, task blocks -- which
# relocate just as freely. Widening is not free: the window is scanned
# unaligned, so a wider range also masks bytes that merely *look* like an
# address in it (`45f9 4017....`, lea's opcode plus the top half of its
# operand, is one that bites). Use it only where a signature is verified
# unique in both images with it, and prefer the default where that works.
DATA_HI = 0x48000000

REQUIRED = frozenset({
    'entry', 'flash_read', 'pend_call', 'completion_sem', 'depack_copy',
    'task_start', 'sem_pend',
})


class SymbolResolutionError(RuntimeError):
    """A REQUIRED symbol could not be resolved. str(e) names exactly which
    one(s) and what rule failed, so this replaces booting for five minutes
    into a blank panel with a refusal at load time."""


# --------------------------------------------------------------------------
# Rules. Each is declarative: given the image, its load address, and the
# dict of symbols already resolved, it returns (value, detail) -- value is
# None if the rule failed, detail is a one-line human explanation either way
# (Profile.report() prints it verbatim).
# --------------------------------------------------------------------------

class Fixed:
    """A literal address. Must be present in the image and, if `verify` is
    given, its opening bytes must match -- this is what makes a firmware
    that does NOT share this RTOS build fail loudly instead of silently."""

    def __init__(self, addr, verify=None):
        self.addr = addr
        self.verify = bytes.fromhex(verify) if verify else None

    def resolve(self, img, load_addr, got):
        off = self.addr - load_addr
        if not (0 <= off < len(img)):
            return None, '0x%08x is outside the image' % self.addr
        if self.verify:
            n = len(self.verify)
            actual = img[off:off + n]
            if actual != self.verify:
                return None, ('bytes at 0x%08x are %s, expected %s'
                               % (self.addr, actual.hex(), self.verify.hex()))
        return self.addr, 'fixed 0x%08x (RTOS base, byte-identical across builds)' % self.addr


class Reloc:
    """A Digitakt II address that may have moved in another build.

    Resolution goes in three steps, cheapest first. The literal address is
    tried with its verify bytes, so the reference build resolves with no
    search at all. Failing that the verify bytes are searched for, and a
    single match wins.

    Short preambles are often not unique, though, and that is what `anchor`
    is for: it names a symbol from the same relinked block, and the offset
    the two addresses had on the reference build is applied to wherever the
    anchor actually landed. The result is still checked against the verify
    bytes, so a wrong anchor fails loudly instead of naming plausible code.

    That last step assumes the block relinked as a unit, which is a claim
    about the build and not a general truth -- the verify is what keeps it
    honest.

    `verify` takes '..' per wildcard byte, same convention as XrefShape, so a
    preamble carrying an inlined address that relocates can still be matched
    on its opcodes."""

    def __init__(self, addr, verify, anchor=None, anchor_addr=None):
        self.addr = addr
        self.shape = verify
        self.n = len(verify) // 2
        self.anchor = anchor
        self.anchor_addr = anchor_addr

    def _check(self, img, load_addr, addr):
        off = addr - load_addr
        if off < 0 or off + self.n > len(img):
            return False
        return _shape_match(img[off:off + self.n], self.shape)

    def _search(self, img):
        return [i for i in range(len(img) - self.n + 1)
                if _shape_match(img[i:i + self.n], self.shape)]

    def resolve(self, img, load_addr, got):
        if self._check(img, load_addr, self.addr):
            return self.addr, 'fixed 0x%08x, verify matches' % self.addr

        hits = self._search(img)
        if len(hits) == 1:
            addr = load_addr + hits[0]
            return addr, ('moved: unique verify match at 0x%08x (%+#x)'
                          % (addr, addr - self.addr))

        if self.anchor is None:
            return None, ('%d verify match(es) for 0x%08x and no anchor to '
                          'disambiguate' % (len(hits), self.addr))
        base = got.get(self.anchor)
        if base is None:
            return None, "depends on unresolved '%s'" % self.anchor
        addr = base + (self.addr - self.anchor_addr)
        if not self._check(img, load_addr, addr):
            return None, ('%d verify match(es); %s%+#x = 0x%08x does not verify'
                          % (len(hits), self.anchor, self.addr - self.anchor_addr,
                             addr))
        return addr, ('moved: %s%+#x = 0x%08x, verified (%d other candidates '
                      'rejected)' % (self.anchor, self.addr - self.anchor_addr,
                                     addr, len(hits) - 1))


class Xrefs:
    """Every `jsr <target>` call site (opcode 4EB9 + abs32 operand). Resolves
    to a tuple of call-site (opcode) addresses; unresolved if there are none."""

    def __init__(self, target):
        self.target = target

    def resolve(self, img, load_addr, got):
        target = got.get(self.target)
        if target is None:
            return None, "depends on unresolved '%s'" % self.target
        sites = _find_all(img, b'\x4e\xb9' + struct.pack('>I', target))
        if not sites:
            return None, 'no jsr 0x%08x call sites found' % target
        return tuple(load_addr + i for i in sites), (
            '%d call site(s) to 0x%08x' % (len(sites), target))


class XrefShape:
    """The call site into `target` whose following bytes match `shape` (a hex
    string, '..' per wildcard byte, applied immediately after the 6-byte jsr
    instruction). Must be unique. Resolves to the jsr opcode address."""

    def __init__(self, target, shape):
        self.target = target
        self.shape = shape

    def resolve(self, img, load_addr, got):
        target = got.get(self.target)
        if target is None:
            return None, "depends on unresolved '%s'" % self.target
        needle = b'\x4e\xb9' + struct.pack('>I', target)
        n = len(self.shape) // 2
        hits = []
        for i in _find_all(img, needle):
            tail = img[i + 6:i + 6 + n]
            if len(tail) == n and _shape_match(tail, self.shape):
                hits.append(i)
        if len(hits) != 1:
            return None, ('%d call site(s) to 0x%08x match shape %s, need exactly 1'
                           % (len(hits), target, self.shape))
        return load_addr + hits[0], (
            'unique jsr 0x%08x + shape %s at 0x%08x' % (target, self.shape, load_addr + hits[0]))


class Operand:
    """A big-endian integer read out of the instruction stream at
    `<resolved symbol> + at`, adjusted by `adjust`."""

    def __init__(self, symbol, at, width=4, adjust=0):
        self.symbol, self.at, self.width, self.adjust = symbol, at, width, adjust

    def resolve(self, img, load_addr, got):
        base = got.get(self.symbol)
        if base is None:
            return None, "depends on unresolved '%s'" % self.symbol
        off = (base + self.at) - load_addr
        if not (0 <= off + self.width <= len(img)):
            return None, 'operand read at %s+%d falls outside the image' % (self.symbol, self.at)
        raw = img[off:off + self.width]
        literal = int.from_bytes(raw, 'big')
        val = literal + self.adjust
        return val, ('%s+%d = 0x%0*x, %+d -> 0x%08x'
                      % (self.symbol, self.at, self.width * 2, literal, self.adjust, val))


class Offset:
    """A fixed byte distance from an already-resolved symbol.

    For the case where two names denote the same instruction from two
    directions -- `sleep_pend` is the *return* address of the `jsr sem_pend`
    that `pend_call` names, so it is exactly `pend_call + 6` in any build,
    with no signature of its own to match."""

    def __init__(self, symbol, delta):
        self.symbol, self.delta = symbol, delta

    def resolve(self, img, load_addr, got):
        base = got.get(self.symbol)
        if base is None:
            return None, "'%s' unresolved" % self.symbol
        return base + self.delta, '%s%+d' % (self.symbol, self.delta)


class Opcode:
    """A raw byte pattern that must occur exactly once in the whole image."""

    def __init__(self, hexstr):
        self.pattern = bytes.fromhex(hexstr)

    def resolve(self, img, load_addr, got):
        hits = _find_all(img, self.pattern)
        if len(hits) != 1:
            return None, ('%d occurrence(s) of opcode %s, need exactly 1'
                           % (len(hits), self.pattern.hex()))
        return load_addr + hits[0], 'unique opcode at 0x%08x' % (load_addr + hits[0])


class Sig:
    """A masked signature: `ref_hex` is N literal bytes captured from the
    DIGITAKT reference image (see the module docstring -- this is the only
    place firmware bytes are embedded). Any 4-byte big-endian window that
    falls in [lo, hi) -- an inlined address into the loaded image itself,
    which relocates between builds -- is wildcarded out before matching.
    Must find exactly one match in the target image.

    `wild` additionally masks individual byte offsets that are not addresses
    but still move between builds -- a small immediate the compiler chose, for
    instance. Use it sparingly and say in a comment what the byte is, because
    every masked byte is one less thing keeping the match unique."""

    def __init__(self, ref_hex, lo=0x40000000, hi=0x40400000, wild=()):
        self.raw = bytes.fromhex(ref_hex)
        self.lo, self.hi = lo, hi
        self.regex = re.compile(_mask_pattern(self.raw, lo, hi, wild), re.DOTALL)

    def resolve(self, img, load_addr, got):
        hits = [m.start() for m in self.regex.finditer(img)]
        if len(hits) != 1:
            return None, ('%d match(es) for masked signature (%d bytes), need exactly 1'
                           % (len(hits), len(self.raw)))
        return load_addr + hits[0], 'unique masked-signature match at 0x%08x' % (load_addr + hits[0])


class SigWhere:
    """A masked signature that is deliberately NOT unique, narrowed to one
    match by an abs32 operand that ties it to an already-resolved symbol.

    The intro's PIT3 handler and the display module's are the same routine
    compiled twice -- ack the timer, post a semaphore -- so they mask to the
    same signature and no amount of extra context separates them: they differ
    only in *which* semaphore they post. That is the operand at `at`, and
    comparing it to `equals` (plus `adjust`) is what says which one is the
    intro's. Requires exactly one surviving match."""

    def __init__(self, ref_hex, at, equals, adjust=0, lo=0x40000000, hi=0x40400000):
        self.raw = bytes.fromhex(ref_hex)
        self.at, self.equals, self.adjust = at, equals, adjust
        self.regex = re.compile(_mask_pattern(self.raw, lo, hi), re.DOTALL)

    def resolve(self, img, load_addr, got):
        want = got.get(self.equals)
        if want is None:
            return None, "'%s' unresolved" % self.equals
        hits = []
        for m in self.regex.finditer(img):
            off = m.start() + self.at
            if off + 4 > len(img):
                continue
            if int.from_bytes(img[off:off + 4], 'big') + self.adjust == want:
                hits.append(m.start())
        if len(hits) != 1:
            return None, ('%d of the masked-signature matches carry %s at +%d, need exactly 1'
                          % (len(hits), self.equals, self.at))
        return load_addr + hits[0], ('masked-signature match at 0x%08x, selected by %s at +%d'
                                     % (load_addr + hits[0], self.equals, self.at))


class OperandGroup:
    """The first `take` DISTINCT big-endian abs32 values in [lo, hi) found by
    scanning `span` bytes starting at `<resolved base symbol>`. Resolves to a
    tuple of up to `take` values, in the order they first appear. Used to
    pull fb_front/fb_back out of panel_diff's own instruction stream -- see
    Pick, which then names each element."""

    def __init__(self, base, span, lo, hi, take):
        self.base, self.span, self.lo, self.hi, self.take = base, span, lo, hi, take

    def resolve(self, img, load_addr, got):
        base = got.get(self.base)
        if base is None:
            return None, "depends on unresolved '%s'" % self.base
        off = base - load_addr
        window = img[off:off + self.span]
        found, i = [], 0
        while i <= len(window) - 4 and len(found) < self.take:
            val = int.from_bytes(window[i:i + 4], 'big')
            if self.lo <= val < self.hi:
                if val not in found:
                    found.append(val)
                i += 4
            else:
                i += 1
        if not found:
            return None, ('no operands in [0x%08x, 0x%08x) within %#x bytes of %s'
                           % (self.lo, self.hi, self.span, self.base))
        return tuple(found), ('%d distinct operand(s) near %s: %s'
                               % (len(found), self.base, ', '.join('0x%08x' % v for v in found)))


class Pick:
    """One element of a symbol that resolved to a tuple (OperandGroup, Xrefs,
    ScanAll). Unresolved if the group is unresolved or too short."""

    def __init__(self, group, index):
        self.group, self.index = group, index

    def resolve(self, img, load_addr, got):
        group = got.get(self.group)
        if group is None or len(group) <= self.index:
            n = 0 if group is None else len(group)
            return None, "'%s' has %d element(s), need index %d" % (self.group, n, self.index)
        return group[self.index], 'element %d of %s' % (self.index, self.group)


class ScanAll:
    """Every occurrence of a raw byte pattern, at a fixed `step` alignment.
    Unlike Opcode/Xrefs this never fails -- zero occurrences is itself a
    valid (if surprising) resolution, e.g. a build with no idle spins."""

    def __init__(self, hexstr, step=2):
        self.pattern = bytes.fromhex(hexstr)
        self.step = step

    def resolve(self, img, load_addr, got):
        n = len(self.pattern)
        hits = [off for off in range(0, len(img) - n + 1, self.step)
                if img[off:off + n] == self.pattern]
        return tuple(load_addr + off for off in hits), '%d occurrence(s)' % len(hits)


class StringTable:
    """A `char *` table, located by the literal strings its entries point at.

    A data table has no opcodes to sign, and this one is reached through a
    C++ object field rather than an immediate operand, so there is nothing
    for Xrefs, Operand or Sig to anchor to. What it does have is content:
    entry n points at a known string. Finding an aligned run of big-endian
    pointers whose targets ARE those strings identifies the table without
    depending on where the compiler put it -- which is what keeps it working
    on a firmware version neither known build has seen.

    The strings are read back out of the image and compared, rather than
    their addresses being required unique, because these products reuse
    short labels: `TRIG` is both a page button and the stem of `TRIG 1`,
    and `ENCODER A` appears in two different tables.

    Only the NUL that TERMINATES an anchor is required, never one before it.
    The compiler tail-merges string literals, so a short label is commonly
    the suffix of a longer one -- `SRC` is the last three bytes of
    `PAGE SRC` -- and demanding a leading NUL matches nothing at all.

    Resolves to the address of entry 0. Must match exactly once.
    """

    def __init__(self, strings):
        self.strings = tuple(strings)

    def _cstr(self, img, load_addr, addr, limit=64):
        off = addr - load_addr
        if off < 0 or off >= len(img):
            return None
        end = img.find(b'\x00', off)
        if end < 0 or end - off > limit:
            return None
        try:
            return img[off:end].decode('ascii')
        except UnicodeDecodeError:
            return None

    def resolve(self, img, load_addr, got):
        first = self.strings[0].encode()
        found = set()
        for off in _find_all(img, first + b'\x00'):
            ptr = load_addr + off
            for base in _find_all(img, struct.pack('>I', ptr)):
                if base % 4:
                    continue
                if base + 4 * len(self.strings) > len(img):
                    continue
                entries = [struct.unpack_from('>I', img, base + 4 * i)[0]
                           for i in range(len(self.strings))]
                if all(self._cstr(img, load_addr, e) == s
                       for e, s in zip(entries, self.strings)):
                    found.add(base)
        if len(found) != 1:
            return None, ('%d table(s) matching %d anchor string(s), need 1'
                          % (len(found), len(self.strings)))
        base = load_addr + found.pop()
        return base, 'char* table at 0x%08x' % base


def _find_all(img, needle):
    out, start = [], 0
    while True:
        i = img.find(needle, start)
        if i < 0:
            return out
        out.append(i)
        start = i + 1


def _shape_match(data, shape):
    for k in range(0, len(shape), 2):
        pair = shape[k:k + 2]
        if pair == '..':
            continue
        if '%02x' % data[k // 2] != pair:
            return False
    return True


def _mask_pattern(raw, lo, hi, wild=()):
    n = len(raw)
    mask = bytearray(n)
    i = 0
    while i <= n - 4:
        val = int.from_bytes(raw[i:i + 4], 'big')
        if lo <= val < hi:
            for k in range(4):
                mask[i + k] = 1
            i += 4
        else:
            i += 1
    for k in wild:
        mask[k] = 1
    return b''.join(b'.' if m else re.escape(bytes([b])) for b, m in zip(raw, mask))


# --------------------------------------------------------------------------
# The symbol table. Order matters: a rule may only depend on a symbol that
# resolves earlier in this list.
# --------------------------------------------------------------------------

# The five RTOS entry points. Verified byte-identical at these addresses in
# both Digitakt II 1.15C and Digitone II 1.10E -- the RTOS is linked at a
# stable base independent of the application above it. `entry` itself is
# application code (the reset handler), not RTOS, but its opening 7 bytes
# happen to match too (its 8th byte does not -- a build-specific operand --
# so only 7 are checked, not 8).
SYMBOLS = [
    ('entry', Fixed(0x400004e8, verify='41ef000423d040'), True),
    ('task_create', Fixed(0x400012c8, verify='202f000c72fcc2af'), False),
    ('task_start', Fixed(0x40001314, verify='2f0240c246fc2700'), True),
    ('sem_pend', Fixed(0x4000141a, verify='226f000440c046fc'), True),
    ('pend_b', Fixed(0x400013a6, verify='226f000440c146fc'), False),

    # Every static call site into task_create -- diagnostic (dspboot logs
    # entry/prio/tcb at each), not required for boot.
    ('task_create_sites', Xrefs('task_create'), False),

    # The one `jsr sem_pend` immediately followed by `508f` (addq.l #8,a7,
    # the transport routine's stack cleanup) and `203c <abs32>` (the
    # unrelated move that follows it in the same block) -- unique in both
    # builds tested. This is the DSP-transport completion wait: patching the
    # semaphore right before this call is what turns "block forever waiting
    # for hardware that will never reply" into "take the sem_pend fast path
    # and continue" -- see dspboot.py's module docstring for the full story.
    ('pend_call', XrefShape('sem_pend', shape='508f203c........'), True),

    # The abs32 operand of the `move.l #imm,d0` at pend_call+10 (6 bytes of
    # jsr, 2 of addq, 2 of move.l's own opcode) is the completion semaphore's
    # MUTEX field, 8 bytes into the semaphore struct -- so the semaphore
    # itself is that value minus 8.
    ('completion_sem', Operand('pend_call', at=10, adjust=-8), True),

    # A second, unrelated aPLib-style depacker's copy-loop entry, embedded in
    # MAIN OS itself (see dspboot.py). This exact 8-byte opcode idiom occurs
    # exactly once in both images tested.
    ('depack_copy', Opcode('12da12da53826600'), True),

    # flash_read: HLE'd entirely (dspboot patches its call site to copy
    # straight out of the emulated flash and return), so what is captured
    # here is its whole calling convention -- stack frame setup through the
    # tail jump -- which happens to need no masking at all: none of these 24
    # bytes are an inlined address into the loaded image.
    ('flash_read', Sig('4feffff448d7040c242f0010262f0014246f00184ebaf7d6'), True),

    # panel_diff: the double-buffer diff/flush/swap (see panel.py). Its own
    # 16-byte opening sequence embeds one inlined pointer -- the FRONT buffer
    # variable's address -- which is exactly the thing that moves between
    # builds, so it is the one window masked out.
    ('panel_diff', Sig('4fefffd848d71c7c24794029f6504283'), False),

    # fb_front / fb_back: the first two distinct pointer-sized values in
    # [0x40200000, 0x40400000) referenced within 0x120 bytes of panel_diff --
    # the FRONT and BACK buffer-pointer variables, read in that order because
    # the diff reads FRONT before it ever touches BACK.
    ('_fb_pair', OperandGroup('panel_diff', span=0x120, lo=0x40200000, hi=0x40400000, take=2), False),
    ('fb_front', Pick('_fb_pair', 0), False),
    ('fb_back', Pick('_fb_pair', 1), False),

    # transport / call_sites: NOT part of the verified table above -- an
    # extension of the same masked-signature technique, kept OPTIONAL because
    # it does not hold up under it. transport's own 12-byte opening sequence
    # resolves fine (unique in both images tested), but the 4 call sites that
    # `jsr` into it exist purely to log a diagnostic (dspboot's
    # do_transport_call prints which timeout argument was pushed -- it
    # changes no register or memory state) and Digitone was NOT verified to
    # resolve it. Left in specifically so that firmware degrades to "no
    # transport-call logging" rather than failing to boot -- see
    # dspboot.py: this is exactly the OPTIONAL-degrades-gracefully case.
    ('transport', Sig('2f032f02242f000c4ab944e4'), False),
    ('call_sites', Xrefs('transport'), False),

    # Optional SLC-status predicate. Its masked signature identifies the
    # helper; its absolute status-byte operand begins at instruction offset 2.
    # The data-RAM operand moves between builds, so it is masked and extracted.
    ('slc_status_predicate',
     Sig('71b94fe491987201b28067084a8056c071004e75', lo=0x48000000, hi=0x50000000),
     False),
    ('slc_status_addr', Operand('slc_status_predicate', at=2), False),

    # ----------------------------------------------------------------
    # The scheduler's two variables. The context switcher is RTOS, so it
    # sits at a fixed address and is byte-identical between builds apart
    # from these two operands -- which is exactly what makes them
    # resolvable: `movea.l <CURRENT_TCB>,a0` at +8 and `movea.l
    # <READY_CURSOR>,a1` at +0x14, whose abs32 operands start two bytes
    # into each. emu/tasks.py and emu/gui.py used to hardcode Digitakt's.
    # ----------------------------------------------------------------
    ('ctx_switch', Fixed(0x40000410, verify='46fc27002f48fffc2079'), False),
    # move.l a0,current_tcb inside ctx_switch: the variable still holds the
    # outgoing task, A0 the incoming one (emu/taskprof.py).
    ('ctx_switch_load', Fixed(0x4000044a, verify='23c847d9adb42288'), False),
    ('current_tcb', Operand('ctx_switch', at=0x0a), False),
    ('ready_cursor', Operand('ctx_switch', at=0x16), False),

    # ----------------------------------------------------------------
    # The intro's frame path. The boot intro paces itself off PIT3 and owns
    # vector 208 until it switches the timer off on its way out, at which
    # point the display module claims the same vector -- so "vector 208
    # still points at the intro's handler" is precisely the window in which
    # the intro is live, and emu/pit.py:intro_running tests exactly that.
    # It hardcoded Digitakt's handler address, so on any other build it
    # answered False throughout the intro and the GUI released the timers
    # into the middle of it.
    #
    # intro_done is the intro's exit sequence -- `clr.w d0` then `pea
    # <frame_sem+8>` then, four instructions later, `move.w d0,$fc08c000`,
    # which is the write that switches PIT3 off. Its two hard literals
    # (the PIT3 base and the INTC address) are what make it unique.
    # ----------------------------------------------------------------
    ('intro_done', Sig('424048794313120845f94000141a33c0fc08c000701013c0'
                       'fc05001c4eb94000155c588f4879431312004e92588f60f4',
                       hi=DATA_HI), False),

    # The `pea` operand at intro_done+4 is the frame semaphore's MUTEX
    # field, 8 bytes into the semaphore struct -- the same idiom as
    # completion_sem above, and the same -8.
    ('frame_sem', Operand('intro_done', at=4, adjust=-8), False),

    # Ack PIT3, post a semaphore, return. Two routines in each image match
    # this: the intro's handler and the display module's. They differ only
    # in the semaphore they post, so frame_sem is what tells them apart --
    # see SigWhere. (The other match is the display module's own PIT3
    # handler: 0x40125f3c on Digitakt, 0x40123384 on Digitone.)
    ('intro_pit3_isr', SigWhere('4feffff048d7030341f9fc08c000720430104879'
                                '43131200808130804eb94000148c4cef03030004'
                                '4fef0014',
                                at=20, equals='frame_sem', hi=DATA_HI), False),

    # ----------------------------------------------------------------
    # The pend sites longrun.py must NOT force-satisfy: each is a wait
    # whose caller re-checks a condition and loops, so satisfying it turns
    # a sleep into an infinite spin. See longrun.RECHECK_PENDS for what
    # each one is. queue_recv is RTOS and byte-identical; sleep_pend is
    # the return address of the very `jsr sem_pend` that pend_call names.
    # ----------------------------------------------------------------
    ('queue_recv', Fixed(0x40001946, verify='588f60f240c246fc2700'), False),
    ('intro_park', Sig('588f60f42f0a2f3c40490fdb45f9401752042f2f000c4e92'
                       '588f2ebc3f000000'), False),
    ('display_wait', Sig('588f60ec48794029e1f82a3c000000804879'
                         '4029e274487800144878001f486e'), False),

    # The display module's own PIT3 ISR (0x40125f3c) posts the progress
    # screen's frame semaphore here (`pea.l display_sem` before the give);
    # the progress-screen task pends on it once per frame at 0x40126132 as
    # well as at display_wait.

    ('pump_wait', Sig('42002f43002849f94018c0a41f40002c2f034e96'
                      '7001266a002c1f4000304200'), False),
    ('sleep_pend', Offset('pend_call', 6), False),

    # tick_dispatch is the RTOS tick-paced dispatcher loop: pend_b(tick_sem);
    # mutex_lock(m); run every due callback; mutex_unlock(m); repeat. The
    # pend at the top of that loop IS the pacing -- it is what holds the
    # loop to one pass per real tick. Force-satisfying it (as longrun's
    # unblock briefly did) does not re-check a condition the way the other
    # recheck entries above do; it makes a tick-paced loop free-running,
    # which ran the 54-slot software timer wheel roughly 100x per real tick
    # and starved the priority-6 Main OS task before it could finish
    # initialising. Verified byte-identical at 0x40002a46 in both Digitakt
    # II 1.15C and Digitone II 1.10E, and the 36-byte signature below occurs
    # exactly once in each image. It covers the frame setup, the movem.l,
    # clr.l d2, the three lea.l loads of fixed RTOS routines (pend_b
    # 0x400013a6, mutex_lock 0x40001608, mutex_unlock 0x4000172a -- all at
    # stable RTOS addresses in both builds), the loop-top move.l d2,d3 /
    # addq.l #1,d3 / eor.l d3,d2, and the pea OPCODE only. The two pea
    # OPERANDS just past the signature are deliberately excluded: they are
    # the build-specific tick semaphore and mutex (Digitone 0x46488008 /
    # 0x464880d0, Digitakt 0x47d9ade0 / 0x47d9aea8).
    ('tick_dispatch', Fixed(0x40002a46,
                            verify='4fefffe848d73c0c428249f9400013a6'
                                   '4bf94000160847f94000172a26025283'
                                   'b7824879'), False),
    # tick_pend is the return address of the `jsr (a4)` at tick_dispatch+0x28;
    # jsr (aN) is two bytes, so the pend returns to tick_dispatch+0x2a -- the
    # same idiom as sleep_pend being pend_call + 6 above.
    ('tick_pend', Offset('tick_dispatch', 0x2a), False),

    # ----------------------------------------------------------------
    # Bitmap::setPixel / getPixel, and the blit that pushes a Bitmap into
    # the panel's framebuffer. Each of the pixel routines has a near-twin
    # 0x66 bytes further on that shares its first 48 bytes (the same
    # bounds checks against a different pixel format), which is what made
    # these ambiguous before: 64 bytes is where the two part company, and
    # it picks the right one of the pair in BOTH images -- on Digitakt the
    # one already known correct from measurement.
    # ----------------------------------------------------------------
    ('set_pixel', Sig('2f032f02206f000c222f0010202f00144a816d4c4a806d48'
                      'b2a800046c42b0a800086c3c43e8000c761f4c1118002400'
                      'ea82c680202f001820680010d282e589'), False),
    ('get_pixel', Sig('2f02206f0008222f000c202f00104a816d344a806d30b2a8'
                      '00046c2ab0a800086c2443e8000c4c1118002400ea822068'
                      '0010d282741fc480203c80000000e4a8'), False),
    ('px_copy', Sig('4e56ffec48d71c0c246e0008266e000c2012'
                    'b0ab0004671248794022c38b4879'), False),

    # ----------------------------------------------------------------
    # The soft-float routines. This ColdFire has no FPU, so every float
    # operation is a libgcc-style routine and the intro's particle
    # simulation spends ~93% of all executed instructions in them --
    # emu/softfloat.py intercepts each entry and returns the host's answer
    # instead. It named all seven by literal Digitakt address, so on any
    # other build the HLE simply never fired and the guest ground through
    # the real arithmetic: measured on Digitone, the intro could not
    # finish a single frame in 20M instructions.
    #
    # 64 bytes each. Every one of these is unique in both images at 48, 64
    # and 80 bytes and resolves to the same address at each, so the window
    # is not sitting on a knife edge -- the short ones (abssf2, subsf3)
    # simply run past their own `rts` into the next routine, which is
    # stable because the whole libgcc block is emitted as a unit.
    # ----------------------------------------------------------------
    ('sf_mulsf3', Sig('4e56ffe848d700fc202e0008222e000c2e00b38702878000'
                      '00002c3c7f8000002a064685283c008000000880001f2400'
                      '670000c20881001f2601670000b0b086'), False),
    ('sf_subsf3', Sig('086f001f00084e56ffe848d700fc202e0008222e000c2040'
                      'd080670002082241d2816700021c283c00ffffff2a3c0100'
                      '00002c00c0844684cc84670001d6bc84'), False),
    ('sf_addsf3', Sig('4e56ffe848d700fc202e0008222e000c2040d08067000208'
                      '2241d2816700021c283c00ffffff2a3c010000002c00c084'
                      '4684cc84670001d6bc84670002504846'), False),
    ('sf_divsf3', Sig('4e56ffe848d700fc202e0008222e000c2e00b38702878000'
                      '00002c3c7f8000002a064685283c008000000880001f2400'
                      '670000b00881001f2601670000d0b086'), False),
    ('sf_abssf2', Sig('202f00040880001f4e750000202f00040880001f0c807f7f'
                      'ffff5fc0710044804e750000202f00040880001f4a806604'
                      '70024e7522000681ff8000000c817eff'), False),
    ('sf_fixsfsi', Sig('4e5600002f02242e00082f3c4f0000002f024eb940175818'
                       '508f4a806c122f024eb940176ac6588f242efffc4e5e4e75'
                       '2f3c4f0000002f024eb940174f1c588f'), False),
    ('sf_cmpsf2', Sig('4e560000487800012f2e000c2f2e000861fffffffd464e5e'
                      '4e7500004e560000487800012f2e000c2f2e000861ffffff'
                      'fd2a4e5e4e7500004e56ffe848d7047c'), False),

    # ----------------------------------------------------------------
    # sd_bringup: the eSDHC/eMMC bring-up routine (FUN_4011d67a on Digitone,
    # FUN_4011fed6 on Digitakt). The two are instruction-for-instruction
    # identical; they differ only in relocated code addresses, the
    # driver-struct base, and the EXT_CSD DMA destination.
    #
    # It clears the driver's "storage is up" flag on entry and sets it only
    # after the whole init sequence completes. Everything that reads or
    # writes block storage returns -1 immediately while that flag is zero,
    # so nothing downstream works until it is set.
    #
    # sd_flag is that flag: the `clr.l (abs).l` operand 28 bytes into the
    # routine, which is also the driver-struct base. sd_status is the
    # driver's own status word at base+0x30, which emu/esdhc.py has to write
    # on command completion; it was previously hardcoded to Digitakt's
    # 0x44E26F1C, which on Digitone left every command looking permanently
    # in-progress.
    #
    # The signature STOPS at 26 bytes even though the routine's prologue is
    # longer: the 4-byte window at +26 is `42b9` (clr.l abs.l) followed by
    # the top half of the flag address, and 0x42b944e2 falls inside the
    # DATA_HI mask window, so extending the signature masks that window and
    # takes the two real bytes at +30..31 with it -- which differ between
    # builds, so a longer signature matches Digitakt and fails on Digitone.
    # This is exactly the hazard the DATA_HI comment at the top of the file
    # warns about. Measured: 40-byte signature = 1 hit on Digitakt, 0 on
    # Digitone; 26-byte = 1 hit on each, at 0x4011fed6 and 0x4011d67a
    # respectively.
    # ----------------------------------------------------------------
    ('sd_bringup', Sig('4fefffe848d70c3c42a776f34eb940126c24487947dcaafc7804',
                       hi=DATA_HI), False),
    ('sd_flag', Operand('sd_bringup', at=28), False),
    ('sd_status', Offset('sd_flag', 0x30), False),

    # sd_cmd_sem / sd_data_sem: two of the three RTOS semaphores the bring-up
    # routine creates in its phase-1 setup, via the "create semaphore, initial
    # count 0" primitive, at driver-struct offsets +0x34, +0x44 and +0x4c.
    # sd_cmd_sem (+0x4c) is the one the command primitive (FUN_4011d5b4 on
    # Digitone, FUN_40120... on Digitakt) pends on after every XFERTYP write;
    # sd_data_sem (+0x44) is the one pended after a data transfer completes,
    # including the EXT_CSD DMA read that ends the bring-up. On Digitone these
    # resolve to 0x44459058 / 0x44459068 / 0x44459070 and on Digitakt to the
    # same offsets from 0x44e26eec.
    ('sd_cmd_sem', Offset('sd_flag', 0x4C), False),
    ('sd_data_sem', Offset('sd_flag', 0x44), False),

    # ----------------------------------------------------------------
    # The front-panel serial link. There is no memory-mapped key matrix to
    # find: tools/mmiotrace.py measured 60M post-intro instructions on each
    # build and saw zero GPIO, zero DSPI and zero unclaimed MMIO. The panel
    # is a separate microcontroller on UART8, and button and encoder events
    # arrive the way MIDI would -- eDMA channel 34 into a 1024-byte ring,
    # then vector 154 into the driver's receive callback.
    #
    # uart8_init is that driver's init routine, and it sits at the SAME
    # address in both builds: it is BSP-layer code, not relocated
    # application code, so it anchors as Fixed rather than by signature.
    # 81 of its first 96 bytes are identical across the two images and the
    # first 25 are a literal match; the verify window stops at 24 because
    # byte 25 begins the first per-build abs32 operand.
    #
    # Everything else chains off it, so none of it needs its own scan. The
    # instruction at +0x16 is `clr.l (abs).l` (42b9) and its operand at
    # +0x18 is the base of the driver's contiguous globals block. The field
    # offsets inside that block were read off both decompiles side by side
    # and are identical:
    #
    #     +0x10  RX ring base    0x4FE1A000 Digitakt / 0x4E502000 Digitone
    #     +0x30  consume index
    #     +0x40  receive callback pointer
    #
    # emu/serial.py hardcoded Digitakt's 0x4094CD84 / 0x4094CDA4 /
    # 0x4094CDB4 for these three. On Digitone they read 0xFFFFFFFF, so
    # feeding that build through it would have written into unmapped memory
    # rather than a ring.
    # ----------------------------------------------------------------
    ('uart8_init',
     Fixed(0x4000243e,
           verify='2f02740f41f9ec09404b1210202f000843f9ec07000042b9'), False),
    ('_uart8_globals', Operand('uart8_init', at=0x18), False),
    ('uart8_ring_ptr', Offset('_uart8_globals', 0x10), False),
    ('uart8_consume_idx', Offset('_uart8_globals', 0x30), False),
    ('uart8_rx_callback', Offset('_uart8_globals', 0x40), False),

    # The factory test mode's own names for the front-panel controls, which
    # is the firmware telling us what each control code means rather than us
    # inferring it from what the screen did. Both are `char *` tables indexed
    # by control code and terminated by 0xFFFFFFFF, and the two products
    # genuinely differ: Digitakt's button table ends at 50 (SAMPLING) while
    # Digitone's continues to 54 (VOICE, ARP, PLUS, STACK, MINUS), which is
    # why Digitakt reports nothing meaningful for channel 6 bits 2..7.
    #
    # Anchored on enough leading entries to separate them from the other two
    # similar tables nearby -- one of which also starts with UNDEFINED, and
    # another of which also contains the ENCODER A..H labels.
    ('panel_button_names',
     StringTable(('UNDEFINED', 'TRIG', 'SRC', 'FLTR', 'AMP', 'FX', 'MOD')),
     False),
    ('panel_encoder_names',
     StringTable(('UNDEFINED', 'ENCODER A', 'ENCODER B', 'ENCODER C')),
     False),

    # ----------------------------------------------------------------
    # Post-intro progress markers. These are what tells you whether the OS
    # actually took over, and emu/gui.py reports them on its status line --
    # it named all three by Digitakt address, so on Digitone the line read
    # "mainloop 0  jobs 0" no matter what the firmware was doing.
    #
    # mainloop is the main application task's message-loop head: it pends on
    # its own queue, whose address is the `pea` operand two bytes in. That
    # queue is the one the DTIM3 handler posts to, and on Digitakt this task
    # is what wakes after the intro and starts everything else -- it spawns
    # the display and job-worker tasks, re-points vector 208 at the display
    # module's own PIT3 handler and arms DTIM3. display_start is that
    # re-pointing routine, recognisable by its two hard MMIO literals (the
    # PIT3 base 0xfc08c000 and the INTC at 0xfc050050/0xfc05001d).
    # ----------------------------------------------------------------
    # Byte +15 is the `moveq #N,%d1` immediately after the queue-receive call
    # -- 40 on Digitakt II 1.15C and Digitone II 1.10E, 41 on Digitone II 1.11.
    # It is a plain immediate, not an address, so the [lo, hi) masking does not
    # reach it and the signature missed 1.11 entirely: bootcheck then reported
    # MISSING: mainloop entered and PARTIAL_MAIN_OS on a run whose main
    # application task was in fact scheduled and drawing. Masking that one byte
    # keeps the match unique on all three builds.
    ('mainloop', Sig('48794094ef3c4eb940001928588f722824407192b28065e8',
                     hi=DATA_HI, wild=(15,)), False),
    ('main_queue', Operand('mainloop', at=2), False),
    ('job_pump', Sig('4fefffcc48d77c3c246f0038240f2a0a260a068500000014'), False),
    ('display_start', Sig('701041f9fc08c000245f13c1fc050050722313c0fc05001d'), False),
    # Must follow display_start: the anchor is an offset from it. `pea <abs>.l ;
    # or.l %d1,%d0` has three sites on Digitone II 1.11 -- one posts frame_sem
    # (the intro's own, just after intro_pit3_isr), one is unrelated early code,
    # and the third sits 182 bytes below display_start, which is this one.
    ('display_frame_post', Reloc(0x40125f4e, '4879........8081',
                                 anchor='display_start',
                                 anchor_addr=0x40125f4e + 182), False),
    ('display_sem',      Operand('display_frame_post', at=2), False),


    # ----------------------------------------------------------------
    # UI-trace hook points: the UI queue, key dispatch to views, and view
    # activate/close. Verified by disassembly on Digitakt II 1.15C (see
    # emu/uitrace.py).
    # ----------------------------------------------------------------
    ('queue_send',       Fixed(0x40001896, verify='2f0a2f02206f000c'), False),
    ('ui_queue',         Operand('mainloop', at=2), False),
    # Three inlined call targets, all of which relocate, so Sig's address
    # masking does the work -- but only with enough bytes around them. The
    # eight bytes recorded before were `jsr <abs>; move.l %d2,-(%sp)`, which
    # masks down to two opcodes and matches 1,401 sites on Digitone II 1.11.
    # 20 bytes is unique on 1.15C, 1.16 and 1.11, and is the longest common
    # prefix: at 24 the body diverges and 1.16 stops matching.
    ('ui_key_dispatch',  Sig('4eb9401072bc2f022f2eff804eb9400305182f02'),
     False),

    # The view manager relinked as one block on Digitone II 1.11, moving by
    # +0xedbc. Three of these resolve on their own because their verify bytes
    # are unique; the other three are not unique and take the offset from a
    # sibling that is, then verify. Order matters -- an anchor must be listed
    # before whatever anchors to it.
    ('view_offer',       Reloc(0x4010ed64, '4e90508f4a0067c2'), False),
    ('view_activate',    Reloc(0x4010dc8a, '42004fefffd048d7'), False),
    ('view_closed_mark', Reloc(0x4010daba, '15400030202a002c'), False),
    ('view_close',       Reloc(0x4010daa2, '2f0a246f00084878',
                               anchor='view_closed_mark',
                               anchor_addr=0x4010daba), False),
    ('view_request_pop', Reloc(0x4010e52c, '7001206f00041140',
                               anchor='view_offer',
                               anchor_addr=0x4010ed64), False),
    ('view_sweep',       Reloc(0x4010ec44, '4fefffd048d77c7c',
                               anchor='view_offer',
                               anchor_addr=0x4010ed64), False),
    # addq.l #1,<counter> ; move.l <abs>,%d0 -- the counter operand is BSS and
    # relocates, so it is wildcarded and the opcodes carry the match. Three
    # sites have this pair on Digitone II 1.11; the anchor picks the one in
    # the view manager's block, and it is the same one the opcodes point at.
    ('ui_tick_inc',      Reloc(0x40110828, '52b9........2039',
                               anchor='view_offer',
                               anchor_addr=0x4010ed64), False),
    ('ui_tick_counter',  Operand('ui_tick_inc', at=2), False),

    # Every `bra.b $self` (opcode 60FE) -- the RTOS idiom for "nothing to do,
    # wait for the scheduler's timer tick to preempt me". dspboot.py already
    # computes this itself with the same algorithm (find_idle_spins), because
    # it is needed even when nothing else in this file is -- kept here too so
    # `python -m emu.symbols` reports it and every caller has one place to
    # get it from.
    ('idle_spins', ScanAll('60fe', step=2), False),
]

_NAMES = {name for name, _, _ in SYMBOLS}


class Profile:
    """Symbols by attribute or item access: `profile.pend_call` or
    `profile['pend_call']`. An OPTIONAL symbol that failed to resolve reads
    as None either way -- callers must check, not assume.
    """

    def __init__(self, image_sha256, load_addr, values, detail):
        self._values = values
        self._detail = detail
        self.image_sha256 = image_sha256
        self.load_addr = load_addr
        self.unresolved = [n for n, v in values.items() if v is None]

    def __getattr__(self, name):
        if name in _NAMES:
            return self._values.get(name)
        raise AttributeError(name)

    def __getitem__(self, name):
        return self._values[name]

    def get(self, name, default=None):
        return self._values.get(name, default)

    def report(self):
        lines = ['profile sha256=%s...  load=0x%08x'
                 % (self.image_sha256[:16], self.load_addr)]
        for name, _, required in SYMBOLS:
            val = self._values.get(name)
            tag = 'REQUIRED' if required else 'optional'
            detail = self._detail.get(name, '')
            if val is None:
                lines.append('  %-18s %-8s UNRESOLVED -- %s' % (name, tag, detail))
            elif isinstance(val, tuple):
                lines.append('  %-18s %-8s %-4d item(s) -- %s' % (name, tag, len(val), detail))
            else:
                lines.append('  %-18s %-8s 0x%08x  -- %s' % (name, tag, val, detail))
        if self.unresolved:
            lines.append('unresolved: %s' % ', '.join(self.unresolved))
        return '\n'.join(lines)


_cache = {}   # image sha256 -> Profile


def resolve(image, load_addr=LOAD_ADDR):
    """-> Profile. Raises SymbolResolutionError if any REQUIRED symbol is
    unresolved or ambiguous, naming exactly which one(s) and why.

    Cached per image SHA-256 (an in-process dict) -- resolving scans a ~3MB
    image several times over and this is called from several modules on
    every run, not once at startup.
    """
    h = hashlib.sha256(image).hexdigest()
    cached = _cache.get(h)
    if cached is not None:
        return cached

    got, detail = {}, {}
    for name, rule, required in SYMBOLS:
        val, why = rule.resolve(image, load_addr, got)
        got[name] = val
        detail[name] = why

    missing = [name for name, _, required in SYMBOLS if required and got[name] is None]
    if missing:
        lines = ['%d REQUIRED symbol(s) failed to resolve:' % len(missing)]
        for name in missing:
            lines.append('  %-16s %s' % (name, detail[name]))
        raise SymbolResolutionError('\n'.join(lines))

    profile = Profile(h, load_addr, got, detail)
    _cache[h] = profile
    return profile


if __name__ == '__main__':
    from emu import config

    path = sys.argv[1] if len(sys.argv) > 1 else config.main_image()
    with open(path, 'rb') as fh:
        img = fh.read()
    print('image: %s (%d bytes)' % (path, len(img)))
    try:
        profile = resolve(img)
    except SymbolResolutionError as e:
        print(str(e))
        raise SystemExit(1)
    print(profile.report())
