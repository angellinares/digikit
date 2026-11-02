# Sampler follow-ups

Status: backlog after the SHOT sampler (branch `sdk/dsp-sampler-discovery`), collected on 2026-11-03. Pick items from here; each names its source. Larger items have their own plan or design note.

## Plans already written

- [digikit-project](digikit-project.md): per-project `.dkp` in the store, the module persistence API (module and project scope), project validation and missing-module warnings. Related: [digikit-hot-reload](digikit-hot-reload.md).
- [device-ab-harness](device-ab-harness.md): the reusable device/emulator A/B harness and the `wb` CLI.
- [param-views](../design/param-views.md): live and latched parameter views in the voice adapter (needs Em's OK).

## SHOT and the sampler engine

1. **Sample name on the SMP knob.** Value-text hook: a per-row formatter pointer (+0x34) for added parameter rows ([capabilities](capabilities.md), "Value text per parameter"). First disassemble the formatter call (DT2 `0x400c897a`, DN2 `FUN_400c25a0`) for its arguments and buffer size. The stock sample glyph (row 205) was deferred: it shows only a bank letter and number, needs SMP stored raw, and DN2 has no stock row that prints a raw value.
2. **Empty slots.** Turning SMP onto an empty pool entry plays silence; skip holes.
3. **Loop position like stock.** Stock loops between a LOOP position and the end ([finding 114](../findings/114-stock-one-shot-playback.md)); SHOT loops [START, END). Decide whether SHOT gets a LOOP parameter.
4. **Adjustable fade in and out.** `sdk/dsp/gain_ramp.inc` already carries a ramp across blocks (`[start, step]` written back). Stock has a fade-in flag (+0x17c) whose setter is [O].
5. **Loop budget.** The loop option raised SHOT's per-block ceilings to 7000/7200 (`modules/machines/shot/src/dsp/shot.s`); the worst case is a 1-frame loop at high pitch that wraps on every output. Options: a minimum loop length (stock clamps the start to at most length - 141), a cheaper wrap path.
6. **PLAY on DN2.** DN2 has no stock PLAY row; PLAY shows as an encoder 0..3. A custom glyph would fix it (Em wants own glyphs later).
7. **Pool size.** 16 entries. DT2 borrowed slots 0x3f0-0x3ff can collide with a project that uses bank 7: test before raising.
8. **Fixtures** still use `sample_pool_add`.
9. **Track level difference.** In the emulator track 2 played 9.2 dB louder than track 1 for both stock and SHOT (finding 114): find the cause.

## DN2 hardware

10. **LP0 transport:** decide the SPI wire-size change (0xabc to 0xacc), then read back the reserved DDR range on hardware (canary probe module, 2 restarts), then add core/sample-pool to the DN2 `device` patch set.
11. DN2 stereo is capped at 8 slots until one `tools/devtools/coupled_ab.py` A/B.
12. The DN2 transport runs 204 pages/s in the emulator; the hardware rate is unknown.
13. `modules/machines/shot/tests/shot-dn2.device.test.ts` fails with "no reply from the device" during the upload stage under host load (seen before and after the main merge; passed on a rerun once).

## SDK

14. Live and latched parameter views ([param-views](../design/param-views.md)).
15. Custom knob glyphs beyond the stock rows (`DK_PARAM_GLYPH` covers stock rows only).
16. A sampler helper that applies `DK_SAMPLE_STOCK_GAIN` and the end ramp, so new sampler engines match stock level by default.
17. The module persistence API (in [digikit-project](digikit-project.md)).

## Runtime and storage

18. Measure the browser listing time on the device after the per-pass change (it showed READING for about 2 s before).
19. Storage items from [storage](storage.md) not yet proven on hardware: the store area on a used card, the close trailer, the MidiRpc chunk size on real USB, eMMC capacity. A sample upload through `wb samples put` worked on a DT2 (hash verified).
20. DT2 loader locking against stock tasks ([finding 109](../findings/109-dt2-stock-pool-borrowing.md), [O]); a false "pool lost" would now show as POOL_LOST in the runtime log.

## UI

21. **Multi-choose mode** for DIGIKIT > MODULES (agreed side quest, not started). The list is an older check-mark list where NO means "done, apply?", which breaks rule 2 of [ui-rules](../design/ui-rules.md). Steps: read-only check of stock multi-select conventions; a mockup in ui-rules for Em's OK (YES toggles marks, an explicit confirm says when it restarts, NO discards); build, host tests, one emulator check.
22. The workbench Remote page polls the screen every 400 ms (about 2 fps): measure the round trip and speed it up (part of the harness).

## Tools and tests

23. The [device-ab-harness](device-ab-harness.md) plan: `wb screen`, `wb key`, `wb turn`, `wb param`, navigation with feedback, fixtures, report; then retire the Python device scripts.
24. `patchkit flash` can wait silently for minutes after the device restarts (`packages/patchkit/src/flash_cli.rs`).
25. The 13 sampler and DSP proof tests in `tests/regression/runtime/*.test.ts` run only by name (Vitest project `proofs`): move them into their modules' tests folders and register them.
26. The runtime crate's `cleanup_tests.rs` does not compile (`step_list`, `step_n`).
27. Emulator test reliability and speed leads were handed to another session (`~/src/digi/test-strategy.md`, outside the repo).

## Merge

28. After the testing refactor (another session) lands on main: rebase this branch, run the routed gate (all steps: the branch carries about 5,800 lines of emulator changes that no gate has run), then the PR.
