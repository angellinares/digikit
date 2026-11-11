/* Module settings API fixture (<dk/platform/settings.h>). `dkm_init` takes the handle and reads the key "boot"; a service command queues one put, get or remove
   request and `dkm_slot_0` runs it in the UI task (the settings API is not for the MIDI task, where service commands run).
   test/settings-b builds this file with another command number and PROOF_VIA_JOB, so two module names use the same key. */
#include <dk/platform/settings.h>
#ifndef PROOF_CMD
#define PROOF_CMD 0x38 /* request; PROOF_CMD + 1 reads the result */
#endif
#define VAL 16u
#define KEYS 64u
static const dk_api *sdk;
static int32_t client, init_rc, result;
static uint8_t init_val[VAL], val[VAL], req_val[VAL];
static char req_key[KEYS];
static uint8_t op, vlen;
static volatile uint32_t pending;
#ifdef PROOF_VIA_JOB
static int32_t job_step(uint8_t *ctx);
static void job_done(uint8_t *ctx, int32_t status);
#endif
static void put32(uint8_t *p, uint32_t v)
{
    p[0] = v >> 24;
    p[1] = v >> 16;
    p[2] = v >> 8;
    p[3] = v;
}
/* Request: op (1 put, 2 get, 3 remove), key length, the key bytes, the value (at most VAL bytes).
   Reply of PROOF_CMD + 1: pending (u8), the last result (i32), the result of the read in dkm_init (i32), its value (VAL bytes), the last get's value (VAL bytes). */
__attribute__((optimize("no-jump-tables"))) static int32_t
command(uint32_t cmd, uint32_t seq, const uint8_t *body, uint32_t len, uint8_t *out)
{
    (void)seq;
    if (cmd == PROOF_CMD) {
        if (len < 2 || pending)
            return -12;
        uint32_t k = body[1];
        if (k >= KEYS || len < 2 + k || len - 2 - k > VAL)
            return -50;
        op = body[0];
        for (uint32_t i = 0; i < k; ++i)
            req_key[i] = (char)body[2 + i];
        req_key[k] = 0;
        vlen = (uint8_t)(len - 2 - k);
        for (uint32_t i = 0; i < vlen; ++i)
            req_val[i] = body[2 + k + i];
        __asm__ volatile("" ::: "memory");
        pending = 1;
#ifdef PROOF_VIA_JOB
        if (sdk->job_post(job_step, job_done, 0) <= 0) {
            pending = 0;
            return -50;
        }
#endif
        return 0;
    }
    if (cmd == PROOF_CMD + 1) {
        out[0] = (uint8_t)pending;
        put32(out + 1, (uint32_t)result);
        put32(out + 5, (uint32_t)init_rc);
        for (uint32_t i = 0; i < VAL; ++i) {
            out[9 + i] = init_val[i];
            out[9 + VAL + i] = val[i];
        }
        return (int32_t)(9 + 2 * VAL);
    }
    return -50;
}
static void run_request(void)
{
    if (!pending)
        return;
    if (op == 1)
        result = dk_settings_put(sdk, client, req_key, req_val, vlen);
    else if (op == 2)
        result = dk_settings_get(sdk, client, req_key, val, VAL);
    else if (op == 3)
        result = dk_settings_remove(sdk, client, req_key);
    else
        result = -50;
    __asm__ volatile("" ::: "memory");
    pending = 0;
}
#ifdef PROOF_VIA_JOB
/* Only one module can bind slot 0, so this one runs its request in the `done` callback of a job (the UI task). */
static int32_t job_step(uint8_t *ctx)
{
    (void)ctx;
    return 0;
}
static void job_done(uint8_t *ctx, int32_t status)
{
    (void)ctx;
    (void)status;
    run_request();
}
#else
void dkm_slot_0(const dk_api *api)
{
    (void)api;
    run_request();
}
#endif
int32_t dkm_init(const dk_api *api)
{
    if (api->size < sizeof(dk_api))
        return -48;
    sdk = api;
    client = dk_settings_client(api);
    if (client <= 0)
        return client ? client : -1;
    init_rc = dk_settings_get(api, client, "boot", init_val, VAL);
    return api->register_cmds(PROOF_CMD, 2, command);
}
