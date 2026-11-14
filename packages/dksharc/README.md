# dksharc

Static analysis of the SHARC+ DSP section of a digikit module. Rust, no Python; builds for the host and `wasm32-unknown-unknown`, so the
workbench can use it in the browser.

```rust
let input = dksharc::Input::from_dkds(dkds_section_bytes, dko_json_bytes)?;
let analysis = dksharc::analyze(&input);
let json = serde_json::to_string(&analysis)?;
```

`Input` is a plain struct (text parcels, relocations, imports, symbols, entries); `from_dkds` fills it from the DKDS section of a `.dkm`
(layout in `packages/patchkit/src/dkmdsp.rs`) and the `.dko.json` of the DSP object (text size and symbols).
`dksharc::dkds` is the one reader of the DKDS container (header, tables, imports, relocations) and `dksharc::dko` the one model of the `.dko` object; patchkit's `DspSection::parse` and `Dko` build on them. A caller that has the pieces
already can fill the struct itself.

## Usage

`wb module analyse` prints the analysis of a module (`tools/devtools/wb.mjs`; it calls the same code through `dkprof.wasm`, so the workbench and the CLI agree). It takes a
`.dkm` (its DKDS section is found by the magic) or a bare DKDS section, and the `.dko.json` of the module's DSP object. From the repository root, with the patchkit binary built
(`pnpm nx:pilot run patchkit:cli`) and `mise run wasm:dkprof` run once:

```sh
mkdir -p out/try
dist/patchkit/patchkit --root . pack digikit/pluck -o out/try/pluck.dkm --device dn2
node tools/devtools/wb.mjs module analyse out/try/pluck.dkm packages/dksharc/tests/fixtures/pluck-dsp.dko.json
# 306 instructions, 3 functions, 26 blocks, 4 loops, 6 calls (4 import), 0 undecoded
# function _pk_coef     +0x0000..+0x00fd  0 loops  complete true
# function _pk_note_on  +0x00fd..+0x01e7  3 loops  complete true
# function _pk_render   +0x01e7..+0x0298  1 loops  complete true
# loop depth 0 body +0x0170..+0x0194  count Reg { ureg: 0, name: "R0" }
# ...
```

`--format json` prints the whole `Analysis` as one JSON line (the shape below). The `.dko.json` here is the crate's fixture of our pluck machine; the object of another module is in
`out/cache/dko/<module>/<hash>/build/*-dsp.dko.json` after its build.

## What it gives (`Analysis`, serde JSON)

- `insns`: every confidently decoded instruction: pc (short word), length in parcels, form name, import name of a relocation on it, memory operands.
- `functions`: named from the `.dko`. They start at the first instruction, at global text symbols, at the voice and effect entries of the
  DKDS section and at the targets of calls inside the module. Local labels are not functions.
- `blocks` of each function, with the control transfer that ends the block, its delay slots (`delay_slots`, two instructions after a delayed
  transfer) and edges (`fall`, `taken`, `loop_back`, `return_site`).
- `loops`: DO loops with the pc of the DO, first and last body instruction, count (`lit`, `reg` with the Ureg, `until` for a condition),
  parent and depth.
- `calls`: inside the module (`local`), import calls resolved through a class 2 relocation on the call (`import`, label `import:<symbol>`),
  indirect (`indirect`) and calls to a direct address outside the text (`outside`).
- memory operands per instruction at form level: DM or PM, load or store, mode (post-modify, pre-modify, immediate offset, direct address),
  I and M register numbers (I0-I15, M0-M15), immediate or address, normal, long word or sub-word access.
- `defuse` per instruction (slice 2): registers read and written by name (R0-R15, I, M, L, B, S, the system registers, and `MRF`, `MRB`,
  `MSF`, `MSB`, `BFFWRP`, `BFF_HI`, `BFF_LO` for the multiplier and the bit FIFO), including the index registers an address post-modify
  writes (`post_modify`) and the loop counters of a DO. Also: `conditional` (a condition other than "always": every effect is "may"),
  `astat_bits` (ASTATX bits defined or forgotten), `mode1` (`full` with the value of an immediate, `set`, `clear`, `toggle` with their mask,
  `push_sts`, `pop_sts`), `stack` (the PC, loop or status stack is pushed or popped), `simd_reads` and `simd_writes` (the S registers and
  ASTATY/STKYY that the same instruction also touches when MODE1.PEYEN is set) and `uncertain`.
- `effects` per function: `writes`, the registers written by an instruction on a path from the entry to a return (blocks that cannot
  reach a return do not count), with the writes of local callees added to a fixed point; `simd_writes`; `mode1`, the MODE1 writes and the
  net effect by a data flow over the blocks (each bit is its entry value, the inverse, a constant or unknown; `Rn = MODE1` ... `MODE1 = Rn`
  and `PUSH STS` ... `POP STS` restore it; `restored` is true only for an exact analysis with no changed bit); `returns` and
  `indirect_returns` (a `JUMP (M13, I13)` or `JUMP (M14, I12)` is taken as the return of the module contract, the address in the register
  is not checked); `other_exits` (every other indirect jump and every jump out of the function: not analysed); `unknown_calls` (imports,
  indirect calls and calls outside the text: their effects are not known); `uncertain_pcs`; `complete` (the write set is complete: no
  undecoded word, no uncertain instruction, no unknown call, no unanalysed exit).

## Fail closed

An instruction that does not decode as `Confident` is listed in `undecoded` (pc, `unknown` or `uncertain`, the raw parcels) and is not
part of `insns`. The sweep goes on after it, by the decoded length for an uncertain form and by one parcel for an unknown word. The functions
that contain it have `complete: false`, as do functions with a control transfer that is not modelled (`10a_rel`, RTI, loop re-entry).
Structural findings (a branch into a delay slot, a DO whose end is not an instruction, a symbol off an instruction start) are in
`diagnostics`.

The same rule holds for the register effects. A compute opcode that the SHARC core does not model, a form without an effect program
(`10a`, which the ISA table does not define, and the undocumented forms) and an undocumented bit operation give `defuse.uncertain` and no
guessed registers; a function with such an instruction on a path has `effects.complete: false`. The core models 283 of the 1024 single
function compute opcodes, 25 of the 64 multifunction categories and 16 of the 64 shift immediate opcodes: the unit tests and the pluck
machine use only modelled ones.

## Limits

- Linear sweep: data in the text section (tables) decodes as code. Symbol kinds could mask them (not done).
- Delayed-branch slots are the next two decoded instructions. Register effects are per instruction and per path of the block graph;
  there is no value tracking, so a computed address or a register used as a MODE1 copy that is saved and restored through memory is not
  followed (the bit shows as unknown in `mode1`).
- The effects are what the SHARC core defines: a register-to-register move onto itself and a shift by zero into the same register are
  reported as writes (the fast tier does not write them). A data register access also has an S register twin under SIMD; that is listed
  apart (`simd_*`) because MODE1.PEYEN is not known statically. Reads of the system registers by a compute (MODE1, ASTATX, the MR
  accumulators) are those the core traces and may be more than the hardware reads; the data register reads of a compute are exact
  (a register whose value changes neither the result nor the flags is not listed).
- No width override from an assembler listing: decoding uses `sharc_rt::decode::decode_at` (lookahead for the 16-bit and 32-bit width ties).
- Memory operands name index registers, not addresses: no value tracking.

## The register effect table

`src/defuse_table.rs` is generated by `tools/sharc_dev/sharc_defuse_gen.py` from `tools/sharc_dev/sharc_core` (read only: the generator does
not change `CORE_FINGERPRINT`). `uv run python -m tools.sharc_dev.sharc_defuse_gen` rewrites the table and the golden vectors,
`--check` fails when they are stale, `--report` prints the verification. It has three parts:

- compute fields: every (unit, opcode), multifunction category, short opcode, MR data move and shift immediate opcode is run through the
  core's `_compute` with distinct register values; a write is a destination the core returns, a read is a register whose value changes the
  result or the flags (checked on four operand sets);
- the universal registers: aliases written together (PX with PX1 and PX2), long word pair mates and the SIMD companions, from the core's
  own tables;
- an effect program per instruction form (which fields name which registers), written in the generator and checked against the core over
  400 random field assignments per form: the registers the core changes must equal the registers the program writes. Forms the core cannot
  run are checked through a sibling with the same register fields (`1b` through `1a`), or listed as `unverified` (`9b_rel`, `13a`, `22a`).

`defuse.rs` interprets the programs; `fxsum.rs` makes the per-function summaries.

## Tests

`cargo test -p dksharc` or `pnpm nx:pilot run dksharc:rust-tests`. `tests/pluck.rs` is the oracle on our pluck machine for the
Digitone II (`tests/fixtures/`: the DKDS section and the DSP `.dko.json`; no firmware bytes). `tests/synthetic.rs` has nested loops, delay
slots, calls, an import call, an unknown word and an uncertain decode. `tests/defuse.rs` has 600 golden instructions (encoded, with the
effects the generator computed from the core; `tests/fixtures/defuse-golden.json`), the loop body of `_pk_render` checked by hand against
`modules/machines/pluck/src/dsp/pluck.s`, and synthetic text: a multifunction compute, DAG post-modify and pre-modify, a long word load, a
conditional write, an unmodelled compute, a width tie, MODE1 bit operations (set and cleared, toggled twice, saved in a register and
restored, `PUSH STS` and `POP STS`, changed on one branch only), paths to the return, a local callee, an import call and a DO loop.

Cross-check against the fast tier (run once on 2026-11-15, not kept as a test): the register-file writes of 5463 generated instructions that
`packages/emulator/sharc/src/fast/lower` can lower were compared with `Lower::written_mask`. 5457 agree; the other 6 are `Rn = Rn`-style
register moves (source and destination are the same register), which the lowering does not write and the core does.
