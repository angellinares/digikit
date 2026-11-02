#include <assert.h>
#include <limits.h>
#include <stdint.h>
#include <stdio.h>

#include "../src/c/remote.c"

_Static_assert(sizeof(u32) == sizeof(uint32_t) && UINT_MAX == UINT32_MAX,
               "remote pacing requires 32-bit unsigned int");

static dk_service_cmd *handlers[256];
static uint32_t blocks;
static unsigned calls, last_encoder;
static int last_delta, callback_result;

u32 dkrt_blocks(void) { return blocks; }
int input_key(u32 code, u32 down) { (void)code; (void)down; return 0; }
/* The settings page of the module is tested on the host UI model (modules/core/adapters/ui/host); here its calls do nothing. */
void dk_ui_text(dk_ui_menu *m, const char *text) { (void)m; (void)text; }
void dk_ui_action(dk_ui_menu *m, const char *text, dk_ui_act_fn *run, void *ctx, int arg)
{
    (void)m; (void)text; (void)run; (void)ctx; (void)arg;
}
void dk_ui_refresh(dk_ui_menu *m) { (void)m; }
int input_turn(u32 encoder, int delta)
{
    calls++;
    last_encoder = encoder;
    last_delta = delta;
    return callback_result;
}

static int32_t register_cmds(u32 first, u32 count, dk_service_cmd *handler)
{
    assert(count >= 1 && first + count <= 256);
    for (u32 c = first; c < first + count; c++) {
        assert(!handlers[c]);
        handlers[c] = handler;
    }
    return 0;
}
static void log_message(const u8 *message, u32 value) { (void)message; assert(value == 0); }

static void accepted(u32 encoder, int delta, int expected)
{
    u8 body[] = {(u8)encoder, (u8)(s8)delta}, out[1];
    unsigned before = calls;
    assert(handlers[0x39](0x39, 1, body, 2, out) == 1);
    int taken = (s8)out[0];
    if (taken != expected)
        fprintf(stderr, "encoder %u delta %d at %u: expected %d, got %d\n",
                encoder, delta, blocks, expected, taken);
    assert(taken >= -30 && taken <= 30);
    assert(taken == expected);
    assert(calls == before + (expected != 0));
    if (expected) {
        assert(last_encoder == encoder);
        assert(last_delta == expected);
    }
}

int main(void)
{
    dk_api api = {0};
    api.register_cmds = register_cmds;
    api.log = log_message;
    assert(dkm_init(&api) == 0);

    accepted(1, 100, 30);
    accepted(1, 100, 0);
    blocks = 95;
    accepted(1, 100, 0);
    blocks = 96;
    accepted(1, 100, 30);
    accepted(2, -100, -30);
    accepted(2, -100, 0);
    blocks = 191;
    accepted(2, -100, 0);
    blocks = 192;
    accepted(2, -100, -30);

    accepted(3, 100, 30);
    accepted(3, -100, -30);
    accepted(3, -100, -30);
    accepted(3, 100, 30);

    accepted(4, 100, 30);
    accepted(5, 100, 30);
    accepted(4, 100, 0);

    callback_result = -7;
    u8 body[] = {6, 100}, out[1];
    unsigned before = calls;
    assert(handlers[0x39](0x39, 1, body, 2, out) == -7);
    assert(calls == before + 1 && last_encoder == 6 && last_delta == 30);
    callback_result = 0;
    accepted(6, 100, 30); /* A refused callback did not consume pending room. */

    /* This multiple of 96 places the next refill across uint32_t wrap. */
    blocks = UINT32_MAX - 63u;
    accepted(7, 100, 30);
    blocks = 31;
    accepted(7, 100, 0); /* 95 blocks after the accepted turn. */
    blocks = 32;
    accepted(7, 100, 30);
    return 0;
}
