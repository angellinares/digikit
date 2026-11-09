# 65. Manifest routes and DIGIKIT > FX ROUTES (FX routing step 4, Digitakt II 1.17, 2026-10-25)

A run-time effect is a master insert or a send effect (finding 62), and the amounts of a send are the framework's (finding 64). Step 4 gives the effect's author a way to say where the effect starts and which routes make sense, and gives the user a page to choose
(DIGIKIT > FX ROUTES). Marks: **[D]** measured or read once by the author (no second agent has checked it), **[O]** open. Pieces: `packages/patchkit` (`typed.rs` `FxDef`, `register.rs` `fx_routes`, `dkmdsp.rs` effect flags), `modules/core/runtime` (`dsp.rs` `fx_flags`,
`fxsend.rs` `routes_of`, `saved_route`, `fx_entry`; `fxpage.rs` `rebuild_list`; exports `dkrt_fx`, `dkrt_settings_flush`), `modules/core/apps/digikit-menu` (the page), the fixtures `tests/fixtures/modules/dyn-fx-gain-send` and `dyn-fx-gain-sendonly`, the scenario
`REF:tests/digikit/scenarios/fx-routes-dt2.toml` (`tools/patchkit/pk_digikit_fxroutes.py`).

## The manifest rule **[D]**

`[[fx]] route` is the default route, `"master"` (when left out) or `"send"`. `[[fx]] routes` lists the routes the user may choose. Left out it is `["master"]`; with `route = "send"` it is `["send", "master"]`, so a send effect can always fall back to the master (the master has no slot limit)
and an effect that says nothing is an insert that cannot move. `routes = ["send"]` makes a send-only effect. `patchkit pack` refuses: a name that is not `master` or `send`; an empty `routes`; a route listed twice; a `route` that `routes` does not list; any send route on an effect without a `tab`
(the send knob and the FX ROUTES row are labelled with the page's tab, so an effect without a page is an insert). Unit tests: `register::tests::the_routes_of_an_effect_follow_the_manifest_rule_and_bad_ones_are_refused`, `dkmdsp::tests::the_routes_are_the_flags_of_the_record_and_zero_is_master_only`.

## The record **[D]**

The effect record's flags word (offset 16, always 0 before) carries both: bit 0 `FXF_SEND` (send allowed), bit 1 `FXF_NOMASTER` (master not allowed), bit 2 `FXF_DEFAULT_SEND`. The word 0 that every earlier record has means master only, default master, so an old `.dkm` loads as it did and a module that
leaves the fields out packs to the same bytes as before (the unit test pins word 0 for master only). Values the packer writes: 0 master only; 1 both, default master; 5 both, default send; 7 send only. The packer's parser and the runtime's loader refuse other combinations (`E_DSP_FORMAT`): an unknown bit, a default
that is not allowed. No contract bump: the contract is still fx 1.0, because the layout only gives meaning to a word that was 0 (a runtime older than the flags ignores them and runs the effect on the master; **[O]** a send-only effect on such a runtime is an insert). The runtime keeps the flags per effect row (`DKRT_DSP.fx_flags`) and the allowed routes and the default per row in `DKRT_FXS` (`allow`, `dflt`).

## What the runtime does with it **[D]**

`fxsend::apply` (when the effect row is registered, at the boot): the route is the stored `fx/route/<module id>` when it is 0 or 1 and the effect allows it, else the manifest's default. An effect with no setting and `route = "send"` starts as a send effect. A send route needs a free send cell
(`send_slots`, CAPACITY `SEND_SLOTS`); an effect past the limit stays on the master, also a send-only one **[O]** (the menu does not offer a send past the limit, but a default can exceed it). On an image without send cells (the Digitone II until step 6) every effect stays on the master and the page lists
it as saved. `fxsend::fx_entry` is behind the new export `dkrt_fx(index, DkrtFx *)`: the effects that have a page, in row order, with the tab, the module id, the allowed routes, the default, the route in force and the route the settings give now. `dkrt_settings_flush()` writes the settings record at once
(the menu needs the write to land before the restart; the 2 s delay would lose it).

**The order of the FX view.** `fxpage::rebuild_list` builds the list of FUNC + FX as DEL, REV, CHO, the send effects, the master effects, each in row order. `fxsend::apply` calls it again once the route of the row is known (`fxpage::relist`), so the list is right when the main view is built; no new hook.

## The page **[D]**

SETTINGS > DIGIKIT, row 2 (after MODULES; ABOUT, HEALTH, MEMORY, DEBUG and REMOTE moved down by one: every scenario and proof that opens one of them presses one DOWN more). Rows:

| Row | Text | YES |
| --- | --- | --- |
| an effect | `<TAB>  SEND` or `<TAB>  MASTER` (tab, two spaces, the pending route), at most 11 characters | switches the pending route to the other allowed one |
| a master-only effect | `<TAB>  MASTER` | message row `<TAB>: MASTER ONLY` (16 characters) |
| a send-only effect | `<TAB>  SEND` | message row `<TAB>: SEND ONLY` |
| a switch to SEND with the slots full | | message row `NO SEND SLOT`, nothing changes; the count includes the pending routes of the other effects, so moving a send effect to the master frees a slot for another |
| no effect | `NO EFFECTS LOADED` | nothing |
| `APPLY & RESTART` | only when a pending route differs from the saved one (the stored route when allowed, else the default) | first press: popup `DEVICE WILL RESTART` (19 characters, 29 fit), the row reads CONFIRM RESTART; second press: `dkrt_settings_put` of `fx/route/<module id>` (1 byte, 1 send, 0 master) for each changed row, `dkrt_settings_flush`, `dkrt_restart` |

A refusal is shown as in MODULES (`NOT SAVED: ERROR -66`, `NO RESTART: BUSY`). The rows and the pending routes are kept in `DK_FXR`, `DK_FXR_N` and `DK_FXR_PEND` for the proofs. The offer is judged against the saved route, not the route in force: a send-only effect past the send slots runs on the master but is saved as send, and an offer
against the route in force would never go away. Two effects of one module share the module's setting (the key is per module id). An effect without a page is not listed.

## Proof **[D]** (scenario `fx-routes-dt2`, quick tier, `pk_digikit_scen run fx-routes-dt2 --image dt2-1.17`)

All 47 checks pass (135 s, 29 emulators, 8 parts of one boot each; the write-watch and flush checks over every boot are clean, the guest wrote only the selection and settings sectors in the store range). Fixtures: `test/dyn-fx-gain` (no route fields), `test/dyn-fx-gain-send`
(`route = "send"`), `test/dyn-fx-gain-sendonly` (`routes = ["send"]`), and the second again under another module id.

- **Old records load as master only.** `test/dyn-fx-gain` has no route fields: its record has flags 0 (unit test), the runtime holds allow = master, default = master (`DKRT_FXS`), the row's route word is 0, and the FX ROUTES row says `GAN  MASTER` (part `list`). The stock effect of every earlier scenario
  (`fx-route-dt2`, `fx-dyn-dt2`, `fx-persist-dt2`, `fx-pages-dt2`) is this module.
- **A module with `route = "send"` and no stored key starts as a send effect.** `test/dyn-fx-gain-send` with no setting: allow = master | send, default = send, row route word 1, the FX key's count cell 2 (page 2 exists), CAPACITY `SEND_SLOTS` 1 of 2 used (parts `list`, `apply`).
- **A stored route that the effect does not allow falls back to the default.** A stored `send` for the master-only effect and a stored `master` for the send-only one: route words 0 and 1, and the page shows `GAN  MASTER`, `GSO  SEND` (part `fallback`).
- **The FX view's order.** With a master effect in row 0 and a send effect in row 1 the list the runtime hands the firmware is the three stock pages, then row 1, then row 0, and FUNC + FX shows `Test Gain Send (4/5)` and `Test Gain (5/5)` (`step4-fxview-4.png`, `-5.png`).
- **The page.** `none`: `NO EFFECTS LOADED`. `list`: `GAN  MASTER`, `GSN  SEND`; YES on the master-only one gives the message row `GAN: MASTER ONLY` and no pending change; YES on the other gives `GSN  MASTER`, clears the message and shows APPLY & RESTART; YES again goes back and the offer
  goes. `sendonly`: `GSO: SEND ONLY`. `full` (three effects, two send slots, the third saved master): `NO SEND SLOT` with nothing changed; after the first effect is moved to the master the third may go to send (`GSN  MASTER`, `GSO  SEND`, `GSN  SEND`), APPLY & RESTART shown, nothing written.
- **APPLY & RESTART.** First press: the popup `DEVICE WILL RESTART`, `DK_FXR_APPLIES` unchanged. Second press: the key is written, flushed, the device restarts. Part `apply` (send default to master): after the restart the module loaded (no safe mode), the route word is 0, the count cell 0 (the FX key has one page:
  frames `apply-fx1-master` and `-again` are equal), no send slot is used, `patchkit store dump` on the card as the guest left it shows `fx/route/test/dyn-fx-gain-send` = master, and the PCM is the dry PCM times the effect's gain 0.5 (rms -6.0206 dB, 0.50 ulps, onset shift 0), where before the restart, on the send route with no amount,
  it was the dry PCM bit for bit. Part `back` (stored master to send): route word 1, count cell 2, one slot used, the dump shows `send` (copy B, seq 2), PCM equal to the dry PCM bit for bit; the FX key has two pages (`back-fx1-send`, `back-fx2-send`). The two directions are two boots: a second restart inside the first 30 s of a
  session is the second unstable boot in a row and the third boot is safe mode (the first try of the scenario, with both restarts in one boot, loaded no module after the second restart: `[-44, -44]`, SAFE MODE; that is the runtime's rule, the page does not change it).
- **The effects of the menu change on the other pages.** The pinned frames of the main DIGIKIT list moved (smoke cells `core/digikit-menu`: `digikit`), every scenario that opens ABOUT, HEALTH, MEMORY or REMOTE presses one DOWN more, and the smoke `menu` tour opens FX ROUTES and checks `NO EFFECTS LOADED` (`DK_FXR`).
- **`digikit_menu_proof` (dt2-1.17 and dn2-1.12) brought up to date.** The proof is not a gate step, and it had failed on main (09d0231c) since 23 Oct: its pins date from 21 Oct, before the size column and the load line of MODULES and the runtime 0.5.x text of ABOUT, and its two presses to APPLY & RESTART landed on the load line. Fixed: one DOWN more to ABOUT, and
  three (four in safe mode) to APPLY & RESTART. All checks pass; the 14 frame pins of each image were viewed and re-pinned: `digikit`, `digikit_main`, `digikit_about/digikit_main`, `back_main`, `safe_main` (the new FX ROUTES row), `safe_popup` (same list under the popup), `list_empty` and `list_after_boot` and `safe_list` (the size column
  `<1K`, the load line, `[X]*` marks: older than this change), `list_ticked` and `confirm_popup` (the load line, the cursor on APPLY & RESTART), `about`, `about_after_boot`, `about_bad` (RUNTIME 0.5.1).

Screenshots (3x): `out/runs/lanes/fxr-shots/step4-*.png`: `none`, `list`, `master-only`, `switched`, `send-only`, `no-send-slot`, `pending-counts`, `fxview-1` to `fxview-5`, `apply-*` (before and after each restart, the popup).

## Open **[O]**

- **[C]** The Digitone II has one send cell since step 6 (finding 67): `fx-routes-dn2` runs the whole scenario with the limit 1 (the `full` boot has two effects and `NO SEND SLOT` on the second). The FX view's order needs no hook on either device (`fxpage::relist`).
- A send-only effect past the send slots runs on the master (above). A module with two effects shares one route setting.
- A power cycle within 2 s of a settings change loses the change (the write waits 2 s). A restart through `dkrt_restart` (MODULES' APPLY & RESTART, FX ROUTES, the mailbox) does not: it writes a pending settings record first (`settings::flush_if_dirty`; scenario `settings-dt2` part `restart`: a put and at once a restart keep the value). FX ROUTES also flushes by itself so that it can show a write error before it restarts.
- A second agent checks the record flags and the list order against the image bytes before the marks become **[V]**.
