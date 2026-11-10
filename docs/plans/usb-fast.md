# Fast transfer over a USB bulk endpoint (usb-fast)

Status: **agreed 2026-11-10; building.** Branch `lane/fast-transfer`. Decisions: SDK file writes open to every module for now (restrict later);
VERIFY on by default; a hardware echo after step 3; the SysEx path is left as it is (no chunk change: it becomes the fallback).

## 1. Goal and numbers

Sample transfer to both devices is limited by the firmware's USB-MIDI SysEx receive chain, not by verification and not by our protocol. Measured on
a real Digitone II 1.12 on 2026-11-10 (high-speed USB confirmed by IOKit, macOS, the workbench's `dkmidi` bridge) **[D]**:

| Measurement | Result |
|---|---|
| digikit store upload, 8 MB, 32 KiB chunks, depth 1 | 0.60 MB/s (54 ms a chunk) |
| the same, depth 2 | 0.60 MB/s: no gain |
| VERIFY in 1 MiB ranges (card read and hash) | 7.4 MB/s, about 8 % of the total |
| ignored SysEx (no handler), 4 KB or 37.5 KB messages | 0.857 MB/s of wire bytes |
| ignored SysEx and 8 VERIFYs interleaved | exactly the sum of the two alone: no overlap |
| round trip of a small request | 2 ms |

The last two rows show the ColdFire is saturated while it takes SysEx: the deferred-call task (priority 9) that splits USB-MIDI events and fills the
SysEx block pool uses all free CPU, so work in the MIDI task (priority 7) cannot overlap with it. The firmware re-arms EP1 OUT in the ISR at once
(32 slots of 512 bytes), so there is no structural pacing (static, DT2 1.17 and DN2 1.12) **[D]**. The emulator counts 20.5 instructions a
SysEx byte for the chain after the event split (assembler, gather, router; same on DN2 1.12 and DT2 1.17) and 38.3 for our DATA; it skips the split
(`FUN_40004504`, about 32 a byte by the static count in finding 42). With the split added, 53 M free instructions a second predict 1.0 MB/s for ignored
SysEx and 0.76 MB/s for DATA, against the measured 0.857 and 0.694 **[D]**. The DT2 desktop path is also slowed by 2 KiB
stop-and-wait chunks (`rpcfiles.ts:24`), about 0.2 MB/s.

Goal: a vendor bulk endpoint that skips that chain. The USB controller DMAs file bytes into our buffers; the CPU only hashes and writes them.

Expected (estimates from the measured free CPU, about 53 M instructions/s, and the content hash at about 7 instructions a byte):

| | today | with usb-fast |
|---|---|---|
| DN2 store, upload and VERIFY | 0.55 MB/s | 3 to 4 MB/s |
| DT2 +Drive, upload and the firmware's close hash | about 0.2 MB/s | 2 to 4 MB/s **[O]**: eMMC program rate and the path filesystem flush are unmeasured |

The hash becomes the limit, not USB (high-speed bulk carries about 35 MB/s).

## 2. Shape in the current architecture

```text
host: dkbulk (Rust, nusb) / WebUSB             SysEx stays for everything else
          |  EP2 OUT/IN bulk 512, vendor interface 2
usb-fast (tool module, .dkm, C)                bulk protocol, transfer loop, verification
          |  imports usb_bulk_*  (requires core/usb-bulk)
          |  SDK file_* , io_post, hash32_*     (new, general)
core/usb-bulk (adapter)    runtime                         core/plusdrive-write (adapter, DT2)
descriptor, table,         file API on the store            open / write / close / delete on the
re-enumeration, EP2,       (shared with SysEx upload),      firmware's C-level file API
ISR -> buffers             I/O worker task, hash export
```

The runtime gets general facilities only: file writes, a blocking I/O worker and the hash. Nothing in the runtime knows about USB bulk or fast
transfer. The module holds the protocol; the adapters hold the firmware knowledge (ARCHITECTURE.md: modules reach only the SDK and adapter imports).
Not selected, the module is absent and USB stays stock (rule 5).

## 3. New SDK facilities (runtime and adapters, general use)

### 3.1 File writes

```c
i32 file_create(u32 root, const char *path, u32 size, const u8 *hash4); // a write session; hash4 NULL or the expected hash; no overwrite: E_EXISTS
i32 file_write(i32 h, u32 offset, const u8 *buf, u32 len);   // offset must be the next byte; len a multiple of 512 except the last
i32 file_close(i32 h, u8 hash_out[4]);                      // commits; hash_out = the device's `hash|1`, big-endian (store slot and +Drive alike)
i32 file_abort(i32 h);                                      // drops the partial file
i32 file_delete(u32 root, const char *path);
```

SDK API 1.10. Handles are tagged with their owner; unloading a module aborts its open session.

- Roots as in `files.h`: `DK_FILES_STORE` and `DK_FILES_SAMPLES`. On DN2 both are the digikit store (samples live in its `samples/` folder). On DT2
  the samples root is the stock +Drive through `core/plusdrive-write`; absent adapter: `E_CONTRACT`.
- **SysEx does not change.** `dkrt_midi_rx` and every SysEx command stay in `upload.rs`: SysEx remains the runtime's always-on channel (upload,
  selection, safe-mode recovery, remote, log, faults), with or without usb-fast. Only the store write steps inside `cmd_begin`, `cmd_data` and
  `cmd_end` (path and space checks, `first_fit`, `write_sectors`, running hash, END hash check, `do_commit`) move into a new runtime file
  `store_writer.rs` (`begin`, `write`, `close`, `abort`). The SysEx handlers call it after decoding a request; the SDK `file_*` calls call it too. A
  refactor with no change in behaviour: the existing upload device tests and `upload-cut` must pass unchanged.
- **One write session** in all, across SysEx upload and the SDK (`E_BUSY`): the store writer holds it.
- **Two tasks, one store**: SysEx store commands run in the MIDI task, SDK calls on the I/O task. One store lock (taken without blocking, under
  the irq mask) covers every store read and write, so a commit never overlaps a LIST, VERIFY or another write. A SysEx store command that finds it
  held replies `E_BUSY` and the host retries after a short wait (`dkupload-core`). The SDK path has its own scratch buffer (table stream and sector
  scratch, about 9 KiB); it never touches `DKRT_UPLOAD_BUF`.
- **Store (DN2, and the DT2 store)**: `file_close` checks the running hash against `hash4` when one was given (`E_HASH`), then commits with the
  existing two-header scheme. Power-cut safe as today.
- **+Drive (DT2)**: `core/plusdrive-write` calls the firmware's C-level open, write and close (DT2 1.17 write `FUN_4015866a`; open and close to
  resolve on 1.17 **[O]**), which take the firmware's recursive filesystem mutex **[D]**. Elektron's allocation, inode and flush code runs unchanged.
  Close runs the firmware's read-back hash (`hashlittle`, seed 0x43fa243a) and stores `hash|1`; `file_close` returns it. Rules from the 1.17
  write-path analysis **[D]**: task context only, never an ISR; never on a file the stock MidiRpc writer has open; whole 32 KiB pages from a
  16-byte-aligned buffer; always close so the hash runs; `file_abort` closes and deletes. The filesystem lock is held for the close hash (seconds for
  a large file): the stock sample browser waits meanwhile.
- Context: only in an I/O worker step (3.2). Other contexts get `E_CONTEXT`.

### 3.2 I/O worker

```c
i32  io_post(io_step step, io_done done, u8 *ctx);  // run on the runtime's I/O task; the step may block and call file_*
i32  io_cancel(i32 id);
void io_wake(i32 id);                              // from an ISR, an adapter or another task: wake a waiting step
// io_step returns DK_IO_MORE (call again), DK_IO_WAIT (sleep until io_wake or a timeout), 0 (done) or a negative error
```

A reusable SDK facility, not only for usb-fast (sampler recording, exports). One runtime-owned RTOS task, created once from the boot hook when a
loaded module imports `io_post`, idle when it has no work; it is never deleted. Priority below the UI task (near the stock loader worker), so a
transfer does not slow the UI; its own stack; one step at a time, round robin between jobs; `done` in the UI task. It sleeps on a semaphore that
`io_wake` posts (ISR-safe), so a transfer is event-driven: the usb-bulk ISR wakes it when a buffer is full. Unload cancels the owner's jobs (as
`worker::retire`). Modules get no raw task or semaphore calls: the runtime owns the task's stack and lifetime. The existing job worker (`job_post`,
the stock loader task, 10 ms, no API calls) stays for short compute steps.

Needs new RTOS anchors (task create, semaphore init, pend and post) on DT2 1.17, DN2 1.12 and DN2 1.11. Creating a task after the scheduler
started is **[O]** (static check, then an emulator proof); if it is not safe, `io_post` falls back to the stock loader worker behind the same API.

### 3.3 Hash

`hash32_init/update/finish` export the runtime's `hash32.rs` (the +Drive content hash) with a caller-owned 28-byte state (`dk_hash32`), so modules
and the host agree on one implementation.

## 4. Adapter `core/usb-bulk`

The USB engine is the same code at the same addresses on DT2 1.17 and DN2 1.12 (strict hashes equal); only data addresses differ **[D]**. One source,
per-device data through anchors.

- **Imports for modules** (`requires = "core/usb-bulk"`): `usb_bulk_enable(io_id)` (the I/O job to wake), `usb_bulk_take(&buf, &len)` (the next
  full RX buffer, or none: non-blocking), `usb_bulk_release(buf)`, `usb_bulk_send(buf, len)` (a reply on EP2 IN), `usb_bulk_state()`.
- **Anchors** (new `devices/fw/usb.toml`, resolved into `devices/ports/`): `cf.usb.data.port_table` (`cf_ptr` in FUN_40006c16, offset 0x22),
  `cf.usb.fn.connect` (FUN_400043e2), `cf.usb.fn.disconnect` (FUN_4000436e), `cf.usb.fn.queue_td` (FUN_40005a86), `cf.usb.fn.cancel_ep`
  (FUN_400059aa), `cf.usb.fn.td_alloc` (FUN_40005764, needs `ordinal`), `cf.usb.fn.stop` (FUN_40006bac), `cf.usb.fn.restart` (FUN_40006bc6).
  DN2 equivalents of the last two: check by hash **[O]**.
- **Descriptor**: a 124-byte copy of port entry 2's configuration with interface 2 appended (`09 04 02 00 02 FF 00 00 00`, `07 05 02 02 0002 00`,
  `07 05 82 02 0002 00`), bNumInterfaces 3, wTotalLength 0x7c. Built from the stock bytes after a hash check, so a changed firmware refuses instead
  of serving a wrong descriptor.
- **Enable** (the module's `dkm_init`, during boot): write entry 2 +0 (connect), +4 (disconnect), +0xC (config pointer), +0x10 (length) in RAM, then
  re-enumerate with stop + restart (the firmware's own USB-mode switch path, 30 ticks). The table is RAM: no static patch, no `[[op]]` on USB code.
- **Connect** (our +0): stock FUN_400044b8, then QH 0x100 and 0x140 caps 0x22000000, ENDPTCTRL2 = 0xc800c8, prime EP2 OUT. Re-entrant (resume calls it
  too). **Disconnect** (our +4): ENDPTCTRL2 = 0, flush EP2 RX and TX (bounded poll, unlike the stock one), cancel both queues, then stock FUN_4000436e.
- **RX**: dTDs from the 32-entry bulk pool (MIDI mode uses about 4) **[D]**, buffers in the uncached DDR alias. Multi-page dTDs (16 KiB each) are our
  own work: the firmware only uses single-page dTDs **[O]**. The ISR callback marks a buffer full, re-primes if a buffer is free and calls
  `io_wake` for the registered job (the runtime posts the I/O task's semaphore, ISR-safe). No free buffer: no prime, the host gets NAK, so flow control
  needs no window protocol.
- **TX**: dTD token `len<<16 | 0x8080`, queue index 6.

## 5. Module `usb-fast`

- Tool module (`modules/tools/usb-fast`), C on the SDK, `requires = ["core/usb-bulk"]`, for both devices.
- `dkm_init`: `io_post` the transfer loop, then `usb_bulk_enable(id)`. The step takes full buffers, writes and hashes them, replies, and returns
  `DK_IO_WAIT` when none is left.
- **Protocol on EP2** (big-endian, no SysEx framing, no 8-in-7):

  ```text
  request:  magic 'DKB1' | cmd u8 | seq u8 | flags u16 | len u32 | body[len]
  reply:    magic 'DKB1' | cmd|0x40 | seq | status i16 | len u32 | body[len]
  ```

  Commands: INFO, BEGIN (root, path, size, expected hash), DATA (offset, up to 64 KiB), END (returns the device's hash), ABORT, VERIFY (DN2 store:
  read back a range and hash it). Numbers and status codes in `sdk.toml [protocol]`, generated for C, Rust and TypeScript like the SysEx protocol.
  LIST, MKDIR, DELETE and everything else stay on SysEx.
- **Verification**: DN2: the store's running hash at close (`E_HASH`), then VERIFY reads back (on by default). DT2: the firmware's close hash returned
  by END; the host compares it with its `content_hash` of the bytes sent plus the 16-byte trailer, as today's GetFileInfoV2 check does. No overwrite:
  the host checks the listing first (as `sameOnDevice` does) and deletes over SysEx when asked.

## 6. Safety and recovery

- Not selected, or safe mode: the module is not loaded, the port table is never written, USB is stock.
- A bad descriptor or a USB hang: hold REC at boot (safe mode, `boot.rs:255-309`), then deselect usb-fast in the DIGIKIT menu. DIN MIDI OS upgrade
  stays as the last resort. Prove the safe-mode path on the emulator before the first hardware run, and do the REC drill once on each device.
- Selecting the module costs one USB reconnect during boot when the host had already enumerated: the MIDI port disappears briefly then.
- SDK file writes: no overwrite, whole-file sessions, the existing card write gate (`card::admit`), one writer per root.

## 7. Host side

- **`packages/dkbulk`** (Rust, `nusb`): find the device by VID 0x1935 and interface class 0xFF, claim interface 2, run the protocol with two DATA
  requests in flight. Used by the desktop app (Tauri command) and a CLI (`mise run` task, also the repeatable bench). The session layer gets a
  `BulkLink` (`write(bytes)`, `read(timeout)`) beside the SysEx-shaped DkTransport, and a samples backend that uses bulk when the interface is present
  and falls back to SysEx otherwise.
- **Web**: a WebUSB `BulkLink` in `apps/web` (Chrome on macOS, Linux, ChromeOS). Claiming a vendor interface of a composite device whose other
  interfaces belong to the OS MIDI driver: **[O]** on macOS, prove early.
- **Windows**: needs WinUSB binding (MS OS 2.0 descriptors through an EP0 hook, bcdUSB 0x0210). Not in v1; SysEx keeps working there.

## 8. Emulator

`midi.usb.in` calls the firmware's EP1 callback directly at ISR level 5 (`runtime.rs:2604-2727`). Add `usb.bulk.in` and `usb.bulk.out`: deliver bytes by
calling the adapter's EP2 OUT completion callback (found by its symbol in the patch plan) under the same ISR emulation and pacing, and capture what
`usb_bulk_send` submits. That tests the protocol, the I/O worker, the file API on both roots, DT2 +Drive writes (read back with plusdrive) and the
power-cut cases. Not testable in the emulator: enumeration, the descriptor on a real host, the dTD engine and real throughput. Descriptor bytes and
table words are checked in a device test by reading guest RAM.

## 9. Safety and reliability proof

**Emulator, DN2 and DT2, with each step:**

- Store writer split: `upload`, `upload-depth`, `upload-faults`, `card-safety` and `store-format` device tests pass unchanged.
- Power cuts: the `DKRT_UPLOAD_CUT` and `DKRT_UPLOAD_TEAR` knobs applied to SDK writes and bulk uploads; the store reads back the old or the new
  state, never a broken one.
- Concurrency: SysEx upload, LIST and VERIFY during an SDK write or commit get `E_BUSY` and nothing is corrupted; a module unloaded mid-write aborts
  cleanly.
- DT2 +Drive: after every write, abort and failure, plusdrive confirms that the hashes of all other files are unchanged and no partial file is left;
  a stock MidiRpc writer and ours never share a file.
- Bulk link faults: lost, duplicate, truncated and out-of-order packets, the host gone mid-file, a USB reset mid-transfer; each ends with the partial
  file removed and SysEx still working.
- Soak: hundreds of files of random sizes, a few large ones, cancels at random points; every file checked by hash.
- UI: the heartbeat and `stall_count` show no stall during a transfer.

**Real device:**

- After step 3 (DN2): enumeration with interface 2, echo, MIDI still working beside it, unplug and replug, the REC safe-mode drill and deselect.
- Step 6 (DN2, then DT2): the bench; a hash listing of every existing sample before and after (identical); USB pulled mid-transfer, then recovery; a
  large file and a folder of hundreds of files; the REC drill on DT2. On DT2 cut USB, not power; back up +Drive with Elektron Transfer first.

**Before the PR:** an independent adversarial code review (given the code, not our conclusions) and `mise run test:full`.

## 10. Work order and proof

Each step ends with its own tests (owner's Nx target), DN2 first, then DT2.

| # | Step | Proof | Agent time |
|---|---|---|---|
| 0 | Findings 42 and 43: today's measurements and corrections | review against the image bytes | 1 hour |
| 1 | Store writer split out of `upload.rs`; SDK `file_*` on the store, `io_post`, `hash32_*` | existing upload and `upload-cut` tests unchanged; a test module writes, aborts, deletes; `E_BUSY` against a SysEx upload | half a day |
| 2 | `core/plusdrive-write` (DT2) behind `file_*` on the samples root | write, close hash, abort cleanup, other files' hashes unchanged (plusdrive read) | half a day |
| 3 | `core/usb-bulk`, anchors, emulator `usb.bulk.in/out`, then a hardware echo on the DN2 | table words and descriptor bytes in RAM; echo through a test module; deselect gives stock words; the DN2 enumerates with interface 2 and echoes | 1 day |
| 4 | `usb-fast` module and protocol | upload, VERIFY, `E_HASH`, ABORT, power cut over bulk, both devices | a few hours |
| 5 | `dkbulk`, desktop backend with SysEx fallback, CLI (also the bench); WebUSB link | host tests on a fake device; workbench against the emulator | 1 day |
| 6 | Hardware, DN2 then DT2: bench, REC drill, before/after listing of existing samples | measured MB/s in finding 43 | 1 hour |

About 3 to 4 agent days. Outcome risk: medium, separate from the time: enumeration, multi-page dTDs, the real eMMC rate and WebUSB on macOS are
hardware-only; the echo in step 3 settles enumeration early. Steps 1 and 2 pay off on their own (file writes for any module).

## 11. Open questions

1. Is creating the I/O worker task safe from our boot hook (3.2)?
2. Multi-page dTDs and uncached buffers in our arena (4).
3. DN2 1.12 addresses of the USB stop/restart pair; DT2 1.17 file open and close (4, 3.1).
4. WebUSB claim on macOS with the MIDI interfaces owned by the OS (7).
