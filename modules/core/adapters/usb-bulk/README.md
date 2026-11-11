# core/usb-bulk

A vendor bulk interface (interface 2, EP2 OUT and IN, 512-byte packets) added to the firmware's USB MIDI mode, for run-time modules: `usb_bulk_enable`,
`usb_bulk_take`, `usb_bulk_release`, `usb_bulk_send` and `usb_bulk_state` (`sdk/sdk.toml`, `sdk/include/dk/usb_bulk.h`). Plan: `docs/plans/usb-fast.md`
section 4; facts: `docs/findings/42-usb-transfer-routes.md` section 4.

- **No firmware patch.** The USB engine is the same code at the same addresses on DT2 1.17 and DN2 1.12. Its port table is RAM: `usb_bulk_enable` checks
  the stock entry 2 (callbacks, length, the 101 descriptor bytes), then writes four words (connect, disconnect, descriptor pointer and length) and
  re-enumerates with the firmware's own stop and restart pair. Not called, nothing is written and USB is stock. Outside USB MIDI mode (USB CONFIG) enable
  refuses with E_STATE and changes nothing; bit 2 of `usb_bulk_state` shows the wrong mode live.
- **Anchors** (`devices/fw/usb.toml`): the port table, connect, disconnect, queue, cancel, dTD allocation, stop and restart.
- **Receive**: four 16 KiB buffers (four 4 KiB pages each: every dTD names all five page pointers) in the uncached alias of DDR, one dTD each on EP2 OUT.
  The completion callback marks a buffer full, queues a free one and wakes the module's I/O job (`dkrt_io_wake`). No free buffer: nothing queued, the host
  gets NAK. **Send**: the bytes are copied into one of two slots and queued on EP2 IN.
- **Reset**: the connect and disconnect callbacks cancel both EP2 queues with a bounded flush (the stock disconnect polls without a bound), drop buffers
  that were full and not taken, and change the reset count of `usb_bulk_state`.
- **Emulator**: the controller is not modelled. `usb.bulk.in` and `usb.bulk.out` (`docs/CONTROL.md`) complete the dTDs through the guest's own callbacks.
  Test: `tests/usb-bulk.device.test.ts` with the fixture `test/usb-echo`.
