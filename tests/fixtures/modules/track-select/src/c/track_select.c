/* test/track-select: the test-only route to the sample pool API (`sample_pool_add`, `_clear`, `_status`, `_info`) for a module that owns a
 * pool table (tests drive it without the SHOT picker). It owns no table. The runtime does not check which module a handle belongs to, and
 * the first module with a table gets handle 1 (client slot 0), so the test passes the handle in the request. Not a product picker. Commands
 * (the remote_state range of sdk/sdk.toml):
 *
 *   0x3e  add or clear:  body = handle, 'a', then the path bytes (no NUL; at most 159): sample_pool_add. The reply body is the pool index
 *                        as a BE32 (the first free entry, or the entry that already holds the path); a negative result is the reply status.
 *                        body = handle, 'c', index: sample_pool_clear; the reply status is its result (0 = cleared), no reply body
 *   0x3f  status:  body = handle, index; the reply body is the status of sample_pool_status as a signed BE32 (0 unused, 1 queued,
 *                  2 loading, 3 ready, < 0 error) and, when the entry is ready, the 16 bytes of sample_pool_info (BE32 frames,
 *                  rate, channels, token)
 * Both run in the MIDI task: they only change state, the runtime loads in its UI loop.
 *
 * The window command (sample_pool_peaks_window, API 1.8) is UI-task only, so it is split: the 0x3e handler (MIDI task) stores the request and a
 * dk_ui_tick timer (UI task, 10 ms) calls the API until it is ready or fails.
 *   0x3e  window request: body = handle, 'w', index, token (BE32), first (BE32), frames (BE32), columns (u8, 1..DK_SAMPLE_WINDOW_MAX), 16 bytes.
 *                         The reply has a status only: 0 = stored, DK_SAMPLE_E_FORMAT for a bad body, DK_SAMPLE_E_CONTRACT when the runtime has
 *                         no sample_pool_peaks_window or dk_ui_tick is not available.
 *   0x3e  window result:  body = handle, 'r'. The reply body is the last return of the API as a signed BE32 (0 = not ready yet, columns = ready,
 *                         < 0 error) and, when it is positive, columns * 4 bytes: per column int16 min then int16 max, big endian.
 */
#include <dk/core/api.h>
#include <dk/sample/pool.h>
#include <dk/platform/ui.h>

typedef uint32_t u32;
typedef uint8_t u8;

#define BARRIER() __asm__ volatile("" ::: "memory")
#define TICK_MS 10

static const dk_api *runtime;

/* The window request: written by the MIDI task, read by the UI task. win_req counts requests and is written last. */
static volatile u32 win_ok;
static volatile u32 win_req;
static volatile u32 win_handle, win_index, win_token, win_first, win_frames, win_columns;
static volatile int32_t win_ret;
static int16_t win_buf[DK_SAMPLE_WINDOW_MAX * 2];

static u32 be32(const u8 *p)
{
    return ((u32)p[0] << 24) | ((u32)p[1] << 16) | ((u32)p[2] << 8) | p[3];
}

static void put32(u8 *p, u32 v)
{
    p[0] = (u8)(v >> 24);
    p[1] = (u8)(v >> 16);
    p[2] = (u8)(v >> 8);
    p[3] = (u8)v;
}

/* UI task: call the API for the current request until it is ready or fails. A result of a replaced request is dropped. */
static void window_tick(void *ctx)
{
    u32 req, handle, index, token, first, frames, columns;
    int32_t r;
    (void)ctx;
    if (win_ret != 0)
        return;
    req = win_req;
    BARRIER();
    handle = win_handle;
    index = win_index;
    token = win_token;
    first = win_first;
    frames = win_frames;
    columns = win_columns;
    BARRIER();
    if (req == 0 || req != win_req)
        return;
    r = runtime->sample_pool_peaks_window(handle, index, token, first, frames, columns,
                                          (u8 *)win_buf, sizeof win_buf);
    BARRIER();
    if (r != 0 && req == win_req)
        win_ret = r;
}

static int32_t window_cmd(const u8 *body, u32 len, u8 *out)
{
    u32 i;
    if (len == 2 && body[1] == 'r') {
        int32_t r = win_ret;
        BARRIER();
        put32(out, (u32)r);
        if (r <= 0 || (u32)r > DK_SAMPLE_WINDOW_MAX)
            return 4;
        for (i = 0; i < (u32)r; i++) {
            u32 min = (uint16_t)win_buf[2 * i], max = (uint16_t)win_buf[2 * i + 1];
            out[4 + 4 * i] = (u8)(min >> 8);
            out[5 + 4 * i] = (u8)min;
            out[6 + 4 * i] = (u8)(max >> 8);
            out[7 + 4 * i] = (u8)max;
        }
        return (int32_t)(4 + 4 * (u32)r);
    }
    if (len != 16)
        return DK_SAMPLE_E_FORMAT;
    if (!win_ok)
        return DK_SAMPLE_E_CONTRACT;
    if (body[15] < 1 || body[15] > DK_SAMPLE_WINDOW_MAX || be32(body + 11) == 0)
        return DK_SAMPLE_E_FORMAT;
    win_handle = body[0];
    win_index = body[2];
    win_token = be32(body + 3);
    win_first = be32(body + 7);
    win_frames = be32(body + 11);
    win_columns = body[15];
    win_ret = 0;
    BARRIER();
    win_req = win_req + 1;
    return 0;
}

static int32_t pool_cmd(u32 cmd, u32 seq, const u8 *body, u32 len, u8 *out)
{
    u8 path[160];
    u32 i;
    int32_t r;
    (void)cmd;
    (void)seq;
    if (len >= 2 && (body[1] == 'w' || body[1] == 'r'))
        return window_cmd(body, len, out);
    if (len == 3 && body[1] == 'c')
        return runtime->sample_pool_clear(body[0], body[2]);
    if (len < 3 || len > 2 + 159 || body[1] != 'a')
        return DK_SAMPLE_E_FORMAT;
    for (i = 2; i < len; i++)
        path[i - 2] = body[i];
    path[len - 2] = 0;
    r = runtime->sample_pool_add(body[0], path);
    if (r < 0)
        return r;
    out[0] = (u8)((u32)r >> 24);
    out[1] = (u8)((u32)r >> 16);
    out[2] = (u8)((u32)r >> 8);
    out[3] = (u8)r;
    return 4;
}

static int32_t status_cmd(u32 cmd, u32 seq, const u8 *body, u32 len, u8 *out)
{
    int32_t status;
    (void)cmd;
    (void)seq;
    if (len != 2)
        return DK_SAMPLE_E_FORMAT;
    status = runtime->sample_pool_status(body[0], body[1]);
    out[0] = (u8)((u32)status >> 24);
    out[1] = (u8)((u32)status >> 16);
    out[2] = (u8)((u32)status >> 8);
    out[3] = (u8)status;
    if (status != DK_SAMPLE_READY)
        return 4;
    if (runtime->sample_pool_info(body[0], body[1], out + 4) != 0)
        return 4;
    return 20;
}

int32_t dkm_init(const dk_api *api)
{
    if (api->version != 1 ||
        api->size < offsetof(dk_api, sample_pool_info) + sizeof(api->sample_pool_info) ||
        !api->sample_pool_add || !api->sample_pool_clear || !api->sample_pool_status ||
        !api->sample_pool_info)
        return DK_SAMPLE_E_CONTRACT;
    runtime = api;
    /* The window command is optional: without the API field or the UI timer 'w' returns an error status. */
    if (api->size >=
            offsetof(dk_api, sample_pool_peaks_window) + sizeof(api->sample_pool_peaks_window) &&
        api->sample_pool_peaks_window && dk_ui_has(DK_UI_CAP_TICK) &&
        dk_ui_tick(window_tick, 0, TICK_MS) == 0)
        win_ok = 1;
    api->register_cmds(0x3e, 1, pool_cmd);
    api->register_cmds(0x3f, 1, status_cmd);
    return 0;
}
