# GCC/binutils support for MCF54415 / MCF5441x

## Executive answer

**Use now, but as an assembler/disassembler and generic-ABI oracle—not as evidence of the firmware's compiler.** Current GNU GCC has a chip-specific `-mcpu=54415` entry with the required ISA-C, hardware-divide, USP, and EMAC bits, and GNU GAS/objdump have the matching CPU name, control-register names, and extensive EMAC expected-disassembly coverage. GCC deliberately does not generate MAC or EMAC instructions, so the immediate payoff is synthetic assembly/ELF fixtures and negative feature tests for the Ghidra language.

## Verified target support

### GCC

* GCC 13.4, 14.3, and 15.2 release-source tags contain `M68K_DEVICE ("54415", ...)` (as do neighbouring `54410`, `54416`–`54418`). The entry selects family `54418`, multilib representative `54455`, tune `cfv4`, ISA `isa_c`, and `FL_CF_HWDIV | FL_CF_USP | FL_CF_EMAC | FL_MMU | FL_UCLINUX`. Thus `-mcpu=54415` is the exact accepted chip option; the same applies to `-mcpu=54418`. This is the defensible version floor from tags checked here; do not assume an older packaged GCC accepts it without probing `-Q --help=target`. The GCC 15.2 options page describes `-mcpu` but its rendered CPU table is stale (it omits the 5441x row); the release source is controlling evidence for acceptance. [GCC 15.2 device definitions](https://gnu.googlesource.com/gcc/+/refs/tags/releases/gcc-15.2.0/gcc/config/m68k/m68k-devices.def) [GCC 14.3 device definitions](https://gnu.googlesource.com/gcc/+/refs/tags/releases/gcc-14.3.0/gcc/config/m68k/m68k-devices.def)
* Accepted generic alternatives are `-march=isac` and `-mtune=cfv4`; `-march` accepts only `isaa`, `isaaplus`, `isab`, and `isac`, while `-mtune` accepts `cfv1`–`cfv4e`. `-mcpu` overrides a compatible `-march`; a conflicting pair is diagnosed. For this chip, prefer **`-mcpu=54415 -msoft-float`**: its source-selected mask is ISA-A + ISA-C + hardware divide + USP + EMAC, with **no ColdFire FPU bit**. `-mhard-float` is accepted as an override but would request an FPU the selected device does not have; `-msoft-float` is the source default for ColdFire devices without one. `-mdiv`/`-mno-div` control the hardware divide choice. Do **not** use `-mcfv4e`: GCC documents it as the 547x/548x alias and says it includes hardware floating-point. [GCC 15.2 M680x0 options](https://gcc.gnu.org/onlinedocs/gcc-15.2.0/gcc/M680x0-Options.html) [GCC option override and flag definitions](https://gcc.gnu.org/git/?p=gcc.git;a=blob_plain;f=gcc/config/m68k/m68k.cc;hb=master) [GCC target flags/ABI definitions](https://gcc.gnu.org/git/?p=gcc.git;a=blob_plain;f=gcc/config/m68k/m68k.h;hb=master)
* GCC's own device-definition comment is decisive on EMAC generation: “the compiler does not (currently) generate MAC or EMAC commands.” No EMAC builtin interface was found in the GCC m68k machine description; its scheduler only recognizes an EMAC-capable target. Therefore ordinary C, vector types, and builtins will not emit representative `mac`, `msac`, or `movclr`; use handwritten GAS source or GCC inline `asm` (with correct clobbers) and let GAS assemble it. That establishes assembler acceptance/bytes, **not** compiler instruction selection. [GCC 15.2 device definitions](https://gnu.googlesource.com/gcc/+/refs/tags/releases/gcc-15.2.0/gcc/config/m68k/m68k-devices.def) [GCC m68k machine description](https://gcc.gnu.org/git/?p=gcc.git;a=blob_plain;f=gcc/config/m68k/m68k.md;hb=master)

### GNU binutils

* GAS accepts `-mcpu=54410`, `54415`, `54416`, `54417`, and `54418`; all select `mcfisa_a | mcfisa_c | mcfhwdiv | mcfemac | mcfusp`. It also accepts `-march=isac`, `-mcfv4e`, and extension switches including `-memac`; for this work use the chip name, because it additionally selects the MCF54418 control-register list. Objdump can disassemble with the resulting ELF architecture flags; the upstream EMAC test explicitly uses `--architecture=m68k:cfv4e`, a broad EMAC decoder setting, so test both the chip-selected object and targeted `objdump -d` output. [GAS CPU/architecture tables](https://sourceware.org/git/?p=binutils-gdb.git;a=blob_plain;f=gas/config/tc-m68k.c;hb=master)
* That CPU list gates `movec` names to `CACR, ASID, ACR0`–`ACR7`, `MMUBAR`, `RGPIOBAR`, `VBR`, `PC`, and `RAMBAR1` (plus documented legacy aliases). The MCF5441x addition assigns RGPIOBAR selector `0x009` and ACR4–ACR7 selectors `0x00c`–`0x00f`. This is a useful independent check for the local control-register table, but the device reference manual remains the semantic authority. [upstream MCF5441x patch](https://sourceware.org/legacy-ml/binutils/2009-11/msg00000.html)
* Public GAS regression material exists: `mcf-emac.s/.d` covers `mac`, `msac`, load/mask forms, all accumulators, `move` forms and `movclr`; expected output fixes words such as `a1c1` for `movclr.l %acc0,%d1` and records instruction lengths/operand order. `mcf-mov3q` covers ISA-C `mov3q`, and `br-isac` added ISA-C `stldsr`; opcode entries are feature-gated (`mcfemac` or `mcfisa_c`). The 2022 fix to EMAC `move` masks shows why GNU output is an oracle to compare against, not unquestionable copied truth. [EMAC test source](https://gnu.googlesource.com/binutils-gdb/+/master/gas/testsuite/gas/m68k/mcf-emac.s) [EMAC expected disassembly](https://gnu.googlesource.com/binutils-gdb/+/master/gas/testsuite/gas/m68k/mcf-emac.d) [ISA-C test/addition](https://sourceware.org/legacy-ml/binutils/2009-02/msg00104.html) [2022 opcode correction](https://sourceware.org/pipermail/binutils-cvs/2022-November/060438.html)

## What GCC/binutils can test

1. **Decode mechanics now.** Handwritten, public-source `.s` assembled with `m68k-elf-as -mcpu=54415`, then `objdump -dr`, can provide independently generated encodings, word lengths, operand order, accumulator selection, and positive/negative gates (for example EMAC accepted on 54415 but rejected with a non-EMAC CPU; ISA-C-only forms rejected under ISA-A). These are especially appropriate fixtures for `movclr` and the EMAC load forms already identified by the local extension.
2. **Generic compiler conventions.** Tiny C translation units compiled with `-mcpu=54415 -msoft-float` can characterize GCC's documented m68k ABI choices: downward stack; 32-bit parameter boundary normally and preferred 32-bit ColdFire stack alignment; D0 scalar return; A1 structure-return address; no argument registers; A6 frame pointer when needed; D0/D1/A0/A1 call-clobbered; and generated prologue/epilogue, `movem`, PIC, relocations, ELF `e_flags`, DWARF/CFI and relocation records. This is useful for a candidate Ghidra compiler spec and p-code fixtures, not a statement about the firmware. [GCC target ABI definitions](https://gcc.gnu.org/git/?p=gcc.git;a=blob_plain;f=gcc/config/m68k/m68k.h;hb=master)
3. **SLEIGH regression fixtures.** Commit a small, independently written source fixture plus expected *manually checked* byte/disassembly facts and the exact tool/version/configuration command. Use GNU output only to cross-check a manual-derived expected result. Exercise rejected encodings and p-code register/flag effects separately; assembly proves syntax/encoding, not the effect of saturation, MACSR, exceptions, or memory ordering.

### Local availability and reproducible route

GNU Binutils 2.47 is now built locally under ignored
`out/toolchains/prefix/binutils-2.47/`; nothing was installed system-wide. The
official `binutils-2.47.tar.xz` SHA-512 from Sourceware's `sha512.sum` is
`3126a1064374d8da40d4d70630c204ed1e75d542c447d53fca9778c7ceff095c28e9b445e15a313fef9729082d7966471ee6b5b715d479aa6d568528743e1d98`.
The out-of-tree configuration was `--target=m68k-elf --disable-nls
--disable-werror`; only `all-binutils`, `all-gas`, `install-binutils`, and
`install-gas` were needed. The installed tools report version
`2.47.20260726`.

The reproducible, committed fixture is
[`tests/fixtures/coldfire/mcf54415-oracle.s`](../../tests/fixtures/coldfire/mcf54415-oracle.s),
with its manually reviewed byte/disassembly contract and tool provenance in
[`mcf54415-oracle.json`](../../tests/fixtures/coldfire/mcf54415-oracle.json).
The generated object and raw binary remain under temporary/ignored paths.

## Limits

This cannot identify the firmware's compiler/version/options, C++ ABI/library, linker script, startup code, optimization choices, runtime, interrupt discipline, MMU/cache setup, or EMAC corner semantics. It cannot establish that firmware bytes are compiler output, prove a proposed calling convention, or replace the device manuals for privileged instruction behavior. In particular, GCC's no-EMAC-generation policy means a matching GNU EMAC byte sequence proves only the public assembler's encoding choice.

## Licensing

GCC is GPLv3-or-later; GAS/binutils are GPL-licensed. Treat the built tools and their source/opcode tables/tests as a **separate executable oracle**: do not copy implementation, opcode tables, or test text into the Apache-derived SLEIGH extension. Derive constructors and p-code independently from the public device/family manuals; retain only original minimal fixtures and cite the manual plus the GNU tool/version used for comparison.

Compiler-generated synthetic assembly, object files, and binaries from repository-authored trivial source are suitable committed fixtures in principle: generated output alone is normally not a derivative software implementation, but retain the authored source, tool release/version/configuration, command line, SHA-256, and a note that GNU output was used as an oracle. Prefer committing source and checked textual expected bytes/disassembly over opaque binaries; obtain a licensing review before importing any upstream GNU test text or distributing a fixture produced from nontrivial copied input.

## Completed smallest experiment

The 17-instruction fixture covers `movclr`, scalar MAC, MAC and MSAC load
forms, accumulator moves, ACR4–ACR7, RGPIOBAR, `mov3q`, `bitrev`, `byterev`,
`ff1`, and a trailing decode sentinel. GNU GAS produces the same 50-byte text
for `-mcpu=54415` and `-mcpu=54418`; `-mcpu=5206` rejects the source. The test
compiles the local SLEIGH source afresh, then pins instruction starts, lengths,
display text, and selected P-code data flow.

`68000:BE:32:ColdfireEMAC` gets all 17 instruction boundaries and the tested
EMAC/ISA-C operand forms right. Its only fixture mismatches are the five
MCF5441x control-register destinations: selectors `0x00c`–`0x00f` display as
`UNK_CTL` instead of ACR4–ACR7, and `0x009` displays as `UNK_CTL` instead of
RGPIOBAR. Their P-code writes a throwaway temporary rather than named state.

**Decision:** the fixture plus the manual-audited full control map is
sufficient evidence for a distinct `68000:BE:32:MCF5441x` language seam, but
not for broadening the generic ColdFireEMAC variant. The device manual assigns
chip-specific meanings to these selectors, and other MCF5441x selectors
collide with names in the inherited generic 68k table (for example
ASID/ACR/MMUBAR versus generic MMU registers). A new language ID can name
those registers without changing decode of other ColdFire targets; its
implementation may still share parameterized SLEIGH source with the generic
variant. The fixture does not reveal a new instruction-boundary blocker in
the firmware, so this remains a bounded correctness/readability improvement
rather than a reason to prioritize a full CPU-language rewrite ahead of the
active SHARC path.

## Sources

* [GCC 15.2 M680x0 options](https://gcc.gnu.org/onlinedocs/gcc-15.2.0/gcc/M680x0-Options.html) — official user-facing options.
* [GCC 15.2 m68k device definitions](https://gnu.googlesource.com/gcc/+/refs/tags/releases/gcc-15.2.0/gcc/config/m68k/m68k-devices.def) and [GCC 14.3 tag](https://gnu.googlesource.com/gcc/+/refs/tags/releases/gcc-14.3.0/gcc/config/m68k/m68k-devices.def) — exact device feature mask and explicit no-EMAC-generation statement.
* [GCC backend options](https://gcc.gnu.org/git/?p=gcc.git;a=blob_plain;f=gcc/config/m68k/m68k.cc;hb=master) and [target ABI definitions](https://gcc.gnu.org/git/?p=gcc.git;a=blob_plain;f=gcc/config/m68k/m68k.h;hb=master) — option resolution, defaults, ABI mechanics.
* [GNU GAS m68k configuration](https://sourceware.org/git/?p=binutils-gdb.git;a=blob_plain;f=gas/config/tc-m68k.c;hb=master) and [MCF5441x upstream patch](https://sourceware.org/legacy-ml/binutils/2009-11/msg00000.html) — CPU masks, control-register gates/names.
* [GNU EMAC regression source](https://gnu.googlesource.com/binutils-gdb/+/master/gas/testsuite/gas/m68k/mcf-emac.s) and [expected disassembly](https://gnu.googlesource.com/binutils-gdb/+/master/gas/testsuite/gas/m68k/mcf-emac.d) — public encoding/operand/length coverage.
