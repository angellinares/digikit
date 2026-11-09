# 43. Module upload over USB SysEx (device side, host tool, scenarios)

Lane r4, 2026-10-18. Spec: `docs/design/digikit-upload.md`; store format version 2: `docs/design/digikit-store.md`; routes and the receive
path: finding 42. Marks: **[V]** verified here, **[D]** measured or read once, **[O]** open, **[C]** corrects an earlier claim. The hook sites and the step 7 call site
are **[V]** (a second agent checked them against the image bytes, 2026-10-18); nothing ran on a device. What is **[D]** was run in the
emulator by the kept scenarios below. The emulator models USB at message level (it hands bytes to the firmware's cable 0 receive callback,
finding 42 stage 3), so no rate here is a device rate.

## What was built

- **Store version 2** (`packages/emulator/plusdrive/src/store.rs`, `modules/core/runtime/sdk` `store`): two header copies (A at sector 0,
  B at `end - selection_sectors - 1`, `devices/*.toml [card.store] header_b_sector = 0x3dffd`), `seq`, a header CRC and
  `table_first_sector`. Readers take versions 1 and 2. The builder writes version 2 and keeps its files out of header B. Unit tests: A/B
  choice by `seq` (equal: A), a torn copy of either header, a table that was not written, version 1, an empty store (CLEAR).
- **The hook** `dkrt.hook.midi_rx` and the send function `dkrt.fw.midi_send` (anchor rules in `modules/core/runtime/module.toml`, resolved
  on all four ports by `port_resolve resolve-all`). The handler is `modules/core/runtime/src/rs/src/upload.rs` (`dkrt_midi_rx`, trampoline
  `dkrt_midi_hook` in `src/cf/tramp.s`): INFO, LIST, BEGIN, DATA, END, DELETE, CLEAR, ABORT; active in safe mode; every card write goes
  through `card_write` into store sectors only.
- **The host tool** `tools/device/dkupload.py` (`info`, `ls`, `put FILE [PATH]`, `rm`, `clear`; two requests in flight; transports: the
  emulator through `midi.usb.in`/`midi.usb.out`, a real device through python-rtmidi, built and **not** run against hardware).
- **The exporter** writes the store back to `digikit/` (`store_pass` in `export.rs`), three-way.
- **Scenarios** `tests/digikit/scenarios/upload-*.toml`, `menu-upload-pluck-demo.toml`, runner `tools/patchkit/pk_digikit_upload.py`.
- The emulator's `midi.usb.in` got two optional parameters, `chunk` (1 to 384) and `period`, to model a high-speed microframe
  (`docs/CONTROL.md`).

## Hook sites per port **[V]**

All four images have the same 428-byte MIDI task function (found by its bytes; only address literals differ). Its queue-2 branch (USB
cable 0) pops the item, gathers it (`FUN_4011d5e0` on DT2 1.16: the blocks into one buffer; the pointer is NULL when the message was over
128 KiB), and calls the router through the table entry of the status nibble with `(msg, len, source)` pushed. The hook is the two
instructions that make the call, `movea.l (0,A2,D1*4),A0; jsr (A0)` (6 bytes, nothing PC-relative):

| port | MIDI task | hook (`dkrt.hook.midi_rx`) | `dkrt.fw.midi_send` | stock cable-0 callback |
|---|---|---|---|---|
| dt2-1.16 | `0x4011edae` | `0x4011ef02` | `0x401217fc` | `0x4011d76c` |
| dt2-1.17 | `0x4011f08e` | `0x4011f1e2` | `0x40121adc` | `0x4011da4c` |
| dn2-1.11 | `0x40120722` | `0x40120876` | `0x401231a4` | `0x4011f0e0` |
| dn2-1.12 | `0x40120a02` | `0x40120b56` | `0x40123484` | `0x4011f3c0` |

- The rule is `cf_words` over `20721c00 4e90 4fef000c 2f052044 4e90588f` (the call and the task's tail that pops the arguments and frees
  the blocks); it matches exactly once per image. The `hook` op with `displaced = "run"` copies the two instructions to a tail; the
  trampoline returns there (`hook.<anchor>.tail`) when the message is not ours, and to `.return` (after the call, at the `lea 12,sp`)
  when it is, so the stock tail that frees the gathered blocks always runs.
- The gathered message starts with F0 and ends with F7 and the length counts both. The stock router compares the first bytes with
  `F0 00 20 3C`; `F0 7D ..` mismatches at byte 1 and the router returns (its one special case is `F0 7E`), so the prefix is harmless even
  if the hook were missing. A message that is not ours is not touched: the trampoline saves and restores `d0/d1/a0/a1`, the stack is as
  the task left it, and the displaced router call runs.
- `dkrt.fw.midi_send(len, msg, port)` (DT2 1.16 `FUN_401217fc`, hash `2a29225a7abd75aa3be8d59d049af587`, once per image) checks the MIDI
  ready flag, takes the TX mutex, calls the TX function `FUN_40004734` (which takes a complete F0 .. F7 message) and releases. Port 2 is
  USB. The TX function spins when its 4 KiB ring is full, so replies are kept under 1 KiB on the wire.
- **[V]** second-agent check (2026-10-18, raw bytes on all four images): the 6+4+4+2+2+4+2 bytes match once per image at task+0x154; at the
  hook `0(sp)`/`4(sp)`/`8(sp)` hold msg, len and source (4 or 2) and A0/D0 hold msg and len; the trampoline pushes them in C order and the
  code after `.return` reads only D5, D4 and A0, which it loads itself. `dkrt.fw.midi_send` takes (len, msg, port) and has six stock call
  sites of that shape. The hook sees every gathered cable-0 message, not only SysEx: `dkrt_midi_rx` checks `len >= 5` and the four prefix
  bytes before anything else. Not checked statically: that the Rust handler keeps d2-d7/a2-a6 (the C ABI).
- **[D]** The Rust host (`packages/dkupload` `Session`, the code of the desktop panel and the web page) ran against this handler on all four
  ports (`upload-rust` scenario, lane r1): INFO, LIST, put with two DATA in flight, replace, delete, E_NOFILE, E_HASH, CLEAR, and a lost
  INFO, DATA and END reply each; LIST equals the Python tool's and the card readback equals the bytes sent.
- The anchor test `tests/patchkit/test_patchkit_dkrt_boot.py::test_the_upload_hook_sits_on_the_router_call...` checks the bytes at the
  hook and the `lea` after it on all four images.

## Spec changes (marked in the spec)

- INFO's port id is the **device** (`dt2`, `dn2`), not `dt2-1.16`: the runtime has no per-port constant and a host can ask the stock
  SoftwareVersion for the version (the stock route is unchanged).
- The upload buffer is a static object of the runtime (36,896 bytes), not an arena block: its address is then known to the write-watch
  of the proofs, which must leave out the card reads that land in it by DMA, as it does for `DKRT_STAGE`.
- Left open by the first draft and decided: a card with no store range content is an empty store; END closes the upload whatever its
  result; BEGIN, DELETE and CLEAR drop an open upload; at most 256 files for upload commands.

## What the scenarios show **[D]** (emulator, all four ports)

Full tier, run once at the end, 20 (scenario, image) pairs in four processes in 318 s wall; all pass. Seconds per pair on the four
ports (dt2-1.16 / dt2-1.17 / dn2-1.11 / dn2-1.12), alone-in-the-batch times are lower:

| scenario | checks | what it proves | seconds |
|---|---|---|---|
| `upload-ops` | 68 | every command and error code (E_PATH, E_FULL with the free bytes, E_HASH commits nothing, E_OFFSET, E_LENGTH, E_STATE, E_NOFILE, E_PROTO, E_MSG), replace, DELETE, CLEAR, a DATA retry; after each step the card, read by the plusdrive reader (version 2, the right header copy), holds the bytes that were sent and equals the device's LIST; no selection sector written; the +Drive round trip | 15 / 20 / 19 / 13 |
| `upload-cut` | 23 | five cut points, a new daemon on the card each time (see below) | 72 / 63 / 68 / 68 |
| `upload-stock` | 5 | stock MidiRpc replies byte for byte equal with and without the runtime | 13 / 12 / 11 / 11 |
| `upload-playback` | 7 | a note with a 100 kB upload during it (DT2 only) | 57 / 39 / - / - |
| `menu-upload-pluck-demo` | 18 | upload, menu, APPLY & RESTART, rows and PCM equal the patched-in set | 193 / 157 / 86 / 150 |
| `upload-quick` | 16 | one upload, a replace, a delete (the gate; DT2 only) | 14 / 12 / - / - |

Every scenario also checks the write-watch on `cf.expansion` (no stray write over every boot of the run) and the flush check.
The quick tier (roundtrip-dt2, equiv-dyn-pluck on both devices, equiv-dn2-set, upload-quick; three processes) took 48.4 s with
`upload-quick` and 49.2 s without it (a first run without it took 66 s on a busy machine): **the quick tier cost is within the noise**;
the scenario is 12 to 21 s of one process.

- **Cut points** use the test knob `DKRT_UPLOAD_CUT` (the upload path refuses every card write after the Nth), so the card is exactly
  the state a power cut after that write leaves. An upload of a three-chunk file does six writes (three DATA chunks, the table, header B,
  header A). After a cut during staging, after all chunks and after the table the next boot reads the old store; after header B (A
  still old, `seq` lower) it reads the new store from B; after A the new store from both. Every case then took another upload and committed.
- **Stock route** : the Ping, SoftwareVersion and DeviceUID replies are byte for byte the same with the runtime and on the unpatched
  firmware (including the build string `1.17`), and `F0 7D 01 02 03 F7` (not our prefix) went the stock way without a reply.
- **Through the menu** (`menu-upload-pluck-demo`): two modules uploaded to a card with no store appear in SETTINGS > DIGIKIT > MODULES
  (rows counted from the menu's own table), are ticked, APPLY & RESTART loads them, the machine rows equal the patched-in
  `digikit/pluck`'s and the PCM of a note equals its PCM. This is the Phase 7 gate on both devices.
- **During playback** (`upload-playback`, DT2): a 100 kB module is uploaded while a note sounds. The PCM equals the note without the
  upload from the onset on (the capture starts one block later, 64 samples, in both kinds of run the same way `equiv` shows; **[C]** Phase 8: the
  onset shift between the two runs follows where the upload is when the key arrives: 64 samples here, 128 after the header-agreement flag, 256 when BEGIN
  made two extra card reads. The scenario now accepts up to 256 and still requires 0 differing samples from the onset on), and the
  card sectors added by the upload are store sectors only (no selection sector).
- **+Drive round trip**: after the `ops` run, `plusdrive export` writes the store's files to `digikit/`, a second export changes nothing, and
  `plusdrive build` of that folder gives a card whose store lists them. Unit tests cover the three-way rule: a file changed in the folder
  after the card was built is kept and reported (`kept`), the card's version is in `export/<stamp>/`.

## Rate in the emulator **[D]**

The runtime hashes the bytes as DATA brings them, and END does not read the file back. The current figures are in "Upload speed in the emulator" below: DATA about 51 instructions a byte, VERIFY 7
(store version 4, the +Drive content hash; with SHA-256 they were 143 and 99). A 1 MiB file at 132 M instructions per second and a real core's 190 M: delivery of 384 bytes per 16,500 instructions (a high-speed
microframe) and 384 per 1,500 give the same count, so the device's work is the limit, not the delivery. The first implementation (a bytewise SHA-256) took about 570 instructions a byte, 91 % of it the hash.

**This is not a device rate**: the emulator does not model the eMMC program time, the cache misses, the USB packet split (stage 2) or the
other firmware load. A module is 4 to 100 kB, so a complete upload takes well under a second of CPU, and the check cannot be dropped (the END reply is the proof
that the card holds the file).

## Step 7: can the runtime take its own SysEx before the firmware's chain? (static, report only) **[D]**

Verdict: **possible, not needed for modules, worth it only for samples; two to three agent days, medium risk, hardware-only proof.**

- **Need.** 1.4 MB/s of file data is 1.6 MB/s of MIDI bytes (8-in-7) and 2.13 MB/s on the wire (USB-MIDI event packing, 3 of 4 bytes):
  17 Mbit/s, about 4,200 packets of 512 bytes per second. High speed carries that easily; full speed (64-byte packets, 1 ms) does not.
  Whether the port runs at high speed is **[O]** (finding 42 section 1).
- **Stock cost.** Per packet: the deferred-call task `FUN_40004504` (about 95 instructions per 4-byte event, 12.2 k per packet), the
  assembler (about 24 per byte), then gather and unpack (about 30 per byte): about 100 instructions per payload byte, 74 % of a core at
  1.4 MB/s with no other load. That is the 1.0 to 1.4 MB/s ceiling of finding 42.
- **Own path.** An event unpack that keeps 3 payload bytes per event (6 to 8 instructions per event), an 8-in-7 unpack (20 to 25 per 8
  bytes) and a store into a ring: about 2.5 k instructions per packet, **8 to 10 per data byte**, 7 % of the core at 1.4 MB/s. The CPU
  ceiling is then 15 to 19 MB/s; the limits become the host's MIDI driver and the eMMC write rate (both **[O]**).
- **Hook site, tier 1 (cheap).** The cable-0 callback is registered once by `FUN_4000434a(cb0, cb1)`; one call site on all four ports
  (`4eb9 4000 434a` after two `pea`s; DT2 1.16 `0x400cc97e`, 1.17 `0x400ccae2`, DN2 1.11 `0x400cedb2`, 1.12 `0x400cef16`; a `cf_words`
  rule `[0x4eb94000, 0x28444879, *, 0x48794011, *, 0x4000434a]` at offset 18 matches once per image; **[V]** by the second agent: one
  `jsr` and one literal of `0x4000434a` per image, cb0 four bytes before it). A `retarget` to a runtime function
  that keeps the stock cb0 and substitutes its own passes everything that is not `F0 7D 64 6B` to the stock callback unchanged. It runs
  in the deferred-call task after the stock event split: it removes the assembler, the gather and the unpack but not stage 2 (27 % of the
  core at 1.4 MB/s).
- **Hook site, tier 2.** The entry of `FUN_40004504` (`lea -0x60,sp; movem.l ...`, cf_func hash `21029fc0c01b893af505129422dd0647`, two
  whole instructions, unique on all four): handle our packets before the CIN-table copy and the split, then release the slot with
  `FUN_400044ca(buf)` (hash `06ab7507d8209e133af1e892da50b5bc`). Foreign events in a mixed packet need a scratch packet for stock.
- **Risks.** (1) The prefix can span packets: a hold of up to 3 bytes, replayed to stock when it turns out not to be ours. (2) A stock
  SysEx in flight when ours starts leaves a stale partial message in the stock assembler: send it a harmless `F0 7D F7`. (3) The
  deferred-call task is the firmware's only one (128 entries): no card write there; unpack into a ring (64 to 128 kB) and let a runtime
  task or the MIDI-task hook write it with `block_write`. (4) Not in the EP1 OUT ISR: the interrupt priorities were not established.
  (5) The 4 KiB TX ring: acknowledge per chunk, small. (6) No USB model in the emulator below the callback: tier 2 cannot be
  tested there (the emulator injects at the stage-3 callback), so the first run would be on the device **[O]**. (7) Samples need the path
  filesystem's file API, not raw sectors: a different job (finding 42 route A/B).
- **Recommendation.** Not now. A module is 4 to 100 kB; the present path takes it in a fraction of a second. For samples (route B in
  finding 42, a vendor bulk interface, 4 to 20 MB/s) is the better investment than a faster SysEx.

## Upload speed in the emulator, and what limits it (lane perf-upload, 2026-10-22) **[D]**

The desktop workbench took 90 s to upload a 7.7 MiB sample (8.07 MB native file) to the emulated Digitone II's digikit store and 29 s to VERIFY it. Marks
as above; nothing ran on a device. The tools are kept: `tools/devtools/upload_bench.py` (one free-running coupled daemon with the runtime, then
`upload_bench.mjs` for the digikit store on the DN2 or `upload_bench_rpc.mjs` for the stock +Drive write on the DT2). Every number below is a paired run on a
machine that other lanes were using (load average 20 to 30 on 16 cores), so compare guest instructions, real-time factors and paired wall times, not wall
times from different hours. `status` gives the guest `icount`, the instructions skipped as idle, the emulated audio seconds (the model's clock: 132 M
instructions per second) and the controller thread's CPU seconds.

### The ceiling

- **USB model**: `midi.usb.in` hands 384 bytes per 16,500 instructions: 3.0 MB/s on the wire, 2.6 MB/s after 8-in-7. The guest reaches 0.35 MB/s of it, so the model
  is not the limit, and the delivery period is not an artifact worth removing.
- **Guest CPU** (decides everything): the firmware's own load is 79 M of the 132 M instructions of every model second (audio interrupt, UI loop; measured with no
  traffic, `bg_insn_per_audio_s`), so 53 M are free. DATA costs, per file byte (the instructions of a phase less that load), about 51 since store version 4:
  the firmware's SysEx chain, the 8-in-7 unpack and the reply, 44; the card write is 1.5 because the block write is a host call; the file hash 7 (section "The file hash
  of the store", below). That is a ceiling of about **1.0 MB/s in model time**; the 8 MB run at one request in flight reached 0.80 (9.97 model seconds), the rest being the wait between two requests
  (about 10 ms of 41 ms for a 32 KiB chunk); with two in flight it reaches 1.14 (section "Two requests in flight", below), so the 1.0 is about 10 % low. VERIFY (read back and hash) is 7.1 per byte: 7.4 MB/s in model time. Before version 4 the file hash was SHA-256 at 99 per byte
  (ColdFire has no rotate: 6,300 instructions a block): DATA 143, 0.32 MB/s, VERIFY 99, 0.53 MB/s.
- **The real device** has 190 M instructions per second and 37 to 47 % firmware load (finding 42), so 101 to 120 M are free: estimates (the eMMC program time and the
  USB path were not measured), for the digikit store about 2.0 to 2.3 MB/s for the guest's work alone and 1.2 to 1.4 MB/s with the 10 ms a chunk of wait that the model shows
  (1.4 to 1.6 if that wait scales with the clock); VERIFY 14 to 17 MB/s; upload and VERIFY together 1.7 to 2.0 MB/s. Before version 4: 0.7 to 0.8 MB/s (0.57 to 0.65 with the wait),
  VERIFY 1.0 to 1.2, together 0.42 to 0.49. The stock +Drive write takes 2.76 model seconds a MB at 16 KiB requests (5.4 at 2 KiB): 0.36 MB/s of model time, 0.52 MB/s at the real clock, and the 0.5 MB/s that
  finding 42 measured on a real DT2 is the same. USB high-speed bulk (35 MB/s) is not the limit; if the port ran at full speed it would be (about 0.8 MB/s of payload
  after the 3/4 event packing and 7/8).

### Two requests in flight (lane upload-depth, 2026-11-07) **[D]**

`DkSession.put` (TypeScript: the desktop app, the web page, the tools and the device tests) keeps `depth` DATA requests in flight. The depth comes from the transport (`dkupload-core.ts`
`DkTransport.dataDepth`, default 1; `EmuUsbTransport` sets `EMU_DATA_DEPTH = 2`, the one place to change it; `DkSessionOptions.depth` overrides). A transport to a device leaves it out: depth 2 is
**not enabled on hardware**. The Rust `packages/dkupload` `Session` already kept two in flight (`put.rs`) and was not changed. DT2 samples use the stock MidiRpc and were not touched.

**Does depth 2 stall the main loop? No [D].** DN2 1.12, 8,000,000 bytes, 32 KiB chunks, hash verified, a fresh daemon for each run, `stall.mjs` and `stall2.mjs` in `out/native/runs/20261107-depth/`
(a `status` poll every 100 ms for the heartbeat): depth 1, 319 heartbeat passes in 10.0 model seconds (31.9 a second), no `stall` event; depth 2 (two runs), 205 and 198 passes in 6.74 and 6.71
seconds (30.4 and 29.5 a second), no event, the longest gap between two polls that saw a pass 0.17 s (depth 1: 0.06; the poll is 0.1 s). The `stall_count` that rose 0, 1, 3 in the first
measurement (`20261107-bugs/upload-speed/depth.log`) was misread: that script took the count after each upload and VERIFY (of the whole 8 MB file in one request) and DELETE ran in between. In
one daemon (put at depth 1, VERIFY, DELETE, put at depth 2, ... twice) every `stall` event (1,001 ms) followed a whole-file VERIFY, never a put. A VERIFY of a file this large in one request holds
the MIDI task for 1.07 s of model time (7.4 MB/s) whatever the depth; hosts verify in ranges of 1 MiB (0.14 s each), as `verifyFile` and the bench do. **[O]** a depth of 3 was not
measured again (the first measurement's count for it is not attributable).

**Why it does not stall (static + the run).** The handler runs in the MIDI task (`dkrt_midi_rx` from the hook; `cmd_data` calls `store::write_sectors` inline: a second request cannot be handled
while the first is written, it can only *arrive*). Arriving needs no MIDI task: the USB ISR and the deferred-call task assemble the SysEx into 151-byte blocks of the 160 KiB pool (1,024 blocks,
finding 42 section 3) and queue the message at F7. A 32 KiB chunk is 37.5 kB on the wire, about 250 blocks, so two requests hold about 500 blocks (half the pool); the host never sends a third before the
first reply, so the pool cannot run empty (the assembler task would wait on its semaphore) and the 128 KiB gather limit applies to one message. The MIDI task sleeps on its queue between messages
and the firmware's other tasks run when it pends; the UI loop passes at the same 30 Hz in both depths. The DATA reply is one 16-byte transfer, so the TX ring (4 KiB) and its pool of 32 never see more than
two of ours: `tx_room` drops a reply only when the host does not read (finding 43, "No unbounded spin on a reply"); a dropped reply is a lost reply, which the host handles (below). The runtime's DATA rule
(next offset, or the exact repeat of the last chunk) is what makes it safe: a request that arrives after a failed one gets `E_OFFSET`, and nothing out of order is written.

**Decision: safe on the emulator, not provable for hardware.** What the emulator does not show: the real USB MIDI driver of the host may hand both messages to EP1 OUT at once (the emulator's delivery,
384 bytes per microframe, is gentler); whether the eMMC program time of the real block write pends the MIDI task (yielding) or spins; whether the real DPC task shares work with the assembler. The memory
argument (half the pool) and the in-order rule hold on any link, so a protocol failure is not expected, but nothing ran on a device. Hence the flag: depth 2 on `EmuUsbTransport`, depth 1 on the
real MIDI transports until a device run (set `dataDepth = 2` on that transport, or `depth` in the session options, to try it; the Rust host's two in flight is the same experiment). No runtime change.

**Host rules.** DATA does not take the one-at-a-time turn per request: `put` sends its chunks in windows of 8 (`WINDOW_CHUNKS`) and holds the turn for one window, so another request (a Log poll, the
`emulatorSession` reader) waits for 8 chunks at most (about 0.3 s of model time, 0.5 s wall) where it waited for one chunk before; the window drains between turns. Replies are matched by seq; a stale
reply (an earlier retry) is ignored as before. An ok reply to a later request proves the earlier ones were done (the device takes a chunk only at its next offset), so a lost reply with a request behind
it needs no resend. A lost reply with nothing behind it, or both lost: a timeout, then the unacknowledged chunks are sent again one at a time from the first, with the normal retries; the runtime
accepts the next chunk or the exact repeat of the last, so an `E_OFFSET` for a chunk that has a later one behind it means it was done. An error reply (`E_OFFSET`, `E_WRITE`, ...) stops the sending,
the replies still in flight are awaited for 2 s, the first error is thrown and `put` sends ABORT as before. `onProgress` reports acknowledged bytes and never goes back. `cancelPending` ends the put at
once (both requests are left unanswered), ABORT follows on the new signal and the session is usable. A failed or lost END is handled as before.

**Tests.** Host: `packages/dk-session/tests/put-depth.test.ts` (a fake device with the runtime's DATA rule: order and depth, the depth from the transport, sizes 0 to 16 chunks, a lost reply with and
without a request behind it, both lost, `E_WRITE` and `E_OFFSET` mid-window, cancel mid-window, another request between windows). Device, DT2 1.17 and DN2 1.12:
`modules/core/runtime/tests/upload-depth.device.test.ts` (sizes and VERIFY, the card read by plusdrive, a flipped bit gives `E_HASH` and commits nothing, a lost reply in both forms); the other upload device tests
now run at depth 2 too, as the harness uses `EmuUsbTransport`.

**Measured** (DN2 1.12, the digikit store, `upload_bench` and the app's path `bench.mjs`, paired on fresh daemons of one build, order 1 2 2 1, load average 3.7 to 4.9 on 16 cores, `--no-lock`: `measure.log`):

| | depth 1 | depth 2 |
|---|---|---|
| 8,000,000 bytes: wall s, MB/s | 18.20 and 17.89, 0.44 and 0.45 | 12.61 and 12.54, 0.63 and 0.64 |
| model seconds, MB/s of model time | 9.98 and 10.09, 0.80 and 0.79 | 6.97 and 7.01, 1.15 and 1.14 |
| real-time factor | 0.55, 0.56 | 0.55, 0.56 |
| `stall_count` (put, then VERIFY in 1 MiB ranges) | 0, 0 | 0, 0 |
| VERIFY wall s | 1.9, 2.0 | 1.9, 1.9 |
| 50 files of 500,042 bytes (18.1 MB stored) through `uploadItems`: wall s | 51.5, 49.3 | 37.4, 37.8 |
| the same: model s, rtf, END (50 files) | 27.7 and 28.4, 0.54 and 0.57, 2.5 and 2.3 s | 21.3 and 21.2, 0.57 and 0.56, 2.3 and 2.4 s |
| the same: failed, VERIFY not ok | 0, 0 | 0, 0 |

Depth 2 is 43 % faster in model time and 29 % in wall time for the large file; 26 % in wall time for 50 files (VERIFY, END, BEGIN and LIST are unchanged, and DATA is a smaller share of that run). The raw-frame run had
1.22 at depth 2: the difference, about 4 %, is the drain between windows (31 of them, one wait each); a window of 16 would halve it and make another request wait longer. END (the time of a commit) and the
VERIFY figures did not change.

**Estimate for the device (not measured; the method of "The ceiling").** At depth 2 the wait of 10 ms a chunk is gone, so the rate is the guest's work alone: 2.0 to 2.3 MB/s for the digikit store
(before: 1.2 to 1.4, with the wait), less the eMMC program time and the USB path that nothing here measured. A 50 MB sample would take about 25 s rather than 40 s; a module of 100 kB is the same
(one chunk of three, END dominates).

### Left **[O]**

- The cap itself: the DSP worker at 1.7 CPU seconds per audio second (DN2) while nothing plays; a faster DSP tier, or a mode that does not render audio during a transfer,
  would be the next lever for wall time.
- Guest work: the firmware's own SysEx chain (event task, assembler, gather: about 40 to 60 instructions a byte) needs the tier-1 hook of step 7 above (hardware-only
  proof). With the file hash at 7 it is now most of the 51 instructions a byte. The wait between chunks is hidden by two requests in flight (done, section "Two requests in flight").
- A host-side shortcut (writing the store through the card model, no guest) would be instant in the workbench but tests nothing of the protocol; not done.
- On a device nothing was measured: the real USB MIDI path, a larger RPC chunk on hardware, and the eMMC program time.

### The file hash of the store (store version 4, lane fast-hash, 2026-11-07) **[D]**

The table's 32-byte slot, BEGIN, END, LIST and VERIFY carry the +Drive content hash `hash | 1` (`dksample::content_hash`, a lookup3 variant over big-endian words) as 4 big-endian bytes and
28 zeros, not a SHA-256 (docs/design/digikit-store.md; the runtime's `hash32.rs`, `dksample::file_hash`, `fileHash` in dk-session, `file_hash` in `tools/device/dkupload.py`; capability
FILEHASH, bit 12). `.dkm` files keep their own header SHA-256 (the loader checks it). Paired runs on this host (DN2 1.12, digikit store, one file of 8,000,000 bytes of generated data, 32 KiB chunks, load average 4 to 6 on 16 cores;
`out/native/runs/20261107-hash/final-base.jsonl` and `final-new.jsonl`; the same session, daemon and bench, only the runtime and the host hash differ):

| | SHA-256 (origin/main) | file hash (version 4) |
|---|---|---|
| DATA, work instructions per file byte (as the bench prints) | 142.9 | 43.8 (about 51: see below) |
| DATA, model seconds, MB/s in model time | 24.63, 0.325 | 9.97, 0.802 |
| DATA, wall seconds, real-time factor | 43.7, 0.563 | 18.4, 0.542 |
| VERIFY, work instructions per file byte | 99.0 | 7.1 |
| VERIFY, model seconds, MB/s in model time | 15.06, 0.531 | 1.09, 7.37 |
| VERIFY, wall seconds, real-time factor | 26.6, 0.566 | 1.97, 0.551 |
| `stall_count` after DATA, after VERIFY (cumulative) | 0, 8 | 0, 0 |
| `bg_insn_per_audio_s` | 79.41 M | 79.35 M |

- The hash costs about 7 ColdFire instructions a byte: 7.1 measured in VERIFY (a saturated phase; it includes the card-read loop), and about 6.7 counted in the object's inner loop (80 instructions for each 12-byte block).
  The printed 43.8 for DATA is too low: the guest now waits for the host between chunks, and the idle load is subtracted for the whole phase. DATA is the 44 of the SysEx chain plus 7, about 51.
- The code is `Hash32::update`, 796 bytes (1,430 bytes with `finish`, `slot` and the rest); the SHA-256 block function (7,112 bytes) stays in the runtime for the `.dkm` header check. Whether this fits the instruction cache was not measured.
- Correctness: unit vectors against `dksample` for lengths 0 to 64, 511, 512, 513, 4096, 32775 and random splits; 13 file sizes uploaded and checked whole and in 4,096-byte ranges on the device against the host's hash;
  the hash of the TypeScript, Python and Rust hosts agrees (the web mock, `dkupload` and the device tests). A flipped byte on the card is reported by VERIFY (another hash) and by the load (`E_SHA`) on both devices; an END with a wrong hash is `E_HASH`.
- A store of another version counts as no store (the old table has SHA-256 slots): readers see none, the card stays ours, the first upload writes version 4 over it. The flash scenario with a real version 3 store, both devices: the boot goes on, LIST is empty,
  the selection record (`modules/sine.dkm`) and the settings record stay, the first upload makes version 4 and a restart loads the module from the kept selection. The old header A and B, the old table sector and the old file sectors are all
  overwritten by the first upload (the new file takes the first gap, the new table follows; the old header B is rewritten as a version 4 header). `patchkit store dump` and `plusdrive store ls` refuse a version 3 store ("unknown version").
- The DT2 samples go through the stock MidiRpc into the +Drive and were not changed: one run (1 MB, 2 KiB requests, 857 guest instructions a byte, stored and hash-checked) still works.

## Open **[O]**

- Nothing ran on a device (depth 2 included: the real MIDI transports stay at one request in flight, "Two requests in flight"). The python-rtmidi transport is untested against hardware. Whether the real MIDI task handles a 37 kB SysEx
  without the host's pacing (the 160 KiB block pool and the DPC queue) is a hardware question; the emulator's delivery is gentler than a
  host driver.
- **[C]** The three items that stood here (a reply into a full TX ring, the menu's reader against a commit, the CLEAR double fault) are
  closed in the last section ("Phase 8 safety"). What stays open from them: the TX analysis is static and the emulator does not model the
  USB IN endpoint, so a host that really never reads has not been run **[O]**; nothing ran on a device.
- A store is limited to 256 files for upload commands.
- The exporter treats a store file named like a deleted folder file by the three-way rule only; a card built from an older folder than
  the one the export meets is reported as `kept`.

## Phase 8 safety: a reply into a full TX ring, the menu's reader against a commit, the CLEAR double fault (lane r4, 2026-10-18)

Marks as in the header. Kept scenarios: `upload-txfull`, `upload-clear-cut` (new), the sdk host tests (`cargo test`, see item 3), beside the
unchanged `upload-*`. All emulator; nothing ran on a device.

### 5. No unbounded spin on a reply **[D]** (static analysis + scenario)

The TX function `FUN_40004734` (same hash, same entry address `0x40004734` on all four images) keeps three RAM words: the *ready* flag, the
bytes *in flight* `f` (the USB stack was handed them and has not sent them: `FUN_400045d6` adds the length, the completion callback
`0x400041a8` subtracts it; DT2 `0x409659dc`, DN2 1.11 `0x4058e224`, 1.12 `0x4058f224`) and the ring's write offset. It returns at once when
ready is 0 or the USB register is 0, and otherwise **spins** in two places: once at a wrap (`len > 0x2f`, write offset `> 0xfbf`: until `f < w -
0x3f`) and per 4-byte event until `queued_in_this_call + f <= 0xffb`. A second blocking point is behind it: `FUN_400045d6` takes a transfer
buffer from a pool of 32 (`FUN_40005560` links 64-byte entries over 0x800 bytes; the allocator was not traced, so one may be reserved **[D]**), and with none free the caller pends. A host that never reads leaves `f` where it is, so every
reply adds to it until the first loop (4 KiB) or the pool stops the MIDI task: the whole device's MIDI and, through the same task, the router.

**Rule** (`upload.rs` `tx_room`): before `midi_send`, read `f` through the anchor `dkrt.fw.tx_inflight_lit` (the 4-byte literal of the
function's `movea.l (<f>).l,a0`, `cf_func` hash `0d9c170d...` offset `0x42`, same on the four ports; the bytes `2079` before it are checked by
`port_resolve`'s hash and by me on all four images). The reply is sent only when `f <= 256` and `f + 4 * (ceil(wire / 3) + 1) <= 0xff0` (the
wire bytes, three per event); otherwise it is **dropped** and `DKRT_UPLOAD_DROPPED` counts it. The host's retry (it already has one: lost
replies are tested in `upload-rust`) asks again. The 256 bytes are the pool rule: our smallest reply is a 16-byte transfer, so at most 16 of
ours wait for a host that reads nothing, and the pool of 32 is not used up by us; stock traffic (4 bytes a transfer) has the rest. With the
anchor not pointing at a DDR word no reply is sent. No wait, no loop, no lock. **[V]** (second agent, 2026-10-18, all four images): the two
spin conditions and that a guarded send cannot reach either, **assuming no stock sender raises `f` between the check and the call**: the margin to
`0xffb` is 11 bytes, and another task sending three or more events in that window could still make a non-reading host hang the MIDI task
**[O]** (closing it means checking `f` under the firmware's TX mutex). The literal of `f` is only 2-aligned; the ColdFire splits the read, so it
works, but a `read_unaligned` would be cleaner.

Test `upload-txfull` (cf-only rig; the emulator captures the arguments of the TX function and does not model its ring, so the test sets `f`
by poking it): `f` = 0xf00 and `f` = 0x200: 20 INFO requests each, no byte reaches `midi.usb.out`, `DKRT_UPLOAD_DROPPED` rises by 40 and
the sent-reply count does not; a stock MidiRpc Ping with `f` still 0xf00 is answered (the MIDI task and its router are alive); `f` = 0x100 and
0: the reply is sent. **Control:** with `tx_room` forced true the same scenario fails on exactly those checks (29 bytes reach the output per
request, nothing counted dropped). Not run: a real full ring (the spin itself) **[O]**.

### 3. The menu's reader against a commit **[D]** (host unit test + static argument; no scenario forces the interleave)

An upload commit writes the new table (fresh sectors), header B, then header A. A reader in the UI task reads header A, header B, then the table.
With fresh sectors the table of either header stays intact, so a plain `open` is already safe for an ordinary commit: it can only pick the old
or the new store. The torn read is the **reuse** case: after CLEAR the next upload stages over sector 1, where the old table was; a reader
that read the old header before the CLEAR finished then reads garbage for its table, its table CRC fails and the menu shows a store error until
it is refreshed.

**Rule** (`dkrt_sdk::store::open_stable`, used by `scan_store` in the menu): read a fingerprint of the two header sectors (header CRC and
table CRC words), `open`, read the fingerprint again; when it changed a commit ran in between: try again (at most 4 retries, then return the
last answer). `scan_store` repeats the same around its whole scan (it also reads file header words for the compatibility mark), at most 3
times. No lock and no flag: neither task ever waits for the other, so there is nothing to deadlock; the cost is two more sector reads per scan.
The upload side needs nothing: it writes in the order that keeps the old store valid until header B.

Proof: the host tests of `modules/core/runtime/sdk` (`cargo test` outside the workspace; the crate has `[workspace]` absent, run it from a copy
or with `--manifest-path` and an empty `[workspace]`): a mocked card whose reads apply the steps of a commit between two reads, at every read
position and with the steps one read apart, two reads apart or all at once. (a) An upload commit: `open_stable` always returns the old or the new
store. (b) A CLEAR followed by staging over the old table: `open_stable` always returns the old or the new store, and the plain `open` fails in
some of the same interleavings (the test asserts that, so it cannot pass by not exercising the race). The emulator cannot force the interleave
(the two tasks run on one scheduler and the card read does not yield at chosen points).

### 4. The CLEAR double fault **[D]** (scenario + control)

The hazard was exactly as described in the Open list: CLEAR cut between header B and header A leaves B = empty store (higher `seq`), A = the old
store, pointing at a table in sector 1 or later; the next upload starts at `used = 1` and stages over that table; if its header B write tears, A
(old, stale) is the only copy that parses and its table CRC fails: both copies unusable, the store reads as damaged (`E_NOSTORE`), though the
last committed store (empty) was fine.

**Rule** (`upload.rs` `sync_headers`, called by BEGIN after it has read the store and before it reserves anything): both header copies must
say the same thing before any sector is staged. When the copy that lost the `seq` comparison is missing, torn, version 1 or different in any
field, BEGIN rewrites it from the winner (version 2, same `seq`) first; if that write fails, BEGIN answers `E_WRITE` and stages nothing.
Reuse (staging over a table that a header still names) now starts only when both headers name no table that staging touches: after CLEAR both
say "empty", after a cut between B and A the lagging one has been brought up. `store::open` has read both headers for its own choice and now says whether they agree (`Table::agree`), so a healthy store costs BEGIN nothing; a repair is one more read and one write. Without
reuse (everything but CLEAR) a stale header is harmless, as before: nothing it points at is overwritten.

Test `upload-clear-cut` (cf-only; new daemon on the overlay of the last, as `upload-cut`; `DKRT_UPLOAD_TEAR` is a new test knob: the Nth
card write keeps only its first 256 bytes and every later write is refused):
- *clear*: CLEAR cut after write 1 (B): refused its second write; the next boot reads B, the empty store. Next upload (write 1 = the repair of
  A, 2 = data, 3 = table, **4 = header B, torn**): the reader then finds a valid empty store (plusdrive: `version=2 seq=2 header=A`), LIST is
  empty, and a further upload commits.
- *upload*: an upload cut after header B (new store in B, old in A), then an upload whose B tears: the first upload's file is still there (A was
  made equal to B by the second BEGIN), plusdrive reads it, a further upload commits.

**Control:** with `sync_headers` forced to return at once the two cases fail (the torn write then lands on a different write, the store
holds a different set); the scenario cannot pass without the rule.

Also changed: `INFO`'s `free_bytes` and the new HEALTH rows use one formula, `dkrt_sdk::store::free_bytes(used, count)`.

### 2. `patchkit pack` refuses a ColdFire object that has not passed the ISA check **[D]**

Mechanism (the simplest sound one): the build records its own passing check. `pk_build.cf_object` runs `pk_link.check_isa` (our decoder over every
instruction, ISA_A/B/C + EMAC only; it raises otherwise) and writes `toolchain.isa_check` into the `.dko.json`: SHA-256 over the object's
sections (name, kind, alignment, size, bytes), symbols and relocations in a fixed framing (`pk_build.isa_digest`; the same framing is
`Dko::isa_digest` in `packages/patchkit/src/dko.rs`). `dkm::pack_full`, the one function every `pack` goes through, calls
`Dko::require_isa_checked`: a ColdFire object with no record, or with a record that does not match the bytes it holds, is refused with the
reason (`... has no ISA check record: build it with mise run patch:build` / `... does not match its bytes (edited after the build?)`). A data-only module (no
ColdFire object) and SHARC objects are not affected. It is a guard against accident (a hand-made object, a changed byte, an object from before
the record), not a signature: whoever can run the build can write the record, and the checker's policy stays in one place (`tools/coldfire/isacheck.py`;
a second copy of it in Rust was the alternative and would be a fourth place to keep equal).

All 24 committed ColdFire objects were rebuilt (`mise run patch:build`; the diff of each is the one added key) and `patch:build --check`
is current. Tests: `cargo test -p patchkit --test isa_record` (every committed ColdFire object's record equals the digest computed in Rust,
which proves the two implementations of the framing agree; a changed code byte, relocation, symbol value or alignment is refused; a record
copied from another object does not help; an object with no record is refused), `dkm::tests::an_object_that_has_not_passed_the_isa_check_is_refused`,
and `tools/patchkit/pk_dkrt.py` (the deliberately bad-import object is re-stamped for its new relocation, as a build would).

### Tests run **[D]**

- Four ports (DT2 1.16, 1.17, DN2 1.11, 1.12), full tier, `--jobs 4` on a machine other lanes were also using: `safe-late-fault`, `upload-clear-cut`,
  `upload-txfull`, `safe-fault`, `upload-cut`, `upload-ops`, `upload-stock`, `upload-playback` (DT2), `menu-upload-pluck-demo`, `equiv-dyn-pluck`,
  `equiv-dn2-set`, `cut-selection`: **44 (scenario, image) pairs, all pass, 897 s wall**. Seconds per pair (loaded machine): `upload-clear-cut`
  37 to 45, `upload-txfull` 11 to 14, `upload-cut` 57 to 62, `upload-ops` 13 to 15, `upload-stock` 11 to 13, `upload-playback` 39 and 60,
  `safe-late-fault` 124 to 328 (87 s alone), `safe-fault` 47 to 69, `menu-upload-pluck-demo` 103 to 237, `equiv-dyn-pluck` 56 to 94.
- After the last runtime change (the store figures read once instead of on every HEALTH refresh): `upload-ops`, `upload-clear-cut`, `upload-txfull`,
  `upload-cut` again on all four ports, 16 of 16 pass; the HEALTH proof (`tools.coldfire.digikit_health_proof run --quick --no-cost`, DT2 1.17 and
  DN2 1.12) 29 of 29.
- `cargo test` of the sdk (12 tests, run by `tests/patchkit/test_patchkit_dkrt_boot.py`), `cargo test -p patchkit` (35 unit tests, the integration
  tests, `isa_record` 2), pytest of `tests/patchkit/test_patchkit_dkrt_boot.py`, `test_digikit_menu.py`, `test_health.py`, `test_patchkit_dkrt.py`,
  `tests/devtools/test_lint.py`, `test_types.py`: 48 pass, 2 skip; ruff and `pathcheck` (no new stale reference).
- **[C]** `upload-playback` accepted an onset shift of at most 128 samples between the note alone and the note during an upload; it was 64 before
  this work. BEGIN with two extra card reads made it 256, a phase effect (see "During playback" above), so the first version of `sync_headers` was
  changed to use what `store::open` has read (no extra reads in a healthy store: shift 128) and the scenario now accepts 256 with 0 differing samples.
