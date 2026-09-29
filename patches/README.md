# Patches

Five diffs against [Unicorn Engine](https://github.com/unicorn-engine/unicorn)
2.1.4, tag commit `8028ec436f2d9376525352dd38ed9ed6b9f6be10`, applied in this
order:

- `unicorn-2.1.4-m68k-hook-ccr-sync.patch` touches
  `qemu/target/m68k/translate.c` and `qemu/target/m68k/unicorn.c`.
- `unicorn-2.1.4-m68k-emac-mac-load.patch` touches
  `qemu/target/m68k/translate.c`.
- `unicorn-2.1.4-m68k-emac-fractional.patch` touches
  `qemu/target/m68k/helper.c`.
- `unicorn-2.1.4-count-hook-fast-path.patch` touches
  `qemu/include/tcg/tcg-op.h` and `uc.c`.
- `unicorn-2.1.4-m68k-flush-flags-sync.patch` touches
  `qemu/target/m68k/translate.c`.

All these files are QEMU or Unicorn sources. **The patches are
derivative works of that code and carry its licence, not this repository's
choice of one** — take their terms from Unicorn and QEMU upstream. This is why
the repository as a whole is GPL-2.0-or-later rather than something
permissive; see the licence section of the top-level [README](../README.md).

## What the CCR patch fixes

Unicorn's m68k translator keeps condition codes lazily and commits them when a
translation block ends normally. A code hook, or a `count=` stop, can return to
the host *mid-block*, before that commit — so a guest `CMP` followed by a
hook-visible `SR` read, or by a counted stop on the next instruction, exposes
stale flags. Guest branches then take the wrong arm.

The patch commits the pending condition codes before handing control back.

## What the flush-flags patch fixes

The CCR patch above made a new mistake possible. Its commit before a code
hook stores the pending CC_OP (for example `CC_OP_LOGIC` after a `move.l`)
and marks it as stored. Instructions that need all flags at once, such as
`btst`, then compute them in line (`gen_flush_flags`) and switch the
translator to `CC_OP_FLAGS`, but they keep the "stored" mark. The CPU state
still says `CC_OP_LOGIC`, so anything that reads the flags from the CPU state
recomputes Z from N:

- a count stop between `btst` and the next instruction. Unicorn normally
  restores CC_OP from the instruction-start record at such a stop, which
  hides the error, but it skips that restore when a hook has written PC
  earlier in the same `emu_start`. `emu/harness.py` implements `rte` that
  way, so every stop after an `rte` is exposed.
- the next translation block, when it tests a flag before setting one
  (`btst`; `beq` to a label whose first instruction is another `beq`), with
  any code hook on the `btst`, including the `count=` instruction counter.

In the Digitakt II 1.16 exact-mode run from
`snapshots/dt2-1.16-drive3/loaded.snap`, a count stop landed between
`btst.l #28,d0` (`0x400cd2f4`) and `beq.w` in `FUN_400cd2bc` after about
69.87M instructions, 49 instructions after an interrupt handler's `rte`, and
the resumed run took the wrong branch. The patch marks CC_OP as not stored
after the in-line flag computation, so the next commit stores
`CC_OP_FLAGS`. Code with no commit between the lazy producer and the in-line
flag computation (no code hook there, no `move from SR`) translates as
before.

## What the EMAC patch fixes

`DISAS_INSN(mac)` handles MAC and MSAC with load (ColdFire Programmer's
Reference Manual, p.6-3 to 6-5 and p.6-22 to 6-23) wrongly in four ways.
QEMU master had the same code on 2026-09-15.

- It takes extension-word bits 1..0 as a dual-accumulate request and, on a
  core without EMAC_B such as the CFV4E, raises an illegal-instruction
  exception. In the load forms those bits are part of Ry, so every Ry whose
  register number has bit 0 or 1 set faulted. The patch honours the request
  only on EMAC_B cores.
- It reads a data-register Rx from operation-word bits 14..12, which are
  always 2 for these opcodes, so Rx was always D2. Rx is extension-word bits
  15..12.
- It reads the MSAC bit from operation-word bit 8, which is always 0 for
  these opcodes, so MSAC added. The bit is extension-word bit 8. This also
  affects MSAC without load.
- It ANDs MASK into every load address. Extension-word bit 5 says whether
  MASK is used.

The Digitakt II SHARC frame build reaches one of these instructions at
`0x400db9e0`.

## What the EMAC fractional patch fixes

The helpers for fractional mode (MACSR[F/I] = 1) differ from the MCF54418
Reference Manual, section 5.3 (p.5-9 to 5-17, PDF p.151 to 159), and from
the store rules in the ColdFire Programmer's Reference Manual (p.6-6 to
6-9). QEMU master had the same code on 2026-09-28.

- `macmulf` multiplied the operands as unsigned numbers and left out the
  `<< 1` of `product[63:0] = (operandY * operandX) << 1`, so 0.5 * 0.5 gave
  0.125 and -0.5 * 0.5 gave +0.375. With R/T set it rounded the unshifted
  product, and -1 * -1 did not give +1.0. The patch multiplies signed,
  shifts, rounds `product[63:24]` to nearest even on `product[23:0]`,
  sign-extends, and zero-fills -1 * -1.
- `set_macsr` meant to move ACCn and ACCext to their places for the new
  mode (p.5-9) but tested the old MACSR twice, so it did nothing: after
  `move.l #$12345678,ACC0` in integer mode, fractional mode read ACC0 as
  0x00123456. The EMAC save and restore sequence (p.5-11) switches modes
  around live accumulators. The patch lays the accumulator out for the new
  mode, sign-extended in the signed modes and zero-extended in unsigned
  integer mode.
- With OMC set, `macsatf` saturated to the 48-bit limits with the wrong
  sign; the manual gives 0x007F_FFFF_FF00 when `result[47]` is set and
  0xFF80_0000_0000 otherwise (p.5-17).
- With OMC set, `get_macf` (MOVE ACCx,Rx and MOVCLR) returned 0 or 1 for a
  32-bit store and never saturated a 16-bit one. The patch saturates when
  ACC[47:39] (after rounding) are not all equal, by the sign in ACC[47].
- `mac_set_flags` set EV from ACC[47:40]; the manual uses ACC[47:39].

Not changed, because the Digitakt II firmware only writes MACSR = 0x00 and
0x20: with OMC set and the destination's PAVn already set, the manual leaves
the accumulator alone, and Unicorn still accumulates. The integer-mode EV
and saturation helpers were not checked against the manual.

The one-pole smoother `FUN_400d92a2`, run from the SHARC frame handler with
MACSR = 0x20, sums `0x7c29/0x8000 * s + 0x03d7/0x8000 * x`. Without the
patch it settles at 0.029 x instead of x, so every smoothed per-track field
in the SPI2 frame (sample slot 7 read as 0) was wrong.

## Count-hook fast path (`unicorn-2.1.4-count-hook-fast-path.patch`)

A speed change, not a correctness fix. With `emu_start(..., count=N)`,
Unicorn adds an instruction-counter code hook, and every translated
instruction then walks the whole code-hook list (about 274 scoped hooks in
the audio-running state, about 1.26 ns each). The patch makes an
instruction covered only by the counter call it directly, and flushes
translated blocks when a code hook is added after start so a new hook is
seen. Measured from `snapshots/dt2-1.16-drive3/loaded.snap`: exact mode
1.55M -> 3.77M instr/s; guest state identical to the unpatched build at 5M
and 20M instructions (`tools/snapeq.py`).

## Checks

`emu/unicorn_compat.py` exercises the CCR shapes (including both `btst`
shapes above, case `btst_flush_z`), the `0x400db9e0` instruction and
fractional EMAC, and refuses to run on an interpreter whose Unicorn lacks any
of the correctness patches, rather than letting a subtly wrong emulation pass
for a working one. `tests/test_unicorn_compat.py` runs `btst_flush_z` on the
loaded Unicorn. `tests/test_unicorn_emac.py` checks the
MAC and MSAC load forms and fractional mode against the manuals.

## Applying them

Do not apply these by hand. `tools/install-patched-unicorn.sh` pins the
upstream commit, verifies each patch's SHA-256, builds only the m68k target,
replaces the dynamic library the Python bindings actually load, and then runs
the compat check. `uv sync` can restore the stock wheel, which puts the
emulator back to refusing to start until the installer is rerun.
