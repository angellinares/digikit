# Test and build workflow

Objective: fast, accurate Nx selection with explicit build and cache boundaries, retaining coverage. Nx owns scheduling; mise remains a thin entry point.

## Sequence and checkpoints

0. **Done: upgrade Nx first.** `80135ab5` pins Nx/devkit 24.0.3. All 120 project definitions and
   edges match Nx 22; representative task selection matches. Frozen install, 15 preflight tasks,
   nine Nx audit unit tests and formatting passed. Native `summary` output works; `--graph` plans
   tasks and `--explain` reports task-selection reasons without execution.
1. **Done: prove selection and adopt the task-level default.** Completed sub-checkpoint: narrowed the
   global desktop UI host-test input, preserved actual web imports, registered standalone runtime
   unit tests, and added real Nx selection regression cases. These check runtime source/dependency
   changes, UI edits and leaf-module conformance under both affected modes. The first unit run exposed
   stale host-test compilation/link fixtures (page parameter wrappers, removed step allocations,
   optional firmware symbols); repaired test-only setup without changing production behavior.
   Host input audits also exposed a stale plusdrive digest path: aligned it with the actual
   `dist/plusdrive/plusdrive` binary read by regression helpers, retaining its content digest.
   Completed evidence: ten real selection cases pass across both modes, including
   SDK/fit edits selecting runtime unit tests. The runtime target passes all 32 host unit tests
   without daemon prerequisites. All five cached host targets pass input audits with successful
   tests; 16/16 affected preflight tasks and formatting pass. No device runs or full gate were run.
   Legacy UI selection still includes web's host suite through web's other UI inputs; task-level
   selection selects only desktop. The accepted matrix now enables Nx 24 task-level selection by
   default. The migration checkpoint's 19 tests compared both modes; the permanent suite now tests
   only the real default with its inherited environment override cleared.
   **Cycle split complete: `6b23946c`.** Nested `modules/core/runtime/tests` project
   `core/runtime-tests` owns acceptance metadata and depends on runtime plus its fixtures. The
   plugin avoids parent-to-child edges for tagged test-only projects, infers their targets and
   includes their metadata when discovering packs. Module-check resolves the nested test-only owner
   directly; the layercheck exception follows the metadata owner.
   Graph checks find no runtime/content cycle; old/new commands, file inventories and all pack
   targets agree (16 device files, 37 pack/alias targets, identical dependency-project closure).
   Saved PLUCK plans select 20 device/conformance tasks before and two after: its own conformance
   plus runtime acceptance. Total tasks including prerequisites fall from 58 to 29; this is
   selection evidence, not a measured speedup. At the split checkpoint, 17 real selection tests passed across both modes, including test-only edits,
   runtime source/units/acceptance, leaf conformance isolation and helper-source triggers.
   Twenty layercheck unit tests, direct layercheck, Python lint/mypy, formatting and seven affected
   preflight tasks pass. No device/native builds or full gate were run for this checkpoint.
   The relocated acceptance target remains uncached. No device suite was run. Earlier custom-audit
   passes did not prove subprocess closure or runtime-digest provenance; helper-source inputs are triggers only.
2. **Active: prove build-cache boundaries.** Checkpoint A (`daf7f3a0`) deletes the custom Nx input auditor,
   tracer and dedicated tests (four preflight checks pass): no operational callers beyond manual policy/docs/tests.
   Use native Nx declared-input inspection plus scoped invalidation/restoration evidence; no replacement tracer.
   Checkpoint B (`10995853`) adds explicit production inputs for the 16-core-module requires closure of profile `emulator`/`sample-tracks` sets.
   Native inspection covers menu/UI/transitive sources and committed objects, excluding consumer acceptance files; 19 selection cases and four preflight tasks pass.
   Nonhermetic Rust tests include dependencies and patchkit's external compile inputs; device digest source changes select consumers.
   Checkpoint C bounds apply's content builds to the selected requires closure, using existing library discovery/loading; check retains all-library semantics.
   Three focused Rust tests pass: selected/dependency objects build, unrelated missing sources stay unloaded, and missing/cyclic requires refuse before builds; five affected preflight tasks and doc formatting pass.
   Next: bounded real core-freshness validation, toolchain/daemon overrides and `PATCHKIT_MODULES` / `DIGI_PK_ROOT`.
   Follow up: manifest-driven dependency/type/category contracts; remove redundant ID branches after checking semantics, and split modules.mjs inference/build/test/dependency responsibilities only where useful; no generic framework.
   Core freshness checkpoint: uncached category-core source-backed producers check scoped generation, then incrementally build tracked objects under shared benchlock; image consumers use the profile-derived acceptance-image tag. No actual core build or generated-output update run here.
   Evidence: 23 scoped Nx host tests and six affected preflight tasks pass; native graphs select exactly 16 producers for image consumers and none for stock/runtime units. Declared Rust source/output checks pass; no end-to-end freshness measurement claimed.
   Conservative producer inputs remain; narrow after test ownership is clearer. Repeated owner checks repeat shared SDK/fw generation checks; optimize later. The mixed stock/runtime regression target prepares images; stage3 can split it.
   Early win `16594f8b` excludes task definitions from the daemon key while hashing the Rust pin
   and actual compiler identity. Six focused tests, mypy, Python lint and eight preflight tasks passed; 88 existing depfiles contain no mise input.
   The new scheme causes one initial daemon-store miss. Next audit module-pack and daemon
   outputs, runtime patched-image preparation during test setup, dependency hashes,
   firmware/ready/card identity, and cache restore behavior. Map setup reads before replacing
   source-input hashing with artifact hashing. Upcoming verification must cover affected selection
   as well as hash invalidation for Rust/Node/toolchain pin changes and ignored firmware/generated
   input changes: runtime hashes alone do not cause Git affected selection. Any new
   cached device target requires input/output review, scoped cache-behavior and coverage-parity evidence before enabling cache.
3. **Next: separate execution boundaries.** Provide independent unit, component, ColdFire-only,
   coupled and device targets. Establish SDK contract fixtures and retain each module's own
   conformance. Runtime edits may skip consumer conformance only when contract evidence proves
   that boundary; folder names alone do not establish independence.
4. **Next: share session infrastructure.** Consolidate TypeScript fixtures, reset and event
   helpers; use bounded concurrency after session isolation is proven. ColdFire waits cannot
   depend on DSP-frame progress. Define a fresh card baseline, epoch/protocol checks and a rule
   to discard dirty daemons before reuse.
5. **Optional, last: move directories.** Consider runtime outside content after dependency and
   execution boundaries work. Create a raw folder only for an actual contract or module.

Command/doc cleanup is implemented: `mise run test` is default affected verification,
`test:plan` invokes the same task with native Nx `--explain`, and `test:full` is the full command.
Per the user's deletion decision, the proposed `gate`/`gate:full` and runtime forwarding aliases
are removed, active callers migrated, and alias-only/legacy-duplicate tests deleted. The repository
guard applies the same full-suite restriction to `test:full`; personal hook/policy files are untouched.
Existing main/full command bodies are unchanged; no second router or target list was added.
Cheap preflight remains `test:changed`. `DIGI_GATE_BASE` coordinates Nx and the Python selector.
Planning checks execute no tests/builds; default output remains native Nx `summary` for agents.
Earlier checkpoint evidence: 19 selection tests, identical main/wrapper plans, formatting and 16/16
cheap preflight tasks passed. The default and full command bodies match their previous versions.
Deletion checkpoint: nine default-mode selection tests, 14 module-check ownership tests and 38
guard tests pass, plus Python lint/mypy, formatting and 16/16 cheap preflight tasks. Canonical
planning works; all 59 executable device/conformance and pack targets retain their configurations.

## Acceptance and remaining decisions

- Keep a real-file selection matrix with task IDs, dependencies and reasons; check positive
  coverage and unwanted selection. Leaf changes retain their own conformance.
- Audit narrowed inputs against meaningful tests. Cached outputs must restore correctly and
  invalidate for source, dependency, toolchain and device-identity changes.
- Decide the minimum SDK contract fixtures before reducing cross-module conformance.
- Decide the fresh-card/reset/epoch contract and ColdFire progress signal before pooling sessions.
- Preserve coverage on DT2 OS 1.17 and DN2 OS 1.12. Delete scenarios only after both-device parity.
- Iterate with scoped owner checks, never full suites. Run the merge gate before merging unless
  the user explicitly approves an exception. Record checkpoint commits and actual checks here.

## Limits of current evidence

No measured speedup or full gate is claimed. Task-level affected selection is the default after
matrix review; the repository workflow no longer maintains legacy-mode compatibility branches.
No device coverage has been removed, and no new device-test cache boundary has been established by
these host-only checks.
