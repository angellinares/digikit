/* Store file write fixture (<dk/platform/files.h>, <dk/platform/io.h>, <dk/platform/hash32.h>). A service command starts an I/O job that writes a file of a known pattern
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
                10 write with two file_write calls: 65536 bytes in one call, then the rest,
                11 the batch: after the path NUL come u8 n (files, at most 99) and u8 mode. File i is the path with the two digits of i appended,
                   `size` bytes of the pattern shifted by 13 * i, created with DK_FILE_DEFER (and
                   DK_FILE_REPLACE when bit 6 is set). Mode 0: dk_files_commit after the last close, then the job ends; mode 1: the job ends after the last
                   close (the runtime commits at the end of the job); mode 2: the job sleeps (DK_IO_WAIT) after the last close until RELEASE, so the runtime's
                   1 s idle commit is what names the files, then the job ends. aux[0] = dk_files_pending after the last close, aux[1] = the commit result
                   (mode 0), aux[2] = dk_files_pending after the commit (mode 0) or the first pending count seen after RELEASE (mode 2), steps = closes done.
                Reply: 0, or -12 when a job is running.
     +1 STATUS  reply: u8 state (0 idle, 1 running, 2 finished), i32 create result (the handle or an error), i32 close result, i32 abort result,
                i32 status of the job (what `done` got), u32 the hash this module computed over the pattern (hash32_*), u8[4] the hash file_close
                wrote, u32 steps run, u32 bytes written, i32 job id, i32 aux[3].
     +2 RELEASE io_wake the job of case 3 and let the spin of case 7 end.
     +3 CONTEXT file_create called here, in the MIDI task: the reply is its result (E_CONTEXT, -19, is right), then u8 1 while the step of case 7 spins
                (it is mid-step), then the results of file_write and file_abort on a handle (body: i32 handle, may be empty) called here. */
#include <dk/core/bytes.h>
#include <dk/core/module.h>
#include <dk/platform/files.h>
#include <dk/platform/host.h>
#include <dk/platform/io.h>
#include <dk/platform/hash32.h>
#ifndef PROOF_CMD
#define PROOF_CMD 0x24
#endif
#define CHUNK 4096u
#define BIG 65536u
static dk_files *files;
static uint8_t buf[BIG] __attribute__((aligned(16)));
static volatile uint32_t state; /* 0 idle, 1 running, 2 finished */
static uint8_t job_case;
static uint32_t root, flags, tail16;
static uint32_t size, written, steps, mod_hash;
static uint32_t b_n, b_mode, b_i;
static char b_path[170];
static char path[160];
static uint8_t dev_hash[4];
static int32_t handle, create_rc, close_rc, abort_rc, job_status, job_id, phase;
static int32_t aux[3];
static volatile uint32_t spin_release, in_spin;
static uint32_t seed;
static void fill(uint32_t off, uint32_t n)
{
    for (uint32_t i = 0; i < n; ++i) {
        uint32_t k = off + i + seed;
        buf[i] = (uint8_t)((k * 7u) + (k >> 8));
    }
}
/* case 11: the batch of deferred files */
static int32_t batch_step(void)
{
    if (phase == 0) {
        uint32_t l = 0;
        while (path[l] != 0 && l < 160)
            l++;
        for (uint32_t k = 0; k < l; ++k)
            b_path[k] = path[k];
        b_path[l] = (char)('0' + b_i / 10u);
        b_path[l + 1] = (char)('0' + b_i % 10u);
        b_path[l + 2] = 0;
        seed = 13u * b_i;
        handle = dk_files_create(files, root, b_path, size, 0, flags | DK_FILE_DEFER);
        if (handle == DK_FILE_E_BUSY)
            return DK_IO_MORE;
        create_rc = handle;
        if (handle <= 0)
            return handle ? handle : -1;
        written = 0;
        phase = 1;
        return DK_IO_MORE;
    }
    if (phase == 1) {
        uint32_t n = size - written < CHUNK ? size - written : CHUNK;
        fill(written, n);
        int32_t rc = dk_files_write(files, handle, written, buf, n);
        if (rc == DK_FILE_E_BUSY)
            return DK_IO_MORE;
        if (rc < 0) {
            dk_files_abort(files, handle);
            return rc;
        }
        written += n;
        if (written >= size)
            phase = 2;
        return DK_IO_MORE;
    }
    if (phase == 2) {
        int32_t rc = dk_files_close(files, handle, dev_hash);
        if (rc == DK_FILE_E_BUSY)
            return DK_IO_MORE;
        close_rc = rc;
        if (rc < 0)
            return rc;
        b_i++;
        steps = b_i;
        aux[0] = dk_files_pending(files);
        phase = b_i < b_n ? 0 : 3;
        return DK_IO_MORE;
    }
    if (phase == 3) {
        if (b_mode == 0) {
            int32_t rc = dk_files_commit(files);
            if (rc == DK_FILE_E_BUSY)
                return DK_IO_MORE;
            aux[1] = rc;
            aux[2] = dk_files_pending(files);
            return rc < 0 ? rc : 0;
        }
        if (b_mode == 1)
            return 0;
        phase = 4;
        return DK_IO_WAIT;
    }
    aux[2] = dk_files_pending(files);
    return 0;
}
static int32_t step(void *arg)
{
    (void)arg;
    if (job_case == 11)
        return batch_step();
    steps++;
    if (job_case == 9) {
        aux[0] = dk_files_write(files, (int32_t)size, 0, buf, 512);
        aux[1] = dk_files_close(files, (int32_t)size, 0);
        aux[2] = dk_files_abort(files, (int32_t)size);
        return 0;
    }
    if (job_case == 6) {
        int32_t rc = dk_files_delete(files, root, path);
        if (rc == DK_FILE_E_BUSY)
            return DK_IO_MORE;
        close_rc = rc;
        return rc;
    }
    if (phase == 0) {
        /* the hash of the whole pattern first (the module checks the device's against it) */
        dk_hash32 hs;
        dk_hash32_init(&hs);
        for (uint32_t off = 0; off < size; off += CHUNK) {
            uint32_t n = size - off < CHUNK ? size - off : CHUNK;
            fill(off, n);
            dk_hash32_update(&hs, buf, n);
        }
        if (tail16) {
            static const uint8_t tail[16] __attribute__((aligned(16)));
            dk_hash32_update(&hs, tail, 16);
        }
        mod_hash = dk_hash32_finish(&hs);
        uint8_t h4[4];
        dk_hash32_bytes(job_case == 5 ? mod_hash ^ 0x100u : mod_hash, h4);
        handle = dk_files_create(files, root, path, size,
                                 job_case == 4 || job_case == 2 || job_case == 3 || job_case == 7 ||
                                         job_case == 8 || job_case == 10
                                     ? 0
                                     : h4,
                                 flags);
        create_rc = handle;
        if (handle <= 0)
            return handle ? handle : -1;
        if (job_case == 7) {
            in_spin = 1;
            while (!spin_release) {
                __asm__ volatile("" ::: "memory");
            }
            in_spin = 0;
            abort_rc = dk_files_abort(files, handle);
            return 0;
        }
        if (job_case == 8) {
            abort_rc = dk_files_abort(files, handle);
            aux[0] = dk_files_write(files, handle, 0, buf, 512);
            aux[1] = dk_files_close(files, handle, 0);
            aux[2] = dk_files_abort(files, handle);
            return 0;
        }
        phase = 1;
        return DK_IO_MORE;
    }
    if (phase == 1 && job_case == 10) {
        /* one call of 64 KiB, then the tail in a second call */
        uint32_t n = size - written < BIG ? size - written : BIG;
        fill(written, n);
        int32_t rc = dk_files_write(files, handle, written, buf, n);
        if (rc == DK_FILE_E_BUSY)
            return DK_IO_MORE;
        if (rc < 0) {
            dk_files_abort(files, handle);
            return rc;
        }
        written += n;
        if (written >= size)
            phase = 3;
        return DK_IO_MORE;
    }
    if (phase == 1) {
        uint32_t n = size - written < CHUNK ? size - written : CHUNK;
        fill(written, n);
        int32_t rc = dk_files_write(files, handle, written, buf, n);
        if (rc == DK_FILE_E_BUSY)
            return DK_IO_MORE;
        if (rc < 0) {
            dk_files_abort(files, handle);
            return rc;
        }
        written += n;
        if (job_case == 2 && written >= CHUNK) {
            abort_rc = dk_files_abort(files, handle);
            return 0;
        }
        if (job_case == 3 && written >= CHUNK) {
            phase = 2;
            return DK_IO_WAIT;
        }
        if (written >= size)
            phase = 3;
        return DK_IO_MORE;
    }
    if (phase == 2) {
        abort_rc = dk_files_abort(files, handle);
        return 0;
    }
    {
        int32_t rc = dk_files_close(files, handle, dev_hash);
        if (rc == DK_FILE_E_BUSY)
            return DK_IO_MORE;
        close_rc = rc;
        return rc;
    }
}
static void finished(void *arg, int32_t status)
{
    (void)arg;
    job_status = status;
    __asm__ volatile("" ::: "memory");
    state = 2;
}
__attribute__((optimize("no-jump-tables"))) static int32_t
command(uint32_t cmd, uint32_t seq, const void *request, uint32_t len, void *reply)
{
    const uint8_t *body = request;
    uint8_t *out = reply;
    (void)seq;
    if (cmd == PROOF_CMD) {
        if (state == 1)
            return -12;
        if (len < 7 || (body[0] & 0x1f) < 1 || (body[0] & 0x1f) > 11)
            return -50;
        uint32_t i = 5;
        while (i < len && body[i] != 0)
            i++;
        if (i >= len || i - 5 >= sizeof path)
            return -50;
        job_case = body[0] & 0x1f;
        b_n = b_mode = b_i = 0;
        if (job_case == 11) {
            if (i + 2 >= len)
                return -50;
            b_n = body[i + 1];
            b_mode = body[i + 2];
            if (b_n == 0 || b_n > 99 || i - 5 > 150)
                return -50;
        }
        tail16 = (body[0] & 0x20) != 0;
        flags = (body[0] & 0x40) ? DK_FILE_REPLACE : 0;
        root = (body[0] & 0x80) ? DK_FILES_SAMPLES : DK_FILES_STORE;
        size = ((uint32_t)body[1] << 24) | ((uint32_t)body[2] << 16) | ((uint32_t)body[3] << 8) |
               body[4];
        for (uint32_t k = 0; k <= i - 5; ++k)
            path[k] = (char)body[5 + k];
        written = steps = mod_hash = 0;
        seed = 0;
        handle = create_rc = close_rc = abort_rc = job_status = phase = 0;
        aux[0] = aux[1] = aux[2] = 0;
        spin_release = 0;
        dev_hash[0] = dev_hash[1] = dev_hash[2] = dev_hash[3] = 0;
        state = 1;
        job_id = dk_io_post(step, finished, 0);
        if (job_id <= 0) {
            state = 0;
            return job_id ? job_id : -1;
        }
        return 0;
    }
    if (cmd == PROOF_CMD + 1) {
        out[0] = (uint8_t)state;
        dk_bytes_put32(out + 1, (uint32_t)create_rc);
        dk_bytes_put32(out + 5, (uint32_t)close_rc);
        dk_bytes_put32(out + 9, (uint32_t)abort_rc);
        dk_bytes_put32(out + 13, (uint32_t)job_status);
        dk_bytes_put32(out + 17, mod_hash);
        out[21] = dev_hash[0];
        out[22] = dev_hash[1];
        out[23] = dev_hash[2];
        out[24] = dev_hash[3];
        dk_bytes_put32(out + 25, steps);
        dk_bytes_put32(out + 29, written);
        dk_bytes_put32(out + 33, (uint32_t)job_id);
        dk_bytes_put32(out + 37, (uint32_t)aux[0]);
        dk_bytes_put32(out + 41, (uint32_t)aux[1]);
        dk_bytes_put32(out + 45, (uint32_t)aux[2]);
        return 49;
    }
    if (cmd == PROOF_CMD + 2) {
        spin_release = 1;
        dk_io_wake(job_id);
        return 0;
    }
    if (cmd == PROOF_CMD + 3) {
        dk_bytes_put32(out,
                       (uint32_t)dk_files_create(files, DK_FILES_STORE, "ctx-probe", 512, 0, 0));
        out[4] = (uint8_t)in_spin;
        int32_t h = len >= 4 ? (int32_t)(((uint32_t)body[0] << 24) | ((uint32_t)body[1] << 16) |
                                         ((uint32_t)body[2] << 8) | body[3])
                             : 0;
        dk_bytes_put32(out + 5, (uint32_t)dk_files_write(files, h, 0, buf, 512));
        dk_bytes_put32(out + 9, (uint32_t)dk_files_abort(files, h));
        return 13;
    }
    return -50;
}
static int32_t init(dk_module_ctx *ctx)
{
    int32_t r = dk_files_open(ctx, &files);
    if (r != DK_OK)
        return r;
    return dk_host_register(PROOF_CMD, 4, command);
}

const dk_module_def dk_module = {.init = init};
