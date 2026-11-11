/* core/usb-bulk: a vendor bulk interface (interface 2: EP2 OUT and EP2 IN, 512-byte packets) added to the firmware's USB MIDI mode.
 * Plan: docs/plans/usb-fast.md section 4. Facts: docs/findings/42-usb-transfer-routes.md sections 1 and 4 (static, DT2 1.17 and DN2 1.12).
 *
 * The USB engine is the firmware's and is not patched. Its port table is RAM: entry 2 (USB MIDI) holds the connect and disconnect callbacks and the
 * pointer and length of the configuration descriptor. usb_bulk_enable repoints those four words (a 124-byte copy of the stock 101-byte descriptor
 * with interface 2 appended; callbacks of ours that call the stock ones) and re-enumerates through the firmware's own stop and restart pair.
 *
 * RX: RX_BUFS buffers of RX_BYTES (RX_PAGES 4 KiB pages: the firmware only builds single-page dTDs; ours name all five page pointers, see queue_transfer). Each is queued
 * as one dTD on EP2 OUT. A dTD completes when it is full or when a packet shorter than 512 bytes (a zero-length packet too) ends the transfer: the host
 * sends a zero-length packet after any frame whose length is a multiple of 512 bytes. The completion callback runs in the USB interrupt: it marks the buffer
 * full and queues a free one. With no free buffer nothing is queued, the host gets NAK, and that is the whole flow control. The dTDs come from the firmware's
 * pool (the engine's own EP1 callback does the same from the interrupt); its free callback is wrapped to see the status byte, and a halted or error status
 * drops the transfer. TX: usb_bulk_send copies into one of TX_SLOTS slots and queues one dTD on EP2 IN (64-byte packets, see QH_CAPS_IN); the completion frees it.
 *
 * Buffers are accessed through the uncached alias of DDR, so the DMA and the CPU never see stale cache lines; only the receive reads of a module use the
 * cached address, after an invalidate (UB_CACHED_RX). The stock disconnect callback polls
 * ENDPTFLUSH without a bound; ours flushes EP2 first with a bounded poll. Interrupts are masked (SR 0x2700) around every state change, never around a
 * copy: the state is touched by the I/O task, the UI loop tick and the USB interrupt. A refused dTD leaves `need_prime`, retried from the module's calls
 * and from usb_bulk_tick (dkrt_poll_add).
 *
 * Take-over (docs/plans/usb-fast-takeover.md): usb_bulk_takeover switches the device from USB AUDIO/MIDI (port 5) to USB MIDI (port 2) in RAM with the firmware's own
 * mode switch, and usb_bulk_giveback (or an automatic give-back) switches back. The stored USB CONFIG setting byte is never written, so a reboot always returns
 * to the stored mode. The calls only record a request; the switch runs from usb_bulk_tick (UI task) at least TO_DELAY_MS later, so a SysEx reply is out first and the
 * switch never runs in the MIDI task or an interrupt. The tick is on the runtime's list from the first takeover call on (bulk need not be enabled).
 * One switch at a time: the firmware's switch has no lock and restarts the device some ticks after it writes the port. Every switch, and enable's own stop and restart,
 * stamps a guard (sw_stamp); no other switch runs, enable returns E_STATE and our disconnect callback records no host leaving until TO_GUARD_MS have passed. The tick
 * claims a pending request under the interrupt mask before it calls the switch (to_pend_port), so a takeover() or giveback() on the MIDI task at that moment is
 * a follow-up request that waits for the guard.
 *
 * Memory: the descriptor is static; the receive and send buffers are one block from the firmware's allocator (buffers_alloc), allocated when the interface can go
 * live and freed when the take-over ends (buffers_free). EP2 stays enabled (ENDPTCTRL2) from the host's connect until the disconnect, whether or not the buffers exist:
 * without them the host gets NAK. usb_bulk_connect always sets the endpoint up and queues the buffers only when they exist; buffers_alloc queues them when the
 * endpoint is up already; buffers_free takes the transfers back (flush, cancel) and frees only when the controller reports EP2 idle.
 *
 * Contexts: usb_bulk_connect and usb_bulk_disconnect are called by the firmware (the USB interrupt, the mode switch); usb_bulk_rx_done, usb_bulk_tx_done
 * and usb_bulk_cancel are the dTD callbacks (interrupt, or the cancel path of the disconnect). The others are for modules (usb_bulk_* of sdk.toml),
 * from the I/O task. */

#define UB_TRACE 0 /* 1 for hardware debug builds: log calls */

typedef unsigned int u32;
typedef unsigned short u16;
typedef unsigned char u8;
typedef int i32;

/* sdk.toml [load.status] */
#define E_NOMEM (-8)
#define E_BUSY (-12)
#define E_CONTRACT (-48)
#define E_STATE (-53)
#define E_LENGTH (-55)
/* adapter status (<dk/platform/usb_bulk.h>): nothing is in flight and the queue refused; the adapter's tick wakes the job to try again */
#define E_AGAIN (-72)

/* what a module sees: <dk/platform/usb_bulk.h> */
#define ST_ENABLED 0x1u
#define ST_CONNECTED 0x2u
#define ST_WRONG_MODE 0x4u /* the device's active USB port is not USB MIDI (live) */
#define ST_TAKEN 0x8u      /* a take-over is active (requested, or the device is switched) */
#define ST_LIVE 0x10u      /* the buffers are allocated (from the moment the device goes to, or is in, USB MIDI with bulk, until the take-over ends) */
#define ST_NOMEM 0x20u     /* the last allocation of the buffers failed (cleared by the next success) */

/* ---- the geometry ---- */
#define PAGE 4096u
/* 128 KiB of receive buffers: two 64 KiB frames of the usb-fast protocol, so the host can send the next frame while the CPU writes the last one.
 * dTDs in flight from the firmware's pool of 31 (finding 42): 8 RX + 2 TX = 10, leaving 21 for the firmware's own (EP0, EP1 MIDI: a few). */
#define RX_BUFS 8u
#define RX_PAGES 4u
#define RX_BYTES (RX_PAGES * PAGE)
#define TX_SLOTS 2u
#define TX_PAGES 4u
#define TX_BYTES (TX_PAGES * PAGE)
#define DESC_BYTES 124u

/* ---- the MCF5441x USB controller and the firmware's queue heads (the engine's own constants, FUN_400043e2) ---- */
#define USB_REG(off) (*(volatile u32 *)(0xfc0b0000u + (off)))
#define USBCMD 0x140u
#define USBSTS 0x144u
#define PORTSC 0x184u
#define PORTSC_SUSP 0x80u /* bit 7: the port is suspended */
#define ENDPTPRIME 0x1b0u
#define ENDPTCOMPLETE 0x1bcu
#define ENDPTSTAT 0x1b8u
#define ENDPTFLUSH 0x1b4u
#define ENDPTCTRL2 0x1c8u
#define FLUSH_EP2 ((1u << 2) | (1u << 18)) /* RX bit 2, TX bit 16 + 2 */
#define FLUSH_SPINS 20000u
#define QH_BASE 0x80000000u
#define QH_EP2_OUT 0x100u /* queue index 2 = endpoint + 4 * direction */
#define QH_EP2_IN 0x140u  /* queue index 6 */
#define IDX_OUT 2u
#define IDX_IN 6u
#define QH_CAPS_OUT 0x22000000u /* 512-byte packets, as the firmware sets EP1 OUT */
/* 64-byte packets, zero-length termination off, as the firmware sets EP1 IN. The TX buffer holds 64 bytes per endpoint; two back-to-back 512-byte IN
 * packets wedged the controller on the DN2. Each 64-byte packet is a short packet for the host, so no zero-length packet is needed. The descriptor
 * still says 512 (USB 2.0 requires it for high-speed bulk). */
#define QH_CAPS_IN 0x20400000u
#define TD_END 0xdead0001u
#define TD_ACTIVE 0x80u
#define TD_HALTED 0x40u /* status bits of a finished dTD: halted, data buffer error, transaction error */
#define TD_BUFERR 0x20u
#define TD_XACTERR 0x08u
#define CTRL_BULK 0x00c800c8u /* enable, data toggle reset, bulk, both directions: the value the firmware writes to ENDPTCTRL1 */

/* the port table entry of USB MIDI (docs/findings/42 section 4) */
#define PORT_ENTRY 2u
#define PORT_AUDIO 5u      /* USB AUDIO/MIDI */
#define PORT_OVERBRIDGE 6u
#define PORT_ENTRY_BYTES 0x28u
#define STOCK_CONFIG_BYTES 101u

/* firmware (anchors by hash in the port files) */
extern u8 usb_port_table[];
extern u8 usb_active_port[]; /* u32 in RAM: 2 USB MIDI, 5 USB AUDIO/MIDI, 6 Overbridge */
extern void usb_fw_connect_body(u32 hs);
extern void usb_fw_disconnect(void);
extern i32 usb_fw_queue_td(u32 index, u8 *dtd);
extern void usb_fw_cancel_ep(u32 index);
extern u8 *usb_fw_td_alloc(void);
extern void usb_fw_stop(void);
extern void usb_fw_restart(void);
extern u8 usb_restart_delay[]; /* u32 in RAM: 30 down to 0, non-zero while a stop waits to restart */
extern u8 *usb_fw_mem_new(u32 size); /* the firmware's operator new (the 32 MiB buddy allocator) and its free */
extern void usb_fw_mem_free(u8 *p);
extern void usb_fw_mode_switch(u32 port); /* FUN_40006c16: returns when `port` is active; else stop, write the port, restart after about 30 USB ticks. Task context. */
/* the board: the uncached alias of DDR is this far above the cached window (device.s) */
extern const u32 usb_alias_delta;
extern const u32 usb_ddr_base; /* the cached DDR window: base and size */
extern const u32 usb_ddr_bytes;
/* the runtime (dkrt.h) */
extern void dkrt_io_wake(i32 id);
extern i32 dkrt_poll_add(u32 f); /* the UI loop calls f once a pass */
extern u32 dkrt_blocks(void);    /* DSP blocks since boot (1500 a second); 0 without core/health */
extern void dkrt_log_ev(u32 kind, u32 flags, const u32 *a);

#if UB_TRACE
/* sdk.toml [[log.kind]] 14 MOD_LOG: args msg0..msg3 (16 ASCII chars, big-endian words) and value */
#define UB_KIND_MOD_LOG 14u
/* flags: not an error (bit 0 clear), level 3 (debug) in bits 1-3 (sdk.toml level_shift 1) */
#define UB_FLAGS (3u << 1)

/* `tag`: a string of at most 16 chars, padded with NULs */
static void ub_log(const char *tag, u32 value)
{
    u32 a[5];
    u32 i;
    u32 live = 1;
    for (i = 0; i < 16u; i++) {
        u32 c = 0;
        if (live && tag[i] != 0)
            c = (u8)tag[i];
        else
            live = 0;
        if ((i & 3u) == 0u)
            a[i >> 2] = 0;
        a[i >> 2] |= c << (24u - 8u * (i & 3u));
    }
    a[4] = value;
    dkrt_log_ev(UB_KIND_MOD_LOG, UB_FLAGS, a);
}
#define UB_LOG(tag, v) ub_log((tag), (v))
#else
#define UB_LOG(tag, v) ((void)0)
#endif

/* the stock configuration descriptor with wTotalLength (bytes 2 and 3, written by the firmware at run time) left out, and the interface appended */
static const u8 stock_config[STOCK_CONFIG_BYTES] = {
    0x09, 0x02, 0x00, 0x00, 0x02, 0x01, 0x00, 0xc0, 0x03, 0x09, 0x04, 0x00, 0x00, 0x00, 0x01, 0x01, 0x00, 0x00, 0x09, 0x24, 0x01,
    0x00, 0x01, 0x09, 0x00, 0x01, 0x01, 0x09, 0x04, 0x01, 0x00, 0x02, 0x01, 0x03, 0x00, 0x00, 0x07, 0x24, 0x01, 0x00, 0x01, 0x41,
    0x00, 0x06, 0x24, 0x02, 0x01, 0x01, 0x00, 0x06, 0x24, 0x02, 0x02, 0x02, 0x00, 0x09, 0x24, 0x03, 0x01, 0x03, 0x01, 0x02, 0x01,
    0x00, 0x09, 0x24, 0x03, 0x02, 0x04, 0x01, 0x01, 0x01, 0x00, 0x09, 0x05, 0x01, 0x02, 0x00, 0x02, 0x00, 0x00, 0x00, 0x05, 0x25,
    0x01, 0x01, 0x01, 0x09, 0x05, 0x81, 0x02, 0x00, 0x02, 0x00, 0x00, 0x00, 0x05, 0x25, 0x01, 0x01, 0x03};
static const u8 bulk_interface[23] = {
    0x09, 0x04, 0x02, 0x00, 0x02, 0xff, 0x00, 0x00, 0x00, /* interface 2, vendor class, two endpoints */
    0x07, 0x05, 0x02, 0x02, 0x00, 0x02, 0x00,             /* EP2 OUT, bulk, 512 */
    0x07, 0x05, 0x82, 0x02, 0x00, 0x02, 0x00};            /* EP2 IN, bulk, 512 */

/* ---- state ---- */
enum { B_FREE = 0, B_PRIMED, B_FULL, B_TAKEN };
enum { T_FREE = 0, T_RESERVED, T_QUEUED };

/* The descriptor stays for good (the port table entry points at it until the next boot). The receive and send buffers are allocated while the interface can be
 * live (buffers_alloc) and freed when the take-over ends (buffers_free); between, the adapter holds only this descriptor. */
static u8 desc_mem[DESC_BYTES + 16] __attribute__((aligned(16)));
static u8 *rx_area, *tx_area; /* the 4 KiB aligned buffers, as the CPU sees them (cached address); null while not live */
static u8 *blk_raw;           /* what the allocator returned (freed as it is) */
static volatile u32 live, alloc_busy, free_req, nomem;
/* for the emulator test: the last block allocated (kept after the free) */
volatile u32 usb_bulk_last_area, usb_bulk_last_bytes;
volatile u32 usb_bulk_free_why; /* why the last buffers_free refused: 1 a receive buffer is held, 2 a send slot is reserved, 4 EP2 not idle (the flush, ENDPTSTAT and ENDPTPRIME bits are in bits 8 and up), 8 a transfer of ours is queued, 16 allocating; 0 freed */
static volatile u8 rx_state[RX_BUFS];
static u32 rx_len[RX_BUFS];
static u8 order[RX_BUFS]; /* the full buffers in arrival order */
static u32 order_head, order_count;
static volatile u8 tx_state[TX_SLOTS];
static volatile u32 enabled, connected, resets;
/* `stopping`: ep2_stop is cancelling the queues; the callbacks it runs must not queue anything. `need_prime`: a receive buffer is free and the
 * engine refused its dTD (the pool was empty); the tick retries. `tx_retry`: a send was refused; the tick wakes the job. `qh_ready`: the EP2 queue
 * heads were set up by this adapter. */
static volatile u32 stopping, need_prime, tx_retry, qh_ready;
static u32 restart_due; /* enable_prepare found the device in USB MIDI mode: the first enable re-enumerates */
static volatile i32 io_job;
/* Take-over state. `taken`: a take-over is active. `to_req`: 0 none, 1 the switch to port 2 is requested, 2 the switch back is requested; the tick runs it when
 * `to_due_ms` have passed since it first saw the request. `to_orig`: the port to give back to. `to_active`: set by any transfer or renew (interrupt or task),
 * consumed by the tick, which stamps `to_act_at`. `to_gone`: the disconnect callback ran while taken over. `to_quiet`: usb_bulk_enable is re-enumerating
 * (its own stop runs our disconnect callback). Times are in clock units of the tick (blocks, or tick passes without a block clock; `to_per_s` per second). */
static volatile u32 taken, to_req, to_active, to_gone, to_quiet;
static u32 to_orig, to_due_ms, to_due_set, to_due_at, to_act_at, to_sus, to_sus_at, to_per_s, to_passes;
/* The switch guard. The firmware's mode switch has no lock: it writes the active port at once and restarts the device about 30 USB ticks later, and a second
 * stop or restart in that window is not safe. `to_sw_set`: a switch (the firmware's mode switch, or enable's own stop and restart) was made at `to_sw_at`
 * (clock units of `to_sw_per` a second). No further switch runs, and our disconnect callback records no host leaving, until TO_GUARD_MS have passed. All under
 * the interrupt mask. `to_pend_port`: the target of a switch the tick has claimed and not yet written (0: none), so takeover() sees the port the switch will leave. */
static u32 to_sw_set, to_sw_at, to_sw_per;
static volatile u32 to_pend_port;
static void (*stock_connect)(u32);
static u32 stock_free; /* the firmware's own free callback of a dTD (+0x38), which ours wraps */
static volatile u32 td_status; /* the status byte of the dTD whose free callback ran last (read by the completion callback that follows) */
/* Counters for the emulator test (read through the plan's symbols): the mode switches and restarts made so far, and the time between the last two in ms. */
volatile u32 usb_bulk_switches, usb_bulk_switch_gap_ms;
volatile u32 usb_bulk_rx_errors; /* transfers dropped for a halted, buffer-error or transaction-error status */
volatile u32 usb_bulk_rx_inval_lines; /* data cache lines invalidated by usb_bulk_take (0 with UB_CACHED_RX 0); the emulator has no data cache model */

/* UB_CACHED_RX 1: usb_bulk_take hands out the CACHED address of a receive buffer, after it invalidated the buffer's data cache lines, so the module's reads
 * fill whole 16-byte lines in bursts (the uncached alias costs one bus access per longword). 0: the uncached alias, as before (the A/B switch).
 * DEFAULT 0 until the emulator aliases DDR: its board keeps the cached window 0x40000000 and the uncached alias +0x08000000 in separate pages
 * (packages/emulator/machine/src/board.rs page_index), so the emulator's DMA data (written at the alias) reads as zeros at the cached address. Hardware: -DUB_CACHED_RX=1.
 * Rules, all in this file:
 *  1. The CPU never writes a receive buffer through its cached address. Only the USB DMA writes these buffers (enable writes only the descriptor, through the
 *     alias; the receive buffers are never written by the CPU). A dirty line could be written back over DMA data later. The cached address exists only as the
 *     result of rx_cached(), which only usb_bulk_take returns, and the module reads it (its parameter is documented read-only).
 *  2. usb_bulk_take invalidates the lines of the received bytes after the DMA completed (the completion callback marked the buffer full) and before the
 *     first CPU read. `cpushl dc,(Ax)` pushes a line if modified and invalidates it (CACR[DDPI] = 0, CFPRM CPUSHL p.8-2, MCF5441x RM 6.4.8 Table 6-10). The firmware
 *     writes CACR = 0xa50ce100 once (finding 07-emulator): SPA (bit 14) = 1 so Ax is a physical address and only the line that holds it is touched, IVO = 0,
 *     DDPI = 0. The lines are clean (rule 1), so the push writes nothing. The caches do not snoop DMA (RM 6.1).
 *  3. The cached address is returned only after the invalidate; usb_bulk_release takes either address. */
#ifndef UB_CACHED_RX
#define UB_CACHED_RX 0
#endif

static inline u32 sr_mask(void)
{
    u32 old;
    __asm__ volatile("move.w %%sr,%0\n\tmove.w #0x2700,%%sr" : "=d"(old) : : "memory", "cc");
    return old;
}

static inline void sr_restore(u32 old)
{
    __asm__ volatile("move.w %0,%%sr" : : "d"(old) : "memory", "cc");
}

static u8 *alias_of(u8 *p)
{
    return p + usb_alias_delta;
}

static u8 *rx_buf(u32 i)
{
    return alias_of(rx_area + i * RX_BYTES);
}

static u8 *tx_buf(u32 i)
{
    return alias_of(tx_area + i * TX_BYTES);
}

static u8 *desc_buf(void)
{
    return alias_of(desc_mem);
}

/* the index of the receive buffer that `p` points into (the uncached alias, or the cached address that usb_bulk_take may have returned), or -1 */
static i32 rx_index(const u8 *p)
{
    u32 off;
    if (!live)
        return -1;
    if ((u32)p < (u32)rx_buf(0))
        p += usb_alias_delta; /* the cached address of the same memory */
    off = (u32)(p - rx_buf(0));
    return off < RX_BUFS * RX_BYTES ? (i32)(off / RX_BYTES) : -1;
}

static i32 tx_index(const u8 *p)
{
    u32 off;
    if (!live)
        return -1;
    off = (u32)(p - tx_buf(0));
    return off < TX_SLOTS * TX_BYTES ? (i32)(off / TX_BYTES) : -1;
}

static volatile u32 *qh(u32 off)
{
    return (volatile u32 *)(QH_BASE + off);
}

static void wake(void)
{
    i32 j = io_job;
    if (j > 0)
        dkrt_io_wake(j);
}

/* The free callback of every dTD of ours (+0x38): note its status byte (the completion callback that follows reads it: the engine passes only the byte
 * count), then the firmware's own, which returns the dTD to the pool. The completion loop of the engine (vector_134_handler) reads the dTD's next pointer
 * after this callback and does not touch the dTD again after the completion callback, so a dTD handed out again by the completion callback is safe. */
void usb_bulk_td_free(u8 *dtd)
{
    td_status = dtd[7];
    ((void (*)(u8 *))stock_free)(dtd);
}

/* A dTD for `bytes` of `data` (pages from a 4 KiB aligned address) with our callbacks; queued on queue `index`. 0 when queued. The firmware's queue
 * function sets the interrupt-on-completion bit because a callback is set. Caller: interrupts masked.
 * Page pointers 1-4 always name the four pages after page 0, as RM 24-71 asks. The controller can use the pointer of the page where a transfer ends
 * (a zero-length packet after a full page); a zero pointer there made the DMA fault and wedged the controller on hardware (2026-11-11). The pages past
 * a short transfer are never read or written (the dTD's total bytes bound the data). The block is allocated with two pages after the area, so the pointers
 * of the last buffer (the fifth names the page after it) stay inside the block we own. */
static i32 queue_transfer(u32 index, u8 *data, u32 bytes, void *done, void *cancel)
{
    u32 *w = (u32 *)usb_fw_td_alloc();
    u32 k;
    if (w == 0)
        return -1;
    if (w[14] != (u32)usb_bulk_td_free)
        stock_free = w[14];
    w[0] = TD_END;
    w[1] = (bytes << 16) | TD_ACTIVE;
    w[2] = (u32)data;
    for (k = 1; k < 5; k++)
        w[2 + k] = (u32)data + k * PAGE;
    w[8] = bytes;        /* +0x20: the length the firmware subtracts the remainder from */
    w[9] = (u32)done;    /* +0x24 */
    w[10] = (u32)cancel; /* +0x28 */
    w[14] = (u32)usb_bulk_td_free; /* +0x38 */
    if (usb_fw_queue_td(index, (u8 *)w) < 0) {
        /* the engine refused (the endpoint is stalled): give the dTD back through the firmware's own free callback */
        ((void (*)(u8 *))stock_free)((u8 *)w);
        return -1;
    }
    return 0;
}

void usb_bulk_rx_done(u32 bytes, u8 *buf);
void usb_bulk_tx_done(u32 bytes, u8 *buf);
void usb_bulk_cancel(u32 bytes, u8 *buf);

/* EP2 IN is the audio clock feedback in USB AUDIO/MIDI: a dTD of ours there could fault the controller, so nothing is queued outside USB MIDI mode */
static int usb_midi_mode(void)
{
    return *(volatile u32 *)usb_active_port == PORT_ENTRY;
}

static void prime_one(u32 i)
{
    if (stopping || !live || !connected || !usb_midi_mode())
        return;
    if (queue_transfer(IDX_OUT, rx_buf(i), RX_BYTES, (void *)usb_bulk_rx_done, (void *)usb_bulk_cancel) == 0)
    {
        rx_state[i] = B_PRIMED;
        UB_LOG("ub.rx.prime", i);
    }
}

/* queue every free receive buffer; a buffer the engine refused leaves `need_prime` set for the tick and the module's next call */
static void prime_all(void)
{
    u32 i, left = 0;
    if (stopping || !live || !connected)
        return;
    if (!usb_midi_mode()) {
        need_prime = 1; /* resumes when the mode is back and the firmware reconnects */
        return;
    }
    for (i = 0; i < RX_BUFS; i++) {
        if (rx_state[i] == B_FREE)
            prime_one(i);
        if (rx_state[i] == B_FREE)
            left = 1;
    }
    need_prime = left;
}

/* ---- the dTD callbacks ---- */

/* EP2 OUT transfer complete (interrupt): `bytes` arrived in `buf`. A zero-length packet (the end of a message that was a multiple of 512 bytes
 * after a full buffer) holds no data: the buffer is queued again. A status with an error bit drops the transfer. */
void usb_bulk_rx_done(u32 bytes, u8 *buf)
{
    u32 sr = sr_mask();
    i32 i = rx_index(buf);
    u32 bad = td_status & (TD_HALTED | TD_BUFERR | TD_XACTERR);
    td_status = 0;
    to_active = 1; /* host activity for the take-over watchdog */
    UB_LOG("ub.rx.done", bytes | ((u32)i << 24));
    if (i >= 0 && rx_state[i] == B_PRIMED) {
        if (bad != 0u) {
            usb_bulk_rx_errors++;
            UB_LOG("ub.rx.err", bad);
            rx_state[i] = B_FREE;
            prime_one((u32)i);
        } else if (bytes == 0) {
            rx_state[i] = B_FREE;
            prime_one((u32)i);
        } else {
            rx_len[i] = bytes;
            rx_state[i] = B_FULL;
            order[(order_head + order_count) % RX_BUFS] = (u8)i;
            order_count++;
            prime_all();
            wake();
        }
        if (rx_state[i] == B_FREE)
            need_prime = 1;
    }
    sr_restore(sr);
}

/* EP2 IN transfer complete (interrupt): the slot is free; a step waiting for one is woken. */
void usb_bulk_tx_done(u32 bytes, u8 *buf)
{
    u32 sr = sr_mask();
    i32 i = tx_index(buf);
    (void)bytes;
    to_active = 1;
    UB_LOG("ub.tx.done", bytes);
    td_status = 0;
    if (i >= 0) {
        tx_state[i] = T_FREE;
        wake();
    }
    sr_restore(sr);
}

/* A queued transfer was cancelled (ep2_stop): the buffer is free again, nothing arrived and nothing is queued in its place. */
void usb_bulk_cancel(u32 bytes, u8 *buf)
{
    u32 sr = sr_mask();
    i32 i = rx_index(buf);
    (void)bytes;
    UB_LOG("ub.cancel", bytes);
    td_status = 0;
    if (i >= 0) {
        if (rx_state[i] == B_PRIMED)
            rx_state[i] = B_FREE;
    } else {
        i = tx_index(buf);
        if (i >= 0)
            tx_state[i] = T_FREE;
    }
    sr_restore(sr);
}

/* ---- the firmware's callbacks of port entry 2 ---- */

/* The EP2 queue heads and the engine's lists of queued dTDs (+0x30 head, +0x34 tail), empty. */
static void ep2_lists_clear(void)
{
    volatile u32 *q = qh(QH_EP2_OUT);
    q[1] = 0;
    q[2] = TD_END;
    q[3] = 0;
    q[0x30 / 4] = 0;
    q[0x34 / 4] = 0;
    q = qh(QH_EP2_IN);
    q[1] = 0;
    q[2] = TD_END;
    q[3] = 0;
    q[0x30 / 4] = 0;
    q[0x34 / 4] = 0;
}

/* Flush EP2 with a bounded poll. 1 when the controller has finished the flush. */
static u32 ep2_flush(void)
{
    u32 spins;
    USB_REG(ENDPTFLUSH) = FLUSH_EP2;
    for (spins = 0; spins < FLUSH_SPINS && (USB_REG(ENDPTFLUSH) & FLUSH_EP2) != 0u; spins++)
        ;
    return (USB_REG(ENDPTFLUSH) & FLUSH_EP2) == 0u;
}

/* No transfer of EP2 is flushing, primed or active in the controller (both directions). */
static u32 ep2_idle(void)
{
    return ((USB_REG(ENDPTFLUSH) | USB_REG(ENDPTSTAT) | USB_REG(ENDPTPRIME)) & FLUSH_EP2) == 0u;
}

/* EP2 off: flush with a bounded poll, cancel both queues (their callbacks run with `stopping` set: they queue nothing), then empty the lists. Every buffer
 * that was queued is free again; full buffers that nobody took belong to the connection that ended and are dropped; a taken buffer stays with its
 * owner. The reset count changes and the job is woken. Interrupts masked by the caller. */
static void ep2_stop(void)
{
    u32 i;
    UB_LOG("ub.ep2stop", resets);
    stopping = 1;
    if (!qh_ready)
        ep2_lists_clear(); /* the engine's init cleared the queue heads, but another USB mode (Overbridge also uses EP2) may have left words */
    USB_REG(ENDPTCTRL2) = 0;
    ep2_flush();
    usb_fw_cancel_ep(IDX_OUT);
    usb_fw_cancel_ep(IDX_IN);
    ep2_lists_clear();
    USB_REG(ENDPTCOMPLETE) = FLUSH_EP2; /* write 1 to clear: a completion of the old connection must not be seen by the engine's loop */
    for (i = 0; i < RX_BUFS; i++)
        if (rx_state[i] != B_TAKEN)
            rx_state[i] = B_FREE;
    order_head = order_count = 0;
    for (i = 0; i < TX_SLOTS; i++)
        if (tx_state[i] != T_RESERVED) /* a send is copying into this slot (usb_bulk_send, interrupts on): it frees the slot itself when it sees the reset */
            tx_state[i] = T_FREE;
    need_prime = 0;
    qh_ready = 1;
    if (connected) {
        resets++;
        connected = 0;
        wake();
    }
    stopping = 0;
}

/* Take every queued transfer of EP2 back without disabling the endpoint (the host gets NAK; the descriptor keeps the interface): flush, cancel, empty lists. Receive
 * buffers held by the module and send slots being filled stay as they are. Interrupts masked by the caller; EP2 must be ours (`connected`). */
static void ep2_quiesce(void)
{
    u32 i;
    stopping = 1;
    ep2_flush();
    usb_fw_cancel_ep(IDX_OUT);
    usb_fw_cancel_ep(IDX_IN);
    ep2_lists_clear();
    USB_REG(ENDPTCOMPLETE) = FLUSH_EP2;
    for (i = 0; i < RX_BUFS; i++)
        if (rx_state[i] != B_TAKEN)
            rx_state[i] = B_FREE;
    order_head = order_count = 0;
    for (i = 0; i < TX_SLOTS; i++)
        if (tx_state[i] != T_RESERVED)
            tx_state[i] = T_FREE;
    need_prime = 0;
    stopping = 0;
}

/* the host's bus reset or resume: the stock work of the port (EP1 for MIDI), then EP2 */
void usb_bulk_connect(void)
{
    u32 sr = sr_mask();
    UB_LOG("ub.connect", resets);
    ep2_stop();
    stock_connect(1); /* the wrapper reads and sets its argument slot */
    qh(QH_EP2_OUT)[0] = QH_CAPS_OUT;
    qh(QH_EP2_IN)[0] = QH_CAPS_IN;
    if ((USB_REG(ENDPTSTAT) & FLUSH_EP2) != 0u)
        ep2_flush(); /* the controller still holds a transfer of the old connection */
    USB_REG(ENDPTCTRL2) = CTRL_BULK;
    connected = 1;
    prime_all();
    sr_restore(sr);
}

static u32 sw_guard_open(void); /* take-over section */

/* the host is gone or the mode switches: EP2 first, then the stock callback */
void usb_bulk_disconnect(void)
{
    u32 sr = sr_mask();
    UB_LOG("ub.disconn", resets);
    if (taken && !to_quiet && sw_guard_open())
        to_gone = 1; /* unplug, suspend or a mode change while taken over: the tick decides (this runs in the USB interrupt) */
    ep2_stop();
    sr_restore(sr);
    usb_fw_disconnect();
}

#if UB_TRACE
/* the controller registers and the EP2 queue head words, logged when they change (read only: USBSTS is write 1 to clear) */
#define UB_SNAP_N 10u
static void ub_snapshot(void)
{
    static u32 last[UB_SNAP_N];
    static const char *const tags[UB_SNAP_N] = {"ub.USBSTS", "ub.PORTSC", "ub.USBCMD", "ub.EPPRIME", "ub.EPSTAT",
                                                "ub.EPCOMPL", "ub.qhO.tok", "ub.qhI.tok", "ub.qhO.cur", "ub.qhI.cur"};
    u32 v[UB_SNAP_N];
    u32 i;
    v[0] = USB_REG(USBSTS);
    v[1] = USB_REG(PORTSC);
    v[2] = USB_REG(USBCMD);
    v[3] = USB_REG(ENDPTPRIME);
    v[4] = USB_REG(ENDPTSTAT);
    v[5] = USB_REG(ENDPTCOMPLETE);
    v[6] = *(volatile u32 *)(QH_BASE + QH_EP2_OUT + 0x0cu);
    v[7] = *(volatile u32 *)(QH_BASE + QH_EP2_IN + 0x0cu);
    v[8] = *(volatile u32 *)(QH_BASE + QH_EP2_OUT + 0x04u);
    v[9] = *(volatile u32 *)(QH_BASE + QH_EP2_IN + 0x04u);
    for (i = 0; i < UB_SNAP_N; i++) {
        if (v[i] != last[i]) {
            last[i] = v[i];
            ub_log(tags[i], v[i]);
        }
    }
}
#endif

/* ---- the buffers: allocated when the interface can go live, freed when the take-over ends ---- */

/* The block: RX_BUFS receive buffers, then TX_SLOTS send slots, 4 KiB aligned, and two pages after them (the dTD page pointers of the last buffer). The firmware's
 * buddy allocator (heap at 0x44f39c40 on the DT2: not 4 KiB aligned itself) gives a block that is 16 byte aligned: the area is rounded up inside it. The allocator
 * rounds the size up to a power of two (256 KiB), the cost while live. The firmware's operator new and free take the heap mutex themselves (the anchors
 * cf.mem.site.heap.lock_lit and unlock_lit lie inside cf.mem.fn.new; finding 01), so they may be called from the I/O, MIDI and UI tasks, never from an interrupt. */
#define BLOCK_AREA (RX_BUFS * RX_BYTES + TX_SLOTS * TX_BYTES)
#define BLOCK_BYTES (BLOCK_AREA + 2u * PAGE)

/* Allocate the buffers (task context, never under the mask or in an interrupt). 0 when live (also when it already was). E_STATE: another caller is allocating.
 * E_NOMEM: no memory, or the block is outside the cached DDR window the alias arithmetic needs (ST_NOMEM shows it; nothing is changed). The area is pushed
 * out of the data cache (the allocator's earlier owner may have left dirty lines) and used through the alias only. When EP2 is up already (the host connected
 * before the buffers existed, or after they were freed), the buffers are queued at once, under the mask. */
static i32 buffers_alloc(void)
{
    u32 sr, i;
    u8 *raw, *a;
    sr = sr_mask();
    if (live) {
        sr_restore(sr);
        return 0;
    }
    if (alloc_busy) {
        sr_restore(sr);
        return E_STATE;
    }
    alloc_busy = 1;
    sr_restore(sr);
    raw = usb_fw_mem_new(BLOCK_BYTES);
    a = (u8 *)(((u32)raw + PAGE - 1u) & ~(PAGE - 1u));
    if (raw == 0 || (u32)a < usb_ddr_base || (u32)a + BLOCK_AREA + PAGE > usb_ddr_base + usb_ddr_bytes) {
        if (raw != 0)
            usb_fw_mem_free(raw);
        nomem = 1;
        alloc_busy = 0;
        return E_NOMEM;
    }
    for (i = 0; i < BLOCK_AREA; i += 16u)
        __asm__ volatile("cpushl %%bc,(%0)" : : "a"((u32)a + i) : "memory");
    usb_bulk_last_area = (u32)a;
    usb_bulk_last_bytes = BLOCK_AREA;
    sr = sr_mask();
    blk_raw = raw;
    rx_area = a;
    tx_area = a + RX_BUFS * RX_BYTES;
    for (i = 0; i < RX_BUFS; i++)
        rx_state[i] = B_FREE;
    for (i = 0; i < TX_SLOTS; i++)
        tx_state[i] = T_FREE;
    order_head = order_count = 0;
    need_prime = 0;
    nomem = 0;
    live = 1;
    alloc_busy = 0;
    if (connected && usb_midi_mode())
        prime_all();
    sr_restore(sr);
    return 0;
}

/* Free the buffers when nothing can touch them (all under the mask, so neither the interrupt nor a task changes anything meanwhile):
 *  - no receive buffer is held by the module (B_TAKEN) and no send slot is being filled (T_RESERVED): those are the CPU's, nothing is freed yet;
 *  - EP2 is ours (`connected`: the host configured it and no disconnect ran since): its transfers are taken back without disabling the endpoint (ep2_quiesce:
 *    flush, cancel, empty lists), and the controller must then report EP2 idle: ENDPTFLUSH clear and neither primed nor active in ENDPTPRIME and ENDPTSTAT,
 *    both directions. If not, nothing is freed and the tick asks again (`need_prime` re-queues the buffers meanwhile);
 *  - EP2 is not ours (no connect since the last disconnect, which ran ep2_stop: ENDPTCTRL2 off, queues cancelled): the endpoint is not touched, because it may
 *    belong to the firmware (the audio clock feedback of USB AUDIO/MIDI); it is enough that none of our transfers is queued.
 * Then `live` is 0 (every queue path tests it) and the block is freed. Returns 1 when the buffers are gone. A free that waits for the allocation in progress
 * keeps `free_req`. */
static u32 buffers_free(void)
{
    u32 sr, i;
    u8 *raw;
    sr = sr_mask();
    if (alloc_busy) {
        usb_bulk_free_why = 16;
        sr_restore(sr);
        return 0;
    }
    if (!live) {
        free_req = 0;
        sr_restore(sr);
        return 1;
    }
    for (i = 0; i < RX_BUFS; i++)
        if (rx_state[i] == B_TAKEN) {
            usb_bulk_free_why = 1;
            sr_restore(sr);
            return 0;
        }
    for (i = 0; i < TX_SLOTS; i++)
        if (tx_state[i] == T_RESERVED) {
            usb_bulk_free_why = 2;
            sr_restore(sr);
            return 0;
        }
    if (connected) {
        ep2_quiesce();
        if (!ep2_idle()) {
            usb_bulk_free_why = 4 | (((USB_REG(ENDPTFLUSH) & FLUSH_EP2) != 0u ? 1u : 0u) | ((USB_REG(ENDPTSTAT) & FLUSH_EP2) != 0u ? 2u : 0u) | ((USB_REG(ENDPTPRIME) & FLUSH_EP2) != 0u ? 4u : 0u)) << 8;
            need_prime = 1;
            sr_restore(sr);
            return 0;
        }
    } else {
        for (i = 0; i < RX_BUFS; i++)
            if (rx_state[i] == B_PRIMED) {
                usb_bulk_free_why = 8;
                sr_restore(sr);
                return 0;
            }
        for (i = 0; i < TX_SLOTS; i++)
            if (tx_state[i] == T_QUEUED) {
                usb_bulk_free_why = 8;
                sr_restore(sr);
                return 0;
            }
    }
    usb_bulk_free_why = 0;
    live = 0;
    raw = blk_raw;
    blk_raw = 0;
    rx_area = tx_area = 0;
    for (i = 0; i < RX_BUFS; i++)
        rx_state[i] = B_FREE;
    for (i = 0; i < TX_SLOTS; i++)
        tx_state[i] = T_FREE;
    order_head = order_count = 0;
    need_prime = 0;
    free_req = 0;
    sr_restore(sr);
    usb_fw_mem_free(raw);
    return 1;
}

/* ---- take-over ---- */

/* Guard after every mode switch. The firmware restarts the device after a delay of 30 ticks of its USB tick (FUN_4000713e counts the word of
 * cf.usb.value.restart_delay down from 30 and restarts at 0). Measured in the emulator on both images: about 1 s (33 ms a tick). That word is the exact answer to "is a restart
 * pending" and does not depend on any clock of ours, so the guard is open only when it is 0 (fw_restart_pending). TO_GUARD_MS is a floor on our own clock from the
 * stamp: it covers the moment between the stamp and the firmware setting the word, and a word that cannot be read. */
#define TO_GUARD_MS 500u
#define TO_DELAY_MS 50u      /* a requested switch runs this long after the tick first saw it (the reply is out by then) */
#define TO_IDLE_MS 30000u    /* no host activity for this long: give back */
#define TO_SUSPEND_MS 500u   /* the bus stayed suspended this long: give back */
#define TO_QUIET_MS 2000u    /* the suspend check starts this long after the last activity (a switch counts as activity) */

/* the tick clock: the runtime's block clock (1500 a second), or the passes of the UI loop (about 120 a second) without one, as core/ui does */
static u32 to_peek(u32 *per_s)
{
    u32 now = dkrt_blocks();
    if (now != 0u) {
        *per_s = 1500u;
        return now;
    }
    *per_s = 120u;
    return to_passes;
}

/* the tick's clock: one more pass of the UI loop when there is no block clock */
static u32 to_clock(u32 *per_s)
{
    to_passes++;
    return to_peek(per_s);
}

static u32 to_elapsed_ms(u32 since, u32 now, u32 per_s)
{
    u32 d = now - since;
    return d / per_s * 1000u + d % per_s * 1000u / per_s;
}

/* The firmware's restart delay (anchor cf.usb.value.restart_delay): 30 down to 0 by the USB tick, non-zero while a stop is waiting to restart. A value over
 * 30 is not the counter and counts as no restart pending. */
#define FW_RESTART_TICKS 30u

static u32 fw_restart_pending(void)
{
    u32 d = *(volatile u32 *)usb_restart_delay;
    return d != 0u && d <= FW_RESTART_TICKS;
}

/* Is a new switch allowed? Interrupts masked by the caller. When the clock source changed since the stamp the guard starts again. */
static u32 sw_guard_open(void)
{
    u32 per, now;
    if (to_pend_port != 0u || fw_restart_pending())
        return 0;
    if (!to_sw_set)
        return 1;
    now = to_peek(&per);
    if (per != to_sw_per) {
        to_sw_per = per;
        to_sw_at = now;
        return 0;
    }
    return to_elapsed_ms(to_sw_at, now, per) >= TO_GUARD_MS;
}

/* record a switch made now. Interrupts masked by the caller. */
static void sw_stamp(void)
{
    u32 per, now = to_peek(&per);
    if (to_sw_set && per == to_sw_per)
        usb_bulk_switch_gap_ms = to_elapsed_ms(to_sw_at, now, per);
    to_sw_set = 1;
    to_sw_at = now;
    to_sw_per = per;
    restart_due = 0; /* every switch connects the port again with entry 2 as it is now */
    usb_bulk_switches++;
}

/* the take-over ends without a switch (the user changed the mode, or the switch was never needed) */
static void to_drop(void)
{
    free_req = 1; /* the tick frees the buffers once nothing holds them (buffers_free) */
    taken = 0;
    to_req = 0;
    to_gone = 0;
    to_sus = 0;
}

/* The take-over's decision for this tick: updates the intervals and the request, and returns the port to switch to, or 0 for no switch. `port` is the active
 * port (or the target of a claimed switch), `guard` the result of sw_guard_open. A target equal to `port` is a request that needs no switch (it is dropped).
 * Interrupts masked by the caller. */
static u32 takeover_decide(u32 now, u32 per_s, u32 port, u32 guard)
{
    if (per_s != to_per_s) { /* the clock source changed: restart the intervals */
        to_per_s = per_s;
        to_act_at = now;
        to_due_set = 0;
        to_sus = 0;
    }
    if (to_active) {
        to_active = 0;
        to_act_at = now;
    }
    /* once switched, the active port stays USB MIDI until we switch back: any other value is the user's choice and wins */
    if (to_req != 1u && port != PORT_ENTRY) {
        to_drop();
        return 0;
    }
    if (to_req == 0u && !guard) { /* our own switch is settling: its disconnects and the bus suspend are not a host leaving */
        to_gone = 0;
        to_sus = 0;
        return 0;
    }
    if (to_req == 0u) { /* the automatic give-back: the host left, no activity for TO_IDLE_MS, or the bus stayed suspended */
        if (to_gone) {
            to_gone = 0;
            to_req = 2;
        } else if (to_elapsed_ms(to_act_at, now, per_s) >= TO_IDLE_MS) {
            to_req = 2;
        } else if ((USB_REG(PORTSC) & PORTSC_SUSP) != 0u && to_elapsed_ms(to_act_at, now, per_s) >= TO_QUIET_MS) {
            if (!to_sus) {
                to_sus = 1;
                to_sus_at = now;
            } else if (to_elapsed_ms(to_sus_at, now, per_s) >= TO_SUSPEND_MS) {
                to_req = 2;
            }
        } else {
            to_sus = 0;
        }
        if (to_req == 2u) { /* no delay */
            to_due_ms = 0;
            to_due_set = 0;
        }
    }
    if (to_req == 0u)
        return 0;
    if (!to_due_set) {
        to_due_set = 1;
        to_due_at = now;
    }
    if (to_elapsed_ms(to_due_at, now, per_s) < to_due_ms || !guard) /* a request waits for its delay and for the guard: one switch at a time */
        return 0;
    if (to_req == 1u) {
        if (port != to_orig) { /* the mode changed before the switch ran: nothing to take over */
            to_drop();
            return 0;
        }
        to_req = 0;
        to_active = 1; /* the switch counts as activity */
        to_sus = 0;
        return PORT_ENTRY;
    }
    to_drop();
    return to_orig;
}

/* Called from usb_bulk_tick, the UI task: decide under the mask, then run the switch. */
static void takeover_tick(void)
{
    u32 per_s, now, sr, port, target;
    if (!taken)
        return;
    /* the switch to USB MIDI connects port 2 from the USB interrupt, which primes the buffers: they must be there before. A failure ends the take-over (ST_NOMEM) */
    if (to_req == 1u && !live) {
        i32 r = buffers_alloc();
        if (r == E_NOMEM) {
            sr = sr_mask();
            to_drop();
            sr_restore(sr);
        }
        if (r != 0)
            return; /* E_STATE: the job is allocating; the next pass looks again */
    }
    now = to_clock(&per_s);
    sr = sr_mask();
    port = to_pend_port != 0u ? to_pend_port : *(volatile u32 *)usb_active_port;
    target = takeover_decide(now, per_s, port, sw_guard_open());
    if (target != 0u && target != port) {
        /* claimed under the mask: from here a takeover() or giveback() on the MIDI task sees the new state, and any further request waits for the guard */
        sw_stamp();
        to_pend_port = target;
    }
    sr_restore(sr);
    if (target != 0u && target != port) {
        usb_fw_mode_switch(target); /* interrupts on, task context: the firmware's own menu action does the same */
        to_pend_port = 0; /* the firmware has written the active port */
    }
}

/* The job is woken every HK_WAKE_MS while the interface is enabled: the module has no timer of its own (a retry of usb_bulk_enable while the mode switch settles,
 * a check for an idle session). */
#define HK_WAKE_MS 250u
static u32 hk_set, hk_at, hk_per, hk_passes;

static void housekeeping_wake(void)
{
    u32 per = 1500u, now = dkrt_blocks();
    if (io_job <= 0)
        return;
    if (now == 0u) { /* no block clock: count the passes of the UI loop (about 120 a second) */
        per = 120u;
        now = ++hk_passes;
    }
    if (hk_set && per == hk_per && to_elapsed_ms(hk_at, now, per) < HK_WAKE_MS)
        return;
    hk_set = 1;
    hk_at = now;
    hk_per = per;
    wake();
}

/* once a pass of the UI loop (dkrt_poll_add): the take-over, then retry what the engine refused */
void usb_bulk_tick(void)
{
    u32 sr, again;
#if UB_TRACE
    ub_snapshot();
#endif
    takeover_tick();
    if (free_req && !taken)
        buffers_free();
    if (!enabled)
        return;
    housekeeping_wake();
    sr = sr_mask();
    if (need_prime)
        prime_all();
    again = tx_retry;
    tx_retry = 0;
    sr_restore(sr);
    if (again)
        wake();
}

/* ---- the interface for modules ---- */

/* The stock callback of the entry must be the firmware's own: `moveq #1,d0; move.l d0,4(sp); bra.w FUN_400043e2`, and the branch lands on the
 * connect body of the anchor. Anything else is a firmware this adapter has not been checked against. */
static int stock_connect_ok(u32 fn)
{
    const u8 *c = (const u8 *)fn;
    i32 disp;
    if (c[0] != 0x70 || c[1] != 0x01 || c[2] != 0x2f || c[3] != 0x40 || c[4] != 0x00 || c[5] != 0x04 || c[6] != 0x60 || c[7] != 0x00)
        return 0;
    disp = (i32)(short)((c[8] << 8) | c[9]);
    return fn + 8u + (u32)disp == (u32)usb_fw_connect_body; /* bra.w: from the displacement word */
}

/* Put the tick on the runtime's list (UI task or MIDI task). The runtime's dkrt_poll_add is idempotent (a function that is in the table returns OK) but does an unlocked
 * scan and store on a 4-entry table: it is called under the mask here, and once only, so the UI loop (which reads the table, a word at a time) never sees a half write. */
static u32 tick_added;

static i32 tick_register(void)
{
    u32 sr;
    i32 r = 0;
    if (tick_added)
        return 0;
    sr = sr_mask();
    if (!tick_added) {
        r = dkrt_poll_add((u32)usb_bulk_tick);
        if (r == 0)
            tick_added = 1;
    }
    sr_restore(sr);
    return r;
}

/* Make entry 2 (USB MIDI) ours: the 124-byte descriptor, our connect and disconnect callbacks, the buffers. Writes only the port table entry and our own state,
 * so it is safe whatever the active port is: the firmware reads the entry when it connects. `restart_due`: the device is in USB MIDI mode and no switch is
 * settling, so the host has seen the stock descriptor and the caller must re-enumerate; in any other case the next connect of port 2 (a switch that is
 * settling, or a later one) already uses the new entry. */
static i32 enable_prepare(i32 io_id)
{
    u32 *e = (u32 *)(usb_port_table + PORT_ENTRY * PORT_ENTRY_BYTES);
    const u8 *cfg = (const u8 *)e[3];
    u32 sr, i;
    u8 *d;
    /* the descriptor must lie in the cached DDR window: the alias is that window plus a constant (the buffers are checked when they are allocated) */
    if ((u32)desc_mem < usb_ddr_base || (u32)desc_mem + sizeof desc_mem > usb_ddr_base + usb_ddr_bytes)
        return E_CONTRACT;
    /* the stock entry, byte for byte (the firmware writes wTotalLength into its own array at run time, FUN_4000677a, through absolute addresses and not through
     * this table: bytes 2 and 3 are free) */
    if (e[4] != STOCK_CONFIG_BYTES || e[9] != 1u || e[1] != (u32)usb_fw_disconnect || !stock_connect_ok(e[0]))
        return E_CONTRACT;
    for (i = 0; i < STOCK_CONFIG_BYTES; i++)
        if (i != 2u && i != 3u && cfg[i] != stock_config[i])
            return E_CONTRACT;
    if (tick_register() != 0)
        return E_NOMEM;
    /* the descriptor is bss: the installer may have zeroed it through the data cache, and a dirty line written back later would overwrite the descriptor
     * below. Push and invalidate every line first (the alias is another address for the same memory, to the cache), then write through the alias only. */
    for (i = 0; i < sizeof desc_mem; i += 16u)
        __asm__ volatile("cpushl %%bc,(%0)" : : "a"((u32)desc_mem + i) : "memory");
    d = desc_buf();
    for (i = 0; i < STOCK_CONFIG_BYTES; i++)
        d[i] = cfg[i];
    for (i = 0; i < 23u; i++)
        d[STOCK_CONFIG_BYTES + i] = bulk_interface[i];
    d[2] = DESC_BYTES; /* wTotalLength, little endian */
    d[3] = 0;
    d[4] = 3; /* bNumInterfaces */
    qh_ready = 0;
    io_job = io_id;
    sr = sr_mask();
    restart_due = usb_midi_mode() && sw_guard_open();
    stock_connect = (void (*)(u32))e[0];
    e[3] = (u32)d;
    e[4] = DESC_BYTES;
    e[0] = (u32)usb_bulk_connect;
    e[1] = (u32)usb_bulk_disconnect;
    enabled = 1;
    sr_restore(sr);
    return 0;
}

/* The caller (the module's I/O job) calls this until it returns 0. The first call prepares entry 2 whatever the mode (see enable_prepare), so a take-over
 * from USB AUDIO/MIDI enumerates once, with the bulk interface already in the descriptor. E_STATE: the device is not in USB MIDI mode, or a switch is
 * settling (the firmware restarts the device a second after it, and the guard keeps a second stop out); call again later. */
i32 usb_bulk_enable(i32 io_id)
{
    u32 sr, ok;
    i32 r;
    if (io_job > 0 && io_job != io_id)
        return E_BUSY; /* one owner */
    if (!enabled) {
        r = enable_prepare(io_id);
        if (r != 0)
            return r;
    }
    io_job = io_id;
    /* the bulk pair lives in the USB MIDI configuration; USB AUDIO/MIDI uses EP2 IN for the audio clock feedback
     * (checked also when the interface is enabled already, so a caller never sees success outside USB MIDI mode) */
    if (!usb_midi_mode())
        return E_STATE;
    /* The device is in USB MIDI mode: the buffers are needed from the next connect on. They are allocated here, not in the AUDIO/MIDI call that only prepared the
     * entry (a take-over allocates them in the tick before its switch). A device that connected before the buffers existed (the user switched mode, or the
     * buffers were freed after a take-over) has EP2 up already: buffers_alloc queues them, no re-enumeration. A switch that is settling is refused before
     * anything is allocated. A failure (E_NOMEM, ST_NOMEM) changes nothing. */
    sr = sr_mask();
    ok = sw_guard_open();
    sr_restore(sr);
    if (!ok)
        return E_STATE;
    r = buffers_alloc();
    if (r != 0)
        return r;
    free_req = 0; /* an enable is a new request for the buffers: a free asked for by an earlier disable or take-over end must not take them back (the tick would free
                   * the block this call has just allocated) */
    /* a switch is settling: the caller retries. Also when enabled already, so a caller never sees success in the middle of one */
    sr = sr_mask();
    ok = sw_guard_open();
    if (ok && restart_due) {
        restart_due = 0;
        sw_stamp(); /* the stop and restart below are a switch */
        to_quiet = 1; /* the stop runs our disconnect callback: not a host leaving */
    } else if (ok) {
        sr_restore(sr);
        return 0;
    }
    sr_restore(sr);
    if (!ok)
        return E_STATE;
    /* the firmware's own USB mode switch (FUN_40006c16 does the same from the task context, interrupts on): stop (calls the disconnect callback and
     * starts a delay), restart (the tick restarts the device when the delay has run); the host sees the device go and come back with interface 2 */
    usb_fw_stop();
    to_quiet = 0; /* only the stop above is our own: a real unplug from here on counts (after the guard) */
    usb_fw_restart();
    to_active = 1;
    return 0;
}

/* the job that enabled the interface goes away (the module's fini): no more wakes. The table stays as it is until the next boot. */
i32 usb_bulk_disable(i32 io_id)
{
    u32 sr, i;
    if (io_id <= 0 || io_job != io_id)
        return E_STATE;
    sr = sr_mask();
    io_job = 0;
    /* the job is going away (the runtime waits for its running step before fini, then cancels it): what it still holds is released, so the free can happen */
    for (i = 0; i < RX_BUFS; i++)
        if (rx_state[i] == B_TAKEN)
            rx_state[i] = B_FREE;
    for (i = 0; i < TX_SLOTS; i++)
        if (tx_state[i] == T_RESERVED)
            tx_state[i] = T_FREE;
    need_prime = 1;
    free_req = 1; /* the tick frees the buffers (once no take-over is on) */
    sr_restore(sr);
    return 0;
}

i32 usb_bulk_take(u8 **buf, u32 *len)
{
    u32 sr = sr_mask();
    i32 r = 0;
    if (need_prime)
        prime_all();
    if (order_count != 0) {
        u32 i = order[order_head];
        order_head = (order_head + 1u) % RX_BUFS;
        order_count--;
        rx_state[i] = B_TAKEN;
        *buf = rx_buf(i);
        *len = rx_len[i];
        r = 1;
    }
    sr_restore(sr);
#if UB_CACHED_RX
    if (r == 1) { /* the buffer is ours (B_TAKEN, the DMA is done): invalidate with interrupts on, then hand out the cached address (rules 2 and 3) */
        u32 a = (u32)*buf - usb_alias_delta, n = (*len + 15u) & ~15u, k;
        for (k = 0; k < n; k += 16u)
            __asm__ volatile("cpushl %%dc,(%0)" : : "a"(a + k) : "memory");
        usb_bulk_rx_inval_lines += n / 16u;
        *buf = (u8 *)a;
    }
#endif
    return r;
}

i32 usb_bulk_release(u8 *buf)
{
    u32 sr = sr_mask();
    i32 i = rx_index(buf); /* -1 when the buffers are gone */
    i32 r = E_STATE;
    if (i >= 0 && rx_state[i] == B_TAKEN) {
        rx_state[i] = B_FREE;
        prime_all();
        r = 0;
    }
    sr_restore(sr);
    return r;
}

/* A slot is reserved under the mask, the bytes are copied with interrupts on (16 KiB is long), and the dTD is queued under the mask again. */
i32 usb_bulk_send(const u8 *buf, u32 len)
{
    u32 sr, i, k, before;
    u8 *p;
    i32 slot = -1, r;
    if (len == 0u || len > TX_BYTES)
        return E_LENGTH;
    sr = sr_mask();
    if (!live || !connected || stopping || !usb_midi_mode()) {
        sr_restore(sr);
        return E_STATE;
    }
    if (need_prime)
        prime_all();
    for (i = 0; i < TX_SLOTS; i++) {
        if (tx_state[i] == T_FREE) {
            tx_state[i] = T_RESERVED;
            slot = (i32)i;
            p = tx_buf(i); /* under the mask: the slot is reserved, so the buffers cannot be freed under the copy */
            break;
        }
    }
    before = resets;
    sr_restore(sr);
    if (slot < 0)
        return E_BUSY; /* both slots are in flight: their completion wakes the job */
    for (k = 0; k < len; k++)
        p[k] = buf[k];
    sr = sr_mask();
    if (!live || !connected || stopping || resets != before) {
        tx_state[slot] = T_FREE; /* the bus was reset while copying (ep2_stop leaves a reserved slot to us) */
        r = E_STATE;
    } else if (queue_transfer(IDX_IN, p, len, (void *)usb_bulk_tx_done, (void *)usb_bulk_cancel) != 0) {
        tx_state[slot] = T_FREE;
        tx_retry = 1;
        r = E_AGAIN;
    } else {
        tx_state[slot] = T_QUEUED;
        UB_LOG("ub.tx.queue", len | ((u32)slot << 24));
        r = 0;
    }
    sr_restore(sr);
    return r;
}

/* Take the USB port over for the bulk link, or renew a take-over (host activity for the watchdog). In USB AUDIO/MIDI (port 5) it records the original port and the
 * tick switches to USB MIDI about TO_DELAY_MS later; in USB MIDI it succeeds without switching. Refused (E_STATE) in Overbridge (port 6) and any other port.
 * A pending give-back is cancelled. Callable from the MIDI task (a command handler): it only sets state; the switch runs in the tick.
 * The caller then calls usb_bulk_enable until it stops returning E_STATE. */
i32 usb_bulk_takeover(void)
{
    u32 sr = sr_mask();
    u32 port = to_pend_port != 0u ? to_pend_port : *(volatile u32 *)usb_active_port; /* the port a claimed switch leaves */
    i32 r = 0;
    if (taken) {
        if (to_req == 2u)
            to_req = 0;
        to_active = 1;
    } else if (port != PORT_ENTRY && port != PORT_AUDIO) {
        r = E_STATE; /* Overbridge (6) or a service mode */
    } else {
        taken = 1;
        free_req = 0; /* a new session: keep the buffers if they are still there */
        to_orig = port;
        to_gone = 0;
        to_sus = 0;
        to_active = 1;
        to_due_set = 0;
        to_due_ms = TO_DELAY_MS;
        to_req = port == PORT_AUDIO ? 1u : 0u;
    }
    sr_restore(sr);
    /* the tick must run while bulk is not enabled: add it here (a second add is a no-op; the slot is kept for good, the tick is cheap while nothing is taken) */
    if (r == 0 && tick_register() != 0) {
        sr = sr_mask();
        to_drop();
        sr_restore(sr);
        return E_NOMEM;
    }
    return r;
}

/* End a take-over: the tick switches back to the remembered port about TO_DELAY_MS later. Nothing taken: 0. A switch that has not run yet is dropped. */
i32 usb_bulk_giveback(void)
{
    u32 sr = sr_mask();
    if (taken) {
        if (to_req == 1u) {
            to_drop(); /* the switch never ran */
        } else {
            to_req = 2;
            to_due_set = 0;
            to_due_ms = TO_DELAY_MS;
        }
    }
    sr_restore(sr);
    return 0;
}

u32 usb_bulk_state(void)
{
    u32 sr = sr_mask();
    u32 i, tx = 0;
    u32 st = (enabled ? ST_ENABLED : 0u) | (connected ? ST_CONNECTED : 0u);
    /* bit 2: a live read of the active port, so a mode change after enable shows too */
    if (!usb_midi_mode())
        st |= ST_WRONG_MODE;
    if (taken)
        st |= ST_TAKEN;
    if (live)
        st |= ST_LIVE;
    if (nomem)
        st |= ST_NOMEM;
    for (i = 0; i < TX_SLOTS; i++)
        tx += tx_state[i] != T_FREE ? 1u : 0u;
    st |= (order_count & 0xffu) << 8;
    st |= (tx & 0xffu) << 16;
    st |= (resets & 0xffu) << 24;
    sr_restore(sr);
    return st;
}
