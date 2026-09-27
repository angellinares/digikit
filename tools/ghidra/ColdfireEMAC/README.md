# ColdfireEMAC Ghidra language

Language id `68000:BE:32:ColdfireEMAC`. It is stock Ghidra 12.1.3's
`68000:BE:32:Coldfire` with the EMAC instructions fixed:

- `movclr.l ACCy,Rx` added. Stock has no constructor for it, so the words
  `a1c0 a3c1 a5c2 a7c3` in interrupt handler prologues decode as bad
  instructions and Ghidra's flow analysis stops there.
- `move.l ACCy,ACCx` copied from the destination into the source; fixed.
- MAC and MSAC with load selected the wrong accumulator (CFPRM p.6-4: bit 7
  is the inverse of the lsb for the load forms); fixed with `accregload`.
- `move.l Ry,ACCx`, `move.l Ry,ACCext01` and `move.l Ry,ACCext23` printed
  their operands in the wrong order; fixed.
- The word forms of MAC, MSAC and the dual-accumulate variants (`mac.w`,
  `msac.w`, `maaac.w`, `masac.w`, `msaac.w`, `mssac.w`, with and without load)
  multiplied two 16-bit operands into a 16-bit temporary, so the decompiler
  showed the product truncated to 16 bits. The product is now the 32-bit
  signed product (signed mode, as the stock `sext` already assumed).
- `sats.l` saturated on `Dx == 0` instead of on the sign of the overflowed
  result. Now: if V, `Dx[31] == 0` gives `0x80000000`, otherwise `0x7fffffff`
  (CFPRM; QEMU's `helper_sats` does the same).

Not modelled: the saturation and rounding in `movclr` (a pcodeop,
`emacSaturate`), and its clearing of ACCext and MACSR[PAVx].

Files in `data/languages/` other than `coldfire_emac.*` are copied from
`Ghidra/Processors/68000/data/languages/` in Ghidra 12.1.3 under the Apache
License 2.0 (`LICENSE.txt`). `68000_emac.sinc` is a modified `68000.sinc`;
every change is marked `# EMAC-LANG:`.

Install with `tools/ghidra/install-coldfire-emac.sh`. It compiles the `.sla`,
writes `extension.properties` for the installed Ghidra version, and puts a
symlink to this folder in Ghidra's user Extensions folder. Both
`analyzeHeadless` and pyghidra load it from there. Projects that use the
stock language are not affected. Re-run the script after a Ghidra upgrade,
because the user settings folder name contains the version.
