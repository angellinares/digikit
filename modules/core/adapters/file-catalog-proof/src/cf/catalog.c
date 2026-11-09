/* Private DT2 emulator diagnostic; no public API or fixed call latency claim.
 * Host changes path/command only after result=2; clear result then publish command
 * last. One firmware call per tick, UI task only. Timeout requires fresh boot. */
typedef unsigned int u32;
typedef int i32;
#define GUARD 0xa55a3cc3u
struct Mail { volatile u32 command, result; volatile i32 rc; volatile u32 error, opened, eof, calls, size; };
struct State { volatile u32 before; u32 words[4]; volatile u32 after; };
struct Output { volatile u32 before; unsigned char bytes[0x114]; volatile u32 after; };
struct Mail catalog_mail __attribute__((aligned(16)));
volatile unsigned char catalog_path[256] __attribute__((aligned(16)));
struct State catalog_state __attribute__((aligned(16))) = { GUARD, {0}, GUARD };
struct Output catalog_output __attribute__((aligned(16))) = { GUARD, {0}, GUARD };
static u32 blocked;
extern i32 catalog_dir_open(const volatile unsigned char *path, u32 *state);
extern i32 catalog_dir_next(u32 *state, unsigned char *output);
extern u32 catalog_file_size(u32 id);

static u32 guards(void) {
    return catalog_state.before == GUARD && catalog_state.after == GUARD &&
           catalog_output.before == GUARD && catalog_output.after == GUARD;
}
void catalog_tick(void) {
    u32 cmd = catalog_mail.command, i;
    i32 rc = -1;
    if (!cmd) return;
    catalog_mail.error = 0;
    if (blocked || !guards()) { blocked = 1; catalog_mail.error = 2; }
    else if (cmd == 1) {
        catalog_mail.opened = 0; catalog_mail.eof = 0;
        for (i = 0; i < 256 && catalog_path[i]; ++i) {}
        if (i == 256) catalog_mail.error = 1;
        else {
            for (i = 0; i < 4; ++i) catalog_state.words[i] = 0;
            ++catalog_mail.calls;
            rc = catalog_dir_open(catalog_path, catalog_state.words);
            if (!rc) catalog_mail.opened = 1;
        }
    } else if (cmd == 2 && catalog_mail.opened) {
        for (i = 0; i < 0x114; ++i) catalog_output.bytes[i] = 0;
        if (catalog_mail.eof) rc = 0;
        else {
            ++catalog_mail.calls;
            rc = catalog_dir_next(catalog_state.words, catalog_output.bytes);
            if (!rc) catalog_mail.eof = 1;
            if (rc < 0) catalog_mail.opened = 0;
        }
    } else if (cmd == 3 && catalog_mail.opened && !catalog_mail.eof && catalog_mail.rc == 1) {
        /* SIZE follows a successful NEXT, never shares its tick. */
        const unsigned char *p = catalog_output.bytes;
        u32 id = ((u32)p[0] << 24) | ((u32)p[1] << 16) | ((u32)p[2] << 8) | p[3];
        ++catalog_mail.calls;
        catalog_mail.size = catalog_file_size(id);
        rc = catalog_mail.size == 0xffffffffu ? -1 : 0;
    } else catalog_mail.error = 3;
    if (!guards()) { blocked = 1; catalog_mail.error = 2; catalog_mail.opened = 0; }
    if (catalog_mail.error) rc = -1;
    catalog_mail.rc = rc;
    catalog_mail.command = 0;
    catalog_mail.result = 2; /* Publish the completed output last. */
}
