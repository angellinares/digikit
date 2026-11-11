# digikit/usb-fast

File uploads on a USB bulk interface, several times faster than SysEx. A run-time module (`.dkm`) in C; it needs `core/usb-bulk` (the adapter that adds interface 2,
EP2 OUT and IN, to the USB MIDI mode) and, on the Digitakt II, `core/plusdrive-write` for the +Drive. Plan: `docs/plans/usb-fast.md` section 5 and
`docs/plans/usb-fast-hardware.md` (step 4). Selected, the device re-enumerates once at boot and offers the bulk interface; not selected, USB is stock and SysEx
does everything as before.

- **SysEx**: `USB_FAST_NONCE` (0x36, range `usb_fast` of `sdk/sdk.toml`): body 8 bytes, empty reply. `TAKEOVER` (0x1e) and `RELEASE` (0x1f, range `usb_fast_takeover`): the host has core/usb-bulk switch a device in USB AUDIO/MIDI to USB MIDI for a transfer and back ([plan](../../../docs/plans/usb-fast-takeover.md)); TAKEOVER also renews. `STATUS` (0x37) is a debug aid: 18 words, word 8 is `usb_bulk_state` (the host reads its wrong-mode bit). The host sends random bytes, then asks INFO on each
  candidate USB device and keeps the one that returns them (the USB serial strings are the same on every unit).
- **Bulk protocol** (`[protocol.bulk]`): request `'DKB1' | cmd | seq | flags u16 | len u32 | body`, reply `'DKB1' | cmd|0x40 | seq | status i16 | len u32 | body`.
  INFO replies `version, flags, reserved, data_max, align, nonce[8]`. BEGIN (`root, path, size, hash4`; flag REPLACE) opens a file with `file_create`, DATA
  (`offset, bytes`) writes with `file_write`, END closes with `file_close` and replies the 4-byte `hash|1`, ABORT drops the file. VERIFY is reserved and answers
  `E_PROTO`: the host verifies over SysEx.
- **Stream**: frames are cut by their length field, not by USB buffers: a frame may span receive buffers and a buffer may hold several frames. Each frame is copied
  into one 16-byte-aligned buffer (so `file_write` gets an aligned pointer) and each receive buffer is released as soon as its bytes are copied. A bad magic or a
  length over `data_max + 4 + frame_slack` is fatal for the stream: the module replies `E_MSG` for the seq it saw, drops what it holds and the open file, and reads
  the next bytes as a new stream.
- **Busy**: a `file_*` call that answers `E_BUSY` is called again (the runtime waits 1 ms) up to 3 times (BEGIN 400 times, about 0.4 s: the +Drive staging folder is removed after an idle session), then the host gets `E_BUSY`. No receive buffer: the job
  sleeps and the adapter wakes it; the host is NAKed while all four are full. A send that finds both slots in flight waits for the wake.
- **Reset**: a bus reset or a disconnect (the reset count of `usb_bulk_state` changes) aborts the open file and clears the parser.
- **Enable**: the job calls `usb_bulk_enable` until it succeeds: the adapter wakes it four times a second while the device is not in USB MIDI mode or a mode switch is settling.
- **Idle session**: the runtime drops a write session with no write for 20 s; the job frees its staging buffer 1 s later.
- **Build flags** (`src/c/uf.h`): `UF_TRACE`, `UF_BENCH` (the storage bench, `src/c/uf-bench.c`) and `UF_KNOBS` (STATUS 'A', the staging buffer test knob) are 0 in shipping builds; the staging buffer test skips its forced-size cases without `UF_KNOBS`.
- **Unload**: `dkm_fini` disables the interface and cancels the job; the runtime aborts a file the job leaves open.
- **Test**: `tests/usb-fast.device.test.ts` (emulator, both devices) over `EmuBulkLink` of `packages/dk-session`.
