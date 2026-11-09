/* Host test of digikit/remote's settings: the module's own C source, compiled here with fakes of core/input and of the runtime's
   Api table. The fake <dk/ui.h> (src/lib.rs) shows the page, and modules/core/adapters/ui/host/tests/remote.rs presses the keys. */
#include "../../../../../../tools/remote/src/c/remote.c"

static dk_service_cmd *handlers[256];
static int key_state[128];
static int key_calls, next_result;

int input_key(uint32_t code, uint32_t down)
{
    key_calls++;
    if (next_result) {
        int r = next_result;
        next_result = 0;
        return r;
    }
    key_state[code & 127] = (int)down;
    return 0;
}

int input_turn(uint32_t encoder, int delta)
{
    (void)encoder;
    (void)delta;
    return 0;
}

static int32_t register_cmds(uint32_t first, uint32_t count, dk_service_cmd *handler)
{
    for (uint32_t c = first; c < first + count && c < 256; c++)
        handlers[c] = handler;
    return 0;
}

static void log_message(const uint8_t *message, uint32_t value)
{
    (void)message;
    (void)value;
}

static void fill_page(dk_ui_menu *m, void *ctx)
{
    dkm_menu(m, ctx);
}

static const dk_ui_menu_desc page = {.name = "RemoteSettings", .title = "REMOTE CONTROL", .fill = fill_page};

int r_init(void)
{
    dk_api api = {0};
    api.register_cmds = register_cmds;
    api.log = log_message;
    return dkm_init(&api);
}

int r_cmd(int cmd, const uint8_t *body, int len)
{
    uint8_t out[4];
    return handlers[cmd & 255]((uint32_t)cmd, 1, body, (uint32_t)len, out);
}

dk_ui_menu *r_page(void) { return dk_ui_menu_new(&page); }
int r_key_down(int code) { return key_state[code & 127]; }
int r_key_calls(void) { return key_calls; }
void r_fail_next(int result) { next_result = result; }
