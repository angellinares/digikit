# digikit/remote

Presses keys and turns encoders from a computer, over the USB MIDI SysEx channel the digikit runtime already speaks. A run-time module (`.dkm`),
loaded like any other from the card store; it needs `core/input` in the image (both device patch sets have it). It is on exactly when it is loaded: the check mark of its row in SETTINGS > DIGIKIT > MODULES is the only switch.

| Command | Number | Body | Reply |
| --- | --- | --- | --- |
| KEY | 0x38 | code, down (0 or 1) | status 0 |
| TURN | 0x39 | encoder (1 to 9), delta (signed byte) | the delta taken (signed byte), at most 30 either way; the host sends the rest again |
| RELEASE | 0x3a | none | status 0: every key this module pressed is released |
| STATE | 0x3e | none | enabled (1 while the module is loaded), state format (1), turn limit (30) |

- **Codes** are the device file's `[panel]` codes (`devices/*.toml`); the host lays the keys out from them. The module knows no device.
- **No switch.** There is no REMOTE switch and no guarded range: a loaded module answers its commands, an unloaded one gives `E_PROTO` (-60). To turn it off, uncheck its row in MODULES, apply and restart.
- **Settings.** The MODULES side panel of the loaded module has SETTINGS... (`dkm_menu`, `DK_MENU` of `<dk/ui.h>`): KEYS HELD (the keys the module holds), LAST (the last command it ran and its result) and the action RELEASE ALL KEYS, which lets go of every held key at once. Without `core/ui` in the image the module still loads (the `dk_ui_*` imports are stubs) and has no page.
- **Pacing.** The firmware flushes a turn at most 30 detents every 4 ticks. The module keeps an estimate of what is still pending per encoder and takes only what fits, capped at ±30 per request even when direction reverses.
- **Held keys.** The module remembers the keys it holds. RELEASE lets go of all of them (a host sends it when its Remote page closes).

Design: `docs/plans/remote-control.md`.

The deterministic host C pacing check uses the actual command handler with a controlled block clock:

```sh
pnpm test:host       # includes modules/tools/remote/tests/pacing.test.ts
```

It requires host `cc` with C11 and 32-bit unsigned int, performs only a tiny bounded compile/run,
and needs no firmware or daemon. It covers direction reversal (pending +30 then -100 accepts -30),
95/96-block refill, signs, independent encoders, callback errors and clock wrap. Live TURN checks
prove command acceptance/delivery; they do not require an arbitrary vendor parameter or OLED change.
