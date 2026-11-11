/* The +Drive write plug-in point, version 1 (DT2). NOT an SDK contract: content modules never see it. The runtime (store_writer.rs) calls the
   optional symbols dkpw_* that modules/core/adapters/plusdrive-write exports, behind the SDK's file_* calls on DK_FILES_SAMPLES. Without the
   adapter the symbols resolve to 0 and those calls return E_CONTRACT. docs/plans/usb-fast.md 3.1, docs/findings/120-dt2-plusdrive-c-file-api.md.

   All calls run in a task (the I/O task, or the UI task for dkpw_abort and dkpw_recover) and may block on the firmware's filesystem mutex: never in an
   interrupt. The runtime makes one call at a time and keeps the session: owner, handle, offsets, idle expiry. The adapter holds one staged file.
   Paths are NUL-terminated, without a leading '/', already checked by the runtime (no empty segment, no "..", not inside dkpw_staging()).
   Statuses are the SDK's: E_EXISTS -18, E_CONTRACT -48, E_PATH -51, E_FULL -52, E_LENGTH -55, E_HASH -56, E_WRITE -57, E_NOFILE -59, E_FILE_IN_USE -71. */
#ifndef DKRT_PDW_H
#define DKRT_PDW_H
#include <stdint.h>

#define DKPW_VERSION 1u
uint32_t dkpw_version(void);

/* The staging folder as a firmware path (leading '/'). Written files are staged there until they are complete and hashed. The adapter creates the
   folder when a BEGIN does not find it and never removes it itself: it stays for an upload session, because creating and removing it costs more
   than a small file. The runtime removes it with dkpw_recover when no write session has run for 2 s. */
const uint8_t *dkpw_staging(void);

#define DKPW_REPLACE 1u
/* Bits 8 to 15 of `flags` are a test knob (the runtime's DKRT_PD_CUT): the adapter stops dead at that step of the write, as a power cut would. */
/* Start a file of `size` bytes (the caller's bytes; the adapter adds the firmware's 16 zero bytes). `hash4`: NULL or the expected `hash|1` of the
   data plus those 16 zeros, big-endian. Refuses an existing destination (E_EXISTS) unless DKPW_REPLACE, and an open one (E_FILE_IN_USE), before any
   data is written; a folder at the destination, a file where a folder of the path should be, or a path under /factory (the firmware's RAM-backed ROM tree) is E_PATH; a file that was never hashed counts
   as existing. 0 or a status. */
int32_t dkpw_begin(const uint8_t *path, uint32_t size, const uint8_t *hash4, uint32_t flags);

/* The next `len` bytes. `buf` is 16-byte aligned. 0 or E_WRITE. */
int32_t dkpw_write(const uint8_t *buf, uint32_t len);

/* All `size` bytes were written: add the 16 zero bytes, hash (the firmware's close hash), check `hash4` (E_HASH), create missing folders and move
   the file into place (a replace deletes the old file first, behind a journal record). `hash_out` (4 bytes, may be NULL) gets the file's `hash|1`,
   big-endian. The staged file is gone afterwards, whatever the result; the folder stays. */
int32_t dkpw_close(uint8_t *hash_out);

/* Drop the staged file: close, then delete (never the other order). Safe when nothing is staged. */
void dkpw_abort(void);

/* Delete a file. 0, E_NOFILE, E_PATH (a folder, or under /factory), or E_FILE_IN_USE (the stock firmware has it open). */
int32_t dkpw_delete(const uint8_t *path);

/* Boot recovery of the staging folder, once, before the first dkpw_begin: a hashed staged file whose journal names a missing destination is
   renamed into place; every other name of ours ("s" and 8 hex digits, optionally ".dest") is deleted; other names are left alone. out[0] files
   renamed into place, out[1] files deleted, out[2] of our names that stayed (a rename that failed), out[3] entries that are not ours. The folder is
   removed when it is empty (the runtime also calls it, with no session open, to end an upload session). -1 while the filesystem is not up (the firmware's mounted flag; call again later), else 0. A runtime that sees a failed
   dkpw_close calls it again before the next dkpw_begin. */
int32_t dkpw_recover(uint32_t *out);
#endif
