# usb-fast: step 4 brief and the hardware runs

Companion to `docs/plans/usb-fast.md` (the design). Status: **agreed with Em 2026-11-11; not started.** Step 4 starts after the three lanes
(fast-transfer, usb-bulk, usb-host) are merged into `lane/fast-transfer`.

## Step 4: the usb-fast module

`modules/tools/usb-fast`, a tool content module in C on the SDK, `requires = ["core/usb-bulk"]`, targets `dt2-1.17` and `dn2-1.12`.

- **Init** (`dkm_init`): register the SysEx module commands of `[protocol.ranges] usb_fast` (0x36 `SET_NONCE`: body 8 bytes, empty reply;
  it stores the nonce for INFO); `io_post` the transfer job; `usb_bulk_enable(id)`. **Fini**: `usb_bulk_disable(id)`, `io_cancel`, abort an open
  file.
- **Stream parser**: frames are cut by their own length field, not by USB buffers. A frame may span RX buffers and a buffer may hold several
  frames; a frame whose length is a multiple of 512 is followed by a zero-length packet from the host (`[protocol.bulk]`). Copy each frame into one
  16-byte-aligned frame buffer (header 12 + `data_max` 65536) in the module's bss, so `file_write` gets an aligned buffer. A bad magic or a length
  over `data_max + header + frame_slack` is fatal for the stream: reply `E_MSG` if a seq is known, drop the buffered bytes, abort an open file.
- **Commands** (`[protocol.bulk.commands]`, statuses as generated):
  - INFO: reply `version, flags, rsvd, data_max, align, nonce[8]` (20 bytes).
  - BEGIN `{root, path, size, hash4}` with header flag REPLACE -> `file_create(root, path, size, hash4, DK_FILE_REPLACE?)`.
  - DATA `{offset, bytes}` -> `file_write`. The host keeps two in flight; handle them in order, one reply each.
  - END -> `file_close(h, hash_out)`; reply the 4-byte `hash|1`.
  - ABORT -> `file_abort`.
  - VERIFY is not implemented on bulk (reply `E_CMD`): the host verifies over SysEx (DN2 VERIFY, DT2 file info). Both are device-CPU bound
    with tiny replies, so bulk would gain nothing. Remove VERIFY from `[protocol.bulk.commands]` or mark it reserved.
- **Busy and back-pressure**: a `file_*` E_BUSY -> return `DK_IO_MORE` (the runtime backs off 1 ms); no RX buffer taken -> `DK_IO_WAIT` (the
  adapter wakes the job); `usb_bulk_send` E_BUSY -> wait for the wake; E_AGAIN -> `DK_IO_MORE`. Release each RX buffer as soon as its bytes are
  copied, so the host is NAKed only while the module is busy.
- **Reset**: a change of the adapter's reset count (`usb_bulk_state`) aborts an open file and clears the parser; the host resyncs.
- **Host additions** (usb-host lane code): an emulator `BulkLink` over `usb.bulk.in/out` so the workbench's Emulator target can negotiate
  Fast USB; a `dkbulk echo [--sizes ...]` subcommand for the first hardware run (step 3 echo, before usb-fast exists).
- **Tests** (emulator, both devices, through the TS `BulkSession` over the emulator link): put to the DN2 store and the DT2 +Drive; REPLACE and
  E_EXISTS; ABORT; a reset mid-file; frames spanning buffers (sizes around 16 KiB and 512 multiples, with and without the ZLP); the nonce: a wrong
  nonce makes the host fall back to SysEx; power cut mid-file on each device (old or new file); the workbench preview shows FAST USB for the
  emulator target. Then the bench in emulator time for a regression baseline.

## Hardware runs

Rules: emulator proofs first ([[hardware after emulator confidence]]); one person at the device; every step that restarts the device is said
before it runs; results go to findings 42 and 43.

### Before

1. All emulator proofs pass on the merged branch: usb-bulk device tests, usb-fast device tests, file-write, plusdrive-write, upload suites.
2. Note what the device runs now: the runtime version and the selected modules (boot report or `mise run dk:store -- --port NAME modules`).
   Keep the stock `.syx` of its OS (DN2 1.12, DT2 1.17) and the currently flashed patched `.syx` at hand for rollback.
3. Know the recovery path: hold REC at boot = safe mode (no modules, stock USB tables); the DIGIKIT menu deselects a module; a stock OS
   upgrade over USB MIDI (or DIN MIDI if USB is unusable) restores the device.
4. DT2 only: back up the +Drive with Elektron Transfer.
5. Hash listing of every existing sample (DN2: the store listing with hashes; DT2: the +Drive listing with hash|1), saved to a file.

### Build and flash (the device restarts)

1. Patch set: the set the device runs today plus `core/usb-bulk` (add a `device-usb` set to `devices/<device>.toml` if none fits).
   `mise run patch -- --firmware firmware/<stock>.syx --set device-usb -o out/patch/<dev>-usb.syx`.
2. `mise run patch:preflight -- out/patch/<dev>-usb.syx` must pass.
3. Flash from the workbench Firmware page (one click). The device restarts.

### DN2 run 1: the adapter alone (step 3, before usb-fast)

1. Module not selected: the device boots, USB MIDI works, and `system_profiler SPUSBDataType` / `ioreg` shows the stock interfaces only
   (the adapter is inert).
2. Upload and select `test/usb-echo`, restart (the device restarts). `ioreg` shows interface 2, class 0xFF, EP2 bulk 512 both ways; the MIDI port
   is still there; `dkbulk list` finds it.
3. `dkbulk echo --sizes 1,511,512,513,4096,16384,65536,1048576`: every size identical. Note MB/s for 1 MiB.
4. SysEx INFO during a 1 MiB echo still answers.
5. Unplug and replug USB; sleep and wake the Mac: echo works again.
6. REC drill: restart holding REC (safe mode): interface 2 is gone; deselect `test/usb-echo` in the DIGIKIT menu; restart normally.

### DN2 run 2: usb-fast (after step 4)

1. Upload and select `usb-fast`, restart. The workbench Transfer pane shows FAST USB for the DN2.
2. Bench with `mise run wb:cli -- samples put` (`--bulk off`, then `--bulk auto`): one 8 MB file and a folder of 100 files of 50 to 500 KB. Two runs
   each. Record MB/s against the SysEx baseline (0.60 MB/s).
3. Replace an existing sample (REPLACE): the new bytes verify.
4. Pull the USB cable mid-file, reconnect: the destination holds the old file or nothing new, the next upload works, the pill shows the
   fallback or Fast USB again.
5. Hash listing again: every pre-existing sample unchanged.
6. REC drill with usb-fast selected.

### DT2 (after DN2 run 2)

The same as DN2 runs 1 and 2, with the +Drive backup first and a cut of USB (never power) for the mid-file case. Also: the stock sample browser
does not show the staging folder after a transfer; a replace of a sample the pool has loaded is refused (E_FILE_IN_USE).

### Rollback

Deselect the module (SysEx keeps working); or flash the previous patched `.syx`; or flash the stock OS. If USB is unusable: REC safe mode first;
DIN MIDI OS upgrade last.

## Results

### DN2 run 1 (2026-11-11)

- Flash 1: the `device` set from lane/fast-transfer (image f5fe094ed67dfe1c): boots, plays, debug/remote load, store listing unchanged,
  SysEx put/delete ok. Flash 2: `device-usb` (device + core/usb-bulk, image 9d686bb778ffd7e8): the adapter is inert while no module asks
  (USB interfaces unchanged).
- USB CONFIG matters: the DN2 was in USB AUDIO/MIDI mode (UAC2 uses EP2 IN for the audio clock feedback and EP3 for audio), where v1 cannot
  add its EP2 bulk pair. In USB MIDI mode, with test/usb-echo selected, **interface 2 (class 0xFF) enumerates on macOS** and `dkbulk list`
  finds it. (A USB CONFIG change made just before a runtime restart did not stick; set it, then restart.)
- `dkbulk echo`: 1, 511, 512 and 513 bytes identical. **4096 bytes: no reply, then every bulk send fails with 0xe00002ed and SysEx INFO
  hangs too: the USB controller wedged; the CPU kept running (buttons, audio); no crash record; a power cycle recovers.**
- Leading suspect: the first transfer that crosses a 4 KiB page of a multi-page dTD (page pointers 1-4, or the controller's DMA and the
  uncached alias addresses). The firmware itself only uses single-page 512-byte dTDs, and the emulator model does not apply the
  controller's page rules, so neither caught it.
- The device was restored to debug + remote on the device-usb firmware. `dk:run` replaces a selection slot.

### Next

1. Diagnose the 4 KiB wedge: the MCF5441x reference manual (out/refs) USB chapter (dTD page pointers, DMA addressing); firmware-query for
   any multi-page dTD in the firmware (CDC, Overbridge) and what the firmware's DMA arena (0x4fe3c000) is; the adapter's dTD construction.
   Make the emulator model enforce the manual's rules so it reproduces the wedge; then fix (correct the pointers, or use single-page
   4 KiB dTDs as the firmware does). Hardware echo again at 4096, 16384, 65536 and 1 MiB (needs a reflash).
2. Before the PR: the adapter refuses cleanly outside the USB MIDI configuration ("wrong USB mode" in usb_bulk_state) and the badge's
   SYSEX tooltip says to set USB CONFIG to USB MIDI; bump the runtime version (still 0.5.6 with SDK 1.10); fix the flash tool's
   "port did not go away within 240 s" false alarm (macOS keeps the port listed); commit the `device-usb` patch set (now uncommitted in
   devices/digitone-ii.toml).
3. DN2 run 2 (usb-fast bench), then the DT2 runs, then `mise run test:full` and a PR to main.

### v2: Fast USB in USB AUDIO/MIDI mode (agreed design, not started)

The workbench takes the device over by SysEx: a RAM-only switch to the USB MIDI configuration with the bulk interface, through the
firmware's own USB-mode switch (FUN_40006c16), released by SysEx, on unplug and on reboot. USB audio drops while the workbench holds the
device. The stored USB CONFIG setting is never written (verify statically). Reuses the v1 protocol, adapter, module and host unchanged.
