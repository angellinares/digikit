/* test/stock-pool: calls the stock DT2 1.17 sample loader, ReloadAllSamples and the purge directly.
 *
 *   0x38  load:   u8 mode (0 loader here, 1 loader on the sample loader worker, 2 the stock assign wrapper here), u32 slot,
 *                 u8 kind, then kind 0: NUL-terminated file path (the key is built from the file), kind 1: the 16-byte key.
 *                 Modes 0 and 2 run before the reply; mode 1 queues the job and replies at once. Mode 2 also writes the
 *                 key into the project settings, as the stock browser does.
 *   0x39  status: u32 state (0 idle, 1 queued, 2 done), i32 loader rc, i32 open rc, i32 key rc, u32 task of the command,
 *                 u32 task of the job, u32 worker task, u32 key[4], u32 jobs run
 *   0x3a  reload: u8 reset; ReloadAllSamples(project settings, reset); reply u32 task of the command, u32 worker task
 *   0x3b  purge:  u8 flag; the stock purge of unused samples; reply i32 result, u32 task of the command
 * All numbers are big endian. The task id is the word the firmware keeps for the running task (0x47db3f64). */

#include <dk/api.h>

typedef uint32_t u32;
typedef int32_t i32;
typedef uint8_t u8;

/* DT2 1.17 addresses (finding 81). The module is a test fixture for one image: dkm_init refuses to load when the first
 * four bytes of a function differ, so another image fails at load and never calls into the wrong code. */
typedef struct {
    u32 addr;
    u32 head;
} Fn;
static const Fn FNS[] = {
    {0x4015481c, 0x4e56ff28}, /* sample loader */
    {0x4019e65c, 0x4e56fff0}, /* SampleLoaderBgWorker getter */
    {0x400f0c1c, 0x42014e56}, /* worker queue */
    {0x4004e8be, 0x4fefffa8}, /* ProjectSettings::ReloadAllSamples */
    {0x40041d18, 0x202f0004}, /* Project -> ProjectSettings (+0x30) */
    {0x40043d9a, 0x4fefff20}, /* purge of unused samples */
    {0x40198b7e, 0x4e56fff0}, /* Project singleton */
    {0x40158902, 0x4e56ffe4}, /* path open */
    {0x401588c0, 0x2f0a246f}, /* path close */
    {0x4015aec2, 0x2f02242f}, /* stored metadata of an entry id */
    {0x4004b502, 0x4e56ffe4}, /* stock assign wrapper: project entry + loader */
};
#define sp_load ((i32(*)(const u32 *, u32))0x4015481c)
#define sp_worker ((u32(*)(void))0x4019e65c)
#define sp_queue ((u32(*)(u32, const char *, void *, void *))0x400f0c1c)
#define sp_reload ((void (*)(u32, u32))0x4004e8be)
#define sp_settings ((u32(*)(u32))0x40041d18)
#define sp_purge ((i32(*)(u32, u32))0x40043d9a)
#define sp_project ((u32(*)(void))0x40198b7e)
#define sp_open ((i32(*)(const u8 *, const u8 *, u32 *))0x40158902)
#define sp_close ((i32(*)(u32 *))0x401588c0)
#define sp_meta ((i32(*)(u32, u32 *))0x4015aec2)
#define sp_assign ((i32(*)(u32, const u32 *, u32))0x4004b502)

#define TASK (*(volatile u32 *)0x47db3f64u)

static volatile u32 state, cmd_task, job_task, jobs;
static volatile i32 load_rc, open_rc, key_rc;
static u32 key[4];
static u32 job_slot, job_kind, job_mode;
static u8 job_path[200];

static void put(u8 *p, u32 v)
{
    p[0] = v >> 24;
    p[1] = v >> 16;
    p[2] = v >> 8;
    p[3] = v;
}

static u32 get(const u8 *p)
{
    return ((u32)p[0] << 24) | ((u32)p[1] << 16) | ((u32)p[2] << 8) | p[3];
}

/* The job: build the key from the file if asked, then call the loader. Runs in whatever task calls it. */
static void run_job(void)
{
    u32 handle[4] = {0, 0, 0, 0};
    job_task = TASK;
    open_rc = 0;
    key_rc = 0;
    load_rc = -100;
    if (job_kind == 0) {
        open_rc = sp_open(job_path, (const u8 *)"r", handle);
        if (open_rc != 0) {
            state = 2;
            return;
        }
        key[0] = key[1] = key[2] = key[3] = 0;
        key_rc = sp_meta(handle[0], key);
        sp_close(handle);
        if (key_rc != 0) {
            state = 2;
            return;
        }
    }
    if (job_mode == 2)
        load_rc = (sp_assign(sp_settings(sp_project()), key, job_slot) & 0xff) ? 0 : -2;
    else
        load_rc = sp_load(key, job_slot);
    ++jobs;
    state = 2;
}

/* std::function job for the worker: an invoker taking the function's storage, and a manager that only copies the pointer. */
static u32 functor_word;
static u32 job_fn[4]; /* storage (2 words), manager, invoker */
static u32 done_fn[8]; /* an empty std::function */

static void invoker(const void *storage)
{
    (void)storage;
    run_job();
}

static int manager(u32 *dest, const u32 *src, int op)
{
    if (op == 1 || op == 2)
        dest[0] = src[0];
    return 0;
}

static int32_t load_cmd(const u8 *body, u32 len, u8 *out)
{
    u32 i;
    (void)out;
    if (len < 7 || state == 1)
        return -50;
    job_slot = get(body + 1);
    job_kind = body[5];
    job_mode = body[0];
    if (job_kind == 0) {
        if (len - 6 >= sizeof job_path || body[len - 1] != 0)
            return -50;
        for (i = 0; i < len - 6; i++)
            job_path[i] = body[6 + i];
    } else {
        if (len != 6 + 16)
            return -50;
        for (i = 0; i < 4; i++)
            key[i] = get(body + 6 + i * 4);
    }
    cmd_task = TASK;
    job_task = 0;
    if (body[0] != 1) {
        state = 1;
        run_job();
        return 0;
    }
    functor_word = 0;
    job_fn[0] = (u32)&functor_word;
    job_fn[1] = 0;
    job_fn[2] = (u32)manager;
    job_fn[3] = (u32)invoker;
    for (i = 0; i < 8; i++)
        done_fn[i] = 0;
    state = 1;
    sp_queue(sp_worker(), "StockPool", job_fn, done_fn);
    return 0;
}

__attribute__((optimize("no-jump-tables")))
static int32_t command(u32 cmd, u32 seq, const u8 *body, u32 len, u8 *out)
{
    (void)seq;
    if (cmd == 0x38)
        return load_cmd(body, len, out);
    if (cmd == 0x39) {
        put(out, state);
        put(out + 4, (u32)load_rc);
        put(out + 8, (u32)open_rc);
        put(out + 12, (u32)key_rc);
        put(out + 16, cmd_task);
        put(out + 20, job_task);
        put(out + 24, *(volatile u32 *)(sp_worker() + 0x20));
        put(out + 28, key[0]);
        put(out + 32, key[1]);
        put(out + 36, key[2]);
        put(out + 40, key[3]);
        put(out + 44, jobs);
        return 48;
    }
    if (cmd == 0x3a) {
        if (len != 1)
            return -50;
        put(out, TASK);
        put(out + 4, *(volatile u32 *)(sp_worker() + 0x20));
        sp_reload(sp_settings(sp_project()), body[0]);
        return 8;
    }
    if (cmd == 0x3b) {
        if (len != 1)
            return -50;
        put(out + 4, TASK);
        put(out, (u32)sp_purge(sp_project(), body[0]));
        return 8;
    }
    return -50;
}

int32_t dkm_init(const dk_api *api)
{
    u32 i;
    if (api->size < sizeof(dk_api))
        return -48;
    for (i = 0; i < sizeof FNS / sizeof FNS[0]; i++)
        if (*(const u32 *)FNS[i].addr != FNS[i].head)
            return -1;
    return api->register_cmds(0x38, 4, command);
}

void dkm_fini(const dk_api *api)
{
    (void)api;
}
