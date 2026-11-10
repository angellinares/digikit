/* core/usb-bulk: a vendor bulk interface (interface 2: EP2 OUT and EP2 IN, 512-byte packets) added to the firmware's USB MIDI mode.
 * Plan: docs/plans/usb-fast.md section 4. Facts: docs/findings/42-usb-transfer-routes.md sections 1 and 4 (static, DT2 1.17 and DN2 1.12).
 *
 * The USB engine is the firmware's and is not patched. Its port table is RAM: entry 2 (USB MIDI) holds the connect and disconnect callbacks and the
 * pointer and length of the configuration descriptor. usb_bulk_enable repoints those four words (a 124-byte copy of the stock 101-byte descriptor
 * with interface 2 appended; callbacks of ours that call the stock ones) and re-enumerates through the firmware's own stop and restart pair.
 *
 * RX: RX_BUFS buffers of RX_BYTES (RX_PAGES 4 KiB pages: a multi-page dTD is our own work; the firmware only builds single-page ones). Each is queued
 * as one dTD on EP2 OUT. A dTD completes when it is full or when a packet shorter than 512 bytes (a zero-length packet too) ends the transfer: the host
 * sends a zero-length packet after any frame whose length is a multiple of 512 bytes. The completion callback runs in the USB interrupt: it marks the buffer
 * full and queues a free one. With no free buffer nothing is queued, the host gets NAK, and that is the whole flow control. The dTDs come from the firmware's
 * pool (the engine's own EP1 callback does the same from the interrupt); its free callback is wrapped to see the status byte, and a halted or error status
 * drops the transfer. TX: usb_bulk_send copies into one of TX_SLOTS slots and queues one dTD on EP2 IN (zero-length termination on); the completion frees it.
 *
 * Buffers are accessed through the uncached alias of DDR, so the DMA and the CPU never see stale cache lines. The stock disconnect callback polls
 * ENDPTFLUSH without a bound; ours flushes EP2 first with a bounded poll. Interrupts are masked (SR 0x2700) around every state change, never around a
 * copy: the state is touched by the I/O task, the UI loop tick and the USB interrupt. A refused dTD leaves `need_prime`, retried from the module's calls
 * and from usb_bulk_tick (dkrt_poll_add).
 *
 * Contexts: usb_bulk_connect and usb_bulk_disconnect are called by the firmware (the USB interrupt, the mode switch); usb_bulk_rx_done, usb_bulk_tx_done
 * and usb_bulk_cancel are the dTD callbacks (interrupt, or the cancel path of the disconnect). The others are for modules (usb_bulk_* of sdk.toml),
 * from the I/O task. */

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
/* adapter status (<dk/usb_bulk.h>): nothing is in flight and the queue refused; the adapter's tick wakes the job to try again */
#define E_AGAIN (-72)

/* what a module sees: <dk/usb_bulk.h> */
#define ST_ENABLED 0x1u
#define ST_CONNECTED 0x2u

/* ---- the geometry ---- */
#define PAGE 4096u
#define RX_BUFS 4u
#define RX_PAGES 4u /* [O] multi-page dTDs: proven in the emulator model only */
#define RX_BYTES (RX_PAGES * PAGE)
#define TX_SLOTS 2u
#define TX_PAGES 4u
#define TX_BYTES (TX_PAGES * PAGE)
#define DESC_BYTES 124u

/* ---- the MCF5441x USB controller and the firmware's queue heads (the engine's own constants, FUN_400043e2) ---- */
#define USB_REG(off) (*(volatile u32 *)(0xfc0b0000u + (off)))
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
#define QH_CAPS_IN 0x02000000u  /* 512-byte packets, zero-length termination on [O] */
#define TD_END 0xdead0001u
#define TD_ACTIVE 0x80u
#define TD_HALTED 0x40u /* status bits of a finished dTD: halted, data buffer error, transaction error */
#define TD_BUFERR 0x20u
#define TD_XACTERR 0x08u
#define CTRL_BULK 0x00c800c8u /* enable, data toggle reset, bulk, both directions: the value the firmware writes to ENDPTCTRL1 */

/* the port table entry of USB MIDI (docs/findings/42 section 4) */
#define PORT_ENTRY 2u
#define PORT_ENTRY_BYTES 0x28u
#define STOCK_CONFIG_BYTES 101u

/* firmware (anchors by hash in the port files) */
extern u8 usb_port_table[];
extern void usb_fw_connect_body(u32 hs);
extern void usb_fw_disconnect(void);
extern i32 usb_fw_queue_td(u32 index, u8 *dtd);
extern void usb_fw_cancel_ep(u32 index);
extern u8 *usb_fw_td_alloc(void);
extern void usb_fw_stop(void);
extern void usb_fw_restart(void);
/* the board: the uncached alias of DDR is this far above the cached window (device.s) */
extern const u32 usb_alias_delta;
extern const u32 usb_ddr_base; /* the cached DDR window: base and size */
extern const u32 usb_ddr_bytes;
/* the runtime (dkrt.h) */
extern void dkrt_io_wake(i32 id);
extern i32 dkrt_poll_add(u32 f); /* the UI loop calls f once a pass */

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

static u8 mem[(RX_BUFS * RX_BYTES) + (TX_SLOTS * TX_BYTES) + DESC_BYTES + PAGE + 16];
static u8 *area; /* the 4 KiB aligned start of the buffers, as the CPU sees it (cached address) */
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
static volatile i32 io_job;
static void (*stock_connect)(u32);
static u32 stock_free; /* the firmware's own free callback of a dTD (+0x38), which ours wraps */
static volatile u32 td_status; /* the status byte of the dTD whose free callback ran last (read by the completion callback that follows) */
volatile u32 usb_bulk_rx_errors; /* transfers dropped for a halted, buffer-error or transaction-error status */

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
    return alias_of(area + i * RX_BYTES);
}

static u8 *tx_buf(u32 i)
{
    return alias_of(area + RX_BUFS * RX_BYTES + i * TX_BYTES);
}

static u8 *desc_buf(void)
{
    return alias_of(area + RX_BUFS * RX_BYTES + TX_SLOTS * TX_BYTES);
}

/* the index of the receive buffer or send slot that `p` (alias address) points into, or -1 */
static i32 rx_index(const u8 *p)
{
    u32 off = (u32)(p - rx_buf(0));
    return off < RX_BUFS * RX_BYTES ? (i32)(off / RX_BYTES) : -1;
}

static i32 tx_index(const u8 *p)
{
    u32 off = (u32)(p - tx_buf(0));
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
 * function sets the interrupt-on-completion bit because a callback is set. Caller: interrupts masked. */
static i32 queue_transfer(u32 index, u8 *data, u32 bytes, u32 pages, void *done, void *cancel)
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
        w[2 + k] = k < pages ? (u32)data + k * PAGE : 0;
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

static void prime_one(u32 i)
{
    if (stopping || !connected)
        return;
    if (queue_transfer(IDX_OUT, rx_buf(i), RX_BYTES, RX_PAGES, (void *)usb_bulk_rx_done, (void *)usb_bulk_cancel) == 0)
        rx_state[i] = B_PRIMED;
}

/* queue every free receive buffer; a buffer the engine refused leaves `need_prime` set for the tick and the module's next call */
static void prime_all(void)
{
    u32 i, left = 0;
    if (stopping || !connected)
        return;
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
    if (i >= 0 && rx_state[i] == B_PRIMED) {
        if (bad != 0u) {
            usb_bulk_rx_errors++;
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

static void ep2_flush(void)
{
    u32 spins;
    USB_REG(ENDPTFLUSH) = FLUSH_EP2;
    for (spins = 0; spins < FLUSH_SPINS && (USB_REG(ENDPTFLUSH) & FLUSH_EP2) != 0u; spins++)
        ;
}

/* EP2 off: flush with a bounded poll, cancel both queues (their callbacks run with `stopping` set: they queue nothing), then empty the lists. Every buffer
 * that was queued is free again; full buffers that nobody took belong to the connection that ended and are dropped; a taken buffer stays with its
 * owner. The reset count changes and the job is woken. Interrupts masked by the caller. */
static void ep2_stop(void)
{
    u32 i;
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

/* the host's bus reset or resume: the stock work of the port (EP1 for MIDI), then EP2 */
void usb_bulk_connect(void)
{
    u32 sr = sr_mask();
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

/* the host is gone or the mode switches: EP2 first, then the stock callback */
void usb_bulk_disconnect(void)
{
    u32 sr = sr_mask();
    ep2_stop();
    sr_restore(sr);
    usb_fw_disconnect();
}

/* once a pass of the UI loop (dkrt_poll_add): retry what the engine refused */
void usb_bulk_tick(void)
{
    u32 sr, again;
    if (!enabled)
        return;
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

i32 usb_bulk_enable(i32 io_id)
{
    u32 *e = (u32 *)(usb_port_table + PORT_ENTRY * PORT_ENTRY_BYTES);
    const u8 *cfg = (const u8 *)e[3];
    u32 sr, i;
    u8 *d;
    if (io_job > 0 && io_job != io_id)
        return E_BUSY; /* one owner */
    if (enabled) {
        io_job = io_id;
        return 0;
    }
    /* the buffers must lie in the cached DDR window: the alias is that window plus a constant */
    if ((u32)mem < usb_ddr_base || (u32)mem + sizeof mem > usb_ddr_base + usb_ddr_bytes)
        return E_CONTRACT;
    /* the stock entry, byte for byte (the firmware writes wTotalLength into its own array at run time, FUN_4000677a, through absolute addresses and not through
     * this table: bytes 2 and 3 are free) */
    if (e[4] != STOCK_CONFIG_BYTES || e[9] != 1u || e[1] != (u32)usb_fw_disconnect || !stock_connect_ok(e[0]))
        return E_CONTRACT;
    for (i = 0; i < STOCK_CONFIG_BYTES; i++)
        if (i != 2u && i != 3u && cfg[i] != stock_config[i])
            return E_CONTRACT;
    if (dkrt_poll_add((u32)usb_bulk_tick) != 0)
        return E_NOMEM;
    area = (u8 *)(((u32)mem + PAGE - 1u) & ~(PAGE - 1u));
    /* the buffers are bss: the installer may have zeroed them through the data cache, and a dirty line written back later would overwrite what the
     * controller stored or the descriptor below. Push and invalidate every line of the area first (the alias is another address for the same memory,
     * to the cache), then write through the alias only. */
    for (i = 0; i < sizeof mem; i += 16u)
        __asm__ volatile("cpushl %%bc,(%0)" : : "a"((u32)mem + i) : "memory");
    d = desc_buf();
    for (i = 0; i < STOCK_CONFIG_BYTES; i++)
        d[i] = cfg[i];
    for (i = 0; i < 23u; i++)
        d[STOCK_CONFIG_BYTES + i] = bulk_interface[i];
    d[2] = DESC_BYTES; /* wTotalLength, little endian */
    d[3] = 0;
    d[4] = 3; /* bNumInterfaces */
    for (i = 0; i < RX_BUFS; i++)
        rx_state[i] = B_FREE;
    for (i = 0; i < TX_SLOTS; i++)
        tx_state[i] = T_FREE;
    order_head = order_count = 0;
    qh_ready = 0;
    io_job = io_id;
    sr = sr_mask();
    stock_connect = (void (*)(u32))e[0];
    e[3] = (u32)d;
    e[4] = DESC_BYTES;
    e[0] = (u32)usb_bulk_connect;
    e[1] = (u32)usb_bulk_disconnect;
    enabled = 1;
    sr_restore(sr);
    /* the firmware's own USB mode switch (FUN_40006c16 does the same from the task context, interrupts on): stop (calls the disconnect callback and
     * starts a delay), restart (the tick restarts the device when the delay has run); the host sees the device go and come back with interface 2 */
    usb_fw_stop();
    usb_fw_restart();
    return 0;
}

/* the job that enabled the interface goes away (the module's fini): no more wakes. The table stays as it is until the next boot. */
i32 usb_bulk_disable(i32 io_id)
{
    if (io_id <= 0 || io_job != io_id)
        return E_STATE;
    io_job = 0;
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
    return r;
}

i32 usb_bulk_release(u8 *buf)
{
    u32 sr = sr_mask();
    i32 i = enabled ? rx_index(buf) : -1;
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
    if (!connected || stopping) {
        sr_restore(sr);
        return E_STATE;
    }
    if (need_prime)
        prime_all();
    for (i = 0; i < TX_SLOTS; i++) {
        if (tx_state[i] == T_FREE) {
            tx_state[i] = T_RESERVED;
            slot = (i32)i;
            break;
        }
    }
    before = resets;
    sr_restore(sr);
    if (slot < 0)
        return E_BUSY; /* both slots are in flight: their completion wakes the job */
    p = tx_buf((u32)slot);
    for (k = 0; k < len; k++)
        p[k] = buf[k];
    sr = sr_mask();
    if (!connected || stopping || resets != before) {
        tx_state[slot] = T_FREE; /* the bus was reset while copying */
        r = E_STATE;
    } else if (queue_transfer(IDX_IN, p, len, (len + PAGE - 1u) / PAGE, (void *)usb_bulk_tx_done, (void *)usb_bulk_cancel) != 0) {
        tx_state[slot] = T_FREE;
        tx_retry = 1;
        r = E_AGAIN;
    } else {
        tx_state[slot] = T_QUEUED;
        r = 0;
    }
    sr_restore(sr);
    return r;
}

u32 usb_bulk_state(void)
{
    u32 sr = sr_mask();
    u32 i, tx = 0;
    u32 st = (enabled ? ST_ENABLED : 0u) | (connected ? ST_CONNECTED : 0u);
    for (i = 0; i < TX_SLOTS; i++)
        tx += tx_state[i] != T_FREE ? 1u : 0u;
    st |= (order_count & 0xffu) << 8;
    st |= (tx & 0xffu) << 16;
    st |= (resets & 0xffu) << 24;
    sr_restore(sr);
    return st;
}
