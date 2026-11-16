/* core/usb-bulk: a vendor bulk interface (interface 2: EP2 OUT and EP2 IN, 512-byte packets) added to the firmware's USB MIDI mode.
 * Plan: docs/plans/usb-fast.md section 4. Facts: docs/findings/42-usb-transfer-routes.md sections 1 and 4 (static, DT2 1.17 and DN2 1.12).
 *
 * The USB engine is the firmware's and is not patched. Its port table is RAM: entry 2 (USB MIDI) holds the connect and disconnect callbacks and the
 * pointer and length of the configuration descriptor. ub_enable repoints those four words (a 124-byte copy of the stock 101-byte descriptor
 * with interface 2 appended; callbacks of ours that call the stock ones) and re-enumerates through the firmware's own stop and restart pair.
 *
 * RX: a ring of up to RX_MAX slots of RX_BYTES (RX_PAGES 4 KiB pages: the firmware only builds single-page dTDs; ours name all five page pointers, see queue_transfer). Each is queued
 * as one dTD on EP2 OUT. A dTD completes when it is full or when a packet shorter than 512 bytes (a zero-length packet too) ends the transfer: the host
 * sends a zero-length packet after any frame whose length is a multiple of 512 bytes. The completion callback runs in the USB interrupt: it marks the buffer
 * full and queues a free one. With no free buffer nothing is queued, the host gets NAK, and that is the whole flow control. The dTDs come from the firmware's
 * pool (the engine's own EP1 callback does the same from the interrupt); its free callback is wrapped to see the status byte, and a halted or error status
 * drops the transfer. TX: ub_send copies into one of TX_SLOTS slots and queues one dTD on EP2 IN (64-byte packets, see QH_CAPS_IN); the completion frees it.
 *
 * Buffers are accessed through the uncached alias of DDR, so the DMA and the CPU never see stale cache lines; only the receive reads of a module use the
 * cached address, after an invalidate (UB_CACHED_RX). The stock disconnect callback polls
 * ENDPTFLUSH without a bound; ours flushes EP2 first with a bounded poll. Interrupts are masked (SR 0x2700) around every state change, never around a
 * copy: the state is touched by the I/O task, the UI loop tick and the USB interrupt. A refused dTD leaves `need_prime`, retried from the module's calls
 * and from usb_bulk_tick (dkrt_poll_add).
 *
 * There is no take-over: dk_usb_bulk_takeover and dk_usb_bulk_giveback are stubs that return E_STATE (SDK compatibility). Hosts use the OUT-only interface of USB AUDIO/MIDI
 * (core/usb-bulk-audio).
 *
 * Memory: the descriptor is static; the receive and send buffers are one block from the firmware's allocator (buffers_alloc), allocated when the interface can go
 * live and freed after dk_usb_bulk_disable (buffers_free). EP2 stays enabled (ENDPTCTRL2) from the host's connect until the disconnect, whether or not the buffers exist:
 * without them the host gets NAK. usb_bulk_connect always sets the endpoint up and queues the buffers only when they exist; buffers_alloc queues them when the
 * endpoint is up already; buffers_free takes the transfers back (flush, cancel) and frees only when the controller reports EP2 idle.
 *
 * Contexts: usb_bulk_connect and usb_bulk_disconnect are called by the firmware (the USB interrupt, the mode switch); usb_bulk_rx_done, usb_bulk_tx_done
 * and usb_bulk_cancel are the dTD callbacks (interrupt, or the cancel path of the disconnect). The others are for modules (usb_bulk_* of sdk.toml),
 * from the I/O task. */

#define UB_TRACE 0 /* 1 for hardware debug builds: log calls */

#include <stdint.h>

#include "../../include/usb_bulk_port5.h"

/* The opaque handle types of the service imports (sdk/sdk.toml [handle]); <dk/gen/imports.h> is not included: it also declares the old import names with the SDK's integer types. */
typedef struct dk_module_ctx dk_module_ctx;
typedef struct dk_usb_bulk dk_usb_bulk;

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
#define ST_WRONG_MODE                                                                              \
    0x4u /* the active USB port has no bulk route: not USB MIDI, and not USB AUDIO/MIDI with core/usb-bulk-audio (live) */
#define ST_LIVE                                                                                    \
    0x10u /* the buffers are allocated (from the moment the device is in a bulk port and enabled, until disable) */
#define ST_NOMEM 0x20u /* the last allocation of the buffers failed (cleared by the next success) */
#define ST_OUT_ONLY                                                                                \
    0x40u /* USB AUDIO/MIDI with core/usb-bulk-audio is the active port: bulk OUT only, no bulk IN (live) */

/* ---- the geometry ---- */
#define PAGE 4096u
/* The receive ring: slots of 16 KiB. The block is allocated with the largest tier the heap can give (RING_TIERS): 60 slots (960 KiB, a 1 MiB block), 28 (a 512 KiB
 * block) or 8 (128 KiB, a 256 KiB block: the old geometry). Only RX_INFLIGHT dTDs are queued on the controller at once, from the firmware's pool of 31 (finding 42):
 * 12 RX + 2 TX = 14, leaving 17 for the firmware's own (EP0, EP1 MIDI: a few). The interrupt queues the next free slot whenever one completes, so the controller keeps
 * receiving into free slots while the I/O task writes the card; the task takes the full slots in arrival order. With the ring switch off only RX_OLD (8) slots are used
 * and all of them are queued, exactly the old behaviour. */
#define RX_MAX 60u /* slots of the largest tier: state arrays are this long */
#define RX_OLD 8u
#define RX_INFLIGHT 12u
#define RX_PAGES 4u
#define RX_BYTES (RX_PAGES * PAGE)
#define TX_SLOTS 2u
#define TX_PAGES 4u
#define TX_BYTES (TX_PAGES * PAGE)
#define DESC_BYTES 124u

/* ---- the MCF5441x USB controller and the firmware's queue heads (the engine's own constants, FUN_400043e2) ---- */
#define USB_REG(off) (*(volatile u32 *)(0xfc0b0000u + (off))) /* writes */
/* Reads: always one 32-bit move. m68k GCC folds `USB_REG(x) & small_mask` into a btst on one byte (a byte access), and a register model that answers only long reads
 * (the emulator did) then sees stale bytes. The asm cannot be refolded. */
static inline __attribute__((always_inline)) u32 reg_rd(u32 off)
{
    u32 v;
    __asm__ volatile("move.l %1,%0" : "=d"(v) : "m"(*(volatile u32 *)(0xfc0b0000u + off)));
    return v;
}
#define USBCMD 0x140u
#define USBSTS 0x144u
#define PORTSC 0x184u
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
#define TD_HALTED                                                                                  \
    0x40u /* status bits of a finished dTD: halted, data buffer error, transaction error */
#define TD_BUFERR 0x20u
#define TD_XACTERR 0x08u
#define CTRL_BULK                                                                                  \
    0x00c800c8u /* enable, data toggle reset, bulk, both directions: the value the firmware writes to ENDPTCTRL1 */
/* USB AUDIO/MIDI (port 5): EP2 OUT only. EP2 IN is the audio clock feedback of the firmware: never touched there. */
#define FLUSH_EP2_RX (1u << 2) /* ENDPTFLUSH, ENDPTSTAT, ENDPTPRIME, ENDPTCOMPLETE bit of EP2 OUT */
#define CTRL_RX_MASK 0x0000ffffu /* RXE, RXR, RXT and the rest of the receive half of ENDPTCTRL2 */
#define CTRL_RX_BULK                                                                               \
    0x000000c8u /* RXE (bit 7), RXR data toggle reset (bit 6), RXT bulk (bits 3:2 = 2) */

/* the port table entry of USB MIDI (docs/findings/42 section 4) */
#define PORT_ENTRY 2u
#define PORT_AUDIO 5u /* USB AUDIO/MIDI */
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
extern u8
    usb_restart_delay[]; /* u32 in RAM: 30 down to 0, non-zero while a stop waits to restart */
extern u8 *usb_fw_mem_new(
    u32 size); /* the firmware's operator new (the 32 MiB buddy allocator) and its free */
extern void usb_fw_mem_free(u8 *p);
/* the board: the uncached alias of DDR is this far above the cached window (device.s) */
extern const u32 usb_alias_delta;
extern const u32 usb_ddr_base; /* the cached DDR window: base and size */
extern const u32 usb_ddr_bytes;
/* the runtime (dkrt.h) */
extern void dkrt_io_wake(i32 id);
extern i32 dkrt_poll_add(u32 f); /* the UI loop calls f once a pass */
extern u32 dkrt_blocks(void);    /* DSP blocks since boot (1500 a second); 0 without core/health */

#if UB_TRACE
extern void dk_log_event(u32 kind, u32 flags, const u32 *a);

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
    dk_log_event(UB_KIND_MOD_LOG, UB_FLAGS, a);
}
/* UB_LOG: trace builds only. */
#define UB_LOG(tag, v) ub_log((tag), (v))
#else
#define UB_LOG(tag, v) ((void)0)
#endif

/* the stock configuration descriptor with wTotalLength (bytes 2 and 3, written by the firmware at run time) left out, and the interface appended */
static const u8 stock_config[STOCK_CONFIG_BYTES] = {
    0x09, 0x02, 0x00, 0x00, 0x02, 0x01, 0x00, 0xc0, 0x03, 0x09, 0x04, 0x00, 0x00, 0x00, 0x01,
    0x01, 0x00, 0x00, 0x09, 0x24, 0x01, 0x00, 0x01, 0x09, 0x00, 0x01, 0x01, 0x09, 0x04, 0x01,
    0x00, 0x02, 0x01, 0x03, 0x00, 0x00, 0x07, 0x24, 0x01, 0x00, 0x01, 0x41, 0x00, 0x06, 0x24,
    0x02, 0x01, 0x01, 0x00, 0x06, 0x24, 0x02, 0x02, 0x02, 0x00, 0x09, 0x24, 0x03, 0x01, 0x03,
    0x01, 0x02, 0x01, 0x00, 0x09, 0x24, 0x03, 0x02, 0x04, 0x01, 0x01, 0x01, 0x00, 0x09, 0x05,
    0x01, 0x02, 0x00, 0x02, 0x00, 0x00, 0x00, 0x05, 0x25, 0x01, 0x01, 0x01, 0x09, 0x05, 0x81,
    0x02, 0x00, 0x02, 0x00, 0x00, 0x00, 0x05, 0x25, 0x01, 0x01, 0x03};
static const u8 bulk_interface[23] = {
    0x09, 0x04, 0x02, 0x00, 0x02, 0xff, 0x00,
    0x00, 0x00,                                /* interface 2, vendor class, two endpoints */
    0x07, 0x05, 0x02, 0x02, 0x00, 0x02, 0x00,  /* EP2 OUT, bulk, 512 */
    0x07, 0x05, 0x82, 0x02, 0x00, 0x02, 0x00}; /* EP2 IN, bulk, 512 */

/* ---- state ---- */
enum { B_FREE = 0, B_PRIMED, B_FULL, B_TAKEN };
enum { T_FREE = 0, T_RESERVED, T_QUEUED };

/* The descriptor stays for good (the port table entry points at it until the next boot). The receive and send buffers are allocated while the interface can be
 * live (buffers_alloc) and freed after disable (buffers_free); between, the adapter holds only this descriptor. */
static u8 desc_mem[DESC_BYTES + 16] __attribute__((aligned(16)));
static u8 *rx_area,
    *tx_area; /* the 4 KiB aligned buffers, as the CPU sees them (cached address); null while not live */
static u8 *blk_raw; /* what the allocator returned (freed as it is) */
static volatile u32 live, alloc_busy, free_req, nomem;
/* for the emulator test: the last block allocated (kept after the free) */
volatile u32 usb_bulk_last_area, usb_bulk_last_bytes;
volatile u32
    usb_bulk_free_why; /* why the last buffers_free refused: 2 a send slot is reserved, 4 EP2 not idle (the flush, ENDPTSTAT and ENDPTPRIME bits are in bits 8 and up), 8 a transfer of ours is queued, 16 allocating; 0 freed */
static volatile u8 rx_state[RX_MAX];
static u32 rx_len[RX_MAX];
static u8 order[RX_MAX]; /* the full slots in arrival order */
static u32 order_head, order_count;
/* The ring. `rx_slots`: the slots of the block allocated for this session (0 while not live). `usb_bulk_ring_off`: the run-time switch (bit 1 of the usb-fast switch mask; on by default):
 * off = only RX_OLD slots are used and all are queued, the old geometry. It is read at every queueing and at the next allocation (a block is sized by it). `prime_cur`:
 * where the interrupt looks for the next free slot, so the slots are used round the ring. */
static u32 rx_slots, prime_cur;
volatile u32
    usb_bulk_ring_off; /* 1 = the switch is off (0, the default, is on: bss); also the emulator test's handle on it */
volatile u32
    usb_bulk_tier_cap; /* emulator test knob: a tier of more slots than this (when not 0) counts as an allocation that failed */
volatile u32
    usb_bulk_last_slots; /* for the emulator test: the slots of the last block allocated (kept after the free) */
static volatile u8 tx_state[TX_SLOTS];
static volatile u32 enabled, connected, resets;
/* `stopping`: ep2_stop is cancelling the queues; the callbacks it runs must not queue anything. `need_prime`: a receive buffer is free and the
 * engine refused its dTD (the pool was empty); the tick retries. `tx_retry`: a send was refused; the tick wakes the job. `qh_ready`: the EP2 queue
 * heads were set up by this adapter. */
static volatile u32 stopping, need_prime, tx_retry, qh_ready;
static u32
    restart_due; /* enable_prepare found the device in USB MIDI mode: the first enable re-enumerates */
static volatile i32 io_job;
static void (*stock_connect)(u32);
static u32 stock_free; /* the firmware's own free callback of a dTD (+0x38), which ours wraps */
static volatile u32
    td_status; /* the status byte of the dTD whose free callback ran last (read by the completion callback that follows) */
volatile u32
    usb_bulk_restarts; /* for the emulator test: the firmware stop and restart pairs of ub_enable (the second enumeration in USB MIDI mode) */
volatile u32
    usb_bulk_rx_errors; /* transfers dropped for a halted, buffer-error or transaction-error status */
volatile u32
    usb_bulk_rx_inval_lines; /* data cache lines invalidated by ub_take (0 with UB_CACHED_RX 0); the emulator has no data cache model */

/* UB_CACHED_RX 1: ub_take hands out the CACHED address of a receive buffer, after it invalidated the buffer's data cache lines, so the module's reads
 * fill whole 16-byte lines in bursts (the uncached alias costs one bus access per longword). 0: the uncached alias, as before (the A/B switch).
 * DEFAULT 0 until the emulator aliases DDR: its board keeps the cached window 0x40000000 and the uncached alias +0x08000000 in separate pages
 * (packages/emulator/machine/src/board.rs page_index), so the emulator's DMA data (written at the alias) reads as zeros at the cached address. Hardware: -DUB_CACHED_RX=1.
 * Hardware result (DN2 1.12, measured 2026-11-16, finding 134): the cached read gave no copy gain and cost about 0.19 s per 9.8 MB (the line invalidate), so the default stays 0.
 * Rules, all in this file:
 *  1. The CPU never writes a receive buffer through its cached address. Only the USB DMA writes these buffers (enable writes only the descriptor, through the
 *     alias; the receive buffers are never written by the CPU). A dirty line could be written back over DMA data later. The cached address exists only as the
 *     result of rx_cached(), which only ub_take returns, and the module reads it (its parameter is documented read-only).
 *  2. ub_take invalidates the lines of the received bytes after the DMA completed (the completion callback marked the buffer full) and before the
 *     first CPU read. `cpushl dc,(Ax)` pushes a line if modified and invalidates it (CACR[DDPI] = 0, CFPRM CPUSHL p.8-2, MCF5441x RM 6.4.8 Table 6-10). The firmware
 *     writes CACR = 0xa50ce100 once (finding 07-emulator): SPA (bit 14) = 1 so Ax is a physical address and only the line that holds it is touched, IVO = 0,
 *     DDPI = 0. The lines are clean (rule 1), so the push writes nothing. The caches do not snoop DMA (RM 6.1).
 *  3. The cached address is returned only after the invalidate; ub_release takes either address. */
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

/* the index of the receive buffer that `p` points into (the uncached alias, or the cached address that ub_take may have returned), or -1 */
static i32 rx_index(const u8 *p)
{
    u32 off;
    if (!live)
        return -1;
    if ((u32)p < (u32)rx_buf(0))
        p += usb_alias_delta; /* the cached address of the same memory */
    off = (u32)(p - rx_buf(0));
    return off < rx_slots * RX_BYTES ? (i32)(off / RX_BYTES) : -1;
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
    w[8] = bytes;                  /* +0x20: the length the firmware subtracts the remainder from */
    w[9] = (u32)done;              /* +0x24 */
    w[10] = (u32)cancel;           /* +0x28 */
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

/* `p5_attached`: core/usb-bulk-audio extended entry 5 (usb_bulk_port5_attach); set once, from the boot hook. In USB AUDIO/MIDI with it the interface is OUT only (bit
 * DK_USB_BULK_OUT_ONLY of the state word): the receive ring runs on EP2 OUT, nothing is sent. `conn_rx`: the host connection is the port 5 one (EP2 OUT only), read when
 * the buffers are freed. */
static volatile u32 p5_attached, conn_rx;

static int usb_p5_mode(void)
{
    return p5_attached && *(volatile u32 *)usb_active_port == PORT_AUDIO;
}

/* the active port can carry the receive ring: USB MIDI (the pair), or USB AUDIO/MIDI with the OUT-only interface */
static int bulk_port(void)
{
    return usb_midi_mode() || usb_p5_mode();
}

/* the slots in use now: the whole ring, or RX_OLD of it with the switch off */
static u32 rx_use(void)
{
    return !usb_bulk_ring_off || rx_slots < RX_OLD ? rx_slots : RX_OLD;
}

static void prime_one(u32 i)
{
    if (stopping || !live || !connected || !bulk_port())
        return;
    if (queue_transfer(IDX_OUT, rx_buf(i), RX_BYTES, (void *)usb_bulk_rx_done,
                       (void *)usb_bulk_cancel) == 0) {
        rx_state[i] = B_PRIMED;
        UB_LOG("ub.rx.prime", i);
    }
}

/* Queue free slots round the ring until RX_INFLIGHT are on the controller (all of them with the ring off: at most RX_OLD). The next completion queues more, so this is the
 * interrupt's re-prime too. A slot the engine refused (the dTD pool was empty) stops the scan and leaves `need_prime` set for the tick and the module's next call. */
static void prime_all(void)
{
    u32 n, i, use, cap, primed = 0, left = 0;
    if (stopping || !live || !connected || io_job <= 0)
        return; /* io_job 0: no session owns the buffers (before the first enable, or after a disable): the host is NAKed, nothing queued is dropped later by the free */
    if (!bulk_port()) {
        need_prime = 1; /* resumes when the mode is back and the firmware reconnects */
        return;
    }
    use = rx_use();
    cap = use <= RX_OLD ? use : RX_INFLIGHT;
    for (i = 0; i < rx_slots; i++)
        if (rx_state[i] == B_PRIMED)
            primed++;
    for (n = 0; n < use && primed < cap; n++) {
        i = (prime_cur + n) % use;
        if (rx_state[i] != B_FREE)
            continue;
        prime_one(i);
        if (rx_state[i] != B_PRIMED) {
            left = 1;
            break;
        }
        primed++;
        prime_cur = (i + 1u) % use;
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
    UB_LOG("ub.rx.done", bytes | ((u32)i << 24));
    if (i >= 0 && rx_state[i] == B_PRIMED) {
        if (bad != 0u) {
            usb_bulk_rx_errors++;
            UB_LOG("ub.rx.err", bad);
            rx_state[i] = B_FREE;
            prime_all();
        } else if (bytes == 0) {
            rx_state[i] = B_FREE;
            prime_all();
        } else {
            rx_len[i] = bytes;
            rx_state[i] = B_FULL;
            order[(order_head + order_count) % RX_MAX] = (u8)i;
            order_count++;
            prime_all();
            wake();
        }
    }
    sr_restore(sr);
}

/* EP2 IN transfer complete (interrupt): the slot is free; a step waiting for one is woken. */
void usb_bulk_tx_done(u32 bytes, u8 *buf)
{
    u32 sr = sr_mask();
    i32 i = tx_index(buf);
    (void)bytes;
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
static void ep2_lists_clear(u32 rx_only)
{
    volatile u32 *q = qh(QH_EP2_OUT);
    q[1] = 0;
    q[2] = TD_END;
    q[3] = 0;
    q[0x30 / 4] = 0;
    q[0x34 / 4] = 0;
    if (rx_only)
        return;
    q = qh(QH_EP2_IN);
    q[1] = 0;
    q[2] = TD_END;
    q[3] = 0;
    q[0x30 / 4] = 0;
    q[0x34 / 4] = 0;
}

/* the ENDPTFLUSH bits of EP2: both directions, or EP2 OUT only (`rx_only`: USB AUDIO/MIDI) */
static u32 ep2_bits(u32 rx_only)
{
    return rx_only ? FLUSH_EP2_RX : FLUSH_EP2;
}

/* Flush EP2 with a bounded poll. 1 when the controller has finished the flush. */
static u32 ep2_flush(u32 rx_only)
{
    u32 spins, m = ep2_bits(rx_only);
    USB_REG(ENDPTFLUSH) = m;
    for (spins = 0; spins < FLUSH_SPINS && (reg_rd(ENDPTFLUSH) & m) != 0u; spins++)
        ;
    return (reg_rd(ENDPTFLUSH) & m) == 0u;
}

/* No transfer of EP2 is flushing, primed or active in the controller (both directions, or EP2 OUT only). */
static u32 ep2_idle(u32 rx_only)
{
    return ((reg_rd(ENDPTFLUSH) | reg_rd(ENDPTSTAT) | reg_rd(ENDPTPRIME)) & ep2_bits(rx_only)) ==
           0u;
}

/* EP2 off: flush with a bounded poll, cancel both queues (their callbacks run with `stopping` set: they queue nothing), then empty the lists. Every buffer
 * that was queued is free again; full buffers that nobody took belong to the connection that ended and are dropped; a taken buffer stays with its
 * owner. The reset count changes and the job is woken. Interrupts masked by the caller. `rx_only` (USB AUDIO/MIDI): only the receive half of ENDPTCTRL2 and EP2 OUT are
 * touched, EP2 IN is the firmware's audio feedback. */
static void ep2_stop(u32 rx_only)
{
    u32 i;
    UB_LOG("ub.ep2stop", resets);
    stopping = 1;
    if (!qh_ready)
        ep2_lists_clear(
            rx_only); /* the engine's init cleared the queue heads, but another USB mode (Overbridge also uses EP2) may have left words */
    USB_REG(ENDPTCTRL2) = rx_only ? reg_rd(ENDPTCTRL2) & ~CTRL_RX_MASK : 0u;
    ep2_flush(rx_only);
    usb_fw_cancel_ep(IDX_OUT);
    if (!rx_only)
        usb_fw_cancel_ep(IDX_IN);
    ep2_lists_clear(rx_only);
    USB_REG(ENDPTCOMPLETE) = ep2_bits(
        rx_only); /* write 1 to clear: a completion of the old connection must not be seen by the engine's loop */
    for (i = 0; i < rx_slots; i++)
        if (rx_state[i] != B_TAKEN)
            rx_state[i] = B_FREE;
    order_head = order_count = 0;
    for (i = 0; i < TX_SLOTS; i++)
        if (tx_state[i] !=
            T_RESERVED) /* a send is copying into this slot (ub_send, interrupts on): it frees the slot itself when it sees the reset */
            tx_state[i] = T_FREE;
    need_prime = 0;
    if (!rx_only)
        qh_ready = 1; /* only the full clear covers the EP2 IN lists */
    if (connected) {
        resets++;
        connected = 0;
        wake();
    }
    stopping = 0;
}

/* Take every queued transfer of EP2 back without disabling the endpoint (the host gets NAK; the descriptor keeps the interface): flush, cancel, empty lists. Receive
 * buffers held by the module and send slots being filled stay as they are. Interrupts masked by the caller; EP2 must be ours (`connected`). `rx_only`: as ep2_stop. */
static void ep2_quiesce(u32 rx_only)
{
    u32 i;
    stopping = 1;
    ep2_flush(rx_only);
    usb_fw_cancel_ep(IDX_OUT);
    if (!rx_only)
        usb_fw_cancel_ep(IDX_IN);
    ep2_lists_clear(rx_only);
    USB_REG(ENDPTCOMPLETE) = ep2_bits(rx_only);
    for (i = 0; i < rx_slots; i++)
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
    ep2_stop(0);
    stock_connect(1); /* the wrapper reads and sets its argument slot */
    qh(QH_EP2_OUT)[0] = QH_CAPS_OUT;
    qh(QH_EP2_IN)[0] = QH_CAPS_IN;
    if ((reg_rd(ENDPTSTAT) & FLUSH_EP2) != 0u)
        ep2_flush(0); /* the controller still holds a transfer of the old connection */
    USB_REG(ENDPTCTRL2) = CTRL_BULK;
    connected = 1;
    conn_rx = 0;
    prime_all();
    sr_restore(sr);
}

/* USB AUDIO/MIDI, called by the port 5 wrappers of core/usb-bulk-audio. No runtime calls (the boot hook and the USB interrupt run before the runtime exists or without it);
 * ISR-safe; never stops the controller. `attach`: entry 5 now carries the OUT-only interface. */
void usb_bulk_port5_attach(void)
{
    p5_attached = 1;
}

/* After the stock port 5 connect (a bus reset or SET_CONFIGURATION: the dTDs of the old connection are gone without callbacks, so the ring is rebuilt): EP2 OUT empty, the
 * queue head caps set, the receive half of ENDPTCTRL2 on (the transmit half stays the firmware's feedback endpoint), the ring queued when the buffers exist. */
void usb_bulk_port5_connect(void)
{
    u32 sr = sr_mask();
    ep2_stop(1);
    qh(QH_EP2_OUT)[0] = QH_CAPS_OUT;
    if ((reg_rd(ENDPTSTAT) & FLUSH_EP2_RX) != 0u)
        ep2_flush(1); /* the controller still holds a transfer of the old connection */
    USB_REG(ENDPTCTRL2) = reg_rd(ENDPTCTRL2) | CTRL_RX_BULK;
    connected = 1;
    conn_rx = 1;
    prime_all();
    sr_restore(sr);
}

/* Before the stock port 5 disconnect: the receive half of ENDPTCTRL2 off, EP2 OUT flushed and cancelled, the ring idle. */
void usb_bulk_port5_disconnect(void)
{
    u32 sr = sr_mask();
    ep2_stop(1);
    sr_restore(sr);
}

/* the host is gone or the mode switches: EP2 first, then the stock callback */
void usb_bulk_disconnect(void)
{
    u32 sr = sr_mask();
    UB_LOG("ub.disconn", resets);
    ep2_stop(0);
    sr_restore(sr);
    usb_fw_disconnect();
}

#if UB_TRACE
/* the controller registers and the EP2 queue head words, logged when they change (read only: USBSTS is write 1 to clear) */
#define UB_SNAP_N 10u
static void ub_snapshot(void)
{
    static u32 last[UB_SNAP_N];
    static const char *const tags[UB_SNAP_N] = {
        "ub.USBSTS",  "ub.PORTSC",  "ub.USBCMD",  "ub.EPPRIME", "ub.EPSTAT",
        "ub.EPCOMPL", "ub.qhO.tok", "ub.qhI.tok", "ub.qhO.cur", "ub.qhI.cur"};
    u32 v[UB_SNAP_N];
    u32 i;
    v[0] = reg_rd(USBSTS);
    v[1] = reg_rd(PORTSC);
    v[2] = reg_rd(USBCMD);
    v[3] = reg_rd(ENDPTPRIME);
    v[4] = reg_rd(ENDPTSTAT);
    v[5] = reg_rd(ENDPTCOMPLETE);
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

/* ---- the buffers: allocated when the interface can go live, freed after disable ---- */

/* The block: the receive slots (ring_tiers), then TX_SLOTS send slots, 4 KiB aligned, and two pages after them (the dTD page pointers of the last buffer). The firmware's
 * buddy allocator (heap at 0x44f39c40 on the DT2: not 4 KiB aligned itself) gives a block that is 16 byte aligned: the area is rounded up inside it. The allocator
 * rounds the size up to a power of two (256 KiB), the cost while live. The firmware's operator new and free take the heap mutex themselves (the anchors
 * cf.mem.site.heap.lock_lit and unlock_lit lie inside cf.mem.fn.new; finding 01), so they may be called from the I/O, MIDI and UI tasks, never from an interrupt. */
/* The size asked from the allocator for `n` receive slots: the area, the two pages after it, and a page of slack for the 4 KiB rounding of the start (the allocator's
 * block is only 16-byte aligned). 60 slots ask 1,028,096 bytes (a 1 MiB block), 28 ask 503,808 (a 512 KiB block), 8 ask 172 KiB (a 256 KiB block). */
#define BLOCK_AREA(n) ((n) * RX_BYTES + TX_SLOTS * TX_BYTES)
#define BLOCK_BYTES(n) (BLOCK_AREA(n) + 3u * PAGE)
/* the tiers tried in order, the largest first; with the ring off only the last */
static const u8 ring_tiers[3] = {60, 28, 8};

/* Allocate the buffers (task context, never under the mask or in an interrupt). 0 when live (also when it already was). E_STATE: another caller is allocating.
 * E_NOMEM: no memory, or the block is outside the cached DDR window the alias arithmetic needs (ST_NOMEM shows it; nothing is changed). The area is pushed
 * out of the data cache (the allocator's earlier owner may have left dirty lines) and used through the alias only. When EP2 is up already (the host connected
 * before the buffers existed, or after they were freed), the buffers are queued at once, under the mask. */
static i32 buffers_alloc(void)
{
    u32 sr, i, n, t;
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
    /* the largest tier the heap gives (the ring switch is read once, here); a failure of every tier is E_NOMEM, never a half-enabled ring */
    n = 0;
    raw = 0;
    a = 0;
    for (t = usb_bulk_ring_off ? 2u : 0u; t < 3u; t++) {
        u32 want = ring_tiers[t];
        u8 *r = usb_bulk_tier_cap != 0u && want > usb_bulk_tier_cap
                    ? 0
                    : usb_fw_mem_new(BLOCK_BYTES(
                          want)); /* the cap: the emulator test's way to make a tier fail */
        u8 *al = (u8 *)(((u32)r + PAGE - 1u) & ~(PAGE - 1u));
        if (r != 0 && (u32)al >= usb_ddr_base &&
            (u32)al + BLOCK_AREA(want) + PAGE <= usb_ddr_base + usb_ddr_bytes) {
            raw = r;
            a = al;
            n = want;
            break;
        }
        if (r != 0)
            usb_fw_mem_free(r);
    }
    if (n == 0u) {
        nomem = 1;
        alloc_busy = 0;
        return E_NOMEM;
    }
    for (i = 0; i < BLOCK_AREA(n); i += 16u)
        __asm__ volatile("cpushl %%bc,(%0)" : : "a"((u32)a + i) : "memory");
    usb_bulk_last_area = (u32)a;
    usb_bulk_last_bytes = BLOCK_AREA(n);
    usb_bulk_last_slots = n;
    sr = sr_mask();
    blk_raw = raw;
    rx_area = a;
    tx_area = a + n * RX_BYTES;
    rx_slots = n;
    prime_cur = 0;
    for (i = 0; i < n; i++)
        rx_state[i] = B_FREE;
    for (i = 0; i < TX_SLOTS; i++)
        tx_state[i] = T_FREE;
    order_head = order_count = 0;
    need_prime = 0;
    nomem = 0;
    live = 1;
    alloc_busy = 0;
    if (connected && bulk_port())
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
    for (i = 0; i < TX_SLOTS; i++)
        if (tx_state[i] == T_RESERVED) {
            usb_bulk_free_why = 2;
            sr_restore(sr);
            return 0;
        }
    if (connected) {
        u32 m = ep2_bits(conn_rx);
        ep2_quiesce(conn_rx);
        if (!ep2_idle(conn_rx)) {
            usb_bulk_free_why = 4 | (((reg_rd(ENDPTFLUSH) & m) != 0u ? 1u : 0u) |
                                     ((reg_rd(ENDPTSTAT) & m) != 0u ? 2u : 0u) |
                                     ((reg_rd(ENDPTPRIME) & m) != 0u ? 4u : 0u))
                                        << 8;
            need_prime = 1;
            sr_restore(sr);
            return 0;
        }
    } else {
        for (i = 0; i < rx_slots; i++)
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
    for (i = 0; i < rx_slots; i++)
        rx_state[i] = B_FREE;
    rx_slots = 0;
    for (i = 0; i < TX_SLOTS; i++)
        tx_state[i] = T_FREE;
    order_head = order_count = 0;
    need_prime = 0;
    free_req = 0;
    sr_restore(sr);
    usb_fw_mem_free(raw);
    return 1;
}

static u32 to_elapsed_ms(u32 since, u32 now, u32 per_s)
{
    u32 d = now - since;
    return d / per_s * 1000u + d % per_s * 1000u / per_s;
}

/* The firmware's restart delay (anchor cf.usb.value.restart_delay): 30 down to 0 by the USB tick (FUN_4000713e, about 33 ms a tick), non-zero while a stop is waiting to
 * restart (the firmware's own mode switch from its menu, or enable's stop below). A larger value is not the counter and counts as no restart pending. A second stop
 * or restart while one is pending is not safe, so enable waits for 0. */
#define FW_RESTART_TICKS                                                                           \
    30u /* the largest value the counter can have: what the firmware's stop sets */

static u32 fw_restart_pending(void)
{
    u32 d = *(volatile u32 *)usb_restart_delay;
    return d != 0u && d <= FW_RESTART_TICKS;
}

/* The job is woken every HK_WAKE_MS while the interface is enabled: the module has no timer of its own (a retry of ub_enable while the mode switch settles,
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

/* once a pass of the UI loop (dkrt_poll_add): free the buffers after a disable, then retry what the engine refused */
void usb_bulk_tick(void)
{
    u32 sr, again;
#if UB_TRACE
    ub_snapshot();
#endif
    if (free_req)
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
    if (c[0] != 0x70 || c[1] != 0x01 || c[2] != 0x2f || c[3] != 0x40 || c[4] != 0x00 ||
        c[5] != 0x04 || c[6] != 0x60 || c[7] != 0x00)
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
    if ((u32)desc_mem < usb_ddr_base ||
        (u32)desc_mem + sizeof desc_mem > usb_ddr_base + usb_ddr_bytes)
        return E_CONTRACT;
    /* the stock entry, byte for byte (the firmware writes wTotalLength into its own array at run time, FUN_4000677a, through absolute addresses and not through
     * this table: bytes 2 and 3 are free) */
    if (e[4] != STOCK_CONFIG_BYTES || e[9] != 1u || e[1] != (u32)usb_fw_disconnect ||
        !stock_connect_ok(e[0]))
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
    restart_due = usb_midi_mode() && !fw_restart_pending();
    stock_connect = (void (*)(u32))e[0];
    e[3] = (u32)d;
    e[4] = DESC_BYTES;
    e[0] = (u32)usb_bulk_connect;
    e[1] = (u32)usb_bulk_disconnect;
    enabled = 1;
    sr_restore(sr);
    return 0;
}

/* The caller (the module's I/O job) calls this until it returns 0. The first call prepares entry 2 whatever the mode (see enable_prepare), so a later switch to
 * USB MIDI from the firmware's menu enumerates once, with the bulk interface already in the descriptor. E_STATE: the device has no bulk port, or a restart is
 * pending (the firmware restarts the device a second after a stop, and a second stop is not safe); call again later. */
static i32 ub_enable(i32 io_id)
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
     * (checked also when the interface is enabled already, so a caller never sees success outside USB MIDI mode). With core/usb-bulk-audio, USB AUDIO/MIDI has the OUT-only
     * interface 5 (EP2 OUT): enable then allocates the ring and returns 0 with no re-enumeration. */
    if (!bulk_port())
        return E_STATE;
    /* The device is in a bulk port: the buffers are needed from the next connect on. A device that connected before the buffers existed (the user switched mode, or the
     * buffers were freed after a disable) has EP2 up already: buffers_alloc queues them, no re-enumeration. A restart that is pending is refused before
     * anything is allocated. A failure (E_NOMEM, ST_NOMEM) changes nothing. */
    ok = !fw_restart_pending();
    if (!ok)
        return E_STATE;
    r = buffers_alloc();
    if (r != 0)
        return r;
    if (need_prime && connected &&
        bulk_port()) { /* the buffers were kept (live) and a disable took the transfers back */
        sr = sr_mask();
        prime_all();
        sr_restore(sr);
    }
    free_req =
        0; /* an enable is a new request for the buffers: a free asked for by an earlier disable must not take them back (the tick would free
                   * the block this call has just allocated) */
    if (usb_p5_mode())
        return 0; /* the device enumerated with interface 5 at boot: nothing to switch or restart */
    /* a restart is pending: the caller retries. Also when enabled already, so a caller never sees success in the middle of one */
    sr = sr_mask();
    ok = !fw_restart_pending();
    if (ok && restart_due)
        restart_due = 0;
    else if (ok) {
        sr_restore(sr);
        return 0;
    }
    sr_restore(sr);
    if (!ok)
        return E_STATE;
    /* the firmware's own stop and restart (its USB mode switch does the same from the task context, interrupts on): stop (calls the disconnect callback and
     * starts a delay), restart (the tick restarts the device when the delay has run); the host sees the device go and come back with interface 2 */
    usb_bulk_restarts++;
    usb_fw_stop();
    usb_fw_restart();
    return 0;
}

/* the job that enabled the interface goes away (the module's fini): no more wakes. The table stays as it is until the next boot. */
static i32 ub_disable(i32 io_id)
{
    u32 sr, i;
    if (io_id <= 0 || io_job != io_id)
        return E_STATE;
    sr = sr_mask();
    io_job = 0;
    /* the job is going away (the runtime waits for its running step before fini, then cancels it): what it still holds is released, so the free can happen */
    for (i = 0; i < rx_slots; i++)
        if (rx_state[i] == B_TAKEN)
            rx_state[i] = B_FREE;
    for (i = 0; i < TX_SLOTS; i++)
        if (tx_state[i] == T_RESERVED)
            tx_state[i] = T_FREE;
    if (connected)
        ep2_quiesce(
            conn_rx); /* no transfer stays queued for a session that is gone: a frame that arrived now would be dropped by the free (the host is NAKed until the next enable) */
    need_prime = 1;
    free_req = 1; /* the tick frees the buffers */
    sr_restore(sr);
    return 0;
}

static i32 ub_take(u8 **buf, u32 *len)
{
    u32 sr = sr_mask();
    i32 r = 0;
    if (io_job <= 0 ||
        !live) { /* the session is disabled (or has no buffers): nothing is handed out, so a buffer is never taken after dk_usb_bulk_disable */
        sr_restore(sr);
        return 0;
    }
    if (need_prime)
        prime_all();
    if (order_count != 0) {
        u32 i = order[order_head];
        order_head = (order_head + 1u) % RX_MAX;
        order_count--;
        rx_state[i] = B_TAKEN;
        *buf = rx_buf(i);
        *len = rx_len[i];
        r = 1;
    }
    sr_restore(sr);
#if UB_CACHED_RX
    if (r ==
        1) { /* the buffer is ours (B_TAKEN, the DMA is done): invalidate with interrupts on, then hand out the cached address (rules 2 and 3) */
        u32 a = (u32)*buf - usb_alias_delta, n = (*len + 15u) & ~15u, k;
        for (k = 0; k < n; k += 16u)
            __asm__ volatile("cpushl %%dc,(%0)" : : "a"(a + k) : "memory");
        usb_bulk_rx_inval_lines += n / 16u;
        *buf = (u8 *)a;
    }
#endif
    return r;
}

static i32 ub_release(u8 *buf)
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
static i32 ub_send(const u8 *buf, u32 len)
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
            p = tx_buf(
                i); /* under the mask: the slot is reserved, so the buffers cannot be freed under the copy */
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
        tx_state[slot] =
            T_FREE; /* the bus was reset while copying (ep2_stop leaves a reserved slot to us) */
        r = E_STATE;
    } else if (queue_transfer(IDX_IN, p, len, (void *)usb_bulk_tx_done, (void *)usb_bulk_cancel) !=
               0) {
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

static u32 ub_state(void)
{
    u32 sr = sr_mask();
    u32 i, tx = 0;
    u32 st = (enabled ? ST_ENABLED : 0u) | (connected ? ST_CONNECTED : 0u);
    /* bit 2: a live read of the active port, so a mode change after enable shows too; set when the port has no bulk route (not USB MIDI, not an attached port 5) */
    if (!bulk_port())
        st |= ST_WRONG_MODE;
    if (usb_p5_mode())
        st |= ST_OUT_ONLY;
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

/* How many 64 KiB frames of the usb-fast protocol the receive ring takes before the controller NAKs: a frame (header, offset and 64 KiB) spans five 16 KiB slots, the last
 * one short. */
#define RING_SLOTS_PER_FRAME 5u

/* The run-time ring switch and the receive window. `set` below 0 only reads; 0 or 1 sets the switch (bit 1 of the usb-fast switch mask: 1 = the ring, the default; 0 = the old 8 slots,
 * all queued). The switch is read when the buffers are allocated (a block is sized by it) and at every queueing. Returns the switch in bit 0, bit 1 always set (so 0 means "no ring": the SDK's stub for an adapter without it), in bits 16..23 the slots of the live block and, in bits 8..15, the receive
 * window in frames of 64 KiB that the buffers allocated now can take: 0 when none are live or the window is the old one (the host keeps 2 DATA frames in flight then). */
static i32 ub_ring(i32 set)
{
    u32 sr = sr_mask();
    u32 frames;
    i32 r;
    if (set >= 0)
        usb_bulk_ring_off = set != 0 ? 0u : 1u;
    frames = live ? rx_use() / RING_SLOTS_PER_FRAME : 0u;
    if (frames < 2u)
        frames = 0;
    r = (i32)((usb_bulk_ring_off ? 0u : 1u) | 2u | (frames << 8) | ((live ? rx_slots : 0u) << 16));
    sr_restore(sr);
    return r;
}

/* ---- the service imports of <dk/gen/imports.h> (docs/plans/sdk.md section 5.1) ----
   The new names of the calls above, with the handle that dk_usb_bulk_open gives out. The service has one owner (the I/O job of ub_enable), so the
   handle is only a token. */

static int usb_bulk_token;

int32_t dk_usb_bulk_open(dk_module_ctx *ctx, dk_usb_bulk **out)
{
    if (!ctx || !out)
        return -48; /* DK_E_CONTRACT */
    *out = (dk_usb_bulk *)&usb_bulk_token;
    return 0;
}

int32_t dk_usb_bulk_enable(dk_usb_bulk *ub, int32_t io_id)
{
    (void)ub;
    return ub_enable(io_id);
}

int32_t dk_usb_bulk_disable(dk_usb_bulk *ub, int32_t io_id)
{
    (void)ub;
    return ub_disable(io_id);
}

int32_t dk_usb_bulk_take(dk_usb_bulk *ub, void **buf, uint32_t *len)
{
    (void)ub;
    return ub_take((u8 **)buf, (u32 *)len);
}

int32_t dk_usb_bulk_release(dk_usb_bulk *ub, void *buf)
{
    (void)ub;
    return ub_release((u8 *)buf);
}

int32_t dk_usb_bulk_send(dk_usb_bulk *ub, const void *buf, uint32_t len)
{
    (void)ub;
    return ub_send((const u8 *)buf, len);
}

uint32_t dk_usb_bulk_state(dk_usb_bulk *ub)
{
    (void)ub;
    return ub_state();
}

/* Removed: the take-over is gone (sdk.toml keeps the imports for compatibility). */
int32_t dk_usb_bulk_takeover(dk_usb_bulk *ub)
{
    (void)ub;
    return E_STATE;
}

int32_t dk_usb_bulk_giveback(dk_usb_bulk *ub)
{
    (void)ub;
    return E_STATE;
}

int32_t dk_usb_bulk_ring(dk_usb_bulk *ub, int32_t set)
{
    (void)ub;
    return ub_ring(set);
}
