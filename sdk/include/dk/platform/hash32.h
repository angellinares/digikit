/* <dk/platform/hash32.h> (API 1.10): the file hash of the store and of the Elektron +Drive (a lookup3 variant over big-endian words, seed 0x43fa243a).
 * One implementation in the runtime (api->hash32_*); the host's dksample::content_hash gives the same value.
 *
 *   dk_hash32 s;  api->hash32_init(&s);  api->hash32_update(&s, p, n);  ...  uint32_t h = api->hash32_finish(&s);
 *
 * hash32_finish returns `hash | 1`, the value the store keeps: 4 bytes big-endian in the first bytes of a 32-byte table slot (the rest
 * zero), and what file_close writes to its `hash_out`. The state is the caller's, 28 bytes, 4-byte aligned. Feed the bytes in any piece
 * sizes (the tail of 1 to 12 bytes is carried inside the state); a 16-byte aligned multiple of 512 bytes is the fast case. */
#ifndef DK_HASH32_H
#define DK_HASH32_H

#include <dk/core/api.h>

/* The state of modules/core/runtime/src/rs/src/hash32.rs (a, b, c, n, buf). Opaque: use api->hash32_*. */
typedef struct dk_hash32 {
    uint32_t a, b, c, n;
    uint8_t buf[12];
} dk_hash32;
_Static_assert(sizeof(dk_hash32) == 28, "dk_hash32 layout");

/* The 4 bytes of the file hash `h` as the store and `file_close` write them. */
static inline void dk_hash32_bytes(uint32_t h, uint8_t out[4]) {
    out[0] = (uint8_t)(h >> 24); out[1] = (uint8_t)(h >> 16); out[2] = (uint8_t)(h >> 8); out[3] = (uint8_t)h;
}

#endif
