/* core/sample-pool on the Digitakt II 1.17: the sample backing (dkrt_pool.h) borrows the stock sample pool. Finding 109 has every fact.
 *
 * A load takes a free stock slot from 0x3ff down to the device's floor and queues a job on the SampleLoaderBgWorker, as the stock
 * ReloadAllSamples does. The job opens the file with the stock path functions, builds the 16-byte key from the stored metadata and
 * calls the stock loader (never the assign wrapper: it writes the Elektron project). The stock loader deduplicates by key, so the same
 * file in two slots shares one block. A release is a job on the same worker, so it never overlaps a load. PCM never moves while a
 * handle is valid.
 *
 * Tasks: dkpool_load, dkpool_release, dkpool_check, dkpool_free_frames and dkpool_tick run in the runtime's UI-loop task. The jobs run
 * in the worker task and only write their own request and the stock tables through the stock loader; the results reach the runtime
 * from dkpool_tick, never from the worker. The emulator is single threaded: the loader's own locking on a real device is [open].
 *
 * A stock reset (ReloadAllSamples with 1, project or kit load) wipes our slots about 1.5 s later. dkpool_tick sees it as key word 0
 * becoming 0xffffffff, drops every handle and tells the runtime once (dkrt_pool_lost). The slot map is not used for that: it reads 0
 * after a reset. */

#include <stdint.h>
#include "dkrt_pool.h"
#include "cf/pool_core.h"
#include "dk_fw_samples.h"
#include "dk_fw_storage.h"

typedef uint32_t u32;
typedef int32_t i32;
typedef uint8_t u8;

extern const u32 sp_dev_slot_floor;   /* device.s, from [adapter.sample_pool] slot_floor */
extern const u8 sp_dsp_pool_base;     /* anchor: its address is the pool origin in the DSP's view (0x8422b9c8) */

#define SP_REQS 16u
#define SP_PATH 256u

/* slot states */
#define SL_FREE 0u
#define SL_LOADING 1u    /* reserved by a queued load */
#define SL_OWNED 2u
#define SL_RELEASE_WAIT 3u  /* to be released, no request free yet */
#define SL_RELEASING 4u  /* a release job is queued */

/* request states */
#define RQ_FREE 0u
#define RQ_QUEUED 1u     /* posted to the worker */
#define RQ_DONE 2u       /* the job finished; dkpool_tick delivers it */
#define RQ_LOAD 0u
#define RQ_RELEASE 1u

typedef struct Req {
    volatile u32 state;
    volatile i32 result;
    volatile u32 cancelled;   /* dkpool_cancel: nobody waits for this load any more */
    u32 kind, slot, token, req;
    u32 pcm;      /* PCM byte length from the file header (bytes 4..8), read before the stock loader runs */
    dkpool_handle *out;
    u32 job[4];   /* the std::function of the job: storage (2 words), manager, invoker */
    u32 done[8];  /* an empty std::function */
    u8 path[SP_PATH];
} Req;

static Req reqs[SP_REQS];
static u8 slot_state[SP_SLOTS];  /* SL_* by stock slot */
static u8 slot_busy[SP_SLOTS];   /* nonzero for every slot this adapter holds (the argument of sp_pick_slot) */
static u32 slot_token[SP_SLOTS];
static u32 tokens_issued;   /* the token of a load is the count so far, from 1; the count never wraps */

/* The value of a 4-byte address literal that a generated site names (the site is one byte as far as the compiler knows). */
static u32 lit(const u8 *site)
{
    __asm__("" : "+r"(site));
    return *(const volatile u32 *)site;
}
static const u32 *keys(void) { return (const u32 *)lit(&dk_fw_samples_slot_keys); }
static const i32 *slot_map(void) { return (const i32 *)lit(&dk_fw_samples_slot_map); }
static const u8 *stereo_flags(void) { return (const u8 *)lit(&dk_fw_samples_slot_stereo); }
static const u32 *vector(void) { return (const u32 *)lit(&dk_fw_samples_record_vector); }

u32 dkpool_version(void) { return DKPOOL_VERSION; }

static void set_slot(u32 slot, u32 state)
{
    slot_state[slot] = (u8)state;
    slot_busy[slot] = state != SL_FREE;
}

/* Planes and frame count of the block the stock loader just made for `slot`. */
static i32 describe(u32 slot, const u32 *key, u32 pcm, dkpool_handle *out)
{
    const u32 *vec = vector();
    i32 idx = slot_map()[slot];
    u32 count = (u32)((vec[1] - vec[0]) / SP_REC_BYTES);
    const u32 *rec;
    if (idx < 0 || (u32)idx >= count)
        return DKPOOL_E_FORMAT;
    rec = (const u32 *)(vec[0] + (u32)idx * SP_REC_BYTES);
    return sp_geometry((u32)&sp_dsp_pool_base, rec[SP_REC_BLOCK / 4], rec[SP_REC_EXTENT / 4], stereo_flags()[slot] != 0, pcm, key[2], out);
}

/* The load job, in the worker task. */
static void run_load(Req *r)
{
    u32 handle[4], key[4], i;
    u8 head[12];
    i32 rc, got;
    if (r->cancelled) {   /* still queued when the runtime gave up on it: do nothing, the slot stays free */
        r->result = DKPOOL_E_LOST;
        return;
    }
    for (i = 0; i < 4; i++)   /* volatile: an initialiser would call memset, which a patched-in object cannot link */
        ((volatile u32 *)handle)[i] = ((volatile u32 *)key)[i] = 0;
    if (dk_fw_storage_path_open(r->path, (const u8 *)"r", handle) != 0) {
        r->result = DKPOOL_E_NOT_FOUND;
        return;
    }
    got = dk_fw_storage_path_read(head, 12, handle);   /* the loader takes the key, not this handle, so the read cannot disturb it */
    rc = dk_fw_storage_directory_metadata(handle[0], key);
    dk_fw_storage_path_close(handle);
    if (got != 12 || rc != 0) {
        r->result = DKPOOL_E_READ;
        return;
    }
    rc = dk_fw_samples_loader_load(key, r->slot);
    if (rc == -1) {
        r->result = DKPOOL_E_POOL;
        return;
    }
    if (rc != 0) {
        r->result = DKPOOL_E_READ;
        return;
    }
    r->pcm = ((u32)head[4] << 24) | ((u32)head[5] << 16) | ((u32)head[6] << 8) | head[7];
    r->result = describe(r->slot, key, r->pcm, r->out);
    if (r->result == 0) {
        r->out->id = r->slot;
        r->out->token = r->token;
    } else {
        dk_fw_samples_slot_release(r->slot);   /* a block we cannot describe is of no use */
    }
}

static void run_release(Req *r)
{
    dk_fw_samples_slot_release(r->slot);
    r->result = 0;
}

/* std::function plumbing for the worker: the invoker gets the function's storage, whose first word is the request. */
static void invoker(const void *storage)
{
    Req *r = *(Req *const *)storage;
    if (r->kind == RQ_LOAD)
        run_load(r);
    else
        run_release(r);
    __asm__ volatile("" ::: "memory");
    r->state = RQ_DONE;
}

static int manager(u32 *dest, const u32 *src, int op)
{
    if (op == 1 || op == 2)
        dest[0] = src[0];
    return 0;
}

static void post(Req *r)
{
    u32 i;
    r->job[0] = (u32)r;
    r->job[1] = 0;
    r->job[2] = (u32)manager;
    r->job[3] = (u32)invoker;
    for (i = 0; i < 8; i++)
        ((volatile u32 *)r->done)[i] = 0;   /* volatile: the compiler would turn a plain loop into a memset call */
    r->state = RQ_QUEUED;
    dk_fw_samples_worker_queue(dk_fw_samples_worker_get(), "SamplePool", r->job, r->done);
}

static Req *take_req(void)
{
    u32 i;
    for (i = 0; i < SP_REQS; i++)
        if (reqs[i].state == RQ_FREE)
            return &reqs[i];
    return 0;
}

i32 dkpool_load(u32 req, const dk_file_id *id, const u8 *path, dkpool_handle *out)
{
    Req *r;
    u32 slot, i;
    (void)id;
    if (!path || !out || tokens_issued == 0xffffffffu)
        return DKPOOL_E_CONTRACT;
    r = take_req();
    if (!r)
        return DKPOOL_E_CONTRACT;
    for (i = 0; i < SP_PATH && path[i]; i++)
        r->path[i] = path[i];
    if (i == SP_PATH)
        return DKPOOL_E_NOT_FOUND;
    r->path[i] = 0;
    slot = sp_pick_slot(sp_dev_slot_floor, keys(), slot_busy);
    if (!slot)
        return DKPOOL_E_POOL;
    set_slot(slot, SL_LOADING);
    slot_token[slot] = ++tokens_issued;
    r->kind = RQ_LOAD;
    r->slot = slot;
    r->token = slot_token[slot];
    r->out = out;
    r->req = req;
    r->result = 0;
    r->cancelled = 0;
    post(r);
    return DKPOOL_QUEUED;
}

/* Optional symbol (dkrt_pool.h): the runtime does not wait for load `req` any more. A job that has not started is dropped by the worker
 * when it gets to it (the stock queue cannot be edited); a job that runs finishes, and dkpool_tick releases its slot. The runtime
 * still gets one dkrt_pool_done for the request, with DKPOOL_E_LOST. */
void dkpool_cancel(u32 req)
{
    u32 i;
    for (i = 0; i < SP_REQS; i++)
        if (reqs[i].kind == RQ_LOAD && reqs[i].state == RQ_QUEUED && reqs[i].req == req)
            reqs[i].cancelled = 1;
}

static int current(const dkpool_handle *h)
{
    return h && h->id >= sp_dev_slot_floor && h->id < SP_SLOTS && slot_state[h->id] == SL_OWNED && slot_token[h->id] == h->token;
}

static void queue_release(u32 slot)
{
    Req *r = take_req();
    if (!r) {
        set_slot(slot, SL_RELEASE_WAIT);   /* dkpool_tick retries when a request is free */
        return;
    }
    set_slot(slot, SL_RELEASING);
    r->kind = RQ_RELEASE;
    r->slot = slot;
    r->out = 0;
    r->result = 0;
    post(r);
}

void dkpool_release(const dkpool_handle *h)
{
    if (current(h))   /* a stale handle (after a reset or a repeat) is ignored: its slot may belong to a newer load */
        queue_release(h->id);
}

i32 dkpool_check(const dkpool_handle *h)
{
    if (!current(h) || sp_slot_lost(keys(), h->id))
        return DKPOOL_E_LOST;
    return 0;
}

u32 dkpool_free_frames(void)
{
    const u32 *vec = vector();
    return sp_free_frames((const u32 *)vec[0], (const u32 *)vec[1]);
}

void dkpool_tick(void)
{
    u32 i, slot, lost = 0;
    /* 1. completions */
    for (i = 0; i < SP_REQS; i++) {
        Req *r = &reqs[i];
        if (r->state != RQ_DONE)
            continue;
        if (r->kind == RQ_RELEASE) {
            set_slot(r->slot, SL_FREE);
            r->state = RQ_FREE;
        } else {
            u32 req = r->req, release;
            u32 slot_of = r->slot;
            i32 result = sp_finish(r->result, r->cancelled, &release);
            set_slot(slot_of, r->result == 0 ? SL_OWNED : SL_FREE);
            r->state = RQ_FREE;
            if (release)
                queue_release(slot_of);   /* loaded for nobody */
            dkrt_pool_done(req, result);
        }
    }
    /* 2. releases that waited for a request */
    for (slot = SP_SLOTS - 1u; slot >= sp_dev_slot_floor && slot < SP_SLOTS; slot--)
        if (slot_state[slot] == SL_RELEASE_WAIT)
            queue_release(slot);
    /* 3. a stock reset: key word 0 of an owned slot is -1 again */
    for (slot = SP_SLOTS - 1u; slot >= sp_dev_slot_floor && slot < SP_SLOTS; slot--)
        if (slot_state[slot] == SL_OWNED && sp_slot_lost(keys(), slot))
            lost = 1;
    if (lost) {
        for (slot = SP_SLOTS - 1u; slot >= sp_dev_slot_floor && slot < SP_SLOTS; slot--)
            if (slot_state[slot] == SL_OWNED)
                set_slot(slot, SL_FREE);
        dkrt_pool_lost();
    }
}
