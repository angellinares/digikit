/* Read-only listing, API contract 1.4. Poll copies one complete owned record;
 * pending/end/error leave output untouched. No source lease/content validation.
 * Acquire client ONLY in init. list copies root-relative path (NUL in 256 bytes;
 * empty=root, no slash at edges, empty/dot/dotdot parts or control/DEL bytes).
 * Provider limits may be smaller (store paths <=159 bytes including DN2
 * samples/ prefix; DT2 input directory <=254 bytes before its leading slash).
 * Row identity is the captured request root plus full path. Cancel/supersede invalidates old tickets.
 * Optional callback runs deferred in UI task, never synchronously from list/poll;
 * it is a coalesced readiness hint: poll the supplied ticket, allow stale errors.
 * Callback/context must remain module-owned until teardown quiesces. */
#ifndef DK_FILES_H
#define DK_FILES_H
#include <dk/core/api.h>
#define DK_FILES_SAMPLES 1u
#define DK_FILES_STORE 2u
#define DK_FILE_FILE 1u
#define DK_FILE_DIRECTORY 2u
#define DK_FILE_SIZE_VALID 1u
#define DK_FILE_HASH_VALID 2u
#define DK_FILE_HASH_NONE 0u
#define DK_FILE_HASH_SHA256 1u
#define DK_FILE_HASH_ELEKTRON32 2u
#define DK_FILES_PENDING 0
#define DK_FILES_ROW 1
#define DK_FILES_END 2
/* SIZE_VALID reports source physical bytes, including DT2's 16-byte trailer
 * (unlike stock RPC ReadDir). HASH_VALID names stored metadata: the stored odd
 * 32-bit content hash (HASH_ELEKTRON32) big endian in hash[0..4], remainder zero,
 * on both devices (store format 4 keeps it in the table's hash slot). HASH_SHA256
 * is reserved: store format 3 kept SHA-256, and no runtime produces it now.
 * Unknown directory/file metadata has flags 0, size/hash fields zero. No writable
 * or read-only permission flag is inferred from firmware node bits. A DT2 file
 * metadata read failure yields a row with flags 0; open/iteration errors remain
 * negative results. Session ID exhaustion returns -13 and requires restart. */
typedef struct dk_file_entry {
    uint32_t kind, flags, size_bytes, hash_kind;
    uint8_t hash[32];
    uint8_t path[256];
} dk_file_entry;
_Static_assert(sizeof(dk_file_entry) == 304, "dk_file_entry layout");
_Static_assert(__builtin_offsetof(dk_file_entry, path) == 48, "dk_file_entry path");

/* Writing and deleting files of the digikit store (api 1.10; since 1.11 the imports dk_file_create, dk_file_write, dk_file_close, dk_file_abort and
 * dk_file_delete). This text was <dk/platform/file.h>, which now only includes this header. The roots are those of the listing above.
 *
 * Only an I/O step (<dk/platform/io.h>, dk_io_post) may call these: the call must be made in the I/O task, by the step. Anywhere else (the UI or MIDI task
 * as well, even while a step waits) they return E_CONTEXT.
 *
 *   h = dk_file_create(DK_FILES_STORE, "samples/kick.dkn", size, hash4, 0);  hash4: NULL, or the expected file hash (4 bytes, big-endian)
 *   dk_file_write(h, 0, buf, n);  dk_file_write(h, n, buf, m);  ...       in order: `offset` is the next byte
 *   dk_file_close(h, hash_out);                                             checks `hash4`, commits, returns the file hash of the bytes
 * or dk_file_abort(h) to drop the partial file. The file is in the store only after file_close returns 0 (a power cut before that leaves
 * the old state; the commit is the store's two-header scheme). A file that already exists is not replaced unless the last argument of
 * file_create is DK_FILE_REPLACE: without it file_create returns E_EXISTS. A replace is atomic: the old file stays whole and readable until
 * file_close swaps the new one in, so a power cut, an error or file_abort leaves the old file (never delete first, then write). A reader that
 * still reads the old file (a sample loading from it) makes file_create or file_close return E_FILE_IN_USE: try again later.
 *
 * Rules:
 *   - One write session in all, shared with the SysEx upload: a second file_create (or a SysEx BEGIN while yours is open) gets E_BUSY. A
 *     SysEx command that finds the store in use gets E_BUSY too; the host asks again.
 *   - file_write: `buf` is 16-byte aligned (E_CONTRACT otherwise); `len` is a multiple of 512 except for the last piece of the file, whose
 *     tail sector is padded with zeros; the file never grows past `size`. Any `len`: the runtime writes it in 32 KiB pieces. The runtime keeps
 *     its own buffers: your `buf` is free as soon as the call returns.
 *   - The store lock: every call waits up to about 3 s for the store (a SysEx command or another module's call may hold it) and then returns
 *     E_BUSY. file_write takes the lock once for its whole call, so it returns E_BUSY only before it wrote anything: call again (return
 *     DK_IO_MORE) with the same arguments. A step that returned DK_IO_MORE after an E_BUSY is run again after 1 ms.
 *   - A write session with no write for 20 s is aborted by the runtime (the file is not in the table): a later call with its handle returns
 *     E_STATE. A step that is waiting for something other than the card must finish or abort its file before that.
 *   - The handle belongs to the module whose job made it. When the job ends with a session open, or the module is unloaded, the runtime
 *     aborts the session.
 *   - file_close writes the 4 bytes of the file hash `hash|1` to `hash_out` (may be NULL) before it checks `hash4`, so a module can report
 *     the hash on E_HASH. It returns E_LENGTH when fewer than `size` bytes were written. The session is closed whatever the result.
 *
 * Roots (above): DK_FILES_STORE on both devices. DK_FILES_SAMPLES: on the Digitone II the `samples/` folder of the store; on the
 * Digitakt II the stock +Drive, written through the firmware's own file code by the adapter core/plusdrive-write (E_CONTRACT when it is not
 * in the image). On the +Drive: `size` is the sample file's size without the 16 zero bytes that the firmware stores after every file (the
 * adapter writes them); `hash4` and the `hash_out` of file_close are the firmware's hash of the data plus those 16 zeros (hash32_* over both);
 * missing folders of `path` are created; an existing file is E_FILE_IN_USE while the stock firmware has it open; file_close can take seconds
 * for a large file (the firmware reads it back to hash it). The replace is a delete and a rename of a fully written, hashed file: a power cut
 * leaves the old or the new file, and the next boot finishes or drops the half-done step.
 *
 * Statuses: E_EXISTS (-18) and E_CONTEXT (-19) of sdk/sdk.toml [load.status]; E_BUSY (-12); the store's E_PATH (-51), E_FULL (-52), E_HASH
 * (-56), E_WRITE (-57), E_NOFILE (-59), E_NOSTORE (-61), E_INUSE (-62), E_FILE_IN_USE (-71, file_delete: a sample is still loading from the file), and E_STATE (-53), E_OFFSET (-54), E_LENGTH (-55) for a misused handle,
 * offset or length. */

/* The new entry (<dk/core/module.h>): `dk_files_open(ctx, &files)` in `init` gives the listing handle that dk_files_list, dk_files_poll and
   dk_files_cancel take where the Api table took a client number. The write calls need no handle. */
#include <dk/core/status.h>

#define DK_FILE_REPLACE 1u /* dk_file_create flag: replace an existing file atomically */
#define DK_FILE_E_BUSY DK_E_BUSY
#define DK_FILE_E_EXISTS DK_E_EXISTS
#define DK_FILE_E_CONTEXT DK_E_CONTEXT
#define DK_FILE_E_PATH DK_E_PATH
#define DK_FILE_E_FULL DK_E_FULL
#define DK_FILE_E_STATE DK_E_STATE
#define DK_FILE_E_OFFSET DK_E_OFFSET
#define DK_FILE_E_LENGTH DK_E_LENGTH
#define DK_FILE_E_HASH DK_E_HASH
#define DK_FILE_E_WRITE DK_E_WRITE
#define DK_FILE_E_NOFILE DK_E_NOFILE
#define DK_FILE_E_FILE_IN_USE DK_E_FILE_IN_USE
#endif
