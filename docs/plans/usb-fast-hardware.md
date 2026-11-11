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
does not show the staging folder after a transfer; a replace of a sample loaded on a track is allowed and the loaded sample keeps playing
(the DT2 plays from its sample RAM). The adapter's E_FILE_IN_USE only applies to a file the firmware has open at that moment.

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
- Diagnosis: see run 1b. The first guess (page pointer 1 = 0) was not the cause.
- The device was restored to debug + remote on the device-usb firmware. `dk:run` replaces a selection slot.

### Fix (2026-11-11, commit 26c4022c)

Page pointers made to follow the RM (not the cause). The adapter fills page pointers 1-4 with the next four pages (RM 24-71 step 7). The
emulator keeps this rule, but it did not reproduce the real fault: the hardware still wedged after the commit. `patchkit flash` no longer
reports the false "port did not go away" on macOS (it asks the device by INFO after 10 s and ignores old-image answers).

### DN2 run 1b: the real cause (2026-11-11)

- Bisect **[D]**: 1024 bytes wedged as the first transfer after a power cycle; 1, 511, 512 and 513 worked.
- HOLD split **[D]**: with the adapter's debug log events (MOD_LOG at debug level, UB_TRACE) and the usb-echo HOLD mode (`dkupload cmd`), OUT
  with two full 512-byte packets worked (rx.done 1024, controller healthy, SysEx alive). The IN send of the same 1024 bytes (two back-to-back
  512-byte packets) wedged the controller the moment the host read it. After a wedge, a USB unplug and replug does not bring the device back
  (it does not attach again); only a power cycle does.
- Cause **[D]**: the controller's TX buffer is 64 bytes per endpoint (RM HWTXBUF: TXCHANADD = 4, 16 x 32-bit entries; TXADD = 6, 256 bytes
  total). The stock firmware declares EP1 IN as bulk 512 in the USB MIDI descriptor but programs its dQH with caps 0x20400000 (max packet 64,
  ZLT off; `FUN_400043e2`). Its audio and Overbridge modes also use 64-byte IN dQHs; only the CDC service mode programs a 512-byte IN dQH.
- Fix (commit 9ae0380a): the EP2 IN dQH caps are 0x20400000 (64-byte packets, ZLT off); the descriptor keeps 512. Each 64-byte packet is a
  short packet for the host; host readers already loop until a frame is complete; usb-fast replies are at most 32 bytes. The emulator faults an
  IN transfer whose packets after the first exceed 64 bytes (measured rule).
- Hardware after the fix **[D]**: `dkbulk echo` identical for 1, 511, 512, 513, 1023, 1025, 2048, 4096, 16384, 65536, 262144 and 1048576 bytes,
  about 2.3 MB/s each way at once (IN is limited by the 64-byte packets). A USB unplug and replug (bus reset) frees the buffers and the
  interface works again. `dkbulk echo` now reads beside the send (write-then-read deadlocked at 64 KiB against the device's buffers).

### DN2 run 2 (2026-11-11)

`wb.mjs samples put`, synthetic WAVs, usb-fast selected; all files verified by device hash **[D]**.

| case | SysEx | Fast USB |
|---|---|---|
| one 8 MB file, put | 0.68 MB/s | 3.86 MB/s (5.7x) |
| one 8 MB file, put with verify | 0.62 MB/s | 2.53 MB/s (4.1x) |
| 100 files of 50-500 KB (28 MB), with verify | 0.57 MB/s | 1.11 MB/s |
| 8 MB replace of an existing sample | | 3.64 MB/s put, hash matches |

For the 100 small files per-file overhead dominates: open, verify over SysEx, close.

Unplug mid-file **[D]**: a 60 MB upload over Fast USB with the USB cable pulled mid-file. The bulk link failed (macOS 0xe00002ed). `wb` printed "USB bulk link failed ...; the rest of this run goes over SysEx" and sent the whole file again over SysEx (0.46 MB/s, device hash matches). Afterwards the store held only complete files (no staging or partial entries). The next upload used Fast USB again (3.68 MB/s, hash matches). The source file limit is 64 MiB (dksample `MAX_SOURCE_BYTES`), so a 64 MB WAV is refused before sending.

REC drill with usb-fast selected **[D]**: a boot holding REC (safe mode) loaded 0 modules, the bulk interface was gone, INFO and the module list answered over SysEx (without the debug module's LOG/PEEK/LOGCFG capabilities). A normal boot loaded debug and usb-fast again (no faults), the interface came back and an 8 MB upload went over Fast USB (2.41 MB/s with verify).

Open **[O]**: `wb.mjs samples ls` gets no reply ("no reply to MidiRpc 10") on the DN2, while `store ls` and `dkupload ls` work. Not related to bulk; to look at.

### DT2 runs (2026-11-11)

DT2 OS 1.17, image bacc88eae0a2bfdf, patch set `device-usb` (device + core/usb-bulk + core/plusdrive-write), commit 90c314ea. All **[D]**.

- USB CONFIG was USB AUDIO/MIDI: usb-echo loaded, the adapter refused (enable -53, state bit 4), no interface, SysEx fine. After switching to
  USB MIDI live, the usb-echo REENABLE command enabled the interface without a restart. A USB CONFIG change is not stored before a runtime
  restart (the restart by `dk:run` reverted it): set it, wait or leave the menu, then power-cycle.
- `dkbulk echo` identical for 1, 511, 512, 513, 1023, 1025, 2048, 4096, 16384, 65536, 262144 and 1048576 bytes; about 2.9 MB/s each way at once.
- usb-fast to the +Drive (`wb.mjs samples put`, synthetic WAVs, all device content hashes match):
  - 8 MB: SysEx 0.11 MB/s, Fast USB 0.94 MB/s (with verify). Later runs of the same 8 MB over Fast USB gave 1.26-1.60 MB/s: the +Drive
    speed varies between runs, so measure several runs.
  - 100 files of 50-500 KB (28 MB): SysEx 0.10 MB/s, Fast USB 0.25 MB/s.
  - Uploads went to new folders only; the top-level listing before and after differs only by the new bench folders.
- Replace of a sample loaded on a track (one-shot machine): allowed, and the loaded sample keeps playing (the DT2 plays from its 400 MB sample
  RAM, not from the +Drive). The adapter's E_FILE_IN_USE refusal only applies to a file the firmware has open at that moment.
- USB cut (cable only) mid-file during a 60 MB upload: the bulk link failed and no partial file exists under the real name. `wb` then fell back
  to MidiRpc for the rest of the run even with `--bulk force` (force only decides at the start) **[O]**: a tool bug. Stopping that fallback left
  a 0-byte file from the stock MidiRpc path (not ours). The next upload went over Fast USB (1.60 MB/s, hash matches).
- The stock sample browser does not show the staging folder (`.dkstage`).
- REC safe mode: no interface, 0 modules, INFO over SysEx. A normal boot loads debug and usb-fast again and an upload goes over Fast USB.
- `wb.mjs samples ls` works on the DT2. On the DN2 it still gets no reply **[O]**.

### Next

1. Done: the wrong-mode refusal (E_STATE outside USB MIDI mode) is committed in 71ba9e29.
2. Done: DN2 run 2 (including the REC drill) and the DT2 runs.
3. Next: the speed work. Measure the stages on hardware first: per-file overhead and storage are the costs, not USB (see the speed notes below).
4. Then `mise run test:full`, a PR to main, then v2 (below).

### Speed: where the time goes (code reading, to measure)

- DN2, large file: serial copy + hash + card write on the priority-3 I/O task, plus a redundant SysEx VERIFY readback after each file (about
  35% of the time).
- DN2, small files, about 180 ms per file: store table rewrite + readback + 2 header writes per commit (grows with file count) + host SysEx list
  before and list + verify after each file.
- DT2: firmware +Drive file API in 32 KiB pieces + a firmware hash re-read at END. Per file 10-15 firmware filesystem calls (staging mkdir, open,
  reserve, write, hash, close, info, lookup, rename, tidy, journal on replace) + host SysEx mkdir and list per file.
- Planned: measure each stage on hardware (device timing events, host phase times, discard and write-only ceilings), then fix by measured gain.
  The emulator is not a speed reference (it predicted DN2 5.6 and DT2 2.1 MB/s).

### v2: Fast USB in USB AUDIO/MIDI mode (plan: [usb-fast-takeover.md](usb-fast-takeover.md))

The workbench takes the device over by SysEx: a RAM-only switch to the USB MIDI configuration with the bulk interface, through the
firmware's own USB-mode switch (FUN_40006c16), released by SysEx, on unplug and on reboot. USB audio drops while the workbench holds the
device. The stored USB CONFIG setting is never written (verify statically). Reuses the v1 protocol, adapter, module and host unchanged. The controller has 4 device endpoints (EP0-EP3, RM DCCPARAMS DEN = 4) and
USB AUDIO/MIDI uses EP2 IN and EP3, so a bulk pair beside live USB audio is not possible; the take-over is the route.
