# Device test audit (test speed plan, step 7)

Static audit of the 50 `*.device.test.ts` files at main f5baea21, from reading the tests and the host suites. Times are the sum of test durations in the
2026-11-09 gate (`out/runs/lanes/pr-275-gate-outputs/`, a loaded machine, so only the order matters). Nothing here ran an emulator except the proof
runs named under "Applied".

Classes: **E** needs the emulator (firmware, DSP, card or boot behaviour that no host code reaches). **H** the logic can be a host test (candidate).
**D** duplicates another test. **S** re-tests a shared SDK or core component that a host test or another device test already covers.
Marks: **[V]** checked against the host test named; **[?]** uncertain, needs a decision or a closer read.

Host suites used as evidence: `modules/core/runtime/sdk/src/{store,settings,sticky,capacity,sample_tracks}/tests.rs` (store layout, torn copies, settings
copies, selection copies, limits and first fit), `modules/core/runtime/src/rs/src/{pageclick,paramapi,boot,intro,safe_screen,nativepage}.rs`,
`modules/core/adapters/ui/host/tests/{picker,menu}.rs`, `tests/regression/host/*`. `card.rs`, `settings.rs`, `upload.rs`, `loader.rs` (runtime side) have no
host tests: their checks stay device tests until the logic is split out.

## Reload-bound files (the plan's start list)

| File | Test or group | s | Class | Reason | Proposed action |
| --- | --- | --- | --- | --- | --- |
| `card-safety` | 1 blank card works, write function refuses writes outside the store | 435 (15 tests) | E | `card.rs` `admit` and `check` have no host test | Keep. **[?]** Splitting `admit`/`check` into a pure function would let tests 2, 5 to 9 (foreign bytes, torn headers A/B, torn selection copy, small card) run on the host. Payoff about 8 of 10 cold boots; cost half a day |
| `card-safety` | torn header A / B / selection copy A (4 tests) | | S | The header and copy decision tables are in `store/tests.rs` (`header_copies_fall_back…`, `a_higher_seq_in_b_wins…`, `a_power_cut_at_any_write…`) and `sticky/tests.rs`; the device test only adds "the runtime obeys the answer" | **[?]** Keep one torn case on the device, drop the rest once `check` is host-testable |
| `settings` | main: put/get, 2 s delay, copy A then B, restart, remove | 411 (16 tests) | E | integration of the mailbox, the timer and the card | Keep |
| `settings` | `copies/*` torn, higher, both (3 tests, 3 reloads) | | S | The decode rule for each case is `settings/tests.rs` (`the_higher_seq_wins…`, `a_damaged_newer_copy…`, `both_copies_damaged…`) **[V]**; the device test checks that boot reads it and the next write goes to the other copy | **[?]** Merge into one device case (torn newer copy); the other two are host-covered. Saves 2 boots |
| `settings` | restart, blank, overlap, oldcard | | E | boot paths in `settings.rs` and `boot.rs` without host tests | Keep |
| `settings-api` | put/get after restart, same key in two modules, key rules | 155 (6 tests) | E / S | The key rule is `settings/tests.rs` (`a_module_key_…refused`) **[V]**; test 3 re-tests it through the mailbox | **[?]** Drop test 3, if the mailbox error mapping is not the point. Not applied: not an exact duplicate |
| `bootscreen` | valid file, tick hash, stock intro with a bad file (several), fault halts, reset reaches safe mode | 1170 (16 tests) | E | the slowest file; every case needs a cold boot with a module on the intro task | Keep. **[?]** `reset from the intro callback reaches safe mode on boot 3` re-tests the boot counter that `safe-mode` ("two boots never reach the UI loop") and `boot.rs` host tests cover; only the reset site differs |
| `bootscreen` | REC held with a valid file | | E | Safe-mode entry by key on the image without the `core/introblank` adapter. `introblank` "REC held" checks the same boot info on another image | Keep |
| `introblank` | 6 groups, 12 tests | 843 | E | the intro is firmware | Keep. "control: a note plays on the stock image" is the oracle of the next test |
| `memaudit` | clean set, stray store, full set (12 tests, 4 skipped) | 535 | E | ColdFire write guard over a boot | Keep. **[?]** "clean set" appears twice with different rigs; read the rigs before merging them |
| `memaudit-module.manual` | one module | n/a | E | manual, no gate cost | Keep |
| `capacity` | full set, files, arena, full set behind a real module (8 tests) | 376 | E / S | The limit logic and first fit are `capacity/tests.rs` **[V]** (staging size, loaded name, full table, exact-full and one-over for every row kind); the device tests prove the loader applies them on the real memory map | Keep. **[?]** `files` (staging size, +4 bytes, repeated name) is the most host-covered part (`a_file_must_fit_the_staging_buffer…`, `pick_record_refuses_a_name_that_is_loaded…`) |
| `inferred-needs` | one boot per module | n/a (full tier) | E | needs inferred from contents vs what the loader takes | Keep |
| `modules-capacity` | MODULES and CAPACITY replies | 55 | E | reply paging and ranges of the real loader | Keep |

## Composite and FX files

| File | Test | s | Class | Reason | Proposed action |
| --- | --- | --- | --- | --- | --- |
| `fx-track-route` | "preserves stock pixels, routes, kit amounts and project controls across ten clean baselines" | 926 (4 tests) | E, composite | ten former tests behind one `test`, one wall-clock budget; the baseline leg ("captures the original FX pixels and PCM") is also in `fx-pages` | Split into ten `test`s sharing the no-hook baseline (plan step 7). Needs the baseline artifacts to be shared; do it with the Nx task slicing, not here |
| `fx-pages` | stock pixels/audio preserved, page knobs, remove through SELECT | 600 | E | overlaps the baseline legs of `fx-track-route` and `freqshift` (same stock-vs-hook comparison) | **[?]** one shared baseline for the three; not an exact duplicate |
| `fx-kit-persist` | kit and project values round trip | 540 | E | persistence of kit values through restart | Keep |
| `fx-routes-panel` | manifest routes, pending choice, cancel, send-slot capacity | 749 | E | **[?]** the pending/cancel logic may be in `menu.rs`; read before deciding |
| `fx-routes-persist` | menu YES persists both route directions | 710 | E | | Keep |
| `fx-send`, `fx-send-dn2` | send amounts and squared gain law; release-tail chord voice slots | 238 / 99 | E | audio | Keep |
| `freqshift` | page metadata, bipolar shift, kit round trip, spectra on four clean cards | 966 | E | `page metadata` and `kit roundtrip` re-test the `fxpages` adapter (S); the spectra are the module's | **[?]** keep only the spectra and one send/master check; saves 2 of 4 cards |
| `fx-chain`, `fx-reuse` | dynfx chain, reuse of an owned daemon | 363 / 349 | E | | Keep |
| `overdrive-type` | OVR TYPE changes the note at a high amount | 4 tests, small | E | | Keep |
| `roundtrip`, `equiv` | project round trip across module sets; run-time set against static twin | 135 / 125 | E | `equiv` belongs to the ts-reload lane | Keep |
| `input-adapter` | menu navigation and TURN after identical boots | 447 | E | | Keep |

## Runtime protocol files

| File | Test | s | Class | Reason | Proposed action |
| --- | --- | --- | --- | --- | --- |
| `upload` | ops, quick, folders, txfull (11 tests, 8 skipped in the quick tier) | 109 | E | `upload.rs` has no host test | Keep |
| `upload-faults` | power cut after the Nth write, CLEAR double fault, upload during a note | n/a | E | same | Keep. The "valid store afterwards" part is `store/tests.rs` `a_power_cut_at_any_write…` **[V]** (S); the runtime's reaction is the device part |
| `upload-depth` | two requests in flight, a flipped bit, a lost reply | 137 | E | the session window against the real USB | Keep |
| `store-format` | flipped byte, version 3 store, installDefaults | 226 | E | `store/tests.rs` has the version rule **[V]**; "installDefaults ends with the module loaded" is the only workbench-path check | Keep |
| `safe-mode` | reset in init, safe-once flag, hang in a session, power cut copies A/B | 272 (22 tests, 18 skipped) | E | the copy-pair test (`power cut, A / B`) is `sticky/tests.rs` **[V]** (S); it runs in the full tier only | **[?]** keep one pair on the device |
| `crash-capture`, `fault-trace`, `debug-guard` | CPU exception, DSP stop, NaN guard | 796 (fault-trace) | E | | Keep |
| `samples-live` (DT2), `samples-store-live` (DN2) | conversion, upload, +Drive bytes | 59 / 114 | E | one per device | Keep |
| `transfer-live`, `service-commands`, `module-load` | CLI transfer, command dispatch, SDK loader | 272 / 43 / 244 | E | | Keep |
| `debug-module` | log, levels, PEEK, early log line | 265 | E | | Keep |
| `sharc-probe`, `remote-input` | probe transport and beacon, remote keys | 285 / 409 | E | | Keep |

## SDK component and module glue

| File | Test | s | Class | Reason | Proposed action |
| --- | --- | --- | --- | --- | --- |
| `shot-picker` | click on SMP, slot list, folder, file | 312 | S, was | re-tested the picker (order, levels, cursor, NO, YES) with framebuffer hashes | **Rewritten** (applied): one glue test with the real hooks (held TRIG, press with a turn, replace, NO levels) on the pool API and the SMP word, 135 s as an Nx task |
| `ui-demo/zero/ui` | rows, list focus, dialogs, toast | 130 | E | the SDK UI component on a device once | Keep. The picker has no ui-demo host (no pool client in the fixture) |
| `digikit-menu/zero/menu`, `fits` | empty-store menu; what-fits rows | 51 / 266 | E | `fits` says the rule and the page logic are host tests (`menu.rs`) and this is the scan path | Keep |
| `native-page` | native callback page cycles and routes turns | 388 | E | `nativepage.rs` host tests exist for the logic **[?]** | Read before deciding |
| `pluck/ui-preview` (zero DSP) | preview loads and edits its page | 73 | E | | Keep |
| `tb303` | loads, plays one note, ends it | n/a | E | module audio, one per module | Keep |
| `shot-dt2`, `shot-dn2` | playback, SMP, window (one test each) | 251 / 697 | E | composite; DN2 is the slowest single test after `fx-track-route`. The DN2 SMP step stops at word 0x100 when a wait on `main_ui_reached` follows the install (9 of 19 runs, 0 of 12 without) | **[?]** split by function (pool, window, play); find why the word freezes (plan step 3, stage 2) |

## Applied

1. `shot-picker.device.test.ts` rewritten (see the plan, step 7 first case).

## For Em to decide

- **card-safety**: split `card.rs` `admit`/`check` into host-testable functions, then move 6 of 10 cases to the host.
- **settings** `copies/*` and **safe-mode** copy pairs: keep one device case, rest is host-covered.
- **freqshift** and **fx-pages**: drop the adapter re-tests (page metadata, kit round trip) from the module test.
- **fx-track-route**, **shot-dn2**: split the composite tests so they run in parallel.
- **bootscreen** "reset from the intro callback": drop if `safe-mode` and `boot.rs` cover the counter.

## Tags (T1e step 1)

`tags` on each `metadata.runtimeAcceptance` record (vocabulary: `tools/nx/acceptance-tags.mjs`). Assigned by reading the files and this audit; a file that spans areas got the wider set. **[?]** marks a guess: `platform-contract` (all areas it touches), `safe-mode` (C:layout for the hang and cut cases), `inferred-needs` (loads machines and fx), `commit`, `dynreg/equiv`, `shot-dn2-link` (loads an fx), `bootscreen*` and `usb-fast` (tool modules outside debug, remote and probe get `M:` tags).

| File (under modules/, tests/) | Tags |
| --- | --- |
| `core/adapters/dynfx/tests/fx-chain.device.test.ts` | A:core/dynfx |
| `core/adapters/dynfx/tests/fx-reuse.device.test.ts` | A:core/dynfx |
| `core/adapters/dynreg/tests/equiv.device.test.ts` | A:core/dynreg, C:dsp, S:machines |
| `core/adapters/dynreg/tests/roundtrip.device.test.ts` | A:core/dynreg, C:state |
| `core/adapters/fx/tests/fx-routes-panel.device.test.ts` | A:core/fx, C:state |
| `core/adapters/fx/tests/fx-routes-persist.device.test.ts` | A:core/fx, C:state |
| `core/adapters/fx/tests/fx-send-dn2.device.test.ts` | A:core/fx |
| `core/adapters/fx/tests/fx-send.device.test.ts` | A:core/fx |
| `core/adapters/fx/tests/patched-dsp-startup.device.test.ts` | A:core/fx, C:boot, C:dsp |
| `core/adapters/fxpages/tests/fx-kit-persist.device.test.ts` | A:core/fxpages, C:state |
| `core/adapters/fxpages/tests/fx-pages.device.test.ts` | A:core/fxpages |
| `core/adapters/fxpages/tests/fx-track-route-kit.device.test.ts` | A:core/fxpages, C:state |
| `core/adapters/fxpages/tests/fx-track-route-limit.device.test.ts` | A:core/fxpages |
| `core/adapters/fxpages/tests/fx-track-route-lock.device.test.ts` | A:core/fxpages |
| `core/adapters/fxpages/tests/fx-track-route-send.device.test.ts` | A:core/fxpages |
| `core/adapters/fxpages/tests/fx-track-route-stock.device.test.ts` | A:core/fxpages |
| `core/adapters/input/tests/input-adapter.device.test.ts` | A:core/input |
| `core/adapters/introblank/tests/introblank-rec.device.test.ts` | A:core/introblank, C:boot |
| `core/adapters/introblank/tests/introblank.device.test.ts` | A:core/introblank, C:boot |
| `core/adapters/overdrive/tests/overdrive-type.device.test.ts` | A:core/overdrive |
| `core/adapters/plusdrive-write/tests/plusdrive-write.device.test.ts` | A:core/plusdrive-write, C:storage |
| `core/adapters/sdhc-large/tests/sdhc-large.device.test.ts` | A:core/sdhc-large, C:storage |
| `core/adapters/usb-bulk-audio/tests/usb-bulk-audio.device.test.ts` | A:core/usb-bulk-audio, C:transport |
| `core/adapters/usb-bulk/tests/usb-bulk.device.test.ts` | A:core/usb-bulk, C:transport |
| `core/apps/digikit-menu/tests/fits.device.test.ts` | A:core/digikit-menu, C:loading |
| `core/apps/digikit-menu/tests/needs.device.test.ts` | A:core/digikit-menu, C:loading |
| `core/apps/digikit-menu/tests/zero/menu.device.test.ts` | A:core/digikit-menu |
| `core/runtime/tests/addrmap.device.test.ts` | C:layout |
| `core/runtime/tests/boot-restart.device.test.ts` | C:boot, C:dsp, C:loading |
| `core/runtime/tests/capacity/capacity.device.test.ts` | C:loading |
| `core/runtime/tests/capacity/commit.device.test.ts` | C:loading, S:machines |
| `core/runtime/tests/capacity/inferred/inferred-needs.device.test.ts` | C:loading, S:machines |
| `core/runtime/tests/card-safety.device.test.ts` | C:storage |
| `core/runtime/tests/crash-capture.device.test.ts` | C:diagnostics, C:layout |
| `core/runtime/tests/debug-guard.device.test.ts` | C:diagnostics, C:dsp, C:layout |
| `core/runtime/tests/entry/entry.device.test.ts` | C:loading |
| `core/runtime/tests/fault-trace.device.test.ts` | C:diagnostics, C:dsp, C:layout |
| `core/runtime/tests/file-write/file-write-cut.device.test.ts` | C:layout, C:storage |
| `core/runtime/tests/file-write/file-write.device.test.ts` | C:storage |
| `core/runtime/tests/fixtures/module-load.device.test.ts` | C:loading |
| `core/runtime/tests/lfo/lfo.device.test.ts` | S:lfo |
| `core/runtime/tests/log-names.device.test.ts` | C:diagnostics, C:layout |
| `core/runtime/tests/memaudit.device.test.ts` | C:layout |
| `core/runtime/tests/modules-capacity.device.test.ts` | C:loading |
| `core/runtime/tests/platform-contract.device.test.ts` | C:boot, C:storage, C:tick, C:transport |
| `core/runtime/tests/safe-mode.device.test.ts` | C:boot, C:layout, C:storage |
| `core/runtime/tests/samples-live.device.test.ts` | C:storage, S:samples |
| `core/runtime/tests/samples-store-live.device.test.ts` | C:storage, S:samples |
| `core/runtime/tests/service-commands/service-commands.device.test.ts` | C:transport |
| `core/runtime/tests/settings/settings-api.device.test.ts` | C:state, C:storage |
| `core/runtime/tests/settings/settings-cards.device.test.ts` | C:boot, C:state, C:storage |
| `core/runtime/tests/settings/settings-copies.device.test.ts` | C:boot, C:state, C:storage |
| `core/runtime/tests/settings/settings.device.test.ts` | C:boot, C:state, C:storage |
| `core/runtime/tests/storage/storage.device.test.ts` | C:storage |
| `core/runtime/tests/store-format.device.test.ts` | C:loading, C:storage |
| `core/runtime/tests/transfer-live.device.test.ts` | C:storage, C:transport |
| `core/runtime/tests/upload-depth.device.test.ts` | C:transport |
| `core/runtime/tests/upload-faults.device.test.ts` | C:layout, C:storage, C:transport |
| `core/runtime/tests/upload.device.test.ts` | C:storage, C:transport |
| `fx/freqshift/tests/freqshift.device.test.ts` | A:core/fx |
| `machines/analog/tests/equiv.device.test.ts` | C:dsp, S:machines |
| `machines/bd/tests/equiv.device.test.ts` | C:dsp, S:machines |
| `machines/pd/tests/equiv.device.test.ts` | C:dsp, S:machines |
| `machines/pluck/tests/equiv.device.test.ts` | C:dsp, S:machines |
| `machines/shot/tests/shot-dn2-audio.device.test.ts` | C:dsp, S:machines, S:samples |
| `machines/shot/tests/shot-dn2-lease.device.test.ts` | S:machines, S:samples |
| `machines/shot/tests/shot-dn2-link.device.test.ts` | S:machines, S:samples |
| `machines/shot/tests/shot-dn2-pool.device.test.ts` | S:machines, S:samples |
| `machines/shot/tests/shot-dn2-supersede.device.test.ts` | M:digikit/remote, S:machines, S:samples |
| `machines/shot/tests/shot-dt2.device.test.ts` | S:machines, S:samples |
| `machines/shot/tests/shot-picker.device.test.ts` | S:machines, S:samples |
| `machines/tb303/tests/tb303.device.test.ts` | S:machines |
| `tests/fixtures/modules/native-page/tests/native-page.device.test.ts` | A:core/ui |
| `tests/fixtures/modules/ui-demo/tests/zero/ui.device.test.ts` | A:core/ui |
| `tests/regression/emulator/reply-inflight.test.ts` | C:transport |
| `tests/regression/emulator/sharc-semantics.test.ts` | C:dsp, M:digikit/probe |
| `tools/bootscreen/tests/bootscreen-fault.device.test.ts` | C:boot, M:tools/bootscreen |
| `tools/bootscreen/tests/bootscreen-safe.device.test.ts` | C:boot, M:tools/bootscreen |
| `tools/bootscreen/tests/bootscreen.device.test.ts` | C:boot, M:tools/bootscreen |
| `tools/debug/tests/debug-module.device.test.ts` | M:digikit/debug |
| `tools/probe/tests/sharc-probe.device.test.ts` | C:dsp, M:digikit/probe |
| `tools/remote/tests/remote-input.device.test.ts` | M:digikit/remote |
| `tools/usb-fast/tests/usb-fast.device.test.ts` | C:storage, C:transport, M:digikit/usb-fast |
## SHARC need (T1d, first changes)

The rows above lack a SHARC column; the full column comes with T1e. Rows checked so far:

| File | Test | SHARC | Evidence |
| --- | --- | --- | --- |
| `digikit-menu/needs` | usb-fast without core/usb-bulk | no (zero), proven on DT2 and DN2 | usb-fast has no DSP object; the load ends in E_IMPORT; reads only `memPeek`, the small ring and menu rows; no `pcm.*`, `dsp.*`, `audio.*` |
| `bootscreen-fault` | fault in the intro callback halts | yes (keeps `sharc`) | no module needs the DSP, but on a zero-DSP boot the halt comes back as `RpcError: Halted (code 6)` without the "guest fault during boot" context the test checks; tried on both devices |
| `bootscreen-safe` | REC held with a valid file | yes for the REC-held rig (keeps `sharc`) | no DSP object, but the card safety check needs observed emulators and exported overlays ("runtime safety needs observed emulators and exported overlays" on a zero-DSP boot); the reset-chain rig already runs on zero |
