# Handover 2026-11-02: SHOT sampler (branch sdk/dsp-sampler-discovery)

## State

- Branch `sdk/dsp-sampler-discovery`, pushed at 56e272fb (main merged in at b2f2379d).
- `nx.json` has an uncommitted change owned by the Nx session. Leave it. Stage by explicit path only.
- SHOT (`modules/machines/shot`, `digikit/shot`; renamed from SPARK) is a stereo one-shot sampler.
- SMP is parameter word 0 on knob B on both devices. START is C, END E, REV F, PITCH G.
- The knob map is shared across devices. `param_slots`: DT2 [1,2,4,5,6], DN2 [1,2,4,5,6,0,3,7]. The old 4-parameter limit is gone.
- Flow ([ui-rules](../design/ui-rules.md)):
  - Click SMP (release without turn): slot list 00-15, cursor on the current slot.
  - YES on a slot opens the browser.
  - YES on a file loads it into that slot (replace if filled). SMP = slot, back on the page.
  - NO goes up one level. RIGHT is CLEAR/INFO.
  - Turn SMP to choose between loaded slots (p-lockable).
- Generic pieces:
  - Chooser slot-list mode: `sdk/include/dk/chooser.h`, `sdk/src/chooser.c`.
  - Sample picker on top: `sdk/src/sample_picker.c`.
  - Machine-page click hook and `DK_PARAM_ACTION`: [machine-page-click](../design/machine-page-click.md).
  - Runtime `param_get`/`param_set` (stock knob-turn setter).
  - Pool API (`sample_pool_set`/`add`/`clear`/`status`/`info`/`path`): [sample-api-1.5](../design/sample-api-1.5.md).
  - Backing adapter `modules/core/adapters/sample-pool`:
    - DT2 borrows the stock pool: [finding 109](../findings/109-dt2-stock-pool-borrowing.md).
    - DN2 has its own DDR pool. The LP0 page pump runs as a SampleLoader worker job: [dn2-sample-pool](../design/dn2-sample-pool.md).
  - Stereo:
    - DT2 pair gate: [finding 112](../findings/112-dt2-pair-gate.md).
    - DN2 twins all pre-pan stages: [dn2-stereo](../design/dn2-stereo.md).
- Proven in the emulator:
  - `modules/machines/shot/tests/shot-dt2.test.ts`, `shot-dn2.test.ts` and `shot-picker.test.ts` pass.
  - DT2 click flow covers load, replace, NO levels and held TRIG.
  - Em loaded a sample through the slot list in the DN2 workbench emulator and heard it.
- Patch sets: DT2 `device` and `emulator` include core/sample-pool. DN2 has it only in `emulator` (no DN2 flash with the pool yet).

## Next for the sampler (in order)

1. DT2 hardware try:
   - Workbench Firmware pane Flash (DT2 `device` set; restart 1).
   - Transfer SHOT, select it in DIGIKIT > MODULES (restart 2).
   - Upload WAVs via the Samples page, then run the SMP flow.
   - Rollback: flash the main runtime.
2. Persist the pool per project in the digikit store. Today it is in memory only and is lost on power cycle. Project identity: [finding 110](../findings/110-project-identity.md) (slot byte + name; RAM name accessor still open).
3. Show the sample name on the SMP knob: value-text hook in core/params (`modules/core/adapters/params`). Today it shows a number.
4. DN2 hardware:
   - Decide the SPI wire-size change of the LP0 transport (0xabc -> 0xacc).
   - Then read back the reserved DDR range on hardware (canary probe module, 2 restarts).
   - Then add core/sample-pool to the DN2 `device` set.
5. Smaller items:
   - Turning SMP onto an empty slot plays silence (skip holes later).
   - Pool size is 16. DT2 slot ids 0x3f0-0x3ff can collide with a project using bank 7: test before raising.
   - DN2 stereo is capped at 8 slots until one `tools/devtools/coupled_ab.py` A/B.
   - DN2 transport runs 204 pages/s in the emulator (hardware rate unknown).
   - Fixtures still use `sample_pool_add`.
6. Gate and PR to main when ready:
   - `mise run verify -- --base main --explain` selects nearly everything (the branch carries ~200 commits). Propose a targeted `--only` set to Em first.
   - The module-own Vitest project (`tests/regression/vitest.config.ts`) is not wired into verify yet. Coordinate with the Nx session.
   - Pre-existing: the runtime crate's `cleanup_tests.rs` does not compile (`step_list`, `step_n`).

## Side quest agreed, not started: multi-choose mode

- The DIGIKIT > MODULES list (`modules/core/apps/digikit-menu`) is an older check-mark list. NO means "done, apply?". This breaks ui-rules rule 2.
- Plan:
  1. Read-only check of stock DT2/DN2 multi-select conventions (manuals in `out/refs`, firmware where useful).
  2. Mockup of a multi-choose chooser mode and the new MODULES flow in [ui-rules](../design/ui-rules.md), for Em's OK. YES toggles marks. An explicit confirm says when it restarts. NO discards.
  3. Build, host tests, one emulator check.

## Handed to another session

- Emulator test reliability and speed leads, Nx caching of the workbench prerequisites, cargo lock waits, and a DSP-free UI component test layer.
- Notes: `~/src/digi/test-strategy.md`, section "Emulator test reliability and speed leads (from the sampler session, 2026-11-02)" (not in this repo).

## How to work on this branch

- Agents get a wall-clock limit and report instead of waiting.
- Emulator runs have a run budget. Overlap with the Nx session's runs is accepted (wall-clock timeouts can still flake under load).
- Agents never push. The coordinator pushes after Em's OK.
- Iterate UI on DN2 with `mise run dk:dev`.
- Rerun the three SHOT tests from `tests/regression`:
  - `pnpm exec vitest run --project module-own ../../modules/machines/shot/tests/<file>`
  - Set DIGI_EMUD_DT2 and DIGI_EMUD_DN2 from `uv run python -m tools.devtools.verify_build daemons --env-out FILE`.
  - Set DIGI_PATCHKIT_BIN=out/native/target/nogen/release/patchkit.
