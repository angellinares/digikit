# 20. Patch toolchains (patchkit spike 0.5)

Goal: compile patchkit modules ahead of time into relocatable objects, link them into the firmware at patch
time with our own code, and unit-test them on the emulated cores. Plan: `docs/plans/patchkit.md`, item 0.5.
Tools: `tools/patchkit/pk_toolchains.py`, `pk_link.py`, `pk_cfcall.py`, `pk_sharc.py` (since phase 6a the SHARC+ build is `packages/patchkit/src/build/sharc_obj.rs`, Selache as a library; `pk_sharc.py` keeps wrappers for the test rigs and the core runner; `pk_elf.py` is deleted); tests
`tests/patchkit/test_pk_cf.py`, `tests/patchkit/test_pk_sharc.py` (sources in `tests/pk_src/`). Both tool chains install under
`out/tools/` (gitignored; `mise run toolchains`); the tests skip when a toolchain or the 1.17 image is absent.

## ColdFire (MCF5441x, ISA_C + EMAC, no FPU)

- **Toolchain [D].** GNU binutils 2.47 (tarball snapshot `2.47.20260726`, sha256 `154ab23b...`) and GCC 16.2.0
  (sha256 `e6738e29...`) built from source for `m68k-elf` by `tools/patchkit/pk_toolchains.py m68k`: about 15 minutes on
  16 cores (binutils 3, gcc 11; the two tarballs are 135 MB), C only, no libc, no multilib, prefix `out/tools/m68k-elf`.
  Homebrew also has bottles of both versions (`m68k-elf-gcc` 16.2.0, `m68k-elf-binutils` 2.47); the source
  script is the pinned, local-only route. `as` and GCC know `-mcpu=54418` directly (GCC: `isa_c`, hardware
  divide, USP, EMAC, MMU, no FPU); `as -mcpu=54418` accepts `ff1`, `bitrev`, `byterev`, `mvz`, `mac`, `muls.l`
  and encodes them as the firmware does (`ff1 d0` = `04c0`). LLVM was not tried: Apple clang has no M68k
  target and the LLVM M68k backend names only 680x0 CPUs (from its documentation; **[O]**, not checked here).
- **Flags.** `-mcpu=54418 -Os -ffreestanding -fno-builtin -nostdlib -fno-pic -ffunction-sections
  -fdata-sections -fno-asynchronous-unwind-tables -Wall -Wextra -Werror`.
- **Object prototype [D].** `pk_link.object_from_elf` turns the ELF into JSON: allocatable sections as hex,
  symbols, relocations, SHA-256 of the sources, toolchain versions. `pk_link.link` works from that JSON alone
  (round trip tested), so patch time needs no ELF parser and no toolchain.
- **Relocations seen [D].** gcc emits only `R_68K_32` (absolute `jsr`, `lea`, data addresses) for C at this
  setting; the linker also handles and tests `R_68K_PC32`/`PC16`/`PC8`, `PLT32/16/8` (as `bsr.l` / `bsr.w`
  to an extern). A `bsr.w` from the expansion region to the firmware does not fit (132 MB apart): the linker
  reports the out-of-range relocation instead of truncating.
- **ISA check [D].** `pk_link.check_isa` decodes every instruction of the executable sections with our decoder
  (`packages/emulator/coldfire` `cfdis`, generated from `tools/coldfire/cfisa/coldfire.json`) and fails on an illegal word, a form
  outside `isa_a`/`isa_b`/`isa_c`/`emac`, or a privileged form. The test module (55 instructions, 22 forms)
  passes; an assembly file built with `-m68020` (`bfextu`) is rejected.
- **Running on the core [D].** `Emulator::call_function` (`packages/emulator/boot/src/runtime/call.rs`, example
  `cf_call`, wrapper `CfMachine`): boots a ready image once (7 s CPU, DT2 1.17), caches the `save_state` blob
  (73 MB) in `out/cache/pk_cfcall/`, afterwards 0.75 s per test process. A call pushes the arguments and a
  return sentinel, runs with SR 0x2700, stops when the PC reaches the sentinel, and reports D0-D7/A0-A7; the
  wrapper flags a clobbered callee-saved register (D2-D7, A2-A6) or a wrong A7. Code outside the MAIN image
  (the expansion region) runs through the bare CPU step; the boot observers apply only inside the image.
  Results against Python references: Adler-32 over 1000 bytes equals `zlib.adler32`; `.data`/`.bss`
  state persists between calls; a module function calls firmware 0x40114a28 and 0x40114984 through extern
  symbols and equals direct calls of both; a `bsr.l` (PC32) call into the firmware works; the ISA_C stub
  (`byterev`, `bitrev`, `ff1`) equals its Python model.
- **Firmware ABI notes [D].** 0x40114a28 (`this+0x10` bit test) sets only the low byte of D0 (`clr.b`): declare
  it `u8` or the caller sees garbage in the upper bytes. 0x40041d24 takes `this` on the stack and returns
  `this+0xf4`.
- **Open [O].** (1) `long long` multiply/divide and every float operation compile to libgcc helpers
  (`__muldi3`, `__udivdi3`, `__addsf3`); the libgcc built here is for the default CPU, not 54418, so modules
  must either avoid them or get helpers from the firmware's softfloat entries or a libgcc built with
  `--with-cpu=54418`. (2) Tested on DT2 1.17 only. (3) EMAC is not emitted by gcc; use inline assembly.

## SHARC+ (ADSP-2156x)

- **Toolchain [D].** Selache at `926bfac9` (GPLv3, ElekLabs fork of the public project), `cargo build --release`, built by
  `tools/patchkit/pk_toolchains.py selache` into `out/tools/selache` in under a minute. `selas -proc ADSP-21569`.
- **Object facts [D].** Section `seg_pmco` (assembly, 48-bit forms) or `seg_swco` (selcc). Symbol values and
  relocation offsets are in 16-bit parcels (the PC unit). Two relocation types: **11** = absolute 24-bit target
  of a 48-bit jump/call (low 24 bits of the instruction), **12** = absolute 32-bit immediate or address (low 32
  bits). Targets are short-word (PC) addresses; relative `DO ... UNTIL` and `CALL (PC,n)` are resolved by the
  assembler. Quirks: the addend of `sym + 4` is part of the symbol name, and the field already holds the addend
  (selcc writes `-1` for a return label: `jump (m14,i12)` adds 1). `pk_sharc.link` accepts a field of 0 or the
  addend and fails otherwise. Locals (`.L_...`) appear as symbols.
- **Re-decode gate [D].** After linking, parcels are swapped to boot order and every instruction is decoded by
  `tools/sharc_dev/sharc_isa.py`: the form must exist, its extent must equal the extent Selache chose, and each relocated
  field must read back as the intended value. A corrupted word fails the build (tested). Selache's shift-immediate
  encode bugs are repaired or refused (finding 05, "Selache shift-immediate encodings"). Selache's *disassembler* prints `m4` for
  `dm(i0,m0)` stores (finding 05 bug 2 class); the bytes are right, our decoder reads M0.
- **Running on the core [D].** `pk_sharc.run_call` builds a `LoadedMemory` from the linked bytes (at the short
  word alias `0x28000000 + 2*sw`), pokes normal-word DM, runs `tools/sharc_dev/sharc_run.py` to a breakpoint. Scale-in-place
  (`F2 = F2*F1` in a `DO` loop, local `CALL`, tail `JUMP` to an extern) over four floats gives exactly
  `x * 2.5`; one word past the buffer is untouched.

## selcc (Selache C compiler) [D]

Usable for small code, with caveats. Six test functions (`tests/patchkit/pk_src/scale_c.c`) compile, every instruction (49 and 123 in two
sample files) decodes with the extent Selache chose, and they run correctly on the core: `add3`, `sum` over an `int` array, a struct
float field, a `switch`, a bit-count loop, a float scale loop. Calling convention: ADI-style frames
(`cjump`/`rframe`; frame pointer I6, return address at `[I6-1]`), arguments R4/R8/R12, result R0, symbols get a
trailing dot (`scale.`). Findings: (1) **pointers are byte addresses** (index scaled by 4); the loops are right
only with `0x2xxxxxxx` byte-alias pointers, not with normal-word addresses (`0x08xxxxxx`), and our core accepts
a byte address for a normal-word access; **[C]** the manual says the silicon does the same (finding 05, "A normal-word access
through a byte-address pointer matches the manual"); (2) integer divide and modulo call
`___div32.`/`___mod32.` (no library is linked); (3) `-O`, `-Oa`, `-Os` produce identical output; **[C]** (2026-10-21) because the
optimisation passes (constant folding, dead code, hardware `DO` loops, copy removal, multifunction packing) always run: the
level is parsed and not read. What makes the code slow is the register use: every local variable (the loop index, filter
states) lives in a stack slot and is loaded and stored each iteration, an address is rebuilt with shifts and adds instead of
a post-modify, and a `static` helper is not inlined (a call each). The scale loop body is 15 instructions per element against
3 by hand; a delay line with a one-pole low-pass in a flat C loop is 46 per sample against about 6-7; a call to a block
routine costs about 25 (`R4/R8/R12`, the frame); (4) patchkit would have to supply the frame for a
top-level call (the tests build it). Recommendation: assembly for DSP code on the hot path, selcc only for glue
and only after (1) is settled on hardware or by the PRM. Selache's shift-immediate encodings: finding 05, "Selache
shift-immediate encodings" (selcc's assembly is checked like hand-written assembly).

## Recommendation for the `.dko` format

The JSON prototypes (`pk_link.object_from_elf`, `pk_sharc.SharcObject.to_json`) carry everything the linker
needed, so `.dko` can be that model with a binary container and a canonical encoding:

- header: arch (`cf-mcf54418`, `sharc+`), format version, source SHA-256, toolchain strings (informational),
  required port symbols with an optional signature hash;
- sections: name, kind (`text|rodata|data|bss`), alignment, size, bytes. Store SHARC code in the order the
  boot stream uses (little-endian parcels) or declare the order in the header; define SHARC relocations by
  logical field (`abs24` = bits 23:0 of the 48-bit instruction, `abs32` = bits 31:0) and let the linker pick
  byte positions;
- symbols: name, section, value, global flag, in the arch's native unit (bytes; SHARC parcels);
- relocations: `{in, offset, type, to_symbol | to_section, addend}` with the types seen here (`cf.32`, `cf.pc32`,
  `cf.pc16`, `cf.pc8`, `sharc.abs24`, `sharc.abs32`); the addend always explicit, never in the symbol name;
- the linker is arch code of about 150 lines each, runs in Python today and ports to Rust/wasm directly; the
  ISA check (cfdis, SHARC re-decode) belongs in the module build, not at patch time.


## Assembler facts found writing core/dsp-voice and digikit/pluck (patchkit 4.2) **[D]**

One agent; each item was met as a build failure or a wrong result and fixed in the source.

- **Compact `ADD` collides with Type 2b in our decoder.** Selache emits `Rn = Rn + Rm` with `Rn`, `Rm` in R0-R7 as the
  16-bit form `c0xy` (`R0 = R0 + R1` = `c001`); our decoder reads `c001 xxxx` as the 32-bit Type 2b, so the
  extents disagree and `pk_sharc` stops with "Selache lists c001, our decoder c001703e". Registers R8-R15 and the
  `c1xx` subtract form are fine. Commute the operands (`R0 = R1 + R0` assembles as the 32-bit form). Which side is
  right on silicon is **[O]**; the firmware has no `c00x` word.
  **[D][C]** Not a Selache encoding bug (checked 2026-10-30 for the fork): `selas` writes the Type 2c word the PRM defines, and the
  firmware does have `c0xx` words (finding 05, "Type2b and the 16-bit Type2c"; finding 45, section 7). The tie is the emulator's
  lookahead, so the fork was not changed and patchkit keeps its width check.
- **`.VAR name[N]` is dropped and a multi-value `.VAR` produces a section-relative relocation.** `pk_sharc` takes
  `.PKDATA name, bytes;` (zeros, loaded) and `.PKBSS name, bytes;` (reserved, not loaded) instead: the line is removed
  before assembly, `name` becomes an extern, and the object gets a data or bss section and a global data symbol.
  `.GLOBAL name;` for such a name makes the symbol exported twice.
- **patchkit leaves a SHARC bss out of the boot stream** (it did copy it as zeros): the chunk image ends where the
  bss begins, and the plan counts the bss against the region. A 262,144-byte delay area adds 0 bytes to the
  container. The program must write reserved memory before it reads it.
- **`(SW)`/`(BW)` are ignored on indirect loads too** (`R1 = DM(I4, M5) (SW);` assembles as the normal-word load);
  the direct form is fixed in the fork (finding 22), the indirect one is not **[O]**.
- **`MODIFY(Ix, My)` was a `selas` mis-encoding [D].** Its 32-bit word lacked the Type 7b fixed bits (frame bits 22-16 = `0111111`, PRM
  Figure 14-20 and the SHARC Processor PGR), so no table row matched and the decoder read a 48-bit Type 7a. Stock has 57 Type 7b words on
  DT2 1.17 and 74 on DN2 1.12, all with those bits; `MODIFY(I5, M2)` is `043f503f`, as at DN2 1.12 `sw 0xb80d93`. Fixed in the fork
  ("selinstr: 32-bit MODIFY(Ii, Mm) carries the Type 7b fixed bits"). Read a short word as a 32-bit word and extract it.
- **`FEXT/FDEP Rx BY Ry`: `Ry` = `bit6 | len6 << 6`** (the PRM figure: `len6` at Ry[11:6]); the immediate form is
  `BY pos:len`. `(SE)` is not parsed by this Selache (`unresolved symbol SE`): shift left and `ASHIFT` right by a
  register holding -16 instead.
- **Immediate arithmetic.** `Rn = Rx + 1` assembles; `Rn = Rx + 26` and `Rn = Rx - 64` do not (load the constant into
  a register). `INC R5` is read as a symbol. `MODIFY(I7, -26) (NW);` assembles to the firmware's own
  `1587 ffff ffe6`; without `(NW)` the immediate is in bytes (`1607 ...`).
- **`CALL (M13, I12)` is not followed by the tracer** (`tools/sharc_dev/sharc_core` ends the callee's `RTS` with "return
  without followed call") and the firmware has no indirect call. core/dsp-voice enters a registered routine with
  `JUMP (M13, I12)` and a return address in `I13`.
