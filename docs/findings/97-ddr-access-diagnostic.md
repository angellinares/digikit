# Bounded DDR access diagnostic

2026-10-30. **[D]** implementation and focused host tests; **[O]** compiled-backend,
Wasm and stock coexistence validation. See the [sampler plan](../plans/dsp-sdk-and-sampler.md).

## Contract

`Mem::set_ddr_monitor` and `Engine::set_ddr_monitor` accept an optional half-open
byte interval within `[0x80000000,0xa0000000)`. The diagnostic records the first
known-address, supported-width overlapping attempt, with address, width and kind.
It leaves ordinary access results unchanged and does not block accesses. It is
not a production reservation, capacity guarantee, hardware watchpoint or security
boundary. No CLI, daemon or public module ABI is exposed in this first step.

CPU read/write attempts are distinguished from storage events. CPU hooks run
before uninitialized-value synthesis, model rejection and journal old-value reads,
including known-address writes whose value is symbolic. Unknown addresses and
unsupported widths are outside this contract. Storage events include host/DMA
memory operations, import/export and journal/rollback traffic; they are not guest
attribution. Duplicate helper calls cannot replace the first latched event. There
is no exact access-count or exact instruction-PC claim.

Setting, replacing or removing a valid range clears the latch and invalidates
resolved windows. Invalid ranges leave configuration and the latch untouched.
Memory reset preserves the configured range but clears the latch; later import
writes may produce storage events. Arm after setup/import and inspect the latch
before exporting memory when the intended experiment concerns guest execution.

## Fast-path and idle coverage mechanism

Watched DDR pages retain their contents but withhold their exported fast pointer.
The policy is reapplied when pages are allocated or refreshed. Cached static and
dynamic windows are invalidated using `layout_gen`. The shared flat-access IR
checks null pointers before native or Wasm raw loads/stores; a declined window
is not itself recorded as an access. Normal execution can then fall back to the
observed memory/semantic boundary. Monitor transitions invalidate idle-loop
snapshots/probes, and idle skipping is disabled while the monitor is active.

Implementation owners are `packages/emulator/sharc-rt/src/mem.rs`, its handwritten
`rt/bnd.rs` semantic boundary, and `packages/emulator/sharc/src/lib.rs`. Tests cover
memory policy, semantic attempts, idle lifecycle, window invalidation and common-IR
fallback. No Python instruction semantics, generated core, firmware bytes or
module/runtime objects changed.

## Validation and open boundaries

Eight focused monitor tests pass; all 21 runtime-crate unit tests pass (counts
overlap). Independent source review found no issues. The common-IR test executes
the reference interpreter and explicitly continues through the semantic boundary;
it does not yet prove automatic full-engine dispatch through the compiled backend.
Same-value stores, read-only accesses, overlap boundaries, invalid ranges, first
latch retention, reset/removal, synthesized reads and rejected symbolic-value
writes are covered within those tests.

**[O]** Reusing the existing native flat-access test harness reproduced the
pre-existing Cranelift block-builder panic documented in the October 27 handover.
It aborts through the backend C ABI. The diagnostic leaves that compiler unchanged;
actual compiled-backend and Wasm execution remain required before claiming their
coverage. Root inspection identifies `Emit::fail_block` switching away from the
current block before its guard branch is emitted as a candidate cause, pending
reproduction and a separately scoped fix. A reference-interpreter pass is not a
substitute for that backend proof.

Stock CPU/DMA coexistence runs, complete integration, and physical memory/cache
behavior remain open. Even after backend validation, a quiet bounded run cannot
establish universal firmware ownership of a candidate sample pool.


Focused validation used `CARGO_TARGET_DIR=out/native/target/ddr-monitor`,
`CARGO_BUILD_JOBS=2`, the shared repository benchlock and no generated-core
variables. Commands under `tools.devtools.benchlock run --shared`:

```sh
cargo test -p sharc-rt -p sharc-native --lib ddr_monitor
cargo test -p sharc-rt --lib
```

Both returned zero. Output was retained in tool sessions only; no standalone run
log was saved. Reusing this target avoids an unnecessary fresh build for follow-up.


Formatting: owner-wide `cargo fmt --check` reports existing unformatted generated
and handwritten sources. A scoped check of the six changed files identified three
new formatting hunks, which were corrected; unrelated formatting was retained.
This is not an owner-wide formatting pass.


## Native compiled-backend follow-up

**[D]** The pre-existing panic is now reproduced and fixed in a separate compiler
change. `Emit::fail_block` previously switched away from a still-open predecessor
before Guard emitted its branch. Failure-block allocation is now separate from
body emission: Guard, GuardAny and Leave terminate their predecessor first, then
fill a fresh failure block. Cached blocks still use the same instruction/exit key,
and temporary boolean-conversion cache cleanup is preserved.

The original flat-load/store tests and the monitor test now execute both reference
IR and actual Cranelift kernels. Three flat-access tests, two new guard/exit
regressions, and eight monitor tests pass. Independent source review found no
issues. The new regressions cover shared and distinct exits, continuation boolean
reuse, GuardAny/Leave, noncanonical truth values and float-bitcast fixups.

Exact commands, environment and result codes are in
`out/runs/ddr-monitor-native/commands.json`; the pre-fix stack trace is
`out/runs/ddr-monitor-native/reproduce.log`. Passing logs are `flat-access.log`,
`guard-regressions.log`, `monitor.log` and `changed-preflight.log` in that directory.
No Python core regeneration was required. This closes the compiled-kernel refusal
proof, not full daemon dispatch, stock coexistence or Wasm execution. Those remain
open, along with hardware ownership and coherence.


## Ordered daemon control route

**[D]** The host-only `dsp.ddr_monitor` RPC accepts `arm` with byte `start` and
exclusive `end`, `status` (default), `clear`, and `disarm`. A typed command crosses
the active DSP worker FIFO with a per-request acknowledgment after audio sync.
Both generated-core bridges implement the same contract; unsupported engines
return an error. Invalid ranges and actions are rejected before mutation.

Responses contain the active generated-core profile label, instruction count,
armed range and first observation. The profile label is not a firmware-version
claim. Status reads only metadata. Access kinds describe helper paths: a host
peek through the CPU semantic accessor can report `cpu_read`, so that label alone
does not establish guest origin.

Four dual-core focused unit cases and a separate no-audio controller case pass;
independent source review found no issues. The scoped regression TypeScript check,
related host selection, lint and format passed directly with installed tools.
The usual changed-files wrapper passes Python/path/layer checks but its JavaScript
steps fail at the existing worktree pnpm hoist/bootstrap layout. This is not a
full workspace type or merge-gate pass. Commands and logs are retained under
`out/runs/ddr-monitor-rpc/`.

The new `tests/regression/runtime/dsp-ddr-monitor.test.ts` requires an explicit
immutable daemon path and registry-ready DT2 1.17 / DN2 1.12 inputs. It checks
lifecycle ordering, unchanged peek data, invalid-command preservation, then a
separate bounded stock-execution observation. Its arbitrary upper-DDR interval
is a diagnostic probe, not a selected allocation. Both native cases pass with immutable daemon `digi-emud-6850e27b98d9`
(SHA-256 `6850e27b98d9123fa77d470516da6cb729082236b9dc0968f0e100948763cbd3`).
Each stock observation advances 3,333,335 DSP instructions and records no hit in
`[0x9fff0000,0x9fff1000)`. Lifecycle, explicit helper-read capture and no-fault
checks pass. Input hashes and load identities are in the per-device acceptance
results; legacy generated profile labels do not override those identities.
These are short ready-state observations, not memory availability evidence.
Commands/logs are in the `native-build` and `native-acceptance` subdirectories of
`out/runs/ddr-monitor-rpc/`, with per-device records under `acceptance`.
Wasm execution remains open; native acceptance does not establish the browser path.


## Focused Wasm backend proof

**[D]** A test-only pointer-width override exports actual `Lower::flat_access`
kernels with a simulated wasm32 page-pointer layout. Production target selection
is unchanged. The fixture checks fixed Page offsets/size and rejects engine-state,
helper and 64-bit pointer-table load dependencies. The existing `wasmgen` tool
emits six kernels: reads and writes of one, two and four bytes.

`packages/emulator/sharc-fast-wasm/flat_access_check.mjs` executes them under
Node 24/V8. All 49 cases pass, including normal, null and restored pointers,
same-value stores, page bounds, presence/dirty bitmap boundaries and code-watch
refusal. Each case compares the entire 1 MiB memory against expected edits,
including an unrelated audio buffer. Null-pointer tests deliberately supply valid
low-address bitmap storage so missing null guards cannot be masked by a later
bitmap refusal. Four native flat-access tests also pass.

Independent source/artifact review found no issues. Exact commands, source/IR/Wasm
hashes, Node/V8 versions and results are under `out/runs/ddr-monitor-wasm/`.
Direct lint/format/related checks and path/layer checks pass; the known pnpm
bootstrap failure still prevents claiming the normal full preflight wrapper.

This proves safe backend exits for real lowering with a simulated target layout.
It is not a target-native Wasm core build, full browser-engine semantic fallback,
monitor lifecycle/RPC integration or hardware proof. Those remain separate gaps.
