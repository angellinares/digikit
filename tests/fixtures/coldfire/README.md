# MCF5441x GNU/Ghidra oracle fixture

`mcf54415-oracle.s` is an original synthetic fixture derived from the public
NXP MCF54418 reference manual and ColdFire programmer's reference manual. It
contains no firmware material and does not copy GNU source or test text.

The checked facts in `mcf54415-oracle.json` were assembled and disassembled
with GNU Binutils 2.47 configured for `m68k-elf`. The release archive and its
official SHA-512 are pinned in that file. The local build is intentionally
kept under ignored `out/toolchains/`:

```sh
prefix="$PWD/out/toolchains/prefix/binutils-2.47"
"$prefix/bin/m68k-elf-as" -mcpu=54415 -o /tmp/mcf54415.o \
  tests/fixtures/coldfire/mcf54415-oracle.s
"$prefix/bin/m68k-elf-objdump" -dr /tmp/mcf54415.o
```

Both `-mcpu=54415` and `-mcpu=54418` must produce the checked byte stream.
The same source must be rejected by `-mcpu=5206`, covering both the EMAC and
ISA-C feature gates as well as MCF5441x-only control-register names.

The `ghidra_current` entries are the manually reviewed output of
`68000:BE:32:ColdfireEMAC`. The five `ghidra_target` entries identify the
bounded mismatch exposed by this fixture: the generic language preserves the
instruction lengths but calls MCF5441x selectors `0x009` and `0x00c`–`0x00f`
unknown and gives their writes no named-register P-code destination. This is
chip-specific privileged state. Together with the selector collisions in the
manual-audited full MCF5441x control map, it motivates a distinct MCF5441x
language ID rather than broadening the generic ColdFireEMAC language. The five
fixture mismatches alone do not rule out sharing a parameterized SLEIGH
implementation between those language IDs.
