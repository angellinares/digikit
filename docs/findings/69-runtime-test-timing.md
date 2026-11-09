# Runtime acceptance phase timing, 2026-10-27

**[D] [C]** The October 26 gate took 1,531 seconds; runtime workflows took
1,244 seconds. Its file timings identify remote-input (560 seconds),
input-adapter (396 seconds), and transfer-live (122 seconds) as the largest
runtime files. Those old logs do not isolate boot time. The earlier claim
that repeated boots account for the long runtime suite was a hypothesis,
not measured attribution.

## Instrumentation

Runtime tests now emit `runtime-phase` JSON records through
`tests/regression/timing.ts`. Records carry a scenario scope, device, unique
session and span IDs, parent span, monotonic wall duration, and success or
failure. Output errors do not replace operation results or errors.

Phases cover aggregate session startup, boot validation, scenario body,
settling/input waits, module upload/install, protocol restart and boot wait,
CLI commands, and shutdown. Module acceptance uses the same scope context.
Durations are inclusive: do not add a parent body/install duration to its
children. CLI install/restart remains an aggregate operation. Startup includes
input resolution, daemon launch, card/firmware preparation and loading; it
is not a pure firmware-boot or DSP measurement.

No waits, timeouts, restart counts, session reuse, test assertions, emulator
defaults, or Rust code were changed. Fresh boots and persistence checks
remain independent.

## Bounded pilot

One DT2 OS 1.17 `service-commands` case passed with the previously verified
daemon and prepared `svc-echo` module. The DN2 case was filtered out. The
run held the exclusive benchmark lock, used normal fast-region configuration,
and performed no Rust build. It is one observation, not a speedup comparison
or an estimate of the whole suite's distribution.

| Phase | Seconds | Relationship |
| --- | ---: | --- |
| Session startup | 30.219 | Separate setup phase |
| Boot validation | 0.040 | Separate validation phase |
| Scenario body | 25.363 | Includes the following nested phases |
| Fixed settle | 12.001 | Child of body |
| Module install | 13.256 | Child of body |
| Module upload | 0.052 | Child of install |
| Restart command | 0.009 | Child of install |
| Wait for boot | 13.157 | Child of install |
| Shutdown | 0.252 | Separate teardown phase |

The whole invocation took 56.519 seconds. Records correlate to one session;
all phases succeeded. Source artifacts are local under
`out/native/runs/20261027-runtime-phase-timing/`: `invocation.json`, `pilot.log`,
`phases.json`, and `validated-summary.json`. The invocation pins the daemon
and prepared module directory from the prior gate.

## Validation and next decision

The changed-file preflight passed, including 38 related host tests, formatting,
lint and workspace types. Five timing-wrapper tests cover nested/concurrent
attribution, returned values, synchronous/asynchronous errors and logging
failure. Independent review found a module-context gap, which was fixed and
re-reviewed; regression types and scoped lint/format passed afterward.
The live pilot checks actual phase output. The full routed gate has not been
rerun for this instrumentation-only change; it is still required before merge.

The next step after the pilot was to profile remote-input and input-adapter
with these records (results below). If their
fixed waits dominate, investigate readiness predicates before shortening
them. If startup dominates, split `Session.start` into measured preparation,
launch and firmware-load phases before considering snapshot reuse. Preserve
the identical fresh-boot comparison and restart/persistence coverage. This
change adds evidence; it does not yet reduce test execution time.

## Remote-input and input-adapter profile, October 27

**[D]** Both files passed all six tests using the previous pinned daemons,
the same prepared modules, normal pre-activation defaults and an exclusive
benchmark lock. Eight independent sessions emitted 242 successful phase
records. Total invocation time was 909.588 seconds.

| Device / session | Startup s | Body s | Fixed waits within body s | Boot wait within body s |
| --- | ---: | ---: | ---: | ---: |
| DT2 remote keys | 29.722 | 75.096 | 53.019 | 13.315 |
| DN2 remote keys | 6.034 | 108.492 | 86.021 | 11.111 |
| DT2 remote switch | 30.162 | 111.517 | 82.642 | 13.158 |
| DN2 remote switch | 5.997 | 144.264 | 115.644 | 11.102 |
| DT2 adapter panel | 29.745 | 64.196 | 45.016 | 13.035 |
| DT2 adapter probe | 30.339 | 64.489 | 45.017 | 13.224 |
| DN2 adapter panel | 6.101 | 96.802 | 78.016 | 11.091 |
| DN2 adapter probe | 6.016 | 97.817 | 78.018 | 11.073 |

Fixed waits (`settle.*` and `input.hold`, sequential in these cases) total
583.393 seconds, about 64.1% of the whole invocation. Aggregate startup totals
144.117 seconds and post-restart boot waits total 97.109 seconds. Body time
already includes fixed waits and boot waits; these columns must not all be
added together. The remaining time includes preparation, RPC/assertion work,
shutdown and runner overhead.

This supports investigating fixed waits first, rather than attributing most
suite cost to repeated boots. It does not establish that every wait is
removable: input hold/pacing, UI readiness and stable boot-history conditions
must remain equivalent. No waits or isolation rules were shortened.

Artifacts: `out/native/runs/20261027-runtime-input-timing/` contains
`invocation.json`, `baseline.log`, `baseline-phases.json` and
`baseline-summary.json`. The source was `f6db6a56`; the daemons were the pinned
October 26 gate binaries. This is an attribution run, not a paired claim of
test-speed improvement.

## Bounded readiness replacement (2026-11-01)

**[D]** A bounded batch now replaces selected waits in remote-input and input-adapter. Readiness
requires valid normal boot fields and the actual runtime UI hook, plus free-running daemon/main-UI
state; post-install observations require a newer epoch. Fresh empty boots explicitly assert zero
selected modules and a zero counter snapshot, allowing one controlled install restart without a
stability wait. No persisted counter update is inferred from the in-memory stability flag. Remote
command readiness and complete ABOUT rows replace only their corresponding fixed waits. ABOUT must
start with an empty buffer and finish with the runtime, BOOT and final RAM row. Individual awaited
readiness reads use the same absolute observation deadline, so an unanswered RPC cannot hang the
poll or prevent diagnostic output. Input holds and release behavior retain their existing contract.

The first DT2 remote-UI proof failed on `cf.ready=false`, with a newer epoch, valid normal boot/UI
fields and `filesystem_verified=null`. Review confirmed that restart preserves the card and relaxes
the DT2 filesystem-worker check. Initial readiness still requires `cf.ready`; after restart the
current epoch/boot/UI guards suffice unless filesystem verification explicitly reports failure.
Null means unobserved, not verified. With that correction, all three selected cases passed on each
device: DN2 OS 1.12 in 203.62 s, DT2 OS 1.17 in 278.20 s. Each run passed two files and skipped the
other device's three cases. Local logs: `/private/tmp/runtime-readiness-dn2.log` and
`/private/tmp/runtime-readiness-dt2-retry.log`.

The earlier host preflight passed 49 cases and static/type checks; after the DT2 correction the
25 helper tests and regression types passed. Independent review reported no issues. The static
removed-wait ceiling is 269 s across eight sessions: 171 s startup, 80 s post-install and 18 s ABOUT.
These focused invocation durations are observations, not a paired speedup measurement. The October
profile above is historical evidence, not an A/B baseline for this batch.

Protocol-only startup, intermediate navigation settles, key holds, remote TURN refill pacing,
faulttrace waits and global settle helpers remain unchanged. The broad gate, including host,
runtime-fixture across all ten runtime files and smoke, has not run. Runtime merge acceptance remains
pending; prior routing-only exceptions do not apply.

**[O]** Consider extending the existing input-probe/debug facility to subscribe to proper completion
hooks and buffer events tagged with boot ID and sequence. Scenarios could wait for completion, then
assert the outcome, while retaining uninstrumented coverage. This is a future design consideration;
no hook/event implementation is authorized or included in this batch.

## Gate-selection fan-out (2026-10-27)

**[D]** The SHARC/workbench branch's merge gate selected the long Python debug,
FX-route and frequency-shifter scenarios through transitive Python imports.
`tools/devtools/changed_tests.py` (`gate_tests` / `deep_tests`) follows every
import chain, not which changed functions a test executes. Confirmed examples:

- `tools.emulator.native_emu` ← `tools.patchkit.pk_harness` ←
  `tools.patchkit.pk_digikit_scen` ← `tests.digikit.scen_cases` ←
  `tests.digikit.test_debug_dn2` (also `test_fxshift_dt2`).
- `tools.emulator.digi_env` ← `tools.patchkit.pk_dkrt` ←
  `tests.digikit.scen_cases` ← those same tests.
- `tools.emulator.coupled` reaches them through `tools.patchkit.hook_ab` and
  `tools.patchkit.pk_dkrt_machine`; `native_pgo` reaches them through `native_emu`.

This explains selection; it does not prove these scenarios are dispensable for
this branch, which also changes SHARC memory/compiler behavior. The normal
routed merge gate remains required before merge, not before opening a PR.

**[D]** Implemented a bounded routing contract for the four exact launcher
modules above: retain all direct/test-helper importers and other deep consumers,
but replace their deep legacy `tests/digikit.scen_cases` scenario fan-out with
focused owner/consumer tests and the existing full cross-device smoke matrix.
This is a coverage policy, not proof that those scenarios cannot be affected.
Filtering is per changed source; changing a runtime, harness or dispatcher
helper alongside a launcher restores its scenario consumers. Missing scenario
helper metadata and neighboring modules keep conservative selection.

Routing regression tests cover mixed changes, explicit tests, full mode, missing
helper metadata, benchmark/matrix tooling and preserved SHARC acceptance steps.
The worker's 259 focused tests passed; an added matrix-tool case then passed in
the 44-test routing suite. Independent review found no issues. The raw import
graph union for the four launchers falls from 71 to 44 files (27 legacy scenarios
removed). The actual branch gate plan falls from 72 to 49 Python files; these
counts include other changed sources and explicit suites. All native SHARC gate
steps and the full smoke matrix remain selected. This is not a measured wall-time
speedup. Plans are saved under `out/native/runs/20261027-sharc-dual-default/`
and `out/native/runs/20261027-sharc-routing/`.

**[O]** Attribute selected tests to complete import chains and estimated cost in
explain output. Consider separating desktop launch policy from shared build/input
helpers. Handle execution-cost reductions (readiness waits, boot reuse, duplicate
proofs) separately from selection correctness.
