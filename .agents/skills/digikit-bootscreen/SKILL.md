---
name: digikit-bootscreen
description: Write, check and send a boot screen animation for the Digitakt II or Digitone II as a Canvas 2D script (ticks, draw(ctx, t)), packed to a .dkbs file with mise run bootscreen (gallery, pack, sheet, dev, put). Use for boot screen scripts, .dkbs files, the 64 KiB size budget and the Workbench Boot screens files area.
---

# digikit boot screens

Run every command from the repository root. Details: `docs/DEVELOPMENT.md` "Boot screens".

## Source contract

A script `x.mjs` exports:

- `ticks`: an integer from 1 to 300.
- `draw(ctx, t)`: draws tick `t` (an integer from 0 to `ticks - 1`) on `ctx`, a Canvas 2D context of 128x64 pixels. The canvas is cleared before each tick.

The result is 1 bit per pixel: a pixel is on when its luma is above 128 (threshold 128). The player runs at 30 ticks per second, so 300 ticks are 10 s. The file may take at most 65,536 bytes (64 KiB). The screen plays once and holds the last frame.

## Tips

- Avoid thin lines and other antialiased shapes: the threshold turns them into noise. Use `fillRect` with whole-pixel coordinates.
- Set `ctx.imageSmoothingEnabled = false` before drawing images or text.
- Use large shapes. Change few pixels from one tick to the next: low-entropy frames keep the file under the budget.
- Do not use `Math.random` without a seed. The same script must give the same bytes each time.

## Loop

`mise run bootscreen` with no arguments opens a gallery of every example in `modules/tools/bootscreen/examples/` (click a tile to zoom; it updates when files change).

1. Write the script.
2. `mise run bootscreen -- sheet x.mjs`. It prints the size against the budget and writes a PNG contact sheet.
3. Read the PNG, adjust the script, repeat.
4. `mise run bootscreen -- put x.mjs [--port Digitakt]` sends it to `data/tools/bootscreen/x.dkbs`. Or use the Boot screens area in Workbench (Transfer, Modules, tick `bootscreen`).

The device uses the file only after you choose it in the module's settings on the device and restart.
