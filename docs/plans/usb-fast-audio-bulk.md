# Fast USB in USB AUDIO/MIDI with a bulk OUT endpoint

Status: **built and hardware-tested on DT2 and DN2 (2026-11-13), take-over deleted, merged with main's SDK S2b-2.** Facts: [finding 123](../findings/123-usb-takeover.md). Follows [usb-fast.md](usb-fast.md). The earlier take-over plan is deleted (git history keeps it).

## Why

A take-over (mode switch in RAM) failed on about 1 in 15 to 1 in 200 switches and needed a power cycle (finding 123), so it was deleted. The probe of 2026-11-13 (commit 8585548d) showed another way: add a bulk OUT endpoint (interface 5, EP 0x02, 512 bytes) to the USB AUDIO/MIDI configuration at boot. macOS keeps audio and MIDI; 200 MB went at 28 MB/s while audio played. The controller is never stopped.

## Design

- New core module `core/usb-bulk-audio` (`modules/core/adapters/usb-bulk-audio/`): the boot hook on the USB init function (`cf.usb.hook.init`, `ua_tramp.s`), `ua_prepare`, and the port 5 connect and disconnect wrappers. The module is in the default `device` and `device-introblank` sets of `devices/digitakt-ii.toml` and `devices/digitone-ii.toml`.
- `ua_prepare` checks the stock configuration (length 0x148, header `09 02 xx xx 05`), copies it, appends interface 5, repoints port entry 5, and calls `usb_bulk_port5_attach()`. It is idempotent and ISR-safe. If a check fails it refuses and touches nothing.
- `core/usb-bulk` exports `usb_bulk_port5_attach`, `usb_bulk_port5_connect` and `usb_bulk_port5_disconnect`. The receive ring gate is "bulk-capable port": port 2 (pair) or port 5 attached (OUT only). Every EP2 control write in port 5 touches only the RX half (mask `0x0000ffff`). EP2 IN is audio feedback and is never queued.
- `usb_bulk_state()` has the bit `DK_USB_BULK_OUT_ONLY`. `usb_bulk_send` returns E_STATE in that mode.
- Replies go over SysEx. usb-fast keeps replies in its queue (16) and the host drains them with `0x37 'R'`: reply body = complete frames (`DKB1`, cmd|0x40, seq, status, len, body), oldest first, at most 879 bytes, never split. INFO `flags` bit 0 = replies over SysEx. The parse throttle (no parsing with 14 or more queued replies) stays, so an unpolled host stalls by NAK.
- Host: `SysExReplyLink` (bulk link whose send() writes OUT and whose receive() polls `0x37 'R'`), `EmuOutOnlyLink` for emulator tests, `chooseBulk` tries OUT-only candidates after the pair candidates, `dkbulk open --out-only`. The desktop and web pill reads "FAST USB" in both modes. The "Set USB CONFIG to USB MIDI" hint stays only for firmware without `core/usb-bulk-audio`.
- The take-over is gone: `dk_usb_bulk_takeover` and `dk_usb_bulk_giveback` stay in the SDK as stubs that return `DK_USB_BULK_E_STATE` (removing an import is an API major bump). `DK_USB_BULK_TAKEN` is never set.

## Split

| PR | Content |
|---|---|
| A | The design above, in USB AUDIO/MIDI and USB MIDI. The `0x37 'S'` speed switch, the card log and the USB MIDI bulk pair stay. |
| B | Delete the take-over: the `usb_bulk_takeover` and `usb_bulk_giveback` bodies, TAKEOVER and RELEASE (0x1e, 0x1f), the PHY park and the recovery code, `BulkTakeOver`, `takeover-cycle`, the `USB_SW` log kind, the anchors only they used. |

## Gating

- The emulator test covers the upload path in USB AUDIO/MIDI (emulator `usb.bulk.in` targets EP2 OUT; replies through `'R'`).
- Hardware: DT2 and DN2 in USB AUDIO/MIDI, upload while audio plays, replug, host sleep, a bus reset during a transfer. The DN2 has run the probe and real uploads (2026-11-13): DN2 6.5 MB at 3.38 MB/s, 200-file pack of 146 MB at 2.09 MB/s, USB audio clean. DT2: 2.74 MB/s and 1.50 MB/s for the same two uploads.
- The SDK change (new state bit, `'R'`) selects a wide merge gate: plan a targeted test list.

## Open [O]

- Hardware recheck on both devices after the merge with main's SDK S2b-2.
- Windows stays on SysEx (see Next, item 5).
- Long-run stability and bus reset during a transfer.
- macOS asks once to allow the accessory: the workbench should say so.

## Next

Agreed with Em on 2026-11-13.

1. Session and memory lifecycle owned by `core/usb-bulk`: ring lifetime, release on wrong mode or idle, one wake source. usb-fast only reacts to state.
   Why: the module-side release (M2) is a stopgap; the adapter header already promises this.
2. An SDK interrupt-mask facility (`dk_irq_save` / `dk_irq_restore`) or a single-consumer reply queue, instead of inline ColdFire asm in a content module.
   Why: a content module should not carry CPU-specific asm for mutual exclusion.
3. Move the bench, knobs and speed switches (`'S'`) out of the shipped usb-fast into a usb-fast-dev module or a build define.
   Why: the speed word is debug-service territory.
4. Stay a `.dkm` tool; split a bulk-link service only when a second consumer of the framed stream exists (for example live sample streaming).
   Why: a service without a second consumer adds a layer and no value.
5. Windows: a WinUSB (MS OS 2.0) descriptor for interface 5 needs a control-request hook; until then Windows stays on SysEx.
   Why: without the descriptor Windows does not bind a driver to the bulk interface.
6. Faster sample packs: the per-file store cost (DN2 table rewrite per commit, DT2 about 195 ms per file).
   Why: it is the main limit now, not the USB link.
