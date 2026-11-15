/* test/entry-demo: a module of the new contract only (docs/plans/sdk.md section 5): the entry point `dk_module`, no Api table, no `dkm_*` symbol.
 *
 * init:  opens the settings and reads the key "boot", opens the USB bulk service (core/usb-bulk is not in a plain boot: DK_E_MISSING, the handle stays
 *        NULL), reads the device and runtime version from the context, registers 0x1c and 0x1d and logs a line.
 * tick:  counts the UI passes and the block clock of the first and the last one; runs the settings write that 0x1d queued (the settings calls are for
 *        the UI task, not for the MIDI task where a command handler runs).
 * menu:  one text row, a submenu, a number row and a row that asks a question (it runs only when the user opens the module's SETTINGS...).
 *
 * `probe` below stays 0: tick links the rest of the SDK surface that no other fixture or module calls (sink, firmware block read, job cancel, debug
 * poll, the dialogs, memcmp) so that `patchkit coverage` sees it. The calls are compiled against the headers and linked; they never run.
 *
 *   0x1c  report (no body); reply, big-endian words:
 *           0  result of dk_settings_open        4  result of the read of "boot" in init (DK_E_SET_NOKEY on a fresh card)
 *           8  result of dk_usb_bulk_open       12  1 when the handle it gave is still NULL
 *          16  dk_module_ctx_device        20  dk_module_ctx_runtime_version
 *          24  ticks so far                     28  block clock of the last tick minus the first
 *          32  result of the last dk_settings_put   36  result of the read back after it (the length, 1)
 *          40  the value read back (1 byte)     41  1 while a write is queued
 *   0x1d  queue a write of the one body byte under the key "boot"; tick runs it, then reads the key back
 */

#include <dk/core/bytes.h>
#include <dk/core/log.h>
#include <dk/core/mem.h>
#include <dk/core/module.h>
#include <dk/core/sink.h>
#include <dk/core/time.h>
#include <dk/platform/debug.h>
#include <dk/platform/host.h>
#include <dk/platform/job.h>
#include <dk/platform/settings.h>
#include <dk/platform/ui.h>

static dk_settings *settings;
static dk_ui *ui;
static dk_ui_menu *sub;
static volatile uint32_t probe; /* stays 0 */
static volatile int32_t cmp;
static int32_t level;
static uint8_t sector[512];
static void sink_none(const void *e)
{
    (void)e;
}
static dk_usb_bulk *usb; /* stays NULL: there is no core/usb-bulk in a plain boot */
static int32_t r_settings, r_boot, r_usb, r_put = 1, r_back = 1;
static uint32_t device, version, ticks, t_first, t_last;
static uint8_t back, want;
static volatile uint32_t pending;

static int32_t command(uint32_t cmd, uint32_t seq, const void *request, uint32_t len, void *reply)
{
    const uint8_t *body = request;
    uint8_t *out = reply;
    (void)seq;
    if (cmd == 0x1d) {
        if (len != 1 || pending)
            return -50;
        want = body[0];
        __asm__ volatile("" ::: "memory");
        pending = 1;
        return 0;
    }
    if (cmd != 0x1c)
        return -50;
    dk_bytes_put32(out + 0, (uint32_t)r_settings);
    dk_bytes_put32(out + 4, (uint32_t)r_boot);
    dk_bytes_put32(out + 8, (uint32_t)r_usb);
    dk_bytes_put32(out + 12, usb == 0);
    dk_bytes_put32(out + 16, device);
    dk_bytes_put32(out + 20, version);
    dk_bytes_put32(out + 24, ticks);
    dk_bytes_put32(out + 28, t_last - t_first);
    dk_bytes_put32(out + 32, (uint32_t)r_put);
    dk_bytes_put32(out + 36, (uint32_t)r_back);
    out[40] = back;
    out[41] = (uint8_t)pending;
    return 42;
}

static int32_t init(dk_module_ctx *ctx)
{
    uint8_t v = 0;
    r_settings = dk_settings_open(ctx, &settings);
    if (r_settings != DK_OK)
        return r_settings;
    r_boot = dk_settings_get(settings, "boot", &v, 1);
    back = v;
    r_usb = dk_usb_bulk_open(ctx, &usb);
    dk_ui_open(ctx, &ui);
    device = dk_module_ctx_device(ctx);
    version = dk_module_ctx_runtime_version(ctx);
    dk_host_register(0x1c, 2, command);
    dk_log(DK_LOG_INFO, "entry-demo init", (uint32_t)r_usb);
    return DK_OK;
}

static void tick(dk_module_ctx *ctx)
{
    (void)ctx;
    t_last = dk_time_blocks();
    if (ticks++ == 0)
        t_first = t_last;
    if (probe) {
        uint32_t fn;
        int32_t n = (int32_t)probe;
        (void)dk_sink_set(DK_SINK_LOG, (uint8_t *)sink_none);
        (void)dk_fw_block_read(0, 512, sector);
        (void)dk_job_cancel(probe);
        (void)dk_debug_poll(0, &fn);
        cmp = __builtin_memcmp(sector, &back,
                               (uint32_t)n); /* a real call: variable length, used result */
    }
    if (pending) {
        uint8_t v = 0;
        r_put = dk_settings_put(settings, "boot", &want, 1);
        r_back = dk_settings_get(settings, "boot", &v, 1);
        back = v;
        __asm__ volatile("" ::: "memory");
        pending = 0;
    }
}

static void fini(dk_module_ctx *ctx)
{
    (void)ctx;
    dk_log(DK_LOG_INFO, "entry-demo fini", ticks);
}

static void answer(void *arg, int32_t value)
{
    (void)arg;
    (void)value;
}

static void ask(void *arg, int32_t value)
{
    static const char *const items[] = {"ONE", "TWO"};
    (void)arg;
    (void)value;
    dk_ui_confirm(ui, "ENTRY DEMO", "SURE?", (dk_ui_confirm_fn *)answer, 0);
    dk_ui_choice(ui, "ENTRY DEMO", items, 2, (dk_ui_choice_fn *)answer, 0);
    (void)dk_ui_menu_checked(sub, 0);
}

static void sub_fill(dk_ui_menu *m, void *arg)
{
    (void)arg;
    dk_ui_menu_add_text(m, "MORE");
}

static void menu(dk_module_ctx *ctx, dk_ui_menu *menu)
{
    static const dk_ui_menu_desc sub_desc = {
        .name = "entry-demo-more", .title = "MORE", .flags = 0, .fill = sub_fill};
    (void)ctx;
    dk_ui_menu_add_text(menu, "ENTRY DEMO");
    if (!sub)
        sub = dk_ui_menu_new(ui, &sub_desc);
    dk_ui_menu_add_submenu(menu, "MORE", sub);
    dk_ui_menu_add_value(menu, "LEVEL", &level, 0, 10, answer, 0);
    dk_ui_menu_add_action(menu, "ASK", ask, 0, 0);
}

const dk_module_def dk_module = {.init = init, .fini = fini, .tick = tick, .menu = menu};
