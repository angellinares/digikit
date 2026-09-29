# pyright: reportMissingImports=false
"""Semantic compatibility check for patched Unicorn m68k CCR and EMAC behavior.

Each case fails on a Unicorn that lacks one of the patches under patches/.
"""

import json
from functools import lru_cache

INSTALL_COMMAND = "tools/install-patched-unicorn.sh"


def _run_case(factory, value, expected):
    """Exercise a lazy CMP followed by a code-hook SR read and guest BEQ."""
    from unicorn import UC_HOOK_CODE
    from unicorn.m68k_const import (
        UC_M68K_REG_A2,
        UC_M68K_REG_D1,
        UC_M68K_REG_D4,
        UC_M68K_REG_PC,
        UC_M68K_REG_SR,
    )

    uc = factory()
    # cmp.l a2,d4; beq taken; move.l #111,d1; bra done;
    # taken: move.l #222,d1.  This is the count-hook failure shape.
    code = bytes.fromhex("b88a 6708 223c0000006f 6006 223c000000de")
    uc.mem_map(0, 0x10000)
    uc.mem_write(0x1000, code + b"\x4e\x71" * 8)
    uc.reg_write(UC_M68K_REG_PC, 0x1000)
    uc.reg_write(UC_M68K_REG_SR, 0x2004)  # deliberately seed stale Z
    observed_sr = []

    def read_sr_from_code_hook(uc_, address, size, data):
        observed_sr.append(uc_.reg_read(UC_M68K_REG_SR) & 0x1F)

    uc.reg_write(UC_M68K_REG_A2, 0)
    uc.reg_write(UC_M68K_REG_D4, value)
    # The BEQ hook runs after CMP but before the branch consumes its flags.
    uc.hook_add(UC_HOOK_CODE, read_sr_from_code_hook, begin=0x1002, end=0x1002)
    uc.emu_start(0x1000, 0, count=5)
    sr = observed_sr[0] if len(observed_sr) == 1 else None
    result = uc.reg_read(UC_M68K_REG_D1)
    return {
        "input": value,
        "sr": sr,
        "branch_value": result,
        "expected_sr": 4 if value == 0 else 0,
        "expected_branch_value": expected,
        "pass": sr == (4 if value == 0 else 0) and result == expected,
    }


def _run_count_boundary_case(factory):
    """A count stop at the instruction after CMP must expose CMP's CCR.

    Unicorn's count hook stops on instruction N+1.  The m68k translator must
    therefore have committed the lazy producer from instruction N before
    returning to the host at that hook.
    """
    from unicorn.m68k_const import (
        UC_M68K_REG_A2,
        UC_M68K_REG_D4,
        UC_M68K_REG_PC,
        UC_M68K_REG_SR,
    )

    uc = factory()
    uc.mem_map(0, 0x10000)
    # cmp.l a2,d4; beq $102e.  The branch is deliberately not executed:
    # count=1 returns at its hook, immediately after the compare.
    uc.mem_write(0x1000, bytes.fromhex("b88a672c") + b"\x4e\x71" * 32)
    uc.reg_write(UC_M68K_REG_PC, 0x1000)
    uc.reg_write(UC_M68K_REG_SR, 0x2001)  # stale Z clear
    uc.reg_write(UC_M68K_REG_A2, 0x44605678)
    uc.reg_write(UC_M68K_REG_D4, 0x44605678)
    uc.emu_start(0x1000, 0, count=1)
    pc = uc.reg_read(UC_M68K_REG_PC)
    sr = uc.reg_read(UC_M68K_REG_SR) & 0x1F
    return {"pc": pc, "sr": sr, "pass": pc == 0x1002 and sr == 4}


def _run_btst_flush_case(factory):
    """BTST after a lazy-CCR producer must leave its Z in the CPU state.

    With a code hook on the BTST (the count= instruction counter covers
    every instruction), the hook CCR sync stores CC_OP_LOGIC from the
    ``move.l`` before BTST runs. BTST then evaluates the flags inline and
    translates on with CC_OP_FLAGS, but without the flush-flags patch the
    translator believes the stored CC_OP is current, so the CPU state keeps
    CC_OP_LOGIC and recomputes Z from N. Two shapes see that stale state:

    - ``tb_boundary``: the block ends at the first BEQ and the next block
      starts with another BEQ, in one ``emu_start``.
    - ``count_stop``: a count stop between BTST and BEQ after an RTE whose
      interrupt hook wrote PC, as emu.harness implements RTE. Without that
      earlier PC write Unicorn restores CC_OP from the instruction-start
      record at the stop and hides the defect. Digitakt II 1.16 hit this at
      0x400cd2f4 (FUN_400cd2bc) after 69.87M exact-mode instructions.

    Bit 28 of 0x80000000 is clear, so Z = 1 and each BEQ is taken (D1 = 2).
    """
    from unicorn import UC_HOOK_INTR
    from unicorn.m68k_const import (
        UC_M68K_REG_D1,
        UC_M68K_REG_PC,
        UC_M68K_REG_SR,
    )

    def machine(code):
        uc = factory()
        uc.mem_map(0, 0x10000)
        uc.mem_write(0x1000, bytes.fromhex(code) + b"\x4e\x71" * 8)
        uc.reg_write(UC_M68K_REG_SR, 0x2700)
        return uc

    # move.l #$80000000,d0; btst #28,d0; beq.w $100e;
    # $100e: beq.s $1014; moveq #1,d1; bra.s $1016; $1014: moveq #2,d1
    uc = machine("203c80000000 0800001c 67000002 6704 7201 6002 7202")
    uc.emu_start(0x1000, 0x1016, count=100)
    tb_boundary = uc.reg_read(UC_M68K_REG_D1)

    # rte; move.l #$80000000,d0; btst #28,d0; beq.w $1014;
    # moveq #1,d1; bra.s $1016; $1014: moveq #2,d1
    uc = machine("4e73 203c80000000 0800001c 67000006 7201 6002 7202")

    def rte(uc_, intno, data):
        uc_.reg_write(UC_M68K_REG_PC, 0x1002)

    uc.hook_add(UC_HOOK_INTR, rte)
    uc.emu_start(0x1000, 0, count=3)
    stop_pc = uc.reg_read(UC_M68K_REG_PC)
    stop_sr = uc.reg_read(UC_M68K_REG_SR) & 0x1F
    uc.emu_start(stop_pc, 0x1016)
    count_stop = uc.reg_read(UC_M68K_REG_D1)
    return {
        "tb_boundary_branch_value": tb_boundary,
        "count_stop_pc": stop_pc,
        "count_stop_sr": stop_sr,
        "count_stop_branch_value": count_stop,
        "pass": tb_boundary == 2
        and stop_pc == 0x100C
        and stop_sr == 0x0C
        and count_stop == 2,
    }


def _run_mac_load_case(factory):
    """MAC with load must run as the manual says (Digitakt II 0x400db9e0).

    Stock Unicorn faults on this Ry (D6) and, with other Ry, reads Rx from D2
    and ANDs MASK into the address even when the instruction does not ask.
    """
    from unicorn import UcError
    from unicorn.m68k_const import (
        UC_M68K_REG_A1,
        UC_M68K_REG_D0,
        UC_M68K_REG_D1,
        UC_M68K_REG_D2,
        UC_M68K_REG_D4,
        UC_M68K_REG_D5,
        UC_M68K_REG_D6,
        UC_M68K_REG_D7,
        UC_M68K_REG_SR,
    )

    uc = factory()
    uc.mem_map(0, 0x10000)
    # move.l d7,MACSR; move.l d5,ACC0; mac.w d6u,d0u,(a1),d4,ACC0;
    # move.l ACC0,d1.  ACC0 + 3 * 5 -> ACC0 and (a1) -> d4.
    uc.mem_write(0x1000, bytes.fromhex("a907 a105 a891 00c6 a181"))
    uc.mem_write(0x2000, bytes.fromhex("0000002a"))
    uc.reg_write(UC_M68K_REG_SR, 0x2700)
    for regid, value in (
        (UC_M68K_REG_D7, 0),
        (UC_M68K_REG_D5, 0),
        (UC_M68K_REG_D6, 0x00030002),
        (UC_M68K_REG_D0, 0x00050004),
        (UC_M68K_REG_D2, 0x00090008),
        (UC_M68K_REG_D4, 0xAAAAAAAA),
        (UC_M68K_REG_A1, 0x2000),
    ):
        uc.reg_write(regid, value)
    try:
        uc.emu_start(0x1000, 0x100A, count=4)
    except UcError as exc:
        return {"error": str(exc), "pass": False}
    acc = uc.reg_read(UC_M68K_REG_D1) & 0xFFFFFFFF
    loaded = uc.reg_read(UC_M68K_REG_D4) & 0xFFFFFFFF
    return {"acc0": acc, "loaded": loaded, "pass": acc == 15 and loaded == 0x2A}


def _run_emac_fractional_case(factory):
    """Fractional EMAC must match MCF54418RM p.5-9 and p.5-17 (PDF p.151, p.159).

    The Digitakt II one-pole smoother FUN_400d92a2 runs with MACSR = 0x20
    (signed fractional). There ``product = (operandY * operandX) << 1`` of
    the signed operands, so 0.5 * 0.5 = 0.25 (0x20000000) and -0.5 * 0.5 =
    -0.25 (0xE0000000). Stock QEMU multiplies the operands unsigned and
    drops the shift (0x10000000, 0x30000000), and a MACSR mode change does
    not keep the ACCn bits (0x12345678 reads back as 0x00123456).
    """
    from unicorn import UcError
    from unicorn.m68k_const import (
        UC_M68K_REG_D0,
        UC_M68K_REG_D1,
        UC_M68K_REG_D2,
        UC_M68K_REG_D3,
        UC_M68K_REG_D4,
        UC_M68K_REG_D5,
        UC_M68K_REG_D6,
        UC_M68K_REG_SR,
    )

    uc = factory()
    uc.mem_map(0, 0x10000)
    # move.l #0,MACSR; move.l #$12345678,ACC0; move.l #$20,MACSR;
    # movclr.l ACC0,d3; mac.w d6u,d0u,ACC0; movclr.l ACC0,d1;
    # mac.l d4,d5,ACC0; movclr.l ACC0,d2.
    code = bytes.fromhex(
        "a93c00000000 a13c12345678 a93c00000020 a1c3 a00600c0 a1c1 aa040800 a1c2"
    )
    uc.mem_write(0x1000, code)
    uc.reg_write(UC_M68K_REG_SR, 0x2700)
    for regid, value in (
        (UC_M68K_REG_D0, 0x40000000),
        (UC_M68K_REG_D6, 0x40000000),
        (UC_M68K_REG_D4, 0xC0000000),
        (UC_M68K_REG_D5, 0x40000000),
    ):
        uc.reg_write(regid, value)
    try:
        uc.emu_start(0x1000, 0x1000 + len(code), count=8)
    except UcError as exc:
        return {"error": str(exc), "pass": False}
    got = {
        "mode_switch": uc.reg_read(UC_M68K_REG_D3) & 0xFFFFFFFF,
        "half_times_half": uc.reg_read(UC_M68K_REG_D1) & 0xFFFFFFFF,
        "minus_half_times_half": uc.reg_read(UC_M68K_REG_D2) & 0xFFFFFFFF,
    }
    expected = {
        "mode_switch": 0x12345678,
        "half_times_half": 0x20000000,
        "minus_half_times_half": 0xE0000000,
    }
    return {**got, "pass": got == expected}


def evaluate(factory=None):
    """Return bounded diagnostics; ``factory`` makes this testable without Unicorn."""
    if factory is None:
        from unicorn import UC_ARCH_M68K, UC_MODE_BIG_ENDIAN, Uc
        from unicorn.m68k_const import UC_CPU_M68K_CFV4E

        def runtime_factory():
            uc = Uc(UC_ARCH_M68K, UC_MODE_BIG_ENDIAN)
            uc.ctl_set_cpu_model(UC_CPU_M68K_CFV4E)
            return uc

        factory = runtime_factory
    cases = {
        "zero_z_taken": _run_case(factory, 0, 0xDE),
        "nonzero_z_clear": _run_case(factory, 1, 0x6F),
        "count_boundary_cmp_z": _run_count_boundary_case(factory),
        "btst_flush_z": _run_btst_flush_case(factory),
        "emac_mac_with_load": _run_mac_load_case(factory),
        "emac_fractional": _run_emac_fractional_case(factory),
    }
    return {"compatible": all(case["pass"] for case in cases.values()), "cases": cases}


@lru_cache(maxsize=1)
def _evaluate_runtime():
    """Cache the real runtime probe; injected evaluators remain uncached."""
    return evaluate()


def require_compatible_unicorn():
    """Raise before a machine can run with a destructive Unicorn SR read."""
    try:
        import unicorn
    except ImportError as exc:
        raise RuntimeError(
            "Unicorn is required; run `uv sync` then `%s`" % INSTALL_COMMAND
        ) from exc
    if getattr(unicorn, "__version__", None) != "2.1.4":
        raise RuntimeError(
            "This project requires unicorn==2.1.4; run `uv sync` then `%s`"
            % INSTALL_COMMAND
        )
    result = _evaluate_runtime()
    if not result["compatible"]:
        failed = ", ".join(
            name for name, case in result["cases"].items() if not case["pass"]
        )
        raise RuntimeError(
            "Installed Unicorn fails the m68k compatibility check (%s). "
            "Run `%s` (uv sync restores stock Unicorn, which this guard rejects)."
            % (failed, INSTALL_COMMAND)
        )
    return result


def main():
    try:
        print(json.dumps(require_compatible_unicorn(), sort_keys=True))
    except RuntimeError as exc:
        print("Unicorn compatibility check failed: %s" % exc)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
