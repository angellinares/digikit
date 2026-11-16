# Ledger: the patch-time machine fixtures and their smoke cells are removed

Do not merge before the new tests pass on the device and an independent review. Status: the tests are written and have not run (no builds were allowed on the machine).

Why: only three fixtures used patch-time `[[entry]]` rows of the `machines` registry: `test/machine-clone` (DT2), `test/dn2-clone` and `test/dn2-pages` (DN2). Since R12-6 the machine hooks are in `machines.dks`, and nothing loads that file for a patched-in row (the runtime finds a service file only for a hard NEED of a selected module, `loader/discover.rs`). Every real machine is a run-time `.dkm`. So the three smoke cells fail, and the fixtures tested a path nobody uses. Removed: the three fixtures, their cells in `tests/patchkit/smoke/matrix.toml`, 54 keys of `goldens.json`, the three `select.toml` files, `tools/patchkit/pk_proof_pages_dn2.py`, the clone configs of `pk_tiers.py`, the wasm and Python tests that used them (replaced by other fixtures where the test is generic), and the allocations in `types.toml` (now retired comments). patchkit refuses a patch-time entry of the `machines` registry (`refuse_entries` of `core/machines`; host test `a_registry_that_refuses_entries_names_the_module_and_the_reason`).

## Coverage of what the cells showed

| Behaviour of the cells | Run-time coverage through machines.dks |
| --- | --- |
| Two machine modules get their own types (XClone 8, FM Clone 5, PAGES 6) | `dynreg/tests/equiv.device.test.ts` (`equiv-dn2-set`: dyn-dn2 and sine, voice rows, rows equal the no-module boot) and `roundtrip.device.test.ts` (`dynType` of each machine, kept in every set order, sound records carry the types). Both existed. |
| The menu list has the added entry; YES commits it | `roundtrip` (`pickSine` / `selectDt2` per machine, the sound carries the type), `equiv` (`selectType`). New: `machine-rows-dn2` (audio mirror holds the type after a TRIG) and `machine-rows-dt2` (`trackType` equals the type). |
| DN2 PAGES row placed by the group hook | The group hook acts when the machine's page is built: `machine-rows-dn2` decodes the page row of DynSyn (name, title, knob layout, slots, ranges, defaults). New. |
| DN2 own page: draw, key, turn | New `machine-rows-dn2`: SYN changes the screen (frame against the frame before in the same run), each knob changes exactly one Sound word, upward, and the words are as far apart as the slots. `roundtrip` turns two knobs and keeps the values through a save. Cycling two pages with SYN (`page_a`, `page_b`, `page_a_again`): new `machine-pages-dn2.device.test.ts` with the new fixture `test/dyn-dn2-pages` (a copy of dyn-dn2 with two pages; dyn-dn2 itself is unchanged): the page rows are the declared ones, and after SYN once, twice and three times the first knob edits the Sound word of ALPH, EPSL, ALPH (state, no pixels). |
| DT2 clone: SRC page | New `machine-rows-dt2` (test/sine): the row (`Sine`, knob Level), the SRC knob changes exactly one Sound word. |
| DT2 clone: DSP selector byte at 0x255970 = 8 | Not applicable: a run-time machine has its own voice row and DSP (`voiceRows` in `equiv`, the PCM pin of `equiv-dt2-sine`, pending approval). The selector of a clone of a stock type no longer exists. |
| DN2 clone: TX frame sends FM TONE's type (`clone_of` 0) | Not applicable: a run-time machine on DN2 has `own_dsp`; the type reaches the DSP (`frametype-slot.device.test.ts` proves the slot, `equiv-dn2-set` the sound). |
| Audible note (-60 .. -3 dB) | `equiv-dt2-sine` and `equiv-dn2-set` (PCM pins). |
| `expect_no_fault` | `session.expectNoFault` in both new tests. |

The generic smoke tour in those cells (`tests/patchkit/smoke/tours/smoke.toml`) had the stock hash for every frame of the three cells except `machine_menu/open`, `machine_menu/down` and, on the DN2 clone cell, `track/track2` (`2f3a3cbe5a`, stock `a8ce60f553`, not explained). The stock cells keep the equal frames. The two menu frames showed the list with one more entry, which the new tests prove by selecting the entry. The `track2` difference belongs to a path that no longer exists.

## New tests (`modules/core/runtime/tests/machines/`, project `core/runtime-machines-tests`)

- `machine-rows-dn2.device.test.ts` (DN2, test/dyn-dn2): one boot, SHARC for the module's DSP part.
- `machine-rows-dt2.device.test.ts` (DT2, test/sine): one boot.
- `machine-pages-dn2.device.test.ts` (DN2, test/dyn-dn2-pages): one boot, SHARC for the DSP part. Open: `[uses.dn2]` of the fixture is computed by hand (`mise run module:check test/dyn-dn2-pages` rewrites it and writes `build/gen`); no firmware word with the current page index was found, so the page shows in what a knob edits.

Open points for the review

1. The slot-to-Sound-word mapping (slot n is halfword n of the Sound array from +0x14 of the Sound object) is assumed; the DN2 test asserts only distances between words.
2. The DN2 audio mirror address 0x8000487c is the one of the removed `select.toml` (DN2 1.12). The list position 5 is the one of the round trip test.
3. The decoded row expectations (names, titles, ranges) come from `build/gen/gallery.json` of `test/dyn-dn2` and the fixture sources; not seen on a device yet.
