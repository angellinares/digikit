# Handover 2026-11-17 evening: R12 batch nearly through, B1 / RS-S / RS-C stacks built, test pyramid work

Start from docs/plans/sdk-runtime-roadmap.md, then this file. Plans: docs/plans/runtime-slimming.md (RS-S, RS-C, Fixtures), docs/plans/digikit-hot-reload.md (B1 design, Fixtures, S0-S5), docs/plans/test-architecture.md (T2). AGENTS.md has the rules added today (PCM pins from a signal report; DRY fixtures).

## Merged to main today

| PR | What |
| --- | --- |
| 562 | runtime slimming plan; B1 design in the hot reload plan; roadmap rows RS-S, RS-C |
| 563 | AGENTS.md / TESTING.md: a PCM pin is set from a numeric signal report, not a hash alone; Claude approves when it matches the fixture |
| 564 | plan Fixtures sections; AGENTS.md DRY fixture rule; roadmap note T2 fixture consolidation |
| 565 | one device gate for B1 + RS-S + RS-C; estimates in hours |

## Branches and lanes (nothing pushed unless noted)

| Branch | Lane | Head | State |
| --- | --- | --- | --- |
| `rt/r12-batch` | sdk-design | 908041e3d | R12 batch: R12 prep, R12-5 fx.dks, R12-6 machines.dks, T2 lanes, assembly layer, today's fixes (fill `devices`, `service!` exports + host declare-check case, `DKRT_PROVIDERS` published to hosts, harness provider/declare output, tap tracking, fx symbols only when needed, commit_records load state), machine fixture tests (patch-time machine path deleted), templates and fixture table refresh, fixture consolidation (patch-time fx path deleted, 10 fixtures merged/removed, dyn-crash fault injector), parked multi-page machines, module:check outputs (dyn-dn2 DT2, bd gallery), batch review fixes (a40562747), lfo/capacity/machine-rows/fx-bipolar/inferred-needs test fixes. Host-green |
| `b1/bind-resolver` | (sdk-gen, not checked out) | b642fca98 | B1 done on the host: bind resolver (dk_fw_/dk_exit_/dk_tail_/dk_ret_/dk_stock_), FW identity item + loader check (E_HOOK_STOCK), hook exits converted, machines_info v2 (16 words), fxpages_info v4 (firmware words zeroed), docs. Reviewed: approve with fixes — restore the zero-address guard in the firmware call wrapper (DkFw), two stale lines in runtime-and-services.md |
| `rs/core-leftovers` | sdk-gen | 24fd32a3b | RS-C on top of B1: limits raised (imports 128, exports 64, Declare 64), golden lowered records, C1 lifecycle hooks, C2(a) params crate, FwMem memory interface (services host-testable). Next: C2 (b) registry statics, (c) assign + settle hook (sticky map to a machines settings key, migration read), (d) native (guard zero words), (e) collect builder state + exports + module_commit; then C3 CoreApi split (+ DKRT_REG test-support resolver), C4 layer rule. Decisions taken: one record block per module, two-pass module_commit, static builder state per .dks, per-service first error. Known: `the_builder_calls_...` test red until (e); TABLE_MAX 640 to measure |
| `rs/storage-tests` | docs-plans | bd084608c | RS-S done on the host: storage/ moves, layout/lock/card split (cycles cut), route.rs one decision, fwfs + plusdrive backends, one lister rule (Filter::Modules / Filter::All), dk_files_read on read_all (E_TOO_BIG / E_SEL_PATH documented), CoreApi module_of/storage_stat/storage_stat_head/irq_mask/irq_restore, sample_pool_info + pool event slots, samples.dks (STATIC = 0), track-select own pool, assembly cases. Reviewed: approve with fixes (below). worker.rs stays in the core until B1 is under it; ORDER still present |
| `test/fingerprint-pins` | docs-handover | 73903bd81 | T2 fingerprint pins written, unverified: pcmpin.ts (v2 fingerprint, tolerant comparer, report), pin CLI, sine property checks; exact pins: equiv-dn2-set and the two freqshift pins. Phase 2: pnpm install, typecheck/host tests, rig pin regeneration, device pins from the next run's kept captures |
| `test/unit-gaps` | t1-tests | 4393ffc71 | ~40 host tests written, unverified (boot driver, planner, loader refusals, discover, patchkit, fx record checks, machines guards). Phase 2: compile; confirm and fix two suspected bugs as separate commits: `report_dsp` writes a result after a module is active; module files skip `role_ok` (a content module claiming PROVIDES/HOOKS loads) |
| `fix/batch-review` | sdk-fixtures | a40562747 | merged into the batch |
| `tmp/coverage-on-batch`, `test/coverage-report` | sdk-fixtures | | coverage tool parked (not merged); the tmp branch is throwaway |

Backup tags `backup/pre-target-rewrite/*` and `refs/original/*` hold the pre-rewrite commits (see "Done today"). Drop them after the batch merges: `git tag -l 'backup/pre-target-rewrite/*' | xargs git tag -d; git for-each-ref --format='%(refname)' refs/original | xargs -n1 git update-ref -d; git gc --prune=now`.

## Device re-run results (batch 908041e3d; logs out-rerun-1..3.log in lane sdk-design)

Passing now: frametype-slot (DN2), fx-contract (DT2, DN2), fx-send (DT2, DN2), fx-reuse, fx-pages (DT2), inferred-needs full tier (DT2, DN2, 8 cases), and earlier in the day lfo, modules-capacity, dynreg equiv and round trip, bootscreen x3, two-notes, routes, track-route files, fixtures, providers, refusals, safe-mode, boot-restart, crash-capture, fault-trace.

Still failing (work these first, one file at a time, `pnpm nx:pilot run <project>:test-device--<file>--<dev>`):
- machine-rows-dn2: the first knob turn changes no Sound word within 3 s, although the DN2 diagnostic run with the same 30 ms timing changed word 26 (8192 -> 8448). Compare the test's steps before the turn with the diagnostic (in git history of this session: the diagnostic did pickSine(5), TRIG 1, SYN, then three 4-detent ENC B events 30 ms apart and a 100 ms settle).
- machine-rows-dt2: "the stock SRC, FLTR and AMP pages show the same frames with machines.dks loaded" fails: the frames differ. Either the machines.dks hooks change stock pages on DT2 (a real finding) or the capture differs between the two boots; look at the saved frames.
- fx-bipolar: DT2 the dry/wet audio compare (`hook_ab compare`) fails; DN2 a gain value 1064956904 (0.9976) where 1065353216 (1.0) was expected. Real numeric change or tolerance.
- fx-pages (DN2): "track send note setup too late: current frame 7486, trigger 7000" (setup timing).
- fx-kit-persist (DT2: `symbol fxp_nload: 0 addresses in the plan`, one more plan lookup of an fx service symbol; DN2: a hook counter 0 where >= 1 was expected).
- overdrive-type did not run (wrong target name in the run); run `core/overdrive`'s device target.

## In flight at the end of this session

- The device re-run finished; results above.
- Agents paused or done: RS-C waits for "continue (b)-(e)"; RS-S, fingerprint pins, unit-gaps wait for their next step.

## Next, in order

1. Batch: read the re-run; apply the review-fix follow-ups below; then the merge proof `mise run test:subset -- <tags> --run` (machines, fx, loading, services, storage, plus gate-smoke and gate-pins) on DT2 and DN2; merge the batch to main in PRs in order (prep, R12-5, R12-6, T2 lanes, assembly layer, fixtures, consolidation), roadmap rows in the same PRs; then PGO retrain, `test:full` once.
2. Review-fix follow-ups (batch): `evidence` throws "requires audio" only when a suite explicitly sets requireAudible, and runs `expectNoFault` first; `expectNoPages` takes an explicit "service expected" from the caller and asserts the service is absent when not expected; `service-check` hard-errors on a missing adapters folder, a slot declared twice, an unparsable dks manifest; document the 4-character provider name in the log reader.
3. RS-S review follow-ups: assembly case "samples.dks present, sample-pool adapter absent → refused with reason" and fix the glue.rs comment; remove the NUL clause from the dk_files_read doc; docs line for the dkrt_project_changed removal; assembly `imports_resolve` tolerates adapter-service imports only when the image set has that adapter; delete the tautological sdk_files.rs constants test.
4. B1 fixes (zero-address guard, two doc lines). Then RS-C (b)-(e), C3, C4 with its reviewer. Rebase/merge RS-S and RS-C onto the merged batch; one shared device gate for B1 + RS-S + RS-C (FX contract suite, machine fixture suite, capacity commit, refusals, needs, providers, frametype slot, storage and samples device cases, shot, usb-fast upload DN2, gate-smoke, gate-pins; a dk:dev boot on both devices first).
5. Fingerprint pins and unit-gap tests: phase 2, reviewer, merge.
6. Then R12-8 SHARC stubs, P8d DSP stats window, P8c dispatcher spans, R14, H hot reload S1-S5.

## Decisions taken this session

- Multi-page run-time machines parked (runtime has one knob page per machine; collect.rs refuses a second); returns with RS-C; test/dyn-dn2-pages and machine-pages-dn2 removed; a host test pins the refusal.
- Patch-time fx path deleted and refused (`refuse_entries`, except core/dynfx reserved rows); fixtures DRY (one per feature).
- DN2 hardware image (`device` patch set) has no core/sample-pool (decision of 2026-11-01/02: SPI wire size, DDR read-back); sample modules are now refused there at boot with a clear reason instead of loading silent.
- Design direction for device capabilities: adapter publishes a versioned function table by name hash in dkrt_info_table; facades read it through one helper; absent table = capability absent (planner refuses). Storage device tables (dkrt_sample_fs, dkrt_files_fs) move to the core/platform adapter as tables (RS-S follow-up). Two facades stay separate: storage (files) and sample memory (playback).
- Coverage: the unified coverage layers and fixture surface coverage are parked; fixtures stay central inside roadmap items (each item adds or extends the canonical fixture of what it touches).
- Every finished agent work item gets an independent reviewer before merge.
- Estimates: rescale planner/architect day estimates to measured agent time (hours); wall time is device gates and reviews.

## Done today worth knowing

- History rewrite: commit a95654415 had committed cargo `target/` dirs (171 MiB) of the service crates; stripped from 8 local unpushed branches (main untouched; verified per branch); `target/` ignored in each dks crate.
- Runtime facts found: provider records were invisible to hosts (now DKRT_PROVIDERS); `service!` did not declare exports (E_DECL_KIND on machines.dks); DT2 machines.dks filled a DN2-only slot (fill `devices`); capacity counts each service file's arena (machines.dks 24,592 B, fx.dks 26,560 B).
- Knob turns on a run-time machine's own page work (DN2 diagnostic); the firmware encoder needs fast detents (12 detents ≈ one or two steps); the machine-row helpers use 4-detent events 30 ms apart.

## Working rules learned

- Lanes stay with the agent that owns them; do not switch a paused agent's lane to another branch.
- Generated files only from the generator (`patchkit gen`, mise gen tasks).
- An agent's `git add -A` in a crate dir can commit build output: check `git show --stat` of agent commits for `target/`.
- Device tests that install machine or effect modules pass `{ device, session }` so the needed service files reach the card.
