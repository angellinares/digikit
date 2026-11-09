# Test architecture: four levels, native module tests, TypeScript (plan, 2026-10-21)

One PR per item, branch `<area>/<topic>`. Related plans (outside the repo, in ~/src/digi):
TEST-SELECTION-PLAN-2026-10-20.md (routing; this plan replaces its Phases 3 and 4 and Phase 2's representative set and
`module:test`), WORKBENCH-AND-MODULES-PLAN-2026-10-19.md (P3d FX contract carries T2).

## Status at end of session
- 2026-10-21: plan written. No code changed. Order: TEST-SELECTION Phase 2 PR 1 (gate routing) first, then the modules
  plan; T1 when module work needs it, T2 with P3d.

## Goal

Each test runs at the cheapest level that answers its question. Module tests, including sound, run in seconds on the
native SHARC engine. Tests that do not listen to audio do not run the DSP. Long term, tests and non-RE tools are
TypeScript (vitest); Python stays for reverse engineering (tools/sharc_dev, tools/coldfire, Ghidra, firmware analysis).

## Decisions (Em, 2026-10-21)

1. Local only. No CI for now.
2. Four test levels (below). Split unit and integration tests.
3. TypeScript and vitest for tests and non-RE tools, long term. Migrate by replacement, not by a bulk port: new levels
   are written in TS; a Python test is deleted when its TS replacement checks the same oracle. No new Python test code
   outside the RE tooling.
4. Little change for the sake of tests: do this work where module or plan work needs it.
5. The Python SHARC core stays the reference for the instruction semantics and generates the Rust core.
6. PCM pins (`tests/scenarios/*.json`, module pins) change only with Em's approval.

## The four levels

| Level | Question | Runs on | Speed |
|---|---|---|---|
| unit | Is the logic right (packing, manifests, routing, tables)? | Rust and TS (Python until moved), no emulator | ms to s |
| module | Is the machine or FX right, and does it sound right? | the module routine alone, through its contract, on the native SHARC engine | s |
| device-cf | Does the firmware handle it (menus, parameters, save and load, upload, type numbers, memory audit)? | ColdFire emulator, DSP stubbed | about 1.1x real time |
| device-coupled | Does it sound right inside the device (stock filter, envelope, FX, master; the hook glue)? | both CPUs | DT2 about 1x, DN2 0.62-0.66x real time at load 9-12 (2026-11-07, [finding 18](../findings/18-dsp-performance.md)) |

Each module type keeps one coupled test per device. Everything else moves down a level where it can.

## Evidence (2026-10-21)

- Replay of the last 25 merged PRs (static, verify's routing against the import graph): 12 touched verify.py or
  verify_route.py and ran everything (about 1,350 file-seconds); 5 Rust or `core/*` PRs expand to tests/patchkit and
  tests/digikit (860-1,335 file-seconds even with the import graph); 8 Python-only PRs drop from 100-1,270 to 3-92.
- The Python SHARC core runs every isolated routine test (MachineRig, dsp_voice_rig, pk_sharc): test_pluck 322 s,
  test_dsp_voice 99, test_patchkit (part) 73, test_machine_conformance 65, test_sine 60, the pluck tune and range files 67,
  others about 15 (medians). About 700 of the 1,350 s of a full python-tests run, and every `module:check`.
- The smoke step costs about 380 s on 4 cores on every non-docs change (17 cells).
- Coupled runs 0.21-0.24 emulated s per wall s; ColdFire-only 1.1 (finding 39). `digi-emud --dsp zero` exists; its
  cost and whether the ColdFire side waits on DSP replies are not measured.

## Items

### T1. Native call harness for module routines (0.5-1 day; payoff high, confidence medium)
- [ ] Scout: can the Rust SHARC engine run one routine outside the coupled loop? Does it have a store hook (needed by
      the P4a write-set check)?
- [ ] A Rust CLI or daemon with a plain request format: load the module objects, set the registers, call `note_on` and
      `render` for N blocks, return the output, cycle counts and recorded stores. Language neutral: Python calls it now,
      TS later. No Python dependency added.
- [ ] MachineRig and dsp_voice_rig use it; the tests stay unchanged. A cross-check test runs a few cases on both engines
      and compares the outputs.
- [ ] Before/after: the files listed in Evidence.

### T2. Module test kit (with P3d; 1-2 days; confidence medium-high)
- [ ] TS/vitest harness over T1, for machines (`render`, `note_on`, `desc`) and FX (`dk_fx_process`).
- [ ] Sound checks a module declares in its `tests/*.toml`: pitch against note, decay time, RMS, DC, parameter response
      (direction per parameter), a pinned PCM hash.
- [ ] `module:test NAME`: build check, conformance, sound checks, the smoke cells and quick scenarios that name it.
- [ ] Retire the Python conformance suite and the pluck sweeps when the TS kit checks the same pins.

### T3. DSP stub for tests that do not listen (1 hour to measure, then about 1 day)
- [ ] Measure `--dsp zero`: cost, and whether the ColdFire side needs DSP replies that a stub must answer. Stop if it does
      not pay.
- [ ] Move to ColdFire-only: menu, registry, upload, round trip, safe mode, cut, the table half of equiv. Only PCM compares
      stay coupled.

### T4. Device levels in TS (several days; by replacement)
- [ ] vitest projects `unit`, `module`, `device-cf`, `device-coupled`; device tests drive `digi-emud` through
      packages/control-client.
- [ ] Each landed TS level deletes its Python counterpart (pk_harness cells, digikit scenario runners).

### T5. Smoke and scenario selection by module (0.5-1 day)
- [ ] Filter the smoke matrix and the quick scenarios to the cells that contain a changed module.
- [ ] `core/*` and runtime changes run a representative set (one cell and one quick scenario per module type per
      device); the full sets run on request before a release.

### T6. verify.py (last; about 1 day)
- [ ] Python routing reduced to what vitest's related-test selection does not cover; keep the CPU budget.

## Rules

- One PR per item; design agreed with Em in chat before code.
- Two test systems run side by side only per level, and only until that level's replacement passes: one Python file
  deleted per TS file that replaces it.
- Each item reports before/after (files, file-seconds, gate wall time) against the recorded runs.


## FX consolidation design (2026-11-01)

Status: Em selected design work and proposed reusing a daemon through the control socket with
setup/teardown fixtures. The first lifecycle/PCM pilot is now implemented under subsequent approval;
its earlier fixture proofs passed. The safety increment below records the current PR #242 state. This design supersedes the historical one-PR-per-item rule: keep the reusable fixture
and compatible FX coverage in one coherent change, with explicit checkpoints rather than a bulk
seven-family port. No legacy deletion, new RPC or benchmark is authorized by this checkpoint.

### Fixture lifecycle and isolation

Extend the existing Session lifecycle with Vitest `beforeAll`, `beforeEach`, `afterEach` and
`afterAll` hooks; do not introduce a parallel test framework. `beforeAll` owns one daemon per device
per test file/worker, reused by serial cases. Never attach to another agent's or the user's daemon.
A Session baseline-reload method used by `beforeEach` updates its private load result, card and event
state; a raw-client load with stale Session metadata is insufficient. Reload through the existing
control-socket `firmware.load` operation (also used by the workbench): `replace: true`, `force: true`, explicit
pristine card/image/runtime patch set and `start: auto`. This rebuilds firmware state in the same
process; firmware boot still occurs. The stock-only
[Session restart wrapper](../../packages/test-support/src/session.ts) is not a limitation of the
underlying load operation. In implementation, extend the existing
[FirmwareLoadParams type](../../packages/control-client/src/types.ts) with its missing `replace`
field rather than inventing a reset RPC.

Use a fixture-owned pristine card directory, not just a pristine base file:
[controller card loading](../../packages/emulator/control/src/controller.rs) applies a saved sibling
overlay. There must be no inherited `overlay.bin`; keep mirror/card synchronization off. Reuse the
fixture-owned pristine base without copying a huge card per case only when read-only backing and
disabled synchronization are guaranteed. Specify `auto_no` consistently on every load. Recreate DkSession and EmuUsbTransport per case, fence observations
by the new epoch, and clear collected events. Actor-level options survive runtime replacement:
explicitly set CPU exception delivery (normally false), watchdog stall policy and mirror settings;
reapply guards/watchdog configuration rather than assuming load resets them.

A load is rejected while a controller job or patch job is active. Settle or cancel owned work within
a deadline before loading; replacement does not promise to cancel every job. Failed or dirty reset
fails setup, preserves diagnostics and discards the owned daemon; never silently continue as a clean
case. `afterEach` collects the case's guard/fault/card/PCM closing evidence before discarding state,
then stops capture, pauses the runtime and cancels pending protocol work. Input helpers own release
in `finally`; no release-all RPC is assumed. `afterAll` closes the owned daemon.

Within a persistence scenario, use protocol restart to preserve guest writes and RAM according to
that scenario's contract. An independent-case clean load must not replace that restart. The first
batch introduces no generic reset RPC, global daemon pool, result cache or snapshot restoration.
CF-only snapshots do not prove matched DSP state, and the existing state-load operation requires
instruction count zero; neither establishes the desired coupled reset contract.

### Static scope and coverage groups

**[D]** Historical counts before the approved gain-leg deletion: these describe rig entries and explicit restarts, not
actual uncached boots or elapsed time. Quick tier has 14 scenario/device pairs; full tier has 15.
The earlier estimate of 80 boots per device reduced to nine is unsupported and is withdrawn.

| Family / source anchor | Quick DT2 rig entries | Quick DN2 rig entries | Explicit restarts per device |
| --- | ---: | ---: | ---: |
| fx (retired runner, historical line 312) | 4 | 4 | 1 |
| [fxsend](../../tools/patchkit/pk_digikit_fxsend.py) line 192 | 5 | 8 | 0 |
| fxroute (retired runner, historical line 558) | 11 | 11 | 0 |
| fxroutes (retired runner, historical line 637) | 8 | 8 | 2 |
| fxpage (retired runner, historical line 513) | 3 | 3 | 1 |
| fxpersist (retired runner; `REF:tools/patchkit/pk_digikit_fxpersist.py`, historical line 275) | 1 | 1 | 0 |
| fxshift (retired device runner, historical line 499) | 4 | 4 | 0 |
| Total | 36 | 39 | 4 |

Quick totals are therefore 40 DT2 and 43 DN2 static operations. Full tier adds one DT2 persistence
rig entry and two explicit restarts, three additional operations. These counts make no savings
promise. Use three coverage groups for the design: chain plus page baseline; send plus routes;
persistence plus frequency shifter. These are coverage boundaries, not a proven nine-boot schedule.

Start with the reusable fixture, one existing representative gain-FX lifecycle/PCM flow and its
cleanup assertion on DT2 OS 1.17 and DN2 OS 1.12; no large new test matrix is needed for this pilot.
Only then fold compatible fx/fxpage checks into the same coherent PR, using an
assertion ledger that maps each old assertion, device, image/seed state and PCM/PIX oracle to its
replacement. Retain special no-hook/fxpages-off images, capacity/overflow and no-module cases,
deselect/restart checks and each case's closing checks. Do not delete a family until every unique
assertion is mapped and its port passes. Contract-feature coverage is a safety aid, not proof that
these scenario facts are interchangeable.

### Proof checkpoint and unresolved implementation choices

The focused isolation proof dirties a case, clean-loads in the same daemon, and checks expected
registry, card/settings, CF/DSP baseline, input state and a new epoch. Compare that reset baseline
with a fresh-process reference once per device using focused cases. A separate persistence case
must prove that writes survive protocol restart. Keep the closing guard/fault/card/PCM checks;
there is no broad sweep for this experiment. Stop the first slice if useful process reuse or exact
isolation cannot be established. Process-reuse payoff is unmeasured; use the existing bounded
measurement workflow only if a later decision needs it, without making a performance claim now.

Preboot seeding must reuse the existing Rust store/settings codecs. Inspect the
[native card builder](../../packages/emulator/control/src/cards.rs) and available APIs before proposing
a narrow operation; do not duplicate the codec in TypeScript. Postboot upload cannot substitute for
assertions about a preboot-seeded state. Exact seed transport and fixture implementation remain open.

PCM capture is already available through generic client calls to `pcm.tap`; `--no-audio` disables
playback only, not capture. The existing
[FX support](../../tests/regression/scenarios/fx-support.ts) uses a Python renderer/NumPy path.
Choose minimal analyzer reuse for live captures or a small TypeScript comparison only after inspecting
the needed oracles; do not port DSP analysis wholesale or invent protocol APIs. Seed/analyzer choices
and the complete actor-state reset checklist are bounded checks during the first implementation
slice; resolve them before relying on the affected assertion, rather than blocking every fixture step.


### First implementation checkpoint (2026-11-01)

**[D]** Historical first fixture proof, before the safety increment: both device proofs and fast preflight passed. the [gain-FX pilot](../../modules/core/adapters/dynfx/tests/fx-reuse.device.test.ts)
uses [Session baseline reload](../../packages/test-support/src/session.ts) and the existing Vitest
[regression fixture](../../tests/regression/support.ts). No legacy family is deleted or declared equivalent.
One serial scenario per device covers dry → gain → protocol deselection/restart → pristine dry,
plus one sequential fresh-process dry reference while the kept daemon is paused.

Clean reload uses the existing firmware-load replacement path in the same owned process, updates
Session metadata, verifies a newer epoch and creates fresh protocol/USB receive state. Firmware
boot still occurs. The deselection stage instead uses protocol restart, retaining guest writes;
it must not be substituted with pristine reload. Reset/cleanup failures discard only the owned
daemon and retain its diagnostics. Native plusdrive builds the preboot selections from prepared
DKMs in owned overlay-free card directories, with mirroring/synchronization disabled. No TypeScript
store codec, new reset/seed RPC, snapshot restore or daemon pool was added.

At that first checkpoint, the following assertions passed the focused DT2 and DN2 proofs:

| Area | Pilot asserts | Retained legacy facts / limits |
| --- | --- | --- |
| Boot and registry | Normal boot, count/loaded/results/UI; assignment identity; reported module types/contracts, FX slots and free/used rows | No-hook/plain, full-capacity/overflow, no-module and fxpage variants remain |
| Gain chain | Enabled/default parameters, descriptor/state within the module DSP range, adapter fault fields and one-call-per-block state progress | Inferred resource needs, exact relocated DSP bytes and exact row pointers remain |
| Reset isolation | New epoch, old protocol invalidation, restored normalized module/FX capacity/input/settings/DSP semantics and fresh-process comparison | Not a whole-RAM/tick digest or deliberate dirty-settings isolation proof |
| PCM | Dry × 0.5 within 2 ULP, RMS within 0.001 dB and onset shift at most 128 samples; deselected/restored/reference dry bit-identical from onset | Existing PCM/PIX oracles and all other FX family scenarios remain |
| Closing evidence | No runtime fault/DSP halt/nonfinite PCM, audible sine, targeted post-load boot-identity guard and exported card overlay | Launch-time expansion write-watch, flush, card-sector allow policy and DT2 selected-track type/retry proof remain |

Captures pause execution, choose current DSP frame + 300 after panel setup (the legacy restarted-note
strategy), step 44,000 CF instructions beyond the boundary and hold TRIG 1 for 1,500 DSP frames.
Key-up runs in `finally`. Input idle is observed while free before pausing. The existing
[PCM comparator](../../tools/patchkit/hook_ab.py) now exposes onset alignment and JSON metrics; the
legacy scaled-comparison alias uses the same function. No WAV codec or DSP analyzer was duplicated.
Absent pristine settings are null only for the structured `E_SET_NOKEY` status; other errors fail.

Prerequisites are prepared separately, not built by the test. For the default module directory:

```sh
uv run python -m tools.firmware.plusdrive_check --build-only
mkdir -p out/regression/modules/dt2 out/regression/modules/dn2
for device in dt2 dn2; do
  uv run python -m tools.patchkit.patchkit pack test/sine --device "$device" -o "out/regression/modules/$device/sine.dkm"
  uv run python -m tools.patchkit.patchkit pack test/dyn-fx-gain --device "$device" -o "out/regression/modules/$device/dyn-fx-gain.dkm"
done
```

The test uses the existing fixed native plusdrive binary and preflighted daemon/ready inputs.
`DIGI_RUNTIME_MODULES` may name an already prepared module directory. Gate ownership maps both
fixtures to the pilot, and both explain/execution select plusdrive preparation for an FX-only run.

Focused checks passed: 50 host cases plus the one canonical-path assertion correction retry (51
across the runs); 10 PCM synthetic cases; seven fault/settings cases; 41 routing cases plus four
new prerequisite/explain cases. Affected package/regression types passed. Independent foundation
and pilot reviews completed with their reported blockers corrected. Both device proofs passed
on their first runs, each with one selected case and the other device's case skipped:

| Device | Invocation | Case | Local log |
| --- | ---: | ---: | --- |
| DT2 OS 1.17 | 89.66 s | 73.23 s | `/private/tmp/fx-reuse-dt2.log` |
| DN2 OS 1.12 | 62.06 s | 53.506 s | `/private/tmp/fx-reuse-dn2.log` |

These runs include the fresh reference, gain PCM, protocol deselection and clean restore. They are
functional evidence, not a speedup benchmark, complete migration or broad-gate acceptance. Legacy
coverage remains retained. Final `mise run test:changed` passed: 612 Python tests, 301 host passed
and one skipped, plus lint, format, types, path, layers and generated checks. Log:
`/private/tmp/fx-reuse-preflight.log`. Final independent static review found no issues after the
reported blockers were corrected. The gate plan was explained only; the broad gate was not run.

At that checkpoint, exact inferred needs, relocated DSP bytes/row pointers, selected-track type
and launch-time write-watch/flush/card-sector policy were not established as equivalent. No legacy
deletion is authorized by this evidence. Next, close the assertion ledger before parallel family
migrations. The coordinator retains integration, remaining acceptance and publication decisions.


### Safety increment in PR #242 (2026-11-01)

**[D]** Both revised device cases and required final fast preflight passed. This is the
same PR #242, with no legacy `one()` deletion and no merge-acceptance claim. Eleven new Python
synthetic cases, 57 affected mocked host cases, affected types and scoped lint passed.

The existing Python runtime rig and the TypeScript test now share the offline watch-range calculation,
including its DMA-buffer holes and allowed writer-PC range. The patch plan is prepared before spawn,
then validated against the loaded image after pristine baseline load. Existing write-watch/flush
instrumentation is configured in the daemon's environment from startup, with CACR activation and
per-emulator sequence output. After the owned daemon closes, the check requires contiguous watch
logs matched by complete summaries, at least the observed epochs, zero write events and zero flush
hits. Every exported overlay is parsed by the existing Python parser: store writes are limited to
the two selection sectors, and writes below sector 0x50000 are limited to those same sectors.
Missing/truncated summaries or malformed overlays fail rather than becoming missing evidence.

The gain oracle reuses the independent Python DSP parser/relocator and the existing needs wrapper.
It now compares the exact relocated DSP image, exact first 12 chain-row words and MODULES/FX-slot
resource figures against the inferred expectations. The row defaults, enabled/was/disabled/fault
fields are checked again after capture, preserving the post-note assertion rather than setup alone.
No new emulator instrumentation, RPC, runner, store codec or relocation implementation was added.

The earlier fresh-process reference passed on both devices as a one-time fixture-equivalence proof;
it is historical evidence, removed from default acceptance without an optional hidden test. The
permanent case remains initial dry → gain → protocol deselection/restart → pristine dry restore.
Reuse now skips Session's redundant SDK patched-image load; the existing CLI stock startup still
occurs. Clean baseline replacement and guest-write-preserving protocol restart remain distinct.

| Revised proof | Invocation | Case | Watch/overlay result | Local log |
| --- | ---: | ---: | --- | --- |
| DT2 OS 1.17 | 63.62 s | 60.64 s | 8 emulator summaries, zero write/flush hits, 4 valid overlays | `/private/tmp/fx-safety-dt2.log` |
| DN2 OS 1.12 | 50.16 s | 47.441 s | 8 emulator summaries, zero write/flush hits, 4 valid overlays | `/private/tmp/fx-safety-dn2.log` |

Both passed on the first revised run. These durations describe correctness runs, not a paired
benchmark or measured speedup. At this safety checkpoint, the selected-track-type decision was
still pending and legacy `one()` remained. The approved replacement below resolves that boundary;
plain/full/fxpage and other family coverage remain. The coordinator owns
acceptance and publication; the broad merge gate remains unrun.

Required fast preflight passed (`/private/tmp/fx-safety-preflight.log`): 612 audited Python routing
tests and 75 host cases (four test-support, 71 regression), plus lint, format, path, layers, generated
checks and all workspace TypeScript types. Mypy was skipped because no changed files were in its
typed set. The separate 11 new Python synthetic and 57 focused mocked-host passes above remain
iteration evidence, not additional unique cases to add to these totals. This is a verified safety
checkpoint in draft PR #242, before the approved gain-leg replacement below.


### Approved gain-leg replacement (2026-11-01)

Em explicitly accepted the FX-behavior contract: the TypeScript gain/deselect sequence proves behavior
on audible input, without a direct selected-track-type assertion. The removed `one()` leg previously
made DT2 type observations for both its wet note and its post-restart dry note. Those observations
are intentionally not reproduced; the independent legacy dry/full legs retain their DT2 selected-sine
checks. This is a bounded coverage decision, not a claim that PCM identifies the selected machine.

Only the nested Python `one()` leg, its parallel entry and its gain/back PCM comparisons are removed.
The remaining plain/dry/full functions, plain-versus-dry and full-chain PCM, overflow/fit rules and
common launch-time checks are unchanged. No scenario TOML, wrapper file or other FX family is deleted.
The replacement owner is `mise run test:runtime -- fx-reuse`; `test:fx` / `test:fx:dt2` own stock-FX
suites and are not the replacement owner.

Runtime ownership also follows the consumed CLI helpers: direct changes to the existing machine
oracle or arena-safety module select the gain case; their import closure, including the function-local
needs wrapper, retains runtime coverage through the existing dependency graph. No synthetic
legacy-runner-to-TypeScript edge was added. The three focused routing cases passed; coordinator
fast preflight of this deletion follow-up passed (final checkpoint below).

| Removed `one()` assertion | Replacement / accepted boundary |
| --- | --- |
| Boot results, safe/loaded state, module names/contracts/rows and inferred DSP range/FX slot | Verified TypeScript `inspect` plus the existing independent needs wrapper |
| Relocated DSP image, exact first 12 chain-row words/defaults and free rows | Independent Python parser/relocator JSON oracle, compared with live DSP bytes/rows |
| Post-note enabled/was/disabled/fault/parameter fields and state call counter | Post-capture row checks and the unchanged >1000 / 90–110 call-count assertions |
| Gain ×0.5 PCM | Existing comparator, at most 2 ULP, RMS tolerance 0.001 dB, onset shift at most 128 samples |
| SELECT/restart, dry-only registry/boot/free rows/capacity and restored dry PCM | Protocol deselection/restart in the TypeScript sequence; bit-identical dry PCM from onset |
| Lifetime write/flush and card-sector policy | Sequenced startup-to-close summaries and every exported overlay, proven on both devices in the safety increment |
| Direct DT2 selected-type observations inside this leg | Explicitly accepted narrower audible-input FX contract; legacy dry/full retain direct type coverage |

Historical static counts above remain the pre-deletion baseline. This deletion subtracts one legacy
rig entry and one explicit restart per device: the `fx` family is now three rig entries and zero
restarts per device. Across the seven legacy quick families, remaining counts are 35 DT2 / 38 DN2
rig entries plus three restarts each (38 / 41 static operations). Full tier still adds one DT2 rig
entry and two restarts; scenario/device pair counts remain 14 quick and 15 full. These are remaining
legacy source counts, excluding the TypeScript case, not uncached boots or measured speedup.

The verified TypeScript execution is unchanged by this deletion-only follow-up; its audible-input
assertion text and selection comment now state the accepted contract. Earlier safety/device results
remain evidence for that unchanged execution. The follow-up fast preflight passed; the coordinator
owned the remaining acceptance decision at that checkpoint; the broad gate remained unrun. Other FX families remain unmodified; subsequent migrations will use the completed assertion mapping and fixture.


Deletion follow-up fast preflight passed (`/private/tmp/fx-gain-deletion-preflight.log`): 345 Python
cases (57 changed-tests and 288 verify), 71 regression-host cases, Ruff, mypy, path, layers,
generated checks, oxlint, Prettier and all workspace types. No device rerun was needed: both-device
safety proof at `b963e257` covers unchanged TS execution; only its wording/comment changed afterward.

The gate was explained against the actual PR base `origin/main` at `0feb02cb`: host/web, three stock
files, 11 runtime files, three module-load files, 49 Python files, smoke and an implicit daemon build.
`/private/tmp/fx-gain-merge-explain-origin-main.txt` supersedes the local-main explanation
(`/private/tmp/fx-gain-merge-explain.txt`): local main at `365b45b` was stale and overselected already
merged changes. At this pre-merge checkpoint, the broad gate had not run and no focused-acceptance
exception existed yet; the subsequent merge decision below supersedes that pending status. Historical proof/count tables and the bounded type-coverage decision stand.


### PR #242 merged and next-batch bookkeeping (2026-11-01)

PR #242 merged at `a41699e5` on 2026-11-01T09:46:15Z. Em explicitly accepted the prior both-device
proofs, helper/host/routing tests and static/type checks under a one-time focused-acceptance exception.
This supersedes the pending acceptance status above; the broad routed gate was not run. The device
runs remain correctness evidence, not a paired speedup benchmark. The bounded audible-input contract
and the retained legacy plain/dry/full coverage remain as recorded in the assertion mapping.

The next coherent batch is mapping the remaining basic-FX and fxpage work, alongside the other
families, for independent workers using the shared fixture and completed assertion mapping. Em
already endorsed parallel migration after the first proof and deletion. This is bookkeeping while
the shared design is mapped, not a broad bulk-deletion plan or a new performance claim.


Next-batch prerequisite: shared gain/SEND support is extracted mechanically into
`tests/regression/runtime/fx-live.ts`. The first migration target is FX SEND: its five DT2 and eight
DN2 same-image legs can use clean reload without alternate images or watch policies. Shared setup
remains explicitly sine/gain on the default emulator image. Optional input hints and per-case
silent-evidence policy do not relax mandatory no-fault/nonfinite/watch/overlay checks. Remaining
basic-FX plain/dry/full and fxpage stay unchanged, retaining their direct type checks; no new SEND
case or further legacy deletion is part of this extraction.


### FX SEND implementation and device proof (2026-11-01)

The SEND replacement covers 13 independent baselines in three owned daemon processes: five common
legs on DT2, five on DN2, and three DN2 chord/idle legs. Each independent leg reloads its pristine
native sine/gain card in the kept process; this is not a persistence restart. The shared fixture
retains startup-to-close write/flush summaries, every-overlay policy checks, guards, finite PCM and
no-fault checks. Idle alone permits silence. This is a coverage/process-count description, not a
measured speedup; stock startup and firmware reloads still occur.

| Legacy SEND facts | Replacement |
| --- | --- |
| Dry audible input; safe boot/module results; clean default row, enabled state and zero route/amount words | Common case plus shared gain-fixture inspection and mandatory closing evidence |
| ZERO/OTHER route 1 and packed amount readback; dry-identical PCM | Common case, unchanged bit-identity and onset-shift limit of 128 samples |
| FULL return adds 0.5; HALF follows `(64/127)^2` | Existing independent added-factor oracle against ZERO, relative tolerances 0.001 and 0.01; positive full gain proves nonidentity with dry |
| Every wet post-note WAS/DIS/NFAULT and >1000 call count; +100 frames gives 90–110 calls | Common and chord cases retain the exact counter assertions; legacy idle retains activity without the delta constraint |
| DN2 chord sums at least two sounding voice slots; idle has >1000 samples and exactly zero peak | DN2-specific anchored slot-bus and existing PCM/added-factor oracles; final fixed-frame proof passed |

Input selection follows the accepted audible-input FX contract, without a direct selected-type
observation in these TS sequences. Basic legacy dry/full retain their direct type proof; plain,
fit/overflow and fxpage remain unchanged. No further family deletion follows from this mapping.

Final common proofs passed: DT2 87.51 s invocation / 82.090 s case
(`digi-fx-reuse-dt2-PJYUEo`), DN2 67.634 s file / 65.074 s case
(`digi-fx-reuse-dn2-KSWG4L`). ZERO and OTHER both had identity shift 0; FULL added about 0.500003
and HALF about 0.126977. Each lifetime check passed eight emulator summaries and five exported
overlays. These durations are correctness evidence only. Earlier `current + 300` timing produced
DT2 shift 192; sharing the first dry target then failed DN2's late-setup guard. The final shape
restores established absolute targets DT2 7500 / DN2 7000, retaining the 20-frame setup guard,
44,000-instruction offset, capture windows and all PCM tolerances. Per-WAV timing artifacts expose
setup frame and target; a missed deadline fails rather than adapting.

Final fixed-7000 DN2 extras passed: 42.055 s file / 39.564 s case (DN2 invocation 109.92 s;
`digi-fx-reuse-dn2-ulBe72`). Both chord legs had exactly two sounding buses, added factor
0.5000029707401741, and idle had 48,000 finite samples with peak 0. Lifetime safety passed five
summaries/three overlays with zero write/flush hits. All three final device cases now pass; no
additional device run is needed for deletion. Static review found no issues. Legacy retirement
checkpoint `42028b59` is integrated as `804bdae2`, after runtime/routing checkpoint `da4fdf7f`: the
SEND wrappers/scenarios and device runner are removed while the analysis CLI remains. No device
execution changed after the final proofs.
Final fast preflight passed: `mise run test:changed -- --base origin/main` ran 621 Python cases
(270 + 57 + 294) and 73 host cases, with Ruff check/format, mypy (one source), pathcheck (zero stale
references), layer/generated checks, oxlint, Prettier and workspace types. Earlier 21 analyzer and
two shared-helper tests also passed. The gate explanation selects host, all 13 runtime fixtures,
47 Python files, smoke and daemon prerequisites; that routed gate was not run. Focused acceptance
for this batch is pending Em's decision after the concrete PR; PR #242's exception
does not transfer.


### Routing ownership checkpoint (2026-11-01)

PR #244 merged at `097eefa2`. The next routing change uses Vitest's existing import discovery
for TypeScript helpers inside the stock, runtime and module acceptance directories. It discovers
filenames without collecting tests or executing hooks; it adds no separate dependency graph.
`runtime/fx-live.ts` selects the three FX files, while `runtime/live.ts` selects all 13 runtime
files and retains all module lifecycle files through its explicit shared ownership. Missing tools,
discovery failures, unconsumed helpers and deleted files retain the whole owner suite. Production, shared session and configuration routes remain conservative.

Runtime TypeScript and digikit scenario TOMLs no longer select the unrelated smoke matrix by
themselves. Production sources, module-owned scenarios, smoke inputs and shared/configuration
files retain smoke. The gain fixture's two TOML sine hints remain explicit runtime data owners,
because filesystem reads are outside Vitest's import discovery. Deleted digikit scenarios still
select the whole Python suite; historical ownership recovery is deferred. These are static
selection changes, with no measured execution speedup or new device proof.


Validation for this routing checkpoint: 648 Python cases and 73 host cases passed across the
final preflight and the single stale-expectation retry. Lint, formatting, layers, generated checks
and workspace TypeScript types passed; mypy was skipped. The overall preflight is not a clean
pass: pathcheck retains three pre-existing references in `docs/plans/module-architecture.md`
(lines 3, 4 and 284), unchanged from `origin/main`. The new adapter references were resolved by
staging the file, and scoped pathcheck then reported zero stale references.

The 15-case static comparison selects FX helper files 13→3 and remote helper files 13→1;
`live.ts` and `runtime-plan.ts` remain 13→13. Production and configuration selections remain
unchanged, apart from correcting `live.ts`'s shared module lifecycle coverage. Replaying PR #244
selects runtime files 13→3 with all 37 Python plan entries retained; its harness change still
requires smoke. The final gate explanation selects host, three stock files, 13 runtime files,
three module files, Python checks and smoke. That gate was not run. No measured speedup or new
device proof is claimed.


### FX route migration checkpoint (2026-11-01)

PR #245 merged at `dd542a11` under Em's explicit focused-acceptance exception, with three prior
baseline path errors retained. That decision was not a full-gate pass and does not transfer to
this batch.

The route port covers all eight `fxroutes` and eleven `fxroute` legacy legs per device: 19 independent
baselines across four owned fixture contexts (panel, persistence, no-hook reference and hooked
track cases). Each context has one exact prepared image, symbol plan and startup-to-close write-watch
policy. The three image variants remain `RT + dynfx`, that set plus `fxpages`, and that set plus
`fxpages + digikit-menu`. Independent cases use pristine cards; the two menu-driven route changes
retain real guest restarts and before/after persistence evidence. Different images do not share
allowed-PC sets. The hooked track context runs its ten legs sequentially, retaining the no-hook
reference in its own process and preserving the final four-overlay project-lock comparison.

Independent review of track port `cbe4566a` found no missing assertions. Fixes preserve paused
protocol queries through the existing stepped USB transport (`fdb745ad`), initialize PCM observation
before route inspection (`a0f52563`), and use the existing epoch/UI restart barrier (`761aebba`).
Prepared loads retain the stock bytes needed to compute the DSP difference while still forcing
firmware reconstruction with the explicit clean card (`f296f998`). Artifacts were repacked through
the standard current-source SDK 1.2 wrapper after a shared-binary mismatch. Gate preparation/routing
checkpoint `440f676a` is pushed. Restart inspection now receives the original captured epoch, while
fresh baselines remain strict; only the combined ten-leg test has a 600-second wall budget
(`9f0cad73`). Guest timing, note margins, PCM tolerances and assertions remain unchanged.

All six device/family proofs passed:

| Device / family | Wall duration | Artifact suffixes | Lifetime safety |
| --- | --- | --- | --- |
| DN2 panel | 151.99 s invocation / 146.644 s case | `rj0mZ0` | 11 watch windows, 5 overlays passed |
| DN2 persistence | 121.88 s invocation / 115.753 s case | `cCWGMv` | 11 watch windows, 8 overlays passed |
| DT2 panel | 179.062 s file / 173.855 s case | `XQjKMS` | 11 watch windows, 5 overlays passed |
| DN2 track | 428.32 s invocation; 16.751 s reference + 401.264 s combined case | reference `8hTbj1`, hooked `dJ0EdG` | reference 3 windows / 1 overlay; hooked 15 windows / 13 overlays passed |
| DT2 persistence | 151.408 s file / 146.082 s case | `lX6mnZ` | 11 watch windows, 8 overlays passed |
| DT2 track | 360.238 s file; 24.078 s reference + 324.879 s combined case | reference `fgYuXW`, hooked `rGPXvd` | reference 3 windows / 1 overlay; hooked 15 windows / 13 overlays passed |

The final DT2 persistence/track invocation took 511.90 s. All safety reports passed with zero
write/flush hits. Watch windows are emulator summaries within the
owned fixture lifecycle, not a count of operating-system processes. The DN2 panel load report confirms
DSP application at cold boot. These durations establish correctness only: no paired A/B or measured
speedup claim is made.

Reviewed legacy retirement `eaf709b9` is integrated and pushed as `86e1a7ae` (23 files, +49/-1400;
349 tests and 15 subtests reported for the retirement work). The legacy route device scenarios
are removed; the independent analysis oracles remain. No device execution changed after the final
proofs.

Final preflight exited 1 only because pathcheck found three pre-existing references in
[module-architecture.md](module-architecture.md), at lines 3, 4 and 284; the file is unchanged from
`origin/main`. This is not a fully green preflight. All 666 Python cases (270 + 57 + 339) and
95 host cases (4 + 91) passed. Ruff check/format, mypy on two sources, layer/generated checks,
oxlint, Prettier and workspace types passed. Scoped pathchecks for the changed docs also passed.

The gate explanation selected host, three stock files, 16 runtime files, three module files,
54 Python files, smoke and native prerequisites. The broad routed gate was not run. PR #246 merged
at `e8c0e8ace2572f9a43a1d58f1f4c5343c7db3cde` on 2026-11-01T19:00:08Z under Em's explicit
PR-246-only focused exception, using the both-device and scoped validation above. This is not a
broad-gate pass, and the exception does not transfer to the next batch.

### BASIC FX and FUNC + FX pages checkpoint (2026-11-02)

The approved next batch ports the remaining BASIC chain and FXPAGE assertions together. Shared
`fx-session.ts` accepts explicit image roots, inventory, required symbols and safety policy; these
ports choose `default` explicitly. Plain/reference images remain independent, while hooked legs
run sequentially in kept contexts with pristine baseline cards. Fresh readiness remains strict;
only loader result words are interpreted as signed int32, including expected `-34`. The generic
extraction preserves the existing route configurations and their route-specific policy.
Legacy note margins, guest timing, PCM tolerances and audibility thresholds remain in their owners;
FXPAGE relative PCM oracles retain audibility responsibility without relaxing finite/safety checks.

| Legacy assertion group | Committed replacement ledger |
| --- | --- |
| BASIC capacity, selected type, free/live rows, overflow refusal and unchanged serial-gain PCM | [fx-chain.device.test.ts](../../modules/core/adapters/dynfx/tests/fx-chain.device.test.ts), with separate plain reference and sequential dry/full baselines |
| FXPAGE stock hashes, free-index poke invariance, page/index metadata, GAIN/MIX and A–E controls, SELECT/restart removal | [fx-pages.device.test.ts](../../modules/core/adapters/fxpages/tests/fx-pages.device.test.ts), with independent reference and sequential stock/effect/deselect context |

Both ports and the reviewed parity fixes passed their four family/device proofs. DN2 BASIC took
58.75 s and DN2 FXPAGE 293.71 s. The combined DT2 invocation took 201.23 s, with BASIC 76.193 s
and FXPAGE 124.770 s per file. These are correctness durations, not a speedup comparison.
On each device BASIC passed 3 + 5 lifetime watch windows and 1 + 2 overlays; FXPAGE passed
3 + 7 windows and 1 + 4 overlays. All eight lifetime-safety reports passed the default policy,
with zero write/flush hits and no invalid sectors.

The four legacy wrappers and four scenario TOMLs are retired after those proofs:
`REF:tests/digikit/test_fx_dt2.py`, `REF:tests/digikit/test_fx_dn2.py`,
`REF:tests/digikit/test_fxpage_dt2.py`, `REF:tests/digikit/test_fxpage_dn2.py`,
`REF:tests/digikit/scenarios/fx-dyn-dt2.toml`, `REF:tests/digikit/scenarios/fx-dyn-dn2.toml`,
`REF:tests/digikit/scenarios/fx-pages-dt2.toml`, and `REF:tests/digikit/scenarios/fx-pages-dn2.toml`.
The Python kind dispatch and runner bodies are removed. Shared row/PCM oracles and profile,
kit, screenshot and knob utilities remain for `fxpersist`, `fxshift` and the TypeScript suites.
Gain aliases 2/3/4 are prepared only for the complete BASIC inventory, and sine selection now
uses the same numeric 7/5 defaults without reading the retired TOMLs. Retirement `bb3972ee`
is committed (21 files, +61/-737, eight deleted). Focused checks passed 14 offline tests plus
15 subtests, 14 routing tests and 12 changed-test cases; independent review reported no issues.

The final required `test:changed` invocation exited 1 on three baseline path errors in
[module-architecture.md](module-architecture.md) (lines 3, 4 and 284, unchanged from `origin/main`)
and two stale expected-list failures. Commit `0adc052d` corrected those two expectations, and both
retry cases passed. Across the run/retry, 674 Python cases passed (270 budget + 57 changed +
345 verify + two retries), along with 99 host cases. Ruff check/format, mypy on two sources,
layers/generated checks, oxlint, Prettier and workspace types passed. Scoped changed-doc
pathchecks passed. The original preflight was not a fully green run.

The authoritative gate explanation uses `origin/main` at `e8c0e8ac` and selects eight FX runtime
files, 41 Python files, host checks, smoke and native prerequisites. The initial `--base main`
explanation used a stale local ref and is superseded. The broad gate was not run.
[PR #247](https://github.com/ElekLabs/digikit/pull/247) merged at `4bd50f5b` on
2026-11-01T19:45:12Z under Em's PR-247-only focused exception, accepting the three baseline path
errors and the focused evidence. This was not a broad-gate pass; the exception does not transfer.
Timings remain correctness evidence only, without a paired speedup claim.

Deleted-scenario routing remains deferred because path-only callers need merge-base ownership
evidence. No cache or tombstone tooling is introduced by this batch.

### FX kit persistence and frequency shifter design (2026-11-02)

The approved remaining FX batch keeps one owned context per device and family, with pristine
cards and explicit baseline reloads for independent legs. DT2 persistence uses the full real
SELECT/restart flow, which subsumes the quick table-poke setup; DN2 retains its existing quick
coverage. Preserve every unique kit-storage, page/row, PCM, project and restart oracle.
Frequency-shifter dry, master, send and other-track legs run on both devices: the DN2 TOML header
is stale, while the existing runner invokes those legs unconditionally. Preserve its independent
PCM/model, parameter, send and settings observations without changing thresholds or guest timing.

The shared prerequisite is the fixed `fx-kit` card-safety policy: exactly the existing per-device
`KIT_AREAS` firmware ranges, with no settings-sector allowance. Default and `fx-routes` policies
retain their existing behavior; callers cannot supply arbitrary allowlists.

The policy prerequisite `9b23b0ff`, kit/UI helpers `6f12384f`, persistence/shifter ports
`73711a01` / `01594832` and gate ownership `8b7c248e` are pushed. Source reviews found no issues.
DN2 persistence passed in 129.50 s invocation / 123.760 s case (`zDOVuw`), with three watch windows
and two overlays under `fx-kit`. DN2 shifter passed in 215.72 s / 209.578 s (`WDkalo`), with nine
windows and four overlays under `fx-routes`. Both lifetime reports had zero write/flush hits.
DT2 shifter passed in 398.171 s file / 375.478 s case (`acXXAd`), and DT2 persistence in
328.322 s / 320.201 s (`86nKeR`), with a combined invocation of 726.77 s. DT2 persistence passed
seven watch windows/four overlays under `fx-kit`; DN2 persistence passed three/two. Both shifter
reports passed nine/four under `fx-routes`. All four lifetime reports passed with zero write/flush
hits and empty invalid-store/low-sector lists. These timings are correctness evidence, not a
performance comparison; no new gate exception exists.

The Nx audit confirms safe use of the five current cached host targets and partial adoption.
Caching and affected-test routing are separate concerns. A next family-project migration remains
unimplemented and needs an exact folder layout, validated project graph and selection matrix;
see the [Nx audit checkpoint](nx-pilot.md#family-project-audit-checkpoint-2026-11-02). No custom
plugin, cache expansion or selector deletion is authorized by this audit alone.

The proved migration retires the four fxpersist/fxshift wrappers, three persistence scenario TOMLs
(including full DT2 restart) and two module-owned shifter scenario TOMLs. Historical inputs:
`REF:tests/digikit/test_fxpersist_dt2.py`, `REF:tests/digikit/test_fxpersist_dn2.py`,
`REF:tests/digikit/test_fxshift_dt2.py`, `REF:tests/digikit/test_fxshift_dn2.py`,
`REF:tests/digikit/scenarios/fx-persist-dt2.toml`, `REF:tests/digikit/scenarios/fx-persist-dn2.toml`,
`REF:tests/digikit/scenarios/fx-persist-restart-dt2.toml`,
`REF:modules/fx/freqshift/tests/scenarios/fx-freqshift-dt2.toml`, and
`REF:modules/fx/freqshift/tests/scenarios/fx-freqshift-dn2.toml`.
The persistence runner module is deleted after moving its exact kit ranges into arena safety.
FXPAGE retains only its PCM CLI; BASIC retains range/fit/peak analysis; the shifter retains its
unchanged spectral/model PCM oracles with an explicit model argument. Module unit, reference and
conformance tests remain. Source ownership follows the reduced imports, with pack/aliases,
production module coverage and conservative unknown/deleted-source fallbacks retained. Final
preflight and gate acceptance remain pending; no device execution changed after the final proof.

The frequency-shifter module's own `project.json` declares its uncached Nx `test-device` target,
reusing the existing named verify selection/preparation workflow. `module:check` runs this target
after Python tests (also for an owned target without Python tests), propagates failures and reports
size/fit only after both pass. Both-device daemon/ready resolution uses the existing harness and
overrides, with a shared lock around resolution only. The parent module-tests project's dependency
retains prior host ownership after introducing the nested project. Conservative bounded target
source inputs cover acceptance, preparation and model code; this is not the proposed family-wide
routing replacement or a cache-closure claim. The command's device acceptance is preserved while
retiring its old TOML discovery path; no second Python module-ownership registry is introduced.


### PR #248 continuation checkpoint (2026-11-02)

[PR #248](https://github.com/ElekLabs/digikit/pull/248) is draft, with code pushed through
`942cae59`. Retirement `ec112ced` removes ten legacy files; module-local placement `501f0482`
keeps the frequency-shifter scenario, analyzer tests and real-module routing assertions together
under its module. The four device proofs above stand unchanged; the moved scenario differs only
in imports and the analyzer tests are byte-identical. Shifter screenshots are captured evidence,
not pixel-equality assertions; its four legs are dry, master, send and other-track send.

Parent preflight passed 680 Python cases, 336 host cases (one skipped), all types/lint/format,
layer and generated checks. Pathcheck alone failed; all new references were corrected and scoped
pathcheck passed, leaving three baseline reports in the separate module-architecture plan.
Focused metadata/routing/collection checks and independent review passed. The broad routed gate
is unrun; no exception from an earlier PR transfers. Exact evidence, inputs and next actions are in
[the handover](../handovers/HANDOVER-2026-11-02-test-consolidation.md).


### PR #248 acceptance decision (2026-11-02)

Em explicitly approved merging PR #248 using the recorded focused checks, accepting the three
baseline pathcheck reports. The full routed gate remains unrun; this is a PR-248-only exception,
not a full-gate pass or permission to skip later gates. The PR and external testing roadmap
record the final merge state/commit; earlier draft/pending statements above are superseded.
