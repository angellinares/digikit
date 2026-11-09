# test/ui-demo

SETTINGS > DIGIKIT opens a menu with one row of each kind of core/ui: a toggle, a value, a toast, a confirm, a choice, a text page, a list
(check marks, a side panel with two actions) and an empty list. It requires `core/ui` and conflicts with `core/digikit-menu` (the same hook); this is a controls fixture, separate from the real DIGIKIT menu.

From the repository root after [setup](../../../../docs/DEVELOPMENT.md#setup), with the DN2 firmware available:

```sh
mise run dk:ui-demo
```

This starts or reloads the kept DN2 emulator with zero DSP (no audio), navigates to the demo and opens a local browser faceplate. The command prints the exact panel URL. Keep it running while using the panel; Ctrl-C stops the browser server and leaves the emulator alive. Stop the emulator with `mise run dk:dev -- test/ui-demo --stop`. For the real menu, use `mise run dk:menu`; see [visible previews and module iteration](../../../../docs/DEVELOPMENT.md#visible-preview).

For screenshots without opening a browser, or to check DT2:

```sh
mise run dk:dev -- test/ui-demo --device dt2 --keys 'tap:NO,tap:SETTINGS,wait:400,tap:DOWN*8,tap:YES,wait:600,shot:menu'
pnpm nx:pilot run sdk-ui-zero-tests:test-device
```

The preview is for iteration; the named device target is the acceptance check. It runs this fixture on DT2 1.17 and DN2 1.12 with zero DSP and no audio session. It patches only this fixture and its requirements, then checks row edits, text scrolling, list checks and panel focus, back refusal, the empty list, confirm and choice answers/cancellation, and toast expiry. Callback state and OLED comparisons are recorded through the shared runtime harness. The Python tool `uv run python -m tools.patchkit.pk_uidemo shots|leak --device dt2|dn2` remains available for screenshots and heap analysis.
