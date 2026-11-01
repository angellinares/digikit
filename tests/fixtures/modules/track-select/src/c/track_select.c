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
 */
#include <dk/api.h>
#include <dk/sample_track.h>

typedef uint32_t u32;
typedef uint8_t u8;

static const dk_api *runtime;

static int32_t pool_cmd(u32 cmd, u32 seq, const u8 *body, u32 len, u8 *out)
{
    u8 path[160];
    u32 i;
    int32_t r;
    (void)cmd;
    (void)seq;
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
    if (api->version != 1 || api->size < offsetof(dk_api, sample_pool_info) + sizeof(api->sample_pool_info)
        || !api->sample_pool_add || !api->sample_pool_clear || !api->sample_pool_status || !api->sample_pool_info)
        return DK_SAMPLE_E_CONTRACT;
    runtime = api;
    api->register_cmds(0x3e, 1, pool_cmd);
    api->register_cmds(0x3f, 1, status_cmd);
    return 0;
}
