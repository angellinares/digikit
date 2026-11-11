# 42. USB transfer routes for modules and samples (spike, static)

Spike, 2026-10-17, updated 2026-11-10. Static; the emulator models USB at message level. The one hardware measurement (a Digitone II
at high speed, SysEx rates) is in finding 43.

Marks: **[D]** read or measured once, **[O]** open. Nothing here is **[V]**: no second-agent byte check. Addresses are DT2 1.17 unless
marked 1.16; file offset = VA - 0x40000400 (`section_3_MAIN_OS.bin`). The USB engine functions (`FUN_40004292`, `FUN_400041c2`, `FUN_40000fe0`,
`FUN_40000ea0`, `FUN_40001118`, `FUN_40004504`, `FUN_400044ca`, `FUN_400043e2`, `FUN_4000436e`, `FUN_40005a86`, `FUN_400059aa`, `FUN_40005764`,
`FUN_40005560`, `FUN_400054fa`, `FUN_40005ff6`, `FUN_4000677a`, `FUN_40006c16`, `FUN_40006c52`, `vector_134_handler` 0x40006e9a) are identical
code at the same addresses on DT2 1.16, DT2 1.17 and DN2 1.12 **[D]** (cfdb strict hashes and raw bytes); only data addresses differ. The MIDI-side
functions of 1.17 are 0x2e0 above their 1.16 addresses. Manual: out/refs/MCF5441XRM
(chapter 24), user manual out/refs/Digitakt-2-User-Manual_ENG_OS1.03A_241023.

## 1. USB controller, speed, descriptors

- **Controller [D]**: MCF5441x USB OTG at 0xFC0B0000, device mode, EHCI-style registers. `FUN_40006c52` (0x40006c52) is the init:
  MISCCR2 (0xEC09001A) bit 0 cleared = OTG uses the external ULPI PHY (manual 10.3.10), PORTSC1 (0xFC0B0184) PTS = 10b (ULPI),
  pin-mux writes at 0xEC094050/4D/5E, USB timer 0xFC0B0080/84, vectors: 134 = USB (`vector_134_handler` 0x40006e9a), 209 =
  USB timer/OTG status (0x40005e10). `FUN_40005cde` (0x40005cde) starts the device: BURSTSIZE 0x204, USBMODE (0x1A8) = 0x1e (device),
  EPLISTADDR = 0x80000000 (the QH list sits in the on-chip SRAM, dTDs at 0x80008000+), USBINTR 0x157, then RS.
- **High speed [D]**: PORTSC bit 24 (PFSC, force full speed) is cleared when the port entry's flag word (+0x24) is 1. Entries 2, 4, 5, 6
  of the port table (DT2 1.17 0x4029ef80, DN2 1.12 0x40287af0, + n*0x28) have flag 1; entries 1 and 3 (flag 0) are full-speed variants and are not selected by the
  normal boot (the inline mapping in `FUN_400cc9c8` picks 6, 5 or 2; 4 in a service flag mode). The user manual's spec list says "Hi-speed USB 2.0 port".
  All bulk descriptors say 512-byte packets and `FUN_400043e2` sets QH EP1 OUT = 512. DCCPARAMS = 0x184: **only 4 endpoint
  numbers (EP0-EP3)**, 8 QHs at 0x80000000 + {0x00,0x80,0x100,0x180} (OUT) / {0x40,0xC0,0x140,0x1C0} (IN). **Measured:** IOKit
  reports a real Digitone II (OS 1.12) at high speed (Device Speed 2, USBSpeed 3) **[D]**.
- **Descriptors [D]**: .data at DT2 1.17 0x40311000-0x4031152b (addresses in this table are 1.16 values); port table entry = {+0 connect cb, +4 disconnect cb, +8 device desc, +0xC config desc,
  +0x10 wTotalLength, +0x14/0x18 other-speed ptr/len, +0x1C string table 0x4029eb58, +0x20 string count 9, +0x24 HS flag}.
  The configs' wTotalLength bytes are written at run time by `FUN_4000677a` (0x4000677a, immediates 0x65, 0x4b, 0x107, 0x148). Device
  descriptors: bcdUSB 2.00, EP0 max packet 64, VID 0x1935, nconf 1.

  | mode (setting byte, DT2 `DAT_4099d5c4`, DN2 `DAT_405ce898`: 0, 1, 2) | port | device desc / PID | config | interfaces and endpoints |
  |---|---|---|---|---|
  | USB MIDI (0) | 2 | 0x40311298 / 0x012b, class 0 | 0x403111ce, 101 B | AudioControl + MIDIStreaming; **EP1 OUT bulk 512, EP1 IN bulk 512**; 2 embedded jacks each way = 2 cables |
  | USB AUDIO/MIDI (2) | 5 | 0x4031150b / 0x102b, EF/02/01 | 0x403113c3, 328 B | UAC2 (EP3 OUT iso 56 B, EP2 IN iso feedback, EP3 IN iso 56 B) + MIDI (EP1 bulk 512 pair) |
  | OVERBRIDGE (1) | 6 | 0x403113b1 / 0x0b2b | 0x403112aa, 263 B | **vendor class 0xFF**: if1 EP3 IN interrupt (32/88/1012 B alts), if2 EP3 OUT interrupt (32/88/256 B), if3 **EP2 OUT bulk 512 + EP2 IN interrupt 64**; MIDI pair EP1 bulk 512 |
  | service (4) | 4 | 0x403111bc / 0xffff | 0x40311126, 75 B | CDC-ACM + CDC data (EP2 bulk 512 pair, EP3 IN interrupt) |

  The setting byte is read at boot by an inline mapping (DT2 `FUN_400cc9c8` 0x400cce0c-0x400cce2e; DN2 `FUN_400cedfc` 0x400cf216-0x400cf238): 0 -> port 2,
  1 -> port 6, 2 -> port 5. The menu actions (DT2 `FUN_4010331a`, `FUN_40103330`, `FUN_40103348`; DN2 `FUN_40104dc2`, `FUN_40104dd8`, `FUN_40104df0`) each store
  the value and call `FUN_40006c16` (the third, storing 2 / port 5, is inferred). The setter is `FUN_400bb7ba`. **[D]**
- EP0 handler `FUN_40005ff6` (0x40005ff6): standard requests plus vendor requests 0xC0/0x40 (0..5), 0xC1 (0x0A, 0x0B, 0x11, 0x12). No BOS
  and no string index 0xEE: Windows will not bind WinUSB by itself **[D]**. DEVICE_QUALIFIER is stalled (HS-only device).

## 2. SysEx receive path (what a MidiRpc byte goes through)

Stages, tasks and sizes **[D]**:

1. **ISR** `vector_134_handler`: walks the 8 queue heads, runs the dTD completion callback (dTD+0x24) with (bytes, dTD). EP1 OUT callback
   `FUN_40004292`: posts `FUN_40004504(len, buf)` to the deferred-call queue (`FUN_40000fe0`, ring of 128 x 28 B, no full check; it cannot fill
   because only 32 slots exist) and re-primes at once in ISR context (it tail-calls `FUN_400041c2`). **Two 512-byte dTDs outstanding**; 32
   buffer slots of 512 B at 0x4fe3d000 (bitmap `DAT_409659e4` on DT2 1.17); `FUN_400044ca` frees a slot at the end of `FUN_40004504`.
   No timer, SOF (USBINTR 0x157 has no SOF) or tick gating: the firmware has no structural pacing **[D]**.
2. **Deferred-call task** (entry `FUN_40000ea0`, priority arg 9, 4 KB stack, made by `FUN_40001118`): `FUN_40004504` splits the packet
   into 4-byte USB-MIDI events. **Per event it copies a 64-byte CIN length table onto the stack (1.17 `FUN_40136388`; 1.16 `FUN_401360ac`)**, then a 9-instruction
   loop per payload byte (3 per event). About 95 instructions per event: 24 per USB wire byte (4 per event), 32 per SysEx byte (3 per event). Cable bit 4 selects cable 0 or 1.
   Cable 0 goes to `FUN_4011da4c` (1.17; 1.16 `FUN_4011d76c`), cable 1 to 1.16 `FUN_4011d9a0` (1.17 address not resolved **[O]**).
3. **SysEx assembler** `FUN_4011da4c` (cable 0, 1.17): about 18 instructions per data byte (static), absolute globals. It builds linked 160-byte
   blocks (151 data bytes, 1024-block pool at 0x47db6b80 = 160 KiB). The message is queued at F7 only. When the pool is empty the
   task waits on a semaphore (`FUN_4000141a(0x47db4b70)`); the free-blocks function (1.17 0x4011d844) frees blocks. Cable 1 (`FUN_4011d1f6`, contiguous buffer,
   32,767 data bytes at most) feeds queue 3, which the MIDI task pops and ignores; DIN feeds queue 1. **MidiRpc is cable 0 -> queue 2
   (sources 2/4)**, which fits finding 25 (device 0x10 handler `FUN_4011efb8` returns unless source is 2 or 4).
4. **MIDI task** (1.17 0x4011f08e; 1.16 `FUN_4011edae`; priority 7, 32 KB stack): the gather function (1.17 0x4011d8c0, about 0.5 instructions per byte) gathers the blocks into one contiguous
   buffer at 0x44d2e8d8 with a **hard limit of 0x20000 bytes (128 KiB)**; larger messages return -1 and are dropped. Then the
   router `FUN_4011e9f0` -> `FUN_4011efb8` -> `FUN_40125184` -> factory `0x4013db36` -> `FUN_40152566`.
5. **[C] The device-0x10 body is 8-in-7 packed.** `FUN_4011efb8` passes (message + 6, length - 7) = from the byte after `10 00`, no F7. `FUN_40152566`
   unpacks it through a virtual `VectorOutputStream::vfunc_2` call per output byte (about 30 instructions per byte): the first byte of each
   8-byte group is the collector, data byte j (1..7) gets bit 7 from collector bit (7-j). **The "cmd" byte 6 in docs/MIDI-SYSEX-RPC.md section 11
   is the first collector** (0 for a header-only Ping, which is why it looked ignored); the `DeviceUID` reply is 5 bytes = 4 + 1 collector.
   docs/MIDI-SYSEX-RPC.md section 11 ("raw body, no packing"), `devrpc.build_request` and `RPC_WRITE` (raw 0x55 x 1024) are wrong for any payload
   byte >= 0x80 or longer than 7 bytes; this may be why finding 25 never found the written bytes. Net wire efficiency 7/8 on RX.
   The reply path has the same framing.
6. **Handlers** (same task): the FsSample requests are dispatched in `FUN_401283e2` (0x401283e2, 9.7 KB); copies on the write path: gather
   (1), unpack into a growing vector (doubling copies), `FUN_401d3b8e` message copy, `FUN_401dcd40` payload copy, then
   `FileOutputStream` (vtable 0x402257ec, `array::at` 0x401587d2 is its write) memcpy into a **128 KiB buffer** flushed by
   `FileOutputStream::vfunc_3` (0x4015897c) -> `FUN_4015893c` -> path filesystem -> `FUN_4012e0c0` (CMD25, eDMA, up to 64 KiB per command,
   direct DMA when 16-byte aligned).
7. **Reply TX**: `FUN_40004734` (called in the MIDI task) packs bytes into 4-byte events on cable 0 only, a 4 KB ring at 0x4fe3c000, and
   **spins** (`do {} while`) when the ring is full; TX efficiency 3/4. This matters for reading samples from the device, not for writes.

Task priorities on DT2 1.17 (higher = more urgent) **[D]**: 10 unidentified (0x40002a46); 9 deferred-call (DPC) task; 7 MIDI task and intro
task; 6 main OS; 5 0x401351e6 and 0x40153c26; 4 0x4013526e and 0x40135302; 3 MidiAsyncWorker site; 2 console.

Cost per USB wire byte, static **[D]**: split about 24, assembler about 18, gather about 0.5: **about 43 per wire byte**. The core runs at 264 MHz, but the firmware's own
load leaves only about 53 M instructions per second free (inferred in finding 43 from the VERIFY rate matching the emulator model). **Measured on a
real Digitone II (finding 43): 0.857 MB/s of SysEx bytes when the firmware ignores the message, 0.694 MB/s for our DATA handler.** The cap is the
firmware's receive chain in the deferred-call task (priority 9), which uses all free CPU; the host, the wire and the eMMC are not the limit.

## 3. Transfer file-write protocol as the firmware implements it

- Requests (type byte after the 5-byte MidiRpc header, all packed): OpenFileForWrite 0x40 (path, NUL), WriteFileV1 0x42, CloseFileWriter 0x41,
  WriteFileV2 0x46 (answers "File is not open" in finding 25; layout unread **[O]**). All 44 advertised commands are in the Ping reply (MIDI-SYSEX-RPC.md).
- **WriteFileV1 request [D]** (DT2 1.17): the request constructor `FsSampleWriteFileV1Request` 0x401e0ffe (make_shared) reads `u32 handle, u32 length,
  u32 offset, data[length]`, big-endian, after unpacking (finding 25's script used handle, offset, length; the ctor reads length second). It resizes a
  std::vector to `length` and copies min(length, bytes left), with no length check. `operator new` is a 32 MiB buddy heap (body 0x4011e954), so the only
  size limit is the gather limit 0x20000 of SysEx bytes (about 114 KB of payload). Writer step `FUN_40128280`: the V1 call site 0x40129734 passes no
  checksum (only V2 can return status 4). Offset rule: offset == position (file position plus buffered bytes) writes; offset == last offset returns
  status 0 without writing (a duplicate is a no-op); anything else is status 6. A wrong handle drops the stream (status 3). Other codes: 1 not open, 2 write
  failed. Appends only, no seek.
- **Acknowledgement**: one response per request (FsSampleWriteFileV1Response). Nothing in the firmware forces the host to wait: messages
  queue in the 160 KiB block pool and the in-order offset check makes pipelining safe. The DPC task keeps assembling the next message while the
  MIDI task writes the previous one; back-pressure comes from the pool semaphore and USB NAK.
- **Chunk size**: not set by the device. The limit is one SysEx message <= 128 KiB (gather buffer 0x20000), about 114 KB of payload. What Transfer sends
  is **[O]**; finding 25's 1024 bytes is the emulator test's choice. The fixed cost per request is about 6 ms of model time, so 16 KiB requests remove
  most of it; 16 KiB is the recommended size if the SysEx path is ever tuned (not done: it becomes the fallback).
- **Where the bytes go** [D]: file bytes unchanged into the `FileOutputStream` 128 KiB buffer (stream+0x10c), with no flush per request. The flush at
  0x20000 goes through `FUN_4015866a` (FS mutex 0x46f4deb0, 32 KiB pages; finding 14) and `FUN_4012e39c` (eMMC mutex 0x47de3cac, 64 KiB per CMD25; a bounce
  buffer at 0x4fe69300 for unaligned sources; the stream buffer is 12 mod 16, so it probably always bounces). Close: stream vfunc 0x401593c2 ->
  `FUN_40158cc4` -> `FUN_4015b3aa` (takes the FS mutex) -> `FUN_4015b1e8` reads the file back in 0x8000 pieces into 0x4066d8c0 and hashes it
  (hashlittle-style `FUN_4015acfc`, seeds 0x43fa243a) in the calling task, before the reply. The hash state is global and only safe under the FS mutex. The
  FS mutex is a recursive owner lock (lock `FUN_400015a0`, unlock `FUN_400016d2`), not tied to the MIDI task. An idle writer is closed after 20,000 ms by
  a timer with a 500 ms period (`FUN_4012823e`); the tick task is **[O]** (likely the main OS task). Cost of the close hash for a 300 MB sample is **[O]**.
- eMMC: CMD6 switches HS timing and 4-bit bus (`FUN_4012d4b2`: args 0x03b90100, 0x03b70100; PROCTL DTW = 4-bit). Bus ceiling 52 MHz x 4 bit =
  26 MB/s; the firmware's SDCLK divider was not decoded **[O]**. Real eMMC sustained program rate is unknown (part not identified).
- **What limits the SysEx route**: the CPU of the firmware's receive chain, not round trips (finding 43, hardware section: depth 2 gives no gain, and
  receive work does not overlap with other MIDI-task work). Our store reaches about 0.6 MB/s of payload on a Digitone II; a SysEx route cannot pass about
  0.86 MB/s of SysEx bytes (about 0.75 MB/s of payload) even with a free handler. The 0.5 MB/s of the stock write on a Digitakt II is in the same range.

## 4. A vendor bulk interface

Plan: `docs/plans/usb-fast.md`. Facts below are static, DT2 1.17 and DN2 1.12 **[D]**.

- **Port table** (8 entries of 0x28 bytes): base DT2 0x4029ef80, DN2 0x40287af0; entry 2 (USB MIDI) DT2 0x4029efd0, DN2 0x40287b40. Fields: +0 connect,
  +4 disconnect, +8 device descriptor, +0xC config pointer, +0x10 config length, +0x14/+0x18 other speed, +0x1C string table, +0x20 string count (9),
  +0x24 HS flag. Port 2 config descriptor: DT2 0x403111ce, DN2 0x402fc1ce, 101 bytes, identical bytes on both. Device descriptor DT2 0x40311298,
  DN2 0x402fc298, PID 0x012b / 0x0134. Free space (`cf.flash_cave`): DT2 0x403117c4-0x40312000 (2108 B), DN2 0x402fc7a8-0x402fd000 (2136 B).
- **Descriptor patch**: port 2 uses only EP0 and EP1, so EP2/EP3 are free. A new interface 2 (class 0xFF, EP2 OUT bulk 512 + EP2 IN bulk 512) is 23 bytes
  (9 + 7 + 7): wTotalLength becomes 0x7c and bNumInterfaces 3. GET_DESCRIPTOR(config) serves entry +0xC / +0x10, so with a prefilled copy in free space
  no code immediate in `FUN_4000677a` needs a change (its stores hit the old copy).
- **Endpoint setup**: the connect callback (`FUN_400044b8` -> `FUN_400043e2`) must also set the QH caps of EP2 (512, ZLT), ENDPTCTRL2 (0xFC0B01C8) like
  EP1's 0xc800c8, and prime dTDs with `FUN_40005a86` (copy `FUN_400041c2`; port 4, CDC, already primes EP2 IN with `FUN_40005a86(6, dtd)`). Queue index
  = ep + 4*dir; QH offsets 0x00 0x80 0x100 0x180 0x40 0xc0 0x140 0x1c0. A completion callback (ISR context) queues the buffer to a task that calls the
  firmware's file API. The engine is endpoint-generic (dTD+0x24 callback): no ISR change. The EP1 IN QH caps are 0x20400000 (64 bytes) even at high speed.
- **dTD pools**: 31 dTDs of 0x40 bytes at 0x80000200 (`FUN_40005560`; allocator `FUN_40005764`, non-blocking, ISR-safe) and a separate 4-dTD EP0 pool at
  0x80000a00 (`FUN_400054fa`); iso rings at 0x80008800 / 0x80008c00. MIDI mode uses about 4, so there is room.
- **The firmware's own dTDs** **[D]**: every dTD is built inline at its call site (18 callers of `FUN_40005a86`). Page pointers 2-4 are never written. DDR-buffer dTDs set
  only page pointer 0 with sizes <= 0x1000 (largest: EP0 IN up to 0x1000 at a page-aligned buffer, `FUN_40005be2`), so none crosses a page. SRAM dTDs (audio, iso, e.g.
  `FUN_40002f84` / `FUN_40003118`, 1012 bytes at 0x80009000 + i*0x3f4) set page pointer 1 = (p0 + 0x1000) & ~0xfff.
- **USB DMA buffers** **[D]** sit in the uncached DDR alias (DDR + 0x0800_0000; ACR0 = 0x4007e020 makes only 0x40000000-0x47ffffff cacheable). DT2: 0x4fe3c000 (4 KiB MIDI TX
  ring), 0x4fe3d000 (EP1 RX slots, 32 x 512), 0x4fe4a000 (interrupt RX), 0x4fe53000 (EP0, 4 x 4 KiB), 0x4fe57000 (EP0 status). DN2 has the same layout at -0x1770000 (0x4e6cc000, ...).
- **Controller setup** **[D]**: USBMODE = 0x1e (device, SLOM, SDIS), BURSTSIZE = 0x204, USBINTR = 0x157 (includes SEI); SBUSCFG and TXFILLTUNING are not written. The SEI
  (system error) bit is only cleared: the firmware never resets the controller after it. QH caps: EP1/EP2 OUT 0x22000000 (512, ZLT off), EP2/EP3 IN 0x02000000 (512, ZLT on).
- **Disconnect callback** of port 2 (`FUN_4000436e`) is not a loop: it clears ENDPTCTRL1, flushes EP1 RX and TX through ENDPTFLUSH 0xfc0b01b4 and polls
  the bits (unbounded), cancels both EP1 queues with `FUN_400059aa`, cancels the timer and clears the state words (the decompile loops because it treats
  the register reads as constant). A callback for EP2 must do the same.
- **EP0** (`FUN_40005ff6`): the BOS request is not answered (silent) and unknown requests stall. Vendor bRequest codes in use: 0xC0: 0, 1, 2, 4, 5;
  0x40: 1, 3, 7; 0xC1: 0x0A, 0x0B, 0x11, 0x12.
- **Re-enumeration**: the firmware's USB mode switch `FUN_40006c16(port)` (same address and bytes on DT2 and DN2) compares `port` with the active port index
  CUR_PORT (DT2 0x40965a60, DN2 0x4058f2a8, u32) and returns if equal. Otherwise it calls `FUN_40006bac` (`FUN_40005dcc`: clear USBCMD.RS, USBINTR 0, disconnect
  callback; then a 30-tick delay), writes CUR_PORT (0x40006c28) and CUR_ENTRY = port*0x28 + port table (0x40006c3c; CUR_ENTRY DT2 0x40965a5c, DN2 0x4058f2a4),
  and calls `FUN_40006bc6` (restart via `FUN_40005cde` when the delay ends). `FUN_40006c52(port)` writes both at boot (0x40006c86, 0x40006c8e/0x40006ca4) and
  re-initialises the controller. Boot order on DT2 1.17: the boot thread
  `FUN_400cc9c8` initialises storage, loads settings (USB mode byte `DAT_4099d5c4`), calls `FUN_40006c52(port)` at 0x400cce06 / 0x400cce2e, then creates
  tasks. Our runtime loads modules at the main-OS task entry, after USB init.
- **Engine facts used by `core/usb-bulk`** (static, DT2 1.17 and DN2 1.12; the adapter and its emulator test follow them) **[D]**: the stock configuration bytes have
  wTotalLength 0 in the image (the firmware's `FUN_4000677a` writes 0x65 into its copy at run time and pushes the cache lines), so a copy compares bytes 2 and 3 as free.
  The port table has 7 entries (entry 7 is not a port; the cfdb rule hashes 0x118 bytes). The entry-2 connect callback `FUN_400044b8` is `moveq #1,d0; move.l d0,4(sp);
  bra.w FUN_400043e2`: it writes its caller's argument slot, so it is called with one dummy argument. `FUN_400043e2` calls the stock disconnect first, so connect is re-entrant.
  Each queue head (0x40 bytes at 0x80000000 + offset) has the firmware's own list of queued dTDs at +0x30 (head) and +0x34 (tail), and `FUN_40005a86` sets the interrupt-on-completion
  bit when the dTD has a completion (+0x24) or free (+0x38) callback. The completion loop (`vector_134_handler`, `FUN_400059aa`) calls the free callback (+0x38, returns the dTD
  to the pool of 32) first, then the completion callback with (bytes = dTD+0x20 minus the remaining count, dTD+0x30 = the buffer pointer given at +8); a cancelled, still active dTD
  gets +0x28 instead. `FUN_40006c16` only acts when the port changes; a re-enumeration on the same port is `FUN_40006bac` then `FUN_40006bc6` (it sets the flag that the
  tick `FUN_4000713e` needs to restart the device after the 30-tick delay). The stock disconnect polls ENDPTFLUSH (0xfc0b01b4) without a bound.
- **Overbridge mode** already has a vendor interface 3 with EP2 bulk OUT 512; its protocol is unknown, so the safe design is a new interface in MIDI mode
  **[O]**. Audio mode uses EP2 for feedback: not available there.
- **Bandwidth coexistence**: EP3 audio takes at most 1012 + 256 bytes per 125 us of about 7.5 KB per microframe; bulk keeps the rest. The limiting resource
  is CPU, not the wire **[D]**.
- **Host side**: macOS and Linux Chrome WebUSB can claim a class-0xFF interface of a composite device (audio/MIDI interfaces are protected
  classes, so the existing MIDI pipe is not usable from the browser). Windows needs WinUSB bound: the firmware has no BOS/MS OS 2.0
  or OS string descriptor, so add them (about 1-2 days) or ship an .inf. Desktop: nusb/libusb on the vendor interface.
- **Risks**: no USB model in the emulator (message-level injection only), so enumeration is hardware-only; a bad descriptor leaves USB unusable
  but DIN OS upgrade still works (finding MIDI-SYSEX-RPC 7).
- DN2 1.12 has no FsSample dispatch, so DN2 needs the vendor route or a new RPC regardless **[O]**.

## 4a. Page pointers of dTDs (hardware, DN2, 2026-11-11)

**[D]** Our `core/usb-bulk` dTDs with the page pointers past the used pages set to 0 wedged the controller at a 4096-byte EP2 IN transfer (1-513 bytes worked). The
closing zero-length packet starts at byte 4096 and (inferred) is moved through page pointer 1 = 0; the DMA faults (system error) and only a power cycle recovers.
RM 24-71 step 7: page pointers 1-4 must each be one page above the previous one. The adapter now always fills all five. The emulator model faults a packet whose page
pointer the USB bus master cannot reach (DDR 0x40000000-0x4fffffff, SRAM backdoor 0x80000000-0x8bffffff). Hardware recheck pending **[O]**. The controller has 4 device
endpoints (DCCPARAMS DEN = 4, RM): EP0-EP3.

## 5. Disk mode

- Controller init a standalone program needs (manual chapter 24, firmware init above): ULPI pin mux, MISCCR2[ULPI]=0, USB clock, USBCMD reset, USBMODE
  device (0x1e), QH list in SRAM, EP0 control enumeration, bulk pair, Bulk-Only Transport and SCSI (INQUIRY, READ CAPACITY, READ10, WRITE10,
  REQUEST SENSE), plus the eSDHC bring-up (about 400 bytes of register sequence in `FUN_4012d4b2`: reset, 400 kHz identify, CMD6 HS and 4-bit, eDMA
  channel 59).
- The cheaper variant of "own driver": run inside the firmware. Add port entry 7 (MSC descriptors) to the firmware's USB engine and serve SCSI
  READ/WRITE with `FUN_4012deda` / `FUN_4012e0c0` (multi-block CMD18/CMD25, up to 64 KiB per call). What the bootstrap leaves configured is unread **[O]**;
  MAIN OS configures USB and eSDHC itself, so the standalone program cannot rely on it.
- eMMC write ceiling: 26 MB/s bus (4-bit, 52 MHz) minus eMMC programming; realistic 8-20 MB/s **[O]**. USB HS bulk 35 MB/s is not the limit.
- The inbox: the module store (12 MiB) or a reserved region; the path filesystem allocates 32 KB pages across the card, so a larger FAT inbox for
  samples needs those pages reserved in its bitmaps **[O]**. The host mounts the volume in the OS file manager; WebUSB cannot (mass storage is a
  protected class), so the browser route does not exist for disk mode. Import at next boot through the mutex-guarded file API as in finding 23 S3.

## 6. Sample format

The device does **not** convert. The write path stores bytes as given (no RIFF/WAVE string anywhere in the image, finding 14), and the loader
requires the native file: 0x40-byte header (+1 stereo flag, +4 length, +8 rate 48000, +0x14 = 0x7F), big-endian int16 PCM, 16-byte trailer (finding 14
"The native sample file", **[V]** there). So the host (Transfer, our desktop app, the browser) converts: 48 kHz, 16-bit BE, as packages/emulator/plusdrive
does. The Query key `sample_file.interleaved_stereo_support` is only a capability answer **[D]**.

## Summary table

| route | ceiling | limiting stage | cost (agent days) | risk |
|---|---|---|---|---|
| A own SysEx (our store, measured on a Digitone II) | about 0.6 MB/s payload; about 0.75 at most with a free handler (0.857 MB/s of SysEx bytes) | firmware receive chain in the deferred-call task (CPU) | none: today's path | low |
| B vendor bulk interface (`docs/plans/usb-fast.md`) | 5-10 MB/s (4-20) **[O]** | path filesystem + eMMC flush path | 5-8 (+1-2 Windows) | medium (hardware-only test) |
| C disk mode (in-firmware MSC) | 10-20 MB/s | eMMC programming | 8-12 | high |
