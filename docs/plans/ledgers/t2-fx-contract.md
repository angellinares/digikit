# Ledger: FX contract suite (T2 step 2, lane 1)

Status 2026-11-16, phase 2 (host checks only): the fixture packs and `patchkit gen` reproduces its `build/gen` files unchanged; lint, format, types, layers and the Python checks pass; the device files are registered and selected (`mise run test:plan`). No device or emulator run yet. No test is deleted yet. Deletions go in their own commits, after the
replacement passes on DT2 OS 1.17 and DN2 OS 1.12 and an independent review has signed the rows below.

Line format: `old file:line | assertion | -> new file: place | or KEPT | or DROPPED: reason`.
New files: `B` = `modules/core/adapters/fxpages/tests/fx-bipolar.device.test.ts` (fixture `test/dyn-fx-gain-bipolar`), `D` = `modules/core/adapters/dynfx/tests/fx-contract.device.test.ts`
(fixture `test/dyn-fx-gain`). Old files: `F` = `modules/fx/freqshift/tests/freqshift.device.test.ts`, `C` = `fx-chain.device.test.ts`, `R` = `fx-reuse.device.test.ts` (both in `modules/core/adapters/dynfx/tests/`).

## Boot count per device (cards booted in one test run)

| Suite | Before | After | Note |
| --- | --- | --- | --- |
| freqshift adapter legs | 4 (F: dry, master, send, other) | B 4 (dry, master, send, other on the bipolar fixture) and F 3 (the `other` boot is gone from F now) | F keeps its dry, master and send cards for the PCM oracle |
| dynfx chain and reuse | 3 (C: plain, dry, full) + 4 (R: initial, wet, restart, restored) = 7 | 5 (D: plain, dry, wet, deselect restart, full) | `restored` dropped |
| fx send, routes panel, routes persist (core/fx) | 5 + 5 + 5, DN2 chord 3 more | not changed in this step | see "Not merged" |

The ledger estimate of the plan (about 92 to about 30 boots) needs the merges in "Not merged" and one more step: several PCM captures per boot. See the open questions of the report.

## F (freqshift adapter legs) to B

| Old | Assertion | New |
| --- | --- | --- |
| F:110 | page count after the stock pages is 4 on master route | B: master, `fxp_count` |
| F:111-113 | page list: three stock pages (DT2 13, 14, 15; DN2 17, 18, 19) and the effect page | B: master, `fxp_list_buf` |
| F:116-126 | page row words: first parameter index, empty cells, MIX at `first + 7` | B: master, `fxp_rows` |
| F:127-128 | tab and title strings | B: master (`GBP`, `Test Gain Bipolar`; the strings are the fixture's) |
| F:133-134 | template words: value index, default, maximum, default raw (signed 0x4000, MIX 0x7f00) | B: master, `pf_table` (signed 0x4000 comes from the fixture default 0 of -64 .. +63) |
| F:135-140 | knob names and tabs (`Freq Shift`, `SHIFT`, `Mix`, `MIX`) | B: master (`Bias`, `BIAS`, `Mix`, `MIX`) |
| F:143-146 | kit raw values, initial row words, route words, `SEND_SLOTS` unused | B: master |
| F:150-166 | MIX endpoints 0 and 0x7f00 after sixty fast events; MIX word 0 or 1.0 | B: `endpoint` |
| F:168-176, 180-183 | signed knob up and down: row word is the float of the raw value; no fault | B: `turn` |
| F:184-202 | kit save, change, load: raw values restored, row word restored | B: `saveKit`, `loadKit` |
| F:204-208 | send card: route word 1, one send slot used, two send pages | B: send |
| F:213-222 | send amount 40 on track 1 reaches the row only as amount byte 0 | B: send |
| F:223-229 | signed value on the send route; route word stays 1; amount stays 40 | B: send |
| F:233-236 | MIX raw 0x4000 gives the float `raw / 0x7f00` within 1e-4 | B: send |
| F:241-256 | other-track amount 100 on track 2 only; signed value reaches the row word | B: other. F lost this boot and the oracle check `other:` of `pk_digikit_fxshift` (its PCM part: track 2 amount adds nothing to track 1, bit for bit) is covered by the `other` card of `fx-send.device.test.ts` |
| F:260-270 (rest), analysis `pk_digikit_fxshift` | PCM oracles of the shifter (spectra, image rejection, send and half levels) | NOT MOVED to B. B checks the gain law on the master as relative PCM (`compare`). The shift itself belongs to the rig sound checks (`tests/rig/freqshift.toml`) and the model tests; the oracle stays in F until the trim step |
| F, adapter-specific | master and send spectra with a real module | KEPT in F (trim step) |

Not covered by B on purpose: audible send PCM with the bipolar knob (the send law is proven with `test/dyn-fx-gain` in `fx-send.device.test.ts`).

## C (fx-chain) and R (fx-reuse) to D

| Old | Assertion | New |
| --- | --- | --- |
| C:62-67 | plain image loads the sine and renders an audible reference | D: `plain image` |
| C:92-95 | dry card: no effect used, every free row is `FREE_PC` | D: dry |
| C:96-97 | hooked-dry PCM equals plain PCM bit for bit from the onset | D: dry (`compare ... 'plain-dry'`) |
| C:99-106 | full card: `1 + slots` modules, the overflow result -34, `FX_SLOTS` used = slots | D: full |
| C:110-120 | per row: exact pc, descriptor, state, enabled, defaults against the independent oracle | D: full |
| C:121-125 | pairwise disjoint DSP ranges of the full chain | D: full |
| C:126-129 | `DKRT_TABLE_BAD` class is 0 and no `TABLE_BAD` event | D: full |
| C:130-137 | live and empty fit descriptions of the overflow (`FX 1 > 0 FREE`) | D: full |
| C:138-140 | full chain gain is `0.5 ** slots` | D: full |
| C:34-54 | capture helper: note at the fixed frame, audible dry/plain/wet | D: `capture` (same code) |
| R:28-29 | initial card (dry) boots with the effect absent | D: dry |
| R:35-38 | clean reload keeps the owned daemon, epoch grows, old protocol handle is dead | D: wet, dry to wet |
| R:39-42 | wet card exact boot state, wet PCM | D: wet |
| R:43-45 | gain row words equal the oracle after a note | D: wet (`expectGainRow`; the oracle is read from the card file) |
| R:46 | wet PCM is half the dry PCM | D: wet |
| R:47-53 | call counter above 1000 and 90 to 110 calls per 100 frames | D: wet |
| R:54 | closing evidence of the wet case (guard, non-finite, fault, overlay) | D: wet (`context.evidence`) |
| R:57-82 | protocol select of the sine alone, restart, wait for boot, deselected PCM equals dry | D: deselect restart |
| R:87-94 | reset to the dry card: same semantic state as the first boot; restored PCM equals dry | DROPPED: it tests the fixture's reload, not the product (a clean reload is already used for every card of D). D proves the dry card once and the clean full reload after the restart |
| R:90 | `resetState.semantic` equals `initialState.semantic` | DROPPED with the leg above |

## Not merged in this step (phase 2 decision)

`core/fx` (`fx-send`, `fx-send-dn2`, `fx-routes-panel`, `fx-routes-persist`) and `core/fxpages` (`fx-pages`, `fx-kit-persist`, `fx-track-route-*`) already run on fixtures and are per
adapter. A merge saves one dry boot per file (dry reference shared) and the `zero` send card (covered by `other`). It needs measured proof that two captures can share a boot (the release
tail of the sine). Until then these files stay as they are.

## Review

| File | Reviewer | Verdict |
| --- | --- | --- |
| (none yet) | | |
