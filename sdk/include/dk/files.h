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
#include <dk/api.h>
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
 * (unlike stock RPC ReadDir). HASH_VALID names stored metadata: SHA256 bytewise,
 * or DT2 stored odd 32-bit hash big endian in hash[0..4], remainder zero.
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
#endif
