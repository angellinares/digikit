# Digitakt II live MIDI SysEx / MidiRpc protocol (1.16)

The runtime SysEx command surface of **MAIN OS 1.16**: how the device is spoken
to over its USB (or DIN) MIDI port while it is running normally, as opposed to
the OS-upgrade *container* format (that is `docs/FINDINGS.md` "Container", and
`tools/firmware/container.py`). Discovered 2026-09-20.

Scope note: `docs/FINDINGS.md` is 1.15C-scoped. **Everything here is
Digitakt II 1.16**, MAIN OS `out/sections/dt2-1.16/section_3_MAIN_OS.bin`,
sha-256 `57bb4dfa8df07d846adc72fdb4fb0d3cd3c5680c524bf498338460207e008e7d`
(matches the `out/ghidra/dt2-1.16-seeded` Ghidra program). The physical device
was upgraded to 1.16 on 2026-09-20, so it and the analysis target are the same
version.

Evidence classes follow `docs/FINDINGS.md`: **[V]** verified by running it here ·
**[D]** documented by reading the image once, not re-checked · **[O]** open.

> **Two distinct protocols.** Device-id `0x14` (§2–§9) is the OS-**upgrade**
> container path: 8-in-7 packed body, `type | 0x80` responses, a trailing
> checksum+length. Device-id **`0x10` (§11)** is the **live** path Elektron
> Transfer actually uses: raw body, no packing, no checksum, no length, no
> response bit. They share the `MidiRpcMessage` class and type enum but nothing
> else. A live round-trip against real hardware is confirmed for the `0x10`
> path (§11).

> **Verification status.** The `0x10` protocol in §11 is confirmed by a live
> hardware round-trip **[V]** plus a firmware trace. The `0x14`/upgrade static
> decode in §2–§9 is single-agent, read once — **[D]** — and has *not* had the
> project's required second-agent byte-check. Do not promote §2–§9 to **[V]**
> without that check. Addresses are for follow-up.

## Why this matters

- **Delivery is the device's own mechanism.** `OsUpgradeStart / OsUpgradeWrite /
  OsUpgradeEnd` (RPC types `0x50 / 0x51 / 0x52`) are ordinary MidiRpc commands
  over this same MIDI port. Flashing a patched OS is driving the official
  update RPC, not a third-party tool. See §7.
- **A live oracle exists.** `Ping`, `SoftwareVersion`, `DeviceUID`,
  `StorageSpace`, `Query` are read-only round-trips. Once we speak the framing
  we can interrogate the real device, which demotes emulator fidelity (gate A2)
  from a blocker to a convenience.
- **No raw peek/poke.** There is no arbitrary-memory read/write command. Live
  data access is filesystem-shaped (`FsSample*`, `Data*`), not RAM-shaped.
- **`DigisharcSysexRpc` is not a path to the DSP** (checked, ruled out). Despite
  the name, its handler (`"Handle sysex RPC"` worker, callback `0x40122952`)
  never calls the DSPI2 driver or touches the SHARC transport; its one payload
  op just returns the hardcoded string `"MY ANALOG FOUR"` (shared-codebase
  cruft). "Digisharc" is a serialization namespace shared with DSP-side
  structs, not a wire into the DSP. Matches the 1.15C "Ruled out as the control
  link" finding in `FINDINGS.md`. **[D]** There is **no discovered live
  SysEx→SHARC channel**; host→DSP influence is only the machine-type byte in
  the periodic DSPI2 frame (§ the ColdFire↔SHARC boundary in `FINDINGS.md`).

## 1. Live device round-trip **[V]**

Over USB MIDI (`Elektron Digitakt II` in/out ports, `python-rtmidi`), with no
device settings changed:

```
sent: F0 7E 7F 06 01 F7          (MIDI Universal Non-Realtime device inquiry)
got:  F0 7E 00 7C 00 F7
```

Sub-ID `0x7C` = **WAIT**. The device routes any `F0 7E …` message
unconditionally into its Sample Dump Standard state machine (§8), whose sub-ID
switch does not handle `0x06` (inquiry) and parks in WAIT. So SysEx over USB
works out of the box; no MIDI menu setting is required to receive.

## 2. Outer framing and the command jump table **[D]**

Raw MIDI byte router: `FUN_4011e9f0`, dispatches on the status-byte low nibble.

Elektron envelope match: the literal `f0 00 20 3c` lives at `0x4021e11a`. A
device-ID table at `0x402b4098` (24 slots; populated `0x00,0x04,0x0a,0x10,0x14,
0x17`) selects a per-device descriptor. Device `0x14` (Digitakt II MAIN OS) →
descriptor at `0x4021e0f0`: separator byte must be `0x00`, command range
`[0x00,0x7F]`, and a **128-entry command jump table at `0x402b4748`** (recovered
by reading the raw pointer array; Ghidra would not resolve it statically —
"too many branches" at `0x4011fda2`).

For device `0x14` the 128 entries collapse to six handlers:

| command range | handler | role |
|---|---|---|
| `0x00-0x0F,0x11-0x4F,0x70-0x77,0x79-0x7D` | `0x4011e9ca` → `FUN_400e27ec` | generic → the **MidiRpc / DigisharcSysexRpc** container (§3) |
| `0x10` | `0x4011e9cc` | singleton, unexamined |
| `0x50-0x5F` | `0x4011eb94` → `FUN_4012208e` | legacy kit/sound/pattern/song/project backup **GET** (object-indexed, `idx < 0x80`, not address-based) |
| `0x60-0x6F` | `0x4011ebba` | symmetric write counterpart, unexamined |
| `0x78` | `0x4011e9ce` | singleton, unexamined |
| `0x7E-0x7F` | `0x4011f1fc` | unexamined |

`FUN_400e27ec` itself contains a further unresolved indirect jump table at
`0x400e2830` (per-object-type dump dispatch), not decoded. **[O]**

## 3. Transport packing, checksum, framing **[D]**

The generic path builds a message as
`header · packed-body · checksum · length · F7`:

```
F0 00 20 3C 14 00 <cmd> <b5> <b6> <b7>   <-- header, FUN_400e25da
<8-in-7 packed body ...>                 <-- FUN_400e26d0
<cksum_hi> <cksum_lo>                     <-- 14-bit additive sum, FUN_400e2750/2716
<len_hi>   <len_lo>                       <-- (bodyLen + 5), 2×7-bit
F7
```

- **8-in-7 packing** (`FUN_40152242`, size `= (rawLen*8 + 6)/7`; unpack
  `FUN_40152428` / `0x401524a2`, dispatch `0x4015253e`) — the same routine the
  OS-upgrade container uses. The whole body (the 5-byte MidiRpc header plus its
  payload) is packed as one contiguous run, MSB-collector byte then 7 data
  bytes. It is **not** sent raw.
- **Checksum** (`FUN_400e2716`): `sum(packed_body[i] & 0x7f) & 0x3fff`, emitted
  MSB-first as two 7-bit bytes.
- The four header bytes after `14 00` — `<cmd> <b5> <b6> <b7>` (each masked
  `&0x7f`) — are the **one unverified gap**. `b5` is very likely the OS-stream
  id (`0x0f` for DT2 on 1.15C). A `0x20` type tag appears when
  `DigisharcSysexRpc` objects are built (`0x400e3150`), a plausible `cmd`.
  Circumstantial guess `cmd=0x20, b5=0x0f, b6=0x00, b7=0x00` — **[O], do not
  trust for hardware without confirming.**

## 4. MidiRpc message layer **[D]**

Base header `elektron::MidiRpcMessage::ctor_dtor` (`0x4013d864`), 5 raw bytes
(pre-packing), big-endian:

```
seq_hi seq_lo   len_hi len_lo   type
```

Master type-byte factory switch at `0x4013aaf4` (called from `0x401243c6`)
builds ~90 concrete request/response classes. **Rule: `type = family | 0x80`
for a response** (verified across families: Ping req `0x01`/resp `0x81`, Query
`0x09`/`0x89`, OsUpgradeWrite `0x51`/`0xD1`).

Request command table (`0x01`–`0x5F`):

| type | name | | type | name |
|---|---|---|---|---|
| 0x01 | Ping | | 0x30 | FsSampleOpenFileForRead |
| 0x02 | SoftwareVersion | | 0x31 | FsSampleCloseFileReader |
| 0x03 | DeviceUID | | 0x32 | FsSampleReadFileV1 |
| 0x04 | Screenshot | | 0x33 | FsRawOpenFileForRead ✝ |
| 0x05 | StorageSpace | | 0x35 | FsRawReadFileV1 ✝ |
| 0x06 | TempoRead | | 0x36 | FsSampleReadFileV2 |
| 0x07 | TempoWrite | | 0x40 | FsSampleOpenFileForWrite |
| 0x09 | Query | | 0x42 | FsSampleWriteFileV1 |
| 0x10 | FsSampleReadDir | | 0x45 | FsRawWriteFileV1 ✝ |
| 0x13 | EnumerateFiles | | 0x46 | FsSampleWriteFileV2 |
| 0x17 | FsSampleListRam | | 0x50 | OsUpgradeStart |
| 0x19 | FsSampleAssign | | 0x51 | OsUpgradeWrite |
| 0x20 | FsSampleDeleteFile | | 0x52 | OsUpgradeEnd |
| 0x21 | FsSampleRenameFile | | 0x53-0x5f | Data* object API |

(Abbreviated; `0x11-0x38` also carry the `FsSample*`/`FsRaw*` create/delete/
rename/get-info/hash variants. ✝ = `FsRaw*`, see §6.)

## 5. Read-only commands (safe live probes) **[D]**

- **Ping (`0x01`)** — request is header-only, `len = 0`, no payload
  (`MidiRpcPingRequest` adds nothing to the base). Response `0x81`
  (`0x401ddb9e`): `u8 blobLen · blob[blobLen] · NUL-terminated string`.
- **SoftwareVersion (`0x02`)** — response `0x401e06ac`: two NUL-terminated
  strings (version, build).
- **DeviceUID (`0x03`)** — response `0x401dce08`: one big-endian `u32`.
- **Screenshot (`0x04`)** — the one command that returns live RAM. The handler
  (`0x401243c6` Screenshot branch → async closure `0x40123728`) `memcpy`s a
  fixed **1024 bytes** of the current OLED shadow framebuffer (`DAT_402b605c`,
  128×64×1bpp, double-buffered with `DAT_402b6058`) into the reply; no request
  field steers the address or length. Response ctor `0x401ddc40`. A read-only,
  no-mod live capture of the display, drivable now over the `0x10` path. **[D]**
  **Checked on 1.17 in the emulator (2026-10-19, lane dkrun) [D]:** the handler is `FUN_401246a6`, its closure `FUN_40123a08` (copies 0x400 bytes
  from the pointer `DAT_402b659c`, the shadow buffer, then sets the done flag), the response constructor
  `elektron::MidiRpcScreenshotResponse::ctor_dtor` at `0x401de056` **[C]** (corrected below: that is the parse-side constructor; 1.16: `0x401243c6`, `0x40123728`, `0x401ddc40`, `0x402b605c`). The reply payload (after the
  type byte `0x84`) is **1033 bytes: a 9-byte header `01 0080 0040 00000400` (format 1, u16 width 128, u16 height 64, u32 length 1024, big endian), then the
  1024 buffer bytes**; the buffer has the layout of the emulator's frame capture (`framebuffer.get`: byte `(7 - y/8) + 8 x`, bit `y mod 8`, so 8 bytes per
  column and the top 8 rows in byte 7 of the column). `tools/device/dkscreen.py` decodes it and writes a PNG (`--screenshot` for the device is that tool:
  `python -m tools.device.dkscreen --port Digitakt out.png`). The scenario `upload-screenshot` sends the request over `midi.usb.in`, reads `midi.usb.out`
  and compares the 1024 bytes with the emulator's own frame capture of the same moment: equal on the main screen and after a key press. Not yet run on the
  device itself **[O]**. **DN2 1.12 in the emulator (2026-10-22, lane tasks-images) [D]:** the same request over `midi.usb.in` is answered with the same 9-byte header and 1024 bytes, equal to
  the frame capture on the main screen and after a key press (scenario `upload-screenshot`, now on both devices). The handler is the same stock command; no DN2-specific change was needed. A real DN2 is not checked **[O]**.
  **[C] Constructors and DN2 facts [V] (second agent, 2026-10-22, hash-matched, static only, not run on hardware):** `0x401de056` (1.17) is the parse-side constructor of `MidiRpcScreenshotResponse`.
  The handler builds the reply with `0x401d4710` through the Sp wrapper `0x401d47bc` (1.17). On DN2 1.12: handler `FUN_40125f9e`; closure `FUN_401253b0` (h_strict `e5b21c0f...`); shadow buffer
  `DAT_402a10cc`, `0x400` bytes; build constructor `0x401bd5aa`; parse constructor `0x401c557a`; the same reply layout and the same `0x10` route. Product ids: DN2 `0x0d` / `0x15`, DT2
  `0x0a` / `0x14`; this route does not use them.
- **StorageSpace (`0x05`)**, **Query (`0x09`)** — read-only, layouts not
  decoded.

Worked Ping request (raw pre-pack body, `seq=0x0001`): `00 01 00 00 01`.
Packed (all bytes `<0x80`, so collector `0x00`): `00 00 01 00 00 01` (6 bytes).
Checksum `0+0+1+0+0+1 = 2` → `00 02`. Length `6+5 = 11` → `00 0B`. Full frame,
with the unverified header bytes bracketed:

```
F0 00 20 3C 14 00 [<cmd> <b5> <b6> <b7>] 00 00 01 00 00 01 00 02 00 0B F7
```

Responses may carry their own monotonic `seq` rather than echoing the request's
(`0x401d3ea6` builds Ping response with `seq=0`, and the header builder
auto-assigns from a global counter when given 0). Not confirmed against a real
round-trip; with one request in flight, ordering suffices for correlation. **[O]**

## 6. Filesystem primitive: live vs dead **[D]**

Both `FsSample*` and `FsRaw*` define an **offset+length file read/write keyed by
an arbitrary path string** — open-by-path → handle, then `{handle, offset,
length[, data]}` (big-endian `u32` fields; path via `FUN_401dd35e`, NUL-
terminated, ≤255 bytes). The write path resizes a heap buffer to the caller's
length then `memcpy`s (`FUN_401dcd40`); the read length is clamped to bytes
remaining in the SysEx buffer, so it cannot over-read the message.

Liveness, by `tools/coldfire/refscan.py` over the raw image (96.78% instruction
coverage), checking each class's RTTI typeinfo for a `dynamic_cast` site:

- **`FsRaw*` (12 request classes): 0 hits — apparently dead on the 1.16 receive
  path.** The factory will parse an incoming `FsRawWriteFileV1` into a live
  object, but nothing ever `dynamic_cast`s to that type, so no I/O follows.
  Control checks (Ping, Query typeinfos each show exactly the one expected site)
  confirm the method detects live sites correctly.
- **`FsSample*` siblings: referenced (live)**, byte-identical wire shape.
- **`OsUpgradeWrite` (`0x51`): live**, the flash-write-with-offset primitive,
  gated by the `OsUpgradeStart`/`End` handshake.

Caveat: refscan is a static byte scan and can miss a computed/indirect typeinfo
load; the FsSample dispatch call site itself was not located, so whether
`FsSampleOpenFile*` confines the path to a sample root (as its name implies) is
**[O]**.

## 7. The OS-upgrade RPC = the delivery channel **[D]**

`OsUpgradeStart (0x50)` / `OsUpgradeWrite (0x51)` / `OsUpgradeEnd (0x52)` are
MidiRpc commands. `MidiRpcOsUpgradeWriteRequest` (`0x401e0e22`) has the same
`{slot, offset, length, data}` shape as the file-write primitives. This is the
same content the offline `.syx` container carries (`tools/firmware/build.py`,
`tools/firmware/roundtrip.py`), delivered over MIDI instead of as a file. It is the
natural target for pushing a rebuilt image to hardware.

Checked in the emulator on DT2 1.17 and DN2 1.12 (finding 75, "The RPC route"): Start `{u32 length, "sysex\0", flag}`, Write `{u32 CRC-32 (init 0xffffffff), u32 length, u32 offset, data}`, End `{commit}`; each is answered, no menu is needed, and the device flashes after End with commit, with no on-device confirm. **[D]**

Nothing sent over **USB** can reach the irreversible bootstrap upgrade: the
bootstrap's STARTUP-menu upgrade path is **MIDI DIN only** (`README.md`,
`docs/FINDINGS.md` "Recovery"), and on 1.16 the bootstrap version gate no longer
fires for a 1.16-derived image anyway (incoming must exceed running).

## 8. Sample Dump Standard (`F0 7E`) **[D]**

Worker `FUN_40121eec` spawns a `MidiAsyncWorker` thread literally named
"Handle sysex SDS", backed by an `SdsManager`, parser `FUN_400e530a`, length
guard `FUN_400e45d0` (≥2). Sub-IDs: `0x01` DUMP HEADER, `0x02` DATA PACKET,
`0x03` DUMP REQUEST, `0x05` extension negotiation, `0x7B` EOF, `0x7C` WAIT,
`0x7D` CANCEL, `0x7E` NAK, `0x7F` ACK — standard MMA handshake, matching the §1
observation. Field readers `FUN_401c8828` (14-bit) / `FUN_401c886c` (21-bit) do
no 7-bit masking and no cursor-vs-length check at that layer; whether
`SdsManager` clamps `sampleLength`/`sampleNumber` upstream is **[O]**.

## 9. Debug console — UART8, not MIDI **[D]**

Not a MIDI surface, recorded here to close it off. Dispatch `strcmp` chain
`FUN_400cae8c` (1.16; was `0x400cd93e` on 1.15C) over UART8 (`0xEC070004`
status, `0xEC07000C` data). Commands include `#HELLO`, `#READ`, `#WRITE`,
`#DUMP_AUDIO`, `#RECEIVE_AUDIO`, `#ENTER_TEST_MODE`, `#MRAM_DUMP`,
`#READ_SERIAL`, and many `#*_TESTED` factory-test variants.

- **`#WRITE <name> <int>` is a named-parameter setter, not a memory poke.**
  `FUN_400cab40` looks the name up in a fixed 11-entry table (one entry named
  `SYNC_1`); a hit calls `entry+0xc(1)` then `entry+4(value)`; a miss prints
  "ADDRESS ERROR". `#READ` uses the same table. Neither takes an address.
- `#DUMP_AUDIO` / `#RECEIVE_AUDIO` take one integer index into a firmware-chosen
  buffer, not an address (index bounds unchecked here — possible OOB, **[O]**).
- `#ENTER_TEST_MODE` sets byte `0x4031be48`, stops audio, arms interrupt
  vector 191, writes `0xFC04C07F`. `FUN_400ceeb4` enter / `FUN_4002dc74` exit.
- **`#MRAM_DUMP` is a fixed 13.81 MiB dump of a flash-mirrored factory blob, not
  a RAM peek.** `FUN_400cae8c` → `FUN_400cabb4(0x4099d588, 0xdd9714)` raw-copies
  `[0x4099d588, 0x41776c9c)` to the console. That region is a checksummed,
  flash-mirrored persistence store (magic `0x434f4b69` "COKi") for serial
  number, UI calibration, reset/test flags and MMC health — its own allocator
  arena, gated by `MRAM_HEADER_BROKEN`/`MRAM_STATE_NOT_WRITTEN`, written to/from
  MMC by `FUN_400f0628`/`FUN_400f04e2`/`FUN_400f044a`. Base and length are
  hard-coded; nothing in the request steers them. It does **not** hold the live
  sequencer/kit/sound working state — that lives just past the dump's end
  (`0x41776cb8`; see `docs/FINDINGS.md`, "Live musical state in RAM"). **[D][C]**
  (Earlier wording implied a battery-backed MRAM chip; the target is a
  DRAM/flash-mirror scratch region.)
- **`#MMCDUMP <addr> <len>` is a raw eMMC sector dump, not a RAM read** (not
  previously recorded). It decodes a caller-supplied address+length (no bounds
  check at the parse site) and streams `[addr, addr+len)` over UART in ≤0x8000
  chunks via `FUN_4012deda`. That leaf is an ESDHC/eDMA card read
  (`_ESDHC_XFERTYP = 0x123a0036`, block index scaled by block size and bounded
  by card block count `_DAT_44e3fea0`, /512 stepping through a fixed DMA landing
  buffer `0x4fe69300` then memcpy to the caller buffer), so it reads +Drive
  storage at the block level — raw, below the FS layer — not main RAM. **[D]**
- **`#DUMP_AUDIO` / `#RECEIVE_AUDIO` cannot over-read.** The index is
  unconditionally clamped to `0..4` (`FUN_400cee80` / `FUN_400cee0e`), selecting
  one of five fixed 2 MiB test buffers at `0x42949548 + n*0x200000`, valid only
  after `#ENTER_TEST_MODE` sets the base `_DAT_47db45ac`. The earlier "possible
  OOB" concern is not borne out. **[D][C]**
- No evidence the console is reachable over any transport other than UART8, and
  no repo evidence UART8 is physically exposed or bridged to USB. **[O]**

## 10. Open questions

1. The four envelope header bytes `<cmd> <b5> <b6> <b7>` (§3) — resolve by
   snooping the official Transfer/Overbridge traffic, or by a bounded read-only
   Ping probe against hardware.
2. Whether responses echo the request `seq` (§5).
3. ~~`DigisharcSysexRpc` — SysEx→SHARC path?~~ Ruled out (see "Why this
   matters"): host-side only, returns `"MY ANALOG FOUR"`, no DSPI2/SHARC reach.
4. `FsSampleOpenFile*` path confinement (§6).
5. `FUN_400e27ec`'s inner jump table `0x400e2830`; the `0x10`, `0x60-0x6F`,
   `0x78`, `0x7E-0x7F` handlers (§2).
6. SDS length/number validation upstream in `SdsManager` (§8).

## 11. The live Transfer protocol (device 0x10) **[V]**

This is the path Elektron Transfer uses and the one we can drive from a host.
Confirmed two ways: a firmware trace of the `0x10` descriptor, and a live
round-trip against the real device on 2026-09-20 (`tools/device/devrpc.py`).

### Frame

Request (host → device) and reply (device → host), confirmed against real
hardware and against a MIDI Monitor capture of Elektron Transfer:

```
request:  F0 00 20 3C 10 00 00 <seq_hi> <seq_lo> 00 00 <type> <payload...> F7
reply:    F0 00 20 3C 10 00 24 <dev16>  ......... <seq_hi> <seq_lo> <type> <payload...> F7
```

- `10` device id, `00` separator — the descriptor at `0x4021e08c` requires the
  separator to be exactly `0x00` (`+0x00` devid `0x10`, `+0x01` sep `0x00`,
  `+0x04` jump-table ptr `0x402b40f8`, `+0x08/+0x0c` cmd range `0..0x7f`).
- **[C] (2026-10-18, finding 42 section 2.5; emulator, not hardware) The whole body after `10 00` is 8-in-7 packed**, request and reply.
  Byte 6 is the first *collector*, not a `cmd`: one collector per up to seven data bytes, data byte j (0-based) takes bit 7 from
  collector bit (6-j). The unpacked request is `seq_hi seq_lo 00 00 type payload...`; the unpacked reply is `counter_hi counter_lo seq_hi seq_lo (type|0x80) payload...` (**[C] 2026-10-29, emulator:** the first pair is a counter of the device's own, not the request's seq: after a boot it counts 1, 2, 3, ..., so it equals the seq only while the host also counts from 1; the echoed seq is the second pair. A client that matches replies by the first pair works after a fresh boot and then loses every reply)
  (the response bit is there; it was hidden by the collector). The raw-body statements below ("`cmd` is ignored", "no 8-in-7 packing", "raw
  NUL-terminated string") hold only while every byte is under 0x80 and the body is at most seven bytes: a Ping, SoftwareVersion, DeviceUID,
  StorageSpace and `ReadDir /` are the same on the wire either way. A longer payload was cut into wrong groups: `ReadDir /factory`,
  `Query`, `DataList` and `DataReadOpen` answered "Invalid path" or an empty list for that reason, and with correct packing they answer
  (DT2 1.16 in the emulator: `ReadDir /factory` lists the factory folders, `Query sample_file.interleaved_stereo_support` answers 1).
  `tools/device/devrpc.py` packs with `tools/firmware/build.py` `encode_8in7` and unpacks with `tools/device/midisniff.py` `unpack_8in7`.
  The `FsSampleOpenFileForWrite 0x40` payload is a `u32` followed by the file name **without a leading slash** (root only here; a name with a
  slash gets status 6): the older guess of a bare path made the file name lose its first four bytes. **[D]**
- **`cmd` (byte 6) [C, see above]:** requests use `0x00`, the device stamps `0x24` on replies.
  All 128 jump-table entries at `0x402b40f8` point to one handler `0x4011efb8`,
  so `cmd` does not gate dispatch — but use `0x00` for requests to match
  Transfer.
- `seq` (bytes 7–8, `u16` BE) is caller-chosen; **the reply echoes it** (at
  reply bytes 9–10), so requests can be correlated to replies by `seq`.
- **Bytes 9–10 of a request must be `00 00`.** Transfer always sends `00 00`
  here regardless of payload, and the device reads the body up to `F7`. A
  non-zero byte 9 (an early attempt sent `0x09`) makes the device parse a
  bogus body and **never reply** — this was the one gotcha. It is not a length.
- `type` (unpacked body byte 4) reuses the shared enum (below); the payload follows it. **[C]** Both are packed with the rest of the body (the
  older text said "raw bytes, no 8-in-7 packing, string args raw"; the receive chain `0x4011efb8` -> `0x40125184` -> `0x40152566` unpacks).
  **[C]** The reply does carry a response bit: the type byte is `type | 0x80` (it was hidden in the collector at wire byte 6; the `0x24` the
  older text calls `cmd` is that collector, the bits of the reply's high bytes).
- Binary reply fields that would exceed `0x7f` are 7-bit encoded (the
  `DeviceUID` comes back as 5 bytes); the exact scheme is not yet decoded. **[O]**

### Command vocabulary (from a Transfer capture)

| type | command | request payload |
|---|---|---|
| `0x01` | Ping | none → reply: capability blob + name `"Digitakt II"` |
| `0x02` | SoftwareVersion | none → reply: strings `"00"`,`"79"`,`"1.16"` |
| `0x03` | DeviceUID | none → reply: 5-byte 7-bit value |
| `0x05` | StorageSpace | 1 byte drive select (`0x01`) |
| `0x09` | Query | a feature-key string, e.g. `sample_file.interleaved_stereo_support` |
| `0x10` | ReadDir | a path, raw NUL-terminated (`"/\0"` for root) |
| `0x53` | DataList | a path + object fields (projects/soundbanks/kits) |

The shared enum (`0x14` path §4) also defines file open/read/write and
`OsUpgrade*`; only the read-only subset above is confirmed *sent* on `0x10`.

### Device-advertised command set (from the Ping capability list) **[V]**

The Ping reply carries the device's own list of supported RPC types (then the
name string `"Digitakt II"`). Decoded, the DT2 1.16 device advertises **44**
commands, and — confirming the dead-`FsRaw` trace — advertises **no** `FsRaw*`:

- **Info/control:** Ping `0x01`, SoftwareVersion `0x02`, DeviceUID `0x03`,
  Screenshot `0x04`, StorageSpace `0x05`, TempoRead `0x06`, TempoWrite `0x07`,
  Query `0x09`.
- **Sample filesystem (read + write, by path):** ReadDir `0x10`,
  CreateDir `0x11`, DeleteDir `0x12`, EnumerateFiles `0x13`, ListRam `0x17`,
  ClearRam `0x18`, Assign `0x19`, DeleteFile `0x20`, RenameFile `0x21`,
  GetFileInfo Path/Hash/PathV2 `0x22/0x23/0x28`, MemoryCompaction `0x29`,
  Open/Read/Close read `0x30/0x32/0x31` (+ ReadV2 `0x36`),
  Open/Write/Close write `0x40/0x42/0x41` (+ WriteV2 `0x46`).
- **Data object API (read + write of projects/sounds/kits):** DataList `0x53`,
  Read{Open,Partial,Close} `0x54/0x55/0x56`,
  Write{Open,Partial,Close} `0x57/0x58/0x59`, Move `0x5a`, Copy `0x5b`,
  Clear `0x5c`, Swap `0x5d`, Rename `0x5e`.
- **OS upgrade (flash channel):** Start `0x50`, Write `0x51`, End `0x52`.
- **Not advertised:** all `FsRaw*` (unsandboxed arbitrary-path FS, `0x14/0x15/
  0x16/0x24-0x27/0x33-0x35/0x37/0x38/0x43-0x45/0x47`), and `DataSetTags` `0x5f`.

**Memory access.** No raw peek/poke-by-address command was found in the handlers
examined. Specifically **[V]** (a second agent byte-checked the handlers):
`DataReadPartial 0x55` and `DataWritePartial 0x58` take `{job_id, seq[,
checksum]}` u32s gated against a session opened earlier **by path string**
(`DataReadOpen 0x54` / `DataWriteOpen 0x57`); the bytes move to/from a process
buffer via a virtual read()/write() on the path-opened resource, never a
wire-supplied address (handlers `0x40126f56`, `0x40126956`; open `0x40125c30`,
`0x40126002`; the leaf `0x401d5e3a`'s memcpy is from a stack-local chunk struct,
not the request). `FsRaw*` is dead (unadvertised, no dynamic_cast site).
`OsUpgradeWrite 0x51` writes attacker-chosen bytes at an offset but into the
flash region via the upgrade handshake, not live RAM.

The **path resolver is also ruled out [V]**: the Data registry (`FUN_4002fc02`,
`0x40966a4c`) holds exactly three handlers — Project/Soundbank/Kit — matching
only `/projects`, `/soundbanks`, `/kits` (and `.metadata`) sub-paths; every leaf
opens a real +Drive file. FsSample opens resolve through a real directory walk
(`FUN_40158626`/`FUN_40159f50`), and `FsSampleReadFileV1`'s "offset" is a
desync check against the server's own position, not a seek. No path maps to a
fixed address or MMIO base.

The **`MemoryStream` / backup route is also ruled out [V]**:
`MemoryStreamReader`/`Writer` (ctors `0x400ee58c`/`0x400ee6b4`) are a raw
`{base, length, cursor}` memcpy wrapper with no bounds/ownership check on
`base` — the exact mechanism a memory primitive would need — and they *are*
MIDI-reachable (the Data RPC's `BackupFileExportAdapter` `0x400e77de` /
`ImportAdapter`, via `DataReadOpen`→`0x400e85aa` and
`DataWritePartial`→`0x400e8896`). But across **all 9** construction sites
(xrefs + `refscan.py`, exhaustive) the `base` is always a firmware-owned struct
field or a fresh heap allocation — never a wire value. The backup path is a
typed router (`RouteResolver` + `Project`/`Soundbank`/`Kit`/`BackupHandler`)
over named content objects, streaming a small `BackupFileHeader` (31/12 bytes)
through the embedded streams; the client picks *which named object* and read vs
write, not an address.

**Two of these gaps are now closed [D]** (single-agent re-reads 2026-09-20, not
yet second-agent byte-checked): (1) the `RouteResolver` registers exactly three
routes — Project/Soundbank/Kit — and all **9** `MemoryStream` construction sites
take a firmware-owned base (an in-object header buffer of constant length 31/12,
or a fresh heap allocation); the "Memory" naming is the internal `MemoryStream`
*class*, not a selectable route, and no wire value reaches a base. (2) The
type-byte factory switch (`0x4013aaf4`) was enumerated in full — no type above
`0x5f`, and no class name suggesting peek/poke/reg/debug/mem — so there is no
hidden "~70 handler" class outside the audited families. **Still open [O]:**
whether an FsSample path can `..`-traverse the +Drive (file disclosure, still
real files, not RAM). Live RAM inspection via the plain protocol is not
available; the UART console (§9) exposes only bounded dumps (a fixed calibration
blob, five test buffers, and eMMC sectors).

**Net across eight audited surfaces** (Data partial R/W, Data/FsSample path
resolvers, `FsRaw`, `DigisharcSysexRpc`, the UART console, `MemoryStream`/backup,
the `RouteResolver` route table, and the full MidiRpc type factory): no
arbitrary, wire-controlled memory read/write is reachable over MidiRpc. If a live report says otherwise it likely means content
(a modified project/sound reloaded live), the `OsUpgradeWrite` flash path, or a
non-MIDI transport — pending the specifics.

### Live, self-driven (verified against hardware)

With Transfer **not** running, `tools/device/devrpc.py` sends
`F0 00 20 3C 10 00 00 00 01 00 00 01 F7` (Ping) and the device replies. Also
confirmed live: SoftwareVersion (`"1.16"`), DeviceUID, StorageSpace (with the
`0x01` arg), and ReadDir `/` (returns the sample-pack directory listing). No
session or handshake is needed — the earlier "only replies when Transfer is
connected" was our malformed byte-9, not a session.

Tools: `tools/device/midisniff.py` (passive capture, transmits nothing),
`tools/device/devrpc.py` (read-only queries + `--readdir PATH`), `tools/device/mmon.py`
(parse a MIDI Monitor `.mmon` capture; `--endpoints` shows direction).
Nothing on the USB path can reach the irreversible bootstrap (DIN-only, §7).

### 0x10 key addresses

```
device 0x10 descriptor       0x4021e08c (sep 0x00, jump table 0x402b40f8)
jump table (all -> handler)  0x402b40f8  -> 0x4011efb8 (128× identical)
receive chain                0x4011efb8 -> 0x40125184 -> 0x401243c6
MidiRpcMessage header parse  0x4013d864 (shared with 0x14 path)
```

## Key addresses

```
router / envelope            0x4011e9f0 ; f0 00 20 3c literal 0x4021e11a
device-ID table              0x402b4098 (dev 0x14 desc 0x4021e0f0)
dev 0x14 command jump table  0x402b4748 (128 × 4 bytes)
generic → RPC container       0x4011e9ca → 0x400e27ec (inner table 0x400e2830)
frame header/body/checksum   0x400e25da / 0x400e26d0 / 0x400e2750 / 0x400e2716
8-in-7 pack / unpack         0x40152242 / 0x40152428 / 0x401524a2 / 0x4015253e
MidiRpc base header          0x4013d864 ; type factory 0x4013aaf4 ; dispatch 0x401243c6
Ping resp / build            0x401ddb9e / 0x401d3ea6
SoftwareVersion / DeviceUID  0x401e06ac / 0x401dce08
FsRaw read/write req         0x401e112a / 0x401e1286 (path reader 0x401dd35e)
FsSample read/write req      0x401e0a8c / 0x401e0be8
OsUpgradeWrite req           0x401e0e22
SDS worker / parser          0x40121eec / 0x400e530a ; field readers 0x401c8828 / 0x401c886c
debug console dispatch       0x400cae8c ; #WRITE name table 0x400cab40
```

## 12. The FsSample file commands on the DT2 1.17 (the +Drive over MidiRpc) **[D]**

Decoded statically on DT2 1.17 (2026-10-22, lane tasks-images, plan `docs/plans/storage.md`, finding 59) and cross-read with earlier write logs. Run once in the emulator (original scenario `samples-live`; current managed acceptance `modules/core/runtime/tests/samples-live.device.test.ts`, client `packages/dk-session/src/rpcfiles.ts` exported as `@digi/dk-session/rpcfiles`): the corrections below are marked **[C]**, the runs **[D]**. Not run on a real device **[O]**; nothing here is **[V]**.
These are the commands the Transfer app uses to put samples into the +Drive (the ekFS path filesystem on the card, `0x5D8000..0x2DF017F`). Framing is section 11: `F0 00 20 3C 10 00 <8-in-7 body> F7`, body
`seq16 00 00 type payload`; the reply type is `type | 0x80`. All integers are big-endian. **In every FsSample reply the status byte 1 means OK.** A path is a C-string of under 256 bytes.

| Command | Type | Request payload | Reply payload |
| --- | --- | --- | --- |
| ReadDir | `0x10` | C-string path | **no status byte**; repeated entries to the end of the message: `u32 hash\|1`, `u32 size-16`, `u8 flag`, `u8 type` (`'D'` or `'F'`), C-string name; `.` and `..` are skipped |
| StorageSpace | `0x05` | `u8 drive` | `u8 status`, `u64 free`, `u64 total` (17 bytes here; 20 was seen once on a real device, settle with a capture **[O]**). **[D]** Drive 1 is the +Drive; drive 0 answers status 0 |
| CreateDir | `0x11` | path (`mkdir -p`) | `u8 status` |
| DeleteDir | `0x12` | path (empty directories only) | `u8 status` |
| DeleteFile | `0x20` | path | `u8 status` |
| RenameFile | `0x21` | old path, new path (two C-strings) | `u8 status` |
| GetFileInfo | `0x22`, `0x28` | path | `0x28`: `u8 found` (1 = a file), `u32 size-16`, `u32 hash\|1` **[D]**; not found: `0` and zeros |
| OpenFileForRead | `0x30` | path | `u8 status`, `u32 id`, `u32 size-16` **[D]** (the id is the record id) |
| CloseFileReader | `0x31` | `u32 handle` | `u32 id`, `u32 size-16`, **no status byte** **[D]**; it ignores the handle |
| ReadFileV1 | `0x32` | `u32 handle`, `u32 offset` (sequential), `u32 len` | `u8 status`, `u32 handle`, `u32 offset`, `u32 len`, `u32 pos`, data. **[O]** In the emulator it answered status 0 and no data for every handle (the open's id, 0 to 8), offset order and length tried, on a factory file and on a file written over MidiRpc; the layout is not settled and the client has no read |
| OpenFileForWrite | `0x40` | `u32` (any value), then the path **with a leading slash** | `u8 status`, `u32 handle` |
| CloseFileWriter | `0x41` | `u32 handle` **and** `u32` data byte count | `u8 status`, `u32 id`, `u32 size-16` (**[D]** the id is the record id, not the hash; the size is the byte count sent) |
| WriteFileV1 | `0x42` | `u32 handle`, `u32 len`, `u32 offset` (sequential), data | `u8 status`, `u32 pos` |

- **"size-16"** is the size of the data without the 16-byte trailer the firmware appends when it closes a file. It also hashes the file (read back from the card) and keeps `hash|1` in the record; `ReadDir` and `GetFileInfoV2` report it. **[D]** The hash is `content_hash(bytes sent ++ 16 zero bytes) | 1`: the trailer of a file written over MidiRpc is zeros, so the host can compute it (`packages/dksample`, `verifyWritten` in `rpcfiles.ts`). A different byte changes it.
- **The close request.** `CloseFileWriter` takes the handle **and** the number of data bytes written. Earlier tools sent only the handle; the length check then fails and the close returns an error status. **[C]** to
  any tool that closes with the handle alone.
- **An idle writer** (no `WriteFileV1` for 20 s) is closed by the firmware's own timer, and a half-written file is what the filesystem then holds. Whether an abandoned writer blocks the next open until the timer runs out is **[O]** (not tried); the client closes a writer it abandons, to be safe.
- **Paths [C] [D].** `OpenFileForWrite` of a bare name (`kick`) answers status 0 and makes a *folder* of that name; `/kick` and `/drums/kick` (the leading slash) work, with the folder made first (`CreateDir`, `mkdir -p`). The earlier "no leading slash, a name with a slash gets status 6" is wrong for 1.17 in the emulator. Non-ASCII names were not tried with a slash **[O]**. Overwriting a file keeps its record id. `RenameFile` moves between folders; `DeleteDir` of a non-empty folder and `DeleteFile` of a missing path answer status 0.
- **Chunks [D].** WriteFileV1 with 2048, 4096 and 16384 data bytes all worked in the emulator (the limit is about 100 KiB, finding 42). A request costs the firmware about 6 ms of model time plus 2.3 us a byte, and the guest idles between requests: 1 MB took 21.6 s at 2 KiB and 11.1 s at 16 KiB (finding 43, "Upload speed in the emulator"). The emulator transports say 16 KiB (`DkTransport.rpcChunk`, `RpcFiles` takes it); a device link keeps `RPC_CHUNK` = 2048. What suits the real USB path is **[O]**.
- **The sample format** is not converted by the firmware: the host sends the native file. A native sample is a 0x40-byte header (byte 1 = stereo flag, `+4` the length, `+8` the rate 48000, `+0x14` `0x7F`),
  big-endian 16-bit PCM, and the 16-byte trailer the close adds. `packages/dksample` is the converter (WAV in, native file out; `plusdrive-format` uses it, and `apps/web/public/dksample.wasm` is it for the browser and the desktop webview). A native file given to the client with its trailer goes without it.
- **The sample browser** on the device lists every file in the +Drive with no filter. That is why the digikit modules are not stored there (`docs/design/digikit-store.md`).
