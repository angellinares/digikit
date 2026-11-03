# Ledger: debug, card, crash, safe and cut scenarios

One line per Python check: `scenario | file:line | what | -> TS file:test`, or `COVERED by <test>`, or `DROPPED/NOT PORTED: reason`.
D = `tools/patchkit/pk_digikit_debug.py`, S = `tools/patchkit/pk_digikit_scen.py`. TS files are in `modules/core/runtime/tests/` (`*.device.test.ts`: debug-guard, crash-capture, card-safety, safe-mode) and `modules/tools/debug/tests/debug-module.device.test.ts`; below they are named without the `.device` part.
Local helpers: `card-rig.ts` (`cardRigs`, `waitReset`), `debug-kit.ts` (`crashRecord`, `OPS`, `openAbout`).

card-safety and card-small: Python deleted; the store-range write check is ported to card-rig.ts (afterClose, uploads). `debug_on` is obsolete (the debug switch is gone); the saved-selection flag bit belongs to debug-log.

card-torn (`tests/digikit/scenarios/card-torn.toml`, added 2026-11-02; D `run_card_torn`):

card-torn | D:111 | torn_b: header B torn, A blank: card blank, upload E_NOSTORE and no writes, CLEAR recovers, next upload commits | -> card-safety.test.ts: card safety: torn sectors (both devices)
card-torn | D:136 | torn_a: header A torn, B whole: ours, upload commits | -> card-safety.test.ts: card safety: torn sectors (both devices)
card-torn | D:147 | torn_a_only: header A torn, B blank: foreign at sector 0, BEGIN and CLEAR E_INUSE, nothing written | -> card-safety.test.ts: card safety: torn sectors (both devices)
card-torn | D:173 | torn_sel: selection copy A torn: blank, next selection writes both copies | -> card-safety.test.ts: card safety: torn sectors (both devices)
card-torn | D:188 | stray_b: bytes in header B only, state reported | DROPPED: report data only, no check

Differences that apply to many lines:

- The Python scenarios run a `cf_only` daemon without DSP for most card and debug checks. The TS runtime sessions are always coupled (`dsp: 'sharc'`), so a boot is slower and has DSP events. No check depends on the missing DSP.
- A Python rig is one daemon per card. In TS one daemon per describe block boots one card image after the other (`fixture.reload`); each case still starts from a cold boot of its card.
- `common_checks` / `common_checks_upload`: write watch and flush check are asserted for every card block (`cardTests`, `afterClose`). The card policy (`runtimeSafety` `pass`) is asserted without `uploads`; with `uploads` (card-safety, card-small, debug-guard) the write watch is asserted and `outsideStore` must be empty (writes only in the store range or from 0x50000).

## debug-log (dt2, quick; D `run_debug` and `part_modules`; `periodic` is full tier)

debug-log | D:492 | without the debug module: no LOG/PEEK/LOGCFG capability, three commands E_PROTO | COVERED by debug-module.test.ts: without the debug module the runtime boots, has no log commands and keeps a small ring
debug-log | D:492 | without the debug module: the debug flag is off | NOT PORTED: `debug_on` is not in `DkInfoFull`; the debug switch is gone (LOGCFG op 2 is E_PROTO, covered)
debug-log | D:501 | small ring holds boot events, no periodic or module events | COVERED by debug-module.test.ts: without the debug module ...
debug-log | D:515 | LOG returns events in sequence from 1, block counter as time not going back | -> debug-module.test.ts: with the debug module ... (`expectLogShape`; sequence step by one was covered)
debug-log | D:527 | boot steps logged: BOOT_START, CARD_CHECK, RAM_KEEP, BOOT_RECORD, UI_FIRST | -> debug-module.test.ts: with the debug module ... (`expectLogShape`)
debug-log | D:527 | DSP_ABSENT when the link is absent | DROPPED: needs a daemon without DSP; the TS session is coupled and the image has dl_diag
debug-log | D:533 | BOOT_START carries runtime version and device id | -> debug-module.test.ts: with the debug module ... (`expectLogShape`)
debug-log | D:539 | INFO has LOG, PEEK, LOGCFG capabilities | COVERED by debug-module.test.ts: with the debug module ...
debug-log | D:539 | INFO: card not read-only | -> debug-module.test.ts: with the debug module ...
debug-log | D:539 | INFO: no crash found | NOT PORTED: `crash_found` is not in `DkInfoFull`; the log has no CRASH_FOUND (ported, `expectLogShape`) and crash-capture reads `DKRT_CRASH.found`
debug-log | D:550 | PEEK reads DKRT_STATE: magic and runtime version | -> debug-module.test.ts: with the debug module ... (`expectPeekRanges`)
debug-log | D:556 | PEEK of 1300 bytes in three requests | -> debug-module.test.ts: with the debug module ... (`expectPeekRanges`)
debug-log | D:565 | PEEK reads the firmware data range | -> debug-module.test.ts: with the debug module ... (`expectPeekRanges`; the range is read from the device file)
debug-log | D:577 | PEEK refuses register, below, past the end, gap, wrap, length 0 and over 512 (E_PEEK) | -> debug-module.test.ts: with the debug module ... (`expectPeekRanges`, one table; over 512 was covered)
debug-log | D:586 | LOGCFG get: default level 2, no module level, debug on | COVERED by debug-module.test.ts: with the debug module ...
debug-log | D:597 | LOGCFG level 0: info event dropped, error event kept | -> debug-module.test.ts: with the debug module ... (`expectLevelsFilterUploadEvents`)
debug-log | D:604 | LOGCFG back to 2: upload command logged again | -> debug-module.test.ts: with the debug module ... (`expectLevelsFilterUploadEvents`)
debug-log | D:613 | LOGCFG op 2 refused (E_PROTO) | COVERED by debug-module.test.ts: with the debug module ...
debug-log | D:613 | a saved selection has flag bit 1 clear | NOT PORTED: needs the card image path to read the record; a plain runtime session has no owned card image (DN2 has none at all). The card-kit `records()` works only in `cardTests`
debug-log | D:623 | dklog follow --once: every event as a readable line, >= 8 events, CARD_CHECK line | -> debug-module.test.ts: with the debug module ... (`expectLogShape`, `eventLine`)
debug-log | D:623 | dklog follow --once: JSON line per event, sorted seq | DROPPED: Python tool output format, checked by tests/digikit/test_dklog.py; seq order is covered
debug-log | D:631 | dklog crash: no crash record | -> debug-module.test.ts: with the debug module ... (`expectLogShape`)
debug-log | D:656 | dklog command line (follow, peek, crash) against the daemon | DROPPED: tests the Python CLI, not the device
debug-log | D:685 | INFO sets MODULES and CAPACITY capabilities | -> debug-module.test.ts: MODULES and CAPACITY list ... (`expectModulesReply`)
debug-log | D:706 | MODULES lists the loaded modules by record index with the file's version and contract bytes | -> debug-module.test.ts: MODULES and CAPACITY list ...
debug-log | D:719 | ColdFire ranges inside the arena, no overlap, free figure inside its size, largest <= free | -> debug-module.test.ts: MODULES and CAPACITY list ... (free and largest from CAPACITY: `DkSession.modules()` drops the raw `arena_free` fields)
debug-log | D:735 | DSP ranges inside the DSP area, free > 0, largest <= free, no overlap | -> debug-module.test.ts: MODULES and CAPACITY list ...
debug-log | D:752 | assigned types are the boot table's, one voice row each, types 16 and 17 | -> debug-module.test.ts: MODULES and CAPACITY list ... (types 16 and 17, one row bit each). NOT PORTED: the cross-check with `DKRT_ASSIGN` and the DSP voice registry (`dyn_type`, `voice_rows` are MISSING in TS)
debug-log | D:763 | no guard fault, guard has switched nothing off | -> debug-module.test.ts: MODULES and CAPACITY list ...
debug-log | D:792 | CAPACITY lists every kind with the device file's limit | -> debug-module.test.ts: MODULES and CAPACITY list ... for MODULES, ARENA, STAGE, DSP_AREA, MACHINE_ROWS, VOICE_ROWS, PARAM_ROWS, PAGE_ROWS, SETTINGS. NOT PORTED: SELECTION, FX_SLOTS, SEND_SLOTS, MAP_ENTRIES, STORE limits (need `contract_max`, `effective_*_slots` and `Store.limit` of pk_device)
debug-log | D:798 | CAPACITY use agrees with MODULES | -> debug-module.test.ts: MODULES and CAPACITY list ... (use counts; the debug module is a third module, so MODULES and SELECTION are 3). The arena and DSP use equalities are tautologies once free comes from CAPACITY: DROPPED
debug-log | D:815 | MODULES pages by start index | -> debug-module.test.ts: MODULES and CAPACITY list ... (from 1: two records, from 9: none, total 3)
debug-log | D:820 | MODULES is not logged as an UPLOAD event | -> debug-module.test.ts: MODULES and CAPACITY list ...
The DT2 periodic events (the four lines below) are ported and pass; the Python scenario debug-log is deleted.

debug-log | D:1233 | HEALTH snapshot every ten seconds (full) | -> debug-module.test.ts: the periodic events ... (`expectPeriodicEvents`)
debug-log | D:1242 | stable period logged once (full) | -> debug-module.test.ts: the periodic events ...
debug-log | D:1249 | DSP diagnostics quiet: eight events (32 words) a set, 3 to 5 sets, ten seconds apart (full) | -> debug-module.test.ts: the periodic events ...
debug-log | D:1259 | a running DSP is never reported stopped (full) | -> debug-module.test.ts: the periodic events ...
debug-log | D:1266 | without dl_diag the DSP poll logs DSP_ABSENT once | DROPPED: the image under test always has dl_diag

## debug-guard (dt2 and dn2, full; D `run_guard` and `run_guard_dn2`)

Ported and passing on both devices; the Python scenario debug-guard is deleted.

debug-guard | D:1118 | DT2: both modules and the debug module loaded, two voice rows | -> debug-guard.test.ts: the guard zeroes a faulting machine ... (boot results; the voice-row count is NOT PORTED: `voice_rows` MISSING)
debug-guard | D:1023 | DN2: fixture and debug module loaded, one voice row | -> debug-guard.test.ts: the guard zeroes ... (boot results; voice row count NOT PORTED as above)
debug-guard | D:1039 | DN2: machine list shows the fixture, track 1 takes its type (MACHINE_CHANGE) | -> debug-guard.test.ts: the guard zeroes ... (`pick`, `pickSine` from fx-live for the key script)
debug-guard | D:1131 | DT2: sine on track 1 and the fixture on track 2 by list position, retried at hint+1 and hint-1 | -> debug-guard.test.ts: the guard zeroes ... (`pick`)
debug-guard | D:1170 | DT2: MACHINE_FAULT logged as error event with the fixture's type, track 2, kind nonfinite | -> debug-guard.test.ts: the guard zeroes ...
debug-guard | D:1057 | DN2: MACHINE_FAULT error event, type, kind nonfinite, count >= 8 | -> debug-guard.test.ts: the guard zeroes ...
debug-guard | D:1180 | DT2: at least 8 faults counted | -> debug-guard.test.ts: the guard zeroes ...
debug-guard | D:1185 | MACHINE_DISABLED once for the fixture's type, error event | -> debug-guard.test.ts: the guard zeroes ...
debug-guard | D:1195 | a press after the switch-off causes no new fault | -> debug-guard.test.ts: the guard zeroes ...
debug-guard | D:1206 | master output finite, capture longer than 48000 samples (WAV via pcm.tap capture) | -> debug-guard.test.ts: the guard zeroes ... (`readFloatWav`)
debug-guard | D:1211 | DT2: the sine stays audible (first 11 windows of 4800 samples above -70 dBFS) | -> debug-guard.test.ts: the guard zeroes ... (`windowRms`; 4800 samples of the interleaved array as in Python)
debug-guard | D:1087 | DN2: MODULES shows the fixture's faults >= 8 and guard_off | -> debug-guard.test.ts: the guard zeroes ...
debug-guard | D:1151 | DT2: delay send of track 2 up (FX page, encoder E) | -> debug-guard.test.ts: the guard zeroes ... (setup step, no assertion)
debug-guard | D:1217 | follow --once writes guard.log | DROPPED: artifact of the Python tool, no assertion
debug-guard | D:1094 | common_checks_upload(project_writes_ok) | -> `cardTests` `afterClose` (write watch only, see above)

## crash-capture (dt2, full; D `run_crash`)

Ported and passing (DT2 only); the Python scenario crash-capture is deleted.

crash-capture | D:1340 | boot 1: no crash record, exception hook installed, RAM not kept, no record in log | -> crash-capture.test.ts: early/late
crash-capture | D:1351 | boot 2: record found, PC equals the log's, vector 4 (early) or 5 (late), module index 1 with a name, RAM kept | -> crash-capture.test.ts: early/late
crash-capture | D:1364 | boot 2: INFO says a crash was found | NOT PORTED: `crash_found` is not in `DkInfoFull`; `DKRT_CRASH.found == 1` is asserted instead
crash-capture | D:1364 | boot 2: 16 registers, 32 stack words, >= 2 tail events, A7 aligned | -> crash-capture.test.ts: early/late (`crashRecord`)
crash-capture | D:1380 | boot 3 (safe mode): same PC, tails kept, no LOG capability | -> crash-capture.test.ts: early/late
crash-capture | D:1393 | early: counters 0,1,2; boot 2 not safe with 2 loaded; stable 0; boot 3 safe (counter), none loaded | -> crash-capture.test.ts: early
crash-capture | D:1410 | late: counter 0,1,2; stable 1; boot 3 safe | -> crash-capture.test.ts: late
crash-capture | D:1432 | safe boot reached the UI loop and kept the selection | -> crash-capture.test.ts: early/late
crash-capture | D:1389 | crash text report | DROPPED: report artifact, no assertion
crash-capture | D:1299 | `deliver_exceptions` and `wait_reset` (soft reset keeping RAM) | -> `RuntimeBaseline.deliverExceptions` and `waitReset` in debug-kit.ts (stepping port of `Rig.wait_reset`, untested)

## card-safety (dt2, quick; D `run_card`) and card-small (dt2 and dn2, quick; D `run_card_small`)

card-safety | D:183 | blank card: check blank, nothing refused, claimed or written | -> card-safety.test.ts: a blank card works as before ...
card-safety | D:201 | blank: eight writes outside the range are E_RANGE | -> card-safety.test.ts: a blank card works as before ...
card-safety | D:207 | blank: refusals counted, nothing written | -> card-safety.test.ts: a blank card works as before ...
card-safety | D:218 | blank: selection written as two equal records, area ours, 2 writes | -> card-safety.test.ts: a blank card works as before ...
card-safety | D:229 | blank: log has the check (blank) and one error event per refused write | -> card-safety.test.ts: a blank card works as before ...
card-safety | D:244 | foreign: check says foreign, first non-blank sector and crc32 | -> card-safety.test.ts: foreign bytes in the range ...
card-safety | D:252 | foreign: the boot runs normally | -> card-safety.test.ts: foreign bytes in the range ...
card-safety | D:261 | foreign: selection and in-range write E_CARD_RO, out of range E_RANGE, 3 refused | -> card-safety.test.ts: foreign bytes in the range ...
card-safety | D:268 | foreign: no selection record written | -> card-safety.test.ts: foreign bytes in the range ...
card-safety | D:274 | foreign: INFO read-only, LIST empty | -> card-safety.test.ts: foreign bytes in the range ...  (`debug_on` is obsolete, see debug-log)
card-safety | D:286 | foreign: BEGIN, DELETE, CLEAR answer E_INUSE | -> card-safety.test.ts: foreign bytes in the range ...
card-safety | D:293 | foreign: log reports state, sector and hash as error event | -> card-safety.test.ts: foreign bytes in the range ...
card-safety | D:319 | foreign: ABOUT rows CARD READ-ONLY, NOT OURS AT n, NO BOOT COUNTER, each <= 21 characters | -> card-safety.test.ts: foreign bytes in the range ... (panel keys in a free-running session, `openAbout`)
card-safety | D:316 | foreign: about-foreign.png | DROPPED: artifact
card-safety | D:327 | foreign: nothing written in the store range | -> card-safety.test.ts: foreign bytes in the range ...
card-safety | D:341 | selection record only, blank headers: ours, upload works | -> card-safety.test.ts: a selection record only ...
card-safety | D:362 | selection record only, foreign headers: ours, BEGIN/DELETE/LIST E_NOSTORE | -> card-safety.test.ts: a selection record only ...
card-safety | D:367 | selection record only, foreign headers: nothing written | -> card-safety.test.ts: a selection record only ...
card-safety | D:377 | our store: ours without reading the range | -> card-safety.test.ts: a card with our store keeps working ...
card-safety | D:386 | our store: the selection loads | -> card-safety.test.ts: a card with our store keeps working ...
card-safety | D:397 | our store: selection and upload work, nothing refused | -> card-safety.test.ts: a card with our store keeps working ...
card-safety | D:416 | claim: the cut upload claimed the range, failed | -> card-safety.test.ts: an upload on a blank card claims the range ...
card-safety | D:431 | claim: next boot calls the area ours, empty store, upload works | -> card-safety.test.ts: an upload on a blank card claims the range ... (`h.after()` = `card_after`)
card-safety | D:439 | common_checks_upload(project_writes_ok) | -> `cardTests` `afterClose` (write watch and `outsideStore` empty)
card-small | D:92 | small card: too small, nothing refused, claimed or written | -> card-safety.test.ts: a card smaller than the store area ... (`DIGI_CARD_IDENTITY=small` via `cardTests` env)
card-small | D:99 | small card: the boot goes on | -> card-safety.test.ts: a card smaller ...
card-small | D:106 | small card: one error event for the check (state 5) | -> card-safety.test.ts: a card smaller ...
card-small | D:122 | small card: BEGIN, DELETE, MKDIR, CLEAR, LIST answer E_NOSTORE; INFO answers | -> card-safety.test.ts: a card smaller ...
card-small | D:128 | small card: CAPACITY STORE 0 | -> card-safety.test.ts: a card smaller ...
card-small | D:136 | small card: selection and write E_CARD_RO, no writes counted | -> card-safety.test.ts: a card smaller ...
card-small | D:141 | small card: nothing written in the store range | -> card-safety.test.ts: a card smaller ...
card-small | D:149 | write watch on cf.expansion and flush check | -> `cardTests` `afterClose`

## safe-fault, safe-late-fault (dt2 and dn2, full; S `run_safe`, `run_safe_late`)

Ported and passing on both devices; the Python scenarios safe-fault and safe-late-fault are deleted.

safe-fault | S:893 | two boots never reach the UI loop, the third is safe mode (counter 2) | -> safe-mode.test.ts: a module that resets the machine in its init ...
safe-fault | S:898 | safe mode loaded no module and reported E_SAFE_MODE | -> safe-mode.test.ts: a module that resets ... (loaded 0, first result -44)
safe-fault | S:898 | all voice rows free | NOT PORTED: `voice_rows` (DSP voice registry) is MISSING in TS
safe-fault | S:906 | the UI loop ran: counter 0, selection kept | -> safe-mode.test.ts: a module that resets ...
safe-fault | S:921 | safe-once flag: safe mode (flag), no module, flag cleared, selection kept | -> safe-mode.test.ts: the safe-once flag ...
safe-late-fault | S:818 | early: boots 1 and 2 load the module and the UI task hangs | -> safe-mode.test.ts: a module that hangs the UI task early ...
safe-late-fault | S:827 | early: counter raised at each boot start and not cleared | -> safe-mode.test.ts: ... early ...
safe-late-fault | S:837 | early: third boot is safe mode (counter), no module, UI keeps running | -> safe-mode.test.ts: ... early ...
safe-late-fault | S:845 | early: safe boot clears the counter, selection kept | -> safe-mode.test.ts: ... early ...
safe-late-fault | S:853 | late: every boot loads the module and hangs | -> safe-mode.test.ts: ... late ...
safe-late-fault | S:863 | late: counter cleared before the failure, every boot starts at 0 | -> safe-mode.test.ts: ... late ...
safe-late-fault | S:772 | fault-after.bin in the store (`cfg/`) | -> `h.cfg` and `buildCard` modules list

## cut-selection (dt2 and dn2, full; S `run_cut`)

Status: proven on DT2 and DN2 (full tier, 2026-11-03); Python scenario deleted.

cut-selection | S:959 | A new / B old, A torn / B old, A old / B new, A torn / B torn: valid copy with the higher seq | -> safe-mode.test.ts: power cut, A x / B y ... (four tests from one table)
cut-selection | S:947 | the quick tier runs the first two cases (`quick_cases`) | DROPPED: the scenario is full tier, so all four cases run
cut-selection | S:974 | one copy written by the runtime: A new, B empty | -> safe-mode.test.ts: one copy written by the runtime ...
cut-selection | S:984 | restart after the cut loads the new copy | -> safe-mode.test.ts: one copy written by the runtime ... (`dk.restart` + `waitForBoot`)

## sticky-types (dt2, quick; D `run_sticky`; Python deleted)

The assignment is now the pure function `dkrt_sdk::sticky::assign`, which the runtime's `assign::plan` calls. The 9 boots are unit tests in `modules/core/runtime/sdk/src/sticky/tests.rs` (run by `dkrt-sdk:rust-tests`). The device roundtrip (`modules/core/adapters/dynreg/tests/roundtrip.device.test.ts`) still covers the card and boot integration.

sticky-types | D:run_sticky boot 1 | fresh card, nan and sine: 16 and 17 in load order, rule 0, both entries written, dirty | -> sticky/tests.rs: fresh_card_gives_the_lowest_numbers_in_load_order_and_writes_the_map
sticky-types | D:run_sticky boot 2 | sine only: keeps its number, map unchanged, not rewritten | -> sticky/tests.rs: a_machine_keeps_its_number_when_other_modules_are_absent_and_nothing_is_rewritten
sticky-types | D:run_sticky boot 3 | sine and params: params takes 18, not the absent nan machine's 16 | -> sticky/tests.rs: a_new_machine_does_not_take_the_number_of_an_absent_machine
sticky-types | D:run_sticky boot 4 | same set again in another load order: every number as remembered, nothing rewritten | -> sticky/tests.rs: load_order_does_not_change_the_numbers_the_map_remembers
sticky-types | D:run_sticky boot 5 | full map: sine keeps 18; nan evicts the lowest foreign entry (slot 0), error event, dirty | -> sticky/tests.rs: a_full_map_evicts_the_lowest_foreign_entry_and_logs_it_as_an_error
sticky-types | D:run_sticky boot 6 | full map, sine in slot 0: a machine of the boot is not evicted, slot 1 goes, nan takes 17 | -> sticky/tests.rs: an_entry_of_a_machine_in_the_boot_is_never_the_one_evicted_even_in_slot_0
sticky-types | D:run_sticky boot 7 | fresh card, nan alone: 16, entry written | -> sticky/tests.rs: fresh_card_with_one_machine_gets_16_and_the_entry_is_written
sticky-types | D:run_sticky boot 8 | another machine's entry holds 16: nan takes 17, that entry stays | -> sticky/tests.rs: another_machines_entry_holds_16_so_the_new_machine_takes_17_and_that_entry_stays
sticky-types | D:run_sticky boot 9 | own entry (20) beats the lowest free number, nothing rewritten | -> sticky/tests.rs: a_machines_own_entry_beats_the_lowest_free_number_and_nothing_is_rewritten
sticky-types | D:run_sticky each boot | SELECT keeps the map bytes | -> sticky/tests.rs: a_selection_keeps_the_map_bytes (`sel::next_record`, the record both selection commands write)
sticky-types | D:run_sticky each boot | every module loaded (ColdFire part) | DROPPED: a load result, not an assignment; the dynreg roundtrip and modules-capacity device tests check results
sticky-types | D:run_sticky end | `common_checks_upload` (write watch, flush) | DROPPED: the card policy is asserted by the TypeScript card tests; the unit tests write no card
sticky-types | D:run_sticky | the map persists on the card between boots | COVERED by the `sel` encode and decode tests in sdk/src/lib.rs (the map bytes round-trip); the roundtrip device test checks that run-time types stay stable across boots

New unit tests with no Python counterpart: a full map whose entries are all in the boot evicts nothing and gives no number (the 9th machine gets 0); pinned keys take no number; a repeated key takes a fresh number and is not recorded; `clean` drops duplicate hashes, duplicate numbers, out-of-range types and entries without a hash.

## capacity, inferred-needs, fits, menu-open, isolation (dt2 and dn2; C `tools/patchkit/pk_digikit_capacity.py`, K `tools/patchkit/pk_capacity.py`; Python deleted)

Status: proven on DT2 and DN2 (2026-11-03). TS tests: `modules/core/runtime/tests/capacity.device.test.ts` (below `capacity.test`), `modules/core/apps/digikit-menu/tests/fits.device.test.ts` (`fits.test`). Helper: `tests/regression/runtime/synth.ts` (typed `synth-dkm` spec, packing, geometry). `fullSet` and its types moved from the capacity test into `synth.ts` (the memaudit full variant uses them). `pk_capacity.py` and `test_capacity_sets.py` are deleted; `fullSet` throws when the sizes do not fit the profile.
The pure limit logic (first-fit DSP placement, row counting, registry rows, type range, stage and read fit) is unit-tested in `modules/core/runtime/sdk/src/capacity.rs` and `sticky.rs`; the device tests keep what a unit cannot: the real loader on the real memory map.
Names in `capacity.test`: **full set CF** = "capacity, ColdFire only > full set: every limit is filled ..."; **files** and **arena** likewise; **full set coupled** = "capacity, coupled > full set behind a real module ..."; **needs** = "inferred needs > ID: the resources taken are the needs inferred from its contents" (full tier, 12 modules per device).

capacity | C:91 | boot report: every module has the wanted result | -> capacity.test: full set CF, files, arena, full set coupled (`expectBoot`)
capacity | C:112 | MODULES lists exactly the loaded modules, in order | -> capacity.test: all (`expectBoot`)
capacity | C:120 | ColdFire blocks inside the arena, DSP ranges inside the area, no overlap | -> capacity.test: all (`expectBoot`)
capacity | C:53 | CAPACITY agrees with MODULES and the device file (modules, selection, arena, stage, DSP area, voice rows, registry rows) | -> capacity.test: all (`expectBoot`); FX_SLOTS, SEND_SLOTS, MAP_ENTRIES, STORE and SETTINGS limits DROPPED here: `modules-capacity.device.test.ts` and the settings and upload tests read them
capacity | C:146 | `DKRT_TABLE_BAD` is 0 and no TABLE_BAD event | -> capacity.test: all (`expectBoot`)
capacity | C:172 modules | all module records in use, SELECT of a ninth path is E_SEL_COUNT, mailbox select too | DROPPED: the table is a loop over records (the full set uses 5 of 8); E_SEL_COUNT is the selection-record format limit, checked by the upload tests and the `sel` unit tests
capacity | C:212 files | a file of the staging buffer size loads with its whole image (`cf_size` = stage - header) | -> capacity.test: files
capacity | C:212 files | one file 4 bytes over is E_TOO_BIG, a repeated name is E_LOADED | -> capacity.test: files
capacity | C:212 files | the arena holds the loaded blocks and nothing of the refused files | -> capacity.test: files (arena used = sum of the blocks)
capacity | C:224 arena | arena filled to the last byte (used = limit, largest 0); raw X, DSP Y and bss = arena Z are E_NOMEM; bss above the arena is E_FORMAT | -> capacity.test: arena
capacity | C:224 arena | the refused DSP module left no DSP range and no row | -> capacity.test: arena
capacity | C:239 dsp-area | R and A back to back from the first 8-aligned byte, B is E_DSP_NOSPACE at reserve, C larger than the area at check | DROPPED: first-fit and the reserve decision are `capacity::first_fit` unit tests; the full set fills the area to its last whole granule (D) and refuses Z with E_DSP_NOSPACE on the real map; the check-time refusal of a range larger than the area is not covered on the device
capacity | C:250 rows-all, rows-split, rows-types | rows taken by one module or two, E_DSP_ROWS before E_DSP_TYPE, E_DSP_TYPE for a used or out-of-range type, E_DSP_FORMAT for too many voices | DROPPED: `capacity::dsp_rows` and the type checks are unit-tested; the full set takes every voice row (V) and refuses Y with E_DSP_ROWS; E_DSP_TYPE and E_DSP_FORMAT are not covered on the device
capacity | C:277 machines | M takes every machine type and parameter row; one more machine is E_TYPE_RANGE, one more parameter E_PARAM_FULL | -> capacity.test: full set CF and coupled take every machine, parameter and page row (CAPACITY used = limit); the two refusal codes DROPPED: `capacity::registry_rows` and `machine_type` unit tests
capacity | C:291 pages | every page row used; one more is E_PAGE_FULL | -> capacity.test: full set (on the DN2, the only device with page rows); E_PAGE_FULL DROPPED as above
capacity | K:404 full_set | M, V, D, F loaded; X E_NOMEM, Y E_DSP_ROWS, Z E_DSP_NOSPACE | -> capacity.test: full set CF (no reference, so a ColdFire-only rig keeps every arena copy and the arena has no free byte, largest 0), full set coupled (behind `test/dyn-fx-gain`)
capacity | C:733 check_full | every voice row has an owner, every machine type in use, CAPACITY counts every machine, voice, parameter and page row, DSP largest < 64 | -> capacity.test: full set CF and coupled (`expectFull`)
capacity | C:733 check_full | arena free 0 and largest 0 | -> capacity.test: full set CF only; coupled boots free the arena copies of the DSP images, so there the test asserts used < limit
capacity | C:327 | the 'files' fixtures are exactly the staging size and 4 over | -> capacity.test: files (file sizes)
inferred-needs | C:356 | each module that packs, alone on a ColdFire-only rig: ARENA (+ARENA_XFER), DSP_AREA, VOICE_ROWS, FX_SLOTS equal the needs; MACHINE, PARAM, PAGE rows for a non-DSP module | -> capacity.test: needs, on a coupled rig (after the DSP acknowledges): ARENA = block only, DSP_AREA, VOICE, MACHINE, PARAM, PAGE rows and FX_SLOTS all equal the needs, for every module (stronger than the CF-only form, as Python `isolation` "alone" was)
inferred-needs | C:416 | the MODULES record: block of cf_size = ARENA, dsp_len = DSP_AREA, popcount(rows) = VOICE_ROWS | -> capacity.test: needs
inferred-needs | C:372 | every module that packs for the device (`dktransfer.candidates`: all fixtures and shipped modules) | CHANGED: 12 modules: the 8 shipped modules and 4 fixtures (raw, machine, effect, machine with params; `test/dyn-params` DT2 only); the set is a list in the test and in the project record. The other fixtures stand for the same shapes
inferred-needs | C:381 | at least 8 modules pack | DROPPED: the list is explicit; a module that does not pack fails `moduleArtifact`
fits | C:520 | rows of the MODULES page equal the memory and kind computed from the needs; nothing ticked | -> fits.test
fits | C:541 | YES on the module larger than the area ticks nothing and opens "HUGE DOES NOT FIT" with the reason from the fit rule | -> fits.test
fits | C:562 | one half-size module ticks, rows unchanged | -> fits.test
fits | C:575 | the second half-size module is refused with "HALF-B DOES NOT FIT" and the reason | -> fits.test
fits | C:588 | unticking the first makes room for the second | -> fits.test
fits | C:520 | PNG of each step (`shot`) | DROPPED: debugging aid, no check
menu-open | C:610 | opening MODULES with 12 modules stays under 25 extra emulated ms; the sector reads of the needs reader are counted | DROPPED by decision: a performance guard only; the rows of the page are checked by fits.test
isolation | C:785 | the reference's DSP bytes in L2 equal its packed image relocated at its range | -> capacity.test: full set coupled; the reference is `test/dyn-fx-gain`, not `test/sine`: every machine module has DSP imports and the TS oracle `relocate` does not resolve them. Only the one boot behind the full set; the "alone" boot DROPPED
isolation | C:850 | alone: the runtime holds exactly the reference's needs after the load | -> capacity.test: needs (every module, including `test/dyn-fx-gain` and `test/sine`)
isolation | C:876 | the reference's MODULES record is equal with and without the full set | DROPPED by decision with the PCM comparison: the ranges are first-fit and independent of what loads behind; the reference's DSP bytes are checked in the full set
isolation | C:884 | the note's PCM is equal from the onset with and without the full set | DROPPED by decision (HANDOVER-2026-11-03-nx-gate.md item 3)
isolation | C:855 | the reference note is audible | DROPPED with the PCM comparison
capacity, inferred-needs, fits | C:350, C:431, C:604 | `common_checks_upload` / `common_checks` (write watch, flush check, card writes) | -> card-rig.ts `afterClose` on every rig (write watch and flush check 0, card writes only the selection sectors)

Differences that apply to many lines:

- The Python `capacity` cases booted in threads, one rig per case, ColdFire-only. In TS one daemon per describe block boots the cases one after the other; the full set, files and arena run on a ColdFire-only daemon (`dsp: 'zero'`), the coupled full set and the needs on a coupled daemon with `hold: true`.
- The full set has no reference module on the ColdFire-only daemon: a DSP reference's registry rows come after the DSP acknowledges, so machine types and rows would not add up there.

## memaudit (dt2 and dn2, quick; D `run_memaudit`; all variants ported; Python deleted)

Status: proven on DT2 and DN2 (full tier, 2026-11-03). The scenarios `memaudit-{dt2,dn2}` and `memaudit-{dt2,dn2}-stray` and `pk_modaudit.py` are deleted. `memaudit-{dt2,dn2}-full` and `pk_digikit_memaudit.py` are deleted too (full variant in the same test, `full` tier). The test is `modules/core/runtime/tests/memaudit.device.test.ts`; the helpers are `tests/regression/runtime/memaudit-kit.ts` (guard plan, writer names, module sets) and `memaudit-suite.ts` (the two daemons), and `project-kit.ts` (the panel drive).

memaudit | D:run_memaudit | every module of the set loaded (ColdFire part), count, no safe mode | -> memaudit.device.test.ts: the set loads ... (`bootInfo`: safe 0, count, results 0 or LOADING)
memaudit | D:run_memaudit | MODULES lists the loaded modules and every block lies inside the arena | -> memaudit.device.test.ts: the set loads ... (`dk.modules()`, `arenaBounds`)
memaudit | D:run_memaudit | the full set uses the arena and the DSP area to the last byte | -> memaudit.device.test.ts: full set > drive (`fullSet` behind test/dyn-demo; the three refused modules with their codes in the boot record; `dk.capacity()`: ARENA limit - used = 0, largest 0, DSP_AREA largest < granule 64)
memaudit | D:plan_guard, arm | protect the free cf regions minus the module blocks, arena labelled, allowed PCs = placed patch code | -> memaudit-kit.ts `guardPlan`, `patchCode` (the plan's `writes`); `guard.set` in the test
memaudit | D:drive | DIGIKIT menu, MODULES, project save, machine on the track, p-lock, save, load | -> memaudit-kit.ts `driveAuditUi`, project-kit.ts `buildPanelProject`, `savePanelProject`, `loadPanelProject`, `selectDt2`
memaudit | D:run_memaudit | the must_run module's block changed while the guard was armed | -> memaudit.device.test.ts: same test (`moduleBlocks`, `bytesChanged`)
memaudit | D:run_memaudit | guard total 0 (clean set) | -> memaudit.device.test.ts: clean set > drive
memaudit | D:run_memaudit | stray: exactly one store, by the code of test/dyn-demo-stray, in the label arena | -> memaudit.device.test.ts: stray store > drive (`whoWrote`); the session's guard fault is allowed for this case only
memaudit | D:load_pass | second cold boot with the write watch on the protected ranges from the first cache enable, no CPU store | -> memaudit-suite.ts: load pass (second daemon, `loadWatchEnv`); the watch leaves out the runtime's DMA buffers (`dmaHoles`) and the daemon fails on any watch event at close
memaudit | D:common_checks_upload | write watch, flush check, card writes in the store range and the project sectors | -> `cardRig` options `uploads`, `relaxFs`, `project` (the rig's close checks)
memaudit | D:sweep | `module:audit`: each of the first 14 machine list entries chosen on track 1, page turned | -> project-kit.ts `sweepMachineList`; memaudit-module.device.test.ts
memaudit | D:report | per-module words changed, hits list in the run report | DROPPED: the artifacts dir keeps `memaudit-plan.json` and `memaudit-result.json`; Python report fields have no reader
