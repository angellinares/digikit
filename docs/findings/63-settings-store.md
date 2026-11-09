# 63. The settings store (FX routing step 2, DT2 1.17 and DN2 1.12, 2026-10-25)

digikit keeps its own settings (which run-time effects are send effects, their 16 send amounts, later more) in a record of our own in the card's store range, never in an Elektron file. The format and the rules are in
`docs/design/digikit-store.md` ("The settings record", "Reading our data locally"); this finding records what was built, how it was checked and what is open. Marks: **[V]** verified here by a second check, **[D]** measured or read once by the author
(nothing here has been checked by a second agent yet, so nothing is **[V]**), **[O]** open.

## What was built **[D]**

| Piece | Where |
| --- | --- |
| the contract: sizes (`[settings]`: 2 copies of 16 sectors, keys up to 47 bytes, values up to 1024), the keys by owner (`[[settings.key]]`), capacity kind `SETTINGS` 13, command `SETTINGS` 0x19, capability bit 11, statuses -66 to -69, log events 32 and 33 | `sdk/sdk.toml`, generated into `gen_sdk.rs`, `dkproto.py`, `gen_proto.rs`, `gen_proto.ts` by `patchkit gen` |
| the layout: header copy B moved from `sectors - 3` to `sectors - 35`; the settings copies at `sectors - 34` and `sectors - 18`; `store::TAIL` 35; `[card.store] settings_sectors = 32` (checked against the sdk by `patchkit gen`) | `modules/core/runtime/sdk/src/store.rs`, `devices/digitakt-ii.toml`, `devices/digitone-ii.toml`, `devgen.rs`, `device-profile`, `plusdrive` (`STORE_TAIL`), `tools/patchkit/pk_device.py` |
| the codec (encode, decode, `why`, edit in place, load of the two copies, write of one) | `modules/core/runtime/sdk/src/settings.rs`; 15 unit tests and the shared vectors `sdk/settings-vectors.json` (8 valid, 18 refused records) |
| the runtime (RAM record, the 2 s delay, the write to the other copy, C ABI, mailbox test commands, SETTINGS command, CAPACITY entry) | `modules/core/runtime/src/rs/src/settings.rs`, `upload.rs`, `lib.rs`, `boot.rs` |
| the readers | `patchkit store dump` (`packages/patchkit/src/storedump.rs`), `tools/device/dksettings.py`, `tools/device/dkupload.py settings` |
| the test | `tests/regression/runtime/settings.test.ts` (the runtime settings store, both devices) |

## Decisions **[D]**

- **Where:** inside the store area, between header copy B and the selection record, so the area still ends with the card (no sector outside `[card.store]` is written) and the runtime's range check (`Layout::write_allowed`, finding 46) needs no new rule.
  The cost is 32 sectors (16 KiB) of 82 MiB on a Digitakt II. The area is part of the tail, so every gap and free-space function counts it as used with no change in their code; only `TAIL` changed.
  Header B moves with it (to `sectors - 35`): on a card built before, the old B sector at `sectors - 3` is now the last sector of settings copy B; nothing reads a header there, header A is still usable and the next commit of the old store writes B at its new place.
  Pre-1.0, no migration (as the store's own versions).
- **The magic is the store header's "DKST"** (Em's design). The two cannot be mixed up: the store's version is the u32 at byte 4 (3), the settings record's is a u16 at byte 4 (1) followed by `seq`, so the u32 is 0x0001xxxx. `card::check` probes only header sectors with the store's parser, which refuses it.
- **Reads go straight into the RAM record by DMA** (8 KiB, `DKRT_SETTINGS`, and `DKRT_SETTINGS_HDR` for the first sectors), not through the staging buffer, so a load needs nothing that is free at the boot only. The proofs' write-watch of `cf.expansion` leaves both out as it leaves the staging
  buffer out (`pk_dkrt_machine.py`, `pk_dkrt.py watch_ranges`, `digikit_menu_proof.py`): without that every boot of every scenario reported 18 writes.
- **A record is written only as far as it reaches** (`ceil(used / 512)` sectors, 1 to 16); the bytes after `used` are ignored, so a shorter record over a longer one is valid.
- **ColdFire code rules:** the codec compares keys with a loop (`same`), not `==` (a slice comparison is a `memcmp` call; `compiler_builtins` links one into the runtime object anyway, but the rule keeps the object free of calls into the firmware's libc).

## Proofs **[D]**

- `cargo test -p dkrt-sdk`: 34 tests (15 new): the vectors (decode, encode back, why, padded and exact), put/get/replace/remove with entries behind, key and value bounds, a full area refused with the record unchanged and a replacement that fits to the byte, no record is an empty one, the higher seq wins
  and A on a tie, a damaged newer copy (entry byte, crc, used) leaves the older one with `damaged` 1, both damaged is empty, a power cut in the write (first sector of 3 written) leaves the current copy, a write takes only the sectors it needs and only its copy, a shorter record over a longer one, read errors.
- `patchkit` (Rust): the copy reader on every vector, value printing, the choice of the copy. `tests/digikit/test_dksettings.py`: the Python reader on the same vectors, the SETTINGS command read in two pieces.
- Scenario `settings-dt2` and `settings-dn2` (22 emulators each, a coupled boot per part): the put and get through the mailbox, the bad keys, CAPACITY (limit 8192, used 18 then 90), SETTINGS before and after the write, nothing written in 1 s and nothing in 1.5 s after a second change (the delay starts again), the write 2.5 s after the first
  change into copy A only (the only store sector written), `patchkit store dump` on the card as the guest left it (copy A in use, seq 1, `fx/route/...` as master, the 16 amounts), the next change into copy B with copy A byte for byte unchanged, a soft restart that reads copy B (seq 2) and gets the last values,
  the next change into copy A (seq 3) without the removed key, a blank card claimed before the first write (header pair, then copy A), and three hand-made cards (torn newer copy: older read, error event, write to the torn copy with seq + 1; seq 9 over 3: A read, B written with 10; both torn: empty, A written with seq 1).
  The write-watch and flush check over every boot are clean.

## Cards written before the settings area **[D]**

The old allocator ended at `sectors - 3` (header B and the selection record were the tail); the new one ends at `sectors - 35`. Checked by reading the code and by scenario:

- **No new file can land in the tail:** `Layout::limit()` is `sectors - TAIL` (`store.rs`, `pub const TAIL: u32 = 1 + SET_SECTORS + SEL_SECTORS`), `gaps` pushes the fixed extent `(header_b, sectors - header_b)`, and `write_sectors` refuses `rel > limit`.
- **An old store that reaches into the tail** (the table or a file ends past `header_b`; `used` or the table past `limit` was already refused by `plausible`) is not used: `store::open` skips a header copy for which `reaches_tail` is true, so there is
  no usable copy, uploads and LIST answer `E_NOSTORE`, nothing loads from it, and CLEAR is the way out. The settings store is **off** (`SETTINGS_LOAD` state 4, CAPACITY limit 0, `E_SET_OFF`); nothing is written in the store range, and the sectors of the
  file, header B and the first settings sector are byte for byte as they were (scenario `settings-*` part `overlap`: a hand-made header, table and four pattern sectors). This is stricter than "the store stays readable": reading it would need `plausible` and the commit code
  to know the old tail, and the case needs about 82 MiB of files on a Digitakt II (a DN2 store is 22 GiB: samples there are not played by the firmware yet). **[O]** a card in that state needs CLEAR, or a runtime that still reads it, which this one does not.
- **An old store that does not reach the tail** (header A valid, old header B at `sectors - 3`, nothing at today's B): boots, loads its module, keeps the selection (seq 1); the settings store is on; a settings write goes to copy A; an upload then writes header B at the new place with
  A's `seq` and both files list (scenario part `oldcard`, hand-made from a built card: today's header B zeroed, header A copied to `sectors - 3`).

## Open **[O]**

- Not run on a real card or device. The write is one block write of up to 16 sectors in the UI task, at most once per 2 s of changes (an upload or a store commit writes more, at once).
- A card the firmware formats afresh (`dt2` +Drive reset) does not touch the store range (finding 59), so the record survives; a card swap loses it with the +Drive, as the selection record does.
- No `.dkm` module API (`<module id>/...` only, reserved), no TypeScript reader (the web client has CAPACITY kind `SETTINGS` by the generated names only), no export of the record into `plus_drive/<device>/digikit/` (the folder mirror ignores it, as it ignores the selection record).
- Step 3 and 4 of FX routing use `settings::get/put` (`dkrt_settings_*` from `core/fxpages` and the menu); the key rules (`fx/route/<module id>`, `fx/send/<module id>`) are in `sdk.toml` already.
