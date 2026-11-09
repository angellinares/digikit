# Default modules installed with the firmware (2026-10-31)

Status: built on branch `ui/log-controls-and-defaults`. Host checks and screenshots are done. Nothing ran against a real device or a live emulator [O].
Desktop workbench only. The site has no module install and shows no list.

## What it does

A **Default modules (N) ▾** drop-down stands in two places: in the Firmware pane next to the file chooser, and in the workbench toolbar next to Open… (the emulator's firmware file). It lists the modules that are installed and enabled together with the firmware. Each entry is ticked by default; the user can untick it (kept per device in `localStorage` `digikit.defaultModules.unticked`).

- **The list is data**: `[digikit] default_modules = ["digikit/debug", "digikit/remote"]` in `devices/digitakt-ii.toml` and `devices/digitone-ii.toml`. The one-line text of an entry is the `summary` of the module's own `module.toml`. Another module is one more id in each device file.
- **Device (Firmware pane)**: Flash is still one button. After the flash the page waits until the runtime answers INFO (the flash verdict already needs that), then for the ticked modules: upload and VERIFY each built `.dkm` of `out/modules/<device>/` (`dktransfer.install`, the Transfer pane's code), then SELECT the modules that are not loaded yet together with the ones that are, and RESTART once (`restartDevice`). The progress bar of the Done step shows the module in work. A failure names the module and the reason; the flash stays done and the module can be installed on the Transfer page. Restore stock installs nothing.
- **Emulator**: after the workbench loads a firmware with "Boot with digikit runtime" on (`boot()` in `Console.tsx`: Open, Reload, file watch), the same install runs on the emulator's session (`emulatorSession`). The card keeps the files, and the mirror keeps the selection as `digikit/selection.txt` in the device folder, so a later load only checks them (same sha256: no upload; already loaded: no select, no restart). The first load of an empty folder still boots twice: it has no card to select on before the first boot. Progress is in the status line, the result in the Log (source `firmware`).
- Nothing in the Elektron files changes: the modules and the selection are digikit's own store and selection record.

## Files

- `apps/web/src/firmware/default-modules-core.ts` (pure: the unticked choices, `installDefaults` over `DefaultOps`), `default-modules-state.ts` (signals, the run after a flash), `DefaultModules.tsx` and `default-modules.css` (the drop-down); `FirmwareEnv.defaultModules` (`env.ts`) is the slot the site leaves empty.
- `apps/desktop/ui/src/default-modules-data.ts` (list from the device files, `waitForRuntime`), `default-modules.ts` (the install over a `DkSession` and the library).
- Tests: `apps/web/tests/default-modules.test.ts`, `apps/desktop/ui/tests/default-modules.test.ts`.
- Screenshots: scenes `defaults-*` in `tools/devtools/firmware_shots.mjs` and `emulator-defaults-open` in `tools/devtools/ui_shots.mjs` (the preview installs on a fake device: `?defmods=ok|slow|fail`).

## Open

- **[O]** The restart after SELECT on a real device: Transfer's Restart button does the same call, but the flash has just restarted the device once. Check the port stays or comes back (`waitForBoot` reads the log through the same session).
- **[O]** The selection record is replaced by SELECT. The runtime has no command to read it, so the list sent is the loaded modules (found in the library by id) plus the defaults. A selected module that failed to load is dropped from it.
- The `.dkm` files come from `mise run module:pack-all` (`out/modules/`). Without them the run fails with that hint.
