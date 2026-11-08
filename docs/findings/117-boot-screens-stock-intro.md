# 117. Boot screens: stock intro, swap point and safe-mode reading

Trigger: a boot screen module must replace the stock intro and know when a boot is unsafe. Scope: DT2 1.17 and DN2 1.12 (DN2 1.11 where named). Marks: **[V]** verified against image bytes by a second agent, **[D]** measured or read once, **[O]** open.

## 1. The stock intro

- **[V]** It is its own task, priority 7. The intro task is created once and there is no second intro. The installer creates it before the main-OS task (DT2 `0x400d1448`, DN2 1.12 `0x400d3920`). It blocks on a PIT3 tick.
- Each boot picks one of five scene tables from an LCG random number. The first table has about 99.9%.
- An entry is `{callback, frame count}`. The first table has 175 frames.
- **[V]** The current-entry pointer: DT2 `0x43153a00`, DN2 1.12 `0x42c4cacc`, DN2 1.11 `0x42c4bacc`.
- **[V]** The tick function (hash `573ca1fa13c8386bc529bf48d5cd1fb4`) is at DT2 `0x400d18f0` and DN2 1.12 `0x400d3dc8`. Each frame it reads the pointer and calls `callback(index, count, bitmap)`. Then it reads the pointer again and does a signed compare: `count > index` continues. Only the setup and the tick function touch the pointer, on all three firmware versions.
- **[V]** Count 0 ends the intro through the normal path: the pointer is cleared (DT2 `0x400d19c8`), PIT3 is stopped, the semaphore `0x43149550` is posted and the main-OS task waits on it (DT2 `0x400337dc`). The signed compare is `cmpa.l; bgt` at DT2 `0x400d19a2`.
- **[V]** A higher priority number is more urgent (`make_ready` `0x40001164`, dispatcher `0x40000410`). The intro task (7) preempts the main-OS task (6, created at `0x40033276`). The main OS cannot interrupt a frame callback, and a callback that never returns starves the main OS until power-off.
- **[V]** The panel parser registers at `0x400cca76`, before the intro starts.
- The main-OS task waits for the intro to finish before the UI loop.
- The blit source view is `{vptr, width, height, bands, pixels @0x10, mask @0x14, owns @0x18}`.

## 2. Frame rate

- Our model: PIT3 PCSR `0x093f` and PMR 8593 give 30 Hz, so 175 frames take 5.83 s. **[D]**
- `docs/findings/07-emulator.md` ("The intro is meant to run at 15.00 fps", about line 1001) derives 15 Hz from the divisor 1024 (period 66.67 ms). This conflicts with the 30 Hz of our model. It is not resolved on hardware. **[O]**

## 3. Swap timing (emulated, one selected module, cold start) **[D]**

- `dkm_init` starts at frame 53 on DN2 and frame 57 on DT2: about 1.8 s and 1.9 s at 30 Hz.
- The intro advances 2 frames during a 64 KiB `store_read` (about 95 ms). It keeps playing during blocking reads.
- The frame count rises with the number of modules loaded first.
- The emulated card latency is not the hardware latency.
- **[D]** Where the gap comes from, DT2, emulated, drawn-frame count of the intro (DT2 `0x43153a0c`) read in the runtime: 56 at `dkrt_boot` entry, 57 after the selection read, 57 after `card::check`, 57 after `settings::load`, 57 at the module's `dkm_init`. About 98% of the gap is stock setup before the main-OS task reaches the runtime hook; our runtime adds 1 frame (the selection read). DN2 (`0x42c4cad8`) is not measured yet **[O]**.
- At that frame the stock logo mark is partly drawn.

## 4. Safe mode

- The stock firmware has no REC safe mode. It is a digikit feature. **[D]**
- **[V]** REC is key `0x13`: state byte 2, bit 2. The state array is at DT2 `0x44dc8644` and DN2 `0x445a2e30`. **[D]** `key_down` returns 0 before the first channel-2 report.
- Emulator, REC held: the old queue scan on main gave `safe:0, reason:0, loaded:1`. The stock key state gives `safe:1, reason:2, loaded:0` on DT2 and DN2. **[D]**
- **[D]** One real Digitakt II, OS 1.17, with this branch's image (2026-11-08): REC held from power-on gave safe mode, and the DIGIKIT menu showed "safe mode key held". The custom boot screen also played about 1.8 s after the start of the stock intro, and the module setting persisted across power cycles. A key held at power-on does produce a report on this device; other panel firmware versions and Digitone II are not measured. **[O]**
- **[D]** A safe boot shows a runtime screen in place of the stock intro for 120 ticks (4 s): `SAFE MODE`, the reason, `NO MODULES LOADED`. `dkrt_boot` renders it into a static buffer (`safe_screen.rs`) and the intro task only blits it, through the same entry install as `intro_take`. Emulator, DT2: the intro framebuffer equals the host-rendered frame for REC held and for the counter chain (boot 3).

## 5. Crash during the intro

- The boot counter is written before any module code and cleared after 30 s of DSP blocks.
- `crash::install` runs only from the UI loop. A fault during the intro reaches the stock handler, which halts.
- Emulator: a reset from the intro callback reaches safe reason COUNTER on boot 3 on both devices (seq 3, 0 modules, stock intro). **[D]**
- Reading the card of a halted boot is not proved. **[O]**

## 6. Stock recovery by FUNC

It lives in the bootstrap: see `docs/findings/01-container-and-patching.md`. It is not re-derived here.

## 7. A black intro (core/introblank)

- **[D]** The five stock scene tables are at DT2 `0x402a1b64`, `0x402a1b74`, `0x402a1b84`, `0x402a1b94`, `0x402a1ba4`. The intro task `0x400d1a12` picks one and calls the setup function (DT2 `0x400d1832`, DN2 1.12 `0x400d3d0a`, same function hash on DN2 1.11) that stores it as the current entry. The load of the argument is `move.l (0x24,SP),D0; movea.l D0,A0` at DT2 `0x400d183a`, DN2 1.12 `0x400d3d12`, DN2 1.11 `0x400d3bae` (the 6 bytes have the same SHA-256 on the three). The stock clear function `FUN_40113700` (DN2 1.12 `0x40115074`, DN2 1.11 `0x40114d94`) is called as `(bitmap, 0, 1)`. Anchors `cf.rtos.site.intro_setup.table_arg` and `cf.draw.fn.bitmap_clear`. Emulator, both devices: with a hook that points the argument at an own table, the intro ends after 11 (DT2) or 8 (DN2) ticks when the runtime writes count 0, the UI is reached at 115 blocks instead of 5973 (DT2) and 6180 (DN2), and a note has the same PCM peak as on the stock image.

## 8. Frame layout on the panel

- **[D]** Frame layout, measured on one real Digitakt II, OS 1.17, 2026-11-08: with y = 0 at the top of the panel, the intro bitmap holds pixel (x, y) at byte `8*x + (63-y)/8`, bit `7 - (63-y)%8`; x is not flipped. The pulse boot screen's bar was at the bottom in the preview and at the top on the panel, and it filled left to right in both. A comparison of the stock logo with the bootloader picture also found a vertical flip only (0 mismatching pixels). DKBS frames, `safe_screen.rs` and `bitmapPixel` use this layout (docs/design/dkbs.md).
- **[O]** The emulator disagrees with the panel. `decode_intro_bitmap` (`packages/emulator/boot/src/runtime.rs`) maps intro bitmap row y to raw byte `(7 - y/8) + 8*x`, bit `y%8`, which assumes the intro flush flips like the main UI blit. Intro frames drawn with the old layout therefore looked upright in the emulator and were flipped on the panel. Not fixed: whether the intro flush (0x40133bea or 0x40133b6c) skips the flip, or the panel is set to another scan direction, is not established, and the stock logo orientation on hardware is not recorded. Frame-equality device tests compare buffers and do not depend on this.
- **[D]** One real Digitakt II, OS 1.17, boot screen with `core/introblank` (2026-11-08): the bootstrap's logo mark, "B" and "1.17" stay until the main OS's first display write; then about 1 to 2 s of black follow, then the boot screen. Holding REC gives safe mode, and the marks of the safe-mode list work.
