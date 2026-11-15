# SHARC interrupt save set (stock) and the adapter dispatchers

What the stock DSP interrupt chain saves, restores, clobbers and relies on, on DT2 OS 1.17 and DN2 OS 1.12, and what the machine and effect adapters save around a module call. It is the stock side of the DSP rules in [sdk.md](../plans/sdk.md) section 9. Everything here is read from the code, with no emulator run. A second agent checked claims 1-7 against the image bytes of both images with two decoders (our `sharc_core` decoder and the selache `seldump` and `selas` tools, which assemble the claimed instructions and compare the bytes). Those sections are **[V]**. Claim 8 and the adapter section stay **[D]**.

## Result

**[V]** An interrupt reaches a handler through three pieces of stock code, in this order:

1. a vector stub (it saves a few registers and reads the interrupt id),
2. the wrapper at `0x1c02a4` (it saves nearly everything else, empties the loop stack, zeroes the L registers and calls the handler through a table),
3. a return stub that the vector stub names as the return address (it restores what the vector stub saved).

**[V]** The wrapper's code is byte for byte the same on DT2 1.17 and DN2 1.12. The two vector stubs and the return stub have the same code on both images; only the addresses and three literals differ.

| piece | DT2 1.17 | DN2 1.12 |
| --- | --- | --- |
| wrapper, `0x1c02a4` to `0x1c066b` (return label `0x1c04b7`) | same address | same address |
| vector stub, generic (no interrupt id read) | `0x1c0b1e` | `0x1c0a70` |
| vector stub, SEC interrupt (reads SHDBG_SECI_ID `0x300eb`) | `0x1c0b7b` | `0x1c0acd` |
| return stub (`PCSTK = ` literal of the stubs) | `0x1c0be2`, `0x1c0be5` | `0x1c0b34`, `0x1c0b37` |
| callback table base used by the stubs (literal) | `0x240ad8`, `0x240948` | `0x240aa0`, `0x240910` |
| handler table used by the wrapper (`I4 = 0x2404b0`, `I12 = DM(I4, M4)`) | `0x2404b0` | `0x2404b0` |

The DN2 stub addresses are the DT2 ones minus `0xae`. The sharcdb function hashes of `0x1c02a4` have the same `reloc_hash` on both images (`6fb397d5...`); the `exact_hash` differs because the database function span runs on into the next function. That function starts at `0x1c066c`; it loads `R8 = 0x1c02a4` at `0x1c0675` (it registers the wrapper) and its CALL target differs (DT2 `0xb89266`, DN2 `0xb8b782`). The identical range is `0x1c02a4` to `0x1c066b`, 968 words; the sha256 of the range is equal on both images.

## What the vector stub saves

**[V]** Stack discipline: I7 is the stack pointer, M7 is -1 (push) and M6 is +1. The SEC stub (`0x1c0b7b`) does, in order:

- stores I7 and B7 in fixed words `DM(0x90117)` and `DM(0x90118)`, then converts both with ACONV (Type 7d);
- pushes PCSTK, then the old PX (PX1, PX2), then R4, R8 and R12 through PX (each as PX1, PX2);
- reads the interrupt id into R4 (`R4 = DM(0x300eb)`, written back to acknowledge) and builds R8 and R12 from it, so R4, R8 and R12 are the dispatch arguments from here on;
- saves I12 (`DM(I7, M6) = I12`), then I6 and B6 (through R12) and the original I7 and B7 into the same frame;
- sets `PCSTK` to the return stub, pushes the PC stack (`ppu`) and enters the callback with a delayed RTS. The callback is the wrapper for generic handlers.

**[V]** Before the delayed RTS the stub also converts I6 and B6 (`0x1c0b6f`, `0x1c0b72` on DT2; DN2 minus `0xae`), not only I7 and B7. It reads B6, which was saved at I7+2.

**[V]** The return stub (`0x1c0be5` / `0x1c0b37`; `0x1c0be2` / `0x1c0b34` is a store to the MMR `0x3108900c` in front of it) pushes the PC stack (`ppu`), reloads PCSTK, B6, I6, R12, R8, R4, the old PX and B7, and ends with the `11c` form with x=1 (a return from interrupt) whose two delay slots reload I7 and I12 from the frame. The generic stub (`0x1c0b1e`) has the same saves; it builds its table index from ASTATX bits 11:0 (`R4 = ASTATX`, `R8 = fext(R4, 0, 12)`) instead of reading SHDBG_SECI_ID.

**[V]** So the stub chain saves and restores R4, R8, R12, I6, I7, I12, B6, B7, PX and PCSTK. It does not save I0-I5, I13, M0-M15, B0-B5, B8-B15, L0-L15, S0-S15 or the multiplier registers.

## What the wrapper saves, by frame slot (offsets from I6)

**[V]** The wrapper first tests `BIT TST MODE1STK, 0x10000` (`0x1c02a4`) and jumps on `NOT TF` to `0x1c02e8` (path B). MODE1 bit 16 is the RND32 bit of the interrupted code's MODE1 (the saved word). Both paths then build a frame: path A pushes a zero word and R0, path B pushes R0 as a PX1, PX2 pair; then `R0 = I6`, `I6 = I7`, push R0 (the link), `I7 -= 0x84`, MODE1STK to `[I6-2]` and PCSTK to `[I6-1]` (`DM(I6, M7)` is a pre-modify with M7 = -1). Path B also builds the saved MODE1STK differently (`R0 = MODE1`, set bit 12, set bit 16 when MMASK bit 16 is clear). It sets IRPTEN in the saved MODE1STK (`MODE1STK = set(MODE1STK, 0x1000)` at `0x1c02bb`).

| group | registers | slots | path |
| --- | --- | --- | --- |
| data registers, direct | R1, R2, R4; S0-S15 | `-55, -54, -53`; `-52 .. -37` | A (bit 16 set) only |
| data registers through PX (two words each, PX1 then PX2) | R1-R7, R9-R11, R13-R15; S0-S15 | `-113 .. -88` for R1 to R15 in order, `-87 .. -56` for S0 to S15 | B (bit 16 clear) only |
| common: DAG | M4, B4, I4, L0-L5, M12, B12, B13, I13, L8-L15 | `-36, -34, -31, -29 .. -24, -35, -33, -32, -30, -23 .. -16` | both |
| common: status | STKYX, STKYY, ASTATX, ASTATY | `-0x73, -0x72, -0x75, -0x74` | both |
| common: loop | LCNTR; every LADDR and CURLCNTR pair of the loop stack (popped with `lpo` until the empty flag, STKYX bit 26, is set); the depth | `-15`; stack area at I4; depth at `[I7]` | both |
| common: multiplier | MRF and MSF (R0-R2 read with MR data moves, the same moves with MODE1.PEYEN set give S0-S2 from the PEy accumulators), MRB and MSB (same) | `-14 .. -9`, `-8, -7 .. -3` | both |
| PX | overwritten on both paths: path B stages the R pushes through PX; the common tail at `0x1c065b` to `0x1c0664` reloads R0 through PX (`PX2 = DM(M6, I6); PX1 = DM(2, I6); R0 = PX`) | not saved by the wrapper; the vector stub saved it and the return stub reloads it | both |

R3, R5-R7, R9-R11, R13-R15 and their PEx twins are **not** saved on path A: the wrapper relies on the handler to keep them (the C callee-saved set, see `sdk/dsp/contract/call.inc`). R8 and R12 are not saved by the wrapper (the vector stub did).

**[V]** Before the call the wrapper: pops the PC stack and the status stack once (`ppo`, `spo` at `0x1c03e7`, `0x1c03ea`), sets L0-L5 and L8-L15 to 0 (17b immediates, `0x1c0435` to `0x1c0451`), sets `I4 = I7`, sets and clears MODE1.PEYEN around the MR data moves (`0x1c0453`/`0x1c045d`, `0x1c046d`/`0x1c0477`), then loads `M4 = R8` (the selector), `R8 = R12`, `I4 = 0x2404b0`, `I12 = DM(I4, M4)`, saves the frame link in R2 and enters the handler with `JUMP (M13, I12) (DB)` (`0x1c04b1`) with the return address `0x1c04b6` pushed in the delay slots (the continuation is `0x1c04b7`).

**[V]** After the handler: it clears IRPTEN (`0x1c04b7`), refills the loop stack, sets IRPTEN again (`0x1c04d0`), reloads the registers of the table in reverse (R, S, MR through MODE1.PEYEN again, M4, B4, I4, M12, B12, B13, I13, L0-L5, L8-L15, ASTATX, ASTATY, LCNTR, STKYX, STKYY), clears IRPTEN (`0x1c0652`), pushes the status stack and the PC stack (`spu`, `ppu`), reloads MODE1STK from `[I6-2]` and PCSTK from `[I6-1]`, and returns with a delayed `RTS` whose slots restore I7 and I6 (`0x1c0666` to `0x1c066b`).

**[V]** Written by the wrapper and not restored by it: R8 (`R8 = R12`) and I12 (the handler address); the return stub restores both from the vector stub's saves. The wrapper does not write R12. PX is overwritten on both paths (path B stages the R pushes through PX; the common tail reloads R0 through PX); the vector stub pushed the old PX and the return stub reloads it.

## What the chain assumes at the interrupt (live-in registers)

**[D]** Read before written, with no save: I6, I7, L7, B7, M5, M6, M7, M13 (the wrapper's pushes and the delayed jump need M7 = -1, M6 = +1, M5 = 0, M13 = 0 and linear addressing of the stack I7). MODE1 bit 16 selects the path. The wrapper reads M5 as the constant 0 (`R0 = M5` at `0x1c048c`). B6 is also read, but the stub saves and converts it, so it is not required to hold a fixed value. I6 is copied and saved by the wrapper, so it need not hold a fixed value either; what the chain needs is a valid stack in I7 with L6 = L7 = 0.

**[D]** What the handler sees: L0-L5 and L8-L15 are 0; I4 = the frame area pointer; R8 = the argument word; M4 = selector; I12 = handler; the frame and the saved copies are below I6.

## Cross-check with the sharcdb tables

**[D]** `regdef` and `reguse` of the database for the instructions that the analysis decoded (`dt2-1.17.sqlite` and `dn2-1.12.sqlite`): 68 registers written, the same 68 as in the analysis. The analysis adds MRF and MRB (the database has no register effect for MR data moves) and reads of B7, L7 (post-modify of I7), I12, M13 (the indirect jump) and MRF, MRB. No register is only in the database. The register effects come from `dksharc` (`packages/dksharc`) run on the words from `0x1c02a4` (968 words of wrapper plus the start of the next function) with the continuation `0x1c04b7` added as an entry (421 instructions, no undecoded word, both functions complete, no unknown call).

## The adapter dispatchers

**[D]** From the sources (`modules/core/adapters/fx/src/dsp/fx.s`, `_fx_enter` and `_fx_leave`; `modules/core/adapters/dsp-voice/src/dsp/dsp-voice.s`, the frame stores `DM(-n, I6)` and the epilogue reloads), not from the image.

| | machine adapter (`dsp-voice`) | effect adapter (`fx`) |
| --- | --- | --- |
| saved and restored around the module | R3, R5-R7, R9-R11, R13-R15; I0-I3, I5; M1-M3; B3; I13; the context registers M5, M6, M7, M13, M14, M15, L0-L5; MODE1 (frame `-6 .. -38`) | R1-R15 (R0, I4, L4 by the hook tail); I0-I3, I5, I12, I13; M1-M4; B3; the context registers M5, M6, M7, M13, M14, M15, L0-L3, L5; MODE1 |
| pinned for the module | M5 = 0, M6 = 1, M7 = -1, M13 = 0, M14 = 1, M15 = -1, L0-L5 = 0, MODE1 with PEYEN, TRUNCATE and ALUSAT clear and RND32 set | the same, L4 = 0 by the hook |
| restores I12 from | the stock frame (`I12 = DM(M7, I6)`: the return address) | its own copy |
| not saved | I4 (free on the stock path), I6, I7, I8-I11, I14, I15, M0, M8-M12, B0-B2, B4-B15, L6, L7, S0-S15, MR, LCNTR, loop and PC stack, ASTAT, STKY, PX | I6, I7, I8-I11, I14, I15, M0, M8-M12, B0-B2, B4-B15, L6, L7, S0-S15, MR, LCNTR, loop and PC stack, ASTAT, STKY, PX |

The machine adapter masks interrupts only in the `DV_C3_NOIRQ` build; the default build leaves IRPTEN as it is, so the wrapper above can run inside module code.

## Comparison with the rules of `sdk.toml`

`[machine.c].rules` and `[fx.c].rules` say: may clobber R0-R15, I0-I5, M1-M4, B3; must keep I6-I15, M0, M5-M15, L0-L7, MODE1. Nothing was changed.

**[D]** No mismatch that makes the rules unsafe for the interrupt chain:

- Everything the wrapper relies on at the interrupt (I6, I7, L7, M5, M6, M7, M13) is on the "must keep" list. B7 is needed too (the vector stub reads it) and is not named by the rules.
- The registers in the "may clobber" list are protected across an interrupt: R0-R15 by the stub chain and the wrapper (path A relies on handlers keeping R3, R5-R7, R9-R11, R13-R15 and I0-I3, I5, M1-M3, B3), I4 and M4 by the wrapper.
- The L registers the wrapper zeroes for the handler are restored, so the "must keep L0-L7" rule does not conflict.

**[D]** Differences and gaps to decide (rules untouched):

1. **Registers named by no rule** that the chain treats as live: B0-B2, B4-B15, S0-S15, the multiplier registers, LCNTR and the loop stack, PX, ASTATX/Y, STKYX/Y. The wrapper saves B4, B12, B13, S0-S15, MRF/MRB/MSF/MSB, LCNTR, the loop stack and the status registers, so a module may use them across interrupts. A module that clobbers B0-B2, B5, B8-B11, B14 or B15 is not repaired by either adapter (neither saves them).
2. **I13 and I12.** The machine adapter header says a routine may clobber I13; the rule says keep I6-I15. The adapter saves I13 and reloads I12 from the stock frame, so the rule is stricter than the adapter needs for these two. The routine still needs I13 for its return `JUMP (M13, I13)`.
3. **The effect adapter saves** MODE1, M5-M7, M13-M15, L0-L3, L5, I12 and I13, so it tolerates an effect that changes them. The rule says keep them. The effect chain runs several rows in sequence with the pins in place, so the rule stays right for M5-M7, M13-M15, L0-L5 and MODE1 as inputs of the next row.
4. **The wrapper's path split depends on MODE1 bit 16.** The pins set RND32 (bit 16), so module code takes path A, which saves only R0-R2, R4 and the S registers and leaves the callee-saved R registers to the handler. A module that clears RND32 takes path B (full save). Both are safe; the rule "must keep MODE1" is stricter than the wrapper needs.

## Claims checked against the image bytes

Claims 1-7 are **[V]**. Claim 8 is **[D]**: derived by hand; no read-before-write tool pass yet.

1. **[V]** Wrapper identical on both images from `0x1c02a4` to `0x1c066b` (968 words).
2. **[V]** `0x1c02a4` is `BIT TST MODE1STK 0x10000`, `0x1c02a7` the `IF NOT TF JUMP 0x1c02e8`.
3. **[V]** Path A stores R1, R2, R4, S0-S15 at `0x1c02bf` to `0x1c02e6`; path B stores R1-R7, R9-R11, R13-R15 and S0-S15 through PX (`0x1c02e8` to `0x1c03e5`). R8, R12 are not stored by the wrapper.
4. **[V]** The common stores from `0x1c03ed` to `0x1c0435`: M4, B4, I4, L0-L5, STKYX, STKYY, M12, B12, B13, I13, L8-L15, ASTATX, ASTATY; the L zeroing at `0x1c0435` to `0x1c0451`; the MR moves and their stores at `0x1c0453` to `0x1c048a`; LCNTR at `0x1c047d`.
5. **[V]** The handler call at `0x1c04ab` (`I12 = DM(I4, M4)`) and `0x1c04b1` (`JUMP (M13, I12) (DB)`), the continuation at `0x1c04b7`.
6. **[V]** The reload of the same registers from `0x1c04d3` to `0x1c064f`, the status and PC stack pushes at `0x1c0655` and `0x1c0658`, the final delayed `RTS` at `0x1c0666`.
7. **[V]** The SEC vector stub (DT2 `0x1c0b7b`, DN2 `0x1c0acd`) saves PCSTK, old PX, R4, R8, R12, I12, I6, B6, I7, B7; the return stub (DT2 `0x1c0be2`, DN2 `0x1c0b34`) restores them.
8. **[D]** Live-in at the interrupt: I6, I7, L7, B7, M5, M6, M7, M13 (derived by hand; no read-before-write tool pass yet).

## Decoder disagreements

**[D]** The two decoders differ in two places; a third difference was a printing error.

- `0x1c0c08` (DN2 `0x1c0b5a`), words `0bfe 9828 0b85`: selache reads one 48-bit `rti, r11=exp r8`. Our decoder reads RTI (DB) plus two delay slots. The frame offsets pair up only with our reading, and R11 is not saved, which also supports it. Selache cannot confirm the RTI.
- `bf04 80xx` words: our decoder reads ACONV (doc 06, from the manual). Selache reads `modify(Ix, Mx) (NW)`.
- The jump at `0x1c04b1` (`083f 2c3f`) is `JUMP (M13, I12) (DB)`; both decoders and dksharc's def/use (reads I12, M13) agree. The Python renderer `tools/sharc_dev/sharcfn/render.py` printed it as `PM(I4,M5)` because it left out the DAG2 offset; that is fixed.

## Open

**[O]** The register state at the first vector instruction (what the hardware has already pushed on the status and PC stacks, and the ACONV of I7 and B7) is read from the code only. How the callback table entries map to the wrapper and to other handlers (the entries that do not point at the wrapper) is not covered. Whether any generic handler breaks the C callee-saved set that path A relies on is not checked.
