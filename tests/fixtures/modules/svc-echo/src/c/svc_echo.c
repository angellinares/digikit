/* test/svc-echo: a module with SysEx commands of its own (docs/design/module-sdk.md, "Service commands").
 *
 * dkm_init registers 0x3e and 0x3f, which the remote_state range [0x3e, 0x3f] of sdk/sdk.toml reserves, and checks that the runtime refuses what it must:
 * a range outside every reserved range, one that overlaps its own, and an empty one (each result is kept in `svc_refused`).
 *
 *   0x3e  echo: the reply body is the request body; a body that starts with 0xee answers the status -77 (a status of the module's own, no body)
 *   0x3f  the refusals: three bytes, the low byte of each result of register_cmds (0xc0 is -64, E_CMD_RANGE)
 */

#include <dk/core/api.h>

typedef uint32_t u32;
typedef uint8_t u8;

u8 svc_refused[3];

static int32_t echo(u32 cmd, u32 seq, const u8 *body, u32 len, u8 *out)
{
    u32 i;
    (void)cmd;
    (void)seq;
    if (len > 0 && body[0] == 0xee)
        return -77;
    for (i = 0; i < len; i++)
        out[i] = body[i];
    return (int)len;
}

static int32_t refusals(u32 cmd, u32 seq, const u8 *body, u32 len, u8 *out)
{
    (void)cmd;
    (void)seq;
    (void)body;
    (void)len;
    out[0] = svc_refused[0];
    out[1] = svc_refused[1];
    out[2] = svc_refused[2];
    return 3;
}

int32_t dkm_init(const dk_api *api)
{
    int ok = api->register_cmds(0x3e, 1, echo);
    api->register_cmds(0x3f, 1, refusals);
    svc_refused[0] = (u8)api->register_cmds(0x10, 1, echo);  /* outside every reserved range */
    svc_refused[1] = (u8)api->register_cmds(0x3e, 1, echo);  /* over a registered one */
    svc_refused[2] = (u8)api->register_cmds(0x38, 0, echo);  /* empty */
    api->log((const u8 *)"svc-echo registered", (u32)ok);
    return 0;
}
