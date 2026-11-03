# test/ui-demo

SETTINGS > DIGIKIT opens a menu with one row of each kind of core/ui: a toggle, a value, a toast, a confirm, a choice, a text page, a list
(check marks, a side panel with two actions) and an empty list. It conflicts with core/digikit-menu (the same hook). Driven by
`uv run python -m tools.patchkit.pk_uidemo shots|leak --device dt2|dn2`.

The named acceptance target `pnpm nx:pilot run sdk-ui-zero-tests:test-device` runs this fixture on DT2 1.17 and DN2 1.12 with zero DSP and no audio session. It patches only this fixture and its requirements, then checks row edits, text scrolling, list checks and panel focus, back refusal, the empty list, confirm and choice answers/cancellation, and toast expiry. Callback state and OLED comparisons are recorded through the shared runtime harness. The Python tool remains available for screenshots and heap analysis.
