# Ledger: three patchkit smoke cells move to TypeScript device tests

Do not merge before the new tests pass on the device and an independent review. Status: the tests are written and have not run (no builds were allowed on the machine). The deletion is the last commit of the branch `test/machines-ui-device-tests`.

Why: the Python harness patches only the image. It puts neither `core/runtime` nor `machines.dks` on the card, so since R12-6 the machines hooks (now in `machines.dks`) are inactive in these cells and they fail. The TypeScript harness boots the runtime image and puts bound services on the card.

Cells removed from `tests/patchkit/smoke/matrix.toml` (54 keys of `goldens.json`, the three `select.toml` files, and the three fixture lines in the inputs of `tools/gate/smoke/project.json`):

- `dt2-1.17:core/machines+test/machine-clone` (18 keys) -> `modules/core/services/machines/tests/machines-dt2-clone.device.test.ts`
- `dn2-1.12:core/machines+test/dn2-clone` (16 keys) -> `machines-dn2-clone.device.test.ts`
- `dn2-1.12:core/machines+core/params+test/dn2-pages` (20 keys) -> `machines-dn2-pages.device.test.ts`

The fixtures stay (patchkit Rust tests use them). Each new test boots one image of its own (image root sets `rt-machine-clone`, `rt-dn2-clone`, `rt-dn2-pages` in `tests/regression/runtime/image-roots.json`), with `machines.dks` put on the card by the rig option `services: ['machines']` (no module is selected). The root sets are in the project's `tests/image-roots.json`. The tests assert firmware state (decoded machine, page and parameter rows, the track's machine type, the audio mirror, Sound words, the DSP selector byte). A frame is compared only with another frame of the same run.

## Cell-specific assertions (the `select.toml` tests)

| Cell assertion | New home |
| --- | --- |
| DT2 `list` frame: XClone is the entry after seven DOWN | `machines-dt2-clone`: `pickSine(rt, 7, 12)` then the track type is 8 (the list entry exists at position 7 and YES commits it). The list frame itself is not kept: the entry is proved by the committed type. |
| DT2 `expect_mem 0x4265338c mask 0xff00 eq 0x800`: track type 8 | same: `trackType(session, 1)` is 8 |
| DT2 `src_page` frame | `machines-dt2-clone`: the XClone row has MANUAL SLICE's descriptor ids (knobs 0xf8 0xf9 0 0xfb 0xfc 0xfd 0 0xfe, level 0x0a, `clone_of` 0); after SRC, knob A and knob B each change one Sound word (not the same one) and A moves up. No frame. |
| DT2 `expect_pcm_db` -60 .. -3 | `notePeak` is above 0.001 (-60 dB). The upper bound (-3 dB) is not asserted: a loud clone is not a failure of the machines service. |
| DT2 `expect_dsp 0x255970 hex 08` | same address, `dspBytes(.., 1)` equals `[0x08]` |
| DN2 clone: FM Clone is the 5th entry; YES commits type 5 | `machines-dn2-clone`: `pickSine(rt, 4)` (four DOWN); the audio mirror byte 0x8000487c is 5 after the note |
| DN2 clone: TX frame sends FM TONE's type (`expect_mem 0x80005ef4 mask 0xffff0000 eq 0`) | same address, upper half is 0 |
| DN2 clone: `expect_pcm_db` -60 .. -3 | `notePeak` above 0.001 (upper bound as above) |
| DN2 clone: row | new: the row of "FM Clone" (`clone_of` 0, `own_dsp` 0, no descriptor ids, level 10) |
| DN2 pages: PAGES is the 5th entry (group hook places its row in the machine menu) | `machines-dn2-pages`: `pickSine(rt, 4)` then the audio mirror is 6 after a TRIG |
| DN2 pages `page_a`, `page_b`, `page_a_again` frames (names and titles ALPH BETA GAMA DELT / EPSL ZETA) | new: the decoded page rows PGA and PGB: titles, knob layout, parameter names and slots (25 26 27 30 / 40 41), the range and default of GAMA. Draw and key: `frameHash` after SYN: page B differs from page A, the third SYN equals the first. |
| DN2 pages `knob_moved` frame (ENC A turned) | new: turning the knob of ALPH changes exactly one Sound word, upward, inside the range; DELT's word is as far from it as the slots differ (5); after SYN, the knob of EPSL changes the word 15 slots above ALPH's |

## Generic smoke tour in each cell (`tests/patchkit/smoke/tours/smoke.toml`)

The tour runs on every cell. Checked against the stock cell of the same device in `goldens.json` (`dt2-1.17/stock`, `dn2-1.12/stock`): every frame of the three cells has the stock hash except the ones listed below. So these segments only check that the added machine changes nothing visible; the stock cells keep that check, and the new tests boot the same runtime image.

| Segment, frame | Cells | Home or reason |
| --- | --- | --- |
| `boot/main`, `track/track1`, `pattern/*`, `play_stop/*`, `machine_menu/closed`, `save_reload/*`, `stress/end`, `audition` | all three | equal to stock: kept by the stock cells |
| `machine_menu/open`, `machine_menu/down` (differ from stock: the list has one more entry) | all three | the list contents: each new test picks the entry by its list position (DT2 7, DN2 4) and reads the committed type |
| `track/track2` (DN2 clone: `2f3a3cbe5a`, stock `a8ce60f553`) | DN2 clone | unexplained difference (DN2 pages and stock agree). Open point 3 |
| `expect_no_fault` | all | `session.expectNoFault` at the end of each new test |

## Open points for the review

1. The index mapping of a slot to a Sound word is assumed (slot n is halfword n of the Sound array, from +0x14 of the Sound object). The test asserts only relative distances, so a constant offset does not matter.
2. The DT2 and DN2 addresses (0x4265338c via `trackType`, 0x255970, 0x8000487c, 0x80005ef4) are the ones of the removed `select.toml` files, for DT2 1.17 and DN2 1.12.
3. `track/track2` of the removed DN2 clone cell differs from stock and from the DN2 pages cell, and nothing in this ledger explains it (open check 5).
4. The runtime finds a service file only for a hard NEED of a selected module (`loader/discover.rs`). With no module selected, `machines.dks` may not load, and the three tests then fail in their first behaviour assertion. The device run decides; if it fails, select a module that needs the service.
5. Open check: `track/track2` of the DN2 clone cell. The DN2 clone test logs the frame hash, both Sound arrays and the decoded rows for TRK + TRIG 2 before any selection (artifact `track2-frame.json`, and the console). Compare with the same dump of a stock boot.
