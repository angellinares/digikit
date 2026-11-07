# digikit module upload over USB SysEx

Status: built (device side, Python tool, exporter; lane r4, finding 43), 2026-10-18; SELECT and RESTART added 2026-10-19 (lane dkrun; runtime 4, protocol version still 1: commands were only added; host `tools/device/dkrun.py`, `dk run MODULE`); LOG and PEEK, the card-safety codes and INFO's log range added
2026-10-18 (lane r4, finding 46; runtime 3, protocol version still 1: commands were only added); spec (plan `docs/plans/digikit-runtime.md` Phase 7, decision 11; routes in
`docs/findings/42-usb-transfer-routes.md`). The device side is `core/runtime`; the hosts are a Python tool (tests, terminal),
the desktop app and the web app. Every host and the device build against this file; change it first, then the code.

## What it does

A host writes, replaces, deletes and lists the files of the module store (`docs/design/digikit-store.md`) while the
firmware runs. Nothing is loaded: an uploaded module appears in DIGIKIT > MODULES and loads after APPLY & RESTART, as
any store file does. The upload works in safe mode (that is how a bad module is replaced). It writes only store sectors;
it never touches the path filesystem or a project. Only SELECT (runtime 4) writes the selection record, and RESTART (runtime 4) resets the device; nothing else does.

## Framing

```text
F0 7D 64 6B 01 <cmd> <seq> <body, 8-in-7 packed> F7          host -> device
F0 7D 64 6B 01 <cmd | 0x40> <seq> <body, 8-in-7 packed> F7   device -> host, one reply per request, in order
```

- `7D` is the MIDI non-commercial ID; `64 6B` is "dk"; `01` is the protocol version. A message with another version gets
  the reply status `E_PROTO` and nothing else happens.
- `seq` (0..127) is the host's; the reply echoes it.
- Packing (as the firmware's MidiRpc, finding 42 section 2.5): each group of up to 7 data bytes becomes a collector byte and
  then the 7 bytes with bit 7 cleared; data byte j (1..7) of the group takes its bit 7 from collector bit (7 - j). The last
  group may be short. Example: `80 01 02 03 04 05 06` packs to `40 00 01 02 03 04 05 06`; `FF` packs to `40 7F`.
- Integers in the unpacked body are big-endian. Paths are bytes plus a NUL, under the store's path rules (1 to 159 bytes of printable text, `/`
  separated, no empty, `.` or `..` part; docs/design/digikit-store.md).
- A request is at most 40 KiB on the wire (the firmware drops a SysEx message over 128 KiB and holds about 150 KiB in its pool).
  A reply is at most 1 KiB on the wire (the firmware's TX ring is 4 KiB and spins when full), and **a reply is sent only when the ring has room**:
the runtime reads the firmware's count of bytes handed to the USB stack and not yet sent (anchor `cf.midi.site.tx_inflight_lit`) and drops the
reply when it is above 256 or when the reply would not fit below 0xff0 with it; a host that does not read its input cannot hang the MIDI task.
The host's retry asks again (`docs/findings/43-digikit-upload.md`, "No unbounded spin on a reply").

Every reply body starts with `i8 status` (0 = ok). Codes, in the runtime's code space (registry -20.., DSP -30.., boot -45):

| code | name | meaning |
|---|---|---|
| -50 | E_MSG | body too short or malformed |
| -51 | E_PATH | path breaks the store rules |
| -52 | E_FULL | no gap for the file, or none for the table beside it (reply carries `u32 free_bytes`, INFO's: the sum of the free space, saturated to 32 bits; a store with many gaps may refuse a file smaller than that) |
| -53 | E_STATE | no upload open (DATA, END) |
| -54 | E_OFFSET | offset is neither the next byte nor a retry of the previous chunk |
| -55 | E_LENGTH | chunk length breaks the rule below, or the file would pass its size |
| -56 | E_HASH | the file hash of the bytes received (hashed as the DATA chunks came) does not match the hash slot of BEGIN |
| -57 | E_WRITE | the firmware's block write failed |
| -58 | E_READ | the firmware's block read failed |
| -59 | E_NOFILE | DELETE of a path that is not in the store |
| -60 | E_PROTO | unknown protocol version or command |
| -61 | E_NOSTORE | the device has no `[card.store]` or the store header is unreadable (CLEAR still works) |
| -62 | E_INUSE | BEGIN, DELETE or CLEAR on a card whose store area is not ours and not blank: the runtime writes nothing there (see "Card safety") |
| -63 | E_PEEK | PEEK of a range that the device file does not list, or a length of 0 or over 512 |
| -65 | (free) | was `E_DISABLED`, the guard of the remote range while DIGIKIT > REMOTE was off; the switch and the guard are gone |
| -66 | E_SET_KEY | a settings key breaks the rules: 1 to 47 printable ASCII characters (runtime 0.5.1; also returned by `dkrt_settings_*`) |
| -67 | E_SET_FULL | the settings area is full |
| -68 | E_SET_NOKEY | no such settings key; for the SETTINGS command: no record on the card and none made |
| -69 | E_SET_VALUE | a settings value is over 1024 bytes, or the caller's buffer is too small |
| -70 | E_SET_OFF | the settings store is off: an older store on the card reaches into its area (or the store's headers do not read) |
| -64 | E_CMD_RANGE | `Api.register_cmds` with a range that is empty, outside every reserved range, or over a registered one (runtime 0.2.5) |

SELECT also answers the selection record's own codes (runtime codes -40 to -49): -40 `E_SEL_PATH` (a path is empty, too long (over 55 bytes) or has a character outside 0x21 to 0x7e), -41 `E_SEL_COUNT` (more than 8 paths), -42 `E_SEL_WRITE` (the firmware's block write failed), -46 `E_RANGE`, -47 `E_CARD_RO`.

## Commands

| cmd | name | request body | reply body after status |
|---|---|---|---|
| 0x01 | INFO | (none) | `u16 protocol 1, u16 RUNTIME_VERSION (4 since SELECT and RESTART, 5 since the DSP guard: voice rows of 16 words, DSP sections of version 2, 24 DSP diagnostic words, log events 27 and 28; the commands are unchanged), device id NUL ("dt2" or "dn2"), u32 free_bytes, u32 max_chunk 32768, u8 flags, u32 log_oldest, u32 log_newest` (flags: bit 0 safe mode, bit 1 upload open, bit 2 card read-only, bit 3 the debug module is loaded (a log sink is set), bit 4 a crash record was found at this boot; the runtime sends 0 and 0 for the two sequence numbers: the log is the debug module's, and its LOG reply has the oldest and newest sequence number; runtime 2 stops after the flags; later runtimes append `u32` the packed runtime version, `u32` the capability bits and, from runtime 0.5.2, `u8 image_id[8]`: the id of the flashed image that patchkit stamps into the runtime (definition: `docs/design/device-bridge.md` 3.3), all zero when the image carries none; a host reads each appended field only when the reply is long enough) |
| 0x02 | LIST | `u32 first`, then optionally `prefix NUL` (1 to 159 bytes; runtime 0.3.0, capability bit 10) | `u32 total, u8 n`, then n entries `path NUL, u32 size, hash[32]` from index `first`, as many as fit the reply. Without a prefix: every entry of the table, `first` is a table index. With one: only the entries whose path starts with it (files, and folders with nothing in them), `first` and `total` count those. A folder entry has size 0 and a zero hash (a file of size 0 has the hash of nothing, `43fa243b`, never zero). The 32-byte hash is the file hash slot: the +Drive content hash `| 1` (`dksample::content_hash`, a lookup3 variant over big-endian words) as 4 big-endian bytes, then 28 zero bytes (capability bit 12, FILEHASH; a host that finds the bit missing stops with "Update the firmware: this runtime uses the old store format"). The host pages until it has `total`. A prefix without its NUL: E_MSG. |
| 0x03 | MODULES (runtime 0.2.0, capability bit 5) | `u8 start` (a module record index) | `u32 arena_free, u32 arena_largest, u32 dsp_free, u32 dsp_largest, u8 total, u8 n`, then `n` records (below). The loaded modules and what they hold; see "MODULES". |
| 0x04 | CAPACITY (runtime 0.2.2, capability bit 6) | (none) | `u8 version 1, u8 n`, then `n` entries `u8 kind, u32 limit, u32 used, u32 largest`. Per resource of the runtime; see "CAPACITY". |
| 0x10 | BEGIN | `path NUL, u32 size, hash[32]` (the file hash slot, see LIST) | (none). Opens an upload; a BEGIN while one is open drops the old one. |
| 0x11 | DATA | `u32 offset, bytes` | (none). |
| 0x12 | END | (none) | (none). Verifies, then commits (below). |
| 0x13 | DELETE | `path NUL` | (none). Commits a table without the path. It does not touch the selection record: a selected or loaded file that is deleted stays selected and loaded until the next boot, which skips the path with a `MOD_LOAD` error line (`E_NOT_FOUND`) and carries on. |
| 0x14 | CLEAR | `u32 0x434c5221` ("CLR!") | (none). Commits an empty store. |
| 0x15 | ABORT | (none) | (none). Drops an open upload. |
| 0x16 | READ (runtime 0.2.4, capability bit 7) | `path NUL, u32 offset, u32 length` | `u32 file size`, then `min(length, 768, size - offset)` bytes of the file from `offset`. Read-only: every state, one card read, not logged as an UPLOAD event. Errors: E_NOFILE (not in the store), E_LENGTH (length 0), E_OFFSET (offset at or past the end of the file), E_PATH, E_MSG. The cap of 768 keeps the reply (status, size, bytes) inside the 880-byte reply body; a host reads a larger file with several requests. A .dkm header is the first 0xa0 bytes (`digikit-dkm.md`). |
| 0x17 | VERIFY (runtime 0.2.4, capability bit 8) | `path NUL`, optionally `u32 offset, u32 length` (runtime 0.3.0) | `hash[32]` (the file hash slot) of the file, or of that range of it, computed by the device from a fresh read of the card. Read-only, not logged. The device spends about 7 ColdFire instructions a byte on the hash (finding 43), so a host still checks a large file in ranges of a MiB or so (`offset` is a multiple of 512). Errors: E_NOFILE, E_READ, E_PATH, E_MSG, E_OFFSET (not a multiple of 512, or at or past the end), E_LENGTH (0, or past the end). A host compares it with its own hash to know that the bytes on the card are the bytes it sent. |
| 0x18 | MKDIR (runtime 0.3.0, capability bit 10) | `path NUL` | (none). Makes a folder: a folder entry with nothing in it. Ok when the folder exists (an entry, or implied by files inside it). E_PATH when the path is a file, lies inside one or breaks the path rules. A folder entry goes when a file or a folder is stored inside the folder. An open upload is dropped. |
| 0x19 | SETTINGS (runtime 0.5.1, capability bit 11) | `u32 offset, u16 length` (1 to 768) | `u8 flags` (bit 0: the RAM record has changes the card does not hold yet), `u8 copy` (the card's current settings copy: 0 A, 1 B, 2 none), `u32 seq` (the card's), `u32 used` (the record's bytes), then `min(length, used - offset)` bytes of the record from `offset`. Read only, not logged; answers from the RAM record, sealed with the card's `seq` when it has changes, so the pieces of one read are one valid record. Errors: E_SET_NOKEY (no record), E_OFFSET (at or past the end), E_LENGTH (0), E_MSG. The record format and rules: `docs/design/digikit-store.md`, "The settings record". Host: `dkupload settings`. |
| 0x20 | LOG | `u32 from_seq` | `u32 oldest, u32 newest, u8 n`, then `n` events of 32 bytes from `from_seq` (or from `oldest` when `from_seq` is older), as many as fit one reply (27). An event is `u32 seq, u32 time, u16 kind, u16 flags, u32 a0..a4`, big endian; `time` is the firmware's block counter (1500 per second), `flags` bit 0 an error. The host polls: ask again from `last seq + 1` until `n` is 0. Nothing is ever sent unless a host asks. |
| 0x21 | PEEK | `u32 addr, u16 len` | `u32 addr, u16 len`, then `len` bytes of RAM. `len` is 1 to 512 and `[addr, addr + len)` must lie wholly inside one range of `[digikit] peek` in the device file (`devices/*.toml`: the firmware's data and the runtime's own region with the crash-record area); no peripheral registers. |
| 0x30 | SELECT (runtime 4) | `u8 n` (0 to 8), then `n` store paths, each followed by a NUL | (none). Saves the selection record (what the next boot loads): `boot::selection_set`, the function the menu's APPLY & RESTART calls through `dkrt_selection_set`, so the card range check and the "blank or ours" rule apply and the boot counter and the DEBUG bit carry over. `E_INUSE` on a card that is not ours and not blank. The paths are not looked up in the store (a missing file is reported by the boot as `E_NOT_FOUND`, as for the menu). `n` 0 clears the selection. No effect until the next boot. |
| 0x31 | RESTART (runtime 4) | (none) | (none). The reply is sent first; the soft reset (`dkrt_restart`, the one APPLY & RESTART runs) follows from the UI task's next passes, once the TX ring has drained (at most 4000 passes). `E_STATE` while an upload is open. When `dkrt_restart` refuses (a card transfer is in flight) the request is dropped and the log gets an UPLOAD error event `{cmd 0x31, status}`; the host sees the runtime still running and asks again. A host that wants the new boot reads LOG: the new boot's log holds no UPLOAD event for cmd 0x31 (the ring starts again), and holds BOOT_START. |

The device id is the device, not the firmware version: the runtime has no per-port constant (a port is a set of anchors, and a
constant would be a fourth copy of a fact `devices/ports/` holds). A host that wants the version asks the stock MidiRpc
SoftwareVersion (the stock route is unchanged). *Changed 2026-10-18 (lane r4); the first draft said "dt2-1.16".*

Behaviour the first draft left open (lane r4, built and tested):

- A card with nothing in the store area is an empty store: INFO, LIST and BEGIN work, and the first END writes both headers. A card that is smaller than the area has no store (`E_NOSTORE` for every command that needs it).
  `E_NOSTORE` is for a header that is there but unreadable (no usable copy); CLEAR still works then.
- END closes the upload whatever its result (a failed END needs a new BEGIN). BEGIN, DELETE and CLEAR drop an open upload. ABORT with
  none open is ok.
- The table is streamed through two 8 KiB buffers, so a store holds as many entries as the card has room for (version 4, docs/design/digikit-store.md); a request body is at most 256 bytes (a path of 159 bytes
  and BEGIN's size and hash slot make 196).
- The buffer is a static object of the runtime, 32 KiB plus 16 bytes of body offset and 4 KiB scratch (36,896 bytes), 16-byte aligned. The first
  draft said "from its arena"; a static address lets the write-watch leave out the card reads that land in it by DMA, as it does for
  the staging buffer, and the buffer cannot fail to allocate.
- The reply status is a signed byte; `body` is what follows it.

DATA rules: `offset` equals the next expected byte, or the offset of the previous chunk with the same length (a retry; the
reply is ok and nothing is written again). The length is 1 to 32768 and a multiple of 512, except for the chunk that ends the
file. The host may keep at most 2 DATA requests in flight; replies come in order.

## Service commands (runtime 0.2.5, capability bit 9)

The runtime keeps its commands in a table (command number, flags, handler: one lookup after the other, since a jump table is not ColdFire code; the
flag marks a read-only command whose reply is not logged). A run-time module can add commands of its own with `Api.register_cmds(first, count, handler)`
(api 1.2, `docs/design/module-sdk.md`). Only numbers in a range that `sdk/sdk.toml` `[protocol.ranges]` reserves can be registered; today `remote = [0x38, 0x3d]`
(the panel's key, turn and release commands of `digikit/remote`) and `remote_state = [0x3e, 0x3f]` (its state). A request number is 6 bits (bit 6 of the command byte marks a reply), so ranges lie in 0x01 to 0x3f.
A range that is empty, outside a reserved range or over a registered one is refused with `E_CMD_RANGE`; at most eight ranges are registered (a module should register one range per command group, with one handler that dispatches on the command number); the module's ranges are released
when it is taken down (`fini`). INFO's `caps` bit 9 (`CAP_SERVICE_CMDS`) says the runtime has them.

A service module is on exactly when it is loaded: the check mark of DIGIKIT > MODULES is the only switch, and no range is guarded (there is no REMOTE switch). A module with settings
of its own shows them under SETTINGS... of its MODULES row (`dkm_menu`, `docs/design/digikit-dkm.md`).

A service command is handled in the firmware's MIDI task, like the built-in ones. The handler gets the command, `seq`, the body (at most 160 bytes: the same limit as
every command but DATA and SELECT) and a buffer for the reply body (at most 879 bytes); it returns the reply length (the status is 0), or a negative status (sent with
no body). A reply of a service command is logged as an UPLOAD event only when its status is not 0 (a key press must not fill the log ring); a number nobody registered is `E_PROTO`.

## CAPACITY

A read-only command like MODULES (every state, no card read, not logged as an UPLOAD event); INFO's `caps` bit 6 (`CAP_CAPACITY`) says the runtime has it. The reply is `u8 version` (1), `u8 n`, then `n` entries of
`u8 kind, u32 limit, u32 used, u32 largest` (big endian, 13 bytes). The kinds are `[capacity.kinds]` of `sdk/sdk.toml` (generated: `dkproto.CAPACITY_KINDS`, `CAPK_*` in `gen_proto.rs`); a host skips a kind it does not know, and a
kind is appended, never renumbered. `used` and `largest` come from the same tables as MODULES; `largest` is 0 where a largest free piece has no meaning.

| kind | name | limit | used | largest |
|---|---|---|---|---|
| 1 | MODULES | module records (`[limits] modules`) | loaded | 0 |
| 2 | SELECTION | paths of the selection record (`SEL_MAX`) | paths this boot loaded | 0 |
| 3 | ARENA | ColdFire arena bytes | taken, headers included | payload of the largest free block |
| 4 | STAGE | staging buffer: the largest .dkm file | 0 | the limit |
| 5 | DSP_AREA | DSP run-time area bytes (0 without a DSP loader) | taken; alignment gaps count as free | the largest free range |
| 6 | MACHINE_ROWS | run-time machine types the registry reserves | in use | 0 |
| 7 | VOICE_ROWS | DSP voice rows | in use | 0 |
| 8 | PARAM_ROWS | parameter rows core/dynreg reserves | in use | 0 |
| 9 | PAGE_ROWS | page rows core/dynreg reserves (0 on the Digitakt II) | in use | 0 |
| 10 | FX_SLOTS | effects in the chain (`[limits] fx_slots`) | 0 (run-time effects do not exist) | 0 |
| 11 | MAP_ENTRIES | sticky machine-type map entries (`MAP_N`) | in use | 0 |
| 12 | STORE | the store area in **KiB** (a store can be 22 GiB; 0 when the card is too small for it) | KiB the headers say are used (header A, the table, the files); a card with no header: 0 | 0 (BEGIN answers E_FULL with the free bytes; a fragmented store can refuse a file smaller than the sum of its gaps) |
| 13 | SETTINGS | the settings record's area of one copy, in **bytes** (`[settings] sectors` x 512: 8192) | the record's bytes, header included (an empty record: 18) | 0 |
| 14 | SEND_SLOTS | effects that can run on a send bus: the send page's cells (`[limits] send_slots`: 2 on the Digitakt II; 0 on an image without the send page, so on the Digitone II for now) | loaded effects on the send route (the setting `fx/route/<module id>`, finding 64) | 0 |

`Uploader.capacity()` returns `{name: {limit, used, largest}}`; `mise run dk:store -- --emu info` prints one line per kind (and says "not reported" for a runtime without the bit). The Rust client (`packages/dkupload`) has no MODULES
or CAPACITY request yet; it gets the generated numbers only (P5).

## MODULES

A read-only command like INFO and LIST: it works in every state (an open upload, safe mode, a read-only card), reads no card sector and is not logged as an UPLOAD event. INFO's `caps` bit 5
(`CAP_MODULES`) says the runtime has it. `start` is a module record index (0 to 7); the reply holds the used records from `start` on that fit one reply body (880 bytes), so a host asks again from
the last index + 1 until it has `total` records (`Uploader.modules()` in `tools/device/dkupload.py` does that). `total` is the number of loaded modules (a module whose CF part is in, with its DSP part
still loading, counts). The four figures are `arena_free` and `arena_largest` (bytes of the ColdFire arena, the largest free block), `dsp_free` and `dsp_largest` (the same for the DSP area; 0 on an image without
the DSP loader). A record, big endian:

```text
  0   1  u8 index            module record index
  1  32  name               the module's name field, NUL padded
 33   4  u32 version        the module's packed version (major << 16 | minor << 8 | patch)
 37   8  contracts          api, machine, fx, service as (major, minor) bytes, as the file declared them (0 = not declared)
 45   4  u32 cf_base        the module's ColdFire block in the arena (image + bss)
 49   4  u32 cf_size
 53   4  u32 dsp_off        its range in the DSP area (byte offset into the area; 0 and 0 without DSP code)
 57   4  u32 dsp_len
 61   4  u32 rows           the DSP voice rows it owns, as a bit mask
 65   1  u8 n_types         0 to 8
 66  nt  type bytes         the machine types it has: its machine rows (the types the boot assigned, in type order), then its voice rows' types not listed yet (a pinned voice type)
 66+nt 4 u32 faults        the DSP guard's fault count for those types since the boot
 70+nt 1 u8 flags          bit 0: the guard has switched one of them off
```

A record is 71 + `n_types` bytes. `faults` and the flag come from the guard words the runtime polls once a second (`DSP_DIAG` words 17 to 23): they name only the type of the *last* fault and of the last type switched off,
so the count is exact for one faulting type and, when two types fault inside one poll, goes to the last one. `dkupload --port NAME modules` (`mise run dk:store -- --port NAME modules`) prints a header with the free space and one line per module.

## The debug log (LOG and PEEK; finding 46)

The runtime keeps a small RAM ring of 32 boot and error events (`modules/core/runtime/src/rs/src/dlog.rs`) and hands every event to one sink; the module `digikit/debug` (`modules/tools/debug`) keeps a ring of 512 events and answers LOG, PEEK and LOGCFG (the service range `debug`, 0x20 to 0x22). The events are: boot steps, safe mode and why, the card check, each module's
load result, upload commands, menu actions, a health snapshot every ten seconds, the DSP diagnostics once a second (raw words of `dl_diag`), the DSP
heartbeat watch, the crash record of the boot before. Without the module only error events and the boot's events are kept (INFO then has no LOG capability); the module's level filter drops the others above its level (LOGCFG). The argument words of each kind are listed in `docs/findings/46-digikit-debug.md`; the host tool is
`tools/device/dklog.py` (`follow`, `crash`, `peek`). LOG and PEEK follow the framing and the TX guard of every other command: a reply is sent only when the
firmware's TX ring has room, so a host that does not read its input cannot hang the MIDI task (the host asks again). LOG, PEEK and INFO are not logged.

## Card safety (finding 46)

Every card write of the runtime goes through one function, which refuses (a) any write outside the store range of the device file (`[card.store]`; error
`E_RANGE` inside the runtime, counted, logged) and (b) every write when the store area is neither ours nor blank. At boot (or at the first write when no boot
hook ran) the runtime looks: the area is **ours** when store header A or B is a valid header or a selection copy is valid (when only a selection copy is valid and *both* header sectors hold bytes that are neither a store header nor blank, the upload commands (BEGIN, DELETE, LIST) answer `E_NOSTORE`: the store is somebody's data and is not treated as empty); otherwise every sector of the range
must read all 0x00 or all 0xFF (**blank**, read once, 64 KiB at a time, stopping at the first sector that is not); else it is **foreign** (or **unreadable**
when a read failed) and the runtime is read-only for this card: no boot counter (so no safe mode by counter), no selection, no upload (BEGIN, DELETE and CLEAR
answer `E_INUSE`; INFO, LIST, LOG and PEEK still work), ABOUT shows `CARD READ-ONLY` and where the foreign data starts, and the log has one error event with the
first non-blank sector and a crc32 of it. A write of data or a table into a blank range (an upload's staging) first writes an empty store header pair, so a cut
upload leaves a card that the next boot calls ours; the selection record and the store headers are themselves marks of ours and need nothing first.

## Store writes and the commit

The store format becomes version 2 (`docs/design/digikit-store.md` is updated with it; readers take 1 and 2):

```text
header, version 2: as version 1, plus
  24  4   u32 table_first_sector   (version 1: always 1)
  28  4   u32 seq                  sequence number of this header
  32  ..  zero
  508 4   u32 crc32 of bytes 0..508
```

There are two header copies: A at store sector 0 and B at the sector just before the selection record (`end_sector -
selection_sectors - 1`; recorded in `devices/<device>.toml` `[card.store]`, not hard-coded). The builder writes both with
`seq` 1. The builder's limit becomes `range - selection_sectors - 1`.

- **Reading:** a copy is valid when the magic, version, header CRC and table CRC hold (a version 1 copy has no header CRC:
  it is valid only when B is not). Use the valid copy with the higher `seq`; equal: A.
- **Equal headers before staging:** BEGIN first makes the two header copies equal (the lagging, torn or version 1 copy is rewritten from
  the winner); CLEAR makes the space reusable, and a cut between B and A must not leave a stale copy that names a table the next upload
  overwrites ("The CLEAR double fault", finding 43). If that write fails BEGIN answers `E_WRITE` and stages nothing.
- **Staging:** BEGIN checks the path against the table (it may replace a file; it may not replace a folder, lie inside a file or be a folder that has entries: `E_PATH`) and finds the
  first gap that holds `ceil(size / 512)` sectors, and a gap beside it for the new table of one more entry (`E_FULL` otherwise). DATA writes whole sectors into the file's extent at once (the last partial sector is zero
  padded). Nothing the current header points at is written.
- **Commit (END, DELETE, CLEAR, MKDIR):** END first checks the file hash of the bytes received (DATA feeds a running hash, so END costs the same for a file of any size and a 4 GiB sample is not read back through a 250 MHz ColdFire; VERIFY reads the card, in ranges). Then the new table is streamed from the old one, with the entry inserted, replaced (a replaced path takes the
  new entry and frees the old extent; a folder entry that the new path lies inside is dropped) or removed, into the first gap that holds it (never over the table in use or the staged file), read back and checked by its CRC; then header B with `seq + 1`, then header A
  with `seq + 1`. A power cut before B leaves the old store; between B and A the next reader takes B. Space freed by a delete or a replace is reused by the next BEGIN; BEGIN, DELETE and CLEAR first
  make the two header copies agree.
- **Full:** BEGIN answers E_FULL with the free bytes. The host's way out is to delete files (the space is reused), or CLEAR and an upload of the files it wants; a module missing at boot is skipped and reported, as today.
- Every card access goes through the firmware's block read and block write (`cf.storage.fn.block.read`, `cf.storage.fn.block.write`; the
  write takes the eMMC mutex), from the MIDI task, one request at a time.

## Where the device handles it

In the firmware's MIDI task, after the SysEx gather (finding 42 section 2, stage 4), before the router: a message that
starts `F0 7D 64 6B` goes to the runtime; every other message takes the stock route unchanged. The exact hook is a ported
anchor on both ports (`dkrt.fw.*`). The runtime's upload buffer is a static 16-byte aligned object of 36,896 bytes (see "Behaviour the first draft left open").

## Tests

- CLI transfer acceptance: `modules/core/runtime/tests/transfer-live.device.test.ts` on DT2 1.17 and DN2 1.12, through the real `tools.device.dktransfer` CLI. Install/restart persists the upload and selection, the mirror matches the card's file hash and metadata, delete plus `card.sync` removes it, and the next protocol restart skips the selected missing module with `MOD_LOAD E_NOT_FOUND` and reaches `UI_FIRST`. Run with `mise run test:runtime -- transfer-live` (prerequisites: `tests/regression/README.md`); the gate selects this acceptance on relevant changes. Remaining scenario tests are in `tests/digikit/scenarios`.
- upload, list, restart, load: equal to a card built with the file in `digikit/` (rows, tables, PCM).
- replace, delete, CLEAR, E_FULL, E_HASH (a wrong file hash commits nothing), DATA retry and bad offset.
- power cut during staging, between header B and A, and after A: the next boot reads a valid store (old or new, never mixed).
- an upload during playback: PCM equal to the same run without it; the write-watch shows store sectors only.
- stock MidiRpc unchanged: a Ping and a SoftwareVersion reply are byte-identical with and without the runtime.
- `upload-folders` (store format 3): MKDIR and its errors, LIST by prefix and its paging (forty entries), a streamed upload of 6 MiB (far over the staging buffer), VERIFY in ranges, a delete that frees its space for the next file (first fit), a smaller file that takes a hole, CAPACITY's STORE entry, the +Drive round trip.
- DN2 sample-store acceptance (`tests/regression/runtime/dn2-samples-store-live.test.ts`, replacing `samples-store`): a WAV converted by the host and sent into `samples/<folder>/<name>` in several chunks through the generic samples API (`packages/dk-session/src/samples.ts`, exported as `@digi/dk-session/samples`), VERIFY against the host's file hash, a corrupted copy reported as failed, a delete that frees the space, a RESTART after which the store still lists and verifies the file, and the folder mirror holding the file. Run with `mise run test:runtime -- dn2-samples-store-live.test.ts`, using the prepared runtime inputs in `tests/regression/README.md`.
- the +Drive round trip: after an upload in the emulator, the exporter writes the store's files back to
  `plus_drive/<device>/digikit/`, so the next card build holds them.

## `dk run` (runtime 4; `tools/device/dkrun.py`)

One command from a module id to a running module: INFO (the runtime must be 4 or newer; its device id picks the pack target), `pk_build` and
`patchkit pack`, PUT to `modules/<name>.dkm` unless LIST shows the same file hash, SELECT with that one path, RESTART, wait for the new boot (poll LOG
until the old boot's RESTART event is gone and BOOT_START is there; the USB port may vanish and return, the tool connects again), print MOD_LOAD and
MOD_DSP (MOD_LOAD result 1 is LOADING, MOD_DSP result 0 the DSP part loaded), then follow the log for `--seconds`. The selection becomes this module
only; the other store files stay. Against the emulator the same code runs (`--emu`; the scenario `menu-dkrun-pluck` proves it: upload, SELECT, RESTART,
the next boot loads the module and the machine plays like the patched-in pluck).
