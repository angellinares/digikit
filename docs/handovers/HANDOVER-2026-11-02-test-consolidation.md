# Handover 2026-11-02: FX persistence/shifter consolidation and next Nx work

## Start here

The active integration checkout is `/Users/em/src/digi/lanes/nx-integration`, branch
`test/fx-persistence-shift-consolidation`, based on `4bd50f5b`.
The Claude checkout was left untouched. Do not switch it or reuse its worktree for integration.
Read [AGENTS.md](../../AGENTS.md), [testing](../TESTING.md), the
[FX assertion ledger](../plans/test-architecture.md#fx-kit-persistence-and-frequency-shifter-design-2026-11-02)
and the [Nx audit checkpoint](../plans/nx-pilot.md#family-project-audit-checkpoint-2026-11-02).

This checkpoint includes completed device proofs, reviewed retirement and the initial final preflight.
The module-placement correction is complete and pushed through `942cae59`.
Em explicitly accepted the focused checks, remaining baseline pathcheck reports and merge of
[PR #248](https://github.com/ElekLabs/digikit/pull/248). This exception applies only to PR #248.
This document records the pre-merge checkpoint; the PR and external roadmap record the merge commit.
Do not interpret the four completed device proofs below as a completed merge gate.

## Decisions and committed checkpoints

Base `4bd50f5b` is merged PR #247 (BASIC FX/pages). Its PR-only exception applied to that batch;
it does not authorize skipping the gate for this batch. No current-batch exception is recorded.
These checkpoints were pushed on the active branch:

| Commit | Change |
| --- | --- |
| `9b23b0ff` | Fixed `fx-kit` write/flush safety policy |
| `6f12384f` | Shared KIT UI and load-observation helper |
| `73711a01` | Consolidated persistence scenarios |
| `01594832` | Consolidated frequency-shifter scenarios |
| `8b7c248e` | Explicit acceptance routing ownership |
| `4b7cbf8d` | Proof and Nx family-audit documentation |
| `ec112ced` | Ten legacy-file deletions and module-owned Nx acceptance dispatch |
| `2eb9c049` | Initial handover draft |
| `501f0482` | Module-local acceptance and generic ownership/preparation |
| `942cae59` | Correct live references and exact temporary-fixture path annotations |

Retirement and module dispatch were committed and pushed at `ec112ced`; the retirement checkpoint
was 33 files, +364/-1497, with ten deletions. Inspect the full branch diff for final PR totals,
including the TypeScript replacements, shared helpers, tests and documentation.

## Acceptance now implemented and proved

[Persistence](../../tests/regression/runtime/fx-kit-persist.test.ts) owns one fixture and test per device,
using one image containing `core/runtime`, `core/dynfx`, `core/fxpages` and an inventory of sine,
gain A and gain B with distinct manifests. DT2 runs the real sine-only Save/Load Kit, SELECT A/B
and restart, then SELECT B/A and restart. This full DT2 case subsumes the old quick DT2 setup;
there is no separate quick DT2 replacement. DN2 preserves its quick zero-ID setup and table swap.

Common persistence assertions retain A01/A02 kit identity, effect defaults, serialized values/IDs,
stock kit values, DSP words, PCM/page round trips and project save/load. Project save must call the
saver at least 100 times. Kit loads observe DSP changes every 2 ms for 600 ms; project load every
10 ms for 7 s, rejecting transient zero p0/mix. Original tolerances remain, with no added generic
peak threshold (`requireAudible: false`; legacy PCM comparisons own that oracle).

[Frequency shifter](../../modules/fx/freqshift/tests/freqshift.device.test.ts) owns four pristine card baselines
per device: dry, master, send, and other-track send. Both devices execute all four;
DN2 must not accidentally omit the other-track case. It retains panel/kit/DSP mapping, page metadata and captured screenshots (no shifter pixel-equality oracle), exact dry/mix-zero/other-track PCM, spectral rejection, send mix and independent float32
model assertions. Settings and route lifetime checks use `fx-routes`; persistence uses `fx-kit`.

Shared [KIT helper](../../tests/regression/runtime/fx-kit-ui.ts):
`createFxKitUi(runtime, plan, profile, artifacts)` exposes `fxPage(page)`, `saveKit(n, tag)` and
`loadKit(n, tag) -> { seen, file }`; `seen` contains `(ms, row, p0, mix)` transitions.
`watchFxKitRows` serves project load polling; `pageKit` accepts optional row zero by default.
The existing tap/release-in-finally and settling primitives are reused. Restart barriers retain the
original `afterEpoch`; silence or an old UI frame is not proof of a new boot.

## Completed device evidence

All four family/device proofs passed using prepared inputs. Durations are wall seconds;
artifact suffixes identify the temporary artifact directories, not stable archive paths.
Exact local paths and input overrides appear below.

| Proof | Invocation / file / case | Artifact suffix | Safety evidence |
| --- | --- | --- | --- |
| DN2 persistence | 129.50 invocation / 123.760 case | `zDOVuw` | 3 watch windows, 2 overlays, `fx-kit` |
| DN2 shifter | 215.72 invocation / 209.578 case | `WDkalo` | 9 watch windows, 4 overlays, `fx-routes` |
| DT2 shifter | 398.171 file / 375.478 case | `acXXAd` | 9 watch windows, 4 overlays, `fx-routes` |
| DT2 persistence | 328.322 file / 320.201 case | `86nKeR` | 7 watch windows, 4 overlays, `fx-kit` |

The combined DT2 invocation took 726.77 s. All lifetime reports recorded zero illegal writes,
flushes and invalid sectors. These are acceptance and safety observations, not a paired performance
benchmark. Do not claim a speedup from them.

Prepared module artifacts and exact daemon/ready overrides are listed below. Do not build new
daemons or replace those inputs merely to resume review. Temporary system artifacts can disappear.

## Retirement and module ownership

The ten completed deletions are five scenario TOMLs (three persistence, two shifter), four legacy
Python device wrappers, and the persistence runner. They were retired after the proofs.
The analysis functions remain: `pk_digikit_fxpage` supplies the PCM comparison CLI;
`pk_digikit_fxshift` retains spectral/model analysis and its JSON adapter. Shared FX/profile/card,
OLED and aligned-PCM analysis remain for their TS consumers. Do not delete independent analysis.
Routing, documentation, the safety policy's source of kit areas and discovery lists are updated
alongside retirement. Independent retirement and dispatch reviews reported no issues.

A final discovery review caught that deleting the module's scenario TOMLs would remove shifter
acceptance from `module:check`. The implemented fix is:

- One uncached `module-freqshift:test-device` target in the module's `project.json` owns acceptance.
- Its command uses existing `verify --tests modules/fx/freqshift/tests/freqshift.device.test.ts`, with no duplicate
  module-to-file ownership table.
- `pk_modcheck` dispatches the owned target after Python conformance/own tests succeed, before
  size/fit. Failure must stop later stages. Legacy daemon/input resolution is preserved.
- Device acceptance still runs both devices; `--device` retains its original conformance and
  size/fit scope. It must not silently narrow the new both-device proof.
- Nested Nx ownership preserves the existing `module-tests` host dependency on the module project.

Actual Nx project/affected checks and exact named verify selection passed after relocation.
The moved scenario selects `module-freqshift:test-device`; module source still selects the parent
`module-tests:test-host`; an unrelated web project change selects no device target. `module:check` was not run end-to-end;
its discovery, failure ordering and daemon resolution were checked with 11 mocked tests. Do not
claim a full module check or broad gate pass. Pure relocation/dispatch changes do not justify
repeating the completed four emulator proofs.

## Final preflight and ownership correction

The parent-owned preflight at `2eb9c049` passed host tests, Ruff check/format, mypy on five sources,
generated checks, oxlint, Prettier and workspace types. It failed:

- Sixteen layer violations from module-specific identities/tests outside their owning module.
- Twelve stale Python expectations after retiring wrappers (11 changed-test cases, one routing case).
- New stale path references plus three unchanged baseline paths in
  `docs/plans/module-architecture.md` (lines 3, 4 and 284).

The earlier child preflight lacked a final exit result and is superseded by this captured run.
The initial failed run is not a green preflight, even though individual checks passed.

The correction moves the shifter scenario and analyzer/module-specific routing tests into the
module. Its project owns `metadata.runtimeAcceptance` with module-relative files, required packs,
external source paths and plusdrive requirement. One generic reader feeds the existing router,
exact named selection and preparation. This metadata is a repository contract: Nx itself does not
interpret it. Generic runtime discovery includes module-owned device files; host discovery excludes
them and TypeScript checking includes them. Owner changes include its reference model. No central
module-ID table, layercheck exception, custom runner or cache is added.

## Final validation and publication checkpoint

The corrected parent preflight passed 680 Python cases (270 + 57 + 353) and 336 host cases
(one skipped), Ruff check/format, mypy, layers, generated checks, oxlint, Prettier and workspace
types. Its exit status was 1 for pathcheck only. After `942cae59`, scoped pathcheck across all
placement files passed with zero stale references; only the three unchanged baseline reports in
the module-architecture plan remain. Do not call this a fully green preflight or full gate pass.

Additional focused evidence: 445 named Python cases and a 14-case metadata/routing check passed;
these overlap the preflight and must not be summed as unique coverage. The runtime collector lists
20 files including the moved suite, its exact filter selects only that suite, helper discovery
returns the registered external path, and host collection excludes device files. Independent final
review reported no issues. The moved scenario body is identical after imports, and analyzer tests
are byte-identical. No emulator proof was rerun for the move.

The final code checkpoint is `942cae59`, pushed on the active branch. PR #248 has explicit focused-acceptance and merge authorization. The final
documentation-only commit follows it; use `git log -1` and upstream comparison for that hash.
The full routed gate is unrun. Em granted a PR-248-only focused exception, including the three
baseline pathcheck reports. This is not a full-gate pass and does not transfer to future work.

## Continue from here

1. PR #248 merged at `7cb81ee5`. Its focused-check exception does not transfer.
2. Current work is on lane `/Users/em/src/digi/lanes/nx-integration`, branch `test/nx-family-design`
   from `origin/main`. The Claude checkout is separate; do not use it.
3. The agreed design and stages are in the
   [Nx plan](../plans/nx-pilot.md#module-graph-and-staged-selection-design-2026-11-02). Stage 1 is implemented; stage 1b is next.
   Do not repeat completed device proofs; stage 1 is proved by identical selection.

The five audited cached host targets remain separate from this uncached device target. Broader
Nx acceptance routing, Python/Rust plugins and device/build caching are not implemented or justified
by this batch. Preserve artifact packs/aliases, domain-module dependencies and conservative fallbacks.

For a fresh context: one writer per lane; scouts/reviewers are read-only, workers own scoped edits,
children do not delegate. The parent owns decisions, gates and publication. The Claude checkout is
not this lane. Do not commit firmware, generated artifacts, cards or audio. Use existing benchlock;
never stop another session's processes. Lane writes need targeted sandbox escalation, not bypasses.

## Exact retained inputs and evidence

The four artifact directories share this prefix:
`/private/var/folders/v4/dxpyscrd41n7h6v9bc672p0r0000gn/T/`.
Their full directory names are:

- `digi-fx-fx-kit-persist-dn2-zDOVuw`
- `digi-fx-fx-freqshift-dn2-WDkalo`
- `digi-fx-fx-freqshift-dt2-acXXAd`
- `digi-fx-fx-kit-persist-dt2-86nKeR`

Each contains `lifetime-safety.json`, watch summaries, overlay evidence and scenario outputs.
A separate read-only reviewer confirmed all four lifetime reports pass and the exact suffixes.
Do not commit these directories: they include firmware/card/audio-derived material.
Temporary artifacts are local evidence and may disappear; the committed assertions and recorded
proof results are the durable record.

Proof environment, all absolute paths below relative to the integration lane when abbreviated:

```text
UV_NO_SYNC=1
UV_CACHE_DIR=/private/tmp/runtime-readiness-uv-cache
REGRESSION_JOBS=1
DIGI_EMUD_DT2=/Users/em/src/digi/lanes/nx-integration/out/native/target-wt/nx-integration/g-e7aa50f9/pgo-emud-dt2/aarch64-apple-darwin/stable/digi-emud-abd870786925
DIGI_EMUD_DN2=/Users/em/src/digi/lanes/nx-integration/out/native/target-wt/nx-integration/prod/pgo-emud-dn2/aarch64-apple-darwin/stable/digi-emud-cf4f57de71b7
DIGI_RUNTIME_MODULES=/Users/em/src/digi/lanes/nx-integration/out/runs/fx-persistence-shift-consolidation/modules
DIGI_PK_READY_DT2_117=/Users/em/src/digi/lanes/nx-integration/out/ready-pk-dt2-117
DIGI_PK_READY_DN2_112=/Users/em/src/digi/lanes/nx-integration/out/ready-fix-dn2-112
RUSTC=/Users/em/.rustup/toolchains/1.98.1-aarch64-apple-darwin/bin/rustc
RUSTDOC=/Users/em/.rustup/toolchains/1.98.1-aarch64-apple-darwin/bin/rustdoc
```

Device proofs used the existing shared benchlock and named Vitest runtime files, one worker;
no new daemons were built. Module artifacts were prepared once using the freshness-checked
patchkit wrapper. Do not substitute another checkout's shared release packer.

Initial failed preflight: `/private/tmp/fx-persistence-shift-final-preflight.log`.
Final corrected preflight: `/private/tmp/fx-persistence-shift-placement-preflight.log`;
Python details: `out/runs/test-changed/20261101-205015` in the integration lane.
Gate explanation: `/private/tmp/fx-persistence-shift-placement-explain.txt`.
The explanation selects 20 runtime files, 3 module files, 89 Python files, stock/host/web/native/
smoke and preparation steps. It was not executed. Compare against `origin/main`; local `main`
is stale and must not be used as the comparison base.
