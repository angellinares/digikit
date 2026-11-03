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
   Pack checkpoint: removed the explicit module build before per-device pack. `pack` calls
   `library_with` with only its module ID; `Library::read_root` calls `build::load_object`, which
   shares `source_hash` / `build_object` with explicit build. ColdFire hashing includes compiler-discovered
   header dependencies. Neither path builds the manifest's `requires` modules; imported symbols remain
   runtime imports. Existing on-demand-cache and header-invalidation tests cover the shared behavior.
   Named `digikit/remote:pack` proof on DT2/DN2: warm artifacts match before/after, and a reversible
   `E_MSG` source edit (-50 to -51) produced changed bytes with identical old/new DKM and complete
   cached DKO metadata. Restoring the source reproduced the original bytes and DKO; warm packs left
   the object mtime unchanged. Both devices' baseline SHA-256:
   `8678c47cbf7abf91c6b55f1f03fea39f367655d5e7bda6fb212d8bdea1b1161c`;
   stale-source SHA-256: `94470a4c75027c3b62afc40bc334f696907377016cc60d2016079878fdfe0b32`.
   Evidence: `/tmp/digikit-pack-proof/`. No cache setting, output or task dependency changed;
   no device/emulator run or performance claim. The inferred remote project is identical apart from
   the removed command. The focused module-builds host test, four affected preflight tasks
   (`DIGI_GATE_BASE=HEAD mise run test:changed`), Prettier and `git diff --check` pass. No new
   exact-command regression was added; the existing cache tests exercise the behavior.
   Pack-cache audit decision: retain inferred `pack`'s `cache: false`. Its DT2/DN2 DKM outputs
   are explicit, but complete input closure is unproven: packing scans all module manifests and
   reads selected sources/includes, SDK definitions, device models and CLI/toolchain inputs.
   `PATCHKIT_MODULES` permits unmodeled external roots, and ignored `out/cache/dko` is mutable
   intermediate state. Nx 24 supports environment/runtime inputs, prerequisite-output inputs
   (`dependentTasksOutputFiles`) and `includeIgnored`; these mechanisms alone do not prove closure.
   Do not enable caching or add a broad all-modules input as a substitute. Revisit after constraining
   external roots and closing selected-object plus CLI/toolchain inputs; verify affected selection
   separately, then prove scoped invalidation and deletion/restoration of declared outputs on
   representative ColdFire and DSP modules. No cache behavior or speedup is claimed by this audit.
   Doc-only audit checks: Prettier and `git diff --check` pass;
   `DIGI_GATE_BASE=HEAD mise run test:changed` succeeds with zero selected tasks.
   Follow up: manifest-driven dependency/type/category contracts; redundant debug pack seed removed after graph equivalence. Further modules.mjs splitting is intentionally deferred until a specific responsibility/schema contract benefits; no generic framework.
   Core freshness checkpoint: uncached category-core source-backed producers check scoped generation, then incrementally build tracked objects under shared benchlock; image consumers use the profile-derived acceptance-image tag. No actual core build or generated-output update run here.
   Evidence: 23 scoped Nx host tests and six affected preflight tasks pass; native graphs select exactly 16 producers for image consumers and none for stock/runtime units. Declared Rust source/output checks pass; no end-to-end freshness measurement claimed.
   Subsequent real runtime preparation: exactly CLI, expansion and runtime tasks pass in 22.3 s, including generated-source checks. Only runtime source_sha256 changed; object payload/symbols/relocations/toolchain metadata are identical. Runtime build status now reports unchanged; no device run.
   Conservative producer inputs remain; narrow after test ownership is clearer. The mixed stock/runtime regression target prepares images; stage3 can split it.
   Shared generation checkpoint: `gen --check --scope shared` checks SDK and firmware outputs without
   scanning module libraries; `--scope modules` retains each producer's module-output checks.
   Scopes require `--check`; unscoped generation/checks and `--modules` keep their existing behavior.
   The uncached `sdk:check-generated` target depends on the CLI and is a prerequisite of every
   core producer, so Nx schedules shared checks once per task graph. No cache boundary or
   generated output changed. Static baseline: each of 16 image producers previously ran both
   shared generators. The real Nx matrix still selects precisely those 16 producers and now
   selects one shared check for every image consumer, with none added to stock/runtime unit plans.
   Evidence: 28 scoped Nx host tests, 33 focused Rust CLI/SDK/firmware tests and eight affected
   preflight tasks pass. The Rust fixture test proves separate stale SDK, firmware and module
   outputs fail their check scope and unscoped/default checks, while the other scope succeeds;
   byte/mtime snapshots remain
   unchanged by successful and failing checks. Fixtures copy repository inputs into a
   temporary root and never edit repository-generated files. The real shared Nx target passes
   (CLI plus shared check), as does the actual runtime module-only check. Prettier and
   `git diff --check` pass. No core object build, device run, full gate or speedup claim.
   Early win `16594f8b` excludes task definitions from the daemon key while hashing the Rust pin
   and actual compiler identity. Six focused tests, mypy, Python lint and eight preflight tasks passed; 88 existing depfiles contain no mise input.
   The new scheme causes one initial daemon-store miss. Next audit module-pack and daemon
   outputs, runtime patched-image preparation during test setup, dependency hashes,
   firmware/ready/card identity, and cache restore behavior. Map setup reads before replacing
   source-input hashing with artifact hashing. Upcoming verification must cover affected selection
   as well as hash invalidation for Rust/Node/toolchain pin changes and ignored firmware/generated
   input changes: runtime hashes alone do not cause Git affected selection. Any new
   cached device target requires input/output review, scoped cache-behavior and coverage-parity evidence before enabling cache.
   Override-input checkpoint (2026-11-04): the device runtime digest now includes the path,
   contents and file permissions of general/device/proof daemon overrides, `DIGI_PATCHKIT`,
   `DIGI_PATCHKIT_BIN` and `DIGI_PLUSDRIVE`. Binary symlinks are followed; missing/nonregular
   paths are recorded without opening FIFOs/devices. Nonempty `PATCHKIT_MODULES` / `DIGI_PK_ROOT`
   and explicitly set `DIGI_RUNTIME_MODULES` deliberately miss the cache through a fresh nonce.
   Arbitrary source/include roots and variant `out/tools` / `out/cache/dko` have no proven bounded
   read closure; no override-tree scan or complete variant-root closure is claimed. Specialized
   catalogue/files/LP0 card, patched-image, plan and provenance overrides also prevent reuse;
   LP0 case-selection flags are hashed. Default mode remains deterministic. Override-mode runs
   can create local/remote entries that are never reused. External/ignored-only edits and
   environment changes still require direct owner-target invocation: runtime hashes do not
   trigger Git affected selection.
   HERMETIC `rust-tests` now includes a runtime input over sorted present
   `CARGO_PROFILE_RELEASE_*` names/values, including build-override options, and its recipe source.
   Evidence: 31 focused runtime-input tests pass (same-path binary replacement, symlink target
   replacement/removal, unrelated-file stability, override nonreuse and profile normalization).
   A real device-digest probe confirms deterministic default input, same-path binary byte
   invalidation, unchanged digest after an unrelated sibling edit, byte-restoration equivalence
   and missing-root override nonreuse. Native Nx `inputs --json` / `--check` confirm the runtime
   digests on `digikit/remote:test-conformance`, `core/runtime-tests:test-device` and
   `dkneeds:rust-tests`, retaining remote source/device-model inputs. All 28 selection cases pass,
   including the new profile-recipe selection and existing module/model isolation cases.
   Four final affected preflight tasks pass (`DIGI_GATE_BASE=HEAD mise run test:changed`), as do
   Prettier and `git diff --check`. These are declared-input and runtime-digest proofs; no device
   rerun, actual Nx result-cache restoration, Cargo compilation or full merge gate was run.
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
   Settings owner checkpoint: unchanged file/record move into core/runtime-settings-tests; existing generic inference and module-check discover all three owners. Settings-file edits select only that owner; shared c-demo edits retain settings and remaining consumers, and runtime edits select all owners. Native plans retain all 16 file contents once and all 37 pack/alias definitions; 41 Nx host tests, tracked layercheck and six affected preflight tasks pass. No device rerun for the move.
   Conformance CF-only pilot stopped (2026-11-04): a temporary change selected zero mode only for
   uploadable modules with `module.dsp === false`, retaining coupled DSP modules and all acceptance
   checks. `pnpm nx:pilot run digikit/remote:test-conformance` executed DT2 1.17 and DN2 1.12.
   Both passed initial boot/readiness, USB upload, selected restart/registration, persisted store/hash
   verification and version/SDK-contract assertions. Both then failed the two-second post-registration
   ColdFire instruction-clock wait with `ColdFire clock reset during wait`; final status showed
   DT2 epoch 4 / elapsed 999999 and DN2 epoch 3 / elapsed 12749899, with `dsp: zero` and no audio
   session. Final module-counter, UI log and fault acceptance had not run: that attempt is not
   conformance evidence. The target took 19.9 s overall, with
   Vitest at 17.02 s (failed cases 16.77/16.59 s); no paired A/B or speedup claim.
   At the first checkpoint, reverted the activation and unused clock helper/tests; conformance stayed coupled, with its
   original checks and per-module Nx ownership/cache configuration unchanged. Did not expand to
   debug/probe or run more device tests. Temporary helper verification passed 32 focused host
   tests and regression typecheck before the revert. Run log: `/tmp/digikit-zero-conformance-remote.log`;
   full test log: `.nx/cache/terminalOutputs/12934460849230255417`. Retained final status/module
   evidence: `$TMPDIR/digi-regression/module-load/dt2/run-J8kvTD/remote/` and
   `$TMPDIR/digi-regression/module-load/dn2/run-WsvdBh/remote/`.
   Final doc-only checkpoint: Prettier and `git diff --check` pass;
   `DIGI_GATE_BASE=HEAD mise run test:changed` succeeds with zero selected tasks.
   Correction after controller audit: the first attempt's elapsed/epoch check did not establish a
   guest reset. Boot-job completion rebases the controller's elapsed clock and advances its epoch;
   the added wait rejected that lifecycle transition. The original two-second wait exists to
   exercise DSP registration, so the retry retains it only for `module.dsp === true`, without a
   substitute CF wait. Uploadable modules without DSP images now select zero mode and explicitly
   assert `dsp: zero` / `audio: null`; DSP modules remain coupled. All upload/restart, persisted
   bytes/hash, registration/contracts, UI, module-counter and fault acceptance remains. No
   `DSP_ABSENT` exception was needed: the forbidden runtime log-kind set is unchanged. No Nx task,
   dependency or cache configuration changed.
   Retry evidence: named `digikit/remote:test-conformance`, `digikit/debug:test-conformance` and
   `digikit/probe:test-conformance` each pass DT2 1.17 and DN2 1.12 (six cases). Nx elapsed times:
   remote 13.6 s, debug 13.6 s, probe 20.0 s; Vitest durations 11.23/11.22/17.58 s respectively.
   These are observations, without a paired A/B or speedup claim. Logs:
   `/tmp/digikit-zero-conformance-remote-retry.log`, `/tmp/digikit-zero-conformance-debug.log`,
   `/tmp/digikit-zero-conformance-probe.log`; full test outputs under `.nx/cache/terminalOutputs/`:
   `9939036396911283181` (remote), `12438612099909313564` (debug), `15819926900823830916` (probe).
   Focused remote-helper host file passes all 31 tests; regression typecheck, three affected preflight
   tasks (`DIGI_GATE_BASE=HEAD mise run test:changed`), Prettier and `git diff --check` pass. This is module
   load conformance, not an audio or coupled DSP correctness proof; no full suite or merge gate run.
   Conformance batching deferred: each Nx conformance target already runs one module/device pair
   per case and retains per-module result caching. Existing session reuse has no additional cases
   within that target to amortize. Sharing boots across targets would need an explicit scheduling
   and pristine-card/reset contract; no batching change or speedup is claimed. SDK contract fixtures
   are additive; loader compatibility changes remain deferred by the user.
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

Affected merge-gate checkpoint (2026-11-04): one `mise run test` run before the
repairs selected 140 tasks: 137 succeeded (10 cached), three failed, in 97m 36s.
Log: `/tmp/digikit-nx24-merge-gate.log`. Failures were
`core/runtime-tests:test-device` (only the capacity file; the other eight files
passed and five were skipped), `gate-smoke:smoke` (patched-pluck UI frames), and
`tools:python-tests` (five named patchkit files).

Focused repairs are committed as `24a1da49` (UI expectations and DN2 BRIGHT
action), `bdec3a98` (Python fixtures aligned with current module contracts), and
`45d9a399` (capacity uses cold DSP mode to keep transfers pending without SSI
timing; CardRig avoids reading a nonexistent initial cold-emulator epoch).
The named patched-pluck quick checks pass on DT2 1.17 (18 checks) and DN2 1.12
(14 checks), as detailed below. The gate's per-file Python runner passes all
six files: `test_dyn_fx.py`, `test_dsp_guard_contract.py`, `test_pk_params.py`,
`test_digikit_menu.py`, `test_pk_conform.py` and `test_pk_current.py`, under
`tests/patchkit/`; log: `/tmp/digikit-py-gate-fix.log`. The capacity lane reports
the named file passing on both devices: eight active tests passed and 23
tier-gated tests skipped. Independent reviewers reported no issues with the
repairs. No second full merge gate has run after these repairs; focused passes
do not establish a green full gate or a measured speedup. The next merge
checkpoint remains `mise run test` on the repaired tree.
This documentation checkpoint passes Prettier, `git diff --check` and
`DIGI_GATE_BASE=HEAD mise run test:changed` (zero selected tasks).

Patchkit UI checkpoint (2026-11-04): the merge gate's smoke task passed 14/16 cells;
only the patched-pluck UI frames failed (`.nx/cache/terminalOutputs/13348367458569934345`).
Named DT2 1.17/DN2 1.12 cells with `--tests quick --jobs 2` reproduced all four
frame hashes and pixel bounds exactly; all other quick assertions and DSP budgets passed.
DT2's faders became encoders intentionally in `7549a4f5`; DN2's parameter positions
became B, C, E, F intentionally in `1c572bf3`. The DN2 scenario still turned C
(now PICK), so corrected its BRIGHT step to E and its layout comment. The named
DN2 rerun visually confirms `Brightness=102`; its corrected moved-frame hash is
`ccd412f8` (the old action produced `787ecba3`). Reproduction logs:
`out/runs/patchkit/pluck-ui-reproduce.log` and `out/runs/patchkit/pluck-ui-dn2-corrected.log`.
Only the four affected UI expectations are refreshed; PCM pins, non-UI assertions
and all other cells remain unchanged. Recording and a separate comparison run of
both named quick cells pass: DT2 18 checks, DN2 14 checks, including DSP budgets
(projected high 45.8%/67.1%, below 85%). Comparison took 32 s;
`out/runs/patchkit/pluck-ui-validate.log` and the `20261104-022220-*patched-pluck.jsonl`
logs retain the checks. Prettier and `git diff --check` pass. Full smoke and the
merge gate are not rerun at this checkpoint.

No measured speedup or full gate is claimed. Task-level affected selection is the default after
matrix review; the repository workflow no longer maintains legacy-mode compatibility branches.
No device coverage has been removed, and no new device-test cache boundary has been established by
these host-only checks.
