# Public SHARC+ tooling options **[D][O]**

This is a bounded tooling assessment, not a firmware or runtime finding. The
project's typed ISA (`tools/sharc_isa.py`) remains the encoding authority;
generated SLEIGH and p-code are adapters. No speedup was benchmarked here.

## Decision

- **[D] Reuse the existing pinned Selache adapter for *differential encoding
  checks*, not architectural semantics.** Its public `selinstr` component
  encodes and disassembles SHARC+ instructions, including VISA; the repository
  already has `tools/sharc_selache.py` and `tools/selasm.py`. Compare instruction
  extent, operand fields and predicate rendering on synthetic fixtures. Its
  disassembler is not an independent execution model for ShiftImm side effects
  or MODE1/SIMD. Selache's public README says its tools are GPL-3.0; keep it
  external/read-only and review licensing before copying or linking its code.
  [Pinned upstream README](https://raw.githubusercontent.com/js216/selache/2b26d3b75c53063575bc5c820fa0d38879335187/README.md),
  [pinned disassembler source](https://github.com/js216/selache/blob/2b26d3b75c53063575bc5c820fa0d38879335187/selinstr/src/disasm.rs).
- **[D] Keep SLEIGH + pypcode for cheap lift probes, not as an independent
  semantic oracle.** Ghidra documents SLEIGH as the bit-to-p-code translator;
  pypcode uses SLEIGH, so agreement between them does not independently
  establish SHARC+ behavior. Use the existing `tools/sharcpcode.py measure`
  (without `--ghidra`) and `compare`, and query its SQLite outputs before any
  serialized Ghidra run. [Ghidra SLEIGH reference](https://github.com/NationalSecurityAgency/ghidra/blob/master/GhidraDocs/languages/html/sleigh.html),
  [pypcode README](https://github.com/angr/pypcode/blob/master/README.md).
- **[O] No verified drop-in SHARC+ execution replacement was found.** A nearby
  public MAME device implementation identifies a classic `adsp21062`; its
  compatibility with SHARC+ VISA and this project's conditional/SIMD semantics
  is not established. [MAME source](https://github.com/mamedev/mame/blob/master/src/devices/cpu/sharc/sharc.h).

## Bounded frontier-report experiment

Before adding an engine, a **small read-only frontier report** can reuse
existing interfaces: select distinct stop owners from v5
`writer_function_facts`, cross-reference recovered `static_snapshot.instructions`,
and check each candidate against loader-final `LoadedMemory.read_sw` and the
typed decoder. Print the predicate, any relevant compute opcode,
owner/stop-count units, and source hash separately. The cache does not retain
each stop PC: do not attribute all 34 Type6a function stops to one instruction
from shared bytes alone.
Measure this report's wall time and mismatch yield against the current manual
workflow before keeping new code. No new firmware artifact should be committed.

Independent lanes can check public manual/opcode mapping, loaded-byte
provenance, and cached function impact in parallel. Serialize shared semantic
edits, cache publication, language installation and Ghidra project access.
In particular, the observed conditional Type6a ShiftImm opcode `0x04` is not
handled by `tools/sharc_trace.py`; neither a predicate-only patch nor a new
disassembler would close that semantic gap. The byte-backed conditional Type7a
frontier in finding 06 is a better immediate target for this report. **[O]**
