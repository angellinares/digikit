# 39. Boot-time scenarios of the digikit runtime kept in the suite (Phase 6, tests)

Plan: `docs/plans/digikit-runtime.md` (Phase 6). Boot path: finding 37. Code: `tools/patchkit/pk_digikit_scen.py` (runner), `tools/patchkit/pk_digikit_project.py`
(project and card-slot helpers; the project round trip is `tests/regression/runtime/roundtrip.test.ts`), `tests/digikit/` (scenario files `scenarios/*.toml`, pytest entries), twin fixtures `tests/fixtures/modules/twin-xslice` and `tests/fixtures/modules/twin-dn2`,
verify suite `digikit-boot` (`tools/devtools/verify.py`), `docs/TESTING.md` ("Digikit boot scenarios"). Lane r2b, 2026-10-17, native emulator, Digitakt II 1.16/1.17 and
Digitone II 1.11/1.12 (stock images; nothing ran on a device). Everything is **[D]** (measured here once; no second agent has checked it against the image bytes).

## Result

**[D]** Every scenario passes, on both devices, on both images of each device where it applies (times: wall clock of one scenario on one image, the boots
restored from the daemon's boot cache where the card is the same).

| Scenario file (kind) | What it proves | Result, time |
| --- | --- | --- |
| `equiv-dyn-pluck` (equiv, DT2) | `test/dyn-pluck` selected: results `[0]`, voice row claimed, the machine, parameter and page rows decoded from RAM equal those of the image with `digikit/pluck` patched in (name, short, clone_of, knobs by parameter name, ranges, defaults, mirror index, the template words), no view rebuild, PCM of a note equals the patched-in machine's | pass 1.16 132 s, 1.17 108 s |
| `equiv-dyn-xslice` (equiv, DT2), `equiv-mixed` (DT2: pluck + xslice) | a data-only machine with a parameter equals its static twin `test/twin-xslice`; the mixed set equals both twins, PCM of the pluck equal | pass 1.17 89 s, 79 s |
| `equiv-dn2-set` (equiv, DN2) | `test/dyn-dn2` (own SYN page, 4 parameters) + `test/dyn-sine-dn2` (voice only): rows, page, voice row equal `test/twin-dn2` + `test/dn2-sine` (its machine entry has no run-time counterpart and is left out), PCM of the sine equal from the onset | pass 1.11 54 s, 1.12 60 s |
| `equiv-dyn-demo`, `equiv-c-demo` (equiv, both) | modules with no registry records (a counter module, a C module with libgcc helpers): result 0, every table equals a boot with no module | pass, 38 to 60 s |
| `roundtrip-dt2` (quick), `roundtrip-dt2-orders` (full), `roundtrip-dn2` (full) | see below | pass; DT2 1.16 349 s (4 boots), DN2 1.11 164 s, DN2 1.12 276 s, DT2 1.17 quick 2 boots |
| `safe-fault` (safe, both) | a module whose `init` resets the machine: boots 1 and 2 raise the counter, the third is safe mode (reason counter, counter 2), no module, result -44, voice rows free, the UI loop clears the counter, selection kept; the safe-once flag | pass 53 to 92 s |
| `cut-selection` (cut, both) | A new/B old, A torn/B old, A old/B new, both torn, and one copy written by the runtime then a restart: the boot uses the valid copy with the higher sequence number | pass; DT2 168 s, DN2 77 s (the quick tier runs the first two cases and the runtime cut) |

Every scenario ends with the common checks: write-watch over `cf.expansion` (0 stray writes over every emulator it ran, 12 to 16 per scenario), flush check (0 hits),
and the card writes (in the store range only the two selection sectors; nothing below sector 0x50000 but those two).

### The project round trip (the check that could show data loss): none found

A project with the run-time machine on track 1 (DT2: and a second one on track 2), parameter values (ENC A, ENC B turned), a note trig on step 1 with a p-lock (ENC C) and a
preset lock (LEVEL) is saved through the panel (FUNC+SETTINGS). Under set A the machine is loaded; the next boot has **set B, which lacks the track-1 machine**, loads the project
(SETTINGS, LOAD PROJECT), saves again; then set A in the other load order. The MmcFs slot is compared sector by sector (the sectors the guest wrote, read from the card files):

- **[D]** After set B and after A in the other order the slot equals the first save **byte for byte, but for one 32-bit header word** at slot offset 0x18 (after the project name).
  That word is `0x56650daa` in the first save of a project and `0x421f3ead` in every later one, with or without a module change (control: a reload under the same set A gives
  `0x421f3ead` too; the saves of boots 2, 3, 4 are byte-identical to each other, stamp included). The same two values appear on DT2 and DN2. It is not the CRC of anything we tried
  and not time-dependent in the emulator; what it is **[O]**. The scenario leaves it out of the comparison with the first save and holds it fixed from the second save on.
- **[D]** The sound records still carry the machine types after set B (the track whose machine is absent keeps its type: 18 on DT2 track 1, 16 on track 2 and on DN2 track 1; the
  type byte is at +0xd2 and +0xf4 of a record). A track whose machine is absent is saved as it was loaded.
- **[D]** The controls: the first save differs from a save of the untouched project. DN2: the trig bytes 4 and 5 of pattern 1, the lock bytes near offset 0x4a34 and 6 bytes of the kit
  (a precise control). DT2: the firmware writes only the patterns and kits that changed, so the untouched project's save does not write pattern 1 at all and the edit save's
  bytes (trig bytes 3, 4, 5, 7, 11 ...) compare against zeros (a weaker control: pattern 1 now holds data). The machine types are present in the first save.
- **[O]** Whether the preset (sound) lock was created by the panel script is not decoded separately: the changed bytes of pattern 1 are a trig and lock data, not named further.
  A project that has the machine on tracks of other patterns or kits, and the 128-kit/pool copies, are not covered (only slot 1's written sectors).

## What was found

- **[D]** A cold boot of the patched firmware on a card an earlier run saved a project on never became ready: the DT2 filesystem check the readiness contract waits for does not
  run on a used card (as after a soft reset, finding 37). `DIGI_COLD_RELAX_FS=1` (`packages/emulator/control/src/controller.rs`, also in the boot-cache key) makes the cold boot
  ready at the main panel; the runner sets it for cards made by `card_after` only.
- **[D]** The machine selection on DT2 puts a track's type byte at `0x4265338e + 0x450 * (track - 1)` (the kit's track stride, finding 02); `TRK` + `TRIG n` selects track n.
- **[D]** (Superseded by the section "Time: where it went" below: the cause is the coupled emulator's speed, 0.24x, and the fix is not to run the SHARC+ where nothing needs it.) Most of a scenario's time is emulated panel time (a save and a load take about 10 s each, a restart about the same), not the boots: restoring the cold boots from the
  cache saves 10 to 40 s per boot only. Measured standalone, the quick tier is 97 s (equiv DT2), about 60 s (equiv DN2), 187 s (round trip DT2), 239 s (safe and cut DT2) and about 130 s (safe and cut DN2); inside `mise run verify`, with the cargo tests and the other python tests running, the same files took 97 to 620 s (five files in parallel, `DIGI_PY_JOBS`), so the budget of about 3 minutes holds only for an idle machine.
- **[D]** `verify` builds in the target directory the scenarios use while the python tests run, so a tool binary can be missing for a moment: the runner retries a missing tool for up to 3 minutes.
- **[C]** `pk_dkrt_boot.py`'s own proof (`proof --quick`) covers equiv for one device image, restart, cut and safe; the scenario suite replaces its equiv, cut and safe parts
  with data files and adds the sets, the round trip and the DN2 variants. The proof tool stays as it is (restart, the watch totals).

## Time: where it went and what changed (lane b, 2026-10-17)

Tool: `DIGIKIT_PROFILE=1 uv run python -m tools.patchkit.pk_digikit_scen run NAME` (or `uv run python -m tools.patchkit.pk_digikit_prof NAME`) prints wall time per phase (exclusive),
per named step and the emulator's speed. All **[D]**, one run each, DT2 1.17 / DN2 1.12, warm boot caches, the machine shared with other lanes.

**The speed of the emulator is the cost.** The coupled daemon (ColdFire + SHARC+ in step) runs the panel at **0.21 to 0.24 emulated seconds per wall second**
(8.7 emulated s in 38.7 s on the pluck scenario, 35.3 s in 147 s on the round trip); a coupled cold boot to ready takes 16 to 20 s (DT2) and 9 s (DN2),
restoring it from the boot cache 1 s. The ColdFire alone (`cf_script`, or a `digi-emud` started without `--auto-coupled`) boots the same patched
firmware to the main panel in **5.7 s**, runs the panel at **1.1 emulated seconds per wall second** and skips idle time (3 emulated idle seconds: 0.13 s).
The SHARC+ is what costs; what the firmware saves to the card does not depend on it.

Before (wall seconds; the phases are exclusive):

| Phase | equiv-dyn-pluck (DT2) | equiv-dn2-set (DN2, cold boots) | roundtrip-dt2 |
| --- | --- | --- | --- |
| image build and patch, pack | 2.9 | 2.8 | 1.5 |
| daemon spawn | 2.0 | 14.2 (DN2 spawn 4.7 each) | 1.6 |
| boot: cache restore (DT2) / cold boot (DN2, first run of a card) | 3.1 (3 x 1.0) | 28.2 (3 x 9.4) | 16.9 (1 x 1.0, 1 x 15.9 cold) |
| reset after the boot | 3.2 | 1.1 | 2.0 |
| emulated panel and settle (coupled `step`) | 38.7 (8.7 emulated s) | 26.3 (5.5 emulated s) | 147.1 (35.3 emulated s) |
| note: run to the trig frame, capture (PCM) | 22.6 (13.5 + 9.1) | 30.5 (21.4 + 9.1) | - |
| oracles (decode, compare, slot diff), close, writes | 1.0 | 1.0 | 4.0 |
| **total** | **73** | **104** | **174** |

In the round trip a save is 18 s, building the project 45 s, a load 33 s. In the note scenarios the frame at which the note starts (`TRIG_FRAME`) was a
constant chosen for the slowest setup: the rigs were at frame 5130 (DT2) and 2700 (DN2) and idled to 7500 and 6000 (6.8 s and 10.8 s per rig).

Ceiling (best case per phase, same oracles): spawn and cache restore 1 + 1 s; image build 1.2 s (patchkit); the panel script in emulated time on the
ColdFire alone (the project round trip: about 14 s for the first boot, 14 s for the second); where the DSP is needed (selection of a machine, the note, the
DSP part of a module), coupled seconds at 4.4 s wall each: the capture of 1500 frames is 2 emulated s = 4.5 s and cannot be shortened without
shortening what the PCM oracle compares; the machine list on the panel about 1.5 emulated s = 6 s; boot settle 0.3 emulated s. The pluck scenario's
floor is about 17 s, the DN2 set's about 12 s, the round trip's about 30 s (two ColdFire boots in a chain, each save needs the card of the one before).

Built (every oracle unchanged; results identical, see below):

- **ColdFire-only rig for the panel script of the round trip** (`pk_dkrt_boot.CfBootRig`, `Ctx.rig(..., cf_only=True)`; scenario key `engine = "cf"`, the default of
  `roundtrip`). The boot check "every module loaded, DSP part included" runs on a coupled boot of the same card in a thread beside it (the ColdFire part is
  checked on the ColdFire rig: `count`, no safe mode, every result 0 or `LOADING`). A realistic coupled run of the same panel script stays as
  `roundtrip-dt2-coupled` (`engine = "coupled"`, full tier, DT2 1.17). The DT2 round trip: 174 s to **38 s**; `roundtrip-dn2` (4 boots) 164 to 276 s to 128 s inside a full-tier run with six jobs at load 18 (not measured alone).
- **Independent boots run side by side** (`Ctx.parallel`): the reference boot and the run-time boot of an equivalence scenario; the cases of a power-cut scenario; the
  two boots of the safe scenario. A boot of "no module" is made only when it is the reference (a set without a twin); with a twin the old run made it only to
  report it. A rig holds an exclusive lock on its run directory (one firmware on one card), so two scenarios in parallel never share a daemon socket.
- **Shorter setup**: the rig runs 300 emulated ms after the boot instead of 1500 (`wait_dsp` still waits for a module's DSP part; the panel scripts that save
  a project keep 1500: with 300 the save wrote nothing, the oracle failed as it should); `trig_frame` of the two note scenarios set to the setup's end
  plus 170 frames (3500 for the pluck, 1050 for the DN2 set; a rig that is already past it fails with "raise TRIG_FRAME", as before).
- **Parallel cases** in the pytest files (`DIGIKIT_JOBS`, default 3) and `pk_digikit_scen run --jobs N`; `DIGIKIT_RUN_SALT=x` forces cold boots (timing of a first run).
- The boot cache already is the "booted-state snapshot": the daemon saves the ready pair (ColdFire snapshot and DSP state, taken after the boot hook has run) per
  firmware and card path; restoring it costs 1 s. A snapshot before the hook would restore faster than a cold boot only on a first run (the pair is cached per card anyway) and would
  have to be keyed by the sections and the card bytes outside the store; with 1 s restores it buys nothing, so it was not built. The boot of the DN2 daemon was cold in every run of my first profile only because that run was the first of those cards.

Not built: calling the firmware's save and load functions from a script (the ColdFire-only rig makes the panel route cheap enough and keeps it the real route);
running the patched-in twin and the boot-loaded image in one daemon (they are different firmwares); a no-DSP rig for the equivalence scenarios (the DSP
part, the voice rows and the PCM are what they check).

After (wall seconds, DT2 1.17 / DN2 1.12, scenario alone, warm caches, then cold boots):

| Scenario | before | after (warm) | after (cold boots) |
| --- | --- | --- | --- |
| equiv-dyn-pluck | 73 (cold first run 131) | 20.7 | 40 (in the quick tier run) |
| equiv-dn2-set | 104 (first run) | 14.2 | 22 |
| roundtrip-dt2 | 174 | 37.7 | 44 |
| the three at once (the quick tier) | 174 (the longest) | 38 | 44 |
| full tier, five pytest files at once (as `verify --full`) | about 50 min serial | 250 s | - |

A/B (same scenario, before and after, same oracle): the project of the round trip is bit-for-bit the same. The report of the old coupled run and of the ColdFire-only run
agree in `edit_vs_baseline`, `pattern0_changes` (the changed bytes of pattern 1), `pattern0_changed_bytes`, `save2_vs_save1`, `header_stamps`, and every check but the
selection hint of the panel list (`9 downs` against `11 downs`: the list path differs, the machine reached is the same type 18); `roundtrip-dt2-coupled` passes with
the old route. Equivalence: all checks pass as before, the PCM of the note exact (`exact True`, 0 samples differ) with the shorter setup. The full tier (31 pairs, all four images) passes.

## Open **[O]**

- The header stamp word (what sets it, why the first save differs).
- The preset lock is not decoded; the p-lock only by changed bytes.
- Only the newest image of each device is in the quick tier; the full tier (`verify --full`, `DIGIKIT_TIER=full`) runs every scenario on all four images (about 50 minutes serial).
- Equivalence compares decoded rows, not the raw bytes of the tables (the type numbers, parameter codes and string pointers differ between a run-time and a patched-in set by construction).

## Phase 6 gate: dyn-pluck on both devices, and through the menu (lane r4, 2026-10-17)

**[D]** Native emulator, stock images, nothing ran on a device.

| Scenario (kind) | What it proves | Result, wall clock of one image |
| --- | --- | --- |
| `equiv-dyn-pluck` (equiv, now both devices; quick tier) | `test/dyn-pluck` now builds for the Digitone II: the `own_dsp` machine (permanent type 17, `types.toml` `[dn2.dynamic]`), its SYN page with four knobs and `pluck_dn2.s` as the DSP section. Selected at boot it gives rows equal to the patched-in `digikit/pluck` and a PCM equal from the onset (96 000 samples, 0 differ after alignment, onset shift 64 as in finding 37), key held through the capture | pass dn2-1.12 67 to 83 s, dn2-1.11 32 to 48 s; dt2-1.16 131 s, dt2-1.17 70 s (unchanged result) |
| `menu-pluck-demo` (menu, new kind; full tier) | boot with an empty selection; SETTINGS > DIGIKIT > MODULES; tick `test/dyn-demo` (code only) and `test/dyn-pluck`; APPLY & RESTART (first press: one popup, nothing saved; second: `dkrt_selection_set` 0, soft reset); the next boot: both results 0, the record in use holds the two paths, the machine is in the list with the patched-in `digikit/pluck` rows, one voice row, a note on it equal to the patched-in set (DN2: exact; DT2: 0 differ from the onset), write-watch 0, flush hits 0, card writes in the store range only `0x3dffe` and `0x3dfff` | pass dt2-1.17 153 s, dt2-1.16 187 s, dn2-1.12 146 s, dn2-1.11 81 s |

- **[D]** On the Digitone II the patched-in `digikit/pluck` cannot share an image with the runtime: `pluck_dn2` reserves 256 KiB of delay lines and the runtime's DSP area is 320 KiB
  (`dsp.l2` has 490 948 bytes; 329 364 used by the runtime, 265 668 asked by the module). The reference image of a DN2 pluck scenario is therefore built **without the runtime**
  (`ref_plain = true`; `PlainRig`, the same panel and capture) and compared by rows and PCM only. The run-time module needs no second copy: its delay lines live in the area.
- `digikit/pluck` lists `dn2-1.11` in `ports` now (its target was `untested`); the equivalence passes on it.
- **[D]** The menu scenario needs more than the 120 s that fit the quick tier (the panel script and the restart are emulated time), so it runs in the full tier; the DN2 `equiv-dyn-pluck` joins the quick tier.
