# USB take-over: switching USB AUDIO/MIDI to USB MIDI in RAM

Plan: [usb-fast-takeover.md](../plans/usb-fast-takeover.md). Code: `modules/core/adapters/usb-bulk` (`usb_bulk_takeover`, `usb_bulk_giveback`), `modules/tools/usb-fast` (TAKEOVER 0x1e, RELEASE 0x1f). Static facts from DT2 1.17 and DN2 1.12, plus emulator runs. Marks: **[D]** read or measured once, **[O]** open. Nothing here is **[V]**: no second agent has checked it against the image bytes, and no hardware run of the take-over has been made yet.

## Why a take-over

The USB controller has 4 endpoints (RM DCCPARAMS DEN = 4). USB AUDIO/MIDI uses EP1 (MIDI), EP2 IN (audio clock feedback) and EP3 (audio streams), so the bulk pair (EP2 OUT and IN) cannot exist beside live USB audio. The workbench switches the device to the USB MIDI configuration in RAM for a transfer and switches back. USB audio pauses meanwhile **[D]**.

## The mode switch

- `FUN_40006c16(port)` has the same address and bytes on both images (hash `6ca45a25378d0a0f22d18114089f4984`, anchor `cf.usb.fn.mode_switch`). It compares `port` with the active port index (`cf.usb.value.active_port`: DT2 0x40965a60, DN2 0x4058f2a8) and returns when equal. Otherwise it stops USB (`FUN_40006bac`: USBCMD.RS clear, USBINTR 0, the old port's disconnect callback), writes the active port and the pointer to its table entry (the word below the port), and restarts the device (`FUN_40006bc6`) after a delay. **[D]**
- The delay is a counter set to 30 by the stop and counted down by the USB tick `FUN_4000713e`; the restart runs when it reaches 0. In the emulator one tick is about 33 ms, so the restart comes about 1 s after the switch on both images. The counter is the anchor `cf.usb.value.restart_delay` (rule `cf_literal` on the tick, offset 0xa: DT2 0x40965a38, DN2 0x4058f280). The adapter used a fixed offset (`active_port - 0x28`) before; the anchor replaces it. **[D]**
- The switch never writes the USB CONFIG setting byte (anchor `cf.usb.value.config_mode`, the operand of its getter: DT2 0x4099d5c4, DN2 0x405ce898; values 0 USB MIDI, 1 Overbridge, 2 USB AUDIO/MIDI). Its writers are the three menu actions, the factory defaults and the settings load. The settings record holds the byte, not the active port, so a save or a power-off during a take-over keeps the stored mode, and a boot always returns to it. The emulator test reads the byte before and after every switch: unchanged. **[D]**
- There is no lock. The stop runs in the caller's task and cuts a reply in flight on EP1. A command that triggers the switch must reply first: the adapter records the request and the UI-pass tick switches at least 50 ms later. A second switch inside the 1 s window is not safe, so the adapter keeps a guard (the restart counter, and a floor of 500 ms on its own clock) and runs one switch at a time. **[D]**
- Only one firmware path re-applies the stored mode by itself: an Overbridge (port 6) disconnect (`FUN_400f2ce0`). A change of the USB CONFIG menu during a take-over stores the new value and switches: the user's choice wins, the adapter drops the take-over without switching back. **[D]**
- Unplug (VBUS) and bus suspend call the active port's disconnect callback and leave the active port as it is. The adapter gives back after a host disconnect, a suspend of 0.5 s, or 30 s without host activity. **[D]**

## One enumeration

The bulk interface is part of port entry 2's configuration descriptor, which the adapter repoints (`usb_bulk_enable`). Done in USB MIDI mode, that needs a stop and restart, so the host sees the device leave and return. A take-over from USB AUDIO/MIDI would then cost two enumerations: the switch, then enable's own stop and restart.

Now `usb_bulk_enable` prepares entry 2 in any mode (it writes only the port table entry and the adapter's own state, with interrupts masked), and re-enumerates only when the device is in USB MIDI mode and no switch is settling. The usb-fast job calls it at its start, also in USB AUDIO/MIDI, so the take-over switch enumerates with interface 2 already present. The emulator test counts the switches of a take-over (`usb_bulk_switches`): one. **[D]** The first enable in USB AUDIO/MIDI followed by a take-over has no emulator test: the emulated boot is in USB MIDI mode and the module's first enable runs there. **[O]**

## Host side

TAKEOVER and RELEASE are SysEx commands of the usb-fast module (`usb_fast_takeover` range 0x1e to 0x1f). The host (`BulkTakeOver` in `packages/dk-session/src/bulk-select.ts`) asks `usbFastWrongMode` (the wrong-mode bit of usb-fast's STATUS) and takes over only then, waits for the device to return on its MIDI port, runs the nonce handshake, transfers, and releases after a linger (10 s in the desktop app; at once in `wb.mjs`). The MIDI port name and the CoreMIDI connection across the configuration change, and the macOS re-enumeration time each way (expected 1 to 2 s), are open **[O]**.
