# fmt: off
"""Image-only derivation of the emulator's never-fake semaphore set.

`emu/longrun.py`'s `unblock` force-satisfies most semaphore pends so a run
does not sit blocked waiting for hardware nothing here emulates. That is
wrong for any semaphore with a poster that CAN run in the emulator: faking a
pend whose real poster would eventually post anyway just races ahead of it
instead of waiting for it, which was the display_sem/worker_done_sem bug
class (see their comments below and in emu/symbols.py) -- found by chasing
one hang at a time. This module generalises that to "enumerate every
give/give_b post site in the image and classify it", by direct inspection of
the bytes, with no Ghidra project required.

Two things run in the emulator and must therefore never be faked:

  * ordinary task code -- any `give`/`give_b` call not reachable from an
    interrupt vector at all;
  * an ISR of a source THIS BUILD models -- e.g. PIT3 (see emu/pit.py),
    which really does fire in the emulator, so its post really does happen.
    The caller passes the vectors it actually raises as `modeled_vectors`;
    a give inside an ISR whose vector is NOT in that set (e.g. give_b's
    vector-134 pair -- hardware nothing here models) is correctly left
    fakeable.

Algorithm, in the order the functions below run it:

  1. `_find_vectors` finds every write into the RAM vector table (VBR
     0x40000000..+0x400): `move.l {Dn|#imm32},(abs32).L` located by direct
     opcode search (`0x23C0+n` / `0x23FC`), not by disassembling the image
     -- a full linear disassembly of a ~3MB image takes several seconds in
     pure Python, far past the "well under 1s, runs at every emulator
     start" budget this has to hit. Each candidate is then re-checked with
     `_confirm_instruction`: disassemble forward from the nearest preceding
     function prologue (a real instruction-boundary anchor) and see if a
     `move.l` actually starts exactly at the candidate address. A raw
     2-byte-aligned opcode search can otherwise land on the tail bytes of
     some longer, unrelated instruction that merely happens to end the same
     way; this is what tells the two apart, cheaply, because there are only
     a couple of dozen candidates to check, not the whole image.
  2. `_isr_reachable` walks forward from each confirmed handler to its
     first `rte`, following every `jsr`/`bsr` with a resolvable absolute or
     PC-relative target to its own body (bounded by its first `rts`),
     depth-limited to MAX_ISR_DEPTH, and records which vector(s) can reach
     which instruction addresses.
  3. `_find_call_sites` / `_immediate_arg` find every give/give_b call site
     with a literal (`pea #imm32`) semaphore operand -- copied from
     scratch/semscan.py, which used the same byte patterns against a
     Ghidra-derived caller graph instead of this module's own reachability
     walk.
  4. A poster site not reachable from any vector is a task poster. One
     reachable only from vector(s) outside `modeled_vectors` is an
     unmodeled-ISR poster (stays fakeable). One reachable from a vector IN
     `modeled_vectors` is a modeled-ISR poster. A semaphore is never-fake if
     any of its poster sites is a task poster or a modeled-ISR poster.

Known blind spot, inherent to walking forward from ISR entry points instead
of walking callers backward from each give site (which is what
scratch/semscan.py's Ghidra-based `classify()` does): if a subroutine is
reachable from BOTH an ISR and ordinary task code, this scan only sees the
ISR path and classifies a give inside it by that path alone. Ghidra's
caller-graph walk would correctly call such a give "task" (any non-ISR
caller makes it task); this module cannot, because it never asks "who
else calls this" -- it only asks "can an ISR reach this". The failure
direction is: a genuinely task-reachable give site could be missed if its
only *discovered* path happens to be through an ISR, i.e. it could be left
out of never_fake when it should be in it. Not observed on any of DT2
1.15C, DT2 1.16 or DN2 1.11 (this module's classification of every
literal-operand give/give_b site matched scratch/semscan.py's Ghidra-based
one exactly on all three), but a build where it matters would need the
Ghidra-based tool to catch it.

Covers literal-operand posters only. A give/give_b call whose semaphore
argument is a register (a computed address, not `pea #imm32`) is not found
here at all -- e.g. worker_done_sem's post and the eSDHC driver's
sd_cmd_sem/sd_data_sem/sd_dma_sem posts. Those still need the existing
Sig/Operand resolution in emu/symbols.py and stay hand-added by the caller.
"""
import hashlib
import struct

from dt2.coldfire import disasm
from emu import symbols

GIVE = 0x4000148c
GIVE_B = 0x400014fc
VBR = 0x40000000
VBR_END = VBR + 0x400
RTE = 0x4e73
RTS = 0x4e75
MAX_ISR_DEPTH = 3
SPAN_MAX_BYTES = 0x4000    # bound on one ISR/callee body scan (largest seen: well under 1K)

# Byte-level heuristics. Both mirror scratch/semscan.py's own approximations
# (see its SCAN_BACK / immediate_arg): a fixed backward byte window rather
# than a true instruction-boundary walk, justified there and here by the
# instruction sequences involved being short and straight-line.
GIVE_ARG_BACKSCAN = 24     # `pea #imm32` lookback before a give/give_b call
VEC_REG_BACKSCAN = 64      # `move.l #imm32,Dn` lookback before a register-form vector store
ANCHOR_WINDOW = 4096       # function-prologue lookback used to validate a vector-store candidate

_cache = {}   # (image sha256, load_addr, modeled_vectors) -> frozenset[int]


# --------------------------------------------------------------------------
# Step 1: vector table writes.
# --------------------------------------------------------------------------

def _find_prologue_anchor(data, base, site, window=ANCHOR_WINDOW):
    """Nearest preceding function prologue (`lea -N(a7),a7`, a `movem`
    register save, or `link`), word-aligned, within `window` bytes before
    `site`. -> guest address, or None."""
    off = site - base
    lo = max(0, off - window)
    for p in range(off - 2, lo - 1, -2):
        if p + 4 > len(data):
            continue
        w = struct.unpack_from('>H', data, p)[0]
        if w == 0x4fef and struct.unpack_from('>H', data, p + 2)[0] & 0x8000:
            return base + p
        if w in (0x48e7, 0x48d7) or 0x4e50 <= w <= 0x4e57:
            return base + p
    return None


def _confirm_instruction(data, base, site, expect_mn):
    """True if disassembling forward from the nearest preceding prologue
    anchor lands exactly on `site` as an instruction boundary with mnemonic
    `expect_mn`. See the module docstring for why this check exists."""
    anchor = _find_prologue_anchor(data, base, site)
    if anchor is None:
        return False
    for pc, _hx, mn, _ops in disasm(data, base, anchor, site + 12):
        if pc == site:
            return mn == expect_mn
        if pc > site:
            return False
    return False


def _find_vectors(data, base):
    """Byte-pattern scan for writes into the RAM vector table, each
    confirmed by _confirm_instruction. -> list of
    {'site', 'handler', 'vector'} dicts."""
    end = base + len(data)
    candidates = []

    # move.l #imm32,(abs32).L -- opcode 0x23FC, imm32, abs32 (10 bytes).
    start = 0
    while True:
        i = data.find(b'\x23\xfc', start)
        if i < 0:
            break
        if i + 10 <= len(data):
            handler = struct.unpack_from('>I', data, i + 2)[0]
            slot = struct.unpack_from('>I', data, i + 6)[0]
            if (VBR <= slot < VBR_END and slot % 4 == 0
                    and base <= handler < end and handler % 2 == 0):
                candidates.append({'site': base + i, 'handler': handler,
                                   'vector': (slot - VBR) // 4})
        start = i + 1

    # move.l Dn,(abs32).L -- opcode 0x23C0+n, abs32 (6 bytes). The handler
    # is whatever was last loaded into Dn by a `move.l #imm32,Dn` (opcode
    # 0x203C+n*0x200) within VEC_REG_BACKSCAN bytes before the store.
    for n in range(8):
        op = struct.pack('>H', 0x23c0 + n)
        load_op = struct.pack('>H', 0x203c + n * 0x200)
        start = 0
        while True:
            i = data.find(op, start)
            if i < 0:
                break
            if i + 6 <= len(data):
                slot = struct.unpack_from('>I', data, i + 2)[0]
                if VBR <= slot < VBR_END and slot % 4 == 0:
                    lo = max(0, i - VEC_REG_BACKSCAN)
                    window = data[lo:i]
                    j = window.rfind(load_op)
                    if j >= 0 and j + 6 <= len(window):
                        handler = struct.unpack_from('>I', window, j + 2)[0]
                        if base <= handler < end and handler % 2 == 0:
                            candidates.append({'site': base + i, 'handler': handler,
                                               'vector': (slot - VBR) // 4})
            start = i + 1

    return [c for c in candidates
            if _confirm_instruction(data, base, c['site'], 'move.l')]


# --------------------------------------------------------------------------
# Step 2: forward reachability from each ISR entry point.
# --------------------------------------------------------------------------

def _resolve_call_target(ops):
    """-> absolute target address for a jsr/bsr operand string, or None
    (a register or indexed target is not followed -- see the module
    docstring's shared-subroutine blind spot)."""
    ops = ops.strip()
    if ops.endswith('.l') and ops.startswith('$'):
        try:
            return int(ops[1:-2], 16)
        except ValueError:
            return None
    if ops.endswith('(pc)') and ops.startswith('$'):
        # capstone prints the already-resolved absolute address for m68k
        # pc-relative operands, not a raw displacement.
        try:
            return int(ops[1:-4], 16)
        except ValueError:
            return None
    return None


def _scan_span(data, base, start, stop_word, max_bytes=SPAN_MAX_BYTES):
    """Linear scan from `start` to the first instruction whose raw opcode
    word equals `stop_word` (RTE or RTS). -> (pcs, calls): pcs is the set of
    instruction-start addresses covered (including the terminator), calls
    is a list of resolved jsr/bsr targets (unresolved ones are dropped, see
    _resolve_call_target)."""
    end = min(base + len(data), start + max_bytes)
    pcs = set()
    calls = []
    for pc, hx, mn, ops in disasm(data, base, start, end):
        pcs.add(pc)
        if mn == 'jsr' or mn.startswith('bsr'):
            target = _resolve_call_target(ops)
            if target is not None:
                calls.append(target)
        if len(hx) >= 4 and int(hx[:4], 16) == stop_word:
            break
    return pcs, calls


def _isr_reachable(data, base, vectors):
    """-> dict {vector_number: frozenset(reachable instruction addresses)},
    one entry per distinct vector in `vectors`, from a depth-limited forward
    walk starting at that vector's handler (shared handlers naturally
    produce identical reachable sets)."""
    by_handler = {}

    def walk(addr, depth, stop_word, visited):
        if addr in visited or depth > MAX_ISR_DEPTH:
            return set()
        visited.add(addr)
        pcs, calls = _scan_span(data, base, addr, stop_word)
        for target in calls:
            if base <= target < base + len(data):
                pcs |= walk(target, depth + 1, RTS, visited)
        return pcs

    result = {}
    for v in vectors:
        handler = v['handler']
        if handler not in by_handler:
            by_handler[handler] = walk(handler, 0, RTE, set())
        result.setdefault(v['vector'], set()).update(by_handler[handler])
    return {vec: frozenset(pcs) for vec, pcs in result.items()}


# --------------------------------------------------------------------------
# Step 3: give/give_b post sites (copied from scratch/semscan.py).
# --------------------------------------------------------------------------

def _find_call_sites(data, base, target):
    """jsr/jmp <target> sites, plus register-indirect jsr/jmp (An) sites
    reached through a `lea <target>.L,An` (the RTOS-mutex-style call
    pattern give_b's only caller uses). -> guest addresses of the
    jsr/jmp instruction itself."""
    out = set()
    for op in (b'\x4e\xb9', b'\x4e\xf9'):        # jsr abs32.L / jmp abs32.L
        needle = op + struct.pack('>I', target)
        start = 0
        while True:
            i = data.find(needle, start)
            if i < 0:
                break
            out.add(base + i)
            start = i + 1
    for an in range(8):
        lea_op = 0x41f9 + an * 0x0200
        needle = struct.pack('>H', lea_op) + struct.pack('>I', target)
        start = 0
        while True:
            i = data.find(needle, start)
            if i < 0:
                break
            window = data[i + 6:i + 6 + 0x300]
            for needle2 in (struct.pack('>H', 0x4e90 + an),
                            struct.pack('>H', 0x4ec0 + an)):
                j, wstart = 0, 0
                while True:
                    j = window.find(needle2, wstart)
                    if j < 0:
                        break
                    out.add(base + i + 6 + j)
                    wstart = j + 1
            start = i + 1
    return sorted(out)


def _immediate_arg(data, base, site):
    """The nearest `pea.l #imm32` within GIVE_ARG_BACKSCAN bytes before
    `site`, if any -> imm32, else None (register argument, not classified
    -- see the module docstring)."""
    off = site - base
    lo = max(0, off - GIVE_ARG_BACKSCAN)
    window = data[lo:off]
    best = None
    for w in range(0, len(window) - 5):
        if window[w:w + 2] == b'\x48\x79':
            best = struct.unpack('>I', window[w + 2:w + 6])[0]
    return best


# --------------------------------------------------------------------------
# Public API.
# --------------------------------------------------------------------------

def never_fake_semaphores(image, load_addr=symbols.LOAD_ADDR, modeled_vectors=frozenset()):
    """-> frozenset[int]: guest addresses of every semaphore with a
    literal-operand give/give_b poster that can run in the emulator --
    ordinary task code, or an ISR whose vector is in `modeled_vectors`.

    This is the image-only replacement for the give/give_b half of
    scratch/semscan.py's methodology (see that module and the docstring
    above for the Ghidra-based version this reproduced exactly on DT2
    1.15C, DT2 1.16 and DN2 1.11). It does NOT find:

      * a poster whose semaphore argument is a register rather than a
        literal (worker_done_sem, the eSDHC sd_*_sem trio) -- resolve those
        the existing way and add them to the caller's never-fake set
        separately;
      * a semaphore that must be fakeable for PART of a run and protected
        for the rest, like emu/longrun.py's frame_sem (fakeable during the
        intro, never-fake after intro_done switches its poster away) --
        that needs the caller's own dynamic handling, not a static set.

    Cached per (image sha256, load_addr, modeled_vectors) the way
    emu.symbols.resolve caches per image sha256 -- this scan reruns a
    handful of times per process otherwise, on the same handful of images.
    """
    h = hashlib.sha256(image).hexdigest()
    key = (h, load_addr, frozenset(modeled_vectors))
    cached = _cache.get(key)
    if cached is not None:
        return cached

    vectors = _find_vectors(image, load_addr)
    reach_by_vector = _isr_reachable(image, load_addr, vectors)
    all_isr_pcs = frozenset().union(*reach_by_vector.values()) if reach_by_vector else frozenset()
    modeled_isr_pcs = frozenset().union(
        *(pcs for vec, pcs in reach_by_vector.items() if vec in modeled_vectors)
    ) if reach_by_vector else frozenset()

    never_fake = set()
    for target in (GIVE, GIVE_B):
        for site in _find_call_sites(image, load_addr, target):
            sem = _immediate_arg(image, load_addr, site)
            if sem is None:
                continue
            if site not in all_isr_pcs or site in modeled_isr_pcs:
                never_fake.add(sem)

    result = frozenset(never_fake)
    _cache[key] = result
    return result
