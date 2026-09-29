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
import random
import subprocess
import sys
from pathlib import Path

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
    from unicorn import UC_ARCH_M68K, UC_MODE_BIG_ENDIAN, Uc
    from unicorn import m68k_const as K

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
        self.writes: list[tuple[int, int]] = []  # (addr, size)
        self.exception = False
        self.unmapped = False

    def reset(self):
        self.reads.clear()
        self.writes.clear()
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

    def _intr_hook(self, uc, intno, ud):
        self.exception = True
        uc.emu_stop()


def run_uc_step(uc, K, trace: StepTrace, pc: int) -> StepTrace:
    from unicorn import UcError

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
        if ev != av:
            out.append("%s: expected 0x%x, got 0x%x" % (f, int(ev), int(av)))  # type: ignore[arg-type]
    return out


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

    from unicorn import UcError

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
    import oracle  # tools/cfisa/oracle.py: image_paths/load_base/listing

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

    args = p.parse_args(argv)
    return args.func(args)


if __name__ == "__main__":
    sys.exit(main())
