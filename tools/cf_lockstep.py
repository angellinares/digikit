#!/usr/bin/env python3
"""Lockstep the native ColdFire interpreter (native/coldfire, via the
`cfabi` cdylib) against the patched Unicorn oracle: load a machine state,
step both cores one instruction at a time, and compare every register and
any memory either side wrote. Stops at the first difference and prints the
instruction, its operands, and expected vs. actual.

    uv run python tools/cf_lockstep.py snap SNAP [--limit N] [--at ADDR]
    uv run python tools/cf_lockstep.py fuzz IMAGE [--limit N] [--seed N]

`snap` starts from an emu snapshot (tools/snapread.py); `fuzz` starts from
real opwords sampled from a built image (out/sections/IMAGE) with
random register content, a lighter-weight substitute for a real execution
window when no snapshot is at hand (also exercises more of the forms in one
run, since a snapshot window mostly repeats a live loop's hot path).

Peripheral (MMIO) reads: this only compares the CPU, not the machine's
peripherals (docs/plan-native-emulator.md P3 stage 2), so every memory read
Unicorn performs while executing the instruction is captured with a
UC_HOOK_MEM_READ hook (reading the backing bytes back out, since this
Unicorn build rejects UC_HOOK_MEM_READ_AFTER) and replayed into the Rust
core as a one-shot
override (cf_set_override) before it runs the same instruction -- whether
the address is RAM (redundant, but harmless) or a peripheral register
(otherwise unmodelled here). A read the override table still holds after
the step (cf_pending_overrides) means Unicorn read an address ours did not,
which is itself reported as a divergence.

Known gap: an emu snapshot's `regs` only holds D0-7/A0-7/PC/SR
(emu/snapshot.py's REGS tuple) -- not OTHER_A7 or any EMAC register (MACSR/
ACCn/ACCextnn/MASK), since nothing needed them before this harness, and
emu/ is out of this lane's ownership. `snap` windows therefore start both
cores' EMAC state at its architectural reset value (MACSR=0, ACC=0,
MASK=0xffff), which is right if the window's own code loads MACSR before
using it (the common pattern -- CFPRM's own EMAC_state_save/restore does
this) but wrong if it inherits EMAC context set up before the window
starts. `fuzz` does not have this gap: it seeds MACSR/ACC/MASK randomly
per instruction, subject to the same MACSR probe limitation below.

Known gap: this Unicorn build exposes no UC_M68K_REG_MACSR/ACC/ACCEXT/MASK
constants (only the integer registers, SR, and the CR_* MMU set -- checked
against unicorn.m68k_const directly), so EMAC register state cannot be
read back or loaded through the normal reg_read/reg_write API. Both `snap`
and `fuzz` therefore only lockstep-check the integer registers, SR and
memory across an EMAC instruction, not the resulting MACSR/ACCn/MASK
values; getting those under lockstep needs a probe-instruction technique
(inject "move.l accN,Dn" etc. at a scratch PC and read Dn back, mirroring
CFPRM's own EMAC_state_save routine) that is not implemented yet.
"""

from __future__ import annotations

import argparse
import ctypes
import importlib
import random
import struct
import subprocess
import sys
from collections.abc import Callable
from pathlib import Path
from typing import Any, cast

ROOT = Path(__file__).resolve().parents[1]
CRATE = ROOT / "native" / "coldfire" / "cfabi"
LIBNAME = {"darwin": "libcfabi.dylib", "linux": "libcfabi.so"}.get(
    sys.platform, "libcfabi.so"
)

_tools = str(ROOT / "tools")
if _tools not in sys.path:
    sys.path.insert(0, _tools)

from snapread import Snapshot  # noqa: E402

# ---------------------------------------------------------------------------
# cfabi ctypes binding


class CfRegs(ctypes.Structure):
    _fields_ = [
        ("d", ctypes.c_uint32 * 8),
        ("a", ctypes.c_uint32 * 8),
        ("other_a7", ctypes.c_uint32),
        ("pc", ctypes.c_uint32),
        ("sr", ctypes.c_uint32),
        ("vbr", ctypes.c_uint32),
        ("cacr", ctypes.c_uint32),
        ("asid", ctypes.c_uint32),
        ("acr", ctypes.c_uint32 * 8),
        ("mmubar", ctypes.c_uint32),
        ("rgpiobar", ctypes.c_uint32),
        ("rambar", ctypes.c_uint32),
        ("macsr", ctypes.c_uint32),
        ("acc", ctypes.c_uint32 * 4),
        ("accext01", ctypes.c_uint32),
        ("accext23", ctypes.c_uint32),
        ("mask", ctypes.c_uint32),
    ]


STEP_OK, STEP_EXCEPTION, STEP_UNIMPLEMENTED, STEP_HALTED = 0, 1, 2, 3
STEP_NAMES = {
    STEP_OK: "ok",
    STEP_EXCEPTION: "exception",
    STEP_UNIMPLEMENTED: "unimplemented",
    STEP_HALTED: "halted",
}


def build_cfabi() -> Path:
    subprocess.run(["cargo", "build", "--release", "--quiet"], cwd=CRATE, check=True)
    return CRATE / "target" / "release" / LIBNAME


def load_cfabi(path: Path) -> ctypes.CDLL:
    lib = ctypes.CDLL(str(path))
    lib.cf_new.restype = ctypes.c_void_p
    lib.cf_free.argtypes = [ctypes.c_void_p]
    lib.cf_get_regs.argtypes = [ctypes.c_void_p, ctypes.POINTER(CfRegs)]
    lib.cf_set_regs.argtypes = [ctypes.c_void_p, ctypes.POINTER(CfRegs)]
    lib.cf_write_mem.argtypes = [
        ctypes.c_void_p,
        ctypes.c_uint32,
        ctypes.c_char_p,
        ctypes.c_size_t,
    ]
    lib.cf_read_mem.argtypes = [
        ctypes.c_void_p,
        ctypes.c_uint32,
        ctypes.c_char_p,
        ctypes.c_size_t,
    ]
    lib.cf_read_mem.restype = ctypes.c_int32
    lib.cf_set_override.argtypes = [
        ctypes.c_void_p,
        ctypes.c_uint32,
        ctypes.c_uint8,
        ctypes.c_uint32,
    ]
    lib.cf_pending_overrides.argtypes = [ctypes.c_void_p]
    lib.cf_pending_overrides.restype = ctypes.c_size_t
    lib.cf_clear_overrides.argtypes = [ctypes.c_void_p]
    lib.cf_step.argtypes = [ctypes.c_void_p]
    lib.cf_step.restype = ctypes.c_int32
    lib.cf_last_vector.argtypes = [ctypes.c_void_p]
    lib.cf_last_vector.restype = ctypes.c_int32
    lib.cf_last_form.argtypes = [ctypes.c_void_p]
    lib.cf_last_form.restype = ctypes.c_int32
    lib.cf_icount.argtypes = [ctypes.c_void_p]
    lib.cf_icount.restype = ctypes.c_uint64
    return lib


def regs_to_dict(r: CfRegs, gp_only: bool = False) -> dict[str, int | list[int]]:
    """Register values keyed by name. `gp_only` (used for the comparison
    against Unicorn) leaves out OTHER_A7 and every EMAC register: Unicorn
    exposes no register IDs for them (see the module docstring), so this
    harness cannot set or read them on the oracle side at all."""
    out: dict[str, int | list[int]] = {"d%d" % i: int(r.d[i]) for i in range(8)}
    out.update({"a%d" % i: int(r.a[i]) for i in range(8)})
    out["pc"] = int(r.pc)
    out["sr"] = int(r.sr)
    if gp_only:
        return out
    out["other_a7"] = int(r.other_a7)
    out["macsr"] = int(r.macsr)
    out["acc"] = list(r.acc)
    out["accext01"] = int(r.accext01)
    out["accext23"] = int(r.accext23)
    out["mask"] = int(r.mask)
    return out


class Core:
    """One cfabi instance: a Cpu plus its sparse memory."""

    def __init__(self, lib: ctypes.CDLL):
        self.lib = lib
        self.handle = lib.cf_new()

    def close(self):
        self.lib.cf_free(self.handle)
        self.handle = None

    def get_regs(self) -> CfRegs:
        r = CfRegs()
        self.lib.cf_get_regs(self.handle, ctypes.byref(r))
        return r

    def set_regs(self, r: CfRegs):
        self.lib.cf_set_regs(self.handle, ctypes.byref(r))

    def write_mem(self, addr: int, data: bytes):
        self.lib.cf_write_mem(self.handle, addr, data, len(data))

    def read_mem(self, addr: int, n: int) -> bytes | None:
        buf = ctypes.create_string_buffer(n)
        if self.lib.cf_read_mem(self.handle, addr, buf, n) != 0:
            return None
        return buf.raw

    def set_override(self, addr: int, size: int, value: int):
        self.lib.cf_set_override(self.handle, addr, size, value)

    def pending_overrides(self) -> int:
        return self.lib.cf_pending_overrides(self.handle)

    def clear_overrides(self):
        self.lib.cf_clear_overrides(self.handle)

    def step(self) -> int:
        return self.lib.cf_step(self.handle)

    def last_vector(self) -> int:
        return self.lib.cf_last_vector(self.handle)

    def last_form(self) -> int:
        return self.lib.cf_last_form(self.handle)

    def icount(self) -> int:
        return self.lib.cf_icount(self.handle)


# ---------------------------------------------------------------------------
# Unicorn oracle


def unicorn_machine():
    from unicorn import UC_ARCH_M68K, UC_MODE_BIG_ENDIAN
    from unicorn import m68k_const as K
    from unicorn.unicorn import Uc

    uc = Uc(UC_ARCH_M68K, UC_MODE_BIG_ENDIAN)
    uc.ctl_set_cpu_model(K.UC_CPU_M68K_CFV4E)
    # Hooks are added once and reused for every step (StepTrace.reset()
    # clears them instead): adding and deleting three hooks per
    # instruction, tens of thousands of times, reliably segfaults inside
    # this Unicorn build after a few thousand steps.
    trace = StepTrace()
    trace.install(uc)
    return uc, K, trace


def uc_get_regs(uc, K) -> CfRegs:
    r = CfRegs()
    for i in range(8):
        r.d[i] = uc.reg_read(getattr(K, "UC_M68K_REG_D%d" % i))
        r.a[i] = uc.reg_read(getattr(K, "UC_M68K_REG_A%d" % i))
    r.pc = uc.reg_read(K.UC_M68K_REG_PC)
    r.sr = uc.reg_read(K.UC_M68K_REG_SR)
    return r


def uc_set_regs(uc, K, r: CfRegs):
    # SR first: writing it while S=1 swaps in QEMU's internal SSP/USP shadow
    # register over whatever A7 already held (confirmed directly -- writing
    # A7 then SR silently zeroes A7 back out), so any address register
    # write must come after it, not before.
    uc.reg_write(K.UC_M68K_REG_SR, int(r.sr))
    for i in range(8):
        uc.reg_write(getattr(K, "UC_M68K_REG_D%d" % i), int(r.d[i]))
        uc.reg_write(getattr(K, "UC_M68K_REG_A%d" % i), int(r.a[i]))
    uc.reg_write(K.UC_M68K_REG_PC, int(r.pc))


class StepTrace:
    """Reads and writes Unicorn performed during exactly one emu_start.
    Hooks are installed once (`install`) and the trace is `reset` before
    each step, rather than adding/deleting hooks per step: this Unicorn
    build reliably segfaults after a few thousand hook_add/hook_del cycles.
    """

    def __init__(self):
        self.reads: list[tuple[int, int, int]] = []  # (addr, size, value)
        # Kept for existing CPU lockstep users, which compare write locations.
        self.writes: list[tuple[int, int]] = []  # (addr, size)
        self.write_values: list[tuple[int, int, int]] = []  # (addr, size, value)
        self.exception = False
        self.unmapped = False

    def reset(self):
        self.reads.clear()
        self.writes.clear()
        self.write_values.clear()
        self.exception = False
        self.unmapped = False

    def install(self, uc):
        from unicorn import UC_HOOK_INTR, UC_HOOK_MEM_READ, UC_HOOK_MEM_WRITE

        uc.hook_add(UC_HOOK_MEM_READ, self._read_hook)
        uc.hook_add(UC_HOOK_MEM_WRITE, self._write_hook)
        uc.hook_add(UC_HOOK_INTR, self._intr_hook)

    def _read_hook(self, uc, access, address, size, value, ud):
        # UC_HOOK_MEM_READ fires before the read completes, so `value` is
        # not populated (UC_HOOK_MEM_READ_AFTER, which would give it to us
        # directly, is rejected by this Unicorn build with UC_ERR_ARG).
        # The backing byte array already holds the value the CPU is about
        # to read, so read it back through the normal memory API instead.
        raw = uc.mem_read(address, size)
        self.reads.append((address, size, int.from_bytes(raw, "big")))

    def _write_hook(self, uc, access, address, size, value, ud):
        self.writes.append((address, size))
        self.write_values.append((address, size, value))

    def _intr_hook(self, uc, intno, ud):
        self.exception = True
        uc.emu_stop()


def run_uc_step(uc, K, trace: StepTrace, pc: int) -> StepTrace:
    from unicorn.unicorn import UcError

    trace.reset()
    try:
        uc.emu_start(pc, 0xFFFFFFFF, count=1)
    except UcError:
        # A real access to memory neither harness mode maps (genuine
        # peripheral MMIO in real firmware code, or a fuzzed register
        # landing outside the scratch region): out of scope here, same as
        # an interrupt -- this core is not being asked to model peripherals.
        trace.unmapped = True
    return trace


# ---------------------------------------------------------------------------
# comparison


def diff_regs(expected: CfRegs, actual: CfRegs) -> list[str]:
    # gp_only=True: every value is an int (see regs_to_dict), never the
    # "acc" list, but the shared return type covers both.
    e, a = regs_to_dict(expected, gp_only=True), regs_to_dict(actual, gp_only=True)
    out = []
    for f, ev in e.items():
        av = a[f]
        # `gp_only` excludes the sole list-valued register field (`acc`).
        expected_value = cast(int, ev)
        actual_value = cast(int, av)
        if expected_value != actual_value:
            out.append(
                "%s: expected 0x%x, got 0x%x" % (f, expected_value, actual_value)
            )
    return out


def is_unicorn_mvz_n_defect(
    core: Core, pc: int, expected: CfRegs, actual: CfRegs
) -> bool:
    """Whether this is Unicorn's known MVZ N-flag defect, and nothing else.

    CFPRM p.125 says MVZ always clears N.  This Unicorn's ColdFire model sets
    it for a nonzero byte result instead.  Keep the exemption opcode-specific
    and require the otherwise-identical SR, so it cannot hide a core defect.
    """
    raw = core.read_mem(pc, 2)
    if raw is None:
        return False
    opword = int.from_bytes(raw, "big")
    is_mvz = opword & 0xF180 == 0x7180  # 0111 ddd1 1smm mrrr, CFPRM p.125
    sr_diff = int(expected.sr) ^ int(actual.sr)
    return is_mvz and sr_diff == 0x0008 and not int(actual.sr) & 0x0008


def run_window(
    core: Core,
    uc,
    K,
    trace: StepTrace,
    start_pc: int,
    limit: int,
    verbose: bool = False,
) -> tuple[int, str | None]:
    """Step both cores from their already-matching current state. Returns
    (instructions compared, None) or (instructions compared, failure
    message) at the first divergence or the first side that stops."""
    n = 0
    pc = start_pc
    while n < limit:
        trace = run_uc_step(uc, K, trace, pc)
        if trace.exception or trace.unmapped:
            return n, None  # out of scope for now (see module docstring)
        for addr, size, value in trace.reads:
            core.set_override(addr, size, value)
        rc = core.step()
        pending = core.pending_overrides()
        core.clear_overrides()
        if rc == STEP_UNIMPLEMENTED:
            return n, "unimplemented form id %d at pc=0x%08x" % (
                core.last_form(),
                pc,
            )
        if rc == STEP_HALTED:
            return n, "core halted at pc=0x%08x" % pc
        if rc == STEP_EXCEPTION:
            # Unicorn did not report one (trace.exception is False here);
            # a real divergence, not an in-scope skip.
            return (
                n,
                "our core took an exception (vector %d) Unicorn did not, at pc=0x%08x"
                % (
                    core.last_vector(),
                    pc,
                ),
            )
        expected = uc_get_regs(uc, K)
        actual = core.get_regs()
        bad = diff_regs(expected, actual)
        for addr, size in trace.writes:
            uc_bytes = uc.mem_read(addr, size)
            our_bytes = core.read_mem(addr, size)
            if our_bytes != uc_bytes:
                bad.append(
                    "mem[0x%08x:%d]: expected %s, got %s"
                    % (addr, size, uc_bytes.hex(), (our_bytes or b"").hex())
                )
        if pending:
            bad.append(
                "%d memory read(s) our core never made (Unicorn read something ours did not)"
                % pending
            )
        if (
            len(bad) == 1
            and bad[0].startswith("sr: ")
            and is_unicorn_mvz_n_defect(core, pc, expected, actual)
        ):
            # Repair only Unicorn's bad N bit before the next instruction;
            # leaving it set would turn every following comparison into the
            # same known oracle defect and could change a later branch.  Never
            # suppress a simultaneous memory or register mismatch.
            uc.reg_write(K.UC_M68K_REG_SR, int(actual.sr))
            bad = []
        if bad:
            msg = "divergence after %d instructions at pc=0x%08x:\n  " % (
                n,
                pc,
            ) + "\n  ".join(bad)
            return n, msg
        if verbose and n % 100000 == 0:
            print("  %d instructions, pc=0x%08x" % (n, pc), file=sys.stderr)
        n += 1
        pc = int(expected.pc)
    return n, None


# ---------------------------------------------------------------------------
# snap subcommand


def cmd_snap(args) -> int:
    snap = Snapshot(args.snap)
    lib = load_cfabi(build_cfabi())
    core = Core(lib)
    uc, K, trace = unicorn_machine()

    regs = snap.regs
    r = CfRegs()
    for i in range(8):
        r.d[i] = regs["d%d" % i] & 0xFFFFFFFF
        r.a[i] = regs["a%d" % i] & 0xFFFFFFFF
    r.pc = args.at if args.at is not None else regs["pc"]
    r.sr = regs["sr"] & 0xFFFF
    r.mask = 0xFFFF  # EMAC reset value (RM p.90 Table 3-1); see module docstring
    core.set_regs(r)
    import contextlib

    from unicorn.unicorn import UcError

    for base in snap.mapped_bases:
        data = snap.read(base, 0x100000)
        if data is None:
            continue
        with contextlib.suppress(UcError):
            uc.mem_map(
                base, len(data)
            )  # UcError: already covered by an adjoining entry
        uc.mem_write(base, data)
        core.write_mem(base, data)
    uc_set_regs(uc, K, r)

    n, err = run_window(core, uc, K, trace, r.pc, args.limit, verbose=args.verbose)
    print("%s: %d instructions compared" % (args.snap, n))
    core.close()
    if err:
        print(err)
        return 1
    return 0


# ---------------------------------------------------------------------------
# fuzz subcommand: real opwords from a built image, random register content


def cmd_fuzz(args) -> int:
    _cfisa = str(ROOT / "tools" / "cfisa")
    if _cfisa not in sys.path:
        sys.path.insert(0, _cfisa)
    oracle = importlib.import_module(
        "oracle"
    )  # tools/cfisa/oracle.py: image_paths/load_base/listing

    image_path, dump = oracle.image_paths(args.image)
    if not image_path.exists() or not dump.exists():
        print(
            "missing %s or %s (extract + ghidradump the firmware first)"
            % (image_path, dump)
        )
        return 1
    image = image_path.read_bytes()
    base = oracle.load_base(dump)
    # Real instruction addresses only (tools/ghidradump.py's listing), not
    # arbitrary byte offsets: fuzzing truly random bytes as opwords mostly
    # hits undefined/non-ColdFire encodings and reserved-bit combinations
    # whose real-hardware behaviour the manual does not define, rather than
    # the 101 forms the firmware (and this stage's semantics) actually cover.
    addrs = sorted(oracle.listing(dump))
    # MOVEC (confirmed here: MOVEC RGPIOBAR calls QEMU's cpu_abort, killing
    # the whole process, not raising a catchable error) and the other forms
    # the P3 stage-1 oracle sweep already found this Unicorn build aborts or
    # cpu_aborts on (WDEBUG; FSAVE; every FPU-unit form, since the firmware
    # never uses the FPU and QEMU's own FBcc/FSAVE handling is where that
    # sweep found the aborts) are decoded here and dropped before any
    # Unicorn call is made for that address, rather than caught -- an abort
    # cannot be caught. JSR/JMP are excluded for a harness reason, not an
    # oracle defect: with a random address register as the target, they
    # jump into the zero-filled scratch region, and this build then
    # reliably segfaults a few hundred thousand single-step calls later
    # (reproduced repeatedly; not narrowed past "involves a register-
    # indirect call/jump into unexecuted memory", so possibly this
    # repo's own count-hook-fast-path patch mishandling a call). Fuzzing
    # them against a nonsense destination is not useful anyway; JSR/JMP's
    # own semantics (the call/jump and the return-address push) are
    # covered by tests/cpu.rs instead.
    # EMAC register moves (movclr, move to/from ACCn/ACCextnn/MACSR/MASK,
    # MAC/MSAC): confirmed here too -- e.g. "movclr acc0,d0" leaves D0
    # exactly as this harness set it on the Unicorn side (no GPR write at
    # all), consistent with the module docstring's "no UC_M68K_REG_MACSR/
    # ACC/ACCEXT/MASK" finding extending to these forms not being usable
    # against this Unicorn build even indirectly via a GPR. Not a lockstep
    # divergence; there is nothing here for this harness to check.
    import json

    table = json.loads((ROOT / "tools" / "cfisa" / "coldfire.json").read_text())
    incomparable_forms = {"movec", "wdebug", "fsave", "jsr", "jmp"} | {
        f["id"] for f in table["forms"] if f["unit"] in ("fpu", "emac")
    }
    decoded = oracle.ours(image_path, base, addrs)
    addrs = [a for a in addrs if decoded[a][1] not in incomparable_forms]

    lo = base & ~0xFFF
    hi = (base + len(image) + 0xFFF) & ~0xFFF
    # Generous and mapped on both sides of the middle: an address register
    # plus a +/-32K displacement (the largest a (d16,An) EA can add) still
    # lands inside it, so an out-of-range access is a real divergence, not
    # scratch-space noise.
    scratch_size = 0x40000
    scratch = hi
    scratch_mid = scratch + scratch_size // 2

    lib = load_cfabi(build_cfabi())

    def fresh_core():
        core = Core(lib)
        core.write_mem(base, image)
        core.write_mem(scratch, bytes(scratch_size))
        return core

    def fresh_uc():
        uc, K, trace = unicorn_machine()
        uc.mem_map(0, 0x1000)
        uc.mem_map(lo, hi - lo)
        uc.mem_write(base, image)
        uc.mem_map(scratch, scratch_size)
        return uc, K, trace

    # This Unicorn build segfaults, unpredictably (seen from ~15,000 up to
    # ~320,000 single-instruction emu_start calls on one Uc instance across
    # different runs), so both the Uc instance and the cfabi core are
    # periodically rebuilt from scratch -- confirmed stable across several
    # full runs at this interval, not just to the point of one crash.
    RECYCLE_EVERY = 10000
    uc, K, trace = fresh_uc()
    core = fresh_core()

    rnd = random.Random(args.seed)
    n_ok = 0
    divergences = []
    rnd.shuffle(addrs)
    for count, pc in enumerate(addrs[: args.limit]):
        if count and count % RECYCLE_EVERY == 0:
            uc, K, trace = fresh_uc()
            core.close()
            core = fresh_core()
        r = CfRegs()
        for i in range(8):
            r.d[i] = rnd.getrandbits(32)
            r.a[i] = scratch_mid + ((rnd.getrandbits(15) - 0x4000) & ~3)
        r.a[7] = scratch_mid
        r.pc = pc
        r.sr = 0x2700  # supervisor, interrupts masked (privileged forms decode)
        core.clear_overrides()
        core.set_regs(r)
        uc_set_regs(uc, K, r)
        if args.trace:
            print("count=%d pc=0x%08x" % (count, pc), file=sys.stderr, flush=True)
        n, err = run_window(core, uc, K, trace, pc, 1)
        if err and "unimplemented form" not in err:
            divergences.append(err)
            if len(divergences) >= args.max_failures:
                break
        n_ok += n
    core.close()
    print(
        "%s: %d/%d random-state single steps agreed, %d divergence(s)"
        % (args.image, n_ok, len(addrs[: args.limit]), len(divergences))
    )
    for d in divergences[:20]:
        print(d)
    return 1 if divergences else 0


# ---------------------------------------------------------------------------
# emac subcommand: EMAC semantics against the oracle via guest probes.
#
# Unicorn exposes no MACSR/ACC/MASK register API (module docstring above), so
# EMAC state is observed the way CFPRM's own EMAC_state_save/restore routine
# does: guest instructions copy it into GPRs, which Unicorn's register API
# CAN read. tests/test_unicorn_emac.py already validates that this Unicorn
# build executes those instructions correctly (movclr, moves to/from ACCn/
# MACSR/MASK/ACCext, MAC/MSAC with and without load, fractional mode) against
# CFPRM's own pseudocode; this subcommand runs the SAME technique through
# BOTH engines via the existing lockstep machinery (run_window), so any
# divergence in native/coldfire's EMAC semantics shows up the same way a
# fuzz/snap divergence does. Every case runs:
#   setup (8 words) -- move Dn into ACC0-3, ACCext01/23, MASK, MACSR, in
#                       that order, MACSR LAST: MOVE-to-ACCn itself rewrites
#                       MACSR's N/Z/PAV bits (native/coldfire/src/cpu.rs
#                       Form::MoveToAcc, CFPRM p.184), so setting MACSR after
#                       the ACCn/ext/mask loads is the only way to hand the
#                       instruction under test the exact MACSR value the
#                       case asked for.
#   test  (1-3 words) -- the EMAC form under test.
#   probe (8 words)   -- move ACC0-3, MACSR, MASK, ACCext01/23 back into Dn,
#                        non-destructively (MOVCLR is only ever the
#                        instruction UNDER TEST here, never the probe, so
#                        its own clear-on-read side effect is itself
#                        checked against the oracle, not assumed correct).
# run_window already lockstep-compares every GPR after every step, so a
# divergence in any of these ~17-19 instructions -- setup, test, or probe --
# is reported, not just the instruction nominally "under test": this also
# exercises every move-to/move-from-Dn form on every single case.
#
# Every encoder below was checked by hand against native/coldfire's own
# decoder before use: build native/coldfire/src/bin/cfdis.rs (`cargo build
# --release --bin cfdis`) and feed it "0 <w0> <w1> <w2>" lines on
# `cfdis --words` stdin; each encoder here reproduces the exact bit
# extractions in native/coldfire/src/decode_gen.rs's f_mac/f_mac_load/
# f_movclr/f_move_to_*/f_move_from_* (the same table cpu.rs's execute()
# dispatches on), and was spot-checked that way, not just derived from
# tools/cfisa/coldfire.json's "enc" strings by inspection.

EMAC_CODE = 0x00300000
EMAC_DATA = 0x00310000
EMAC_DATA_SIZE = 0x2000
EMAC_REG_POOL = list(range(15))  # D0-D7 (0-7), A0-A6 (8-14); A7 stays the SP
EMAC_MODES = [m << 4 for m in range(16)]  # OMC,S/U,F/I,R/T (CFPRM Table 1-5)
EMAC_EDGE32 = (
    0,
    1,
    0x7FFFFFFF,
    0x80000000,
    0xFFFFFFFF,
    0x80000001,
    0x0000FFFF,
    0xFFFF0000,
)


def enc_move_to_macsr(m, r):
    return [0xA900 | (m & 7) << 3 | (r & 7)]


def enc_move_from_macsr(r):
    return [0xA980 | (r & 0xF)]


def enc_move_to_acc(acc, m, r):
    return [0xA100 | (acc & 3) << 9 | (m & 7) << 3 | (r & 7)]


def enc_move_from_acc(acc, r):
    return [0xA180 | (acc & 3) << 9 | (r & 0xF)]


def enc_movclr(acc, r):
    return [0xA1C0 | (acc & 3) << 9 | (r & 0xF)]


def enc_move_to_mask(m, r):
    return [0xAD00 | (m & 7) << 3 | (r & 7)]


def enc_move_from_mask(r):
    return [0xAD80 | (r & 0xF)]


def enc_move_to_accext01(m, r):
    return [0xAB00 | (m & 7) << 3 | (r & 7)]


def enc_move_from_accext01(r):
    return [0xAB80 | (r & 0xF)]


def enc_move_to_accext23(m, r):
    return [0xAF00 | (m & 7) << 3 | (r & 7)]


def enc_move_from_accext23(r):
    return [0xAF80 | (r & 0xF)]


def enc_mac(y, v, x, u, f, z, acc, msac=False):
    """mac/msac without load (CFPRM p.170-171,189-190). y/v is the Ry
    operand (register 0-15, upper/lower half select for a word op); x/u is
    Rx the same way; f is the scale (0 none, 1 <<1, 3 >>1); z is size
    (0=.w, 1=.l); acc is the target accumulator 0-3."""
    w0 = 0xA000 | ((x >> 3 & 1) << 6) | ((x & 7) << 9) | ((acc & 1) << 7) | (y & 0xF)
    w1 = (
        ((z & 1) << 11)
        | ((f & 3) << 9)
        | (0x100 if msac else 0)
        | ((u & 1) << 7)
        | ((v & 1) << 6)
        | ((acc >> 1 & 1) << 4)
    )
    return [w0, w1]


def enc_mac_load(y, v, x, u, f, z, m, r, k, rw, acc, msac=False, disp=None):
    """mac/msac with load (CFPRM p.172-173,191-192). m/r is the memory
    operand's raw mode/register (2=(An), 3=(An)+, 4=-(An), 5=(d16,An), r is
    An 0-7); k is the MASK-applies-to-address flag; rw is the register (0-15)
    the loaded long word lands in; disp is the (d16,An) displacement, added
    as a third word when given. Note the accumulator field's LSB is stored
    inverted (CFPRM p.172-173's own footnote, reproduced in
    native/coldfire/src/decode_gen.rs's f_mac_load)."""
    w0 = (
        0xA000
        | ((rw >> 3 & 1) << 6)
        | ((rw & 7) << 9)
        | (((acc & 1) ^ 1) << 7)
        | ((m & 7) << 3)
        | (r & 7)
    )
    w1 = (
        ((z & 1) << 11)
        | ((f & 3) << 9)
        | (0x100 if msac else 0)
        | ((u & 1) << 7)
        | ((v & 1) << 6)
        | ((k & 1) << 5)
        | ((acc >> 1 & 1) << 4)
        | ((x & 0xF) << 12)
        | (y & 0xF)
    )
    words = [w0, w1]
    if disp is not None:
        words.append(disp & 0xFFFF)
    return words


def _setup_words():
    return [
        enc_move_to_acc(0, 0, 1)[0],
        enc_move_to_acc(1, 0, 2)[0],
        enc_move_to_acc(2, 0, 3)[0],
        enc_move_to_acc(3, 0, 4)[0],
        enc_move_to_accext01(0, 5)[0],
        enc_move_to_accext23(0, 6)[0],
        enc_move_to_mask(0, 7)[0],
        enc_move_to_macsr(0, 0)[0],
    ]


_PROBE_WORDS = [
    enc_move_from_acc(0, 0)[0],
    enc_move_from_acc(1, 1)[0],
    enc_move_from_acc(2, 2)[0],
    enc_move_from_acc(3, 3)[0],
    enc_move_from_macsr(4)[0],
    enc_move_from_mask(5)[0],
    enc_move_from_accext01(6)[0],
    enc_move_from_accext23(7)[0],
]


def _base_state(rnd, edge_i):
    """d[1..7]: ACC0-3, ACCext01, ACCext23, MASK source values (d[0], MACSR,
    is filled in by the caller since it is mode-driven). `edge_i`, if not
    None, selects EMAC_EDGE32 values (cycled with an offset per case) instead
    of random ones."""
    if edge_i is not None:
        return {i + 1: EMAC_EDGE32[(edge_i + i) % len(EMAC_EDGE32)] for i in range(7)}
    return {i: rnd.getrandbits(32) for i in range(1, 8)}


def _rand_a(rnd):
    return [rnd.getrandbits(32) for _ in range(7)]  # A0-A6 default fill


def _macsr_source(rnd, mode, edge_i):
    # Keep bits 31-12 clear: CFPRM Table 1-5 defines them "Reserved, should
    # be cleared", and this patched Unicorn is already known (and pinned by
    # tests/test_unicorn_emac.py's test_known_gap_macsr_read_keeps_high_bits)
    # not to clear them on a MACSR read -- an oracle defect, not a
    # native/coldfire one, so left out of the main sweep to avoid burying
    # real findings under one already-documented gap repeated thousands of
    # times. bits 3-0 (status) and 11-8 (PAVx) are real, software-writable
    # bits (Table 1-5) and are exercised randomly.
    if edge_i is not None:
        return mode
    return mode | rnd.getrandbits(4) | (rnd.getrandbits(4) << 8)


def _gen_mac_form(rnd, mode, msac, edge_i):
    d = _base_state(rnd, edge_i)
    d[0] = _macsr_source(rnd, mode, edge_i)
    y = rnd.choice(EMAC_REG_POOL)
    x = rnd.choice(EMAC_REG_POOL)
    v = rnd.getrandbits(1)
    u = rnd.getrandbits(1)
    f = rnd.choice((0, 1, 3))
    z = rnd.getrandbits(1)
    acc = rnd.randrange(4)
    words = enc_mac(y, v, x, u, f, z, acc, msac=msac)
    note = "acc%d %s.%s r%d.%s,r%d.%s f=%d" % (
        acc,
        "msac" if msac else "mac",
        "w" if z == 0 else "l",
        y,
        "u" if v else "l",
        x,
        "u" if u else "l",
        f,
    )
    return words, d, _rand_a(rnd), [], note


_LOAD_EA = {"ind": 2, "post": 3, "pre": 4, "disp": 5}


def _gen_mac_load_form(rnd, mode, msac, ea_kind, masked, edge_i):
    d = _base_state(rnd, edge_i)
    d[0] = _macsr_source(rnd, mode, edge_i)
    a = _rand_a(rnd)
    y = rnd.choice(EMAC_REG_POOL)
    x = rnd.choice(EMAC_REG_POOL)
    v = rnd.getrandbits(1)
    u = rnd.getrandbits(1)
    f = rnd.choice((0, 1, 3))
    z = rnd.getrandbits(1)
    acc = rnd.randrange(4)
    rw = rnd.choice(EMAC_REG_POOL)
    m = _LOAD_EA[ea_kind]
    loaded_val = (
        rnd.getrandbits(32)
        if edge_i is None
        else EMAC_EDGE32[edge_i % len(EMAC_EDGE32)]
    )
    mem = []
    disp = None
    base = EMAC_DATA + 0x100
    if masked:
        # Exact pattern validated in tests/test_unicorn_emac.py's
        # LOAD_CASES "msac.w with load subtracts and applies MASK":
        # MASK source 0x00000FFF -> self.emac.mask = 0xFFFF0FFF, address
        # EMAC_DATA+0x1010 masked down to EMAC_DATA+0x010.
        d[7] = 0x00000FFF
        target = EMAC_DATA + 0x1010
        masked_addr = target & 0xFFFF0FFF
        a[1] = target  # A1
        mem.append((masked_addr, loaded_val))
        k = 1
    else:
        k = 0
        if ea_kind == "ind" or ea_kind == "post":
            a[1] = base
            mem.append((base, loaded_val))
        elif ea_kind == "pre":
            a[1] = base + 4
            mem.append((base, loaded_val))
        elif ea_kind == "disp":
            a[1] = EMAC_DATA
            disp = 0x100
            mem.append((base, loaded_val))
    words = enc_mac_load(y, v, x, u, f, z, m, 1, k, rw, acc, msac=msac, disp=disp)
    note = "acc%d %s.%s load(%s%s) r%d.%s,r%d.%s rw=r%d" % (
        acc,
        "msac" if msac else "mac",
        "w" if z == 0 else "l",
        ea_kind,
        "+mask" if masked else "",
        y,
        "u" if v else "l",
        x,
        "u" if u else "l",
        rw,
    )
    return words, d, a, mem, note


def _gen_movclr_form(rnd, mode, edge_i):
    d = _base_state(rnd, edge_i)
    d[0] = _macsr_source(rnd, mode, edge_i)
    a = _rand_a(rnd)
    acc = rnd.randrange(4)
    dest = rnd.choice(EMAC_REG_POOL)
    words = enc_movclr(acc, dest)
    note = "movclr.l acc%d,r%d" % (acc, dest)
    return words, d, a, [], note


def _ea_an_or_imm(rnd, a, val, use_imm):
    if use_imm:
        return 7, 4, [(val >> 16) & 0xFFFF, val & 0xFFFF]
    r = rnd.randrange(7)
    a[r] = val
    return 1, r, []


def _gen_move_to_dedicated(rnd, mode, target, use_imm, edge_i):
    """move.l An,<target> / move.l #imm,<target> -- the Dn-source variant of
    every move-to-EMAC form is already exercised by every case's own setup
    (_setup_words), so this covers only the other two valid EA kinds (CFPRM
    p.184-188's EA restriction to Dn/An/Imm)."""
    d = _base_state(rnd, edge_i)
    d[0] = _macsr_source(rnd, mode, edge_i)
    a = _rand_a(rnd)
    val = (
        rnd.getrandbits(32)
        if edge_i is None
        else EMAC_EDGE32[edge_i % len(EMAC_EDGE32)]
    )
    m, r, extra = _ea_an_or_imm(rnd, a, val, use_imm)
    if target == "macsr":
        words = enc_move_to_macsr(m, r) + extra
    elif target == "mask":
        words = enc_move_to_mask(m, r) + extra
    elif target == "accext01":
        words = enc_move_to_accext01(m, r) + extra
    elif target == "accext23":
        words = enc_move_to_accext23(m, r) + extra
    else:
        words = enc_move_to_acc(int(target[3]), m, r) + extra
    note = "move.l %s,%s" % (
        "#0x%08x" % val if use_imm else "a%d(=0x%08x)" % (r, val),
        target,
    )
    return words, d, a, [], note


def _gen_move_from_dedicated(rnd, mode, source, edge_i):
    """move.l <source>,An -- the Dn-dest variant is already exercised by
    every case's own probe (_PROBE_WORDS)."""
    d = _base_state(rnd, edge_i)
    d[0] = _macsr_source(rnd, mode, edge_i)
    a = _rand_a(rnd)
    r = rnd.randrange(7)
    if source == "macsr":
        words = enc_move_from_macsr(8 + r)
    elif source == "mask":
        words = enc_move_from_mask(8 + r)
    elif source == "accext01":
        words = enc_move_from_accext01(8 + r)
    elif source == "accext23":
        words = enc_move_from_accext23(8 + r)
    else:
        words = enc_move_from_acc(int(source[3]), 8 + r)
    note = "move.l %s,a%d" % (source, r)
    return words, d, a, [], note


def _emac_generators():
    generators: list[tuple[str, Callable[..., Any]]] = []
    for msac, tag in ((False, "mac"), (True, "msac")):
        generators.append(
            (tag, lambda rnd, mode, ei, msac=msac: _gen_mac_form(rnd, mode, msac, ei))
        )
        for ea_kind in ("ind", "post", "pre", "disp"):
            label = "%s_load_%s" % (tag, ea_kind)
            generators.append(
                (
                    label,
                    lambda rnd, mode, ei, msac=msac, ea=ea_kind: _gen_mac_load_form(
                        rnd, mode, msac, ea, False, ei
                    ),
                )
            )
        generators.append(
            (
                "%s_load_masked" % tag,
                lambda rnd, mode, ei, msac=msac: _gen_mac_load_form(
                    rnd, mode, msac, "ind", True, ei
                ),
            )
        )
    generators.append(("movclr", lambda rnd, mode, ei: _gen_movclr_form(rnd, mode, ei)))
    for target in (
        "acc0",
        "acc1",
        "acc2",
        "acc3",
        "macsr",
        "mask",
        "accext01",
        "accext23",
    ):
        for use_imm, tag in ((False, "an"), (True, "imm")):
            label = "move_to_%s_%s" % (target, tag)
            generators.append(
                (
                    label,
                    lambda rnd, mode, ei, t=target, ui=use_imm: _gen_move_to_dedicated(
                        rnd, mode, t, ui, ei
                    ),
                )
            )
    for source in (
        "acc0",
        "acc1",
        "acc2",
        "acc3",
        "macsr",
        "mask",
        "accext01",
        "accext23",
    ):
        label = "move_from_%s_an" % source
        generators.append(
            (
                label,
                lambda rnd, mode, ei, s=source: _gen_move_from_dedicated(
                    rnd, mode, s, ei
                ),
            )
        )
    return generators


def _apply_case_regs(r, d, a):
    for i in range(8):
        r.d[i] = d.get(i, 0) & 0xFFFFFFFF
    for i in range(7):
        r.a[i] = a[i] & 0xFFFFFFFF
    r.a[7] = EMAC_DATA + EMAC_DATA_SIZE - 0x100


def _run_one_case(core, uc, K, trace, words, d, a, mem):
    setup = _setup_words()
    full = setup + list(words) + _PROBE_WORDS
    code = b"".join(struct.pack(">H", w & 0xFFFF) for w in full)
    core.write_mem(EMAC_CODE, code)
    uc.mem_write(EMAC_CODE, code)
    for addr, val in mem:
        packed = struct.pack(">I", val & 0xFFFFFFFF)
        core.write_mem(addr, packed)
        uc.mem_write(addr, packed)
    r = CfRegs()
    r.pc = EMAC_CODE
    r.sr = 0x2700
    _apply_case_regs(r, d, a)
    core.clear_overrides()
    core.set_regs(r)
    uc_set_regs(uc, K, r)
    # `limit` is an INSTRUCTION count, not a word count: setup and probe are
    # always one word per instruction, but the form under test is always
    # exactly one instruction regardless of how many words it encodes as
    # (mac/msac 2 words, mac_load/msac_load 2-3 with a (d16,An) EA) -- using
    # len(full) here made run_window attempt one phantom extra step per
    # extra word, decoding whatever garbage memory follows the probe.
    steps = len(setup) + 1 + len(_PROBE_WORDS)
    n, err = run_window(core, uc, K, trace, EMAC_CODE, steps)
    return n, err, steps


def cmd_emac(args) -> int:
    lib = load_cfabi(build_cfabi())

    def fresh_core():
        return Core(lib)

    def fresh_uc():
        uc, K, trace = unicorn_machine()
        uc.mem_map(EMAC_CODE, 0x1000)
        uc.mem_map(EMAC_DATA, EMAC_DATA_SIZE)
        return uc, K, trace

    core = fresh_core()
    uc, K, trace = fresh_uc()
    rnd = random.Random(args.seed)
    generators = _emac_generators()

    # A fresh Uc (and cfabi Cpu) EVERY case, not just periodically: this
    # Unicorn build's documented "general instability under sustained
    # single-stepping" (tools/cf_lockstep.py module docstring; P3 stage-2
    # report) turns out to include SILENT register corruption, not just the
    # segfaults that motivated fuzz's 10,000-step recycle -- confirmed here
    # directly (case ~1730, ~2,200 steps into that recycle window: Unicorn
    # left the instruction's own destination register unwritten AND wrote a
    # stale-looking value into an unrelated register the instruction never
    # touches; the identical case against a brand-new Uc/Cpu pair matched
    # cleanly). Each emac case is only ~17-19 instructions, so per-case
    # recycling is cheap enough to just always do.
    case_count = 0
    total_cases = 0
    total_steps = 0
    incomplete = 0
    divergences = []

    def do_case(label, mode, words, d, a, mem, tag):
        nonlocal core, uc, K, trace, case_count, total_cases, total_steps, incomplete
        case_count += 1
        core.close()
        core = fresh_core()
        uc, K, trace = fresh_uc()
        n, err, total = _run_one_case(core, uc, K, trace, words, d, a, mem)
        total_cases += 1
        total_steps += n
        if n < total and err is None:
            incomplete += 1
        if err:
            divergences.append("%s mode=0x%02x %s\n  %s" % (label, mode, tag, err))

    stop = False
    for label, gen in generators:
        if stop:
            break
        for mode in EMAC_MODES:
            for i in range(args.per_cell):
                words, d, a, mem, note = gen(rnd, mode, None)
                do_case(label, mode, words, d, a, mem, "case=%d %s" % (i, note))
                if args.limit and total_cases >= args.limit:
                    stop = True
                    break
            if stop:
                break
            for ei in range(len(EMAC_EDGE32)):
                words, d, a, mem, note = gen(rnd, mode, ei)
                do_case(label, mode, words, d, a, mem, "edge=%d %s" % (ei, note))
                if args.limit and total_cases >= args.limit:
                    stop = True
                    break
            if stop:
                break

    core.close()
    print(
        "%d forms x %d MACSR modes: %d cases, %d single-step comparisons, "
        "%d divergence(s), %d incomplete (unmapped access/interrupt, not compared)"
        % (
            len(generators),
            len(EMAC_MODES),
            total_cases,
            total_steps,
            len(divergences),
            incomplete,
        )
    )
    for dtext in divergences[: args.max_failures]:
        print(dtext)
    return 1 if divergences else 0


def main(argv=None) -> int:
    p = argparse.ArgumentParser(description=__doc__)
    sub = p.add_subparsers(dest="cmd", required=True)

    ps = sub.add_parser("snap", help="lockstep from an emu snapshot")
    ps.add_argument("snap")
    ps.add_argument("--limit", type=int, default=1000)
    ps.add_argument("--at", type=lambda s: int(s, 0), default=None)
    ps.add_argument("--verbose", action="store_true")
    ps.set_defaults(func=cmd_snap)

    pf = sub.add_parser(
        "fuzz", help="single-step real opwords from a built image, random regs"
    )
    pf.add_argument("image")
    pf.add_argument("--limit", type=int, default=2000)
    pf.add_argument("--seed", type=int, default=0)
    pf.add_argument("--max-failures", type=int, default=50)
    pf.add_argument(
        "--trace",
        action="store_true",
        help="print each pc tried, for debugging a crash",
    )
    pf.set_defaults(func=cmd_fuzz)

    pe = sub.add_parser(
        "emac",
        help="EMAC semantics (MAC/MSAC/MOVCLR/moves) via guest probes, synthetic instructions",
    )
    pe.add_argument("--seed", type=int, default=0)
    pe.add_argument(
        "--per-cell",
        type=int,
        default=6,
        help="random cases per form x MACSR-mode cell",
    )
    pe.add_argument("--limit", type=int, default=0, help="cap total cases (0 = no cap)")
    pe.add_argument("--max-failures", type=int, default=50)
    pe.set_defaults(func=cmd_emac)

    args = p.parse_args(argv)
    return args.func(args)


if __name__ == "__main__":
    sys.exit(main())
