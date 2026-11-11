/* test/usb-echo: the proof of core/usb-bulk. An I/O job enables the bulk interface, then takes every received buffer, sends its bytes back on EP2 IN
 * (in pieces of 4096 bytes: four sends a buffer, so both send slots run full when completions are slow), releases it and sleeps (DK_IO_WAIT) when no buffer is left or both send slots are in flight. The adapter's
 * interrupt callbacks wake the job.
 *
 * Commands: ECHO_CMD + 1 (0x2d) HOLD, body u8 mode, and an io_wake of the job: 0 echo as usual; 1 hold every taken buffer (nothing is sent or released while the mode
 * is 1); 2 send the first piece of a buffer, then hold it. A bus reset (the reset count changes) ends a hold: the buffer is released at once.
 * ECHO_CMD + 2 (0x2e) REENABLE, no body: usb_bulk_disable, forget the buffer and the hold mode, and an io_wake of the job: its next step calls usb_bulk_enable again and
 * stores the result (a refused enable leaves the job waiting, so a later REENABLE can enable again).
 * ECHO_CMD + 3 (0x2f) TAKEOVER, body u8 op: 0 usb_bulk_takeover (also the renew), 1 usb_bulk_giveback; reply: i32 result (the module does not enable the interface itself
 * after a take-over: REENABLE does, once the device is in USB MIDI mode).
 * ECHO_CMD (0x2c): STATUS, reply: i32 result of usb_bulk_enable, u32 usb_bulk_state, u32 steps, u32 buffers taken, u32 bytes taken, u32 bytes sent,
 * u32 times a send was busy, i32 job id, u32 the reset count seen by the job. */
#include <dk/api.h>
#include <dk/io.h>

#ifndef ECHO_CMD
#define ECHO_CMD 0x2c
#endif
typedef uint8_t u8;
typedef uint32_t u32;
typedef int32_t i32;

#define E_BUSY (-12)
#define E_AGAIN (-72) /* <dk/usb_bulk.h> DK_USB_BULK_E_AGAIN */
#define PIECE 4096u

static const dk_api *sdk;
static i32 job_id, enable_rc;
static volatile u32 steps, taken, bytes_in, bytes_out, busy_count, last_epoch;
static int enabled_done;
static volatile u32 hold;
static u8 *cur;
static u32 cur_len, cur_off;

static void put32(u8 *p, u32 v)
{
    p[0] = (u8)(v >> 24);
    p[1] = (u8)(v >> 16);
    p[2] = (u8)(v >> 8);
    p[3] = (u8)v;
}

static i32 step(u8 *ctx)
{
    (void)ctx;
    steps++;
    if (!enabled_done) {
        enabled_done = 1;
        enable_rc = usb_bulk_enable(job_id);
        return DK_IO_WAIT; /* also after a refused enable: REENABLE wakes the job again */
    }
    for (;;) {
        u32 epoch = (usb_bulk_state() >> 24) & 0xffu;
        if (epoch != last_epoch) { /* the bus was reset: what was half done is void */
            last_epoch = epoch;
            cur_off = cur_len;
        }
        if (cur == 0) {
            if (usb_bulk_take(&cur, &cur_len) == 0) {
                cur = 0;
                return DK_IO_WAIT;
            }
            cur_off = 0;
            taken++;
            bytes_in += cur_len;
        }
        if (hold == 1 && cur_off < cur_len)
            return DK_IO_WAIT;
        while (cur_off < cur_len) {
            if (hold == 2 && cur_off != 0)
                return DK_IO_WAIT;
            u32 n = cur_len - cur_off < PIECE ? cur_len - cur_off : PIECE;
            i32 r = usb_bulk_send(cur + cur_off, n);
            if (r == E_BUSY || r == E_AGAIN) { /* both slots in flight, or the engine refused: the adapter wakes the job */
                busy_count++;
                return DK_IO_WAIT;
            }
            if (r < 0)
                break;
            cur_off += n;
            bytes_out += n;
            if (hold == 2 && cur_off < cur_len)
                return DK_IO_WAIT;
        }
        usb_bulk_release(cur);
        cur = 0;
    }
}

static void finished(u8 *ctx, i32 status)
{
    (void)ctx;
    (void)status;
}

static i32 command(u32 cmd, u32 seq, const u8 *body, u32 len, u8 *out)
{
    (void)seq;
    if (cmd == ECHO_CMD + 1) {
        hold = len > 0 ? body[0] : 0;
        sdk->io_wake(job_id);
        return 0;
    }
    if (cmd == ECHO_CMD + 2) {
        (void)usb_bulk_disable(job_id);
        enabled_done = 0;
        if (cur != 0)
            (void)usb_bulk_release(cur);
        cur = 0;
        cur_len = 0;
        cur_off = 0;
        hold = 0;
        sdk->io_wake(job_id);
        return 0;
    }
    if (cmd == ECHO_CMD + 3) {
        put32(out, (u32)(len > 0 && body[0] == 1 ? usb_bulk_giveback() : usb_bulk_takeover()));
        return 4;
    }
    if (cmd != ECHO_CMD)
        return -50;
    put32(out, (u32)enable_rc);
    put32(out + 4, usb_bulk_state());
    put32(out + 8, steps);
    put32(out + 12, taken);
    put32(out + 16, bytes_in);
    put32(out + 20, bytes_out);
    put32(out + 24, busy_count);
    put32(out + 28, (u32)job_id);
    put32(out + 32, last_epoch);
    return 36;
}

void dkm_fini(const dk_api *api)
{
    (void)api;
    usb_bulk_disable(job_id);
}

i32 dkm_init(const dk_api *api)
{
    if (api->size < sizeof(dk_api))
        return -48;
    sdk = api;
    job_id = api->io_post(step, finished, 0);
    if (job_id <= 0)
        return job_id ? job_id : -1;
    return api->register_cmds(ECHO_CMD, 4, command);
}
