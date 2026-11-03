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
   Next: remaining core-freshness validation, toolchain/daemon overrides and `PATCHKIT_MODULES` / `DIGI_PK_ROOT`.
   Follow up: manifest-driven dependency/type/category contracts; redundant debug pack seed removed after graph equivalence. Further modules.mjs splitting is intentionally deferred until a specific responsibility/schema contract benefits; no generic framework.
   Core freshness checkpoint: uncached category-core source-backed producers check scoped generation, then incrementally build tracked objects under shared benchlock; image consumers use the profile-derived acceptance-image tag. No actual core build or generated-output update run here.
   Evidence: 23 scoped Nx host tests and six affected preflight tasks pass; native graphs select exactly 16 producers for image consumers and none for stock/runtime units. Declared Rust source/output checks pass; no end-to-end freshness measurement claimed.
   Subsequent real runtime preparation: exactly CLI, expansion and runtime tasks pass in 22.3 s, including generated-source checks. Only runtime source_sha256 changed; object payload/symbols/relocations/toolchain metadata are identical. Runtime build status now reports unchanged; no device run.
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
3. **Active pilot: separate execution boundaries.** Provide independent unit, component, ColdFire-only,
   coupled and device targets. Establish SDK contract fixtures and retain each module's own
   conformance. Runtime edits may skip consumer conformance only when contract evidence proves
   that boundary; folder names alone do not establish independence.
   CF-only pilot: existing session helpers accept an optional DSP mode; service-command acceptance alone uses zero mode, without an Nx target split. Both DT2 1.17/DN2 1.12 pass original RPC/readiness assertions and report non-SHARC status; a first attempt exposed only a new status-label assertion mismatch (none vs zero), corrected from controller semantics.
   Successful warm named-file run: 15.07 s total, cases 6.65/6.59 s, including about 5.04 s install boot waits each. Observation only, no paired speedup claim. The 31-test host lifecycle file, regression typecheck and five affected preflight tasks pass; no prerequisite builds were needed.
   Ownership split: move that unchanged file/record into nested core/runtime-service-tests, exclude descendant-owned files generically, retain native test-device scheduling. Module-check discovers all valid owners and schedules them serially; main/full target lists stay unchanged. Both owners remain uncached.
   Evidence: 26 Nx host tests, 15 module-check tests, Python lint/mypy, tracked layercheck, canonical planning and six affected preflight tasks pass. Service-file/svc-echo edits select only CF; runtime edits select both. All 16 runtime file contents and 37 pack/alias definitions match before; no device rerun for the unchanged file move.
   Original settings zero-DSP pilot: DT2/DN2 reached ready at epoch2 but the first delayed card write was absent. The patched loader dropped requested zero-mode SSI timing; settings debounce itself remains unchanged. Preserve the existing 96 kHz SSI zero peer across patched cold/cache/fallback/reset construction, with a separate zero snapshot key. One focused DT2 native test executed cold/cache/fallback/controller-RPC restart in 21.13 s, proving non-SHARC status and advancing block clocks.
   After rebuilding only DT2/DN2 daemons, both devices passed the delayed-write/copy assertions, then hit CardRig.restart's dk.waitForBoot deadline. The controller can answer during resets, so protocol silence is not a reliable emulator boot observer. CardRig now keeps its existing fresh-epoch/CF-ready/UI-hook predicate as the single wait, with a 90 s restart budget (other settles stay 45 s); real-transport waitForBoot is unchanged.
   Settings now runs zero mode: both named main cases pass (61.52 s file), then all 16 original cases pass on DT2/DN2 (239.62 s). One DN2 coupled main case also passes (106.58 s), with 37 lifecycle host tests, regression typecheck and five affected preflight tasks passing. These are observations, not paired A/B speedup claims; no assertions or reset guards removed. Logs: /tmp/digikit-zero-settings-epoch-full.log and /tmp/digikit-coupled-settings-epoch-main.log.
   Next highest-payoff candidate: batch conformance daemon boots with a proven pristine-card/reset contract before reducing runtime-to-module coverage edges. SDK contract fixtures are additive; loader compatibility changes remain deferred by the user.
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

Versioning checkpoint: Nx Release separates independent shipped non-core modules from one runtime/SDK
release; sdk/sdk.toml is canonical and core/runtime's module version mirrors it (baseline aligned to 0.5.4).
The formatting-preserving TOML adapter rejects unsupported packed versions/mirror drift; dependent bumps,
git operations and changelogs are disabled. Use only version subcommands; no publishing. Future public
runtime range >=0.5.4 <0.6.0 is a policy plan only; loading/contracts/binary behavior remain unchanged.
Evidence: 39 Nx host checks, tracked layercheck and 16 affected preflight tasks pass, including formatting/mirror/packed-version refusal and native filtered
dry runs with unchanged source/index/tag hashes. Module patch proposes only its manifest; runtime minor
proposes only canonical SDK and mirror versions (0.5.4 to 0.6.0); no builds or publish/tag commands ran.

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
