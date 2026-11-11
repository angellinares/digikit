/* test/input-probe: SysEx commands that press keys and turn encoders through core/input (docs/plans/remote-control.md). The proof of the input adapter:
 * what these commands do is compared with the emulator's own input.button and input.turn. digikit/remote is the module for players.
 *
 *   0x38  key:  body = code (u8), down (u8)       reply: status 0, or core/input's negative result
 *   0x39  turn: body = encoder (u8), delta (i8)   reply: status 0, or the negative result
 *
 * input_key and input_turn are imports of the runtime's table (the names core/input provides); a runtime without core/input does not find them and the
 * module does not load. */

#include <dk/core/api.h>

typedef uint32_t u32;
typedef uint8_t u8;
typedef int8_t s8;

static int32_t key(u32 cmd, u32 seq, const u8 *body, u32 len, u8 *out)
{
    (void)cmd;
    (void)seq;
    (void)out;
    if (len != 2u)
        return -50; /* E_MSG */
    return input_key(body[0], body[1]);
}

static int32_t turn(u32 cmd, u32 seq, const u8 *body, u32 len, u8 *out)
{
    (void)cmd;
    (void)seq;
    (void)out;
    if (len != 2u)
        return -50;
    return input_turn(body[0], (s8)body[1]);
}

int32_t dkm_init(const dk_api *api)
{
    int a = api->register_cmds(0x38, 1, key);
    int b = api->register_cmds(0x39, 1, turn);
    api->log((const u8 *)"input-probe registered", (u32)(a | b));
    return 0;
}
