/* Store file write fixture (<dk/file.h>, <dk/io.h>, <dk/hash32.h>). A service command starts an I/O job that writes a file of a known pattern
   (byte i = (i * 7 + (i >> 8)) & 255), a chunk of 4096 bytes a step; another command reports what the job saw. The tests run on both devices.
   Commands (PROOF_CMD = 0x24):
     +0 START   body: u8 case (low 4 bits; bit 5 = the expected hash covers the pattern and 16 zero bytes (the +Drive's files), bit 6 = DK_FILE_REPLACE,
                bit 7 = the DK_FILES_SAMPLES root), u32 size (big-endian), path NUL. Cases:
                1 write with the correct hash4, 4 write with hash4 NULL, 5 write with a wrong hash4 (file_close answers E_HASH),
                2 write one chunk, then file_abort, 3 write one chunk, then sleep (DK_IO_WAIT) with the session open until RELEASE, then abort,
                6 file_delete of the path,
                7 spin inside the step (create, then wait with the session open for RELEASE; the step is mid-call while the MIDI task runs),
                8 create, abort, then file_write, file_close and file_abort on the stale handle (aux 0 to 2),
                9 file_write, file_close and file_abort on a handle that another module made (the size field is the handle; aux 0 to 2),
                10 write with two file_write calls: 65536 bytes in one call, then the rest.
                Reply: 0, or -12 when a job is running.
     +1 STATUS  reply: u8 state (0 idle, 1 running, 2 finished), i32 create result (the handle or an error), i32 close result, i32 abort result,
                i32 status of the job (what `done` got), u32 the hash this module computed over the pattern (hash32_*), u8[4] the hash file_close
                wrote, u32 steps run, u32 bytes written, i32 job id, i32 aux[3].
     +2 RELEASE io_wake the job of case 3 and let the spin of case 7 end.
     +3 CONTEXT file_create called here, in the MIDI task: the reply is its result (E_CONTEXT, -19, is right), then u8 1 while the step of case 7 spins
                (it is mid-step), then the results of file_write and file_abort on a handle (body: i32 handle, may be empty) called here. */
#include <dk/file.h>
#include <dk/io.h>
#include <dk/hash32.h>
#ifndef PROOF_CMD
#define PROOF_CMD 0x24
#endif
#define CHUNK 4096u
#define BIG 65536u
static const dk_api *sdk;
static uint8_t buf[BIG] __attribute__((aligned(16)));
static volatile uint32_t state; /* 0 idle, 1 running, 2 finished */
static uint8_t job_case;
static uint32_t root, flags, tail16;
static uint32_t size, written, steps, mod_hash;
static char path[160];
static uint8_t dev_hash[4];
static int32_t handle, create_rc, close_rc, abort_rc, job_status, job_id, phase;
static int32_t aux[3];
static volatile uint32_t spin_release, in_spin;
static void put32(uint8_t *p, uint32_t v) { p[0] = v >> 24; p[1] = v >> 16; p[2] = v >> 8; p[3] = v; }
static void fill(uint32_t off, uint32_t n) {
    for (uint32_t i = 0; i < n; ++i) { uint32_t k = off + i; buf[i] = (uint8_t)((k * 7u) + (k >> 8)); }
}
static int32_t step(uint8_t *ctx) {
    (void)ctx;
    const dk_api *a = sdk;
    steps++;
    if (job_case == 9) {
        aux[0] = a->file_write((int32_t)size, 0, buf, 512);
        aux[1] = a->file_close((int32_t)size, 0);
        aux[2] = a->file_abort((int32_t)size);
        return 0;
    }
    if (job_case == 6) {
        int32_t rc = a->file_delete(root, path);
        if (rc == DK_FILE_E_BUSY) return DK_IO_MORE;
        close_rc = rc;
        return rc;
    }
    if (phase == 0) {
        /* the hash of the whole pattern first (the module checks the device's against it) */
        dk_hash32 hs;
        a->hash32_init((uint8_t *)&hs);
        for (uint32_t off = 0; off < size; off += CHUNK) {
            uint32_t n = size - off < CHUNK ? size - off : CHUNK;
            fill(off, n);
            a->hash32_update((uint8_t *)&hs, buf, n);
        }
        if (tail16) {
            static const uint8_t tail[16] __attribute__((aligned(16)));
            a->hash32_update((uint8_t *)&hs, tail, 16);
        }
        mod_hash = a->hash32_finish((uint8_t *)&hs);
        uint8_t h4[4];
        dk_hash32_bytes(job_case == 5 ? mod_hash ^ 0x100u : mod_hash, h4);
        handle = a->file_create(root, path, size, job_case == 4 || job_case == 2 || job_case == 3 || job_case == 7 || job_case == 8 || job_case == 10 ? 0 : h4, flags);
        create_rc = handle;
        if (handle <= 0) return handle ? handle : -1;
        if (job_case == 7) {
            in_spin = 1;
            while (!spin_release) { __asm__ volatile("" ::: "memory"); }
            in_spin = 0;
            abort_rc = a->file_abort(handle);
            return 0;
        }
        if (job_case == 8) {
            abort_rc = a->file_abort(handle);
            aux[0] = a->file_write(handle, 0, buf, 512);
            aux[1] = a->file_close(handle, 0);
            aux[2] = a->file_abort(handle);
            return 0;
        }
        phase = 1;
        return DK_IO_MORE;
    }
    if (phase == 1 && job_case == 10) {
        /* one call of 64 KiB, then the tail in a second call */
        uint32_t n = size - written < BIG ? size - written : BIG;
        fill(written, n);
        int32_t rc = a->file_write(handle, written, buf, n);
        if (rc == DK_FILE_E_BUSY) return DK_IO_MORE;
        if (rc < 0) { a->file_abort(handle); return rc; }
        written += n;
        if (written >= size) phase = 3;
        return DK_IO_MORE;
    }
    if (phase == 1) {
        uint32_t n = size - written < CHUNK ? size - written : CHUNK;
        fill(written, n);
        int32_t rc = a->file_write(handle, written, buf, n);
        if (rc == DK_FILE_E_BUSY) return DK_IO_MORE;
        if (rc < 0) { a->file_abort(handle); return rc; }
        written += n;
        if (job_case == 2 && written >= CHUNK) { abort_rc = a->file_abort(handle); return 0; }
        if (job_case == 3 && written >= CHUNK) { phase = 2; return DK_IO_WAIT; }
        if (written >= size) phase = 3;
        return DK_IO_MORE;
    }
    if (phase == 2) {
        abort_rc = a->file_abort(handle);
        return 0;
    }
    {
        int32_t rc = a->file_close(handle, dev_hash);
        if (rc == DK_FILE_E_BUSY) return DK_IO_MORE;
        close_rc = rc;
        return rc;
    }
}
static void finished(uint8_t *ctx, int32_t status) {
    (void)ctx;
    job_status = status;
    __asm__ volatile("" ::: "memory");
    state = 2;
}
__attribute__((optimize("no-jump-tables")))
static int32_t command(uint32_t cmd, uint32_t seq, const uint8_t *body, uint32_t len, uint8_t *out) {
    (void)seq;
    if (cmd == PROOF_CMD) {
        if (state == 1) return -12;
        if (len < 7 || (body[0] & 0x1f) < 1 || (body[0] & 0x1f) > 10) return -50;
        uint32_t i = 5;
        while (i < len && body[i] != 0) i++;
        if (i >= len || i - 5 >= sizeof path) return -50;
        job_case = body[0] & 0x1f;
        tail16 = (body[0] & 0x20) != 0;
        flags = (body[0] & 0x40) ? DK_FILE_REPLACE : 0;
        root = (body[0] & 0x80) ? DK_FILES_SAMPLES : DK_FILES_STORE;
        size = ((uint32_t)body[1] << 24) | ((uint32_t)body[2] << 16) | ((uint32_t)body[3] << 8) | body[4];
        for (uint32_t k = 0; k <= i - 5; ++k) path[k] = (char)body[5 + k];
        written = steps = mod_hash = 0;
        handle = create_rc = close_rc = abort_rc = job_status = phase = 0;
        aux[0] = aux[1] = aux[2] = 0;
        spin_release = 0;
        dev_hash[0] = dev_hash[1] = dev_hash[2] = dev_hash[3] = 0;
        state = 1;
        job_id = sdk->io_post(step, finished, 0);
        if (job_id <= 0) { state = 0; return job_id ? job_id : -1; }
        return 0;
    }
    if (cmd == PROOF_CMD + 1) {
        out[0] = (uint8_t)state;
        put32(out + 1, (uint32_t)create_rc);
        put32(out + 5, (uint32_t)close_rc);
        put32(out + 9, (uint32_t)abort_rc);
        put32(out + 13, (uint32_t)job_status);
        put32(out + 17, mod_hash);
        out[21] = dev_hash[0]; out[22] = dev_hash[1]; out[23] = dev_hash[2]; out[24] = dev_hash[3];
        put32(out + 25, steps);
        put32(out + 29, written);
        put32(out + 33, (uint32_t)job_id);
        put32(out + 37, (uint32_t)aux[0]);
        put32(out + 41, (uint32_t)aux[1]);
        put32(out + 45, (uint32_t)aux[2]);
        return 49;
    }
    if (cmd == PROOF_CMD + 2) {
        spin_release = 1;
        sdk->io_wake(job_id);
        return 0;
    }
    if (cmd == PROOF_CMD + 3) {
        put32(out, (uint32_t)sdk->file_create(DK_FILES_STORE, "ctx-probe", 512, 0, 0));
        out[4] = (uint8_t)in_spin;
        int32_t h = len >= 4 ? (int32_t)(((uint32_t)body[0] << 24) | ((uint32_t)body[1] << 16) | ((uint32_t)body[2] << 8) | body[3]) : 0;
        put32(out + 5, (uint32_t)sdk->file_write(h, 0, buf, 512));
        put32(out + 9, (uint32_t)sdk->file_abort(h));
        return 13;
    }
    return -50;
}
int32_t dkm_init(const dk_api *api) {
    if (api->size < sizeof(dk_api)) return -48;
    sdk = api;
    return api->register_cmds(PROOF_CMD, 4, command);
}
