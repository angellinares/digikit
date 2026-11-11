# Fast USB in USB AUDIO/MIDI mode: the workbench takes over USB

Status: **agreed 2026-11-11; not started.** Follows [usb-fast.md](usb-fast.md) and [usb-fast-hardware.md](usb-fast-hardware.md) (section v2).

## Goal

Fast USB transfers while the device's USB CONFIG is USB AUDIO/MIDI, without the user switching modes. The controller has 4 endpoints. USB
AUDIO/MIDI uses EP1 (MIDI), EP2 IN (audio clock feedback) and EP3 (audio streams), so a bulk pair cannot exist beside live USB audio. Instead the
workbench switches the device to the USB MIDI configuration in RAM for the transfer and switches back. USB audio pauses during the take-over.

## Decisions (Em, 2026-11-11)

- The existing usb-fast module owns the commands (TAKEOVER, RELEASE). The core/usb-bulk adapter does the switch. The usb_fast SysEx range widens
  by two numbers (one SDK regeneration).
- Automatic: only when the workbench starts a transfer. Nothing switches while the workbench is closed.
- Hold time: the workbench takes over at the start of a transfer action (a "batch": one user action, for example 100 dropped files), keeps it
  across the batch, and gives back about 10 s after the last transfer ends (a second drop within 10 s reuses it). It gives back at once when the
  workbench closes or loses the device. Device backstop: give back after 30 s with no host activity.

## Firmware facts (static, DN2 1.12 and DT2 1.17, read from code; finding 42)

- The mode switch `FUN_40006c16(port)` has the same address and bytes on both images. It compares with the active port index (DN2 0x4058f2a8, DT2
  0x40965a60) and returns if equal. Otherwise it stops USB (`FUN_40006bac` -> `FUN_40005dcc`: RS clear, USBINTR 0, the old port's disconnect
  callback), writes the active port and entry, and restarts (`FUN_40006bc6`) after a delay of about 30 ticks counted by the USB tick
  `FUN_4000713e`.
- It never writes the USB CONFIG setting byte (DT2 `DAT_4099d5c4`, DN2 `DAT_405ce898`). Its only writers are the three menu actions (DT2
  0x4010331a, 0x40103330, 0x40103348; DN2 0x40104dc2, 0x40104dd8, 0x40104df0), the factory defaults and the settings load. The settings record
  holds the byte, not the active port. A save or power-off during a take-over therefore keeps the stored mode. At boot the byte selects the port
  (DT2 `FUN_400cc9c8`, DN2 `FUN_400cedfc`), so a reboot always returns to the stored mode.
- Stopping port 5 runs its normal disconnect path (`FUN_40004054`: endpoint 2 and 3 off, flushes, timers cancelled), the same as an unplug.
- There is no lock. The stop runs in the caller's task and cuts any in-flight USB MIDI reply. A command that triggers the switch must reply first,
  and the switch runs a little later (adapter tick, about 50 ms).
- Unplug: VBUS (OTGSC BSV) and bus suspend call the active port's disconnect callback. The active port does not change. Connection state word:
  0x40965a40 (DT2).
- Only one firmware path re-applies the stored mode by itself: an Overbridge (port 6) disconnect (`FUN_400f2ce0`). The USB CONFIG menu shows the
  stored setting. Changing it during a take-over stores the new value and switches: the user's choice wins.

## Design

| Part | Change |
|---|---|
| Firmware anchor | `cf.usb.fn.mode_switch` (cf_func on `FUN_40006c16`, same hash as `cf.usb.data.port_table`) |
| core/usb-bulk adapter | API `usb_bulk_takeover()` and `usb_bulk_giveback()`; a "taken over" state bit. Refuse in Overbridge (port 6). Remember the original port. Switch from the adapter tick about 50 ms after the reply. Give back on RELEASE, on unplug or suspend (from the tick, not the interrupt), and after 30 s idle. Drop the take-over when the active port changes by other means. |
| usb-fast module | Commands TAKEOVER and RELEASE. The I/O job waits for USB MIDI mode instead of ending on E_STATE, and enables the bulk link when it appears. |
| Host (`packages/dk-session`) | A `takeOver()` step next to `chooseBulk`: send TAKEOVER, wait for re-enumeration, nonce handshake, transfer, linger 10 s, RELEASE. Used by `tools/devtools/wb.mjs` and the desktop app. |
| Desktop UI | The link pill shows the take-over. |
| Web (WebUSB) | Later: re-pairing is needed after re-enumeration. |

## Tests

Emulator:

- Active port poked to 5, TAKEOVER gives 2, RELEASE gives 5.
- Watchdog give-back.
- A menu change drops the take-over.
- The setting byte is unchanged before and after (read it through its anchor).

Hardware (DN2 and DT2 in USB AUDIO/MIDI):

- Take-over, echo and bench, release, USB audio back.
- Unplug during a take-over.
- Reboot during a take-over returns the stored mode.
- USB CONFIG menu change during a take-over.

## Open (hardware only) [O]

- Whether the MIDI port name and the CoreMIDI connection survive the configuration change.
- macOS re-enumeration time each way (expected 1 to 2 s).
- The audio side's reaction to a stop mid-stream (same path as an unplug: likely fine).

## Steps and effort

About 1 day of agent work:

| Step | Estimate |
|---|---|
| Anchor | 1 h |
| Adapter API, tick and watchdog | 3 h |
| usb-fast commands and job wait | 2 h |
| Host helper, wb, desktop and pill | 3 h |
| Tests | 2 h |
| One hardware session | after the above |

The SDK range change selects a wide merge gate: plan a targeted test list (as for PR 404).
