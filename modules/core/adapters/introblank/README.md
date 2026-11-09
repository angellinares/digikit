# core/introblank

Optional. The stock intro draws the Elektron animation. With this adapter in the image, the intro draws black until a boot screen module (or the safe-mode screen) takes it over. When nothing takes it over, the runtime ends the intro right after the module load, so the UI starts in a fraction of a second instead of after the 5.8 s animation. It is in no default patch set.

## How it works

- The intro task calls one setup function (DT2 1.17 `0x400d1832`, DN2 1.12 `0x400d3d0a`, DN2 1.11 the same code) with one of five scene tables (`{callback, count}` entries and a 0 terminator). A hook before the setup function's load of that argument (`cf.rtos.hook.intro_setup.table_arg`, 6 bytes at `0x400d183a`) writes the address of the adapter's own table over the argument on the stack.
- The table is in the bss (one cache line of its own). The hook fills it on each call: one scene of 175 ticks (the stock length) with the callback `introblank_black`, and the terminator.
- The callback clears the intro bitmap with the stock clear function (`cf.draw.fn.bitmap_clear`, `FUN_40113700`, called as `(bitmap, 0, 1)`), because the bitmap persists between frames and the intro tick does not clear it. It reads nothing, allocates nothing and takes no input.
- `intro::end_blank` of core/runtime (optional symbol `introblank_table`) runs after the module loads in `dkrt_boot`. When the intro's current entry is still the adapter's table (no module published an intro, no safe screen), it writes count 0 into the table. The stock compare then ends the intro at the next tick: the pointer becomes NULL, the semaphore the main-OS task waits on is posted. Without the adapter nothing is written and the boot is as before.
- A module that takes the intro (`intro_take`) or the safe-mode screen publishes its own entry through `intro::install`, which replaces the pointer; the black table is then not used again.

## Include it

```sh
mise run patch -- --firmware firmware/Digitakt_II_OS1.17.syx \
  --modules core/runtime,core/digikit-menu,core/health,core/dynreg,core/dyndsp,core/introblank \
  -o out/patch/digikit-dt2-introblank.syx
```

The Workbench Firmware page adds it with the toggle "Custom boot screen" (patch set `device-introblank` of the device files, `device` plus this module). The toggle is off by default.

## Risk

The black callback runs at intro frame 0, before REC is read and before the boot counter is raised. A bug in the hook or the callback would halt every boot, and the safe-mode paths cannot help: only the stock FUNC bootstrap recovery and a reflash recover. The code takes no input and reads no storage, so one successful boot in the emulator and one on hardware prove it for every boot. The adapter stays out of the default sets until it has booted on a device.

## What does not depend on the intro length

Findings 36 and 116: the boot thread does the DSP boot, the eMMC init, the project decode, the card mount and the block interrupt before it creates the intro and main-OS tasks. The intro pointer and counters are read only by the intro functions. The semaphore `0x43149550` has one wait (the main-OS task, `0x400d1502`) and one post (the end of the intro task, `0x400d1a9a`). The device test `tests/introblank.device.test.ts` proves a complete boot after the early end: the UI is reached at 115 blocks instead of 5973 (DT2), a note plays with the same PCM peak as on the stock image, and the REC and boot screen paths work.

Proof: `pnpm nx:pilot run core/introblank:test-device` (DT2 1.17 and DN2 1.12).
