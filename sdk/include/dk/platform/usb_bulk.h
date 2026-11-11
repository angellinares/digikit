/* <dk/platform/usb_bulk.h> (needs core/usb-bulk): a vendor bulk interface to the host, beside the stock USB MIDI. The calls are imports of the runtime
 * (sdk/sdk.toml, `requires = "core/usb-bulk"`): usb_bulk_enable, usb_bulk_take, usb_bulk_release, usb_bulk_send, usb_bulk_state, usb_bulk_takeover and usb_bulk_giveback. A module that uses
 * them lists `requires = ["core/usb-bulk"]`; without the adapter in the image the module does not load.
 *
 *   id = api->io_post(step, done, 0);   in dkm_init
 *   usb_bulk_enable(id);                in the first step: adds interface 2 (vendor class 0xFF, EP2 OUT and EP2 IN, bulk, 512-byte packets) to the
 *                                       USB MIDI configuration and, in USB MIDI mode, re-enumerates once (the host sees the device leave and return)
 *   while (usb_bulk_take(&buf, &len)) { ...use buf[0..len)...; usb_bulk_release(buf); }
 *   usb_bulk_send(reply, n);
 *   return DK_IO_WAIT;                  the adapter wakes the job (io_wake) when a buffer is full or a send slot is free
 *
 * Transfers. The host's OUT data arrives in buffers of up to 16 KiB (four 4 KiB pages, the same address space as the DMA: the pointer is in the uncached
 * alias of DDR). A buffer ends with the USB transfer: a full buffer, or a packet shorter than 512 bytes. A host frame whose length is a multiple of 512 bytes
 * must therefore be followed by a zero-length packet (the usual USB rule; without it the buffer never completes); the adapter drops the empty buffer. There are four buffers. When all are full or taken
 * the adapter queues nothing, the controller answers NAK, and the host waits: that is the flow control, no window protocol is needed. A message may span
 * buffers: the module's protocol (lengths in its header) puts the stream together.
 *
 * usb_bulk_send copies up to 16 KiB into one of two send slots and returns at once (E_BUSY when both are in flight: return DK_IO_WAIT and try again
 * when woken; DK_USB_BULK_E_AGAIN when nothing is in flight and the engine refused: the adapter wakes the job within a few milliseconds).
 *
 * usb_bulk_enable prepares the interface in any USB mode, and re-enumerates once when the device is in USB MIDI mode. It returns E_STATE (-53) while the device is
 * not in USB MIDI mode (USB AUDIO/MIDI uses EP2 IN for the audio clock feedback) or a mode switch is settling (the firmware restarts the device about a second after
 * it): the job calls it again later (a timed wake). Bit 2 of the state word shows the mode live.
 *
 * Take-over (docs/plans/usb-fast-takeover.md). In USB AUDIO/MIDI the bulk pair cannot exist. usb_bulk_takeover() (from a command handler is fine: it only records the
 * request) makes the adapter switch the device to USB MIDI in RAM about 50 ms later, from its UI-pass tick, never in the caller's context; the USB CONFIG setting is
 * not written, so a reboot returns to it. USB audio pauses. In USB MIDI it succeeds at once without a switch. E_STATE in Overbridge or another USB mode. After it the
 * job calls usb_bulk_enable until it stops returning E_STATE (the switch is not done yet). The switch enumerates with the bulk interface already present: one
 * re-enumeration, not two, when the job has called usb_bulk_enable before (in any mode). usb_bulk_giveback() switches back about 50 ms later. The adapter gives back
 * by itself after the host's disconnect, a bus suspend of 0.5 s, or 30 s without host activity (any bulk transfer, or another usb_bulk_takeover() call, which renews),
 * and it drops the take-over without switching when the USB mode changes by another route (the user's USB CONFIG choice wins). Bit 3 of the state word shows it.
 *
 * The adapter also wakes the job about four times a second while the interface is enabled (a timer for a module that has none: the retry of usb_bulk_enable,
 * a check for an idle session).
 *
 * One job owns the interface (usb_bulk_enable from another job is E_BUSY); call usb_bulk_disable(id) in dkm_fini. The job is also woken when the bus is reset.
 *
 * A USB bus reset or a disconnect cancels every queued transfer and drops the buffers that were full and not taken; the reset count in usb_bulk_state
 * changes. A buffer that is taken stays yours. The module's protocol must treat a changed reset count as the end of any transfer in progress.
 *
 * State word of usb_bulk_state: */
#ifndef DK_USB_BULK_H
#define DK_USB_BULK_H

#include <dk/core/api.h>
#include <dk/core/status.h>

#define DK_USB_BULK_ENABLED 0x1u   /* usb_bulk_enable was called */
#define DK_USB_BULK_CONNECTED 0x2u /* the host configured the device and EP2 is on */
#define DK_USB_BULK_WRONG_MODE 0x4u /* the device is not in USB MIDI mode (USB CONFIG); live, also after enable */
#define DK_USB_BULK_TAKEN 0x8u /* usb_bulk_takeover is active (requested, or the device is switched) */
#define DK_USB_BULK_FULL(st) (((st) >> 8) & 0xffu)  /* receive buffers full and not taken */
#define DK_USB_BULK_TX_BUSY(st) (((st) >> 16) & 0xffu) /* send slots in flight */
#define DK_USB_BULK_RESETS(st) (((st) >> 24) & 0xffu)  /* changes with every bus reset or disconnect */

#define DK_USB_BULK_E_STATE DK_E_STATE /* usb_bulk_enable: the device is not in USB MIDI mode (USB CONFIG); nothing was changed */
#define DK_USB_BULK_E_AGAIN DK_E_AGAIN /* usb_bulk_send: nothing in flight, the engine refused; wait for the wake and call again */

#define DK_USB_BULK_RX_BYTES 16384u /* the largest buffer usb_bulk_take gives */
#define DK_USB_BULK_TX_BYTES 16384u /* the most one usb_bulk_send takes */

#endif
