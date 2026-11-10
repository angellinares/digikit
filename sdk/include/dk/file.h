/* <dk/file.h> (API 1.10): write and delete files of the digikit store from a module (api->file_create, file_write, file_close, file_abort,
 * file_delete). The listing API is <dk/files.h>; the roots are the same.
 *
 * Only an I/O step (<dk/io.h>, api->io_post) may call these: the call must be made in the I/O task, by the step. Anywhere else (the UI or MIDI task
 * as well, even while a step waits) they return E_CONTEXT.
 *
 *   h = api->file_create(DK_FILES_STORE, "samples/kick.dkn", size, hash4, 0);  hash4: NULL, or the expected file hash (4 bytes, big-endian)
 *   api->file_write(h, 0, buf, n);  api->file_write(h, n, buf, m);  ...       in order: `offset` is the next byte
 *   api->file_close(h, hash_out);                                             checks `hash4`, commits, returns the file hash of the bytes
 * or api->file_abort(h) to drop the partial file. The file is in the store only after file_close returns 0 (a power cut before that leaves
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
 * Roots (<dk/files.h>): DK_FILES_STORE on both devices. DK_FILES_SAMPLES: on the Digitone II the `samples/` folder of the store; on the
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
#ifndef DK_FILE_H
#define DK_FILE_H

#include <dk/api.h>
#include <dk/files.h>

#define DK_FILE_REPLACE   1u  /* file_create flag: replace an existing file atomically */
#define DK_FILE_E_BUSY    (-12)
#define DK_FILE_E_EXISTS  (-18)
#define DK_FILE_E_CONTEXT (-19)
#define DK_FILE_E_PATH    (-51)
#define DK_FILE_E_FULL    (-52)
#define DK_FILE_E_STATE   (-53)
#define DK_FILE_E_OFFSET  (-54)
#define DK_FILE_E_LENGTH  (-55)
#define DK_FILE_E_HASH    (-56)
#define DK_FILE_E_WRITE   (-57)
#define DK_FILE_E_NOFILE  (-59)
#define DK_FILE_E_FILE_IN_USE (-71)

#endif
