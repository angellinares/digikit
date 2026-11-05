# Reverse-engineering tools

This repository includes small, purpose-built tools for extracting firmware,
querying Ghidra, measuring the emulator, and analysing the SHARC+ program.
This page is the index: use each tool's `--help` and module docstring for its
full command line. Run output and diagnostics (`out/runs/`, `--diag`) are in
[INSTRUMENTATION.md](INSTRUMENTATION.md).

## Generated-file rule

Firmware and anything derived from it must stay out of git. This includes
`.syx` files (`firmware/`), extracted sections (`out/sections/`), snapshots, Ghidra dumps, measurement
databases, disassembly reports, and mapped string output. Put repeatable
results under `out/`; commit the tool and synthetic tests, not its output.

Before trusting an emulator run, confirm that `out/sections/<image>/.source-sha256`
matches the source `.syx`. Bound emulator runs with an instruction limit.

The full test suite is:

```sh
uv run --with pytest python -m pytest tests -q
```

## Firmware and container tools

| Tool | Purpose |
| --- | --- |
| `python -m tools.firmware.extract` | Extract all sections from a supplied `.syx`. This is the normal entry point. |
| `tools/firmware/roundtrip.py` | Rebuild and re-extract a container as an acceptance gate for the repack chain. |
| `tools/firmware/content_hmac.py` | Inspect or calculate the container's content-authentication trailer. |
| `tools/patchkit/patchimg.py` | Apply byte-exact, preconditioned patches to an extracted section image. |
| `tools/coldfire/ddr_geometry.py` | Derive DDR geometry from the bootstrap's initialization sequence. |
| `tools/coldfire/regions.py` | RAM map of a MAIN OS image, flash caves and the expansion region per image; `confirm` canary-fills a region and runs the emulator; `loader-test` boots a patched image with a reset-path loader. |
| `tools/firmware/plusdrive_check.py` | `mise run dev:plusdrive-check`: build a `plus_drive/`-style fixture (nested folders, a plain file, a 702-entry directory, multi-extent files) with `plusdrive build`, boot the DT2 with it and compare the firmware's own mount, readdir, lookup and file reads with the folder (about 1 min). |
| `tools/firmware/aplib_check.py` | Pack every section of the two firmwares with `tools/firmware/aplibpack.py` and check it with `tools/firmware/elz.py` and the bootstrap's own depacker on the native core. |

Never patch the bootstrap or updater sections. See
[`PATCHING.md`](PATCHING.md) before producing an image intended for hardware.

## ColdFire static analysis and Ghidra

| Tool | Purpose |
| --- | --- |
| `tools/coldfire/ghidradump.py` | Export an analysed Ghidra program into grep-friendly disassembly/decompilation plus SQLite indexes. |
| `tools/coldfire/ghidraq.py` | Run read-only PyGhidra queries for functions, callers, references, strings, ranges, decompilation, and bounded HighFunction dataflow. Queries can be chained with `--then` so one JVM answers several questions. |
| `tools/coldfire/refscan.py` | Exhaustively scan a raw ColdFire image for direct references into an address range. Use this to check Ghidra's incomplete reference tables. |
| `tools/coldfire/codeseeds.py` | Recover likely function entries from vectors, calls, and code pointers. |
| `tools/coldfire/ghidraapply.py` | Apply code seeds or RTTI discoveries to a Ghidra project. |
| `tools/coldfire/rttiscan.py` | Locate GCC RTTI, vtables, and class/method strings in a ColdFire image. |
| `tools/coldfire/ghidravt.py` | Run and export Ghidra Version Tracking matches between two programs. |
| `tools/coldfire/vtcheck.py` | Check Version Tracking matches against the images and analysis dumps. |
| `tools/coldfire/hwlookup.py` | Resolve an MCF5441x address, exception vector, or eDMA channel through the cited hardware-reference contract. |

`hwlookup.py` is backed by
`docs/contracts/mcf5441x-reference-v1.json`, not by guessed peripheral names.
For example, `hwlookup.py address 0xfc045654` reports TCD50.CITER,
`hwlookup.py dma 50` connects SSI0 TFE0 to vector 170, and
`hwlookup.py vector 191` reports the unassigned hardware source and its
`INTFRCH1` software-force bit. Register and routing facts retain manual-page
or independent-header provenance, and contract tests keep emulator constants
consistent with the lookup.
| `tools/coldfire/export_profile.py` / `tools/coldfire/apply_profile.py` | Export resolved firmware symbols and apply them as Ghidra labels. |
| `tools/coldfire/entryhist.py` | Summarise discovered function entries over known code ranges. |

An empty Ghidra caller or reference list is not proof of absence. Confirm raw
references with `tools/coldfire/refscan.py`, especially for trampolines and code outside
recognised functions.

Typical read-only query:

```sh
uv run python -m tools.coldfire.ghidraq /section_3_MAIN_OS.bin callers 0xADDRESS \
  --project ~/ghidra-projects/dt2-emac --project-name dt2-emac
```

### Raw instruction p-code and bounded HighFunction dataflow

`pcode PC` reports raw `Instruction.getPcode()` at exactly one listed instruction,
without decompiling, changing references, or inferring SSA. Its JSON includes
requested, logical, and displayed coordinates, instruction length, ordered p-code
operations, and address/language metadata. `no-instruction`, empty, and malformed
p-code are structured statuses. It is a diagnostic, not an SSA slice.

`slice PC SELECTOR` finds the containing function and follows a bounded backward slice;
selectors include `store:value`, `store:address`, `load:value`, `load:address`,
`input:N`, `output`, and `reg:NAME`. `stores TARGET [LO HI]` and its symmetric
`loads TARGET [LO HI]` inspect memory writes and reads globally or for function
entries in `[LO, HI)`, reporting exact direct addresses separately from computed
or unresolved candidates. Raw `STORE`/`LOAD` and HighFunction direct-memory
`COPY` forms are normalized with `STORE`, `LOAD`, `COPY_DIRECT_WRITE`, or
`COPY_DIRECT_READ` representations. A `COPY` with direct memory on both ends is
the decompiler's folded memory-to-memory move: it reports as
`COPY_MEM_TO_MEM_WRITE` for `stores` and `COPY_MEM_TO_MEM_READ` for `loads`, so
one instruction is visible from both its destination and its source. Both use
the normal decompile timeout,
report failures/partial results instead of stopping a `--then` chain, and do
not save the project.

For the ColdFire program, raw/displayed addresses are unprefixed:

```sh
uv run python -m tools.coldfire.ghidraq /section_3_MAIN_OS.bin pcode 0x40008000 \
  --json --project ~/ghidra-projects/dt2-emac --project-name dt2-emac
uv run python -m tools.coldfire.ghidraq /section_3_MAIN_OS.bin slice 0x40008000 store:value \
  --json --project ~/ghidra-projects/dt2-emac --project-name dt2-emac
```

For SHARC, replace `SHARC_PROGRAM` and `SHARC_PROJECT` with the program path
and project containing that program. The program must be imported with
`SHARC_VISA:LE:32:default`; use `sw:` for a short-word PC target. DM data
targets (on-chip and external alike) are byte addresses and go unprefixed --
the generated language translates a DM byte address into the `ram` space's
unit offset internally (`tools/sharc_dev/sharcspec/ghidra/gen_sleigh.py`
`dm_byte_addr_to_ram_unit`), so the plain literal from the instruction
stream is also the address to query here:

```sh
uv run python -m tools.coldfire.ghidraq SHARC_PROGRAM pcode sw:0x1c1928 \
  --json --project SHARC_PROJECT --project-name SHARC_PROJECT
uv run python -m tools.coldfire.ghidraq SHARC_PROGRAM loads 0x254d98 \
  --json --project SHARC_PROJECT --project-name SHARC_PROJECT
uv run python -m tools.coldfire.ghidraq SHARC_PROGRAM stores 0x8055c874 \
  --json --project SHARC_PROJECT --project-name SHARC_PROJECT
```

Results identify p-code by sequence PC/time and structured varnode fields, not
Java display strings. SHARC_VISA results include byte/short-word metadata and
the caveat that HighFunction p-code does not model delay-slot execution.

The Python Unicorn reference emulator and its tools (boot checks, snapshot tools, GUI runs, MMIO traces) were
removed; the last commit that has them is tagged `unicorn-reference-final`. `tools/emulator/snapread.py` still reads `.snap`
files without a machine.

## Native emulator, scenarios and performance tools

The native emulator (`packages/emulator/`, see [`packages/emulator/README.md`](../packages/emulator/README.md)) is built
and run through mise (`mise tasks` lists every task). These tools sit behind those tasks;
each has a module docstring with its full command line.

| Tool | Purpose |
| --- | --- |
| `tools/emulator/native_emu.py` | `mise run workbench` (`-- --build-only` builds only), `emu:serve`: build the desktop emulator once and run it (`--plus-drive DIR` or `--samples DIR`; default `plus_drive/<device>/`, `--dsp-patch FILE`, `--cf-only`, `--daemon`). |
| `tools/emulator/native_target.py` | Pick the cargo target dir for a generated core (`prod`, `nogen`, `generic`, `g-<hash>`). |
| `tools/emulator/native_pgo.py` | Profile-guided optimisation of the desktop build (`mise run perf:pgo`; `bench-bins`). |
| `tools/nx/wasm.mjs` | `mise run web:wasm`: build the browser cores. |
| `tools/nx/nx.mjs` | `pnpm nx:pilot`: the one Nx launcher. Computes the CPU budget, takes the shared benchlock for task runs. |
| `tools/nx/gate.mjs` | `mise run test`, `test:plan`, `test:changed`, `test:full`: target lists are `TIERS`. |
| `mise run dev:boot -- --syx ABS --out ABS_DIR --limit N` | The bounded native boot diagnostic. `mise run dev:plusdrive` runs the standalone +Drive CLI (`cargo run`, no wrapper script). |
| `tools/emulator/ready.py` | `mise run emu:ready -- dn2\|dt2`: build the ready snapshots from the `.syx` alone (cold DSP start). |
| `tools/emulator/coupled.py` | `mise run scenario`, `scenario:all`, `scenario:replay`, `scenario:bisect`: build and run `coupled-run`, bisect DSP tiers. |
| `tools/emulator/coupled_matrix.py` | Run scenarios across DSP configurations, repeated, under the benchmark lock. |
| `tools/control/digi_client.py` | `mise run emu:ctl`: drive a running emulator over its control socket (`status`, `inject`, `step`, `tap`, `frame`, `call`, `events`). |
| `tools/patchkit/dsp_patch.py` | DSP patch files: `check`, `show`, `apply` on a packed image, `syx` writes a patched `.syx`. Patches in `tools/patchkit/dsp_patches/`. |
| `tools/devtools/perf.py` | `mise run perf`: the standard DSP performance report (CPU per audio second, tier split, per-function cost ranking, `--calibrate`). |
| `tools/devtools/fastcov.py` | `mise run perf:cov`: DSP CPU, tier split and fast-tier refusals per tier configuration. `perf.py` is built on it. |
| `tools/sharc_dev/fast_ddmin.py` | Shrink the set of fast-tier regions that reproduces a DSP halt (delta debugging with `SHARC_FAST_ONLY`). |
| `tools/sharc_dev/aot_gen.py` | `mise run perf:aot`: generate and register a firmware block library (AOT core) by DSP image. |
| `tools/devtools/sharc_regionbench.py` | One-region microbenchmark for generated block code (about 50 s per variant). |
| `tools/devtools/benchlock.py` | Shared lock for builds and exclusive lock for timed runs (`status` shows holders). |
| `tools/devtools/upload_bench.py` | `uv run python -m tools.devtools.upload_bench [--device dn2\|dt2] -- --bytes N`: one free-running daemon, then `upload_bench.mjs` (digikit BEGIN/DATA/END and ranged VERIFY, DN2) or `upload_bench_rpc.mjs` (stock +Drive MidiRpc write, DT2): wall MB/s, guest instructions per byte (`work_insn_per_byte` less the firmware's own load), the real-time factor, controller CPU seconds. |
| `tools/nx/rust-env.mjs` | `node tools/nx/rust-env.mjs [--gen] [--variant NAME] -- CMD`: runs a cargo command with the pinned toolchain; `--gen` sets `SHARC_GEN_DIR` and `CARGO_TARGET_DIR` and checks the generated-core fingerprint. |
| `tools/emulator/digi_env.py` | Paths, firmware lookup, ready-input lookup, the generated-core registry and fast-region tables (`tools/emulator/fast_regions.json`). |
| `tools/devtools/wasm_parity.mjs` | `mise run web:parity NAME`: check the browser core renders a scenario's PCM byte-identical to native. |
| `tools/devtools/worktree-setup.sh` | Link `out/`, `.venv`, each `firmware/*.syx` and generated language files into an agent worktree. |

Generated fast-tier lowering (see finding 18 for the design):

| Tool | Purpose |
| --- | --- |
| `tools/sharc_dev/sharc_lowergen_regen.py` | `mise run gen:regen`: regenerate the shared core, `gen_forms.rs`, the fuzz vectors and the test programs. |
| `tools/sharc_dev/sharc_lowergen.py` | The form tracer. `sharc_lowergen_rs.py` is the Rust emitter, `sharc_lowergen_helpers.py` prints helper bodies. |
| `tools/sharc_dev/sharc_lowergen_survey.py` / `sharc_lowergen_leaf.py` | Show the traced leaves of all forms or of one form with pinned fields. |
| `tools/sharc_dev/sharc_lowergen_vectors.py` / `sharc_lowergen_programs.py` | Write the random-encoding fuzz vectors and the hand-assembled test programs. |
| `tools/sharc_dev/sharc_lowergen_bisect.sh` | PCM and tier split with a subset of the generated forms. |

`tools/devtools/archive/` holds one-off probes that nothing references any more (see its README).
Run output goes to `out/runs/<scenario-or-kind>/<stamp>/` ([INSTRUMENTATION.md](INSTRUMENTATION.md)).

## Machine and frame-link tools

| Tool | Purpose |
| --- | --- |
| `tools/patchkit/machineprofile.py` | Identify a known MAIN OS by hash and report its machine-related anchors and preconditions. |
| `tools/patchkit/machinepatch.py` / `tools/patchkit/machinebuild.py` | Plan and build the static MAIN OS patches for an eighth machine. |
| `tools/coldfire/isacheck.py` | Refuse any instruction a ColdFire V4e (ISA_A/B/C + EMAC) does not have, in a `.dko.json`, an ELF object or an archive; `--committed` scans every committed (core) module object and `--cached` the built objects in `out/cache/dko` (finding 35). It is a front end of `patchkit dko isa-check FILE...`: the check (the ISA allow-list) exists once, in `packages/patchkit/src/build/cf.rs`, and the module build runs it in process. |
| `tools/patchkit/pk_cdemo.py` | `proof`: boot with `test/c-demo` (64-bit divide, remainder, multiply, shifts through the ColdFire libgcc helpers) selected in a card's selection record and compare its results with Python (finding 35). |
| `tools/device/dksettings.py` | The settings record on the host (`python -m tools.device.dksettings FILE [--json]`): decode, encode, which copy is in use, the keys by owner printed as what they hold; the same rules as `dkrt_sdk::settings` and `patchkit store dump`, kept equal by `sdk/vectors/settings-vectors.json` (finding 63). |
| `patchkit build` (`packages/patchkit/src/build/`; `mise run module:build`) | Module sources to objects (`build/*.dko.json` of a `core/*` module, committed; `out/cache/dko/<module id>/<path>` for every other module, build output): ColdFire C, assembly and Rust crates in the binary (gcc, as, ld, objcopy, cargo as child processes; the ISA check through the `coldfire` crate), SHARC+ with the Selache assembler as a library (`build/sharc_obj.rs`: the encoding-bug guards, the fix-ups, and a re-decode of every instruction with the `sharc-rt` decoder; the same code is in `patchkit.wasm`, entry `pk_sharc_object`). `[MODULE...]` is a folder or a module id, none builds all. Skips an object whose `source_sha256` is current; `--check`, `--force`, `--define NAME --out DIR` (DSP build options), `--status` (JSON, builds nothing). Library entries `patchkit::build::build_module` and, for the module library, `patchkit::build::load_object` (`Library::scan_repo` builds a stale content object on demand; `patchkit pack MODULE` builds and packs in one step). Also `patchkit dko isa-check FILE...` (ColdFire ISA check of a `.dko.json`, ELF object or archive). Also `patchkit dko show FILE [--relocs]` (a readable summary of an object), `dko isa-digest FILE` `cf-link SRC.. --base HEX` (link C routines against firmware addresses for unit tests; `tools/patchkit/patchkit.py cf_link`) and `sharc text|asm|link|object|cond|inc|shift-check` (the SHARC+ build for the Python test rigs, which place an object themselves and run it on the core; `tools/patchkit/pk_sharc.py`). |
| `patchkit store dump` (`packages/patchkit/src/storedump.rs`) | Digikit's own data on a card image (the emulator's card files, a `card.overlay` laid over one with `--overlay`, or a raw dump with `--base-sector`): the store header and files, the selection record, the settings record with both copies and every entry decoded; `--json`; `--record FILE` for one settings record saved with `dkupload settings -o FILE` (docs/design/digikit-store.md, "Reading our data locally"). |
| `tools/device/dkupload.py` | Module upload over USB SysEx (`info`, `ls`, `put FILE [PATH]`, `rm`, `clear`, `settings [-o FILE]` reads the settings record; two requests in flight; the emulator through `midi.usb.in/out`, or a real device with python-rtmidi, never run against hardware by the lane that wrote it); the framing of `docs/design/digikit-upload.md` (finding 43). |
| `tools/device/dklog.py` | The runtime's debug log over USB SysEx (`follow`, `crash`, `peek`, `info`; `level` reads or sets the log levels, LOGCFG, runtime 0.1.0; `follow --level L --module NAME` filters). Event kinds come from `tools/device/dklog_kinds.py`, generated from `sdk/sdk.toml` by `mise run gen:modules`. |
| `tools/patchkit/pk_ctxprobe.py` | The SHARC context registers at the voice hook in the coupled emulator (what `core/dsp-voice` records for `dl_diag`: M5, M6, M7, M13, M14, M15, I6, I7, B7, L7, MODE1 and two heartbeats), also as the ColdFire's record copy shows them; `--cost` prints the DSP cycles per block for a paired A/B of two module versions (finding 45). |
| `tools/patchkit/pk_digikit_upload.py` | The `upload` kind of the digikit scenarios: ops, power cuts (the runtime's `DKRT_UPLOAD_CUT` knob), stock MidiRpc unchanged, an upload during playback, the +Drive round trip (finding 43). |
| `tools/patchkit/pk_digikit_prof.py` | Per-phase wall-time profile of a digikit boot scenario (exclusive phases, named steps, the emulator's emulated seconds per wall second); `DIGIKIT_PROFILE=1` on `pk_digikit_scen run` prints the same table (finding 39). |
| `tools/patchkit/pk_dkrt.py` | The digikit runtime in the emulator (`proof`): patch `core/runtime`, pack `test/dyn-demo` and two broken variants into a card store, boot with a selection record (three bad files, the good module, a duplicate), read the boot report, the counter, `E_BOOT_ONLY` after the boot, write-watch and flush check (finding 29, converted for boot-time loading). Also the shared helpers (`Tools`, `patch`, `pack`, `build_card`, `write_selection`, `run_script`). Runs `packages/emulator/boot/examples/cf_script.rs`. |
| `tools/patchkit/port_resolve.py` | Resolves the rules declared in `devices/fw/` into `devices/ports/` (`mise run patch:port`); `rule-sharc` prints a SHARC rule for an address ([design](design/firmware-decls.md)). |
| `tools/sharc_dev/framelink.py` | Describe the ColdFire-to-SHARC frame layout for known images. |
| `tools/sharc_dev/dspmap.py` | Find ColdFire instructions that reference frame-link tables. |

`machineprofile.py` is the guardrail for the patching tools: do not carry an
address from one firmware image into another without a matching profile and
byte preconditions.

## SHARC+ loader and memory map

The DSP firmware is a loader stream containing multiple writes to mapped
memory, not one flat executable. The address mapping and last-write order are
important.

| Tool | Purpose |
| --- | --- |
| `tools/sharc_dev/sharcldr.py` | Parse loader records, validate headers, map file offsets to loaded addresses, dump selected blocks, and extract the final main-program region. |
| `tools/sharc_dev/sharc_import.py` | Import the complete loader memory map into Ghidra, replaying writes in stream order. |
| `tools/sharc_dev/sharcscan.py` | Recover direct calls and candidate dispatch tables across the loader stream. |
| `tools/sharc_dev/sharcstrings.py` | Map bounded printable runs to their loader blocks and loaded addresses, with optional SQLite-reference correlation. This is an orientation aid, not semantic evidence. |

Examples:

```sh
uv run python -m tools.sharc_dev.sharcldr out/sections/dt2-1.16/section_7_BLOB.bin --align

uv run python -m tools.sharc_dev.sharcstrings \
  out/sections/dt2-1.16/section_7_BLOB.bin \
  --grep 'frame|buffer' --limit 100 --json
```

Mapped-string JSON is firmware-derived and belongs under `out/`, never in a
commit. Printable instruction bytes also look like strings; require a real
data reference before treating a hit as a label.

## SHARC+ decoding and control flow

| Tool | Purpose |
| --- | --- |
| `tools/sharc_dev/sharc_disasm.py` | Decode variable-width VISA instructions from the public-manual-derived tables. Unknown or ambiguous instructions stop rather than silently desynchronising. |
| `tools/sharc_dev/sharcflow.py` | Recover delayed calls and returns, measure coverage, and optionally seed/repair Ghidra functions. |
| `tools/sharc_dev/sharcimm.py` | Search decoded instruction fields, or every loaded block's words, for immediate values and peripheral addresses. |
| `tools/sharc_dev/sharcfields.py` | Report per-form decoded-field distributions across main programs. |
| `tools/sharc_dev/sharccompare.py` | Compare the generated decoder against the independent specification decoder. |
| `tools/sharc_dev/sharcpcode.py` | Build and measure the generated Ghidra language, record decoder/Ghidra views in SQLite, and compare two measurement runs. |
| `tools/sharc_dev/sharc_seeddecode.py` | Decode exact Ghidra instruction-start seeds from every loader-mapped region and report form, field, length, and confidence agreement without sweeping data blocks. |

Language changes should be measured before and after:

```sh
uv run python -m tools.sharc_dev.sharcpcode measure --out out/sharcpcode/new --ghidra
uv run python -m tools.sharc_dev.sharcpcode compare out/sharcpcode/old out/sharcpcode/new
```

Use `tools/sharc_dev/sharcpcode.sql` and the generated SQLite files for questions the
measurement already records; avoid starting another Ghidra JVM just to repeat
a query.

To inspect a non-main function through the loader map:

```sh
uv run python -m tools.sharc_dev.sharc_seeddecode \
  out/sections/dt2-1.16/section_7_BLOB.bin \
  out/sharcpcode/new/dt2-1.16.sqlite \
  --function 0xFUNCTION --only-problems
```

The SQLite instruction starts are boundary evidence; the tool never performs
a linear sweep over loader regions that may contain data.

## Targeted SHARC+ data flow

These tools are deliberately narrow. They are faster and safer than pretending
the incomplete SHARC+ language can decompile an entire receive path.

### `tools/sharc_dev/sharc_trace.py`

A bounded, delay-aware abstract interpreter starting at an exact short-word
PC. It currently models a proved subset of register moves, integer compute,
DAG address updates, direct Type 14a DM/PM transfers, other memory accesses,
and two independent delay slots. A normal Type 14a event marks that an
unmodelled SIMD companion access is possible; forced-long-word Type 14a stops
explicitly rather than pretending its neighboring register pair is one UREG.
Unsupported or provisional forms stop the state explicitly.

The default mode reads a flat extracted region and therefore requires
`--base-sw`. `--blob` instead decodes exact PCs directly from the complete
section-7 loader memory map, including non-main and cross-block code.

Seeds can be concrete or symbolic:

```sh
uv run python -m tools.sharc_dev.sharc_trace out/sharc/dt2-1.16-main.bin \
  --base-sw 0x1c1338 --start 0xSTART \
  --set I2=@receive_words --set R4=@track_index \
  --max-steps 100 --max-states 32 --json

uv run python -m tools.sharc_dev.sharc_trace \
  out/sections/dt2-1.16/section_7_BLOB.bin \
  --blob --start 0xSTART --max-steps 100 --json
```

Symbolic arithmetic is affine, so expressions such as
`receive_buffer + 0x94 + 2*track_index` survive copies, addition, subtraction,
and multiplication by a constant. Non-affine operations become `Unknown`.

The direct-transfer support is sufficient to recover the concrete DAI0/DAI1
MMR stores in the 1.15C and 1.16 setup blocks. For example:

```sh
uv run python -m tools.sharc_dev.sharc_trace \
  out/sections/dt2-1.16/section_7_BLOB.bin \
  --blob --start 0x1cb28b --max-steps 100 --json
```

This traces the primary PE through the DAI stores and then stops at the next
unsupported `9b_abs` form; it is not a general SHARC emulator.

### `tools/sharc_dev/sharc_run.py`

A concrete, single-path SHARC+ runner built on `sharc_trace.py`, replacing
the removed `87df16d^:tools/sharcemu.py` (a Ghidra `EmulatorHelper` p-code harness;
its arithmetic already came from `sharc_trace.py`, and this runner covers
the same "run this one routine forward with real inputs" need without a
Ghidra JVM). `sharc_trace.py` is symbolic and forks on an unresolved
conditional; `sharc_run.py` seeds every register with a real value and
treats a fork, or any state it tracks as stopped, as a hard stop.

```sh
uv run python -m tools.sharc_dev.sharc_run dt2-1.16 --start 0x1c4ecf --max-steps 20000
```

Library use goes through `Runner` directly, given an already-loaded
`tools.sharc_dev.sharcldr.LoadedMemory`:

```py
import sharc_run
runner = sharc_run.Runner(loaded_memory, start=0x1c4ecf)
result = runner.run(max_steps=20000)
print(result.halt.reason, result.instructions_per_second)
```

### `tools/sharc_dev/sharc_accesses.py`

A step's data accesses, read from the trace events the runner's forms record
(`load`, `store`, `load-pey`, `store-pey`, with space, address and width). It is
for tools that measure a run: cycle models, DM/PM bus checks, write watches.
Use it instead of wrapping `sharc_core.memory._dm_read` or `_dm_write`. The forms
modules import those names directly, so a wrapper on the memory module misses
most of their accesses.

```py
from tools.sharc_dev.sharc_accesses import enable, step_accesses
enable(runner.state)                  # a saved snapshot may have events off
start = len(runner.state.trace)
...                                   # one step
for a in step_accesses(runner.state.trace[start:]):
    a.kind, a.space, a.address, a.width   # "r"/"w", "DM"/"PM", byte address, bytes
del runner.state.trace[:-1]           # keep the trace bounded
```

Python runner only. The native engine has `SHARC_WWATCH`
(`docs/INSTRUMENTATION.md`).

### `tools/sharc_dev/sharc_worklist.py`

Measures how much of a SHARC+ region the current language gives real p-code.
It sweeps the aligned instruction stream (`tools/sharc_dev/sharcflow.py` `aligned()`),
lifts each instruction with the in-tree language (`tools/sharc_dev/sharcpcode.py`
`load_context`/`lift_one`), and tabulates, per form and overall, the share
that lifts to at least one p-code op. Type21a, and Type9a/9b_abs with
`b == 1`, count as covered because empty is their correct semantics.

```sh
uv run python -m tools.sharc_dev.sharc_worklist --image dt2-1.16 \
    --function 0x1c18a6:0x1c1f7d \
    --out-json out/sharc-semantics/worklist.json \
    --out-md out/sharc-semantics/worklist.md
```

`--function LO:HI` (short-word addresses, end exclusive) adds the same table
for one span. The output is a ranked worklist of forms still without
semantics.

### `tools/sharc_dev/sharc_candidates.py`

Ranks exact Type19a address-adjust hypotheses in a `sharcpcode` SQLite file.
It checks byte and word forms of an offset, finds the nearest source writer,
and classifies the adjusted index register as consumed, overwritten, or
blocked by uncertain/control flow.

```sh
uv run python -m tools.sharc_dev.sharc_candidates \
  out/sharcpcode/new/dt2-1.16.sqlite \
  --offset 0x94 --word-bytes 2 --window 64 --json
```

A candidate is a hypothesis, not a finding. Confirm its base-pointer
provenance and the consuming memory operation with the tracer and image bytes.

### `tools/sharc_dev/sharcwriters.py`

Which SHARC+ instructions can write a given DM byte address -- image-wide,
over every store form, not just the ones Ghidra's SLEIGH language can see.

Census: decodes every aligned instruction in the code blocks
(`tools/sharc_dev/sharcinv.py` `CODE_BLOCKS`) and finds every DM store from decoded
fields alone (no SLEIGH, no execution): forms 15a/15b/3a/3b/3c/3d/4a/4b/4d/
6a_mem/14a/14d store when merged `d==1`/`g==0`; 16a/16b always store, DM
when `g==0`; dual-memory 1a/1b store on the DM side when `dmd==1`.
(`sharcinv.py`'s own `MEM_FORMS` omits Type3c even though it has a real `d`
bit and stores exactly like its siblings -- this tool keeps its own table.)

Resolve: groups the census by owning function (`tools/sharc_dev/sharcfn/`
`load_context`/`build_inventory`) and runs `tools/sharc_dev/sharc_trace.py`'s tracer
once per function from its entry, with every I/M/B register seeded as its
own named symbol (so an address survives as `Affine(I6e - 12)` instead of
collapsing to `Unknown`) and `concrete_memory=True`. L registers are seeded
concrete zero rather than symbolic -- the one deliberate exception, because
the tracer's own Type19a_scaled MODIFY handler only takes its cheap linear
path for a `Const` zero L register; a merely-symbolic L instead poisons
every later use of that I register with `Unknown`, which was the single
largest source of lost resolution on a full run.

Classify: `HIT` / `EXCLUDED-CONST` / `EXCLUDED-STACK` / `STACK-RELATIVE` /
`LOADED-POINTER` (address depends on an unresolved memory load; the load's
own expression is recorded) / `ENTRY-RELATIVE` (depends on a
caller-supplied register; every register in the expression is listed,
including a mixed frame+modifier idiom like `DM(I7,M7)`) / `UNRESOLVED`.
Every census DM store lands in exactly one class; the class totals always
sum to the census total (checked at the end of every run).

```sh
uv run python -m tools.sharc_dev.sharcwriters 0x252658 --jobs 16 \
    --json out/sharcwriters/252658.json
```

`--stack LO:HI` overrides the default stack bounds (derived from the
loader's own layout: the untouched gap between two code blocks, DM
`0x26f000`-`0x2c0000` -- see the module docstring); `--stack none` reports
every stack-relative store as `STACK-RELATIVE` instead of trying to exclude
it. `--jobs N` runs one function's trace per worker process. The
interpreter-independent logic (census rules, width tables, the address
classifier) is pure and unit-tested in `tests/sharc/analysis/test_sharcwriters.py` with
synthetic fields and trace events -- no firmware required.

`out/sharcwriters/stack-invariant.md`/`.json` census every instruction in
the image that can write I6, I7, B6 or B7 (7764 instances, whole-image,
function-owned) and argue S = `[0x26f000, 0x2c0000)` (the same
`DEFAULT_STACK_LO`/`HI`) usually contains them -- **conditionally, not
proven closed**: the census traces I7 through a genuine multi-context
stack-switch shape (`blk69@0xb8853a` sw `0xb885f5`, `I7` repointed to a
runtime-populated context-struct address) that cannot be resolved
statically to either "still in S" or a specific other region. See the
proof's "Item 2" for the full writer-by-writer table and verdict. Every
`EXCLUDED-STACK` result now states this explicitly rather than hiding it:
an `'assumption'` string (`ENTRY_SEED_ASSUMPTION`) on every row, plus
`excluded_stack_depends_on_unproven_entry_assumption` (a count -- every
`EXCLUDED-STACK` row, since the assumption underlies `STACK_SYMBOLS`
itself) in the JSON's top level.

The proof's one classifier-relevant fix lives mostly in
`tools/sharc_dev/sharc_trace.py`: the Type19a_scaled circular-MODIFY handler
previously collapsed to `Unknown('scaled circular modify I%d')` whenever
the pre-modify value, B or L were not all concrete -- true on nearly every
occurrence, since I6/I7 are seeded as named symbols at each function's own
entry. PRM p.6-7 guarantees the wrapped result still lands in
`[B,B+L*scale)` regardless, the *same* window a value already bounded by
S (an entry-time symbol, or an earlier circular-MODIFY result) already
denotes -- so `_stack_bounded_symbol()` recognises that shape and the
handler mints a **fresh**, uniquely-named `circ_<reg>_<pc>` symbol for the
result. Fresh, not reused: an earlier version of this fix re-used the
input symbol unchanged, which asserted two different circular MODIFYs of
I7 (different sites, or the same site with different runtime state) were
*equal*, letting the Affine algebra cancel their difference to a spurious
0 and alias two different stack frames -- replaced before it reached any
other tool (grep the image for `scaled circular modify` if adding a new
consumer of this reason string). `tools/sharc_dev/sharcwriters.py`'s classifier was
extended to match: `is_circ_symbol()`/`combined_affine_range()` treat a
`circ_` term as ranging over S widened by `CIRC_WRAP_SLACK` (`L7*4 =
0x7f4`) on both sides -- not S itself -- and `via_circular_modify` marks
an `EXCLUDED-STACK` result that used this. Recovers 948 of the ~1045
stores the stack pointer's circular MODIFY was blocking for target
`0x252658` (`3441 -> 4389 EXCLUDED-STACK`, `8438 -> 7492 UNRESOLVED`,
`HIT`/`EXCLUDED-CONST`/`ENTRY-RELATIVE` counts unchanged); the remainder
needs an offset at or beyond one buffer length, or an unrecognised symbol
name, which the fix deliberately does not claim bounded (see the proof).

## Agent query CLI and the atlas

`tools/coldfire/fw.py` is the first stop for firmware questions (the `firmware-query` skill says the same). One process,
compact text, a 60-line cap with a "... N more" tail, ColdFire and SHARC from one image name
(`-i dt2-1.17`, or `FW_IMAGE`). It uses only the standard library; the call graph is cached in `out/cache/fw/`
(keyed by the database hash).

| Command | Answers |
| --- | --- |
| `fn X`, `decomp X`, `disasm X` | Summary (size, callers, callees, strings, a few decompiled lines), full C, listing. X is an address, a Ghidra name or a `cf_names` name. |
| `callers X`, `callees X`, `xref ADDR`, `path A B` | Call sites, callees, every reference to an address (data, code, vtable slots), shortest call path. |
| `str TEXT`, `class NAME` | Strings and the functions using them; an RTTI class with bases, derived classes and vtable. |
| `port X --from dt2-1.16 --to dt2-1.17` | The same function on another version (`cfmatch` matches in the database). |
| `search TERM` | FTS5 over function and class names, strings, decompiled C and the atlas. |
| `atlas TERM` | Machines, parameters, classes, UI strings, structs and manual pages for a manual term. |
| `sql "SELECT ..."` | Read-only SQL on the same database. |
| `batch FILE\|-` | One command per line, one `## command` result block each, one process. |

Full-text indexes are sidecars, `out/cfdb/<image>-emac.fts.sqlite` (`tools/coldfire/cfdb.py fts IMAGE`, 2 s, built on
first use): trigram tables for names and strings, a token index for decompiled C. SHARC `search` uses LIKE over
function names, labels and notes (there is no SHARC decompilation).

`tools/coldfire/atlas.py build IMAGE...` writes `out/atlas/<image>.sqlite` from the rules in the repository: the
machine/page descriptor table (found by its 44-byte stride, any version), the parameter table
(`paramtable.py`), RTTI classes and vtable slots, UI strings grouped by the view class that uses them, storage
structs, manual pages (`out/refs/`, Digitakt II 1.03A and Digitone II 1.12 text via `tools/devtools/refstext.py`) and a
glossary of manual terms (`TERMS` in the file). Every row has `provenance` and `confidence`. The data is derived
from firmware and stays uncommitted. Add a term by adding a `TERMS` entry; rebuild takes about 12 s per image.

Measured (Apple M-series): `fw.py fn` 0.05 s, `path` 0.10 s with a cold graph cache and 0.06 s warm, a
12-query `batch` 0.27 s.

## Reference manuals

`tools/devtools/refstext.py` extracts supplied public manuals into searchable,
page-addressable text under `out/refs/`; `--render PDF PAGE` creates a PNG for
figures. Read the extracted text rather than repeatedly processing PDFs.

Only public SHARC+ references belong in documentation and generated language
sources.

## Choosing the fastest tool

| Question | Start with |
| --- | --- |
| Where is feature X handled on firmware Y; what does this function do? | `fw.py atlas X`, `fw.py search`, `fw.py fn` |
| What does this loader offset become in DSP memory? | `sharcldr.py` |
| Where is a known constant or peripheral address used? | `sharcimm.py`, then SQLite |
| Which exact offset calculations are plausible readers? | `sharc_candidates.py` |
| Which instructions can write a given DM address, image-wide? | `sharcwriters.py` |
| Which non-main instructions disagree with the decoder? | `sharc_seeddecode.py --only-problems` |
| Does a pointer remain `base + stride*index + offset`? | `sharc_trace.py` |
| Did Ghidra miss a ColdFire reference? | `refscan.py` |
| Did a language change regress decoding or analysis? | `sharcpcode.py compare` |
| Can a diagnostic string identify a subsystem? | `sharcstrings.py`, then require a real reference |
| What does a snapshot hold at an address? | `snapread.py` |

Record verified results in [`FINDINGS.md`](FINDINGS.md), not in this tool
guide. Keep current state and next steps in the newest handover.
