# tools/bootscreen

Plays a custom boot animation (a DKBS file) instead of the stock one. A run-time module (`.dkm`), built from C, for the Digitakt II and Digitone II.

- **Files.** The module reads the files in `data/tools/bootscreen/` on the card. The workbench lists them as "Boot screens" and previews them as the 128x64 1-bit OLED.
- **Choice.** The row BOOT SCREEN in the module's menu lists the files and saves the choice as the setting `active`. It is active at the next restart.
- **Boot.** `dkm_init` reads `active` and the file, checks it (`src/dkbs.c`) and calls `intro_take` (`<dk/platform/intro.h>`). It runs after the REC, safe-mode and boot-counter checks and does no card I/O before the main OS starts.
- **Examples.** `examples/*.mjs` draw a screen with Canvas 2D. Pack one with `mise run bootscreen -- sheet modules/tools/bootscreen/examples/pulse.mjs`.
