/* digikit/usb-fast storage bench (a measurement aid, no USB), compiled only when uf.h defines UF_BENCH as 1. Its entry points are called by usb-fast.c. */
#include "uf.h"
#if UF_BENCH
/* the code is placed as it is: no jump tables (data in the code) */
#pragma GCC optimize("no-jump-tables")
#include <dk/core/api.h>
#include <dk/platform/file.h>
#include <dk/platform/hash32.h>
#include <dk/platform/io.h>

typedef uint8_t u8;
typedef uint32_t u32;
typedef int32_t i32;
#define E_MSG (-50)

static const dk_api *sdk;

static void put32(u8 *p, u32 v)
{
    p[0] = (u8)(v >> 24);
    p[1] = (u8)(v >> 16);
    p[2] = (u8)(v >> 8);
    p[3] = (u8)v;
}

/* Storage bench (a measurement aid, no USB): STATUS (0x37) with body 'B' [mask [MiB]] starts it, body 'R' returns the last times. It runs in its own I/O
 * job, so the SysEx handler returns at once. Every test moves MiB (default 8) and logs "bn.<name>.ms" and "bn.<name>.kbps" (MOD_LOG, info), timed with
 * dkrt_blocks() (1 block = 2/3 ms). The source buffer is never initialised: the content does not matter.
 * mask bit 0: store writes with 64 KiB and 1 MiB calls (s64, s1m); the file is aborted: nothing is committed;
 *      bit 1: writes to DK_FILES_SAMPLES (the +Drive on the DT2, the store's samples/ folder on the DN2) with 64 KiB, 256 KiB and 1 MiB calls (d64,
 *             d256, d1m); each file is aborted;
 *      bit 2: the same with 1 MiB calls, then file_close (dcl.cls: on the DT2 the firmware's hash re-read) and file_delete (dcl.del); the file exists
 *             between the two;
 *      bit 3: reads of the card with the firmware block read, sector 0 upward, 64 KiB and 1 MiB per call (r64, r1m); reads only, nothing is written;
 *      bit 4: the CPU rate of the file hash, api->hash32_* over the bytes (h32);
 *      bit 5: store write with 64 KiB calls, file_close (the store's hash check and commit: scl.cls) and file_delete (scl.del).
 * The bench writes only the one file it creates through file_create (a store file or a +Drive file; the runtime reserves its space) and never raw
 * sectors. A failed call logs "bn.<name>.err" with its status and aborts the file. */
#define BN_STORE_FILE "dkbench.bin"
#define BN_DRIVE_FILE "Samples/dkbench.bin"
#define BN_SLOTS 14u
#define BN_PHASES 10u
enum { S_S64, S_S1M, S_D64, S_D256, S_D1M, S_DCL, S_DCL_CLS, S_DCL_DEL, S_R64, S_R1M, S_H32, S_SCL, S_SCL_CLS, S_SCL_DEL };
static i32 bn_job;
static volatile u32 bn_req;
static u32 bn_mask, bn_mib, bn_phase, bn_running;
static u32 bn_ms[BN_SLOTS]; /* the last results, in ms, by slot (0 = not run or failed) */
static u8 *bn_raw, *bn_buf;
static u32 bn_buf_len;
static int bn_fw; /* bn_raw came from fw_alloc */

/* "bn." name suffix (names are at most 7 chars, so the tag fits 16) */
static void bn_tag(char *t, const char *name, const char *suffix)
{
    u32 i = 0;
    t[i++] = 'b';
    t[i++] = 'n';
    t[i++] = '.';
    while (*name && i < 11u)
        t[i++] = *name++;
    while (*suffix && i < 16u)
        t[i++] = *suffix++;
    t[i] = 0;
}

/* `mib` 0: a time only (no rate) */
static void bn_result(const char *name, u32 slot, u32 blocks, u32 mib)
{
    char t[17];
    u32 ms = blocks * 2u / 3u;
    bn_tag(t, name, ".ms");
    uf_log(t, ms);
    if (mib) {
        bn_tag(t, name, ".kbps");
        uf_log(t, ms ? (mib << 10) * 1000u / ms : 0);
    }
    bn_ms[slot] = ms ? ms : 1u;
}

static void bn_err(const char *name, i32 rc)
{
    char t[17];
    bn_tag(t, name, ".err");
    uf_log(t, (u32)rc);
}

/* Write `bn_mib` MiB to a new file with calls of `call` bytes. close 0: abort the file. close 1: file_close, then file_delete. */
static void bn_write(const char *name, u32 root, const char *path, u32 call, int close, u32 slot)
{
    u32 size = bn_mib << 20, off = 0, tries;
    i32 r = DK_FILE_E_BUSY, h = 0;
    u8 hash[4];
    if (call > bn_buf_len)
        call = bn_buf_len;
    for (tries = 0; tries < 5u && r == DK_FILE_E_BUSY; tries++) {
        r = sdk->file_create(root, path, size, 0, DK_FILE_REPLACE);
        if (r > 0) {
            h = r;
            r = 0;
        }
    }
    if (r != 0) {
        bn_err(name, r);
        return;
    }
    u32 t0 = dkrt_blocks();
    while (off < size) {
        u32 n = size - off < call ? size - off : call;
        r = DK_FILE_E_BUSY;
        for (tries = 0; tries < 3u && r == DK_FILE_E_BUSY; tries++)
            r = sdk->file_write(h, off, bn_buf, n);
        if (r != 0) {
            bn_err(name, r);
            sdk->file_abort(h);
            return;
        }
        off += n;
    }
    u32 t1 = dkrt_blocks();
    bn_result(name, slot, t1 - t0, bn_mib);
    if (!close) {
        sdk->file_abort(h);
        return;
    }
    r = sdk->file_close(h, hash);
    u32 t2 = dkrt_blocks();
    /* name is 3 chars here ("dcl", "scl"): the close and the delete are logged as "<name>.cls" and "<name>.del" */
    char n2[8] = {name[0], name[1], name[2], '.', 'c', 'l', 's', 0};
    if (r != 0) {
        bn_err(n2, r);
        return;
    }
    bn_result(n2, slot + 1u, t2 - t1, 0);
    n2[4] = 'd';
    n2[5] = 'e';
    n2[6] = 'l';
    r = sdk->file_delete(root, path);
    bn_result(n2, slot + 2u, dkrt_blocks() - t2, 0);
    if (r != 0)
        bn_err(n2, r);
}

static void bn_read(const char *name, u32 call, u32 slot)
{
    u32 total = bn_mib << 20, off = 0, sector = 0;
    if (call > bn_buf_len)
        call = bn_buf_len;
    u32 t0 = dkrt_blocks();
    while (off < total) {
        u32 n = total - off < call ? total - off : call;
        i32 r = fw_block_read(sector, n, bn_buf);
        if (r != 0) {
            bn_err(name, r);
            return;
        }
        off += n;
        sector += n >> 9;
    }
    bn_result(name, slot, dkrt_blocks() - t0, bn_mib);
}

static void bn_hash(void)
{
    dk_hash32 st;
    u32 t0 = dkrt_blocks();
    sdk->hash32_init((uint8_t *)&st);
    for (u32 left = bn_mib << 20; left;) {
        u32 n = left < bn_buf_len ? left : bn_buf_len;
        sdk->hash32_update((uint8_t *)&st, bn_buf, n);
        left -= n;
    }
    (void)sdk->hash32_finish((uint8_t *)&st);
    bn_result("h32", S_H32, dkrt_blocks() - t0, bn_mib);
}

/* One phase per step: a file call blocks for seconds, so the job returns between phases. */
static i32 bn_step(u8 *ctx)
{
    (void)ctx;
    if (!bn_running) {
        if (!bn_req)
            return DK_IO_WAIT;
        bn_req = 0;
        bn_running = 1;
        bn_phase = 0;
        for (u32 i = 0; i < BN_SLOTS; i++)
            bn_ms[i] = 0;
        bn_fw = 0;
        bn_buf_len = 1u << 20;
        bn_raw = sdk->alloc(bn_buf_len + 16u);
        if (!bn_raw) {
            bn_raw = fw_alloc(bn_buf_len + 16u);
            bn_fw = bn_raw != 0;
        }
        if (!bn_raw) {
            bn_buf_len = 65536u;
            bn_raw = sdk->alloc(bn_buf_len + 16u);
        }
        if (!bn_raw) {
            uf_log("bn.noalloc", 0);
            bn_running = 0;
            return DK_IO_WAIT;
        }
        bn_buf = (u8 *)(((u32)bn_raw + 15u) & ~15u);
        uf_log("bn.buf.kib", bn_buf_len >> 10);
        uf_log("bn.mib", bn_mib);
    }
    u32 m = bn_mask;
    switch (bn_phase) {
    case 0: if (m & 1u) bn_write("s64", DK_FILES_STORE, BN_STORE_FILE, 65536u, 0, S_S64); break;
    case 1: if (m & 1u) bn_write("s1m", DK_FILES_STORE, BN_STORE_FILE, 1u << 20, 0, S_S1M); break;
    case 2: if (m & 2u) bn_write("d64", DK_FILES_SAMPLES, BN_DRIVE_FILE, 65536u, 0, S_D64); break;
    case 3: if (m & 2u) bn_write("d256", DK_FILES_SAMPLES, BN_DRIVE_FILE, 262144u, 0, S_D256); break;
    case 4: if (m & 2u) bn_write("d1m", DK_FILES_SAMPLES, BN_DRIVE_FILE, 1u << 20, 0, S_D1M); break;
    case 5: if (m & 4u) bn_write("dcl", DK_FILES_SAMPLES, BN_DRIVE_FILE, 1u << 20, 1, S_DCL); break;
    case 6: if (m & 8u) bn_read("r64", 65536u, S_R64); break;
    case 7: if (m & 8u) bn_read("r1m", 1u << 20, S_R1M); break;
    case 8: if (m & 16u) bn_hash(); break;
    default: if (m & 32u) bn_write("scl", DK_FILES_STORE, BN_STORE_FILE, 65536u, 1, S_SCL); break;
    }
    if (++bn_phase < BN_PHASES)
        return DK_IO_MORE;
    if (bn_fw)
        fw_free(bn_raw);
    else
        sdk->free(bn_raw);
    bn_raw = bn_buf = 0;
    uf_log("bn.done", 0);
    bn_running = 0;
    return DK_IO_WAIT;
}

static void bn_finished(u8 *ctx, i32 status)
{
    (void)ctx;
    (void)status;
}

/* STATUS body 'B' [mask [MiB]]: start (E_BUSY while a run is going); 'R': running, phase and the BN_SLOTS times in ms. Returns the reply length, or a status. */
i32 uf_bench_command(const u8 *body, u32 len, u8 *out)
{
    if (body[0] == 'B') {
        if (bn_running || bn_req)
            return DK_FILE_E_BUSY;
        bn_mask = len > 1 ? body[1] : 0x3fu;
        bn_mib = len > 2 ? body[2] : 8u;
        if (bn_mib < 1u || bn_mib > 64u)
            bn_mib = 8u;
        bn_req = 1;
        sdk->io_wake(bn_job);
        put32(out, 1);
        return 4;
    }
    if (body[0] == 'R') {
        put32(out, bn_running);
        put32(out + 4, bn_phase);
        for (u32 i = 0; i < BN_SLOTS; i++)
            put32(out + 8 + 4 * i, bn_ms[i]);
        return 8 + 4 * BN_SLOTS;
    }
    return E_MSG;
}

/* dkm_init: the bench job (0 or a negative error) */
i32 uf_bench_init(const dk_api *api)
{
    sdk = api;
    bn_job = api->io_post(bn_step, bn_finished, 0);
    return bn_job > 0 ? 0 : (bn_job ? bn_job : -1);
}

void uf_bench_fini(const dk_api *api)
{
    api->io_cancel(bn_job);
}
#endif
