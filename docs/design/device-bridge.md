# Device bridge and flashing

> **Deferred (Em, 2026-10-19).** Phase 10 now starts with the module loop (`dk run`), the firmware's own Screenshot command (MidiRpc 0x04,
> docs/MIDI-SYSEX-RPC.md), verify speed and DSP debugging. Known since this draft: Transfer sends a 1.9 MB image in about 20 s and the device asks
> YES after the data. Kept for later.

Status: design, 2026-10-19 (plan `docs/plans/digikit-runtime.md` Phase 10 item 1). Nothing in sections 1, 2 and 4 to 8 is built; section 3 (flashing) was replaced by `patchkit flash` on 2026-10-28. Marks: **[V]** read in the
code or a finding for this design, **[D]** measured once elsewhere, **[O]** open. No device command was sent and no emulator was run to
write this file (section 3 was rewritten after `patchkit flash` was built and run in the emulator).

## 1. Goals, non-goals, guardrails

Goal: tools, scenarios and agents address "a device" (the emulator, or Em's Digitakt II on OS 1.17) through one interface, chosen by an
address string. The set of facts about the device (peek ranges, store, panel codes) comes from `devices/*.toml`
(`docs/design/device-data.md`), not from the bridge's code.

Non-goals: no card or flash write except the OS update through the vendor path (`patchkit flash`); no sample transfer (Phase "Later");
no DSP peek, tracepoints or PC sampler (Phase 10 items 3 and 4); no GDB bridge (item 6); no remote access (local socket only); no
change to the stock MidiRpc route.

| Guardrail | How it holds |
|---|---|
| Read-only by default | The device backend offers INFO, LIST, LOG, PEEK, SCREEN and raw notes. Every other method is refused with `E_READONLY` before a byte is sent. |
| Writes need DEBUG and the remote module | KEY (and a later POKE) are accepted only when the debug log is ON **and** `digikit/remote` is loaded (checked in SETTINGS > DIGIKIT > MODULES, applied at the next restart). The per-boot REMOTE CONTROL switch was removed in phase 4 of docs/plans/sdk-api.md: a host that can upload and select modules could already load any module that calls `input_key`, so the switch was not a boundary against the host. |
| No card or flash writes except the OS update | The bridge never calls BEGIN, DATA, END, DELETE or CLEAR unless the client asked for `store.*`; `store.*` is the upload protocol, which writes only store sectors and is already guarded by the runtime ("Card safety", `docs/design/digikit-upload.md:100`). The OS update goes only through `patchkit flash`. |
| Dialogs stay human | The OS-update screen asks no YES (finding 75), so `patchkit flash` has nothing to press. The runtime cannot tell which prompt is on screen, so KEY refuses YES (code 10) and NO (12) always; Em presses them. [O] whether this is too strict for tests (open question 3). |
| Rate limits | Bridge side: one request in flight per command class; SCREEN at most 10 per second, KEY at most 20 per second, LOG poll 1 per second by default. Runtime side: a mailbox of 8 key slots, a full mailbox answers `E_BUSY`; every reply keeps the TX-ring guard (`digikit-upload.md:15`). |
| Nothing in interrupt context allocates | The runtime handles SysEx in the firmware's MIDI task, not in an interrupt (`digikit-upload.md` "Where the device handles it"). KEY only writes a mailbox slot there; the key is posted to the UI queue from the UI loop hook (`modules/core/runtime/src/rs/src/lib.rs:776`, task context), as the module mailbox is polled today. SCREEN copies 512 bytes into the static reply buffer. No heap use. |

## 2. What exists today

| Piece | Where | Facts |
|---|---|---|
| Emulator control server | `packages/emulator/control/src/server.rs:109` (`serve`), discovery file `current.json` `server.rs:81`, run dir `out/native/run` | JSON-RPC 2.0, one object per line, Unix socket; served by the desktop app and `digi-emud` (`docs/CONTROL.md:1-20`). **[V]** |
| Method list | dispatch `packages/emulator/control/src/controller.rs:1365-1381`; table `docs/CONTROL.md:49-90` | `status`, `step`, `input.button/turn/tap`, `framebuffer.get` (`controller.rs:1901`: 1024 bytes, 128x64, `oled-1bpp-column-major`), `mem.cf.peek/poke`, `midi.in/out` (DIN), `midi.usb.in/out`, `leds.get`, `events.subscribe`, `firmware.load`, `restart`. **[V]** |
| Emulator client | `tools/control/digi_client.py` (`mise run emu:ctl`, `mise.toml:36`) | `Emu` wrapper: `usb_in`, `usb_out`, `frame`, `peek32`, `tap`, ... **[V]** |
| USB MIDI in the emulator | `controller.rs:1677` (`midi.usb.in` docs), `docs/CONTROL.md` paragraph "midi.in" | Bytes go to the firmware's USB receive callback in 64-byte chunks one USB frame apart; `midi.usb.out` returns what the firmware passed to its USB transmit function. USB itself is not modelled. **[V]** |
| Upload protocol | `docs/design/digikit-upload.md:59-68` | INFO, LIST, BEGIN, DATA, END, DELETE, CLEAR, ABORT, LOG 0x20, PEEK 0x21. Frame `F0 7D 64 6B 01 cmd seq body F7`; reply `cmd|0x40`. **[V]** |
| Host tools | `tools/device/dkupload.py` (`EmuTransport:109`, `RtMidiTransport:146`, `Uploader:184`, `info:228`, `log_page:271`, `peek:284`); `tools/device/dklog.py` (`Follower:302`, `follow:345`, `peek_range:519`) | A transport is any object with `send(bytes)` and `recv(timeout) -> bytes`. The emulator transport already runs the same `Uploader` over `midi.usb.in/out`. **[V]** |
| Runtime side | `modules/core/runtime/src/rs/src/upload.rs:468-492` (`cmd_info`, 25-byte body), `dlog.rs:34-38` (`debug_on`, `set_debug`), `lib.rs:776` (UI loop hook) | `RUNTIME_VERSION = 3` (`modules/core/runtime/sdk/src/lib.rs:13`). **[V]** |
| Device data | `devices/digitakt-ii.toml:104` `peek = [[0x40000000, 0x47e30000], [0x47e8b000, 0x47fe1000]]`; `:91` `safe_key = 19`; `[panel]` `:121` | Peek ranges (at most four) become `dkrt_peek_ranges` in the runtime (`docs/design/device-data.md`). **[V]** |
| Preflight | `mise.toml:255`, `tools/patchkit/patchkit.py:306-313`, Rust `packages/patchkit/src/main.rs:360-363` | Rust checks (known stock base, sections 2/4/5/8 identical, checksum, HMAC, limits) print `PREFLIGHT OK: <path>` or `PREFLIGHT FAILED`; the ISA scan of section 7 (`dspscan.rs`) is one of its lines; exit 1 on any failure. **[V]** |

On the device today (runtime 3, finding 47): INFO, LIST, LOG, PEEK work over USB MIDI through `RtMidiTransport` **[D]**
(`docs/findings/47-dt2-device-rungs.md:3-4,10-18`). So the read-only half of the bridge needs no flash.

## 3. Flashing: `patchkit flash`

Built on 2026-10-28 (plan `docs/plans/sdk-api.md`, step 6d). It replaces the Python `dkflash` of the earlier draft: one Rust core,
`packages/patchkit/src/flash.rs`, with no I/O, for the command line, the desktop app and the browser. The device side is
[finding 75](../findings/75-os-upgrade-receive.md): OS UPGRADE takes the plain `.syx` stream, sends no reply, validates after the last
message (content checksum, on DT2 the build floor "0060", HMAC) and erases nothing when the validation fails. It asks no YES.

### 3.1 Steps

`patchkit flash STOCK.syx --port NAME [--restore] [--set device] [--dry-run]` (`--emulator SOCKET` sends through the control daemon's
`midi.usb.in` instead).

| # | Step | Refuses when |
|---|---|---|
| 1 | `prepare`: the stock file is identified by SHA-256 against the ports; patch with the device file's patch set; preflight (sections 2, 4, 5, 8 identical to stock); verify (packet checksums, framing, content checksum, HMAC, limits, record); product byte; build floor; the image id; the message list run through a model of the device's receiver. `--restore` takes the stock file through the same checks without a patch. | A patched or unknown file, or any failed check: the checks are printed, nothing is sent. `--dry-run` stops here. |
| 2 | Print the instructions (power adapter, cable stays in, SETTINGS > SYSTEM > OS UPGRADE until WAITING FOR SYSEX, the recovery steps). Wait for the word `flash` on the keyboard. | Anything else: nothing is sent. |
| 3 | Send the messages with the `Sender` state machine: one message every 2 ms, about 30 s (DT2) or 40 s (DN2). | A MIDI error: the run stops (no resume). A new run starts from the header. The device never validates a cut stream. |
| 4 | Wait for the port to disappear (the device flashes and restarts) and to come back. | No change within 240 s: the screen tells what happened. |
| 5 | Send INFO: the runtime version and the image id must be those of the image that was sent. `--restore` has no runtime to ask. | A mismatch or no answer: exit 1 with both values. |

### 3.2 Pacing

The device sends nothing back, so the sender paces by time: 128-byte messages, 2 ms apart (finding 75: 12 times slower than the rate the emulated
receiver keeps up with, below the USB frame limit). The flash starts only after the last message, so no pause is needed for it. The real rate
is not measured on a device **[O]**.

### 3.3 Identifying the running image

INFO ends with an 8-byte image id (runtime 0.5.2; an older runtime sends nothing there, a client reads the field only when it is present).
patchkit stamps it into the runtime's device object (`dkrt_image_id`, after the marker `DKIMGID1`) when it makes the image. Definition: the first
8 bytes of SHA-256 over `"DKIMG1"`, the stock base's SHA-256, then the decoded MAIN OS section and the decoded DSP section, each as a 32-bit
length and the bytes, with the 8 id bytes set to zero (`packages/patchkit/src/imageid.rs`). The other sections of a patched image are the
stock base's, so these inputs fix the whole container. The packed container cannot be hashed, because repacking changes it once the id is in
it; the decoded sections have a fixed layout. `prepare` re-reads the id from the finished image by its marker and checks it against the sections.

### 3.4 Failure modes and recovery

| What the user sees | Meaning | Action |
|---|---|---|
| `FAILED` checks | The image is not a known-safe patch of a known stock image | Fix the build; nothing is sent. |
| Port not found | Cable, or USB MIDI is off (SETTINGS > SYSTEM > USB CONFIG) | Fix; nothing is sent. |
| Sent all, the port does not go away | The device refused the stream (a checksum, HMAC or build-floor error on its screen) and kept the old image | Read the screen; start again. |
| Port back, INFO shows another image id | The update was not applied | Start again. |
| Port back, INFO times out | The new image does not reach the runtime (boot loop, safe mode, a broken hook) | Power-cycle, read the screen, recovery. |
| Port never comes back | The new image faults before USB | Recovery. |

Recovery **[V in the emulator, not on hardware]**: hold FUNC while powering on; the bootstrap's STARTUP menu comes up (patchkit never changes
section 2); TRIG 4 selects OS UPGRADE; it receives over **MIDI DIN only**, so a DIN interface is needed; send the stock `.syx` of the
installed version (`docs/PATCHING.md`, "Recovery"). A DIN mode of `patchkit flash` is not built.

## 4. The bridge

### 4.1 Process model and addresses

`dkbridge.py` (new, in tools/device): one Mac process per device. It owns the MIDI port (python-rtmidi, as `RtMidiTransport`; not a repository dependency,
so the bridge is started with `uv run --with python-rtmidi`). It listens on a Unix socket `out/runs/bridge/<name>.sock` (mode 0700 directory) and
writes `current.json` beside it, the same layout as the emulator's run dir, speaking the same JSON-RPC 2.0 lines. A client that already talks to
the emulator (`tools/control/digi_client.py`) therefore connects unchanged for the methods both support. Only the bridge reads the port; a second
reader would steal replies.

| Address | Meaning |
|---|---|
| `emu` | the emulator found through `out/native/run/current.json` (direct, no bridge) |
| `emu:<socket>` | a given emulator socket |
| `dt2:<port substring>` | a Digitakt II through a bridge for that MIDI port (the bridge is started on the first use, or found by name) |
| `dn2:<port substring>` | a Digitone II (device id from `[digikit] name`; the file decides, nothing else is hard-coded) |
| `bridge:<socket>` | an already running bridge |
| `emu-usb:<socket>` | the bridge's device backend on the emulator's USB MIDI (section 6) |

`device.py` (new, in tools/device) resolves the string and returns an object with the methods of section 4.2; the existing `Emu` class is one backend,
the bridge socket client the other. Scenarios and agents write `Device("dt2:Digitakt").peek(addr, n)` or `Device("emu").peek(addr, n)`.

### 4.2 Command set

| Method | Emulator backend | Device backend | Exists today |
|---|---|---|---|
| `status` | `status` | INFO plus bridge state (port up, last reboot seen, image id) | emulator yes; device mapping new |
| `info` | the INFO command through `EmuTransport` | INFO | tool yes (`dkupload.py:228`); bridge method new |
| `mem.cf.peek` | `mem.cf.peek` | PEEK in 512-byte pieces, only inside `[digikit] peek` (`E_PEEK` otherwise) | both yes; bridge new |
| `mem.cf.poke` | `mem.cf.poke` | refused (`E_READONLY`); a later `POKE` limited to a `[digikit] poke` range is possible, not designed here | emulator yes |
| `log.follow` (JSON lines) | LOG through `EmuTransport` | LOG, polled | tool yes (`dklog.py:345`); stream method new |
| `framebuffer.get` | `framebuffer.get` | SCREEN (new, 4.4) | emulator yes; device new |
| `input.button`, `input.tap` | same | KEY (new, 4.6), needs the RC row | emulator yes; device new |
| `input.turn` | same | KEY with an encoder record | emulator yes; device new, **[O]** |
| `midi.usb.in` / `midi.usb.out` | same | raw bytes to and from the port (notes); dk replies are taken out of the stream by the bridge | emulator yes; device new, no runtime change |
| `store.ls`, `store.put`, `store.rm`, `store.clear` | the Uploader over `EmuTransport` | the Uploader over the port | tool yes (`dkupload.py`); bridge methods new |
| `flash` | `firmware.load` (the emulator's flash is a load) | `patchkit flash` steps 3-5 after the human steps | device new |
| `events.subscribe` | emulator kinds | `log` events, `port.up`, `port.down`, `reboot` | emulator yes; device new |
| `restart`, `step`, `run.mode`, `state.*`, `dsp.*`, `guard.*`, `pcm.tap`, `leds.get` | yes | `E_NOT_ON_DEVICE` | emulator yes |

A client asks `hello` for `{backend, device, capabilities}`; the capability list comes from INFO (a `caps` byte in the same tail as `image_id`)
so a client does not call SCREEN on a runtime that lacks it.

### 4.3 Log streaming and notes

`log.follow` reuses `dklog.Follower` unchanged: the bridge polls LOG once a second (every 250 ms while a client asks), and pushes each event as
one JSON-RPC notification whose params are the object `dklog.event_dict` builds, plus the gap and device-restart notes `Follower` already
makes. A client that wants a file gets the format `dklog follow` writes today (`# ` lines and JSON lines).

Notes: plain USB MIDI. `midi.usb.in {bytes}` writes the bytes to the port; the device's USB-MIDI receive path routes notes to the sequencer
or the external-input handling as it does for any keyboard. `midi.usb.out` returns the non-dk bytes that arrived (clock, notes out). The bridge
drops `F0 7D 64 6B` replies from this stream. The firmware's USB receive callback is the SysEx assembler (`docs/CONTROL.md` paragraph on
`midi.usb.in`), so a note message needs no special case. Channel and port selection (USB MIDI has two cables, `42:29`) is a parameter, default cable 0.

### 4.4 Screen capture

Emulator: the OLED frame comes from the panel serial records the firmware sends to the panel MCU (`packages/emulator/boot/src/panel_leds.rs:3`,
`runtime.rs:652`), 1024 bytes, `oled-1bpp-column-major`. The real device has no host-visible copy, so the options are:

| Route | Anchor | Cost | Confidence |
|---|---|---|---|
| A. PEEK of the firmware's Bitmap object | The panel framebuffer instance (`0x4313b298` in the 1.15C-era image, `07-emulator.md:830`; the 1.17 address and the pixel layout of the Bitmap object are not established **[O]**: `setPixel` is called once per pixel, 8192 per frame, so the storage may be a byte per pixel, not 1 bpp) | No runtime change if the address is in a peek range (the first range of `devices/digitakt-ii.toml:104` covers it). 8192 bytes would be 16 PEEKs per frame. | Medium: needs the 1.17 address as a `[digikit] oled_addr` and `oled_layout` in the device file, found with `fw.py`/cfdb. |
| B. New SCREEN command | Same address, read by the runtime, converted to the emulator's 1024-byte format in the MIDI task | Runtime change, about 100 bytes of code; one request per 512 bytes (2 requests, 2 replies of about 590 wire bytes, under the 1 KiB reply limit). | Medium-high once A has found the address. |
| C. Hook the panel UART transmit | The ColdFire driver that sends `0x10..0x17` OLED rows (`panel_leds.rs:3`) | Code in a time-critical path, a new hook | Low; not recommended. |

Recommendation: A first as a read-only experiment through the existing PEEK (zero runtime change, finds the layout), then B so that a frame is
one consistent snapshot. Rate: 2 requests per frame at about 3-5 ms per round trip gives 20 to 50 frames per second for the wire; the bridge
caps at 10 per second. A frame read across the UI task's redraw can tear; SCREEN reads twice and compares, A reads twice and keeps the second if
they differ.

### 4.5 Upload

`store.*` is the existing protocol through `Uploader` with the port transport; the bridge adds a lock so a LOG poll never interleaves with the
2-in-flight DATA window (`digikit-upload.md` "DATA rules"): while an upload runs, log polling pauses and resumes after END.

### 4.6 Panel input

A key must reach the firmware's UI queue, not the panel hardware. Known **[V]** (finding 03, `03-ui-and-panel.md:150-175`, 1.15C-era addresses): the UI
main loop pops a queue of item pointers, 16-byte records `type, code, flags, timestamp`; type 0 is a key event; flags `0x01` press, `0x10`
release; edges are posted from `0x40110d7c`. Needed anchor: the firmware function that posts a key edge (and what allocates the item), for 1.17,
as a rule in the runtime module (`dkrt.fw.ui_post_key`). Design: KEY (`u8 code, u8 flags`) writes a mailbox slot in the MIDI task; the UI loop
hook (task context) calls the firmware's post function; this is the same shape as the module mailbox. Encoders use a different record type, not
yet traced **[O]**. Confidence: medium for buttons (the post function is identified for 1.15C/1.16; the 1.17 anchor and the item allocation
need one static pass, about 3 hours), low for encoders until traced. Checks in the runtime: debug ON, RC ON, code not YES/NO, rate and mailbox
limits.

## 5. Runtime changes and cost

| Change | Where | Cost on the device |
|---|---|---|
| `RUNTIME_VERSION` 3 -> 4 | `modules/core/runtime/sdk/src/lib.rs:13` | none |
| INFO tail: `image_id[8]` after the capability bits (built in runtime 0.5.2) | `upload.rs` `cmd_info`; patchkit stamps the id | 8 bytes data, about 20 instructions per INFO |
| SCREEN 0x22 `u8 part` (route B) | `upload.rs`, anchor `dkrt.fw.oled` | about 100 bytes code; runs only when asked, 512-byte copy in the MIDI task |
| KEY 0x30 `u8 code, u8 flags` | `upload.rs` plus the UI loop hook | mailbox of 8 slots (64 bytes); the hook adds one load and compare per UI pass when empty |
| REMOTE CONTROL row | DIGIKIT menu (`core/digikit-menu`) | one more row; one RAM flag, not stored |
| Error codes `E_RC_OFF` (-64), `E_BUSY` (-65), `E_NOT_SUPPORTED` (-66) | protocol table | none |

Protocol version stays 1 (commands are only added; `digikit-upload.md` status line). Everything is behind DEBUG; with DEBUG off the runtime
answers INFO, LIST, LOG-errors and PEEK as today and refuses SCREEN and KEY. Idle cost is zero: the device sends nothing unless asked.

## 6. Tests (emulator first)

The device backend takes a transport, so the same bridge code runs on three: the real port, a fake port and the emulator. The emulator case is
possible **[V]**: `EmuTransport` (`dkupload.py:109`) already carries the `Uploader` over `midi.usb.in/out`. One difference: it steps the emulator
inside `recv`; a bridge needs `LiveEmuTransport` that polls `midi.usb.out` against a daemon in `run.mode free` (`digi-emud --free`, small new
code). The emulator does not model USB timing, enumeration, the 4 KiB TX ring timing or a port disappearing, so those are tested with the fake.

| Piece | Emulator-first proof | Kept test (new) |
|---|---|---|
| Bridge framing, rate limits, refusals, reconnect | A fake `rtmidi` loopback driven by a reference decoder of the protocol; port vanishes and returns | `test_bridge_fake.py` (new) |
| Same commands on both addresses | `emu` and `emu-usb:` give equal results for `info`, `mem.cf.peek` (inside the ranges), `log.follow`, `store.ls` | `test_bridge_emu.py` (new) |
| Log streaming | Events from `emu-usb:` equal `dklog --emu` output (existing `tests/digikit/test_dklog.py` pattern) | same |
| `patchkit flash` refusals | Failing checks, a tampered image, a wrong product byte or build word, a damaged message list: nothing is sent | `packages/patchkit/src/flash.rs` and `packages/patchkit/tests/flash.rs` (built) |
| `patchkit flash` happy path | Emulator: the update streamed through `midi.usb.in` at OS UPGRADE; the flash equals the image; the emulator boots it; INFO `image_id` equals patchkit's (built, finding 75) | `tools/emulator/os_upgrade_rig.py --patchkit` |
| Cut transfer | Emulator: truncate the stream at N messages; assert the flash model is unchanged and the old image boots (answers 3.2 (a), (b)) | same |
| Image id | Build, flash in the emulator, INFO equals; a stock image has no marker and gives none | `imageid.rs` and `packages/patchkit/tests/flash.rs` (built) |
| SCREEN | Emulator: SCREEN bytes equal `framebuffer.get` data for the same frame (hash compare) once the emulator exposes the Bitmap object to the runtime; layout test on the Bitmap object for 1.17 | `tests/digikit/scenarios` case |
| KEY | Emulator: RC off refuses; RC on, KEY(code) gives the same frame hash after N instructions as `input.tap(code)`; YES/NO refused | scenario case |
| Existing pins | PCM pins and stock MidiRpc byte equality do not change (a stock image has no runtime) | `mise run test` |

Hardware is not needed for any of these. A proof that the device accepts the pacing needs the device (rung D2).

## 7. Steps, estimates and first device rung

Estimates are agent time, not calendar time. Payoff: one interface for emulator and hardware removes the hand-run `dklog`/`dkupload` commands,
makes the flash step checked (preflight, image id) and is the base of the device-versus-emulator diff (Phase 10 item 5). Confidence: high for
bridge, log, peek, upload, notes and flashing on the emulator; medium for SCREEN (layout of the 1.17 Bitmap object unknown); medium-low
for KEY (anchor and encoder record). About 4 days in total, as the plan says.

| # | Step | Estimate | Output |
|---|---|---|---|
| 1 | Static read of the MAIN OS update receive path (ack, storage, truncated stream) and an emulator run of the USB update, with a truncated stream | 3 h | finding with [V]/[O] marks; pacing decision |
| 2 | `device.py`, `dkbridge.py` (log, info, peek, store, notes), `LiveEmuTransport`, fake-port tests | 4 h | `emu-usb:` and fake proofs |
| 3 | `patchkit flash` (built 2026-10-28; `--din` is not built) | done | tests |
| 4 | Runtime 4: INFO tail with `image_id` and `caps`, patchkit stamp, `preflight` prints the id | 3 h | emulator proof |
| 5 | Find the 1.17 Bitmap object; PEEK-based screen (route A) in the bridge; SCREEN (route B) if the layout is simple | 4 h | frame equals `framebuffer.get` |
| 6 | REMOTE CONTROL row, KEY, anchor `ui_post_key`, emulator proof | 6 h | key proofs |
| 7 | Device-versus-emulator use: `mise run` tasks and `emu:ctl`-like `dev:ctl` CLI | 2 h | docs in `docs/CONTROL.md` style |

First device rung (**D1**, no flash): with the runtime 3 image already on the device (rung C4, finding 47), run the bridge read-only: INFO, LOG
follow, PEEK inside the ranges, LIST, notes. It proves the port handling and the JSON-lines stream on silicon and needs only Em to plug in the
cable. D2: Em flashes the runtime 4 image with `patchkit flash` at 2 ms per message, watching the OLED (no YES is asked); this is also the first
real-device test of the pacing and the image id. D3: SCREEN. D4: REMOTE CONTROL and KEY, with Em setting the row by hand.

## 8. Open questions for Em

1. Does the device answer each message of the OS update, and does the YES prompt appear before the data or after the last message? (From
   what you saw in Transfer: its progress and how long the 1.88 MB took, which bounds the real rate.)
2. Is 2 ms per message acceptable as the first device pacing, with you watching, or do you want step 1 (static read plus emulator run) to
   decide the rate before any send?
3. Should KEY ever press YES and NO? The design refuses both so a test cannot confirm an OS update or a restart; some menu tests need them.
4. Encoders: needed for the first version, or buttons only?
5. Should the bridge offer a `restart` on the device (a runtime command behind RC that does what APPLY & RESTART does), or stay human?
6. Image identity: is the hash of the patch record (base plus module objects) the right definition, or do you want the sha256 of the `.syx`
   file shown by `shasum` (cannot be embedded; would need a side table keyed by the id)?
7. `dn2` is in the address scheme but there is no Digitone II on the desk; keep it design-only until one is connected?
